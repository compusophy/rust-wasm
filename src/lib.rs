use std::cell::RefCell;
use std::rc::Rc;
use std::collections::{BinaryHeap, HashMap};
use std::cmp::Ordering;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::Clamped;
use web_sys::{WebSocket, HtmlCanvasElement, CanvasRenderingContext2d, ImageData, MouseEvent, WheelEvent, MessageEvent};
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
    id: u32,
    chunk_x: i32,
    chunk_y: i32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type")]
enum GameMessage {
    Welcome { player_id: u32, chunk_x: i32, chunk_y: i32, players: Vec<PlayerInfo> },
    NewPlayer { player: PlayerInfo },
    PlayerMove { player_id: u32, x: f32, y: f32 },
}

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
    pub fn send_message(&self, msg: &str) -> Result<(), JsValue> {
        self.socket.send_with_str(msg)
    }
}

// --- PIXEL BUFFER ENGINE ---
const WIDTH: u32 = 800;
const HEIGHT: u32 = 600;
const CHUNK_SIZE: i32 = 32;
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
}

struct Building {
    tile_x: i32, // Global Tile Pos
    tile_y: i32,
    kind: u8,
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
    my_id: Option<u32>,
    my_chunk_x: i32,
    my_chunk_y: i32,
    other_players: Vec<PlayerInfo>,
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
        };

        // Generate Initial Chunk (0,0)
        gs.generate_chunk(0, 0);
        
        // Center camera on 0,0 chunk center roughly
        gs.camera_x = (CHUNK_SIZE as f32 * TILE_SIZE_BASE) / 2.0;
        gs.camera_y = (CHUNK_SIZE as f32 * TILE_SIZE_BASE) / 2.0;

        // Place Town Center at 0,0 chunk center
        let cx = CHUNK_SIZE / 2;
        let cy = CHUNK_SIZE / 2;
        gs.buildings.push(Building { tile_x: cx, tile_y: cy, kind: 0 });

        // Add Villagers
        let sx = (cx as f32 * TILE_SIZE_BASE) as f32;
        let sy = (cy as f32 * TILE_SIZE_BASE) as f32;
        gs.units.push(Unit { x: sx + 30.0, y: sy + 30.0, path: Vec::new(), selected: false, kind: 0, color: (0, 0, 255) });
        gs.units.push(Unit { x: sx - 20.0, y: sy + 40.0, path: Vec::new(), selected: false, kind: 0, color: (0, 0, 255) });

        gs
    }

    fn generate_chunk(&mut self, cx: i32, cy: i32) {
        if self.chunks.contains_key(&(cx, cy)) { return; }
        
        let mut tiles = vec![TileType::Grass; (CHUNK_SIZE * CHUNK_SIZE) as usize];
        for y in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                let idx = (y * CHUNK_SIZE + x) as usize;
                let r = random();
                if r < 0.1 { tiles[idx] = TileType::Water; }
                else if r < 0.25 { tiles[idx] = TileType::Forest; }
                else if r < 0.28 { tiles[idx] = TileType::Mountain; }
                else if r < 0.30 { tiles[idx] = TileType::Gold; }
            }
        }
        
        // Ensure walkability for Town Center if at 0,0
        if cx == 0 && cy == 0 {
            let mid = CHUNK_SIZE / 2;
            for y in (mid-2)..(mid+3) {
                for x in (mid-2)..(mid+3) {
                    if x < CHUNK_SIZE && y < CHUNK_SIZE {
                        tiles[(y * CHUNK_SIZE + x) as usize] = TileType::Grass;
                    }
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

        let mut clicked_unit = false;
        // Check unit click (approximate radius 10/zoom?)
        // Actually units are drawn at world pos, so check world pos
        
        for unit in &mut self.units {
            // Unit hit box approx 16x16
            if wx >= unit.x - 5.0 && wx <= unit.x + 15.0 && wy >= unit.y - 5.0 && wy <= unit.y + 15.0 {
                unit.selected = !unit.selected;
                clicked_unit = true;
                break;
            }
        }

        if !clicked_unit {
            // Move command
            let mut paths = Vec::new();
            for (i, unit) in self.units.iter().enumerate() {
                if unit.selected {
                    let path = self.find_path((unit.x, unit.y), (wx, wy));
                    paths.push((i, path));
                }
            }
            for (i, path) in paths {
                self.units[i].path = path;
            }
        }
    }
    
    fn handle_zoom(&mut self, delta_y: f32) {
        let sensitivity = 0.001;
        let new_zoom = self.zoom - delta_y * sensitivity;
        self.zoom = new_zoom.clamp(0.2, 4.0);
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
    {
        let gs = game_state.clone();
        let onmessage_callback = Closure::wrap(Box::new(move |e: MessageEvent| {
            if let Ok(txt) = e.data().dyn_into::<js_sys::JsString>() {
                let txt: String = txt.into();
                if let Ok(msg) = serde_json::from_str::<GameMessage>(&txt) {
                    let mut state = gs.borrow_mut();
                    match msg {
                        GameMessage::Welcome { player_id, chunk_x, chunk_y, players } => {
                            state.my_id = Some(player_id);
                            state.my_chunk_x = chunk_x;
                            state.my_chunk_y = chunk_y;
                            state.other_players = players;
                            
                            // Ensure initial chunk exists
                            state.generate_chunk(chunk_x, chunk_y);
                            
                            // Move camera to this chunk
                            state.camera_x = (chunk_x as f32 * CHUNK_SIZE as f32 * TILE_SIZE_BASE) + (CHUNK_SIZE as f32 * TILE_SIZE_BASE / 2.0);
                            state.camera_y = (chunk_y as f32 * CHUNK_SIZE as f32 * TILE_SIZE_BASE) + (CHUNK_SIZE as f32 * TILE_SIZE_BASE / 2.0);
                            
                            log(&format!("Welcome! Assigned to Chunk ({}, {})", chunk_x, chunk_y));
                        },
                        GameMessage::NewPlayer { player } => {
                            log(&format!("New Player joined at ({}, {})", player.chunk_x, player.chunk_y));
                            // Generate/Track the new player's chunk
                            state.generate_chunk(player.chunk_x, player.chunk_y);
                            state.other_players.push(player);
                        },
                        GameMessage::PlayerMove { .. } => {}
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
            gs.borrow_mut().handle_click(event.offset_x() as f32, event.offset_y() as f32);
        }) as Box<dyn FnMut(_)>);
        canvas.add_event_listener_with_callback("mousedown", closure.as_ref().unchecked_ref())?;
        closure.forget();
    }
    {
        let gs = game_state.clone();
        let closure = Closure::wrap(Box::new(move |event: WheelEvent| {
            event.prevent_default();
            gs.borrow_mut().handle_zoom(event.delta_y() as f32);
        }) as Box<dyn FnMut(_)>);
        canvas.add_event_listener_with_callback("wheel", closure.as_ref().unchecked_ref())?;
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
        // Viewport World Rect: [cam_x - W/2/z, cam_x + W/2/z]
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

            buffer.rect((sx - size/2.0) as i32, (sy - size/2.0) as i32, size as i32, size as i32, 160, 82, 45);
        }

        // Render Units
        for u in &gs.units {
            let sx = (u.x - cam_x) * zoom + screen_center_x;
            let sy = (u.y - cam_y) * zoom + screen_center_y;
            
            if u.selected {
                let box_size = tile_size * 1.2;
                buffer.rect_outline((sx - 2.0) as i32, (sy - 2.0) as i32, box_size as i32, box_size as i32, 0, 255, 0);
                
                // Path
                if !u.path.is_empty() {
                    let mut prev_sx = sx as i32 + 3;
                    let mut prev_sy = sy as i32 + 3;
                    
                    for point in u.path.iter().rev() {
                        let p_sx = ((point.0 - cam_x) * zoom + screen_center_x) as i32;
                        let p_sy = ((point.1 - cam_y) * zoom + screen_center_y) as i32;
                        
                        buffer.line(prev_sx, prev_sy, p_sx, p_sy, 255, 255, 255, true);
                        prev_sx = p_sx;
                        prev_sy = p_sy;
                    }
                }
            }
            
            let w = tile_size * 0.6;
            buffer.rect(sx as i32, sy as i32, w as i32, w as i32, u.color.0, u.color.1, u.color.2);
        }

        // HUD / Debug Info
        buffer.rect(0, 0, WIDTH as i32, 30, 50, 50, 50);
        
        // Minimap/Status
        let status_color = if gs.my_id.is_some() { (0, 255, 0) } else { (100, 100, 100) };
        buffer.rect(10, 10, 10, 10, status_color.0, status_color.1, status_color.2);

        // Display other players
        for p in &gs.other_players {
             let cx = p.chunk_x;
             let cy = p.chunk_y;
             // Draw at center of their chunk
             let px = (cx as f32 * CHUNK_SIZE as f32 * TILE_SIZE_BASE) + (CHUNK_SIZE as f32 * TILE_SIZE_BASE / 2.0);
             let py = (cy as f32 * CHUNK_SIZE as f32 * TILE_SIZE_BASE) + (CHUNK_SIZE as f32 * TILE_SIZE_BASE / 2.0);
             
             let sx = (px - cam_x) * zoom + screen_center_x;
             let sy = (py - cam_y) * zoom + screen_center_y;
             
             let size = tile_size * 0.8;
             buffer.rect((sx - size/2.0) as i32, (sy - size/2.0) as i32, size as i32, size as i32, 255, 0, 0);
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
