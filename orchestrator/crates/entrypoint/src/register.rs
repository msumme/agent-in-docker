use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// Connect to the orchestrator WS, send register, and stay connected.
/// Reconnects with exponential backoff if the connection drops.
pub async fn register_and_stay_connected(
    url: String,
    name: String,
    role: String,
) -> Result<()> {
    let mut delay = 1u64;

    loop {
        match connect_and_run(&url, &name, &role).await {
            Ok(()) => {
                // Clean disconnect -- orchestrator shut down or agent exiting
                eprintln!("[entrypoint] WS connection closed");
                break;
            }
            Err(e) => {
                eprintln!("[entrypoint] WS error: {}", e);
                eprintln!("[entrypoint] Reconnecting in {}s...", delay);
                tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                delay = (delay * 2).min(30);
            }
        }
    }

    Ok(())
}

/// Single connection lifecycle: connect, register, listen for messages.
async fn connect_and_run(url: &str, name: &str, role: &str) -> Result<()> {
    let (ws, _) = tokio_tungstenite::connect_async(url).await?;
    let (mut sender, mut receiver) = ws.split();

    // Register
    let msg = register_message(name, role);
    sender.send(WsMessage::Text(serde_json::to_string(&msg)?.into())).await?;

    // Wait for register_ack
    if let Some(Ok(WsMessage::Text(text))) = receiver.next().await {
        let resp: serde_json::Value = serde_json::from_str(&text)?;
        if resp.get("type").and_then(|v| v.as_str()) == Some("register_ack") {
            let agent_id = resp["payload"]["agentId"].as_str().unwrap_or("unknown");
            eprintln!("[entrypoint] Registered as {} ({})", agent_id, name);
        }
    }

    // Stay connected -- respond to pings
    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(WsMessage::Ping(data)) => {
                let _ = sender.send(WsMessage::Pong(data)).await;
            }
            Ok(WsMessage::Close(_)) => return Ok(()),
            Err(e) => return Err(e.into()),
            _ => {}
        }
    }

    Ok(())
}

fn register_message(name: &str, role: &str) -> serde_json::Value {
    json!({
        "id": simple_id(),
        "type": "register",
        "from": "pending",
        "payload": { "name": name, "role": role }
    })
}

fn simple_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    format!("{:032x}", t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_message_has_correct_shape() {
        let msg = register_message("Alice", "code-agent");
        assert_eq!(msg["type"], "register");
        assert_eq!(msg["from"], "pending");
        assert_eq!(msg["payload"]["name"], "Alice");
        assert_eq!(msg["payload"]["role"], "code-agent");
        assert!(msg["id"].as_str().unwrap().len() > 0);
    }

    #[test]
    fn simple_id_generates_unique() {
        let a = simple_id();
        let b = simple_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
    }
}
