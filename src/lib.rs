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
#[serde(tag = "type")]
enum GameMessage {
    Join { version: u32, token: Option<String> },
    Welcome { player_id: i32, chunk_x: i32, chunk_y: i32, players: Vec<PlayerInfo>, units: Vec<UnitDTO>, token: String },
    NewPlayer { player: PlayerInfo },
    UnitMove { player_id: i32, unit_idx: usize, x: f32, y: f32 },
    Error { message: String },
}

const CLIENT_VERSION: u32 = 7;

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
const CHUNK_SIZE: i32 = 64; // User requested 64x64 plot per player
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

    fn generate_chunk(&mut self, cx: i32, cy: i32) {
        if self.chunks.contains_key(&(cx, cy)) { return; }
        
        let mut tiles = vec![TileType::Grass; (CHUNK_SIZE * CHUNK_SIZE) as usize];
        for y in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                let idx = (y * CHUNK_SIZE + x) as usize;
                // Pseudo-random seed based on coordinates for consistency
                let seed = ((cx as i64 * 73856093) ^ (cy as i64 * 19349663) ^ (x as i64 * 83492791) ^ (y as i64 * 23492871)) as f64;
                let r = (seed.sin() * 10000.0).fract().abs();
                
                // Reduced water frequency from 0.1 to 0.03
                if r < 0.03 { tiles[idx] = TileType::Water; }
                else if r < 0.25 { tiles[idx] = TileType::Forest; }
                else if r < 0.28 { tiles[idx] = TileType::Mountain; }
                else if r < 0.30 { tiles[idx] = TileType::Gold; }
            }
        }
        
        // Ensure walkability for Town Center (simplified: clear center of ANY chunk where a player spawns)
        // Center is at CHUNK_SIZE / 2
        let mid = CHUNK_SIZE / 2;
        
        // Clear a 5x5 area in the center for Building + Units
        for y in (mid-3)..(mid+4) {
            for x in (mid-3)..(mid+4) {
                if x >= 0 && x < CHUNK_SIZE && y >= 0 && y < CHUNK_SIZE {
                    tiles[(y * CHUNK_SIZE + x) as usize] = TileType::Grass;
                }
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
            None
        }
    }

    fn is_tile_walkable(&self, gx: i32, gy: i32) -> bool {
        match self.get_tile_type(gx, gy) {
            Some(t) => match t {
                TileType::Water | TileType::Forest | TileType::Mountain | TileType::Gold => return false,
                _ => {}
            },
            None => return false, // Cannot walk in void
        }
        
        // Check Buildings
        for b in &self.buildings {
            if gx >= b.tile_x - 1 && gx <= b.tile_x + 1 && gy >= b.tile_y - 1 && gy <= b.tile_y + 1 {
                return false;
            }
        }
        true
    }

    fn find_path(&self, start: (f32, f32), end: (f32, f32)) -> Vec<(f32, f32)> {
        let start_tx = (start.0 / TILE_SIZE_BASE).floor() as i32;
        let start_ty = (start.1 / TILE_SIZE_BASE).floor() as i32;
        let end_tx = (end.0 / TILE_SIZE_BASE).floor() as i32;
        let end_ty = (end.1 / TILE_SIZE_BASE).floor() as i32;

        if start_tx == end_tx && start_ty == end_ty { return vec![end]; }
        if !self.is_tile_walkable(end_tx, end_ty) { return vec![]; }

        // Limit pathfinding search space for performance (e.g., 50 tile radius)
        if (start_tx - end_tx).abs() > 100 || (start_ty - end_ty).abs() > 100 { return vec![]; }

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
            if steps > 2000 { break; } // Safety break

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

    fn update(&mut self) {
        let speed = 0.3;
        let separation_radius = 10.0;
        let separation_force = 0.06;

        let unit_positions: Vec<(f32, f32)> = self.units.iter().map(|u| (u.x, u.y)).collect();
        let mut updates: Vec<(usize, f32, f32, bool)> = Vec::new();

        for (i, unit) in self.units.iter().enumerate() {
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

            // Separation
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

            let new_x = unit.x + dx;
            let new_y = unit.y + dy;

            // Collision
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
        }

        for (i, x, y, pop) in updates {
            let u = &mut self.units[i];
            u.x = x;
            u.y = y;
            if pop { u.path.pop(); }
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
        let (wx, wy) = self.screen_to_world(screen_x, screen_y);
        let my_id = if let Some(id) = self.my_id { id } else { return };

        let mut clicked_unit = false;
        
        for unit in &mut self.units {
            if unit.owner_id != my_id { continue; }

            // Unit hit box approx 16x16 (radius 8)
            let dx = (unit.x - wx).abs();
            let dy = (unit.y - wy).abs();
            
            // Check if click is within 10 units of center
            if dx < 10.0 && dy < 10.0 {
                unit.selected = !unit.selected;
                clicked_unit = true;
                break;
            }
        }

        if !clicked_unit {
            // Move command
            let mut paths = Vec::new();
            let mut move_commands = Vec::new();
            
            // First, calculate paths for local update
            for (i, unit) in self.units.iter().enumerate() {
                if unit.selected && unit.owner_id == my_id {
                    let path = self.find_path((unit.x, unit.y), (wx, wy));
                    if !path.is_empty() {
                        paths.push((i, path));
                        
                        // Calculate relative index for this unit within my units
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
            }
            
            // Update local state
            for (i, path) in paths {
                self.units[i].path = path;
            }
            
            // Send commands
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
    
    fn handle_zoom(&mut self, delta_y: f32, mouse_x: f32, mouse_y: f32) {
        // 1. Project mouse screen coords to world coords *before* zoom
        let (world_x, world_y) = self.screen_to_world(mouse_x, mouse_y);

        // 2. Apply Zoom
        let sensitivity = 0.001;
        let new_zoom = self.zoom - delta_y * sensitivity;
        // Allow infinite zoom out practically (0.01)
        let new_zoom = new_zoom.clamp(0.01, 10.0);
        
        if (new_zoom - self.zoom).abs() < 0.0001 { return; }
        self.zoom = new_zoom;

        // 3. We want (world_x, world_y) to still be under (mouse_x, mouse_y)
        // New World calc: (screen - center) / new_zoom + new_cam = world
        // Therefore: new_cam = world - (screen - center) / new_zoom
        
        let center_x = WIDTH as f32 / 2.0;
        let center_y = HEIGHT as f32 / 2.0;
        
        self.camera_x = world_x - (mouse_x - center_x) / self.zoom;
        self.camera_y = world_y - (mouse_y - center_y) / self.zoom;
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
                        GameMessage::Welcome { player_id, chunk_x, chunk_y, players, units, token } => {
                            state.my_id = Some(player_id);
                            state.my_chunk_x = chunk_x;
                            state.my_chunk_y = chunk_y;
                            state.other_players = players.clone();
                            
                            // Save Token
                            let window = web_sys::window().unwrap();
                            let storage = window.local_storage().unwrap().unwrap();
                            storage.set_item("temty_token", &token).unwrap();
                            
                            // Ensure chunks exist and spawn buildings for ALL players (me + others)
                            // Note: players includes existing players. Does it include me? 
                            // Server logic: "existing_players" = values of map BEFORE inserting me (Wait, check server code)
                            // Server: 
                            //   gs.players.insert(me); 
                            //   existing_players = gs.players.values().collect();
                            // So YES, "players" list in Welcome includes ME.
                            
                            state.buildings.clear(); // Clear buildings to avoid dupes if any
                            
                            for p in &players {
                                state.generate_chunk(p.chunk_x, p.chunk_y);
                                
                                // Spawn Building
                                let mid = CHUNK_SIZE / 2;
                                state.buildings.push(Building { 
                                    tile_x: p.chunk_x * CHUNK_SIZE + mid, 
                                    tile_y: p.chunk_y * CHUNK_SIZE + mid, 
                                    kind: 0,
                                    owner_id: p.id
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

                                    // Expand by 1 chunk padding
                                    for cy in (min_cy - 1)..=(max_cy + 1) {
                                        for cx in (min_cx - 1)..=(max_cx + 1) {
                                            if !state.chunks.contains_key(&(cx, cy)) {
                                                state.generate_chunk(cx, cy);
                                            }
                                        }
                                    }

                                    let path = state.find_path((start_x, start_y), (x, y));
                                    state.units[idx].path = path;
                                    
                                    // Teleport fallback to prevent desync
                                    if state.units[idx].path.is_empty() {
                                        state.units[idx].x = x;
                                        state.units[idx].y = y;
                                    }
                                }
                            }
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
            if event.buttons() == 1 {
                gs.handle_pan(event.offset_x() as f32, event.offset_y() as f32);
            } else {
                gs.last_pan_x = Some(event.offset_x() as f32);
                gs.last_pan_y = Some(event.offset_y() as f32);
            }
        }) as Box<dyn FnMut(_)>);
        canvas.add_event_listener_with_callback("mousemove", closure.as_ref().unchecked_ref())?;
        closure.forget();
    }
    
    {
        let gs = game_state.clone();
        let closure = Closure::wrap(Box::new(move |event: MouseEvent| {
            gs.borrow_mut().end_touch(); // Reset pan state
            gs.borrow_mut().handle_click(event.offset_x() as f32, event.offset_y() as f32);
        }) as Box<dyn FnMut(_)>);
        canvas.add_event_listener_with_callback("mousedown", closure.as_ref().unchecked_ref())?;
        closure.forget();
    }
    {
        let gs = game_state.clone();
        let closure = Closure::wrap(Box::new(move |_event: MouseEvent| {
            gs.borrow_mut().end_touch();
        }) as Box<dyn FnMut(_)>);
        canvas.add_event_listener_with_callback("mouseup", closure.as_ref().unchecked_ref())?;
        closure.forget();
    }
    {
        let gs = game_state.clone();
        let closure = Closure::wrap(Box::new(move |_event: MouseEvent| {
            gs.borrow_mut().end_touch();
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
    {
        let gs = game_state.clone();
        let closure = Closure::wrap(Box::new(move |event: TouchEvent| {
            let touches = event.touches();
            if touches.length() == 2 {
                event.prevent_default(); 
                let t1 = touches.get(0).unwrap();
                let t2 = touches.get(1).unwrap();
                
                let dx = (t1.client_x() - t2.client_x()).abs() as f32;
                let dy = (t1.client_y() - t2.client_y()).abs() as f32;
                let dist = (dx*dx + dy*dy).sqrt();
                
                let cx = (t1.client_x() + t2.client_x()) as f32 / 2.0;
                let cy = (t1.client_y() + t2.client_y()) as f32 / 2.0;
                
                gs.borrow_mut().handle_touch_zoom(dist, cx, cy);
            } else if touches.length() == 1 {
                // Pan
                event.prevent_default();
                let t = touches.get(0).unwrap();
                gs.borrow_mut().handle_pan(t.client_x() as f32, t.client_y() as f32);
            }
        }) as Box<dyn FnMut(_)>);
        canvas.add_event_listener_with_callback("touchmove", closure.as_ref().unchecked_ref())?;
        closure.forget();
    }
    {
        let gs = game_state.clone();
        let closure = Closure::wrap(Box::new(move |_event: TouchEvent| {
             gs.borrow_mut().end_touch();
        }) as Box<dyn FnMut(_)>);
        canvas.add_event_listener_with_callback("touchend", closure.as_ref().unchecked_ref())?;
        closure.forget();
    }


    // --- RENDER LOOP ---
    let f = Rc::new(RefCell::new(None));
    let g = f.clone();

    *g.borrow_mut() = Some(Closure::wrap(Box::new(move || {
        let mut gs = game_state.borrow_mut();
        gs.update();

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

        // Render Buildings
        for b in &gs.buildings {
            let bx = b.tile_x as f32 * TILE_SIZE_BASE;
            let by = b.tile_y as f32 * TILE_SIZE_BASE;
            
            let sx = (bx - cam_x) * zoom + screen_center_x;
            let sy = (by - cam_y) * zoom + screen_center_y;
            let size = tile_size * 1.5;

            // Color logic: Blue for me, Red for enemy
            let color = if Some(b.owner_id) == gs.my_id { (0, 0, 255) } else { (255, 0, 0) };
            
            buffer.rect((sx - size/2.0) as i32, (sy - size/2.0) as i32, size as i32, size as i32, color.0, color.1, color.2);
        }

        // Render Units
        for u in &gs.units {
            let sx = (u.x - cam_x) * zoom + screen_center_x;
            let sy = (u.y - cam_y) * zoom + screen_center_y;
            
            // Only render if on screen
            if sx > -50.0 && sx < WIDTH as f32 + 50.0 && sy > -50.0 && sy < HEIGHT as f32 + 50.0 {
                let w = tile_size * 0.6;
                
                // Draw Unit Center (Offset by half width to center it on x,y)
                let unit_draw_x = sx - w/2.0;
                let unit_draw_y = sy - w/2.0;

                if u.selected {
                    let box_size = tile_size * 1.2;
                    // Center selection box on x,y
                    buffer.rect_outline((sx - box_size/2.0) as i32, (sy - box_size/2.0) as i32, box_size as i32, box_size as i32, 0, 255, 0);
                    
                    // Path
                    if !u.path.is_empty() {
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
