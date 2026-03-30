use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Connect to the orchestrator WS, send register, and stay connected.
/// This runs as a background task -- the connection keeps the agent
/// visible in the orchestrator TUI until Claude Code exits.
pub async fn register_and_stay_connected(
    url: String,
    name: String,
    role: String,
) -> Result<()> {
    let (ws, _) = match tokio_tungstenite::connect_async(&url).await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("[entrypoint] WS registration failed: {} (orchestrator may not be running)", e);
            return Ok(());
        }
    };

    let (mut sender, mut receiver) = ws.split();

    // Send register message
    let register_msg = json!({
        "id": uuid_v4(),
        "type": "register",
        "from": "pending",
        "payload": {
            "name": name,
            "role": role
        }
    });
    sender
        .send(WsMessage::Text(serde_json::to_string(&register_msg)?.into()))
        .await?;

    // Wait for register_ack
    if let Some(Ok(WsMessage::Text(text))) = receiver.next().await {
        let msg: serde_json::Value = serde_json::from_str(&text)?;
        if msg.get("type").and_then(|v| v.as_str()) == Some("register_ack") {
            let agent_id = msg["payload"]["agentId"].as_str().unwrap_or("unknown");
            eprintln!("[entrypoint] Registered as {} ({})", agent_id, name);
        }
    }

    // Stay connected -- respond to pings, ignore other messages
    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(WsMessage::Ping(data)) => {
                let _ = sender.send(WsMessage::Pong(data)).await;
            }
            Ok(WsMessage::Close(_)) => break,
            Err(_) => break,
            _ => {} // Ignore other messages
        }
    }

    eprintln!("[entrypoint] WS connection closed");
    Ok(())
}

fn uuid_v4() -> String {
    // Simple UUID v4 without external dep
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    format!("{:032x}", t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_v4_generates_unique() {
        let a = uuid_v4();
        let b = uuid_v4();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
    }
}
