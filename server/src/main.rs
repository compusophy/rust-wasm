use tokio::net::{TcpListener, TcpStream};
use futures_util::{StreamExt, SinkExt};
use tokio_tungstenite::accept_async;
use std::env;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

type Db = Arc<Mutex<HashMap<String, String>>>;

#[tokio::main]
async fn main() {
    // Railway provides the PORT environment variable
    let port = env::var("PORT").unwrap_or_else(|_| "9001".to_string());
    let addr = format!("0.0.0.0:{}", port);

    // Create a broadcast channel for chat messages
    let (tx, _rx) = broadcast::channel(100);

    let listener = TcpListener::bind(&addr).await.expect("Failed to bind");
    println!("Listening on: {}", addr);

    while let Ok((stream, _)) = listener.accept().await {
        let tx = tx.clone();
        tokio::spawn(handle_connection(stream, tx));
    }
}

async fn handle_connection(stream: TcpStream, tx: broadcast::Sender<String>) {
    let ws_stream = match accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            println!("Error during the websocket handshake occurred: {}", e);
            return;
        }
    };

    let (mut write, mut read) = ws_stream.split();
    
    // Subscribe to the broadcast channel
    let mut rx = tx.subscribe();

    // Spawn a task to forward broadcast messages to this client
    let mut send_task = tokio::spawn(async move {
        while let Ok(msg) = rx.recv().await {
            if write.send(tokio_tungstenite::tungstenite::Message::Text(msg)).await.is_err() {
                break;
            }
        }
    });

    // Process incoming messages from this client
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = read.next().await {
            if msg.is_text() {
                let text = msg.to_text().unwrap();
                // Broadcast the message to all subscribers
                let _ = tx.send(text.to_string());
            }
        }
    });

    // Wait for either task to finish (connection closed or error)
    tokio::select! {
        _ = (&mut send_task) => recv_task.abort(),
        _ = (&mut recv_task) => send_task.abort(),
    };
}

