use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    #[serde(rename = "type")]
    pub msg_type: String,
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterPayload {
    pub name: String,
    pub role: String,
    #[serde(default, rename = "workspacePath")]
    pub workspace_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterAckPayload {
    #[serde(rename = "agentId")]
    pub agent_id: String,
    pub peers: Vec<PeerInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub id: String,
    pub name: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPromptPayload {
    pub question: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPromptResponsePayload {
    pub answer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPayload {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReadPayload {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitPushPayload {
    #[serde(default = "default_remote")]
    pub remote: String,
    #[serde(default)]
    pub branch: String,
}

fn default_remote() -> String {
    "origin".into()
}

#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    pub role: String,
    pub workspace_path: Option<String>,
}

/// Status of a managed agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStatus {
    Starting,
    Connected,
    Working,
    Idle,
    Exited,
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Starting => write!(f, "starting"),
            Self::Connected => write!(f, "connected"),
            Self::Working => write!(f, "working"),
            Self::Idle => write!(f, "idle"),
            Self::Exited => write!(f, "exited"),
        }
    }
}

/// An agent managed by the orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedAgent {
    pub name: String,
    pub role: String,
    pub mode: String,
    pub status: AgentStatus,
    pub tmux_window: String,
    pub container_name: String,
    pub project_path: String,
    pub prompt: String,
    pub ws_agent_id: Option<String>,
    pub last_activity: String,
}

/// Request from CLI to start an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartAgentPayload {
    pub name: String,
    pub role: String,
    pub mode: String,
    pub project_path: String,
    pub prompt: String,
    pub agent_dir: String,
    pub image_name: String,
    pub network_name: String,
    pub orchestrator_port: u16,
    pub mcp_port: u16,
    pub dolt_port: Option<u16>,
}

/// Events emitted by the core server to the frontend (TUI).
#[derive(Debug, Clone)]
pub enum OrchestratorEvent {
    AgentConnected(AgentInfo),
    AgentDisconnected { id: String },
    RequestReceived {
        agent_id: String,
        agent_name: String,
        request_id: String,
        request_type: String,
        payload: Value,
    },
    AgentOutput {
        agent_id: String,
        text: String,
    },
    ManagedAgentUpdated(ManagedAgent),
}

/// Commands sent from the frontend (TUI) to the core server.
#[derive(Debug, Clone)]
pub enum TuiCommand {
    RespondToRequest {
        request_id: String,
        payload: Value,
    },
    ApproveRequest {
        request_id: String,
    },
    DenyRequest {
        request_id: String,
        reason: String,
    },
    SendTask {
        agent_id: String,
        prompt: String,
    },
    Shutdown,
}
