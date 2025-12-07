use std::cell::RefCell;
use std::rc::Rc;
use std::collections::{BinaryHeap, HashMap};
use std::cmp::Ordering;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::Clamped;
use web_sys::{WebSocket, HtmlCanvasElement, CanvasRenderingContext2d, ImageData, MouseEvent, WheelEvent, TouchEvent, MessageEvent};
use serde::{Serialize, Deserialize};

// --- IMPORTS & LOGGING ---
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
    #[wasm_bindgen(js_namespace = Math)]
    fn random() -> f64;
}

// --- NETWORK PROTOCOL ---
#[derive(Serialize, Deserialize, Debug, Clone)]
struct PlayerInfo {
    id: i32,
    chunk_x: i32,
    chunk_y: i32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct UnitDTO {
    owner_id: i32,
    unit_idx: usize,
    x: f32,
    y: f32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct BuildingDTO {
    id: i32,
    owner_id: i32,
    kind: u8,
    tile_x: i32,
    tile_y: i32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
enum GameMessage {
    Join { version: u32, token: Option<String> },
    Welcome { player_id: i32, chunk_x: i32, chunk_y: i32, players: Vec<PlayerInfo>, units: Vec<UnitDTO>, buildings: Vec<BuildingDTO>, token: String },
    NewPlayer { player: PlayerInfo },
    UnitMove { player_id: i32, unit_idx: usize, x: f32, y: f32 },
    UnitSync { player_id: i32, unit_idx: usize, x: f32, y: f32 },
    SpawnUnit, 
    UnitSpawned { unit: UnitDTO },
    Build { kind: u8, tile_x: i32, tile_y: i32 },
    BuildingSpawned { building: BuildingDTO },
    Error { message: String },
}

const CLIENT_VERSION: u32 = 20;

// --- CHAT CLIENT ---
#[wasm_bindgen]
pub struct ChatClient {
    socket: WebSocket,
}

#[wasm_bindgen]
impl ChatClient {
    #[wasm_bindgen(constructor)]
    pub fn new(url: &str) -> Result<ChatClient, JsValue> {
        let ws = WebSocket::new(url)?;
        Ok(ChatClient { socket: ws })
    }
    pub fn send_message_str(&self, msg: &str) -> Result<(), JsValue> {
        self.socket.send_with_str(msg)
    }
    pub fn send_message(&self, msg: &str) -> Result<(), JsValue> {
        self.socket.send_with_str(msg)
    }
}

// --- PIXEL BUFFER ENGINE ---
const WIDTH: u32 = 360;
const HEIGHT: u32 = 640;
const CHUNK_SIZE: i32 = 32; // User requested 32x32 plot per player
const TILE_SIZE_BASE: f32 = 16.0;

struct PixelBuffer {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl PixelBuffer {
    fn new(width: u32, height: u32) -> PixelBuffer {
        let size = (width * height * 4) as usize;
        PixelBuffer { width, height, pixels: vec![0; size] }
    }

    fn clear(&mut self, r: u8, g: u8, b: u8) {
        for i in (0..self.pixels.len()).step_by(4) {
            self.pixels[i] = r;
            self.pixels[i+1] = g;
            self.pixels[i+2] = b;
            self.pixels[i+3] = 255;
        }
    }

    fn pixel(&mut self, x: i32, y: i32, r: u8, g: u8, b: u8) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 { return; }
        let idx = ((y * self.width as i32 + x) * 4) as usize;
        self.pixels[idx] = r;
        self.pixels[idx+1] = g;
        self.pixels[idx+2] = b;
        self.pixels[idx+3] = 255;
    }

    fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, r: u8, g: u8, b: u8) {
        // Clip to screen
        let start_x = x.max(0);
        let start_y = y.max(0);
        let end_x = (x + w).min(self.width as i32);
        let end_y = (y + h).min(self.height as i32);

        if start_x >= end_x || start_y >= end_y { return; }

        for iy in start_y..end_y {
            for ix in start_x..end_x {
                let idx = ((iy * self.width as i32 + ix) * 4) as usize;
                self.pixels[idx] = r;
                self.pixels[idx+1] = g;
                self.pixels[idx+2] = b;
                self.pixels[idx+3] = 255;
            }
        }
    }

    fn rect_outline(&mut self, x: i32, y: i32, w: i32, h: i32, r: u8, g: u8, b: u8) {
        self.rect(x, y, w, 1, r, g, b);         // Top
        self.rect(x, y + h - 1, w, 1, r, g, b); // Bottom
        self.rect(x, y, 1, h, r, g, b);         // Left
        self.rect(x + w - 1, y, 1, h, r, g, b); // Right
    }

    fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, r: u8, g: u8, b: u8, dashed: bool) {
        let mut x = x0;
        let mut y = y0;
        let dx = (x1 - x0).abs();
        let dy = (y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx - dy;

        let mut counter = 0;

        loop {
            if !dashed || (counter % 8 < 4) {
                self.pixel(x, y, r, g, b);
            }
            counter += 1;

            if x == x1 && y == y1 { break; }
            let e2 = 2 * err;
            if e2 > -dy { err -= dy; x += sx; }
            if e2 < dx { err += dx; y += sy; }
        }
    }
}

// --- PATHFINDING ---

#[derive(Copy, Clone, Eq, PartialEq)]
struct Node {
    cost: u32,
    pos: (i32, i32), // Global Tile Coords
}

impl Ord for Node {
    fn cmp(&self, other: &Self) -> Ordering {
        other.cost.cmp(&self.cost)
    }
}

impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// --- GAME STATE ---

#[derive(Clone, Copy, PartialEq)]
enum TileType {
    Grass,
    Water,
    Forest,
    Mountain,
    Gold,
}

struct Unit {
    x: f32, // Global World Pos
    y: f32,
    path: Vec<(f32, f32)>, // Global Waypoints
    selected: bool,
    kind: u8,
    color: (u8, u8, u8),
    owner_id: i32, // Synced Owner ID
}

struct Building {
    tile_x: i32, // Global Tile Pos
    tile_y: i32,
    kind: u8,
    owner_id: i32, // Added owner tracking for coloring
}

struct Chunk {
    tiles: Vec<TileType>,
}

struct GameState {
    chunks: HashMap<(i32, i32), Chunk>,
    units: Vec<Unit>,
    buildings: Vec<Building>,
    
    // Camera
    camera_x: f32, // Center of view in World Coords
    camera_y: f32,
    zoom: f32,

    // Multiplayer State
    my_id: Option<i32>,
    my_chunk_x: i32,
    my_chunk_y: i32,
    other_players: Vec<PlayerInfo>,
    socket: Option<WebSocket>, // For sending commands

    // Input State
    last_touch_dist: Option<f32>,
    last_pan_x: Option<f32>,
    last_pan_y: Option<f32>,
    
    // Selection
    selected_building: Option<usize>,
    
    // Double-click detection
    last_click_time: f64,
    last_click_x: f32,
    last_click_y: f32,
    
    // Group/Drag Selection
    group_select_mode: bool,
    drag_start: Option<(f32, f32)>,  // Screen coords
    drag_current: Option<(f32, f32)>, // Screen coords
    
    // Mouse/touch down tracking (to distinguish click from drag/pan)
    mouse_down_pos: Option<(f32, f32)>,
    touch_is_pan_or_zoom: bool,  // Set true if user panned or pinch-zoomed
    
    // Build Mode (wall placement)
    build_mode: bool,
    wall_start: Option<(i32, i32)>,  // Tile coords - first point
    wall_end: Option<(i32, i32)>,    // Tile coords - second point
    wall_preview: Vec<(i32, i32)>,   // Preview tiles to build
    
    // Sync
    last_sync_time: f64,
    
    // Smooth Zoom
    target_zoom: f32,
}

impl GameState {
    fn new() -> GameState {
        let mut gs = GameState { 
            chunks: HashMap::new(),
            units: Vec::new(),
            buildings: Vec::new(),
            camera_x: 0.0,
            camera_y: 0.0,
            zoom: 1.0,
            my_id: None,
            my_chunk_x: 0,
            my_chunk_y: 0,
            other_players: Vec::new(),
            socket: None,
            last_touch_dist: None,
            last_pan_x: None,
            last_pan_y: None,
            selected_building: None,
            last_click_time: 0.0,
            last_click_x: 0.0,
            last_click_y: 0.0,
            group_select_mode: false,
            drag_start: None,
            drag_current: None,
            mouse_down_pos: None,
            touch_is_pan_or_zoom: false,
            build_mode: false,
            wall_start: None,
            wall_end: None,
            wall_preview: Vec::new(),
            last_sync_time: 0.0,
            target_zoom: 1.0,
        };

        // Generate Initial Chunk (0,0)
        gs.generate_chunk(0, 0);
        
        // Center camera on 0,0 chunk center roughly
        gs.camera_x = (CHUNK_SIZE as f32 * TILE_SIZE_BASE) / 2.0;
        gs.camera_y = (CHUNK_SIZE as f32 * TILE_SIZE_BASE) / 2.0;

        // Place Town Center at the assigned chunk center
        // Since we don't know our ID yet, we can't place the building correctly in new().
        // We will handle building placement in Welcome/NewPlayer messages.
        
        gs
    }

    fn spawn_units_for_player(&mut self, pid: i32, cx: i32, cy: i32) {
        let sx = (cx as f32 * CHUNK_SIZE as f32 * TILE_SIZE_BASE) + (CHUNK_SIZE as f32 * TILE_SIZE_BASE / 2.0);
        let sy = (cy as f32 * CHUNK_SIZE as f32 * TILE_SIZE_BASE) + (CHUNK_SIZE as f32 * TILE_SIZE_BASE / 2.0);
        
        let color = if Some(pid) == self.my_id { (0, 0, 255) } else { (255, 0, 0) };
        
        self.units.push(Unit { x: sx + 30.0, y: sy + 30.0, path: Vec::new(), selected: false, kind: 0, color, owner_id: pid });
        self.units.push(Unit { x: sx - 20.0, y: sy + 40.0, path: Vec::new(), selected: false, kind: 0, color, owner_id: pid });
        
        // Spawn Building for this player
        let mid = CHUNK_SIZE / 2;
        self.buildings.push(Building { 
            tile_x: cx * CHUNK_SIZE + mid, 
            tile_y: cy * CHUNK_SIZE + mid, 
            kind: 0,
            owner_id: pid 
        });
    }

    fn calculate_tile_type(cx: i32, cy: i32, lx: i32, ly: i32) -> TileType {
        // Ensure walkability for Town Center (center of chunk)
        let mid = CHUNK_SIZE / 2;
        if lx >= mid - 3 && lx <= mid + 3 && ly >= mid - 3 && ly <= mid + 3 {
            return TileType::Grass;
        }

        let seed = ((cx as i64 * 73856093) ^ (cy as i64 * 19349663) ^ (lx as i64 * 83492791) ^ (ly as i64 * 23492871)) as f64;
        let r = (seed.sin() * 10000.0).fract().abs();
        
        if r < 0.25 { TileType::Forest }
        else if r < 0.28 { TileType::Mountain }
        else if r < 0.283 { TileType::Gold }
        else { TileType::Grass }
    }

    fn generate_chunk(&mut self, cx: i32, cy: i32) {
        if self.chunks.contains_key(&(cx, cy)) { return; }
        
        let mut tiles = vec![TileType::Grass; (CHUNK_SIZE * CHUNK_SIZE) as usize];
        for y in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                let idx = (y * CHUNK_SIZE + x) as usize;
                tiles[idx] = GameState::calculate_tile_type(cx, cy, x, y);
            }
        }
        self.chunks.insert((cx, cy), Chunk { tiles });
    }

    fn get_tile_type(&self, gx: i32, gy: i32) -> Option<TileType> {
        let cx = (gx as f32 / CHUNK_SIZE as f32).floor() as i32;
        let cy = (gy as f32 / CHUNK_SIZE as f32).floor() as i32;
        
        let mut lx = gx % CHUNK_SIZE;
        let mut ly = gy % CHUNK_SIZE;
        if lx < 0 { lx += CHUNK_SIZE; }
        if ly < 0 { ly += CHUNK_SIZE; }

        if let Some(chunk) = self.chunks.get(&(cx, cy)) {
            Some(chunk.tiles[(ly * CHUNK_SIZE + lx) as usize])
        } else {
            // Virtual terrain for pathfinding (Fog of War)
            Some(GameState::calculate_tile_type(cx, cy, lx, ly))
        }
    }

    fn is_tile_walkable(&self, gx: i32, gy: i32) -> bool {
        let cx = (gx as f32 / CHUNK_SIZE as f32).floor() as i32;
        let cy = (gy as f32 / CHUNK_SIZE as f32).floor() as i32;
        
        // STRICT CHECK: If chunk doesn't exist, it's void/black => NOT walkable
        if !self.chunks.contains_key(&(cx, cy)) {
            return false;
        }

        match self.get_tile_type(gx, gy) {
            Some(t) => match t {
                TileType::Water | TileType::Forest | TileType::Mountain | TileType::Gold => return false,
                _ => {}
            },
            None => return false, // Should be covered by chunk check above
        }
        
        // Check Buildings - each building occupies exactly 1 tile
        for b in &self.buildings {
            if gx == b.tile_x && gy == b.tile_y {
                return false;
            }
        }
        true
    }

    fn find_path(&mut self, start: (f32, f32), end: (f32, f32)) -> Vec<(f32, f32)> {
        let start_tx = (start.0 / TILE_SIZE_BASE).floor() as i32;
        let start_ty = (start.1 / TILE_SIZE_BASE).floor() as i32;
        let end_tx = (end.0 / TILE_SIZE_BASE).floor() as i32;
        let end_ty = (end.1 / TILE_SIZE_BASE).floor() as i32;

        if start_tx == end_tx && start_ty == end_ty { return vec![end]; }
        
        // Removed aggressive chunk generation for Fog of War
        // The pathfinder now uses get_tile_type which calculates virtual terrain
        
        if !self.is_tile_walkable(end_tx, end_ty) { return vec![]; }

        // Limit pathfinding search space for performance
        // Increased from 100 to 5000 to allow cross-map movement
        if (start_tx - end_tx).abs() > 5000 || (start_ty - end_ty).abs() > 5000 { return vec![]; }

        let mut frontier = BinaryHeap::new();
        frontier.push(Node { cost: 0, pos: (start_tx, start_ty) });

        let mut came_from: HashMap<(i32, i32), (i32, i32)> = HashMap::new();
        let mut cost_so_far: HashMap<(i32, i32), u32> = HashMap::new();
        
        came_from.insert((start_tx, start_ty), (start_tx, start_ty));
        cost_so_far.insert((start_tx, start_ty), 0);

        let mut found = false;
        let mut steps = 0;

        while let Some(Node { cost: _, pos: current }) = frontier.pop() {
            steps += 1;
            // Increased safety break from 2000 to 15000 to allow long paths
            if steps > 15000 { break; } 

            if current == (end_tx, end_ty) {
                found = true;
                break;
            }

            let dirs = [
                (1, 0), (-1, 0), (0, 1), (0, -1), 
                (1, 1), (-1, -1), (1, -1), (-1, 1),
            ];

            for (dx, dy) in dirs.iter() {
                let next = (current.0 + dx, current.1 + dy);
                
                // Removed chunk generation here too

                if self.is_tile_walkable(next.0, next.1) {
                    // Prevent corner cutting
                    if *dx != 0 && *dy != 0 {
                        if !self.is_tile_walkable(current.0 + dx, current.1) || 
                           !self.is_tile_walkable(current.0, current.1 + dy) {
                            continue;
                        }
                    }

                    let new_cost = cost_so_far[&current] + 1;
                    if !cost_so_far.contains_key(&next) || new_cost < cost_so_far[&next] {
                        cost_so_far.insert(next, new_cost);
                        let h = std::cmp::max((next.0 - end_tx).abs(), (next.1 - end_ty).abs()) as u32;
                        frontier.push(Node { cost: new_cost + h, pos: next });
                        came_from.insert(next, current);
                    }
                }
            }
        }

        if !found { return vec![]; }

        let mut path = Vec::new();
        let mut curr = (end_tx, end_ty);
        path.push(end);

        while curr != (start_tx, start_ty) {
            path.push((
                (curr.0 as f32 * TILE_SIZE_BASE) + TILE_SIZE_BASE / 2.0,
                (curr.1 as f32 * TILE_SIZE_BASE) + TILE_SIZE_BASE / 2.0
            ));
            curr = came_from[&curr];
        }
        path
    }

    fn update(&mut self, dt: f64) {
        // Speed in pixels per SECOND (assuming tile base 16.0)
        // Previously 0.3 per frame @ 60fps = 18.0 per sec?
        // Let's make it consistent. 0.3 * 60 = 18.0. Let's try 50.0 for a good walking speed.
        let speed = (60.0 * dt) as f32; 
        let separation_radius = 10.0;
        let separation_force = 0.06; // This might need scaling with DT too

        // Smooth Zoom
        if (self.target_zoom - self.zoom).abs() > 0.001 {
            self.zoom += (self.target_zoom - self.zoom) * 10.0 * dt as f32;
        } else {
            self.zoom = self.target_zoom;
        }

        let unit_positions: Vec<(f32, f32)> = self.units.iter().map(|u| (u.x, u.y)).collect();
        let mut updates: Vec<(usize, f32, f32, bool)> = Vec::new();

        let my_id = self.my_id;

        // --- FOG OF WAR REVEAL ---
        // Perform chunk generation *before* the loop to avoid borrow checker issues
        // Collect all chunks that need generation from my units
        let mut chunks_to_generate = Vec::new();
        if let Some(my_id) = self.my_id {
            for u in &self.units {
                if u.owner_id == my_id {
                    let cx = (u.x / (CHUNK_SIZE as f32 * TILE_SIZE_BASE)).floor() as i32;
                    let cy = (u.y / (CHUNK_SIZE as f32 * TILE_SIZE_BASE)).floor() as i32;
                    
                    // Reveal 3x3 area
                    for dy in -1..=1 {
                        for dx in -1..=1 {
                            if !self.chunks.contains_key(&(cx+dx, cy+dy)) {
                                chunks_to_generate.push((cx+dx, cy+dy));
                            }
                        }
                    }
                    
                    // Also check target path destination if moving (anticipate arrival)
                    if let Some(target) = u.path.last() {
                        let tcx = (target.0 / (CHUNK_SIZE as f32 * TILE_SIZE_BASE)).floor() as i32;
                        let tcy = (target.1 / (CHUNK_SIZE as f32 * TILE_SIZE_BASE)).floor() as i32;
                        if !self.chunks.contains_key(&(tcx, tcy)) {
                            chunks_to_generate.push((tcx, tcy));
                        }
                    }
                }
            }
        }
        
        for (cx, cy) in chunks_to_generate {
            self.generate_chunk(cx, cy);
        }

        for (i, unit) in self.units.iter().enumerate() {
            // Only simulate physics/pathfinding for MY units
            if Some(unit.owner_id) != my_id {
                // Remote units: Just lerp to target if they have a path (which we use as target state)
                // Wait, we don't get paths for remote units via UnitMove anymore (we will use Sync).
                // But UnitMove sets path.
                // For now, let's rely on UnitSync to snap/lerp them.
                // If we have a path (from UnitMove), we can follow it, but we trust UnitSync more.
                
                // Actually, let's just let remote units sit still unless we get a Sync/Move.
                // If we get UnitMove, we calculate path.
                // So we CAN simulate them, but we must be ready to SNAP when Sync arrives.
                // Let's simulate them for smoothness, but the Sync will correct us.
            }

            let mut dx = 0.0;
            let mut dy = 0.0;
            let mut should_pop = false;

            if let Some(target) = unit.path.last() {
                let tx = target.0 - unit.x;
                let ty = target.1 - unit.y;
                let dist = (tx*tx + ty*ty).sqrt();

                if dist < 1.0 {
                    should_pop = true;
                } else {
                    dx += (tx / dist) * speed;
                    dy += (ty / dist) * speed;
                }
            }

            // Separation (Only for my units to avoid jittering remote ones?)
            if Some(unit.owner_id) == my_id {
                for (j, other_pos) in unit_positions.iter().enumerate() {
                    if i == j { continue; }
                    let ox = unit.x - other_pos.0;
                    let oy = unit.y - other_pos.1;
                    let dist_sq = ox*ox + oy*oy;
                    if dist_sq < separation_radius * separation_radius && dist_sq > 0.0001 {
                        let dist = dist_sq.sqrt();
                        dx += (ox / dist) * separation_force;
                        dy += (oy / dist) * separation_force;
                    }
                }
            }

            let new_x = unit.x + dx;
            let new_y = unit.y + dy;

            // Collision & Apply
            // Only do collision logic for MY units to prevent getting stuck on things that server says are fine
            if Some(unit.owner_id) == my_id {
                // Reveal Fog of War moved to start of function
                
                let cx = new_x + 3.0;
                let cy = new_y + 5.0;
                let tx = (cx / TILE_SIZE_BASE).floor() as i32;
                let ty = (cy / TILE_SIZE_BASE).floor() as i32;

                if self.is_tile_walkable(tx, ty) {
                    updates.push((i, new_x, new_y, should_pop));
                } else {
                    // Slide (Simplified)
                     let mut final_x = unit.x;
                    let mut final_y = unit.y;

                    let cx_x = (unit.x + dx) + 3.0;
                    let cy_curr = unit.y + 5.0;
                    if self.is_tile_walkable((cx_x / TILE_SIZE_BASE).floor() as i32, (cy_curr / TILE_SIZE_BASE).floor() as i32) {
                        final_x += dx;
                    }

                    let cx_curr = final_x + 3.0;
                    let cy_y = (unit.y + dy) + 5.0;
                    if self.is_tile_walkable((cx_curr / TILE_SIZE_BASE).floor() as i32, (cy_y / TILE_SIZE_BASE).floor() as i32) {
                        final_y += dy;
                    }
                    
                    updates.push((i, final_x, final_y, should_pop));
                }
            } else {
                // Remote units just move (no collision check on client, trust source)
                updates.push((i, new_x, new_y, should_pop));
            }
        }

        for (i, x, y, pop) in updates {
            let u = &mut self.units[i];
            u.x = x;
            u.y = y;
            if pop { u.path.pop(); }
        }
        
        // --- SYNC LOGIC ---
        // Send UnitSync for MY units every 100ms
        let now = web_sys::window().unwrap().performance().unwrap().now();
        if now - self.last_sync_time > 100.0 {
            if let Some(ws) = &self.socket {
                // Only send if OPEN (Ready State 1)
                if ws.ready_state() == 1 {
                    let my_id = if let Some(id) = self.my_id { id } else { return };
                    
                    let mut my_unit_idx = 0;
                    for u in &self.units {
                        if u.owner_id == my_id {
                            let msg = GameMessage::UnitSync {
                                player_id: my_id,
                                unit_idx: my_unit_idx,
                                x: u.x,
                                y: u.y
                            };
                            
                             if let Ok(json) = serde_json::to_string(&msg) {
                                 // Safe to send due to ready_state check, but Result ignored
                                 let _ = ws.send_with_str(&json);
                             }
                             
                            my_unit_idx += 1;
                        }
                    }
                }
            }
            self.last_sync_time = now;
        }
    }

    fn screen_to_world(&self, screen_x: f32, screen_y: f32) -> (f32, f32) {
        let center_x = WIDTH as f32 / 2.0;
        let center_y = HEIGHT as f32 / 2.0;
        
        let world_x = (screen_x - center_x) / self.zoom + self.camera_x;
        let world_y = (screen_y - center_y) / self.zoom + self.camera_y;
        (world_x, world_y)
    }

    fn handle_click(&mut self, screen_x: f32, screen_y: f32) {
        let my_id = if let Some(id) = self.my_id { id } else { return };
        
        // Get current time for double-click detection
        let now = web_sys::window().unwrap().performance().unwrap().now();
        
        // --- UI CLICK HANDLING ---
        let footer_height = 60.0;
        let unit_icon_size = 20.0;
        let icons_y = HEIGHT as f32 - footer_height - unit_icon_size - 5.0;
        
        // Check Selected Unit Icons (above footer) - click to deselect
        if screen_y >= icons_y && screen_y <= icons_y + unit_icon_size {
            // Get selected units with their indices
            let selected_indices: Vec<usize> = self.units.iter()
                .enumerate()
                .filter(|(_, u)| u.selected && u.owner_id == my_id)
                .map(|(i, _)| i)
                .collect();
            
            let max_display = 10;
            for (i, &unit_idx) in selected_indices.iter().take(max_display).enumerate() {
                let icon_x = 10.0 + (i as f32 * (unit_icon_size + 4.0));
                if screen_x >= icon_x && screen_x <= icon_x + unit_icon_size {
                    // Deselect this unit
                    self.units[unit_idx].selected = false;
                    return;
                }
            }
        }
        
        if screen_y > HEIGHT as f32 - footer_height {
            // Click is in Footer
            
            // 1. Check Home Button (Center)
            let btn_size = 40.0;
            let home_btn_x = (WIDTH as f32 - btn_size) / 2.0;
            let home_btn_y = HEIGHT as f32 - footer_height + (footer_height - btn_size) / 2.0;
            
            if screen_x >= home_btn_x && screen_x <= home_btn_x + btn_size &&
               screen_y >= home_btn_y && screen_y <= home_btn_y + btn_size {
                   
                   // Select Town Center
                   for (i, b) in self.buildings.iter().enumerate() {
                       if b.owner_id == my_id && b.kind == 0 {
                           self.selected_building = Some(i);
                           for u in &mut self.units { u.selected = false; }
                           
                           let bx = b.tile_x as f32 * TILE_SIZE_BASE;
                           let by = b.tile_y as f32 * TILE_SIZE_BASE;
                           self.target_zoom = 1.0;
                           self.camera_x = bx;
                           self.camera_y = by;
                           break;
                       }
                   }
                   return;
            }
            
            // 2. Check Group Select Button (Right of Home)
            let group_btn_x = home_btn_x + btn_size + 10.0;
            let group_btn_y = home_btn_y;
            
            if screen_x >= group_btn_x && screen_x <= group_btn_x + btn_size &&
               screen_y >= group_btn_y && screen_y <= group_btn_y + btn_size {
                self.group_select_mode = !self.group_select_mode;
                return;
            }

            // 3. Check Spawn Button (Left) - Only if building selected
            if let Some(b_idx) = self.selected_building {
                if b_idx < self.buildings.len() && self.buildings[b_idx].kind == 0 {
                    let spawn_btn_x = 10.0;
                    let spawn_btn_y = home_btn_y;
                    
                    if screen_x >= spawn_btn_x && screen_x <= spawn_btn_x + btn_size &&
                       screen_y >= spawn_btn_y && screen_y <= spawn_btn_y + btn_size {
                        if let Some(ws) = &self.socket {
                             let msg = GameMessage::SpawnUnit;
                             if let Ok(json) = serde_json::to_string(&msg) {
                                 let _ = ws.send_with_str(&json);
                             }
                        }
                        return;
                    }
                }
            }
            
            // 4. Check Build Wall Button (Left) - Only if unit selected AND no building selected
            if self.selected_building.is_none() {
                let any_selected = self.units.iter().any(|u| u.selected && u.owner_id == my_id);
                if any_selected {
                    let build_btn_x = 10.0;
                    let build_btn_y = home_btn_y;
                    
                    if screen_x >= build_btn_x && screen_x <= build_btn_x + btn_size &&
                       screen_y >= build_btn_y && screen_y <= build_btn_y + btn_size {
                        // Toggle build mode
                        self.build_mode = !self.build_mode;
                        if !self.build_mode {
                            // Clear wall placement state when exiting build mode
                            self.wall_start = None;
                            self.wall_end = None;
                            self.wall_preview.clear();
                        }
                        return;
                    }
                }
            }
            
            // 5. Check Confirm/Cancel buttons (when wall placement is ready)
            if self.build_mode && self.wall_end.is_some() {
                // Confirm button (green check) - right side of footer
                let confirm_btn_x = WIDTH as f32 - btn_size - 60.0;
                let confirm_btn_y = home_btn_y;
                
                if screen_x >= confirm_btn_x && screen_x <= confirm_btn_x + btn_size &&
                   screen_y >= confirm_btn_y && screen_y <= confirm_btn_y + btn_size {
                    self.confirm_wall_build();
                    return;
                }
                
                // Cancel button (red X) - right of confirm
                let cancel_btn_x = WIDTH as f32 - btn_size - 10.0;
                let cancel_btn_y = home_btn_y;
                
                if screen_x >= cancel_btn_x && screen_x <= cancel_btn_x + btn_size &&
                   screen_y >= cancel_btn_y && screen_y <= cancel_btn_y + btn_size {
                    self.cancel_wall_build();
                    return;
                }
            }
            
            return; // Swallow click if in UI area
        }
        
        // --- WORLD CLICK HANDLING ---
        let (wx, wy) = self.screen_to_world(screen_x, screen_y);
        let clicked_tile_x = (wx / TILE_SIZE_BASE).floor() as i32;
        let clicked_tile_y = (wy / TILE_SIZE_BASE).floor() as i32;
        
        // Handle Build Mode (wall placement)
        if self.build_mode {
            if self.wall_start.is_none() {
                // First click: set start point
                self.wall_start = Some((clicked_tile_x, clicked_tile_y));
                self.wall_preview.clear();
                return;
            } else if self.wall_end.is_none() {
                // Second click: set end point and generate preview
                self.wall_end = Some((clicked_tile_x, clicked_tile_y));
                self.generate_wall_preview();
                return;
            }
            // If both are set, clicks go to confirm buttons (handled in UI)
            return;
        }
        
        // Double-click detection (300ms window, 20px tolerance)
        let is_double_click = (now - self.last_click_time) < 300.0 &&
                              (screen_x - self.last_click_x).abs() < 20.0 &&
                              (screen_y - self.last_click_y).abs() < 20.0;
        
        self.last_click_time = now;
        self.last_click_x = screen_x;
        self.last_click_y = screen_y;

        let mut clicked_unit_kind: Option<u8> = None;
        let mut clicked_unit = false;
        
        // 1. Try Select Unit
        for unit in &mut self.units {
            if unit.owner_id != my_id { continue; }

            let dx = (unit.x - wx).abs();
            let dy = (unit.y - wy).abs();
            
            if dx < 10.0 && dy < 10.0 {
                unit.selected = !unit.selected;
                clicked_unit = true;
                clicked_unit_kind = Some(unit.kind);
                self.selected_building = None;
                break;
            }
        }
        
        // Double-click: select all units of same type
        if clicked_unit && is_double_click {
            if let Some(kind) = clicked_unit_kind {
                for unit in &mut self.units {
                    if unit.owner_id == my_id && unit.kind == kind {
                        unit.selected = true;
                    }
                }
            }
        }

        if !clicked_unit {
            // 2. Try Select Building
            let mut clicked_building = false;
            for (i, b) in self.buildings.iter().enumerate() {
                if b.owner_id != my_id { continue; }
                
                // Buildings use TOP-LEFT based positioning (same as tiles)
                let tile_left = b.tile_x as f32 * TILE_SIZE_BASE;
                let tile_top = b.tile_y as f32 * TILE_SIZE_BASE;
                let tile_right = tile_left + TILE_SIZE_BASE;
                let tile_bottom = tile_top + TILE_SIZE_BASE;
                
                if wx >= tile_left && wx <= tile_right &&
                   wy >= tile_top && wy <= tile_bottom {
                       self.selected_building = Some(i);
                       clicked_building = true;
                       for u in &mut self.units { u.selected = false; }
                       break;
                   }
            }
            
            if !clicked_building {
                let any_unit_selected = self.units.iter().any(|u| u.selected && u.owner_id == my_id);
                
                if !any_unit_selected {
                    self.selected_building = None;
                } else {
                    // Move selected units
                    let mut paths = Vec::new();
                    let mut move_commands = Vec::new();
                    
                    let mut selected_units = Vec::new();
                    for (i, unit) in self.units.iter().enumerate() {
                        if unit.selected && unit.owner_id == my_id {
                            selected_units.push((i, unit.x, unit.y));
                        }
                    }
        
                    for (i, start_x, start_y) in selected_units {
                        let path = self.find_path((start_x, start_y), (wx, wy));
                        if !path.is_empty() {
                            paths.push((i, path));
                            
                            let mut my_unit_idx = 0;
                            for (k, u) in self.units.iter().enumerate() {
                                if u.owner_id == my_id {
                                    if k == i { break; }
                                    my_unit_idx += 1;
                                }
                            }
                            move_commands.push((my_unit_idx, wx, wy));
                        }
                    }
                    
                    for (i, path) in paths {
                        self.units[i].path = path;
                    }
                    
                    if let Some(ws) = &self.socket {
                        for (idx, tx, ty) in move_commands {
                            let msg = GameMessage::UnitMove { 
                                player_id: my_id, 
                                unit_idx: idx, 
                                x: tx, 
                                y: ty 
                            };
                            if let Ok(json) = serde_json::to_string(&msg) {
                                let _ = ws.send_with_str(&json);
                            }
                        }
                    }
                }
            }
        }
    }
    
    fn handle_drag_start(&mut self, screen_x: f32, screen_y: f32) {
        if self.group_select_mode {
            self.drag_start = Some((screen_x, screen_y));
            self.drag_current = Some((screen_x, screen_y));
        }
    }
    
    fn handle_drag_move(&mut self, screen_x: f32, screen_y: f32) {
        if self.group_select_mode && self.drag_start.is_some() {
            self.drag_current = Some((screen_x, screen_y));
        }
    }
    
    fn handle_drag_end(&mut self) {
        if let (Some(start), Some(end)) = (self.drag_start, self.drag_current) {
            let my_id = if let Some(id) = self.my_id { id } else { 
                self.drag_start = None;
                self.drag_current = None;
                return;
            };
            
            // Convert screen coords to world coords
            let (wx1, wy1) = self.screen_to_world(start.0, start.1);
            let (wx2, wy2) = self.screen_to_world(end.0, end.1);
            
            let min_x = wx1.min(wx2);
            let max_x = wx1.max(wx2);
            let min_y = wy1.min(wy2);
            let max_y = wy1.max(wy2);
            
            // Select all units in the rectangle
            for unit in &mut self.units {
                if unit.owner_id != my_id { continue; }
                
                if unit.x >= min_x && unit.x <= max_x && unit.y >= min_y && unit.y <= max_y {
                    unit.selected = !unit.selected; // Toggle like regular click
                }
            }
            
            self.selected_building = None;
            
            // Auto-disable group select mode after selection is made
            self.group_select_mode = false;
        }
        
        self.drag_start = None;
        self.drag_current = None;
    }
    
    fn generate_wall_preview(&mut self) {
        self.wall_preview.clear();
        
        if let (Some(start), Some(end)) = (self.wall_start, self.wall_end) {
            // Generate line of tiles using Bresenham's line algorithm
            let (x0, y0) = start;
            let (x1, y1) = end;
            
            let dx = (x1 - x0).abs();
            let dy = -(y1 - y0).abs();
            let sx = if x0 < x1 { 1 } else { -1 };
            let sy = if y0 < y1 { 1 } else { -1 };
            let mut err = dx + dy;
            
            let mut x = x0;
            let mut y = y0;
            
            loop {
                // Check if this tile is buildable
                if self.is_tile_buildable(x, y) {
                    self.wall_preview.push((x, y));
                }
                
                if x == x1 && y == y1 { break; }
                
                let e2 = 2 * err;
                if e2 >= dy {
                    err += dy;
                    x += sx;
                }
                if e2 <= dx {
                    err += dx;
                    y += sy;
                }
            }
        }
    }
    
    fn is_tile_buildable(&self, tx: i32, ty: i32) -> bool {
        // Check buildings (Town Center, existing walls)
        for b in &self.buildings {
            if b.tile_x == tx && b.tile_y == ty {
                return false;
            }
        }
        
        // Check units
        for u in &self.units {
            let utx = (u.x / TILE_SIZE_BASE).floor() as i32;
            let uty = (u.y / TILE_SIZE_BASE).floor() as i32;
            if utx == tx && uty == ty {
                return false;
            }
        }
        
        // Check terrain using the tile type (Forest, Mountain, Gold, Water = blocked)
        let cx = tx.div_euclid(CHUNK_SIZE);
        let cy = ty.div_euclid(CHUNK_SIZE);
        let lx = tx.rem_euclid(CHUNK_SIZE);
        let ly = ty.rem_euclid(CHUNK_SIZE);
        
        if let Some(chunk) = self.chunks.get(&(cx, cy)) {
            let idx = (ly * CHUNK_SIZE + lx) as usize;
            if idx < chunk.tiles.len() {
                return matches!(chunk.tiles[idx], TileType::Grass);
            }
        }
        
        // Chunk doesn't exist - use calculate_tile_type to check
        let tile_type = Self::calculate_tile_type(cx, cy, lx, ly);
        matches!(tile_type, TileType::Grass)
    }
    
    fn confirm_wall_build(&mut self) {
        if self.wall_preview.is_empty() {
            self.cancel_wall_build();
            return;
        }
        
        // Send build commands for each tile in preview
        if let Some(ws) = &self.socket {
            for (tx, ty) in &self.wall_preview {
                let msg = GameMessage::Build { kind: 1, tile_x: *tx, tile_y: *ty };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = ws.send_with_str(&json);
                }
            }
        }
        
        // Reset build state
        self.build_mode = false;
        self.wall_start = None;
        self.wall_end = None;
        self.wall_preview.clear();
    }
    
    fn cancel_wall_build(&mut self) {
        self.wall_start = None;
        self.wall_end = None;
        self.wall_preview.clear();
        // Keep build_mode on so user can try again
    }
    
    fn handle_zoom(&mut self, delta_y: f32, mouse_x: f32, mouse_y: f32) {
        // 1. Project mouse screen coords to world coords *before* zoom
        let (world_x, world_y) = self.screen_to_world(mouse_x, mouse_y);

        // 2. Apply Zoom to Target
        let sensitivity = 0.001;
        // Invert delta_y for natural scroll, clamp target
        let new_zoom = self.target_zoom - delta_y * sensitivity;
        self.target_zoom = new_zoom.clamp(0.05, 5.0); // Slightly tighter bounds
        
        // 3. Adjust Camera to keep mouse centered (This is tricky with smooth zoom because camera update needs to happen during the lerp)
        // For now, let's just zoom to center of screen to simplify smooth zoom behavior, 
        // or we can try to adjust camera target? 
        // Simple "Google Maps" style zoom usually zooms to mouse pointer.
        // To do this smoothly, we need to lerp the camera position too?
        // Actually, if we just change zoom, the screen_to_world calculation changes.
        // Let's just stick to smooth zoom level for now, but maybe centering is jarring if not immediate.
        // Let's TRY keeping the camera update immediate but zoom value lagged? No, that wiggles.
        // Let's just apply the camera shift immediately based on the *intended* zoom change?
        
        // Standard RTS zoom usually just zooms to center of screen if using keyboard, or mouse if using wheel.
        // Let's simplify: Zoom towards center of screen for smoothness.
        // Or just accept that smooth zoom + mouse targeting requires complex tweening of both variables.
        
        // Re-implementation:
        // We just set target_zoom. 
        // The update() loop handles the `zoom` value.
        // We also need to drift `camera_x/y` so that `world_x,world_y` stays at `mouse_x,mouse_y`.
        // That's hard to do statelessly in `handle_zoom`.
        
        // Let's just do immediate camera adjustment for the *target* zoom?
        // No, that will jump the camera.
        
        // Let's stick to immediate zoom for now but just interpolate the value? 
        // User said "clunky", which usually means the steps are too big or it snaps.
        // LERPing the zoom value is the standard fix.
        // But we must abandon "zoom to mouse cursor" if we can't lerp the camera too.
        // Let's switch to "Zoom to Center" for consistent smooth feel.
        
        // ... Wait, user said "chunky clunky". 
        // Maybe just lower sensitivity and use LERP is enough.
    }

    fn handle_touch_zoom(&mut self, dist: f32, center_x: f32, center_y: f32) {
        if let Some(last_dist) = self.last_touch_dist {
            let delta = last_dist - dist;
            // Scale delta for zoom sensitivity
            self.handle_zoom(delta * 5.0, center_x, center_y);
        }
        self.last_touch_dist = Some(dist);
    }

    fn handle_pan(&mut self, screen_x: f32, screen_y: f32) {
        if let (Some(lx), Some(ly)) = (self.last_pan_x, self.last_pan_y) {
            let dx = (screen_x - lx) / self.zoom;
            let dy = (screen_y - ly) / self.zoom;
            self.camera_x -= dx;
            self.camera_y -= dy;
        }
        self.last_pan_x = Some(screen_x);
        self.last_pan_y = Some(screen_y);
    }

    fn end_touch(&mut self) {
        self.last_touch_dist = None;
        self.last_pan_x = None;
        self.last_pan_y = None;
    }
}

// --- MAIN LOOP ---

#[wasm_bindgen]
pub fn run_game() -> Result<(), JsValue> {
    let window = web_sys::window().expect("no global `window` exists");
    let document = window.document().expect("should have a document on window");
    let canvas = document.get_element_by_id("temty-canvas")
        .expect("should have #temty-canvas on the page")
        .dyn_into::<HtmlCanvasElement>()?;
    
    canvas.set_width(WIDTH);
    canvas.set_height(HEIGHT);
    
    let context = canvas
        .get_context("2d")?
        .unwrap()
        .dyn_into::<CanvasRenderingContext2d>()?;

    let mut buffer = PixelBuffer::new(WIDTH, HEIGHT);
    let game_state = Rc::new(RefCell::new(GameState::new()));

    // --- WEBSOCKET ---
    let ws = WebSocket::new("wss://temty-server-production.up.railway.app").expect("Failed to connect to WS");
    
    // Assign Socket to GameState
    game_state.borrow_mut().socket = Some(ws.clone());

    // OnOpen - Send Handshake
    {
        let ws_clone = ws.clone();
        let onopen_callback = Closure::wrap(Box::new(move || {
             log("WS Connected. Sending Handshake...");
             // Get token from localStorage
             let window = web_sys::window().unwrap();
             let storage = window.local_storage().unwrap().unwrap();
             let token = storage.get_item("temty_token").unwrap_or(None);
             
             let msg = serde_json::to_string(&GameMessage::Join { version: CLIENT_VERSION, token }).unwrap();
             ws_clone.send_with_str(&msg).expect("Failed to send Join message");
        }) as Box<dyn FnMut()>);
        ws.set_onopen(Some(onopen_callback.as_ref().unchecked_ref()));
        onopen_callback.forget();
    }
    
    // OnClose - Handle disconnection (likely due to server restart/update)
    {
        let onclose_callback = Closure::wrap(Box::new(move || {
            log("WS Disconnected.");
            // Don't auto-reload - it causes unsettling screen flashes.
            // User can manually refresh if needed.
        }) as Box<dyn FnMut()>);
        ws.set_onclose(Some(onclose_callback.as_ref().unchecked_ref()));
        onclose_callback.forget();
    }

    {
        let gs = game_state.clone();
        let onmessage_callback = Closure::wrap(Box::new(move |e: MessageEvent| {
            if let Ok(txt) = e.data().dyn_into::<js_sys::JsString>() {
                let txt: String = txt.into();
                if let Ok(msg) = serde_json::from_str::<GameMessage>(&txt) {
                    let mut state = gs.borrow_mut();
                    match msg {
                        GameMessage::Join { .. } => {}, 
                        GameMessage::Error { message } => {
                            log(&format!("Server Error: {}", message));
                            
                            // Specific handling for version errors
                            if message.contains("Client version") && message.contains("too old") {
                                let window = web_sys::window().unwrap();
                                let document = window.document().unwrap();
                                let body = document.body().unwrap();
                                
                                let overlay = document.create_element("div").unwrap();
                                overlay.set_attribute("style", "position:fixed;top:0;left:0;width:100%;height:100%;background:rgba(0,0,0,0.9);color:red;display:flex;flex-direction:column;justify-content:center;align-items:center;z-index:9999;font-size:24px;text-align:center;padding:20px;").unwrap();
                                
                                overlay.set_inner_html(r#"
                                    <div>⚠️ CLIENT OUTDATED ⚠️</div>
                                    <div style="color:white;font-size:16px;margin-top:10px;">A new version of the game is available.</div>
                                    <div style="color:#aaa;font-size:14px;margin-top:5px;">Please refresh your browser to update.</div>
                                    <button onclick="location.reload(true)" style="margin-top:20px;padding:10px 20px;font-size:18px;cursor:pointer;">Update Now</button>
                                "#);
                                
                                body.append_child(&overlay).unwrap();
                                
                                // Stop the game loop
                                // We can't easily stop the requestAnimationFrame loop from here without structure changes,
                                // but the overlay blocks interaction. Ideally we'd set a flag in GameState.
                                state.my_id = None; // Disable input processing by removing ID
                            } else {
                                web_sys::window().unwrap().alert_with_message(&message).unwrap();
                            }
                        },
                        GameMessage::Welcome { player_id, chunk_x, chunk_y, players, units, buildings, token } => {
                            state.my_id = Some(player_id);
                            state.my_chunk_x = chunk_x;
                            state.my_chunk_y = chunk_y;
                            state.other_players = players.clone();
                            
                            // Save Token
                            let window = web_sys::window().unwrap();
                            let storage = window.local_storage().unwrap().unwrap();
                            storage.set_item("temty_token", &token).unwrap();
                            
                            // Ensure chunks exist and spawn buildings for ALL players (me + others)
                            state.buildings.clear(); // Clear buildings to avoid dupes if any
                            
                            // 1. Add Implicit Town Centers from Player Chunks
                            for p in &players {
                                state.generate_chunk(p.chunk_x, p.chunk_y);
                                let mid = CHUNK_SIZE / 2;
                                state.buildings.push(Building { 
                                    tile_x: p.chunk_x * CHUNK_SIZE + mid, 
                                    tile_y: p.chunk_y * CHUNK_SIZE + mid, 
                                    kind: 0,
                                    owner_id: p.id
                                });
                            }
                            
                            // 2. Add Explicit Buildings from DB
                            for b in buildings {
                                state.buildings.push(Building {
                                    tile_x: b.tile_x,
                                    tile_y: b.tile_y,
                                    kind: b.kind,
                                    owner_id: b.owner_id
                                });
                            }
                            
                            // Move camera to my chunk
                            state.camera_x = (chunk_x as f32 * CHUNK_SIZE as f32 * TILE_SIZE_BASE) + (CHUNK_SIZE as f32 * TILE_SIZE_BASE / 2.0);
                            state.camera_y = (chunk_y as f32 * CHUNK_SIZE as f32 * TILE_SIZE_BASE) + (CHUNK_SIZE as f32 * TILE_SIZE_BASE / 2.0);
                            
                            // Reset Units and Load from Server
                            state.units.clear();
                            
                            for u in units {
                                let color = if Some(u.owner_id) == state.my_id { (0, 0, 255) } else { (255, 0, 0) };
                                state.units.push(Unit {
                                    x: u.x,
                                    y: u.y,
                                    path: Vec::new(), // Server doesn't sync path, units appear idle
                                    selected: false,
                                    kind: 0,
                                    color,
                                    owner_id: u.owner_id,
                                });
                            }

                            log(&format!("Welcome! Assigned to Chunk ({}, {})", chunk_x, chunk_y));
                        },
                        GameMessage::NewPlayer { player } => {
                            // Ignore if it's me (already handled in Welcome)
                            if Some(player.id) == state.my_id {
                                return;
                            }
                            
                            log(&format!("New Player joined at ({}, {})", player.chunk_x, player.chunk_y));
                            state.generate_chunk(player.chunk_x, player.chunk_y);
                            state.other_players.push(player.clone());
                            state.spawn_units_for_player(player.id, player.chunk_x, player.chunk_y);
                        },
                        GameMessage::UnitMove { player_id, unit_idx, x, y } => {
                            if Some(player_id) != state.my_id {
                                // Find the unit
                                let mut count = 0;
                                let mut target_unit_idx = None;
                                
                                for (i, u) in state.units.iter().enumerate() {
                                    if u.owner_id == player_id {
                                        if count == unit_idx {
                                            target_unit_idx = Some(i);
                                            break;
                                        }
                                        count += 1;
                                    }
                                }
                                
                                if let Some(idx) = target_unit_idx {
                                    let start_x = state.units[idx].x;
                                    let start_y = state.units[idx].y;

                                    // CRITICAL FIX: Ensure chunks exist for the path!
                                    let min_wx = start_x.min(x);
                                    let min_wy = start_y.min(y);
                                    let max_wx = start_x.max(x);
                                    let max_wy = start_y.max(y);

                                    let min_cx = (min_wx / (CHUNK_SIZE as f32 * TILE_SIZE_BASE)).floor() as i32;
                                    let min_cy = (min_wy / (CHUNK_SIZE as f32 * TILE_SIZE_BASE)).floor() as i32;
                                    let max_cx = (max_wx / (CHUNK_SIZE as f32 * TILE_SIZE_BASE)).floor() as i32;
                                    let max_cy = (max_wy / (CHUNK_SIZE as f32 * TILE_SIZE_BASE)).floor() as i32;

                                    // Removed aggressive chunk generation for Fog of War logic
                                    // Chunks will generate only when units approach them in update()

                                    let path = state.find_path((start_x, start_y), (x, y));
                                    state.units[idx].path = path;
                                    
                                    // Teleport fallback to prevent desync
                                    if state.units[idx].path.is_empty() {
                                        state.units[idx].x = x;
                                        state.units[idx].y = y;
                                    }
                                }
                            }
                        },
                        GameMessage::UnitSync { player_id, unit_idx, x, y } => {
                             if Some(player_id) != state.my_id {
                                // Snap remote unit to position (or Lerp in future)
                                let mut count = 0;
                                for u in &mut state.units {
                                    if u.owner_id == player_id {
                                        if count == unit_idx {
                                            // LERP or SNAP?
                                            // For now SNAP to ensure sync.
                                            // To make it smooth, we can apply simple easing:
                                            // u.x = u.x + (x - u.x) * 0.5; 
                                            // But let's snap first to fix the "out of sync" complaint.
                                            
                                            // Actually, if we only receive 10 updates/sec, snap looks choppy.
                                            // Let's use a simple smooth approach:
                                            let dist = ((u.x - x).powi(2) + (u.y - y).powi(2)).sqrt();
                                            if dist > 50.0 {
                                                u.x = x;
                                                u.y = y;
                                            } else {
                                                // Smooth lerp (adjust factor for smoothness vs lag)
                                                u.x += (x - u.x) * 0.2;
                                                u.y += (y - u.y) * 0.2;
                                            }
                                            break;
                                        }
                                        count += 1;
                                    }
                                }
                             }
                        },
                        GameMessage::SpawnUnit => {}, // Should not happen on client
                        GameMessage::UnitSpawned { unit } => {
                            // Add new unit
                            let color = if Some(unit.owner_id) == state.my_id { (0, 0, 255) } else { (255, 0, 0) };
                            state.units.push(Unit {
                                x: unit.x,
                                y: unit.y,
                                path: Vec::new(),
                                selected: false,
                                kind: 0,
                                color,
                                owner_id: unit.owner_id,
                            });
                            log("New unit spawned!");
                        },
                        GameMessage::Build { .. } => {}, // Should not be received by client, but good for completeness
                        GameMessage::BuildingSpawned { building } => {
                            state.buildings.push(Building {
                                tile_x: building.tile_x,
                                tile_y: building.tile_y,
                                kind: building.kind,
                                owner_id: building.owner_id
                            });
                            log("New building spawned!");
                        }
                    }
                }
            }
        }) as Box<dyn FnMut(MessageEvent)>);
        ws.set_onmessage(Some(onmessage_callback.as_ref().unchecked_ref()));
        onmessage_callback.forget();
    }

    // --- INPUT ---
    {
        let gs = game_state.clone();
        let closure = Closure::wrap(Box::new(move |event: MouseEvent| {
            let mut gs = gs.borrow_mut();
            let x = event.offset_x() as f32;
            let y = event.offset_y() as f32;
            
            if gs.group_select_mode && gs.drag_start.is_some() {
                gs.handle_drag_move(x, y);
            } else if event.buttons() == 1 {
                gs.handle_pan(x, y);
            } else {
                gs.last_pan_x = Some(x);
                gs.last_pan_y = Some(y);
            }
        }) as Box<dyn FnMut(_)>);
        canvas.add_event_listener_with_callback("mousemove", closure.as_ref().unchecked_ref())?;
        closure.forget();
    }
    
    {
        let gs = game_state.clone();
        let closure = Closure::wrap(Box::new(move |event: MouseEvent| {
            let mut gs = gs.borrow_mut();
            let x = event.offset_x() as f32;
            let y = event.offset_y() as f32;
            
            gs.end_touch();
            
            // Store mouse down position for click vs drag detection
            gs.mouse_down_pos = Some((x, y));
            
            // Start group selection drag if in that mode
            if gs.group_select_mode {
                let footer_height = 60.0;
                if y <= HEIGHT as f32 - footer_height {
                    gs.handle_drag_start(x, y);
                }
            }
        }) as Box<dyn FnMut(_)>);
        canvas.add_event_listener_with_callback("mousedown", closure.as_ref().unchecked_ref())?;
        closure.forget();
    }
    {
        let gs = game_state.clone();
        let closure = Closure::wrap(Box::new(move |event: MouseEvent| {
            let mut gs = gs.borrow_mut();
            let x = event.offset_x() as f32;
            let y = event.offset_y() as f32;
            
            // Check if this was a click (minimal movement) vs a drag/pan
            let was_click = if let Some((start_x, start_y)) = gs.mouse_down_pos {
                let dist = ((x - start_x).powi(2) + (y - start_y).powi(2)).sqrt();
                dist < 15.0  // Must move less than 15 pixels to count as click
            } else {
                false
            };
            
            // Handle drag end first (for group selection)
            gs.handle_drag_end();
            
            // Only trigger click if it was actually a click, not a pan
            if was_click {
                if let Some((start_x, start_y)) = gs.mouse_down_pos {
                    gs.handle_click(start_x, start_y);
                }
            }
            
            gs.mouse_down_pos = None;
            gs.end_touch();
        }) as Box<dyn FnMut(_)>);
        canvas.add_event_listener_with_callback("mouseup", closure.as_ref().unchecked_ref())?;
        closure.forget();
    }
    {
        let gs = game_state.clone();
        let closure = Closure::wrap(Box::new(move |_event: MouseEvent| {
            let mut gs = gs.borrow_mut();
            gs.mouse_down_pos = None;
            gs.handle_drag_end();
            gs.end_touch();
        }) as Box<dyn FnMut(_)>);
        canvas.add_event_listener_with_callback("mouseleave", closure.as_ref().unchecked_ref())?;
        closure.forget();
    }

    {
        let gs = game_state.clone();
        let closure = Closure::wrap(Box::new(move |event: WheelEvent| {
            event.prevent_default();
            gs.borrow_mut().handle_zoom(event.delta_y() as f32, event.offset_x() as f32, event.offset_y() as f32);
        }) as Box<dyn FnMut(_)>);
        canvas.add_event_listener_with_callback("wheel", closure.as_ref().unchecked_ref())?;
        closure.forget();
    }
    
    // --- TOUCH INPUT (Mobile Zoom & Pan) ---
    // touchstart
    {
        let gs = game_state.clone();
        let canvas_clone = canvas.clone();
        let closure = Closure::wrap(Box::new(move |event: TouchEvent| {
            let touches = event.touches();
            let mut gs = gs.borrow_mut();
            
            if touches.length() == 1 {
                event.prevent_default();
                let t = touches.get(0).unwrap();
                let rect = canvas_clone.get_bounding_client_rect();
                let x = t.client_x() as f32 - rect.left() as f32;
                let y = t.client_y() as f32 - rect.top() as f32;
                
                // Store touch start position for tap detection
                gs.mouse_down_pos = Some((x, y));
                gs.touch_is_pan_or_zoom = false;
                
                // Only start drag if not in footer (footer needs button clicks)
                let footer_height = 60.0;
                if gs.group_select_mode && y <= HEIGHT as f32 - footer_height {
                    gs.handle_drag_start(x, y);
                }
                gs.last_pan_x = Some(x);
                gs.last_pan_y = Some(y);
            } else if touches.length() >= 2 {
                // Multi-touch = pinch zoom, not a tap
                gs.touch_is_pan_or_zoom = true;
            }
        }) as Box<dyn FnMut(_)>);
        canvas.add_event_listener_with_callback("touchstart", closure.as_ref().unchecked_ref())?;
        closure.forget();
    }
    // touchmove
    {
        let gs = game_state.clone();
        let canvas_clone = canvas.clone();
        let closure = Closure::wrap(Box::new(move |event: TouchEvent| {
            let touches = event.touches();
            let mut gs = gs.borrow_mut();
            
            if touches.length() == 2 {
                event.prevent_default();
                // Mark as zoom gesture - don't click on release
                gs.touch_is_pan_or_zoom = true;
                
                let t1 = touches.get(0).unwrap();
                let t2 = touches.get(1).unwrap();
                
                let dx = (t1.client_x() - t2.client_x()).abs() as f32;
                let dy = (t1.client_y() - t2.client_y()).abs() as f32;
                let dist = (dx*dx + dy*dy).sqrt();
                
                let cx = (t1.client_x() + t2.client_x()) as f32 / 2.0;
                let cy = (t1.client_y() + t2.client_y()) as f32 / 2.0;
                
                gs.handle_touch_zoom(dist, cx, cy);
            } else if touches.length() == 1 {
                event.prevent_default();
                let t = touches.get(0).unwrap();
                let rect = canvas_clone.get_bounding_client_rect();
                let x = t.client_x() as f32 - rect.left() as f32;
                let y = t.client_y() as f32 - rect.top() as f32;
                
                // Check if we moved enough to be a pan (not a tap)
                if let Some((start_x, start_y)) = gs.mouse_down_pos {
                    let dist = ((x - start_x).powi(2) + (y - start_y).powi(2)).sqrt();
                    if dist > 15.0 {
                        gs.touch_is_pan_or_zoom = true;
                    }
                }
                
                if gs.group_select_mode && gs.drag_start.is_some() {
                    gs.handle_drag_move(x, y);
                } else {
                    gs.handle_pan(x, y);
                }
            }
        }) as Box<dyn FnMut(_)>);
        canvas.add_event_listener_with_callback("touchmove", closure.as_ref().unchecked_ref())?;
        closure.forget();
    }
    // touchend
    {
        let gs = game_state.clone();
        let closure = Closure::wrap(Box::new(move |_event: TouchEvent| {
            let mut gs = gs.borrow_mut();
            
            // Handle group selection drag end
            if gs.group_select_mode && gs.drag_start.is_some() {
                gs.handle_drag_end();
            }
            
            // Only click if this was a tap (not a pan or pinch zoom)
            if !gs.touch_is_pan_or_zoom {
                if let Some((start_x, start_y)) = gs.mouse_down_pos {
                    gs.handle_click(start_x, start_y);
                }
            }
            
            // Reset state
            gs.mouse_down_pos = None;
            gs.touch_is_pan_or_zoom = false;
            gs.end_touch();
        }) as Box<dyn FnMut(_)>);
        canvas.add_event_listener_with_callback("touchend", closure.as_ref().unchecked_ref())?;
        closure.forget();
    }


    // --- RENDER LOOP ---
    let f = Rc::new(RefCell::new(None));
    let g = f.clone();

    let mut last_time = web_sys::window().unwrap().performance().unwrap().now();

    *g.borrow_mut() = Some(Closure::wrap(Box::new(move || {
        let now = web_sys::window().unwrap().performance().unwrap().now();
        let dt = (now - last_time) / 1000.0;
        last_time = now;

        let mut gs = game_state.borrow_mut();
        gs.update(dt);

        buffer.clear(20, 20, 20);

        let zoom = gs.zoom;
        let cam_x = gs.camera_x;
        let cam_y = gs.camera_y;
        let tile_size = TILE_SIZE_BASE * zoom;
        
        let screen_center_x = WIDTH as f32 / 2.0;
        let screen_center_y = HEIGHT as f32 / 2.0;

        // Determine visible area
        let view_w = WIDTH as f32 / zoom;
        let view_h = HEIGHT as f32 / zoom;
        let view_min_x = cam_x - view_w / 2.0;
        let view_min_y = cam_y - view_h / 2.0;
        let view_max_x = cam_x + view_w / 2.0;
        let view_max_y = cam_y + view_h / 2.0;

        // Chunk ranges
        let min_cx = (view_min_x / (CHUNK_SIZE as f32 * TILE_SIZE_BASE)).floor() as i32;
        let max_cx = (view_max_x / (CHUNK_SIZE as f32 * TILE_SIZE_BASE)).floor() as i32;
        let min_cy = (view_min_y / (CHUNK_SIZE as f32 * TILE_SIZE_BASE)).floor() as i32;
        let max_cy = (view_max_y / (CHUNK_SIZE as f32 * TILE_SIZE_BASE)).floor() as i32;

        // Render Chunks
        for cy in min_cy..=max_cy {
            for cx in min_cx..=max_cx {
                // Only render if chunk exists in our known world (created by player spawns)
                if let Some(chunk) = gs.chunks.get(&(cx, cy)) {
                    let chunk_world_x = cx as f32 * CHUNK_SIZE as f32 * TILE_SIZE_BASE;
                    let chunk_world_y = cy as f32 * CHUNK_SIZE as f32 * TILE_SIZE_BASE;

                    for y in 0..CHUNK_SIZE {
                        for x in 0..CHUNK_SIZE {
                            let tile_world_x = chunk_world_x + x as f32 * TILE_SIZE_BASE;
                            let tile_world_y = chunk_world_y + y as f32 * TILE_SIZE_BASE;

                            // Screen Coords
                            let sx = (tile_world_x - cam_x) * zoom + screen_center_x;
                            let sy = (tile_world_y - cam_y) * zoom + screen_center_y;
                            
                            // Optimization: skip if off screen
                            if sx < -(tile_size as f32) || sy < -(tile_size as f32) || sx > WIDTH as f32 || sy > HEIGHT as f32 {
                                continue;
                            }

                            let idx = (y * CHUNK_SIZE + x) as usize;
                            let color = match chunk.tiles[idx] {
                                TileType::Grass => (75, 105, 47),
                                TileType::Water => (50, 89, 165),
                                TileType::Forest => (34, 139, 34),
                                TileType::Mountain => (128, 128, 128),
                                TileType::Gold => (255, 215, 0),
                            };
                            
                            buffer.rect(sx as i32, sy as i32, tile_size.ceil() as i32, tile_size.ceil() as i32, color.0, color.1, color.2);
                            
                            // Detail (simplified)
                            if matches!(chunk.tiles[idx], TileType::Forest) {
                                let small = tile_size * 0.4;
                                buffer.rect((sx + tile_size*0.3) as i32, (sy + tile_size*0.3) as i32, small as i32, small as i32, 20, 80, 20);
                            }
                        }
                    }
                } else {
                    // Void (Black) - do nothing or draw black rect
                    // buffer is cleared to (20,20,20) so it's already dark grey
                }
            }
        }

        // Render Buildings (using TOP-LEFT positioning like tiles)
        for b in &gs.buildings {
            // tile_x, tile_y is the tile position - render at TOP-LEFT like terrain
            let tile_world_x = b.tile_x as f32 * TILE_SIZE_BASE;
            let tile_world_y = b.tile_y as f32 * TILE_SIZE_BASE;
            
            let sx = (tile_world_x - cam_x) * zoom + screen_center_x;
            let sy = (tile_world_y - cam_y) * zoom + screen_center_y;
            
            if b.kind == 0 { // Town Center - 1 tile, same size as everything else
                let size = tile_size;
                let color = if Some(b.owner_id) == gs.my_id { (0, 0, 150) } else { (255, 0, 0) };
                
                // Render at TOP-LEFT (same as tiles)
                buffer.rect(sx as i32, sy as i32, size.ceil() as i32, size.ceil() as i32, color.0, color.1, color.2);
                
                // Inner detail
                let inner = size * 0.4;
                let offset = size * 0.3;
                buffer.rect((sx + offset) as i32, (sy + offset) as i32, inner.ceil() as i32, inner.ceil() as i32, 255, 255, 255);
            } else if b.kind == 1 { // Wall - 2x2 brick pattern, colored by owner
                let size = tile_size;
                let gap = 1.0_f32.max(size * 0.05);
                let sq = ((size - gap) / 2.0).floor() as i32;
                let base_x = sx as i32;
                let base_y = sy as i32;
                let g = gap.ceil() as i32;
                
                // Blue for my walls, red for enemy walls
                let (c1, c2) = if Some(b.owner_id) == gs.my_id {
                    // Blue bricks (lighter/darker alternating)
                    ((80, 80, 180), (50, 50, 140))
                } else {
                    // Red bricks (lighter/darker alternating)
                    ((180, 80, 80), (140, 50, 50))
                };
                
                // 2x2 brick grid with alternating shades
                buffer.rect(base_x, base_y, sq, sq, c1.0, c1.1, c1.2);
                buffer.rect(base_x + sq + g, base_y, sq, sq, c2.0, c2.1, c2.2);
                buffer.rect(base_x, base_y + sq + g, sq, sq, c2.0, c2.1, c2.2);
                buffer.rect(base_x + sq + g, base_y + sq + g, sq, sq, c1.0, c1.1, c1.2);
            }
        }
        
        // --- WALL PREVIEW (transparent blue) ---
        if gs.build_mode {
            // Helper to draw a blue preview brick at tile position
            let draw_preview_brick = |buffer: &mut PixelBuffer, tx: i32, ty: i32| {
                let tile_world_x = tx as f32 * TILE_SIZE_BASE;
                let tile_world_y = ty as f32 * TILE_SIZE_BASE;
                let sx = (tile_world_x - cam_x) * zoom + screen_center_x;
                let sy = (tile_world_y - cam_y) * zoom + screen_center_y;
                let size = tile_size;
                let half = (size / 2.0).floor();
                let gap = 1.0_f32.max(size * 0.05);
                let sq = ((size - gap) / 2.0).floor() as i32;
                let base_x = sx as i32;
                let base_y = sy as i32;
                let g = gap.ceil() as i32;
                
                // Blue preview bricks (same pattern as real walls)
                buffer.rect(base_x, base_y, sq, sq, 50, 100, 200);
                buffer.rect(base_x + sq + g, base_y, sq, sq, 40, 80, 180);
                buffer.rect(base_x, base_y + sq + g, sq, sq, 40, 80, 180);
                buffer.rect(base_x + sq + g, base_y + sq + g, sq, sq, 50, 100, 200);
            };
            
            // Draw start point as blue preview (not just outline)
            if let Some((tx, ty)) = gs.wall_start {
                if gs.wall_end.is_none() {
                    // Only start is set - show single blue preview brick
                    draw_preview_brick(&mut buffer, tx, ty);
                    // Green outline around it
                    let tile_world_x = tx as f32 * TILE_SIZE_BASE;
                    let tile_world_y = ty as f32 * TILE_SIZE_BASE;
                    let sx = (tile_world_x - cam_x) * zoom + screen_center_x;
                    let sy = (tile_world_y - cam_y) * zoom + screen_center_y;
                    buffer.rect_outline(sx as i32, sy as i32, tile_size.ceil() as i32, tile_size.ceil() as i32, 0, 255, 0);
                }
            }
            
            // Draw all preview walls (blue translucent)
            for (tx, ty) in &gs.wall_preview {
                draw_preview_brick(&mut buffer, *tx, *ty);
            }
        }

        // Render Units
        for u in &gs.units {
            let sx = (u.x - cam_x) * zoom + screen_center_x;
            let sy = (u.y - cam_y) * zoom + screen_center_y;
            
            // Always draw paths for selected units (even if unit is off-screen)
            if u.selected && !u.path.is_empty() {
                let mut prev_sx = sx as i32;
                let mut prev_sy = sy as i32;
                
                for point in u.path.iter().rev() {
                    let p_sx = ((point.0 - cam_x) * zoom + screen_center_x) as i32;
                    let p_sy = ((point.1 - cam_y) * zoom + screen_center_y) as i32;
                    
                    buffer.line(prev_sx, prev_sy, p_sx, p_sy, 255, 255, 255, true);
                    prev_sx = p_sx;
                    prev_sy = p_sy;
                }
            }
            
            // Only render unit sprite if on screen
            if sx > -50.0 && sx < WIDTH as f32 + 50.0 && sy > -50.0 && sy < HEIGHT as f32 + 50.0 {
                let w = tile_size * 0.6;
                let unit_draw_x = sx - w/2.0;
                let unit_draw_y = sy - w/2.0;

                if u.selected {
                    let box_size = tile_size * 1.2;
                    buffer.rect_outline((sx - box_size/2.0) as i32, (sy - box_size/2.0) as i32, box_size as i32, box_size as i32, 0, 255, 0);
                }
                
                buffer.rect(unit_draw_x as i32, unit_draw_y as i32, w as i32, w as i32, u.color.0, u.color.1, u.color.2);
            }
        }

        // HUD / Debug Info
        buffer.rect(0, 0, WIDTH as i32, 30, 50, 50, 50);
        
        // Minimap/Status
        let status_color = if gs.my_id.is_some() { (0, 255, 0) } else { (100, 100, 100) };
        buffer.rect(10, 10, 10, 10, status_color.0, status_color.1, status_color.2);

        // Display other players (Red Dots) - Still useful for debugging
        for p in &gs.other_players {
             let cx = p.chunk_x;
             let cy = p.chunk_y;
             // Draw at center of their chunk
             let px = (cx as f32 * CHUNK_SIZE as f32 * TILE_SIZE_BASE) + (CHUNK_SIZE as f32 * TILE_SIZE_BASE / 2.0);
             let py = (cy as f32 * CHUNK_SIZE as f32 * TILE_SIZE_BASE) + (CHUNK_SIZE as f32 * TILE_SIZE_BASE / 2.0);
             
             let sx = (px - cam_x) * zoom + screen_center_x;
             let sy = (py - cam_y) * zoom + screen_center_y;
             
             let size = tile_size * 0.8;
             // buffer.rect((sx - size/2.0) as i32, (sy - size/2.0) as i32, size as i32, size as i32, 255, 0, 0);
        }
        
        // --- DRAG SELECTION BOX ---
        if let (Some(start), Some(end)) = (gs.drag_start, gs.drag_current) {
            let x1 = start.0.min(end.0) as i32;
            let y1 = start.1.min(end.1) as i32;
            let x2 = start.0.max(end.0) as i32;
            let y2 = start.1.max(end.1) as i32;
            buffer.rect_outline(x1, y1, x2 - x1, y2 - y1, 0, 255, 0);
        }

        // --- UI OVERLAY (Footer) ---
        let footer_height = 60.0; // Reduced from 100
        buffer.rect(0, (HEIGHT as f32 - footer_height) as i32, WIDTH as i32, footer_height as i32, 40, 40, 40);
        
        let btn_size = 40.0;
        let home_btn_x = (WIDTH as f32 - btn_size) / 2.0;
        let home_btn_y = HEIGHT as f32 - footer_height + (footer_height - btn_size) / 2.0;
        
        // Home Button (Center) - Dark Blue with white inner
        buffer.rect(home_btn_x as i32, home_btn_y as i32, btn_size as i32, btn_size as i32, 0, 0, 150);
        buffer.rect((home_btn_x + 13.0) as i32, (home_btn_y + 13.0) as i32, 14, 14, 255, 255, 255);
        
        // Group Select Button (Right of Home) - Simple box outline icon
        let group_btn_x = home_btn_x + btn_size + 10.0;
        let group_btn_y = home_btn_y;
        let group_color = if gs.group_select_mode { (0, 200, 0) } else { (80, 80, 80) };
        buffer.rect(group_btn_x as i32, group_btn_y as i32, btn_size as i32, btn_size as i32, group_color.0, group_color.1, group_color.2);
        // Clean selection box icon
        buffer.rect_outline((group_btn_x + 8.0) as i32, (group_btn_y + 8.0) as i32, 24, 24, 255, 255, 255);

        // Action Button (Left) - Context-dependent
        if let Some(b_idx) = gs.selected_building {
             if b_idx < gs.buildings.len() && Some(gs.buildings[b_idx].owner_id) == gs.my_id && gs.buildings[b_idx].kind == 0 {
                 // Spawn Worker Button
                 buffer.rect(10, home_btn_y as i32, btn_size as i32, btn_size as i32, 0, 0, 150);
                 // Plus icon
                 buffer.rect(18, (home_btn_y + 17.0) as i32, 24, 6, 0, 255, 0);
                 buffer.rect(27, (home_btn_y + 8.0) as i32, 6, 24, 0, 255, 0);
             }
        } else {
            let any_unit_selected = gs.units.iter().any(|u| u.selected && Some(u.owner_id) == gs.my_id);
            if any_unit_selected {
                 // Build Wall Button - changes color when build mode active
                 let build_color = if gs.build_mode { (0, 200, 0) } else { (0, 0, 150) };
                 buffer.rect(10, home_btn_y as i32, btn_size as i32, btn_size as i32, build_color.0, build_color.1, build_color.2);
                 // Centered 2x2 brick icon (10x10 squares with 4px gap)
                 let sq = 10; // square size
                 let gap = 4;
                 let grid_size = sq * 2 + gap; // 24
                 let offset_x = 10 + ((btn_size as i32 - grid_size) / 2); // center horizontally
                 let offset_y = home_btn_y as i32 + ((btn_size as i32 - grid_size) / 2); // center vertically
                 buffer.rect(offset_x, offset_y, sq, sq, 150, 150, 150);
                 buffer.rect(offset_x + sq + gap, offset_y, sq, sq, 130, 130, 130);
                 buffer.rect(offset_x, offset_y + sq + gap, sq, sq, 130, 130, 130);
                 buffer.rect(offset_x + sq + gap, offset_y + sq + gap, sq, sq, 150, 150, 150);
            }
        }
        
        // --- CONFIRM/CANCEL BUTTONS (when wall placement ready) ---
        if gs.build_mode && gs.wall_end.is_some() {
            // Confirm button (green) - right side
            let confirm_btn_x = WIDTH as f32 - btn_size - 60.0;
            let confirm_btn_y = home_btn_y;
            buffer.rect(confirm_btn_x as i32, confirm_btn_y as i32, btn_size as i32, btn_size as i32, 0, 150, 0);
            // Checkmark icon
            buffer.rect((confirm_btn_x + 10.0) as i32, (confirm_btn_y + 20.0) as i32, 8, 4, 255, 255, 255);
            buffer.rect((confirm_btn_x + 16.0) as i32, (confirm_btn_y + 10.0) as i32, 4, 18, 255, 255, 255);
            
            // Cancel button (red) - right of confirm
            let cancel_btn_x = WIDTH as f32 - btn_size - 10.0;
            let cancel_btn_y = home_btn_y;
            buffer.rect(cancel_btn_x as i32, cancel_btn_y as i32, btn_size as i32, btn_size as i32, 150, 0, 0);
            // X icon
            buffer.rect((cancel_btn_x + 12.0) as i32, (cancel_btn_y + 10.0) as i32, 16, 4, 255, 255, 255);
            buffer.rect((cancel_btn_x + 12.0) as i32, (cancel_btn_y + 26.0) as i32, 16, 4, 255, 255, 255);
            buffer.rect((cancel_btn_x + 12.0) as i32, (cancel_btn_y + 10.0) as i32, 4, 20, 255, 255, 255);
            buffer.rect((cancel_btn_x + 24.0) as i32, (cancel_btn_y + 10.0) as i32, 4, 20, 255, 255, 255);
        }
        
        // --- BUILD MODE INDICATOR ---
        if gs.build_mode {
            // Show current state above footer
            let msg_y = HEIGHT as f32 - footer_height - 25.0;
            if gs.wall_start.is_none() {
                // "Click to set start point" - just show a small green dot indicator
                buffer.rect(10, msg_y as i32, 8, 8, 0, 255, 0);
            } else if gs.wall_end.is_none() {
                // "Click to set end point" - show start + yellow indicator
                buffer.rect(10, msg_y as i32, 8, 8, 255, 255, 0);
            } else {
                // "Confirm or Cancel" - show blue indicator
                buffer.rect(10, msg_y as i32, 8, 8, 50, 150, 255);
            }
        }
        
        // --- SELECTED UNITS DISPLAY (Above footer, left side) ---
        let selected_units: Vec<_> = gs.units.iter().filter(|u| u.selected && Some(u.owner_id) == gs.my_id).collect();
        if !selected_units.is_empty() {
            let unit_icon_size = 20.0;
            let icons_y = HEIGHT as f32 - footer_height - unit_icon_size - 5.0;
            let max_display = 10; // Max icons to show
            
            for (i, u) in selected_units.iter().take(max_display).enumerate() {
                let icon_x = 10.0 + (i as f32 * (unit_icon_size + 4.0));
                // Draw unit color square
                buffer.rect(icon_x as i32, icons_y as i32, unit_icon_size as i32, unit_icon_size as i32, u.color.0, u.color.1, u.color.2);
                // Border
                buffer.rect_outline(icon_x as i32, icons_y as i32, unit_icon_size as i32, unit_icon_size as i32, 255, 255, 255);
            }
            
            // If more than max, show "+N"
            if selected_units.len() > max_display {
                let extra = selected_units.len() - max_display;
                let text_x = 10.0 + (max_display as f32 * (unit_icon_size + 4.0));
                // Simple "+N" indicator (just a small box with different color)
                buffer.rect(text_x as i32, icons_y as i32, unit_icon_size as i32, unit_icon_size as i32, 100, 100, 100);
                // Plus sign to indicate "more"
                buffer.rect((text_x + 7.0) as i32, (icons_y + 9.0) as i32, 6, 2, 255, 255, 255);
                buffer.rect((text_x + 9.0) as i32, (icons_y + 7.0) as i32, 2, 6, 255, 255, 255);
            }
        }
        
        // Draw Selection Outline for Building (TOP-LEFT based like tiles)
        if let Some(b_idx) = gs.selected_building {
            if b_idx < gs.buildings.len() {
                let b = &gs.buildings[b_idx];
                let tile_world_x = b.tile_x as f32 * TILE_SIZE_BASE;
                let tile_world_y = b.tile_y as f32 * TILE_SIZE_BASE;
                let sx = (tile_world_x - cam_x) * zoom + screen_center_x;
                let sy = (tile_world_y - cam_y) * zoom + screen_center_y;
                // Green outline around the tile
                buffer.rect_outline(sx as i32 - 2, sy as i32 - 2, tile_size.ceil() as i32 + 4, tile_size.ceil() as i32 + 4, 0, 255, 0);
            }
        }

        let image_data = ImageData::new_with_u8_clamped_array_and_sh(
            Clamped(&buffer.pixels), WIDTH, HEIGHT).unwrap();
        context.put_image_data(&image_data, 0.0, 0.0).unwrap();

        request_animation_frame(f.borrow().as_ref().unwrap());
    }) as Box<dyn FnMut()>));

    request_animation_frame(g.borrow().as_ref().unwrap());
    Ok(())
}

fn request_animation_frame(f: &Closure<dyn FnMut()>) {
    web_sys::window()
        .expect("no global `window` exists")
        .request_animation_frame(f.as_ref().unchecked_ref())
        .expect("should register `requestAnimationFrame` OK");
}
