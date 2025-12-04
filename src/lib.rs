use std::cell::RefCell;
use std::rc::Rc;
use std::collections::{BinaryHeap, HashMap};
use std::cmp::Ordering;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::Clamped;
use web_sys::{WebSocket, HtmlCanvasElement, CanvasRenderingContext2d, ImageData, MouseEvent, MessageEvent};
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

// --- CHAT CLIENT (Stub - preserved for JS compatibility) ---
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
const GAME_SIZE: u32 = 512;
const TILE_SIZE: u32 = 16;
const MAP_W: u32 = GAME_SIZE / TILE_SIZE;
const MAP_H: u32 = GAME_SIZE / TILE_SIZE;

// Game Area Offsets
const OFF_X: u32 = (WIDTH - GAME_SIZE) / 2;
const OFF_Y: u32 = (HEIGHT - GAME_SIZE) / 2;

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

    fn pixel(&mut self, x: u32, y: u32, r: u8, g: u8, b: u8) {
        if x >= self.width || y >= self.height { return; }
        let idx = ((y * self.width + x) * 4) as usize;
        self.pixels[idx] = r;
        self.pixels[idx+1] = g;
        self.pixels[idx+2] = b;
        self.pixels[idx+3] = 255;
    }

    fn rect(&mut self, x: u32, y: u32, w: u32, h: u32, r: u8, g: u8, b: u8) {
        for iy in y..(y + h) {
            for ix in x..(x + w) {
                self.pixel(ix, iy, r, g, b);
            }
        }
    }

    fn rect_outline(&mut self, x: u32, y: u32, w: u32, h: u32, r: u8, g: u8, b: u8) {
        for ix in x..(x + w) {
            self.pixel(ix, y, r, g, b);
            self.pixel(ix, y + h - 1, r, g, b);
        }
        for iy in y..(y + h) {
            self.pixel(x, iy, r, g, b);
            self.pixel(x + w - 1, iy, r, g, b);
        }
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
                self.pixel(x as u32, y as u32, r, g, b);
            }
            counter += 1;

            if x == x1 && y == y1 { break; }
            let e2 = 2 * err;
            if e2 > -dy { err -= dy; x += sx; }
            if e2 < dx { err += dx; y += sy; }
        }
    }
    
    // Helper to draw text (bitmap font simulation for coords)
    fn draw_digit(&mut self, x: u32, y: u32, digit: i32, r: u8, g: u8, b: u8) {
         // Very simple 3x5 pixel font logic could go here, keeping it simple for now:
         // Just a colored block to indicate something
         self.rect(x, y, 4, 6, r, g, b);
    }
}

// --- PATHFINDING ---

#[derive(Copy, Clone, Eq, PartialEq)]
struct Node {
    cost: u32,
    pos: (i32, i32),
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
    x: f32,
    y: f32,
    path: Vec<(f32, f32)>, // Waypoints (reverse order)
    selected: bool,
    kind: u8,
    color: (u8, u8, u8),
}

struct Building {
    tile_x: u32,
    tile_y: u32,
    kind: u8,
}

struct GameState {
    tiles: Vec<TileType>,
    units: Vec<Unit>,
    buildings: Vec<Building>,
    
    // Multiplayer State
    my_id: Option<u32>,
    chunk_x: i32,
    chunk_y: i32,
    other_players: Vec<PlayerInfo>,
}

impl GameState {
    fn new() -> GameState {
        let mut tiles = vec![TileType::Grass; (MAP_W * MAP_H) as usize];
        let mut units = Vec::new();
        let mut buildings = Vec::new();

        // Generate Map
        for y in 0..MAP_H {
            for x in 0..MAP_W {
                let idx = (y * MAP_W + x) as usize;
                let r = random();
                if r < 0.1 { tiles[idx] = TileType::Water; }
                else if r < 0.25 { tiles[idx] = TileType::Forest; }
                else if r < 0.28 { tiles[idx] = TileType::Mountain; }
                else if r < 0.30 { tiles[idx] = TileType::Gold; }
            }
        }

        // Place Town Center
        let cx = MAP_W / 2;
        let cy = MAP_H / 2;
        
        // Clear area for TC
        for y in (cy-2)..(cy+3) {
            for x in (cx-2)..(cx+3) {
                if x < MAP_W && y < MAP_H {
                    tiles[(y * MAP_W + x) as usize] = TileType::Grass;
                }
            }
        }

        buildings.push(Building { tile_x: cx, tile_y: cy, kind: 0 });
        
        // Add Villagers
        let sx = (cx * TILE_SIZE) as f32;
        let sy = (cy * TILE_SIZE) as f32;
        units.push(Unit { x: sx + 30.0, y: sy + 30.0, path: Vec::new(), selected: false, kind: 0, color: (0, 0, 255) });
        units.push(Unit { x: sx - 20.0, y: sy + 40.0, path: Vec::new(), selected: false, kind: 0, color: (0, 0, 255) });

        GameState { 
            tiles, 
            units, 
            buildings,
            my_id: None,
            chunk_x: 0,
            chunk_y: 0,
            other_players: Vec::new(),
        }
    }

    fn is_tile_walkable(&self, x: i32, y: i32) -> bool {
        if x < 0 || x >= MAP_W as i32 || y < 0 || y >= MAP_H as i32 { return false; }
        let idx = (y * MAP_W as i32 + x) as usize;
        match self.tiles[idx] {
            TileType::Water | TileType::Forest | TileType::Mountain | TileType::Gold => return false,
            _ => {}
        }
        
        // Check Buildings (TC area)
        for b in &self.buildings {
            let bx = b.tile_x as i32;
            let by = b.tile_y as i32;
            if x >= bx - 1 && x <= bx + 1 && y >= by - 1 && y <= by + 1 {
                return false;
            }
        }
        true
    }

    fn find_path(&self, start: (f32, f32), end: (f32, f32)) -> Vec<(f32, f32)> {
        let start_tx = (start.0 / TILE_SIZE as f32) as i32;
        let start_ty = (start.1 / TILE_SIZE as f32) as i32;
        let end_tx = (end.0 / TILE_SIZE as f32) as i32;
        let end_ty = (end.1 / TILE_SIZE as f32) as i32;

        // If clicked same tile or unwalkable, try to get close or return
        if start_tx == end_tx && start_ty == end_ty { return vec![end]; }
        if !self.is_tile_walkable(end_tx, end_ty) { return vec![]; }

        let mut frontier = BinaryHeap::new();
        frontier.push(Node { cost: 0, pos: (start_tx, start_ty) });

        let mut came_from: HashMap<(i32, i32), (i32, i32)> = HashMap::new();
        let mut cost_so_far: HashMap<(i32, i32), u32> = HashMap::new();
        
        came_from.insert((start_tx, start_ty), (start_tx, start_ty));
        cost_so_far.insert((start_tx, start_ty), 0);

        let mut found = false;

        while let Some(Node { cost: _, pos: current }) = frontier.pop() {
            if current == (end_tx, end_ty) {
                found = true;
                break;
            }

            // Directions (dx, dy)
            let dirs = [
                (1, 0), (-1, 0), (0, 1), (0, -1), 
                (1, 1), (-1, -1), (1, -1), (-1, 1),
            ];

            for (dx, dy) in dirs.iter() {
                let next = (current.0 + dx, current.1 + dy);

                if self.is_tile_walkable(next.0, next.1) {
                    // Prevent corner cutting: if diagonal, check cardinal neighbors
                    if *dx != 0 && *dy != 0 {
                        if !self.is_tile_walkable(current.0 + dx, current.1) || 
                           !self.is_tile_walkable(current.0, current.1 + dy) {
                            continue;
                        }
                    }

                    let new_cost = cost_so_far[&current] + 1;
                    if !cost_so_far.contains_key(&next) || new_cost < cost_so_far[&next] {
                        cost_so_far.insert(next, new_cost);
                        
                        // Heuristic: Chebyshev distance (max(dx, dy)) is better for 8-way with uniform cost
                        let h = std::cmp::max((next.0 - end_tx).abs(), (next.1 - end_ty).abs()) as u32;
                        let priority = new_cost + h;
                        
                        frontier.push(Node { cost: priority, pos: next });
                        came_from.insert(next, current);
                    }
                }
            }
        }

        if !found { return vec![]; }

        // Reconstruct path
        let mut path = Vec::new();
        let mut curr = (end_tx, end_ty);
        
        // Push actual click position as final sub-tile target
        path.push(end);

        while curr != (start_tx, start_ty) {
            // Push tile center
            path.push((
                (curr.0 as f32 * TILE_SIZE as f32) + TILE_SIZE as f32 / 2.0,
                (curr.1 as f32 * TILE_SIZE as f32) + TILE_SIZE as f32 / 2.0
            ));
            curr = came_from[&curr];
        }
        // Path is now [Target, TileCenter_N, ..., TileCenter_1]
        // Unit pops from end, so it goes to TileCenter_1 first.
        path
    }

    fn update(&mut self) {
        let speed = 0.3; // Reduced speed
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
                    // Reached waypoint
                    should_pop = true;
                } else {
                    // Move towards waypoint
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

            // Collision with Map (using center of unit roughly)
            let cx = new_x + 3.0;
            let cy = new_y + 5.0;
            let tx = (cx / TILE_SIZE as f32) as i32;
            let ty = (cy / TILE_SIZE as f32) as i32;

            if self.is_tile_walkable(tx, ty) {
                updates.push((i, new_x, new_y, should_pop));
            } else {
                // Slide against walls
                let mut final_x = unit.x;
                let mut final_y = unit.y;

                // Try X movement
                let cx_x = (unit.x + dx) + 3.0;
                let cy_curr = unit.y + 5.0;
                if self.is_tile_walkable((cx_x / TILE_SIZE as f32) as i32, (cy_curr / TILE_SIZE as f32) as i32) {
                    final_x += dx;
                }

                // Try Y movement
                let cx_curr = final_x + 3.0;
                let cy_y = (unit.y + dy) + 5.0;
                if self.is_tile_walkable((cx_curr / TILE_SIZE as f32) as i32, (cy_y / TILE_SIZE as f32) as i32) {
                    final_y += dy;
                }
                
                updates.push((i, final_x, final_y, should_pop));
            }
        }

        for (i, x, y, pop) in updates {
            let u = &mut self.units[i];
            u.x = x;
            u.y = y;
            if pop {
                u.path.pop();
            }
        }
    }

    fn handle_click(&mut self, screen_x: f32, screen_y: f32) {
        let gx = screen_x - OFF_X as f32;
        let gy = screen_y - OFF_Y as f32;

        let mut clicked_unit = false;

        // Click Unit?
        for unit in &mut self.units {
            if gx >= unit.x - 2.0 && gx <= unit.x + 12.0 && gy >= unit.y - 2.0 && gy <= unit.y + 12.0 {
                unit.selected = !unit.selected;
                clicked_unit = true;
                break;
            }
        }

        // Move Selection (Left Click Move)
        if !clicked_unit {
            if gx >= 0.0 && gx <= GAME_SIZE as f32 && gy >= 0.0 && gy <= GAME_SIZE as f32 {
                // Calculate paths for all selected units
                // We need to clone unit positions to calculate paths without borrowing conflict
                let mut paths = Vec::new();
                for (i, unit) in self.units.iter().enumerate() {
                    if unit.selected {
                        // Slight offset for group movement could be added here
                        let path = self.find_path((unit.x, unit.y), (gx, gy));
                        paths.push((i, path));
                    }
                }
                
                // Apply paths
                for (i, path) in paths {
                    self.units[i].path = path;
                }
            }
        }
    }

    fn handle_right_click(&mut self, screen_x: f32, screen_y: f32) {
        let gx = screen_x - OFF_X as f32;
        let gy = screen_y - OFF_Y as f32;
        
        if gx < 0.0 || gx > GAME_SIZE as f32 || gy < 0.0 || gy > GAME_SIZE as f32 { return; }

        let mut paths = Vec::new();
        for (i, unit) in self.units.iter().enumerate() {
            if unit.selected {
                let path = self.find_path((unit.x, unit.y), (gx, gy));
                paths.push((i, path));
            }
        }
        for (i, path) in paths {
            self.units[i].path = path;
        }
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

    // --- WEBSOCKET SETUP ---
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
                            state.chunk_x = chunk_x;
                            state.chunk_y = chunk_y;
                            state.other_players = players;
                            log(&format!("Welcome! Assigned to Chunk ({}, {})", chunk_x, chunk_y));
                        },
                        GameMessage::NewPlayer { player } => {
                            log(&format!("New Player joined at ({}, {})", player.chunk_x, player.chunk_y));
                            state.other_players.push(player);
                        },
                        GameMessage::PlayerMove { .. } => {
                            // Handle other player moves later
                        }
                    }
                }
            }
        }) as Box<dyn FnMut(MessageEvent)>);
        
        ws.set_onmessage(Some(onmessage_callback.as_ref().unchecked_ref()));
        onmessage_callback.forget(); // Leak memory to keep callback alive
    }

    // --- INPUT HANDLERS ---
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
        let closure = Closure::wrap(Box::new(move |event: MouseEvent| {
            event.prevent_default();
            gs.borrow_mut().handle_right_click(event.offset_x() as f32, event.offset_y() as f32);
        }) as Box<dyn FnMut(_)>);
        canvas.add_event_listener_with_callback("contextmenu", closure.as_ref().unchecked_ref())?;
        closure.forget();
    }

    // --- RENDER LOOP ---
    let f = Rc::new(RefCell::new(None));
    let g = f.clone();

    *g.borrow_mut() = Some(Closure::wrap(Box::new(move || {
        let mut gs = game_state.borrow_mut();
        gs.update();

        buffer.clear(20, 20, 20);
        buffer.rect_outline(OFF_X - 2, OFF_Y - 2, GAME_SIZE + 4, GAME_SIZE + 4, 100, 100, 100);

        for y in 0..MAP_H {
            for x in 0..MAP_W {
                let idx = (y * MAP_W + x) as usize;
                let px = OFF_X + x * TILE_SIZE;
                let py = OFF_Y + y * TILE_SIZE;
                
                match gs.tiles[idx] {
                    TileType::Grass => {
                        buffer.rect(px, py, TILE_SIZE, TILE_SIZE, 75, 105, 47);
                        if (x + y) % 3 == 0 { buffer.pixel(px+4, py+4, 85, 115, 57); }
                    },
                    TileType::Water => {
                        buffer.rect(px, py, TILE_SIZE, TILE_SIZE, 50, 89, 165);
                        if (x + y) % 4 == 0 { buffer.pixel(px+2, py+2, 200, 200, 255); }
                    },
                    TileType::Forest => {
                        buffer.rect(px, py, TILE_SIZE, TILE_SIZE, 75, 105, 47);
                        buffer.rect(px+6, py+8, 4, 8, 101, 67, 33);
                        buffer.rect(px+3, py+2, 10, 8, 45, 77, 30);
                    },
                    TileType::Mountain => {
                        buffer.rect(px, py, TILE_SIZE, TILE_SIZE, 75, 105, 47);
                        buffer.rect(px+2, py+2, 12, 12, 120, 120, 120);
                        buffer.rect(px+5, py+2, 6, 4, 220, 220, 220);
                    },
                    TileType::Gold => {
                        buffer.rect(px, py, TILE_SIZE, TILE_SIZE, 75, 105, 47);
                        buffer.rect(px+4, py+4, 8, 8, 255, 215, 0);
                        buffer.pixel(px+6, py+6, 255, 255, 200);
                    },
                }
            }
        }

        for b in &gs.buildings {
            let px = OFF_X + b.tile_x * TILE_SIZE;
            let py = OFF_Y + b.tile_y * TILE_SIZE;
            buffer.rect(px - 8, py - 8, 32, 24, 160, 82, 45); 
            buffer.rect(px - 4, py - 12, 24, 8, 200, 200, 200);
            buffer.rect(px + 2, py, 12, 12, 50, 50, 200);
        }

        for u in &gs.units {
            let px = OFF_X + u.x as u32;
            let py = OFF_Y + u.y as u32;
            
            if u.selected {
                buffer.rect_outline(px-2, py-2, 10, 14, 0, 255, 0);

                // Draw Path
                if !u.path.is_empty() {
                    let mut prev_x = px as i32 + 3; // Center of unit
                    let mut prev_y = py as i32 + 5;

                    // Iterate path in reverse (from end to start) because that's how it's stored
                    for point in u.path.iter().rev() {
                        let next_x = (OFF_X as f32 + point.0) as i32;
                        let next_y = (OFF_Y as f32 + point.1) as i32;
                        
                        buffer.line(prev_x, prev_y, next_x, next_y, 255, 255, 255, true);
                        
                        // Draw Waypoint Dot
                        buffer.rect(next_x as u32 - 1, next_y as u32 - 1, 3, 3, 255, 255, 255);

                        prev_x = next_x;
                        prev_y = next_y;
                    }
                }
            }

            buffer.rect(px, py, 6, 10, 255, 200, 150); 
            buffer.rect(px, py+4, 6, 6, u.color.0, u.color.1, u.color.2); 
        }

        // Draw "HUD"
        buffer.rect(0, 0, WIDTH, 30, 50, 50, 50);
        
        // Display Chunk Info (Red/Blue boxes to indicate coords roughly for now)
        if let Some(_id) = gs.my_id {
             let cx = gs.chunk_x;
             let cy = gs.chunk_y;
             // Simple visualization of coords: center is 0,0 (Green). 
             // Right is +x (Blue), Left -x (Red), Down +y (Yellow), Up -y (Cyan)
             let mut r = 100; let mut g = 100; let mut b = 100;
             if cx == 0 && cy == 0 { r=0; g=255; b=0; }
             else {
                if cx > 0 { b += 50; } else if cx < 0 { r += 50; }
                if cy > 0 { r += 50; g += 50; } // Yellowish
                else if cy < 0 { g += 50; b += 50; } // Cyanish
             }
             buffer.rect(10, 10, 10, 10, r, g, b);
        } else {
            buffer.rect(10, 10, 10, 10, 50, 50, 50); // Gray = disconnected/connecting
        }

        buffer.rect(80, 10, 10, 10, 139, 69, 19); 
        buffer.rect(150, 10, 10, 10, 255, 215, 0); 

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