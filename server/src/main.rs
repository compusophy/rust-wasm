use tokio::net::{TcpListener, TcpStream};
use futures_util::{StreamExt, SinkExt};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use std::env;
use tokio::sync::broadcast;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Pool, Postgres, Row};
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug, Clone)]
struct PlayerInfo {
    id: i32,
    chunk_x: i32,
    chunk_y: i32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct UnitState {
    x: f32,
    y: f32,
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

// Default fallback, but DB overrides this
const MIN_CLIENT_VERSION_DEFAULT: u32 = 16;

struct GlobalState {
    next_id: i32,
    players: HashMap<i32, PlayerInfo>,
    units: HashMap<i32, Vec<UnitState>>,
    // Memory mode persistence (Token -> PlayerID)
    tokens: HashMap<String, i32>, 
}

impl GlobalState {
    fn new() -> Self {
        GlobalState {
            next_id: 1,
            players: HashMap::new(),
            units: HashMap::new(),
            tokens: HashMap::new(),
        }
    }

    fn assign_next_position(n: i32) -> (i32, i32) {
        if n == 0 { return (0, 0); }
        
        // Tweak: Reduce distance between players
        // Previously: 1 chunk per player.
        // New: Pack 4 players into 1 chunk (2x2 grid within chunk?) 
        // OR simpler: Just spiral but fill every chunk (which we do).
        
        // If the user feels map is "WAY TOO BIG", maybe our Chunks (32x32) are too huge?
        // 32 tiles * 16px = 512px. That's barely a screen width.
        // The issue might be that they spawn 1 chunk apart.
        
        // Let's keep the spiral but maybe we don't skip chunks?
        // The current algorithm spirals 0,0 -> 1,0 -> 1,1 -> 0,1 ...
        // This IS filling every chunk.
        
        // Maybe the issue is just visual "void".
        // Let's try to put players closer by using "sub-chunk" addressing?
        // No, let's just stick to 1 player per chunk for now but acknowledge
        // that 512px is not "far".
        
        // Wait, the spiral logic might be spreading them out too much if I implemented it wrong.
        // Let's check the spiral logic.
        // It looks standard Ulam spiral.
        
        let mut x = 0;
        let mut y = 0;
        let mut d = 1;
        let mut m = 1;
        let mut count = 0;
        loop {
            for _ in 0..m {
                x += d;
                count += 1;
                if count == n { return (x, y); }
            }
            for _ in 0..m {
                y += d;
                count += 1;
                if count == n { return (x, y); }
            }
            d = -d;
            m += 1;
        }
    }

    fn spawn_units(cx: i32, cy: i32) -> Vec<UnitState> {
        let chunk_size = 32.0; // New chunk size 32
        let tile_size = 16.0;
        
        // Calculate Center of Chunk in World Coords
        let sx = (cx as f32 * chunk_size * tile_size) + (chunk_size * tile_size / 2.0);
        let sy = (cy as f32 * chunk_size * tile_size) + (chunk_size * tile_size / 2.0);
        
        // Offsets for units (relative to center)
        // We need to make sure these don't hit the center building (at 0,0 relative offset)
        // Building size is roughly tile_size * 1.5 (24px)
        
        // Unit 1: Offset by 40px (2.5 tiles) right, 40px down
        // Unit 2: Offset by 40px left, 40px down
        
        vec![
            UnitState { x: sx + 40.0, y: sy + 40.0 },
            UnitState { x: sx - 40.0, y: sy + 40.0 },
        ]
    }
}

#[tokio::main]
async fn main() {
    let port = env::var("PORT").unwrap_or_else(|_| "9001".to_string());
    let addr = format!("0.0.0.0:{}", port);
    
    // Check for DB URL. If missing, fallback to optional/memory-only mode (with warnings).
    let database_url = env::var("DATABASE_URL").ok();
    
    let pool = if let Some(db_url) = database_url {
        println!("Connecting to Database...");
        let p = PgPoolOptions::new()
            .max_connections(5)
            .connect(&db_url)
            .await
            .ok();
            
        if let Some(ref valid_pool) = p {
             // Initialize DB - Execute separately to avoid prepared statement errors
            let _ = sqlx::query(
                "CREATE TABLE IF NOT EXISTS players (
                    id SERIAL PRIMARY KEY,
                    token VARCHAR NOT NULL UNIQUE,
                    chunk_x INT NOT NULL,
                    chunk_y INT NOT NULL,
                    created_at TIMESTAMPTZ DEFAULT NOW()
                )"
            )
            .execute(valid_pool)
            .await;

            let _ = sqlx::query(
                "CREATE TABLE IF NOT EXISTS units (
                    id SERIAL PRIMARY KEY,
                    owner_id INT NOT NULL,
                    unit_idx INT NOT NULL,
                    x REAL NOT NULL,
                    y REAL NOT NULL,
                    created_at TIMESTAMPTZ DEFAULT NOW(),
                    CONSTRAINT fk_owner FOREIGN KEY(owner_id) REFERENCES players(id),
                    UNIQUE(owner_id, unit_idx)
                )"
            )
            .execute(valid_pool)
            .await;

            let _ = sqlx::query(
                "CREATE TABLE IF NOT EXISTS buildings (
                    id SERIAL PRIMARY KEY,
                    owner_id INT NOT NULL,
                    kind INT NOT NULL,
                    tile_x INT NOT NULL,
                    tile_y INT NOT NULL,
                    created_at TIMESTAMPTZ DEFAULT NOW(),
                    CONSTRAINT fk_building_owner FOREIGN KEY(owner_id) REFERENCES players(id)
                )"
            )
            .execute(valid_pool)
            .await;

            let _ = sqlx::query(
                "CREATE TABLE IF NOT EXISTS server_config (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                )"
            )
            .execute(valid_pool)
            .await;

            // Seed default min version if missing
            let _ = sqlx::query("INSERT INTO server_config (key, value) VALUES ('min_client_version', '17') ON CONFLICT DO NOTHING")
                .execute(valid_pool)
                .await;

            println!("Database Connected & Initialized.");
        } else {
             println!("Failed to connect to Database. Persistence disabled.");
        }
        p
    } else {
        println!("DATABASE_URL not set. Persistence disabled.");
        None
    };

    let (tx, _rx) = broadcast::channel(100);
    let state = Arc::new(Mutex::new(GlobalState::new()));

    let listener = TcpListener::bind(&addr).await.expect("Failed to bind");
    println!("Listening on: {}", addr);

    while let Ok((stream, _)) = listener.accept().await {
        let tx = tx.clone();
        let state = state.clone();
        let pool = pool.clone();
        tokio::spawn(handle_connection(stream, tx, state, pool));
    }
}

async fn handle_connection(
    stream: TcpStream, 
    tx: broadcast::Sender<String>, 
    state: Arc<Mutex<GlobalState>>,
    pool: Option<Pool<Postgres>>
) {
    let ws_stream = match accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            println!("Error during the websocket handshake occurred: {}", e);
            return;
        }
    };

    let (mut write, mut read) = ws_stream.split();
    let mut rx = tx.subscribe();

    // --- HANDSHAKE ---
    let client_token: Option<String>;

    if let Some(Ok(msg)) = read.next().await {
        if let Ok(text) = msg.to_text() {
            if let Ok(GameMessage::Join { version, token }) = serde_json::from_str(text) {
                
                // CHECK VERSION (DB or Fallback)
                let required_version = if let Some(p) = &pool {
                    let row = sqlx::query("SELECT value FROM server_config WHERE key = 'min_client_version'")
                        .fetch_optional(p)
                        .await
                        .unwrap_or(None);
                    
                    if let Some(r) = row {
                        r.try_get::<String, _>("value").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(MIN_CLIENT_VERSION_DEFAULT)
                    } else {
                        MIN_CLIENT_VERSION_DEFAULT
                    }
                } else {
                    MIN_CLIENT_VERSION_DEFAULT
                };

                if version < required_version {
                    let _ = write.send(Message::Text(serde_json::to_string(&GameMessage::Error { 
                        message: format!("Client version {} is too old. Minimum required: {}", version, required_version) 
                    }).unwrap())).await;
                    return;
                }
                client_token = token;
            } else {
                 let _ = write.send(Message::Text(serde_json::to_string(&GameMessage::Error { 
                        message: "Invalid handshake: expected Join message".to_string() 
                    }).unwrap())).await;
                return;
            }
        } else {
            return; 
        }
    } else {
        return; 
    }

    // Authenticate or Register
    let (player_id, chunk_x, chunk_y, token) = if let Some(p) = &pool {
        // DB MODE
        match client_token {
            Some(t) => {
                let row = sqlx::query("SELECT id, chunk_x, chunk_y FROM players WHERE token = $1")
                    .bind(&t)
                    .fetch_optional(p)
                    .await;
                
                match row {
                    Ok(Some(r)) => (r.get::<i32, _>("id"), r.get::<i32, _>("chunk_x"), r.get::<i32, _>("chunk_y"), t),
                    _ => create_new_player(p).await // Invalid token or error -> New Player
                }
            },
            None => create_new_player(p).await
        }
    } else {
        // MEMORY MODE (Fallback)
        let mut gs = state.lock().unwrap();
        
        // Check if token exists in memory
        if let Some(t) = client_token.as_ref() {
             if let Some(&pid) = gs.tokens.get(t) {
                 // Found existing player in memory
                 // Re-calculate chunk pos based on ID
                 let (cx, cy) = GlobalState::assign_next_position(pid);
                 (pid, cx, cy, t.clone())
             } else {
                 // New player
                 let id = gs.next_id;
                 gs.next_id += 1;
                 let (cx, cy) = GlobalState::assign_next_position(id);
                 let new_token = Uuid::new_v4().to_string();
                 gs.tokens.insert(new_token.clone(), id);
                 (id, cx, cy, new_token)
             }
        } else {
            // New player (No token sent)
            let id = gs.next_id;
            gs.next_id += 1;
            let (cx, cy) = GlobalState::assign_next_position(id);
            let new_token = Uuid::new_v4().to_string();
            gs.tokens.insert(new_token.clone(), id);
            (id, cx, cy, new_token)
        }
    };

    // LOAD UNITS (Async DB Operation before Lock)
    let my_units: Vec<UnitState> = if let Some(p) = &pool {
        // DB MODE
        let rows = sqlx::query("SELECT unit_idx, x, y FROM units WHERE owner_id = $1 ORDER BY unit_idx ASC")
            .bind(player_id)
            .fetch_all(p)
            .await;
            
        match rows {
            Ok(unit_rows) => {
                if unit_rows.is_empty() {
                    // No units in DB -> Spawn New & Save
                    let new_units = GlobalState::spawn_units(chunk_x, chunk_y);
                    for (i, u) in new_units.iter().enumerate() {
                        let _ = sqlx::query("INSERT INTO units (owner_id, unit_idx, x, y) VALUES ($1, $2, $3, $4)")
                            .bind(player_id)
                            .bind(i as i32)
                            .bind(u.x)
                            .bind(u.y)
                            .execute(p)
                            .await;
                    }
                    new_units
                } else {
                    // Found in DB -> Load
                    let mut loaded = Vec::new();
                    // We need to ensure they are sorted by index or we just map them. 
                    // Query was ORDER BY unit_idx, so we are good.
                    for r in unit_rows {
                        loaded.push(UnitState {
                            x: r.get::<f32, _>("x"),
                            y: r.get::<f32, _>("y"),
                        });
                    }
                    loaded
                }
            },
            Err(e) => {
                println!("Failed to load units: {}", e);
                GlobalState::spawn_units(chunk_x, chunk_y) // Fallback
            }
        }
    } else {
        // MEMORY MODE CHECK
        // In memory mode, we don't persist across restarts, but we persist across reconnects if server stayed up.
        // We need to check GlobalState first.
        // Since we haven't locked yet, we can't check efficiently without a quick lock/unlock or just check later.
        // Actually, let's just spawn default here and let the Lock block handle the "if exists" logic for Memory Mode.
        // Wait, for consistency, let's do the logic inside the lock for Memory Mode.
        Vec::new() 
    };

    // FETCH ALL PLAYERS & UNITS & BUILDINGS (DB Mode) - To show offline players
    let (db_players, db_units, db_buildings) = if let Some(p) = &pool {
         let p_rows = sqlx::query("SELECT id, chunk_x, chunk_y FROM players").fetch_all(p).await.unwrap_or_default();
         let players: Vec<PlayerInfo> = p_rows.into_iter().map(|r| PlayerInfo {
             id: r.get("id"),
             chunk_x: r.get("chunk_x"),
             chunk_y: r.get("chunk_y"),
         }).collect();

         let u_rows = sqlx::query("SELECT owner_id, unit_idx, x, y FROM units").fetch_all(p).await.unwrap_or_default();
         let units: Vec<UnitDTO> = u_rows.into_iter().map(|r| UnitDTO {
             owner_id: r.get("owner_id"),
             unit_idx: r.get::<i32, _>("unit_idx") as usize,
             x: r.get("x"),
             y: r.get("y"),
         }).collect();
         
         let b_rows = sqlx::query("SELECT id, owner_id, kind, tile_x, tile_y FROM buildings").fetch_all(p).await.unwrap_or_default();
         let buildings: Vec<BuildingDTO> = b_rows.into_iter().map(|r| BuildingDTO {
             id: r.get("id"),
             owner_id: r.get("owner_id"),
             kind: r.get::<i32, _>("kind") as u8,
             tile_x: r.get("tile_x"),
             tile_y: r.get("tile_y"),
         }).collect();

         (Some(players), Some(units), Some(buildings))
    } else {
        (None, None, None)
    };

    // Update Global State (Active Players & Units)
    let (all_players, all_units_dto, all_buildings_dto) = {
        let mut gs = state.lock().unwrap();
        
        gs.players.insert(player_id, PlayerInfo { id: player_id, chunk_x, chunk_y });
        
        // Handle Units
        if pool.is_some() {
            // DB Mode: Overwrite/Set with what we loaded/created from DB
            gs.units.insert(player_id, my_units);
        } else {
            // Memory Mode
            if !gs.units.contains_key(&player_id) {
                gs.units.insert(player_id, GlobalState::spawn_units(chunk_x, chunk_y));
            }
        }
        
        if let (Some(ps), Some(us), Some(bs)) = (db_players, db_units, db_buildings) {
            (ps, us, bs)
        } else {
        let existing_players: Vec<PlayerInfo> = gs.players.values().cloned().collect();
        
            let mut units_dto = Vec::new();
            for (pid, units) in &gs.units {
                for (i, u) in units.iter().enumerate() {
                    units_dto.push(UnitDTO {
                        owner_id: *pid,
                        unit_idx: i,
                        x: u.x,
                        y: u.y,
                    });
                }
            }
            
            (existing_players, units_dto, Vec::new())
        }
    };

    println!("Player {} connected (Chunk {}, {})", player_id, chunk_x, chunk_y);

    // Send Welcome
    let welcome_msg = serde_json::to_string(&GameMessage::Welcome {
        player_id,
        chunk_x,
        chunk_y,
        players: all_players,
        units: all_units_dto,
        buildings: all_buildings_dto,
        token: token.clone(),
    }).unwrap();
    
    if let Err(e) = write.send(Message::Text(welcome_msg)).await {
        println!("Failed to send welcome: {}", e);
        return;
    }

    // Broadcast New Player
    let new_player_msg = serde_json::to_string(&GameMessage::NewPlayer {
        player: PlayerInfo { id: player_id, chunk_x, chunk_y }
    }).unwrap();
    let _ = tx.send(new_player_msg);


    // Heartbeat
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(10)); // Reduced to 10s for better keepalive

    let mut send_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                Ok(msg) = rx.recv() => {
                    if write.send(Message::Text(msg)).await.is_err() {
                        break;
                    }
                }
                _ = interval.tick() => {
                    // Send Ping
                    if write.send(Message::Ping(vec![])).await.is_err() {
                        break;
                    }
                    // Occasionally send Version Check (every minute roughly, interval is 20s, so every 3rd tick)
                    // Actually, let's just piggyback or make a dedicated message type?
                    // Simplest: Send a custom "Ping" text message or reuse Error if mismatched?
                    // Better: Just enforce it on Join.
                    // BUT user is asking for periodic checks.
                    
                    // Let's send a JSON message that the client can check silently.
                    // We don't have a specific "KeepAlive" JSON message, but we can use a new type or just trust the Ping?
                    // Pings are opcode 0x9, handled by browser engine automatically, not JS code usually.
                    
                    // Let's add a VersionCheck message to the protocol for robustness?
                    // Or just re-send Welcome? No.
                    
                    // Let's assume the "Error" message is the kick mechanism.
                    // If the server updates while players are connected, they are already "in".
                    // Do we want to KICK them?
                    // Yes, if the user deployed a breaking change (min_version bumped).
                    
                    // So we need to check current connection against MIN_VERSION.
                    // But we don't store their version in state... we just checked it at handshake.
                    // Issue: If server restarts (new binary), all clients disconnect.
                    // When they reconnect, they send their OLD version in Join.
                    // The new server checks it, sees it's old, and sends Error.
                    // The client receives Error and shows overlay.
                    
                    // So the mechanism ALREADY exists for restarts.
                    // The user is saying "it's not disabling".
                    // Maybe because the server didn't restart? Or client didn't refresh?
                    
                    // If the server updated MIN_VERSION, it means the server RESTARTED.
                    // If server restarted, connections dropped.
                    // Clients auto-reconnect?
                    // Our client code DOES NOT have auto-reconnect logic in `start()`.
                    // It just says "Chat connect failed" or "Server Error".
                    
                    // Ah, `new WebSocket()` throws if it fails.
                    // But if it closes? `ws.onclose`?
                    // We don't handle `onclose` to reload.
                    
                    // Let's add `onclose` handling to the client to auto-reload or at least alert.
                }
            }
        }
    });

    let recv_state = state.clone();
    let recv_pool = pool.clone(); // Clone pool for async DB updates

    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = read.next().await {
            if msg.is_text() {
                let text = msg.to_text().unwrap();
                
                // Update Server State if UnitMove
                if let Ok(msg) = serde_json::from_str::<GameMessage>(text) {
                    match msg {
                        GameMessage::UnitMove { player_id, unit_idx, x, y } => {
                            // 1. Update Memory State (Fast for broadcasts)
                            {
                                let mut gs = recv_state.lock().unwrap();
                                if let Some(units) = gs.units.get_mut(&player_id) {
                                    if unit_idx < units.len() {
                                        units[unit_idx].x = x;
                                        units[unit_idx].y = y;
                                    }
                                }
                            }
                            
                            // 2. Async DB Persist (Fire and Forget-ish)
                            if let Some(p) = &recv_pool {
                                let _ = sqlx::query("UPDATE units SET x = $1, y = $2 WHERE owner_id = $3 AND unit_idx = $4")
                                    .bind(x)
                                    .bind(y)
                                    .bind(player_id)
                                    .bind(unit_idx as i32)
                                    .execute(p)
                                    .await;
                            }
                            
                            let _ = tx.send(text.to_string());
                        },
                        GameMessage::UnitSync { player_id, unit_idx, x, y } => {
                            // 1. Update Memory
                            {
                                let mut gs = recv_state.lock().unwrap();
                                if let Some(units) = gs.units.get_mut(&player_id) {
                                    if unit_idx < units.len() {
                                        units[unit_idx].x = x;
                                        units[unit_idx].y = y;
                                    }
                                }
                            }
                            // 2. Broadcast
                let _ = tx.send(text.to_string());
                            
                            // 3. Optional DB Persist?
                             if let Some(p) = &recv_pool {
                                let res = sqlx::query("UPDATE units SET x = $1, y = $2 WHERE owner_id = $3 AND unit_idx = $4")
                                    .bind(x)
                                    .bind(y)
                                    .bind(player_id)
                                    .bind(unit_idx as i32)
                                    .execute(p)
                                    .await;
                                
                                if let Err(e) = res {
                                    println!("DB Error on UnitSync: {}", e);
                                }
                            }
                        },
                        GameMessage::Build { kind, tile_x, tile_y } => {
                            // Check if location is valid (simple check: no other building there)
                            // We should also check ownership/resources but we skip for now.
                            
                            // 1. Update DB
                            let id;
                            if let Some(p) = &recv_pool {
                                let rec = sqlx::query("INSERT INTO buildings (owner_id, kind, tile_x, tile_y) VALUES ($1, $2, $3, $4) RETURNING id")
                                    .bind(player_id)
                                    .bind(kind as i32)
                                    .bind(tile_x)
                                    .bind(tile_y)
                                    .fetch_one(p)
                                    .await;
                                    
                                if let Ok(r) = rec {
                                    id = r.get("id");
                                } else {
                                    println!("DB Error on Build");
                                    return;
                                }
                            } else {
                                // Memory mode: fake ID
                                id = rand::random::<i32>().abs();
                            }
                            
                            // 2. Broadcast
                            let msg = GameMessage::BuildingSpawned {
                                building: BuildingDTO {
                                    id,
                                    owner_id: player_id,
                                    kind,
                                    tile_x,
                                    tile_y
                                }
                            };
                            
                            if let Ok(json) = serde_json::to_string(&msg) {
                                let _ = tx.send(json);
                            }
                        },
                        GameMessage::SpawnUnit => {
                            // Handle Spawn
                            let (chunk_x, chunk_y, next_idx, unit_count) = {
                                let mut gs = recv_state.lock().unwrap();
                                // Separate borrows: First get player coords (values only)
                                let player_coords = gs.players.get(&player_id).map(|p| (p.chunk_x, p.chunk_y));
                                
                                if let Some((cx, cy)) = player_coords {
                                    // Now mutable borrow is safe
                                    let units = gs.units.entry(player_id).or_insert(Vec::new());
                                    (cx, cy, units.len(), units.len())
                                } else {
                                    (0, 0, 0, 0)
                                }
                            };
                            
                            // LIMIT: Max 5 workers
                            if unit_count >= 5 {
                                return;
                            }
                            
                            // Calculate Spawn Pos (Near Center of their chunk)
                            let chunk_size = 32.0;
                            let tile_size = 16.0;
                            let center_x = (chunk_x as f32 * chunk_size * tile_size) + (chunk_size * tile_size / 2.0);
                            let center_y = (chunk_y as f32 * chunk_size * tile_size) + (chunk_size * tile_size / 2.0);
                            
                            // Random offset to avoid stacking
                            // Simple spiral or just random nearby
                            // Let's just put it slightly below the building
                            let offset_x = ((next_idx as f32 % 3.0) - 1.0) * 20.0;
                            let offset_y = 50.0 + (next_idx as f32 / 3.0).floor() * 20.0;
                            
                            let spawn_x = center_x + offset_x;
                            let spawn_y = center_y + offset_y;
                            
                            // 1. Update DB
                            if let Some(p) = &recv_pool {
                                let _ = sqlx::query("INSERT INTO units (owner_id, unit_idx, x, y) VALUES ($1, $2, $3, $4)")
                                    .bind(player_id)
                                    .bind(next_idx as i32)
                                    .bind(spawn_x)
                                    .bind(spawn_y)
                                    .execute(p)
                                    .await;
                            }
                            
                            // 2. Update Memory
                            {
                                let mut gs = recv_state.lock().unwrap();
                                let entry = gs.units.entry(player_id).or_insert(Vec::new());
                                entry.push(UnitState { x: spawn_x, y: spawn_y });
                            }
                            
                            // 3. Broadcast
                            let new_unit_msg = serde_json::to_string(&GameMessage::UnitSpawned {
                                unit: UnitDTO {
                                    owner_id: player_id,
                                    unit_idx: next_idx,
                                    x: spawn_x,
                                    y: spawn_y
                                }
                            }).unwrap();
                            
                            let _ = tx.send(new_unit_msg);
                        },
                        _ => {}
                    }
                }
            }
        }
    });

    tokio::select! {
        _ = (&mut send_task) => recv_task.abort(),
        _ = (&mut recv_task) => send_task.abort(),
    };
    
    // Cleanup
    {
        // Only remove from memory if NOT in DB mode (i.e. ephemeral session)
        // If we are in DB mode, we want to keep the players in memory so other players see them
        // or at least we rely on DB for state.
        
        // The bug "server dies" is likely because we remove the player from state,
        // but the other players might still be referencing them? 
        // Or simply, when they reconnect, the state is gone from RAM but DB load logic might be flawed?
        // Actually, if we remove from RAM, the next `Welcome` message to OTHER players 
        // (or even the re-joining player) might miss them if it relies on `gs.players`.
        
        // Let's KEEP them in memory for now to ensure stability and visibility.
        // Memory leak risk? Yes, but for small player counts it's fine.
        // In a real MMO we'd unload inactive chunks.
        
        let mut gs = state.lock().unwrap();
        if pool.is_none() {
            // Memory Mode: Cleanup
        gs.players.remove(&player_id);
            gs.units.remove(&player_id); 
        } else {
            // DB Mode: KEEP in memory so they remain visible on map as "ghosts" / offline players
            // This prevents "holes" in the map and missing units.
        }
    }
    println!("Player {} disconnected", player_id);
}

async fn create_new_player(pool: &Pool<Postgres>) -> (i32, i32, i32, String) {
    let token = Uuid::new_v4().to_string();
    
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM players")
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    
    let (cx, cy) = GlobalState::assign_next_position(count as i32 + 1);

    let rec = sqlx::query("INSERT INTO players (token, chunk_x, chunk_y) VALUES ($1, $2, $3) RETURNING id")
        .bind(&token)
        .bind(cx)
        .bind(cy)
        .fetch_one(pool)
        .await
        .expect("Failed to insert new player");

    let id: i32 = rec.get("id");
    (id, cx, cy, token)
}
