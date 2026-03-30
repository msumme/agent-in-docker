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
    ReattachAgent {
        name: String,
    },
    StartNewAgent {
        name: String,
        role: String,
    },
    Shutdown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn message_serialization_roundtrip() {
        let msg = Message {
            id: "test-1".into(),
            msg_type: "user_prompt".into(),
            from: "agent-1".into(),
            to: Some("orchestrator".into()),
            payload: json!({"question": "What color?"}),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "test-1");
        assert_eq!(parsed.msg_type, "user_prompt");
        assert_eq!(parsed.payload["question"], "What color?");
    }

    #[test]
    fn message_without_to_omits_field() {
        let msg = Message {
            id: "1".into(),
            msg_type: "register".into(),
            from: "pending".into(),
            to: None,
            payload: json!({}),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("\"to\""));
    }

    #[test]
    fn register_payload_roundtrip() {
        let p = RegisterPayload { name: "bob".into(), role: "code-agent".into(), workspace_path: Some("/ws".into()) };
        let json = serde_json::to_string(&p).unwrap();
        let parsed: RegisterPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "bob");
        assert_eq!(parsed.workspace_path.as_deref(), Some("/ws"));
    }

    #[test]
    fn register_payload_missing_workspace() {
        let json = r#"{"name":"alice","role":"review-agent"}"#;
        let parsed: RegisterPayload = serde_json::from_str(json).unwrap();
        assert!(parsed.workspace_path.is_none());
    }

    #[test]
    fn agent_status_display() {
        assert_eq!(AgentStatus::Starting.to_string(), "starting");
        assert_eq!(AgentStatus::Working.to_string(), "working");
        assert_eq!(AgentStatus::Exited.to_string(), "exited");
    }

    #[test]
    fn managed_agent_serialization() {
        let agent = ManagedAgent {
            name: "alice".into(),
            role: "code-agent".into(),
            mode: "long-running".into(),
            status: AgentStatus::Connected,
            tmux_window: "alice".into(),
            container_name: "alice".into(),
            project_path: "/workspace".into(),
            prompt: "hi".into(),
            ws_agent_id: Some("ws-1".into()),
            last_activity: "connected".into(),
        };
        let json = serde_json::to_string(&agent).unwrap();
        let parsed: ManagedAgent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.status, AgentStatus::Connected);
        assert_eq!(parsed.ws_agent_id.as_deref(), Some("ws-1"));
    }

    #[test]
    fn start_agent_payload_roundtrip() {
        let p = StartAgentPayload {
            name: "test".into(),
            role: "code-agent".into(),
            mode: "oneshot".into(),
            project_path: "/tmp".into(),
            prompt: "hi".into(),
            agent_dir: "/a".into(),
            image_name: "img".into(),
            network_name: "net".into(),
            orchestrator_port: 9800,
            mcp_port: 9801,
            dolt_port: Some(3307),
        };
        let json = serde_json::to_string(&p).unwrap();
        let parsed: StartAgentPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.dolt_port, Some(3307));
    }

    #[test]
    fn git_push_payload_defaults() {
        let json = r#"{}"#;
        let parsed: GitPushPayload = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.remote, "origin");
        assert_eq!(parsed.branch, "");
    }
}
