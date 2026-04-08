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
    pub seed_credentials: String,
    pub image_name: String,
    pub network_name: String,
    pub orchestrator_port: u16,
    pub mcp_port: u16,
    pub dolt_port: Option<u16>,
}

impl StartAgentPayload {
    /// Generate podman run arguments for this agent configuration.
    /// Returns args to follow `podman run -it` (includes --rm through image name).
    pub fn container_run_args(&self) -> Vec<String> {
        let mut args = vec![
            "--rm".to_string(),
            "--name".to_string(),
            self.name.clone(),
            "--network".to_string(),
            self.network_name.clone(),
            "--cap-drop=ALL".to_string(),
            "--cap-add=NET_RAW".to_string(),
            "--cap-add=DAC_OVERRIDE".to_string(),
            "-v".to_string(),
            format!("{}:/workspace:Z", self.project_path),
            "-v".to_string(),
            format!("{}:/root/.claude:Z", self.agent_dir),
            "-v".to_string(),
            format!("{}:/root/.claude/.credentials.json:Z", self.seed_credentials),
            "-e".to_string(),
            format!(
                "ORCHESTRATOR_URL=ws://host.containers.internal:{}",
                self.orchestrator_port
            ),
            "-e".to_string(),
            format!("MCP_PORT={}", self.mcp_port),
            "-e".to_string(),
            format!("AGENT_NAME={}", self.name),
            "-e".to_string(),
            format!("AGENT_ROLE={}", self.role),
            "-e".to_string(),
            format!("AGENT_MODE={}", self.mode),
            "-e".to_string(),
            format!("AGENT_PROMPT={}", self.prompt),
            "-e".to_string(),
            "IS_SANDBOX=1".to_string(),
        ];

        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            if !key.is_empty() {
                args.extend_from_slice(&["-e".to_string(), format!("ANTHROPIC_API_KEY={}", key)]);
            }
        }

        if let Some(port) = self.dolt_port {
            args.extend_from_slice(&[
                "-e".to_string(), "DOLT_HOST=host.containers.internal".to_string(),
                "-e".to_string(), format!("DOLT_PORT={}", port),
            ]);
        }

        args.push(self.image_name.clone());
        args
    }
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
            seed_credentials: "/creds".into(),
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
    fn container_run_args_has_required_fields() {
        let p = StartAgentPayload {
            name: "test".into(),
            role: "code-agent".into(),
            mode: "oneshot".into(),
            project_path: "/tmp/project".into(),
            prompt: "hi".into(),
            agent_dir: "/tmp/agent".into(),
            seed_credentials: "/tmp/creds.json".into(),
            image_name: "agent-img".into(),
            network_name: "agent-net".into(),
            orchestrator_port: 9800,
            mcp_port: 9801,
            dolt_port: None,
        };
        let args = p.container_run_args();
        assert!(args.iter().any(|a| a == "AGENT_NAME=test"));
        assert!(args.iter().any(|a| a == "AGENT_MODE=oneshot"));
        assert!(args.iter().any(|a| a == "MCP_PORT=9801"));
        assert!(args.iter().any(|a| a == "IS_SANDBOX=1"));
        assert!(args.iter().any(|a| a == "agent-img"));
        assert!(args.iter().any(|a| a.contains("/workspace:Z")));
    }

    #[test]
    fn container_run_args_includes_dolt_when_set() {
        let p = StartAgentPayload {
            name: "test".into(),
            role: "code-agent".into(),
            mode: "oneshot".into(),
            project_path: "/tmp".into(),
            prompt: String::new(),
            agent_dir: "/tmp/a".into(),
            seed_credentials: "/tmp/c.json".into(),
            image_name: "img".into(),
            network_name: "net".into(),
            orchestrator_port: 9800,
            mcp_port: 9801,
            dolt_port: Some(3307),
        };
        let args = p.container_run_args();
        assert!(args.iter().any(|a| a == "DOLT_PORT=3307"));
        assert!(args.iter().any(|a| a == "DOLT_HOST=host.containers.internal"));
    }

    #[test]
    fn git_push_payload_defaults() {
        let json = r#"{}"#;
        let parsed: GitPushPayload = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.remote, "origin");
        assert_eq!(parsed.branch, "");
    }
}
