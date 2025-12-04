use tokio::net::{TcpListener, TcpStream};
use futures_util::{StreamExt, SinkExt};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use std::env;
use tokio::sync::broadcast;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use serde::{Serialize, Deserialize};

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
    PlayerMove { player_id: u32, x: f32, y: f32 }, // Client position update
}

struct GlobalState {
    next_id: u32,
    players: HashMap<u32, PlayerInfo>,
}

impl GlobalState {
    fn new() -> Self {
        GlobalState {
            next_id: 0,
            players: HashMap::new(),
        }
    }

    fn assign_next_position(&mut self) -> (i32, i32) {
        let n = self.next_id as i32;
        if n == 0 { return (0, 0); }

        // Spiral logic
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
}

#[tokio::main]
async fn main() {
    let port = env::var("PORT").unwrap_or_else(|_| "9001".to_string());
    let addr = format!("0.0.0.0:{}", port);

    let (tx, _rx) = broadcast::channel(100);
    let state = Arc::new(Mutex::new(GlobalState::new()));

    let listener = TcpListener::bind(&addr).await.expect("Failed to bind");
    println!("Listening on: {}", addr);

    while let Ok((stream, _)) = listener.accept().await {
        let tx = tx.clone();
        let state = state.clone();
        tokio::spawn(handle_connection(stream, tx, state));
    }
}

async fn handle_connection(stream: TcpStream, tx: broadcast::Sender<String>, state: Arc<Mutex<GlobalState>>) {
    let ws_stream = match accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            println!("Error during the websocket handshake occurred: {}", e);
            return;
        }
    };

    let (mut write, mut read) = ws_stream.split();
    let mut rx = tx.subscribe();

    // Assign Player
    let (player_id, chunk_x, chunk_y, all_players) = {
        let mut gs = state.lock().unwrap();
        let (cx, cy) = gs.assign_next_position();
        let id = gs.next_id;
        
        let player_info = PlayerInfo { id, chunk_x: cx, chunk_y: cy };
        
        // Gather existing players for welcome message
        let existing_players = gs.players.values().cloned().collect();
        
        gs.players.insert(id, player_info.clone());
        gs.next_id += 1;
        
        (id, cx, cy, existing_players)
    };

    println!("New Player {} assigned to Chunk ({}, {})", player_id, chunk_x, chunk_y);

    // Send Welcome
    let welcome_msg = serde_json::to_string(&GameMessage::Welcome {
        player_id,
        chunk_x,
        chunk_y,
        players: all_players,
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


    // Heartbeat interval
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
                    // Send Ping to keep connection alive
                    if write.send(Message::Ping(vec![])).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = read.next().await {
            if msg.is_text() {
                // Just echo/broadcast for now if needed, or handle moves
                // let text = msg.to_text().unwrap();
                // let _ = tx.send(text.to_string());
            }
        }
    });

    tokio::select! {
        _ = (&mut send_task) => recv_task.abort(),
        _ = (&mut recv_task) => send_task.abort(),
    };
    
    // Cleanup (optional: remove player from state)
    println!("Player {} disconnected", player_id);
}
