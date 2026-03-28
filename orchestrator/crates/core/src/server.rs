use std::collections::HashMap;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{error, info, warn};

use crate::types::*;

/// A handle for sending messages to a connected agent.
type AgentSender = mpsc::UnboundedSender<String>;

struct ConnectedAgent {
    info: AgentInfo,
    sender: AgentSender,
}

/// Generates unique agent IDs. Injectable for deterministic testing.
pub trait IdGenerator: Send + Sync {
    fn next_id(&self) -> String;
}

/// Default ID generator using UUIDs.
pub struct UuidIdGenerator;

impl IdGenerator for UuidIdGenerator {
    fn next_id(&self) -> String {
        format!("agent-{}", &uuid::Uuid::new_v4().to_string()[..8])
    }
}

/// Core server state, extracted for testability.
pub struct ServerState {
    agents: HashMap<String, ConnectedAgent>,
    pending_requests: HashMap<String, String>, // request_id -> agent_id
    event_tx: mpsc::UnboundedSender<OrchestratorEvent>,
    id_gen: Arc<dyn IdGenerator>,
}

impl ServerState {
    pub fn new(
        event_tx: mpsc::UnboundedSender<OrchestratorEvent>,
        id_gen: Arc<dyn IdGenerator>,
    ) -> Self {
        Self {
            agents: HashMap::new(),
            pending_requests: HashMap::new(),
            event_tx,
            id_gen,
        }
    }

    fn send_to_agent(&self, agent_id: &str, msg: &Message) {
        if let Some(agent) = self.agents.get(agent_id) {
            let text = serde_json::to_string(msg).unwrap();
            let _ = agent.sender.send(text);
        }
    }

    fn agent_name(&self, agent_id: &str) -> String {
        self.agents
            .get(agent_id)
            .map(|a| a.info.name.clone())
            .unwrap_or_default()
    }

    fn peer_list(&self, exclude: &str) -> Vec<PeerInfo> {
        self.agents
            .values()
            .filter(|a| a.info.id != exclude)
            .map(|a| PeerInfo {
                id: a.info.id.clone(),
                name: a.info.name.clone(),
                role: a.info.role.clone(),
            })
            .collect()
    }

    /// Register a new agent. Returns the assigned agent ID.
    pub fn register_agent(
        &mut self,
        name: String,
        role: String,
        sender: AgentSender,
    ) -> (String, Vec<PeerInfo>) {
        let id = self.id_gen.next_id();
        let info = AgentInfo {
            id: id.clone(),
            name: name.clone(),
            role: role.clone(),
        };
        let peers = self.peer_list(&id);
        let _ = self
            .event_tx
            .send(OrchestratorEvent::AgentConnected(info.clone()));
        self.agents.insert(
            id.clone(),
            ConnectedAgent { info, sender },
        );
        (id, peers)
    }

    /// Handle a user_prompt request from an agent.
    pub fn handle_user_prompt(
        &mut self,
        agent_id: &str,
        request_id: String,
        payload: Value,
    ) {
        let agent_name = self.agent_name(agent_id);
        let _ = self.event_tx.send(OrchestratorEvent::RequestReceived {
            agent_id: agent_id.to_string(),
            agent_name,
            request_id: request_id.clone(),
            request_type: "user_prompt".into(),
            payload,
        });
        self.pending_requests
            .insert(request_id, agent_id.to_string());
    }

    /// Respond to a pending request (called from TUI).
    pub fn respond_to_request(&mut self, request_id: &str, payload: Value) {
        if let Some(agent_id) = self.pending_requests.remove(request_id) {
            let response = Message {
                id: request_id.to_string(),
                msg_type: "user_prompt_response".into(),
                from: "orchestrator".into(),
                to: Some(agent_id.clone()),
                payload,
            };
            self.send_to_agent(&agent_id, &response);
        }
    }

    /// Remove an agent on disconnect.
    pub fn remove_agent(&mut self, agent_id: &str) {
        self.agents.remove(agent_id);
        let _ = self
            .event_tx
            .send(OrchestratorEvent::AgentDisconnected {
                id: agent_id.to_string(),
            });
    }

    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    pub fn pending_count(&self) -> usize {
        self.pending_requests.len()
    }
}

pub async fn run(
    addr: &str,
    event_tx: mpsc::UnboundedSender<OrchestratorEvent>,
    cmd_rx: mpsc::UnboundedReceiver<TuiCommand>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    run_with_id_gen(addr, event_tx, cmd_rx, Arc::new(UuidIdGenerator)).await
}

pub async fn run_with_id_gen(
    addr: &str,
    event_tx: mpsc::UnboundedSender<OrchestratorEvent>,
    mut cmd_rx: mpsc::UnboundedReceiver<TuiCommand>,
    id_gen: Arc<dyn IdGenerator>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind(addr).await?;
    info!("WebSocket server listening on {}", addr);

    let state = Arc::new(Mutex::new(ServerState::new(event_tx, id_gen)));

    // Handle commands from TUI
    let state_for_cmds = state.clone();
    tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                TuiCommand::RespondToRequest {
                    request_id,
                    payload,
                } => {
                    let mut s = state_for_cmds.lock().await;
                    s.respond_to_request(&request_id, payload);
                }
                TuiCommand::Shutdown => break,
            }
        }
    });

    // Accept connections
    loop {
        let (stream, addr) = listener.accept().await?;
        info!("New TCP connection from {}", addr);
        let state = state.clone();
        tokio::spawn(handle_connection(stream, state));
    }
}

async fn handle_connection(stream: TcpStream, state: Arc<Mutex<ServerState>>) {
    let ws_stream = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => {
            error!("WebSocket handshake failed: {}", e);
            return;
        }
    };

    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    // Create a channel for outbound messages to this agent.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(text) = out_rx.recv().await {
            if ws_sender.send(WsMessage::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    let mut agent_id: Option<String> = None;

    while let Some(msg) = ws_receiver.next().await {
        let text = match msg {
            Ok(WsMessage::Text(t)) => t.to_string(),
            Ok(WsMessage::Close(_)) => break,
            Ok(_) => continue,
            Err(e) => {
                warn!("WebSocket error: {}", e);
                break;
            }
        };

        let message: Message = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                warn!("Invalid JSON message: {}", e);
                continue;
            }
        };

        match message.msg_type.as_str() {
            "register" => {
                let payload: RegisterPayload =
                    match serde_json::from_value(message.payload.clone()) {
                        Ok(p) => p,
                        Err(e) => {
                            warn!("Invalid register payload: {}", e);
                            continue;
                        }
                    };

                let mut s = state.lock().await;
                let (id, peers) = s.register_agent(
                    payload.name.clone(),
                    payload.role.clone(),
                    out_tx.clone(),
                );

                let ack = Message {
                    id: message.id,
                    msg_type: "register_ack".into(),
                    from: "orchestrator".into(),
                    to: Some(id.clone()),
                    payload: serde_json::to_value(RegisterAckPayload {
                        agent_id: id.clone(),
                        peers,
                    })
                    .unwrap(),
                };
                let _ = out_tx.send(serde_json::to_string(&ack).unwrap());

                agent_id = Some(id.clone());
                info!("Agent registered: {} ({})", payload.name, id);
            }

            "user_prompt" => {
                if let Some(ref aid) = agent_id {
                    let mut s = state.lock().await;
                    s.handle_user_prompt(aid, message.id.clone(), message.payload);
                }
            }

            other => {
                warn!("Unknown message type from agent: {}", other);
            }
        }
    }

    // Clean up on disconnect
    if let Some(ref id) = agent_id {
        let mut s = state.lock().await;
        s.remove_agent(id);
        info!("Agent disconnected: {}", id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct SequentialIdGenerator {
        counter: AtomicU32,
    }

    impl SequentialIdGenerator {
        fn new() -> Self {
            Self {
                counter: AtomicU32::new(1),
            }
        }
    }

    impl IdGenerator for SequentialIdGenerator {
        fn next_id(&self) -> String {
            let n = self.counter.fetch_add(1, Ordering::SeqCst);
            format!("agent-{}", n)
        }
    }

    fn setup() -> (
        ServerState,
        mpsc::UnboundedReceiver<OrchestratorEvent>,
    ) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let id_gen = Arc::new(SequentialIdGenerator::new());
        let state = ServerState::new(event_tx, id_gen);
        (state, event_rx)
    }

    #[test]
    fn register_agent_assigns_id_and_emits_event() {
        let (mut state, mut event_rx) = setup();
        let (sender, _receiver) = mpsc::unbounded_channel();

        let (id, peers) = state.register_agent(
            "test-agent".into(),
            "code-agent".into(),
            sender,
        );

        assert_eq!(id, "agent-1");
        assert!(peers.is_empty());
        assert_eq!(state.agent_count(), 1);

        let event = event_rx.try_recv().unwrap();
        match event {
            OrchestratorEvent::AgentConnected(info) => {
                assert_eq!(info.id, "agent-1");
                assert_eq!(info.name, "test-agent");
                assert_eq!(info.role, "code-agent");
            }
            _ => panic!("Expected AgentConnected event"),
        }
    }

    #[test]
    fn register_second_agent_sees_first_as_peer() {
        let (mut state, _event_rx) = setup();
        let (s1, _r1) = mpsc::unbounded_channel();
        let (s2, _r2) = mpsc::unbounded_channel();

        state.register_agent("agent-a".into(), "code-agent".into(), s1);
        let (_, peers) = state.register_agent("agent-b".into(), "review-agent".into(), s2);

        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].name, "agent-a");
        assert_eq!(state.agent_count(), 2);
    }

    #[test]
    fn remove_agent_emits_disconnect_event() {
        let (mut state, mut event_rx) = setup();
        let (sender, _receiver) = mpsc::unbounded_channel();

        let (id, _) = state.register_agent("test".into(), "code-agent".into(), sender);
        let _ = event_rx.try_recv(); // consume AgentConnected

        state.remove_agent(&id);
        assert_eq!(state.agent_count(), 0);

        let event = event_rx.try_recv().unwrap();
        match event {
            OrchestratorEvent::AgentDisconnected { id: disconnected_id } => {
                assert_eq!(disconnected_id, id);
            }
            _ => panic!("Expected AgentDisconnected event"),
        }
    }

    #[test]
    fn handle_user_prompt_stores_pending_and_emits_event() {
        let (mut state, mut event_rx) = setup();
        let (sender, _receiver) = mpsc::unbounded_channel();

        let (id, _) = state.register_agent("test".into(), "code-agent".into(), sender);
        let _ = event_rx.try_recv(); // consume AgentConnected

        state.handle_user_prompt(&id, "req-1".into(), json!({"question": "hello?"}));

        assert_eq!(state.pending_count(), 1);
        let event = event_rx.try_recv().unwrap();
        match event {
            OrchestratorEvent::RequestReceived {
                agent_id,
                agent_name,
                request_id,
                request_type,
                payload,
            } => {
                assert_eq!(agent_id, id);
                assert_eq!(agent_name, "test");
                assert_eq!(request_id, "req-1");
                assert_eq!(request_type, "user_prompt");
                assert_eq!(payload["question"], "hello?");
            }
            _ => panic!("Expected RequestReceived event"),
        }
    }

    #[test]
    fn respond_to_request_sends_to_agent_and_clears_pending() {
        let (mut state, mut event_rx) = setup();
        let (sender, mut receiver) = mpsc::unbounded_channel();

        let (id, _) = state.register_agent("test".into(), "code-agent".into(), sender);
        let _ = event_rx.try_recv();

        state.handle_user_prompt(&id, "req-1".into(), json!({"question": "color?"}));
        let _ = event_rx.try_recv();

        state.respond_to_request("req-1", json!({"answer": "blue"}));

        assert_eq!(state.pending_count(), 0);

        let sent = receiver.try_recv().unwrap();
        let msg: Message = serde_json::from_str(&sent).unwrap();
        assert_eq!(msg.msg_type, "user_prompt_response");
        assert_eq!(msg.id, "req-1");
        assert_eq!(msg.payload["answer"], "blue");
    }

    #[test]
    fn respond_to_unknown_request_is_noop() {
        let (mut state, _event_rx) = setup();
        // Should not panic
        state.respond_to_request("nonexistent", json!({"answer": "test"}));
        assert_eq!(state.pending_count(), 0);
    }

    #[tokio::test]
    async fn integration_ws_register_and_prompt() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

        // Start server on random port
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let id_gen = Arc::new(SequentialIdGenerator::new());
        let addr_str = addr.to_string();
        tokio::spawn(async move {
            let _ = run_with_id_gen(&addr_str, event_tx, cmd_rx, id_gen).await;
        });

        // Give server time to start
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Connect a WebSocket client
        let url = format!("ws://{}", addr);
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let (mut sender, mut receiver) = ws.split();

        // Send register
        let register_msg = json!({
            "id": "reg-1",
            "type": "register",
            "from": "pending",
            "payload": { "name": "test-agent", "role": "code-agent" }
        });
        sender
            .send(WsMessage::Text(serde_json::to_string(&register_msg).unwrap().into()))
            .await
            .unwrap();

        // Read register_ack
        let ack_text = match receiver.next().await.unwrap().unwrap() {
            WsMessage::Text(t) => t.to_string(),
            other => panic!("Expected text, got {:?}", other),
        };
        let ack: Message = serde_json::from_str(&ack_text).unwrap();
        assert_eq!(ack.msg_type, "register_ack");
        assert_eq!(ack.payload["agentId"], "agent-1");

        // Check event
        let event = event_rx.recv().await.unwrap();
        assert!(matches!(event, OrchestratorEvent::AgentConnected(_)));

        // Send user_prompt
        let prompt_msg = json!({
            "id": "prompt-1",
            "type": "user_prompt",
            "from": "agent-1",
            "payload": { "question": "What color?" }
        });
        sender
            .send(WsMessage::Text(serde_json::to_string(&prompt_msg).unwrap().into()))
            .await
            .unwrap();

        // Check event
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let event = event_rx.recv().await.unwrap();
        match event {
            OrchestratorEvent::RequestReceived { request_id, .. } => {
                assert_eq!(request_id, "prompt-1");
            }
            _ => panic!("Expected RequestReceived"),
        }

        // Respond via TUI command
        cmd_tx
            .send(TuiCommand::RespondToRequest {
                request_id: "prompt-1".into(),
                payload: json!({"answer": "red"}),
            })
            .unwrap();

        // Read response from WebSocket
        let resp_text = match receiver.next().await.unwrap().unwrap() {
            WsMessage::Text(t) => t.to_string(),
            other => panic!("Expected text, got {:?}", other),
        };
        let resp: Message = serde_json::from_str(&resp_text).unwrap();
        assert_eq!(resp.msg_type, "user_prompt_response");
        assert_eq!(resp.payload["answer"], "red");
    }
}
