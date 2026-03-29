use std::collections::HashMap;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{error, info, warn};

use crate::types::*;

type AgentSender = mpsc::UnboundedSender<String>;

struct ConnectedAgent {
    info: AgentInfo,
    sender: AgentSender,
}

pub trait IdGenerator: Send + Sync {
    fn next_id(&self) -> String;
}

pub struct UuidIdGenerator;

impl IdGenerator for UuidIdGenerator {
    fn next_id(&self) -> String {
        format!("agent-{}", &uuid::Uuid::new_v4().to_string()[..8])
    }
}

struct PendingRequest {
    agent_id: String,
    request_type: String,
    payload: Value,
}

pub struct ServerState {
    agents: HashMap<String, ConnectedAgent>,
    pending_requests: HashMap<String, PendingRequest>,
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

    pub fn register_agent(
        &mut self,
        name: String,
        role: String,
        workspace_path: Option<String>,
        sender: AgentSender,
    ) -> (String, Vec<PeerInfo>) {
        let id = self.id_gen.next_id();
        let info = AgentInfo {
            id: id.clone(),
            name: name.clone(),
            role: role.clone(),
            workspace_path,
        };
        let peers = self.peer_list(&id);
        let _ = self
            .event_tx
            .send(OrchestratorEvent::AgentConnected(info.clone()));
        self.agents.insert(id.clone(), ConnectedAgent { info, sender });
        (id, peers)
    }

    pub fn handle_request(
        &mut self,
        agent_id: &str,
        request_id: String,
        request_type: &str,
        payload: Value,
    ) {
        let agent_name = self.agent_name(agent_id);
        let _ = self.event_tx.send(OrchestratorEvent::RequestReceived {
            agent_id: agent_id.to_string(),
            agent_name,
            request_id: request_id.clone(),
            request_type: request_type.to_string(),
            payload: payload.clone(),
        });
        self.pending_requests.insert(
            request_id,
            PendingRequest {
                agent_id: agent_id.to_string(),
                request_type: request_type.to_string(),
                payload,
            },
        );
    }

    pub fn respond_to_request(&mut self, request_id: &str, msg_type: &str, payload: Value) {
        if let Some(pending) = self.pending_requests.remove(request_id) {
            let response = Message {
                id: request_id.to_string(),
                msg_type: msg_type.to_string(),
                from: "orchestrator".into(),
                to: Some(pending.agent_id.clone()),
                payload,
            };
            self.send_to_agent(&pending.agent_id, &response);
        }
    }

    /// Execute an approved request (file_read, git_push) and send the result.
    pub fn execute_approved_request(&mut self, request_id: &str) {
        if let Some(pending) = self.pending_requests.remove(request_id) {
            let (msg_type, payload) = match pending.request_type.as_str() {
                "file_read" => {
                    let path = pending.payload.get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    match crate::handlers::file_read::read_file(path) {
                        Ok(content) => (
                            "file_read_response",
                            serde_json::json!({"content": content}),
                        ),
                        Err(e) => (
                            "error",
                            serde_json::json!({"code": "READ_FAILED", "message": e}),
                        ),
                    }
                }
                "git_push" => {
                    let remote = pending.payload.get("remote")
                        .and_then(|v| v.as_str())
                        .unwrap_or("origin");
                    let branch = pending.payload.get("branch")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let workspace = self.agent_workspace(&pending.agent_id)
                        .unwrap_or_default();

                    let branch = if branch.is_empty() {
                        crate::handlers::git_push::current_branch(&workspace)
                            .unwrap_or_else(|_| "main".into())
                    } else {
                        branch.to_string()
                    };

                    match crate::handlers::git_push::git_push(&workspace, remote, &branch) {
                        Ok(output) => (
                            "git_push_response",
                            serde_json::json!({"success": true, "output": output}),
                        ),
                        Err(e) => (
                            "error",
                            serde_json::json!({"code": "PUSH_FAILED", "message": e}),
                        ),
                    }
                }
                _ => (
                    "error",
                    serde_json::json!({"code": "UNKNOWN_REQUEST", "message": "Cannot execute this request type"}),
                ),
            };

            let response = Message {
                id: request_id.to_string(),
                msg_type: msg_type.to_string(),
                from: "orchestrator".into(),
                to: Some(pending.agent_id.clone()),
                payload,
            };
            self.send_to_agent(&pending.agent_id, &response);
        }
    }

    pub fn remove_agent(&mut self, agent_id: &str) {
        self.agents.remove(agent_id);
        let _ = self
            .event_tx
            .send(OrchestratorEvent::AgentDisconnected {
                id: agent_id.to_string(),
            });
    }

    pub fn agent_role(&self, agent_id: &str) -> Option<String> {
        self.agents.get(agent_id).map(|a| a.info.role.clone())
    }

    pub fn agent_workspace(&self, agent_id: &str) -> Option<String> {
        self.agents
            .get(agent_id)
            .and_then(|a| a.info.workspace_path.clone())
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

    let state_for_cmds = state.clone();
    tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            let mut s = state_for_cmds.lock().await;
            match cmd {
                TuiCommand::RespondToRequest {
                    request_id,
                    payload,
                } => {
                    s.respond_to_request(&request_id, "user_prompt_response", payload);
                }
                TuiCommand::ApproveRequest { request_id } => {
                    s.execute_approved_request(&request_id);
                }
                TuiCommand::DenyRequest {
                    request_id,
                    reason,
                } => {
                    s.respond_to_request(
                        &request_id,
                        "error",
                        serde_json::json!({"code": "PERMISSION_DENIED", "message": reason}),
                    );
                }
                TuiCommand::SendTask { agent_id, prompt } => {
                    let msg = Message {
                        id: uuid::Uuid::new_v4().to_string(),
                        msg_type: "send_task".into(),
                        from: "orchestrator".into(),
                        to: Some(agent_id.clone()),
                        payload: serde_json::json!({"prompt": prompt}),
                    };
                    s.send_to_agent(&agent_id, &msg);
                }
                TuiCommand::Shutdown => break,
            }
        }
    });

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
                    payload.workspace_path.clone(),
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

            "user_prompt" | "file_read" | "git_push" => {
                if let Some(ref aid) = agent_id {
                    let mut s = state.lock().await;
                    s.handle_request(aid, message.id.clone(), &message.msg_type, message.payload);
                }
            }

            other => {
                warn!("Unknown message type from agent: {}", other);
            }
        }
    }

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

    fn setup() -> (ServerState, mpsc::UnboundedReceiver<OrchestratorEvent>) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let id_gen = Arc::new(SequentialIdGenerator::new());
        let state = ServerState::new(event_tx, id_gen);
        (state, event_rx)
    }

    #[test]
    fn register_agent_assigns_id_and_emits_event() {
        let (mut state, mut event_rx) = setup();
        let (sender, _receiver) = mpsc::unbounded_channel();

        let (id, peers) =
            state.register_agent("test-agent".into(), "code-agent".into(), None, sender);

        assert_eq!(id, "agent-1");
        assert!(peers.is_empty());
        assert_eq!(state.agent_count(), 1);

        let event = event_rx.try_recv().unwrap();
        match event {
            OrchestratorEvent::AgentConnected(info) => {
                assert_eq!(info.id, "agent-1");
                assert_eq!(info.name, "test-agent");
            }
            _ => panic!("Expected AgentConnected"),
        }
    }

    #[test]
    fn register_second_agent_sees_first_as_peer() {
        let (mut state, _event_rx) = setup();
        let (s1, _r1) = mpsc::unbounded_channel();
        let (s2, _r2) = mpsc::unbounded_channel();

        state.register_agent("agent-a".into(), "code-agent".into(), None, s1);
        let (_, peers) =
            state.register_agent("agent-b".into(), "review-agent".into(), None, s2);

        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].name, "agent-a");
    }

    #[test]
    fn remove_agent_emits_disconnect_event() {
        let (mut state, mut event_rx) = setup();
        let (sender, _receiver) = mpsc::unbounded_channel();

        let (id, _) = state.register_agent("test".into(), "code-agent".into(), None, sender);
        let _ = event_rx.try_recv();

        state.remove_agent(&id);
        assert_eq!(state.agent_count(), 0);

        match event_rx.try_recv().unwrap() {
            OrchestratorEvent::AgentDisconnected { id: did } => assert_eq!(did, id),
            _ => panic!("Expected AgentDisconnected"),
        }
    }

    #[test]
    fn handle_request_stores_pending_and_emits_event() {
        let (mut state, mut event_rx) = setup();
        let (sender, _receiver) = mpsc::unbounded_channel();

        let (id, _) = state.register_agent("test".into(), "code-agent".into(), None, sender);
        let _ = event_rx.try_recv();

        state.handle_request(&id, "req-1".into(), "user_prompt", json!({"question": "hello?"}));

        assert_eq!(state.pending_count(), 1);
        match event_rx.try_recv().unwrap() {
            OrchestratorEvent::RequestReceived {
                request_id,
                request_type,
                ..
            } => {
                assert_eq!(request_id, "req-1");
                assert_eq!(request_type, "user_prompt");
            }
            _ => panic!("Expected RequestReceived"),
        }
    }

    #[test]
    fn respond_to_request_sends_to_agent_and_clears_pending() {
        let (mut state, mut event_rx) = setup();
        let (sender, mut receiver) = mpsc::unbounded_channel();

        let (id, _) = state.register_agent("test".into(), "code-agent".into(), None, sender);
        let _ = event_rx.try_recv();

        state.handle_request(&id, "req-1".into(), "user_prompt", json!({"question": "color?"}));
        let _ = event_rx.try_recv();

        state.respond_to_request("req-1", "user_prompt_response", json!({"answer": "blue"}));

        assert_eq!(state.pending_count(), 0);

        let sent = receiver.try_recv().unwrap();
        let msg: Message = serde_json::from_str(&sent).unwrap();
        assert_eq!(msg.msg_type, "user_prompt_response");
        assert_eq!(msg.payload["answer"], "blue");
    }

    #[test]
    fn respond_to_unknown_request_is_noop() {
        let (mut state, _event_rx) = setup();
        state.respond_to_request("nonexistent", "error", json!({"code": "NOT_FOUND"}));
        assert_eq!(state.pending_count(), 0);
    }

    #[test]
    fn file_read_request_emits_event() {
        let (mut state, mut event_rx) = setup();
        let (sender, _receiver) = mpsc::unbounded_channel();

        let (id, _) = state.register_agent("test".into(), "code-agent".into(), None, sender);
        let _ = event_rx.try_recv();

        state.handle_request(&id, "fr-1".into(), "file_read", json!({"path": "/etc/hosts"}));

        match event_rx.try_recv().unwrap() {
            OrchestratorEvent::RequestReceived {
                request_type,
                payload,
                ..
            } => {
                assert_eq!(request_type, "file_read");
                assert_eq!(payload["path"], "/etc/hosts");
            }
            _ => panic!("Expected RequestReceived"),
        }
    }

    #[test]
    fn git_push_request_emits_event() {
        let (mut state, mut event_rx) = setup();
        let (sender, _receiver) = mpsc::unbounded_channel();

        let (id, _) = state.register_agent(
            "test".into(),
            "code-agent".into(),
            Some("/workspace".into()),
            sender,
        );
        let _ = event_rx.try_recv();

        state.handle_request(
            &id,
            "gp-1".into(),
            "git_push",
            json!({"remote": "origin", "branch": "main"}),
        );

        match event_rx.try_recv().unwrap() {
            OrchestratorEvent::RequestReceived {
                request_type,
                payload,
                ..
            } => {
                assert_eq!(request_type, "git_push");
                assert_eq!(payload["remote"], "origin");
            }
            _ => panic!("Expected RequestReceived"),
        }
    }

    #[tokio::test]
    async fn integration_ws_register_and_prompt() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let id_gen = Arc::new(SequentialIdGenerator::new());
        let addr_str = addr.to_string();
        tokio::spawn(async move {
            let _ = run_with_id_gen(&addr_str, event_tx, cmd_rx, id_gen).await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let url = format!("ws://{}", addr);
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let (mut sender, mut receiver) = ws.split();

        // Register
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

        let ack_text = match receiver.next().await.unwrap().unwrap() {
            WsMessage::Text(t) => t.to_string(),
            other => panic!("Expected text, got {:?}", other),
        };
        let ack: Message = serde_json::from_str(&ack_text).unwrap();
        assert_eq!(ack.msg_type, "register_ack");

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

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let event = event_rx.recv().await.unwrap();
        assert!(matches!(event, OrchestratorEvent::RequestReceived { .. }));

        // Respond
        cmd_tx
            .send(TuiCommand::RespondToRequest {
                request_id: "prompt-1".into(),
                payload: json!({"answer": "red"}),
            })
            .unwrap();

        let resp_text = match receiver.next().await.unwrap().unwrap() {
            WsMessage::Text(t) => t.to_string(),
            other => panic!("Expected text, got {:?}", other),
        };
        let resp: Message = serde_json::from_str(&resp_text).unwrap();
        assert_eq!(resp.msg_type, "user_prompt_response");
        assert_eq!(resp.payload["answer"], "red");

        // Test file_read request + deny
        let fr_msg = json!({
            "id": "fr-1",
            "type": "file_read",
            "from": "agent-1",
            "payload": { "path": "/etc/secret" }
        });
        sender
            .send(WsMessage::Text(serde_json::to_string(&fr_msg).unwrap().into()))
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let _ = event_rx.recv().await.unwrap();

        cmd_tx
            .send(TuiCommand::DenyRequest {
                request_id: "fr-1".into(),
                reason: "Not allowed".into(),
            })
            .unwrap();

        let deny_text = match receiver.next().await.unwrap().unwrap() {
            WsMessage::Text(t) => t.to_string(),
            other => panic!("Expected text, got {:?}", other),
        };
        let deny: Message = serde_json::from_str(&deny_text).unwrap();
        assert_eq!(deny.msg_type, "error");
        assert_eq!(deny.payload["code"], "PERMISSION_DENIED");

        // Test file_read request + approve (reads a real file)
        let test_file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut test_file.as_file(), b"test content here").unwrap();
        let test_path = test_file.path().to_str().unwrap().to_string();

        let fr_msg2 = json!({
            "id": "fr-2",
            "type": "file_read",
            "from": "agent-1",
            "payload": { "path": test_path }
        });
        sender
            .send(WsMessage::Text(serde_json::to_string(&fr_msg2).unwrap().into()))
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let _ = event_rx.recv().await.unwrap();

        cmd_tx
            .send(TuiCommand::ApproveRequest {
                request_id: "fr-2".into(),
            })
            .unwrap();

        let approve_text = match receiver.next().await.unwrap().unwrap() {
            WsMessage::Text(t) => t.to_string(),
            other => panic!("Expected text, got {:?}", other),
        };
        let approve: Message = serde_json::from_str(&approve_text).unwrap();
        assert_eq!(approve.msg_type, "file_read_response");
        assert_eq!(approve.payload["content"], "test content here");
    }
}
