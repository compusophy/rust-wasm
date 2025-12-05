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
#[serde(tag = "type")]
enum GameMessage {
    Join { version: u32, token: Option<String> },
    Welcome { player_id: i32, chunk_x: i32, chunk_y: i32, players: Vec<PlayerInfo>, units: Vec<UnitDTO>, token: String },
    NewPlayer { player: PlayerInfo },
    UnitMove { player_id: i32, unit_idx: usize, x: f32, y: f32 },
    Error { message: String },
}

const MIN_CLIENT_VERSION: u32 = 7;

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
        let chunk_size = 64.0; // New chunk size 64
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
             // Initialize DB
            let _ = sqlx::query(
                r#"
                CREATE TABLE IF NOT EXISTS players (
                    id SERIAL PRIMARY KEY,
                    token VARCHAR NOT NULL UNIQUE,
                    chunk_x INT NOT NULL,
                    chunk_y INT NOT NULL,
                    created_at TIMESTAMPTZ DEFAULT NOW()
                );
                CREATE TABLE IF NOT EXISTS units (
                    id SERIAL PRIMARY KEY,
                    owner_id INT NOT NULL,
                    unit_idx INT NOT NULL,
                    x REAL NOT NULL,
                    y REAL NOT NULL,
                    created_at TIMESTAMPTZ DEFAULT NOW(),
                    CONSTRAINT fk_owner FOREIGN KEY(owner_id) REFERENCES players(id),
                    UNIQUE(owner_id, unit_idx)
                );
                "#
            )
            .execute(valid_pool)
            .await;
            println!("Database Connected.");
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
    let mut client_token: Option<String> = None;

    if let Some(Ok(msg)) = read.next().await {
        if let Ok(text) = msg.to_text() {
            if let Ok(GameMessage::Join { version, token }) = serde_json::from_str(text) {
                if version < MIN_CLIENT_VERSION {
                    let _ = write.send(Message::Text(serde_json::to_string(&GameMessage::Error { 
                        message: format!("Client version {} is too old. Minimum required: {}", version, MIN_CLIENT_VERSION) 
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
                    _ => createNewPlayer(p).await // Invalid token or error -> New Player
                }
            },
            None => createNewPlayer(p).await
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

    // Update Global State (Active Players & Units)
    let (all_players, all_units_dto) = {
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
        
        (existing_players, units_dto)
    };

    println!("Player {} connected (Chunk {}, {})", player_id, chunk_x, chunk_y);

    // Send Welcome
    let welcome_msg = serde_json::to_string(&GameMessage::Welcome {
        player_id,
        chunk_x,
        chunk_y,
        players: all_players,
        units: all_units_dto,
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
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(20));

    let mut send_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                Ok(msg) = rx.recv() => {
                    if write.send(Message::Text(msg)).await.is_err() {
                        break;
                    }
                }
                _ = interval.tick() => {
                    if write.send(Message::Ping(vec![])).await.is_err() {
                        break;
                    }
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
                if let Ok(GameMessage::UnitMove { player_id, unit_idx, x, y }) = serde_json::from_str::<GameMessage>(text) {
                    
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
                    // We don't await this strictly before broadcasting to keep lag low, 
                    // but we do await it in the loop.
                    if let Some(p) = &recv_pool {
                        let _ = sqlx::query("UPDATE units SET x = $1, y = $2 WHERE owner_id = $3 AND unit_idx = $4")
                            .bind(x)
                            .bind(y)
                            .bind(player_id)
                            .bind(unit_idx as i32)
                            .execute(p)
                            .await;
                    }
                }

                let _ = tx.send(text.to_string());
            }
        }
    });

    tokio::select! {
        _ = (&mut send_task) => recv_task.abort(),
        _ = (&mut recv_task) => send_task.abort(),
    };
    
    // Cleanup
    {
        let mut gs = state.lock().unwrap();
        gs.players.remove(&player_id);
        gs.units.remove(&player_id); 
        // Note: In DB mode, we might want to keep units in memory cache or not.
        // Currently we remove them from memory to save RAM. They are safe in DB.
    }
    println!("Player {} disconnected", player_id);
}

async fn createNewPlayer(pool: &Pool<Postgres>) -> (i32, i32, i32, String) {
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
