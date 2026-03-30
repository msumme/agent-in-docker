use std::collections::HashMap;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{error, info, warn};

use crate::mcp::AgentRegistry;
use crate::types::*;

/// Executes approved requests (file reads, git pushes). Injectable for testing.
pub trait RequestExecutor: Send + Sync {
    fn execute_file_read(&self, path: &str) -> Result<String, String>;
    fn execute_git_push(&self, workspace: &str, remote: &str, branch: &str) -> Result<String, String>;
    fn current_branch(&self, workspace: &str) -> Result<String, String>;
}

/// Real executor using file I/O and git commands.
pub struct RealRequestExecutor;

impl RequestExecutor for RealRequestExecutor {
    fn execute_file_read(&self, path: &str) -> Result<String, String> {
        crate::handlers::file_read::read_file(path)
    }
    fn execute_git_push(&self, workspace: &str, remote: &str, branch: &str) -> Result<String, String> {
        crate::handlers::git_push::git_push(workspace, remote, branch)
    }
    fn current_branch(&self, workspace: &str) -> Result<String, String> {
        crate::handlers::git_push::current_branch(workspace)
    }
}

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
    executor: Arc<dyn RequestExecutor>,
    registry_snapshot: Option<Arc<std::sync::Mutex<Vec<PeerInfo>>>>,
}

impl ServerState {
    pub fn new(
        event_tx: mpsc::UnboundedSender<OrchestratorEvent>,
        id_gen: Arc<dyn IdGenerator>,
    ) -> Self {
        Self::with_executor(event_tx, id_gen, Arc::new(RealRequestExecutor))
    }

    pub fn with_executor(
        event_tx: mpsc::UnboundedSender<OrchestratorEvent>,
        id_gen: Arc<dyn IdGenerator>,
        executor: Arc<dyn RequestExecutor>,
    ) -> Self {
        Self {
            agents: HashMap::new(),
            pending_requests: HashMap::new(),
            event_tx,
            id_gen,
            executor,
            registry_snapshot: None,
        }
    }

    pub fn set_registry_snapshot(&mut self, snapshot: Arc<std::sync::Mutex<Vec<PeerInfo>>>) {
        self.registry_snapshot = Some(snapshot);
    }

    fn sync_registry_snapshot(&self) {
        if let Some(ref snapshot) = self.registry_snapshot {
            *snapshot.lock().unwrap() = self.agent_list();
        }
    }

    fn send_to_agent(&self, agent_id: &str, msg: &Message) {
        if let Some(agent) = self.agents.get(agent_id) {
            let text = serde_json::to_string(msg).unwrap();
            let _ = agent.sender.send(text);
        }
    }

    fn send_to_agent_direct(&self, sender: &AgentSender, msg: &Message) {
        let text = serde_json::to_string(msg).unwrap();
        let _ = sender.send(text);
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

        // Broadcast peer_joined to existing agents
        let joined_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            msg_type: "peer_joined".into(),
            from: "orchestrator".into(),
            to: None,
            payload: serde_json::to_value(PeerInfo {
                id: id.clone(),
                name: name.clone(),
                role: role.clone(),
            })
            .unwrap(),
        };
        for agent in self.agents.values() {
            self.send_to_agent_direct(&agent.sender, &joined_msg);
        }

        let _ = self
            .event_tx
            .send(OrchestratorEvent::AgentConnected(info.clone()));
        self.agents.insert(id.clone(), ConnectedAgent { info, sender });
        self.sync_registry_snapshot();
        (id, peers)
    }

    /// Handle a discover request: return list of all agents.
    pub fn handle_discover(&self, agent_id: &str, request_id: &str) {
        let peers = self.peer_list(agent_id);
        let response = Message {
            id: request_id.to_string(),
            msg_type: "discover_response".into(),
            from: "orchestrator".into(),
            to: Some(agent_id.to_string()),
            payload: serde_json::json!({"agents": peers}),
        };
        self.send_to_agent(agent_id, &response);
    }

    /// Route a message from one agent to another.
    pub fn route_agent_message(
        &self,
        from_id: &str,
        request_id: &str,
        to_id: &str,
        content: &str,
    ) -> bool {
        if !self.agents.contains_key(to_id) {
            let err = Message {
                id: request_id.to_string(),
                msg_type: "error".into(),
                from: "orchestrator".into(),
                to: Some(from_id.to_string()),
                payload: serde_json::json!({"code": "AGENT_NOT_FOUND", "message": format!("Agent {} not found", to_id)}),
            };
            self.send_to_agent(from_id, &err);
            return false;
        }

        let from_name = self.agent_name(from_id);
        let delivery = Message {
            id: uuid::Uuid::new_v4().to_string(),
            msg_type: "agent_message_delivery".into(),
            from: "orchestrator".into(),
            to: Some(to_id.to_string()),
            payload: serde_json::json!({"from": from_id, "fromName": from_name, "content": content}),
        };
        self.send_to_agent(to_id, &delivery);

        let ack = Message {
            id: request_id.to_string(),
            msg_type: "agent_message_ack".into(),
            from: "orchestrator".into(),
            to: Some(from_id.to_string()),
            payload: serde_json::json!({"delivered": true}),
        };
        self.send_to_agent(from_id, &ack);
        true
    }

    /// Get a snapshot of all connected agents (for MCP tools).
    pub fn agent_list(&self) -> Vec<PeerInfo> {
        self.agents
            .values()
            .map(|a| PeerInfo {
                id: a.info.id.clone(),
                name: a.info.name.clone(),
                role: a.info.role.clone(),
            })
            .collect()
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

    /// Execute an approved request and return the result payload (for MCP resolution).
    pub fn execute_approved_request_with_result(&mut self, request_id: &str) -> Option<serde_json::Value> {
        if let Some(pending) = self.pending_requests.remove(request_id) {
            let (msg_type, payload) = self.execute_request(&pending);
            let response = Message {
                id: request_id.to_string(),
                msg_type: msg_type.to_string(),
                from: "orchestrator".into(),
                to: Some(pending.agent_id.clone()),
                payload: payload.clone(),
            };
            self.send_to_agent(&pending.agent_id, &response);
            Some(payload)
        } else {
            None
        }
    }

    /// Execute an approved request (file_read, git_push) and send the result.
    pub fn execute_approved_request(&mut self, request_id: &str) {
        self.execute_approved_request_with_result(request_id);
    }

    fn execute_request(&self, pending: &PendingRequest) -> (&'static str, serde_json::Value) {
        match pending.request_type.as_str() {
            "file_read" => {
                let path = pending.payload.get("path").and_then(|v| v.as_str()).unwrap_or("");
                match self.executor.execute_file_read(path) {
                    Ok(content) => ("file_read_response", serde_json::json!({"content": content})),
                    Err(e) => ("error", serde_json::json!({"code": "READ_FAILED", "message": e})),
                }
            }
            "git_push" => {
                let remote = pending.payload.get("remote").and_then(|v| v.as_str()).unwrap_or("origin");
                let branch = pending.payload.get("branch").and_then(|v| v.as_str()).unwrap_or("");
                let workspace = self.agent_workspace(&pending.agent_id).unwrap_or_default();
                let branch = if branch.is_empty() {
                    self.executor.current_branch(&workspace).unwrap_or_else(|_| "main".into())
                } else { branch.to_string() };
                match self.executor.execute_git_push(&workspace, remote, &branch) {
                    Ok(output) => ("git_push_response", serde_json::json!({"success": true, "output": output})),
                    Err(e) => ("error", serde_json::json!({"code": "PUSH_FAILED", "message": e})),
                }
            }
            _ => ("error", serde_json::json!({"code": "UNKNOWN_REQUEST", "message": "Cannot execute this request type"})),
        }
    }

    pub fn remove_agent(&mut self, agent_id: &str) {
        self.agents.remove(agent_id);

        // Broadcast peer_left to remaining agents
        let left_msg = Message {
            id: uuid::Uuid::new_v4().to_string(),
            msg_type: "peer_left".into(),
            from: "orchestrator".into(),
            to: None,
            payload: serde_json::json!({"id": agent_id}),
        };
        for agent in self.agents.values() {
            self.send_to_agent_direct(&agent.sender, &left_msg);
        }

        self.sync_registry_snapshot();

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

/// Snapshot-based registry that avoids blocking_lock on the tokio Mutex.
/// Updated by the server whenever agents connect/disconnect.
pub struct ServerStateRegistry {
    agents: Arc<std::sync::Mutex<Vec<PeerInfo>>>,
    state: Arc<Mutex<ServerState>>,
}

impl ServerStateRegistry {
    pub fn update_agents(&self, agents: Vec<PeerInfo>) {
        *self.agents.lock().unwrap() = agents;
    }
}

impl AgentRegistry for ServerStateRegistry {
    fn list_agents(&self) -> Vec<PeerInfo> {
        self.agents.lock().unwrap().clone()
    }

    fn route_message(&self, from: &str, to: &str, content: &str) -> Result<(), String> {
        // route_message needs the full state to send WS messages.
        // Use try_lock to avoid blocking; if contended, return error.
        match self.state.try_lock() {
            Ok(s) => {
                if s.route_agent_message(from, "", to, content) {
                    Ok(())
                } else {
                    Err(format!("Agent {} not found", to))
                }
            }
            Err(_) => Err("Server busy, try again".into()),
        }
    }
}

pub async fn run(
    addr: &str,
    event_tx: mpsc::UnboundedSender<OrchestratorEvent>,
    cmd_rx: mpsc::UnboundedReceiver<TuiCommand>,
    mcp_state: Option<Arc<crate::mcp::McpState>>,
    agent_mgr: Option<Arc<std::sync::Mutex<crate::agent_manager::AgentManager>>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    run_with_id_gen(addr, event_tx, cmd_rx, Arc::new(UuidIdGenerator), mcp_state, agent_mgr).await
}

pub async fn run_with_id_gen(
    addr: &str,
    event_tx: mpsc::UnboundedSender<OrchestratorEvent>,
    mut cmd_rx: mpsc::UnboundedReceiver<TuiCommand>,
    id_gen: Arc<dyn IdGenerator>,
    mcp_state: Option<Arc<crate::mcp::McpState>>,
    agent_mgr: Option<Arc<std::sync::Mutex<crate::agent_manager::AgentManager>>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind(addr).await?;
    info!("WebSocket server listening on {}", addr);

    let state = Arc::new(Mutex::new(ServerState::new(event_tx, id_gen)));

    // Wire the agent registry into the MCP state
    let registry_snapshot = Arc::new(std::sync::Mutex::new(Vec::<PeerInfo>::new()));
    {
        let mut s = state.lock().await;
        s.set_registry_snapshot(registry_snapshot.clone());
    }
    if let Some(ref mcp) = mcp_state {
        mcp.set_registry(Box::new(ServerStateRegistry {
            agents: registry_snapshot.clone(),
            state: state.clone(),
        }));
    }

    let state_for_cmds = state.clone();
    let agent_mgr_for_cmds = agent_mgr.clone();
    let mcp_for_cmds = mcp_state.clone();
    tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            let mut s = state_for_cmds.lock().await;
            match cmd {
                TuiCommand::RespondToRequest {
                    request_id,
                    payload,
                } => {
                    s.respond_to_request(&request_id, "user_prompt_response", payload.clone());
                    if let Some(ref mcp) = mcp_for_cmds {
                        mcp.resolve(&request_id, payload);
                    }
                }
                TuiCommand::ApproveRequest { request_id } => {
                    // Execute and get the result payload
                    let result = s.execute_approved_request_with_result(&request_id);
                    // Also resolve MCP pending request
                    if let (Some(ref mcp), Some(payload)) = (&mcp_for_cmds, result) {
                        mcp.resolve(&request_id, payload);
                    }
                }
                TuiCommand::DenyRequest {
                    request_id,
                    reason,
                } => {
                    let payload = serde_json::json!({"code": "PERMISSION_DENIED", "message": reason});
                    s.respond_to_request(&request_id, "error", payload.clone());
                    if let Some(ref mcp) = mcp_for_cmds {
                        mcp.resolve(&request_id, payload);
                    }
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
                TuiCommand::StartNewAgent { name, role } => {
                    if let Some(ref mgr) = agent_mgr_for_cmds {
                        let payload = StartAgentPayload {
                            name: name.clone(),
                            role,
                            mode: "long-running".into(),
                            project_path: std::env::current_dir().unwrap_or_default().to_string_lossy().to_string(),
                            prompt: String::new(),
                            agent_dir: String::new(), // Will need config
                            image_name: "agent-in-docker".into(),
                            network_name: "agent-net".into(),
                            orchestrator_port: 9800,
                            mcp_port: 9801,
                            dolt_port: None,
                        };
                        let mut m = mgr.lock().unwrap();
                        match m.start_agent(&payload) {
                            Ok(_) => info!("Started agent '{}' from TUI", name),
                            Err(e) => warn!("Failed to start agent '{}': {}", name, e),
                        }
                    }
                }
                TuiCommand::ReattachAgent { name } => {
                    if let Some(ref mgr) = agent_mgr_for_cmds {
                        let mut m = mgr.lock().unwrap();
                        match m.reattach_agent(&name) {
                            Ok(()) => info!("Reattached agent: {}", name),
                            Err(e) => warn!("Failed to reattach {}: {}", name, e),
                        }
                    }
                }
                TuiCommand::Shutdown => break,
            }
        }
    });

    loop {
        let (stream, addr) = listener.accept().await?;
        info!("New TCP connection from {}", addr);
        let state = state.clone();
        let mgr = agent_mgr.clone();
        tokio::spawn(handle_connection(stream, state, mgr));
    }
}

async fn handle_connection(
    stream: TcpStream,
    state: Arc<Mutex<ServerState>>,
    agent_mgr: Option<Arc<std::sync::Mutex<crate::agent_manager::AgentManager>>>,
) {
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

                // Notify agent manager that this agent connected
                if let Some(ref mgr) = agent_mgr {
                    let mut m = mgr.lock().unwrap();
                    m.agent_registered(&payload.name, &id);
                }

                info!("Agent registered: {} ({})", payload.name, id);
            }

            "user_prompt" | "file_read" | "git_push" => {
                if let Some(ref aid) = agent_id {
                    let mut s = state.lock().await;
                    s.handle_request(aid, message.id.clone(), &message.msg_type, message.payload);
                }
            }

            "discover" => {
                if let Some(ref aid) = agent_id {
                    let s = state.lock().await;
                    s.handle_discover(aid, &message.id);
                }
            }

            "agent_message" => {
                if let Some(ref aid) = agent_id {
                    let to_id = message.payload.get("to").and_then(|v| v.as_str()).unwrap_or("");
                    let content = message.payload.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    let s = state.lock().await;
                    s.route_agent_message(aid, &message.id, to_id, content);
                }
            }

            "start_agent" => {
                let payload: StartAgentPayload = match serde_json::from_value(message.payload.clone()) {
                    Ok(p) => p,
                    Err(e) => {
                        warn!("Invalid start_agent payload: {}", e);
                        let err = Message {
                            id: message.id.clone(),
                            msg_type: "start_agent_ack".into(),
                            from: "orchestrator".into(),
                            to: None,
                            payload: serde_json::json!({"success": false, "message": format!("Invalid payload: {}", e)}),
                        };
                        let _ = out_tx.send(serde_json::to_string(&err).unwrap());
                        continue;
                    }
                };
                let result = if let Some(ref mgr) = agent_mgr {
                    let mut m = mgr.lock().unwrap();
                    m.start_agent(&payload)
                } else {
                    Err("Agent manager not available".into())
                };
                let ack = Message {
                    id: message.id.clone(),
                    msg_type: "start_agent_ack".into(),
                    from: "orchestrator".into(),
                    to: None,
                    payload: match result {
                        Ok(agent) => serde_json::json!({"success": true, "agent": agent}),
                        Err(e) => serde_json::json!({"success": false, "message": e}),
                    },
                };
                let _ = out_tx.send(serde_json::to_string(&ack).unwrap());
                info!("start_agent: {}", payload.name);
            }

            "stop_agent" => {
                let name = message.payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let result = if let Some(ref mgr) = agent_mgr {
                    let mut m = mgr.lock().unwrap();
                    m.stop_agent(name)
                } else {
                    Err("Agent manager not available".into())
                };
                let ack = Message {
                    id: message.id.clone(),
                    msg_type: "stop_agent_ack".into(),
                    from: "orchestrator".into(),
                    to: None,
                    payload: match result {
                        Ok(()) => serde_json::json!({"success": true}),
                        Err(e) => serde_json::json!({"success": false, "message": e}),
                    },
                };
                let _ = out_tx.send(serde_json::to_string(&ack).unwrap());
            }

            "reattach_agent" => {
                let name = message.payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let result = if let Some(ref mgr) = agent_mgr {
                    let mut m = mgr.lock().unwrap();
                    m.reattach_agent(name)
                } else {
                    Err("Agent manager not available".into())
                };
                let ack = Message {
                    id: message.id.clone(),
                    msg_type: "reattach_agent_ack".into(),
                    from: "orchestrator".into(),
                    to: None,
                    payload: match result {
                        Ok(()) => serde_json::json!({"success": true}),
                        Err(e) => serde_json::json!({"success": false, "message": e}),
                    },
                };
                let _ = out_tx.send(serde_json::to_string(&ack).unwrap());
            }

            "list_managed" => {
                let agents = if let Some(ref mgr) = agent_mgr {
                    let m = mgr.lock().unwrap();
                    m.list_agents()
                } else {
                    vec![]
                };
                let resp = Message {
                    id: message.id.clone(),
                    msg_type: "list_managed_response".into(),
                    from: "orchestrator".into(),
                    to: None,
                    payload: serde_json::json!({"agents": agents}),
                };
                let _ = out_tx.send(serde_json::to_string(&resp).unwrap());
            }

            other => {
                warn!("Unknown message type from agent: {}", other);
            }
        }
    }

    // Update agent manager on disconnect
    if let Some(ref id) = agent_id {
        if let Some(ref mgr) = agent_mgr {
            let mut m = mgr.lock().unwrap();
            m.agent_disconnected(id);
        }
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
            let _ = run_with_id_gen(&addr_str, event_tx, cmd_rx, id_gen, None, None).await;
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

    #[test]
    fn register_broadcasts_peer_joined() {
        let (mut state, _event_rx) = setup();
        let (s1, mut r1) = mpsc::unbounded_channel();
        let (s2, _r2) = mpsc::unbounded_channel();

        state.register_agent("first".into(), "code-agent".into(), None, s1);
        // Register second -- first should get peer_joined
        state.register_agent("second".into(), "review-agent".into(), None, s2);

        let msg_text = r1.try_recv().unwrap();
        let msg: Message = serde_json::from_str(&msg_text).unwrap();
        assert_eq!(msg.msg_type, "peer_joined");
        assert_eq!(msg.payload["name"], "second");
        assert_eq!(msg.payload["role"], "review-agent");
    }

    #[test]
    fn remove_broadcasts_peer_left() {
        let (mut state, _event_rx) = setup();
        let (s1, mut r1) = mpsc::unbounded_channel();
        let (s2, _r2) = mpsc::unbounded_channel();

        state.register_agent("first".into(), "code-agent".into(), None, s1);
        let (id2, _) = state.register_agent("second".into(), "review-agent".into(), None, s2);
        let _ = r1.try_recv(); // consume peer_joined

        state.remove_agent(&id2);

        let msg_text = r1.try_recv().unwrap();
        let msg: Message = serde_json::from_str(&msg_text).unwrap();
        assert_eq!(msg.msg_type, "peer_left");
        assert_eq!(msg.payload["id"], id2);
    }

    #[test]
    fn discover_returns_peer_list() {
        let (mut state, _event_rx) = setup();
        let (s1, mut r1) = mpsc::unbounded_channel();
        let (s2, _r2) = mpsc::unbounded_channel();

        let (id1, _) = state.register_agent("first".into(), "code-agent".into(), None, s1);
        state.register_agent("second".into(), "review-agent".into(), None, s2);
        let _ = r1.try_recv(); // consume peer_joined

        state.handle_discover(&id1, "disc-1");

        let msg_text = r1.try_recv().unwrap();
        let msg: Message = serde_json::from_str(&msg_text).unwrap();
        assert_eq!(msg.msg_type, "discover_response");
        let agents = msg.payload["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["name"], "second");
    }

    #[test]
    fn route_message_delivers_to_target() {
        let (mut state, _event_rx) = setup();
        let (s1, mut r1) = mpsc::unbounded_channel();
        let (s2, mut r2) = mpsc::unbounded_channel();

        let (id1, _) = state.register_agent("sender".into(), "code-agent".into(), None, s1);
        let (id2, _) = state.register_agent("receiver".into(), "review-agent".into(), None, s2);
        let _ = r1.try_recv(); // peer_joined

        let delivered = state.route_agent_message(&id1, "msg-1", &id2, "hello from sender");
        assert!(delivered);

        // Sender gets ack
        let ack_text = r1.try_recv().unwrap();
        let ack: Message = serde_json::from_str(&ack_text).unwrap();
        assert_eq!(ack.msg_type, "agent_message_ack");
        assert!(ack.payload["delivered"].as_bool().unwrap());

        // Receiver gets delivery (no peer_joined since receiver registered after sender)
        let del_text = r2.try_recv().unwrap();
        let del: Message = serde_json::from_str(&del_text).unwrap();
        assert_eq!(del.msg_type, "agent_message_delivery");
        assert_eq!(del.payload["content"], "hello from sender");
        assert_eq!(del.payload["from"], id1);
    }

    #[test]
    fn route_message_to_nonexistent_agent_fails() {
        let (mut state, _event_rx) = setup();
        let (s1, mut r1) = mpsc::unbounded_channel();

        let (id1, _) = state.register_agent("sender".into(), "code-agent".into(), None, s1);

        let delivered = state.route_agent_message(&id1, "msg-1", "nonexistent", "hello");
        assert!(!delivered);

        let err_text = r1.try_recv().unwrap();
        let err: Message = serde_json::from_str(&err_text).unwrap();
        assert_eq!(err.msg_type, "error");
        assert_eq!(err.payload["code"], "AGENT_NOT_FOUND");
    }

    #[test]
    fn agent_list_returns_all() {
        let (mut state, _event_rx) = setup();
        let (s1, _) = mpsc::unbounded_channel();
        let (s2, _) = mpsc::unbounded_channel();

        state.register_agent("a".into(), "code-agent".into(), None, s1);
        state.register_agent("b".into(), "review-agent".into(), None, s2);

        let list = state.agent_list();
        assert_eq!(list.len(), 2);
    }

    struct FakeExecutor;
    impl RequestExecutor for FakeExecutor {
        fn execute_file_read(&self, path: &str) -> Result<String, String> {
            Ok(format!("fake content of {}", path))
        }
        fn execute_git_push(&self, _ws: &str, _remote: &str, _branch: &str) -> Result<String, String> {
            Ok("fake push ok".into())
        }
        fn current_branch(&self, _ws: &str) -> Result<String, String> {
            Ok("main".into())
        }
    }

    #[test]
    fn execute_approved_file_read_with_fake_executor() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let id_gen = Arc::new(SequentialIdGenerator::new());
        let mut state = ServerState::with_executor(event_tx, id_gen, Arc::new(FakeExecutor));
        let (sender, mut receiver) = mpsc::unbounded_channel();

        let (id, _) = state.register_agent("test".into(), "code-agent".into(), None, sender);
        let _ = event_rx.try_recv();

        state.handle_request(&id, "fr-1".into(), "file_read", json!({"path": "/etc/hosts"}));
        let _ = event_rx.try_recv();

        state.execute_approved_request("fr-1");

        let sent = receiver.try_recv().unwrap();
        let msg: Message = serde_json::from_str(&sent).unwrap();
        assert_eq!(msg.msg_type, "file_read_response");
        assert_eq!(msg.payload["content"], "fake content of /etc/hosts");
    }

    #[test]
    fn execute_approved_git_push_with_fake_executor() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let id_gen = Arc::new(SequentialIdGenerator::new());
        let mut state = ServerState::with_executor(event_tx, id_gen, Arc::new(FakeExecutor));
        let (sender, mut receiver) = mpsc::unbounded_channel();

        let (id, _) = state.register_agent("test".into(), "code-agent".into(), Some("/ws".into()), sender);
        let _ = event_rx.try_recv();

        state.handle_request(&id, "gp-1".into(), "git_push", json!({"remote": "origin", "branch": "main"}));
        let _ = event_rx.try_recv();

        state.execute_approved_request("gp-1");

        let sent = receiver.try_recv().unwrap();
        let msg: Message = serde_json::from_str(&sent).unwrap();
        assert_eq!(msg.msg_type, "git_push_response");
        assert!(msg.payload["success"].as_bool().unwrap());
        assert_eq!(msg.payload["output"], "fake push ok");
    }
}
