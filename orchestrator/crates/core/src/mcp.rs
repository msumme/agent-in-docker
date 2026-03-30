use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Mutex;
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::permissions::PermissionResult;
use crate::types::{OrchestratorEvent, PeerInfo};

/// JSON-RPC 2.0 request.
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// JSON-RPC 2.0 response.
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

impl JsonRpcResponse {
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Value, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError { code, message }),
        }
    }
}

/// Tool definition for MCP tools/list response.
#[derive(Debug, Serialize, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// Injectable permission checker for MCP tool calls.
pub trait PermissionCheck: Send + Sync {
    fn check_file_read(&self, role: &str, path: &str) -> PermissionResult;
    fn check_git_push(&self, role: &str, remote: &str) -> PermissionResult;
}

/// No-op permission checker that allows everything (needs TUI approval anyway).
pub struct AllowAllPermissions;
impl PermissionCheck for AllowAllPermissions {
    fn check_file_read(&self, _role: &str, _path: &str) -> PermissionResult { PermissionResult::NeedsApproval }
    fn check_git_push(&self, _role: &str, _remote: &str) -> PermissionResult { PermissionResult::NeedsApproval }
}

/// Injectable registry for querying connected agents.
pub trait AgentRegistry: Send + Sync {
    fn list_agents(&self) -> Vec<PeerInfo>;
    fn route_message(&self, from: &str, to: &str, content: &str) -> Result<(), String>;
}

/// No-op registry for tests and when WS server isn't available.
pub struct NoOpRegistry;
impl AgentRegistry for NoOpRegistry {
    fn list_agents(&self) -> Vec<PeerInfo> { vec![] }
    fn route_message(&self, _from: &str, _to: &str, _content: &str) -> Result<(), String> {
        Err("No agent registry available".into())
    }
}

/// Shared state for the MCP HTTP server.
pub struct McpState {
    pub event_tx: mpsc::UnboundedSender<OrchestratorEvent>,
    pub pending: Mutex<std::collections::HashMap<String, oneshot::Sender<Value>>>,
    pub tools: Vec<ToolDef>,
    pub registry: Mutex<Box<dyn AgentRegistry>>,
    pub permissions: Mutex<Box<dyn PermissionCheck>>,
}

impl McpState {
    pub fn new(event_tx: mpsc::UnboundedSender<OrchestratorEvent>) -> Self {
        Self {
            event_tx,
            pending: Mutex::new(std::collections::HashMap::new()),
            tools: default_tools(),
            registry: Mutex::new(Box::new(NoOpRegistry)),
            permissions: Mutex::new(Box::new(AllowAllPermissions)),
        }
    }

    pub fn set_registry(&self, registry: Box<dyn AgentRegistry>) {
        *self.registry.lock().unwrap() = registry;
    }

    pub fn set_permissions(&self, checker: Box<dyn PermissionCheck>) {
        *self.permissions.lock().unwrap() = checker;
    }

    /// Resolve a pending MCP request. Returns true if a pending request was found.
    pub fn resolve(&self, request_id: &str, payload: Value) -> bool {
        let mut pending = self.pending.lock().unwrap();
        if let Some(sender) = pending.remove(request_id) {
            let _ = sender.send(payload);
            true
        } else {
            false
        }
    }
}

fn default_tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "read_host_file".into(),
            description: "Read a file from the host machine. Only allowed paths are accessible.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Absolute path to the file on the host"}
                },
                "required": ["path"]
            }),
        },
        ToolDef {
            name: "git_push".into(),
            description: "Push the current branch to a remote using the host's git credentials.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "remote": {"type": "string", "description": "Git remote name (default: origin)"},
                    "branch": {"type": "string", "description": "Branch to push (default: current branch)"}
                }
            }),
        },
        ToolDef {
            name: "list_agents".into(),
            description: "List all currently connected agents and their roles.".into(),
            input_schema: json!({"type": "object", "properties": {}}),
        },
        ToolDef {
            name: "message_agent".into(),
            description: "Send a message to another connected agent.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "agentId": {"type": "string", "description": "ID of the agent to message"},
                    "message": {"type": "string", "description": "Message content"}
                },
                "required": ["agentId", "message"]
            }),
        },
    ]
}

/// Format a JSON-RPC response as an SSE event.
fn sse_response(resp: &JsonRpcResponse) -> impl IntoResponse {
    let body = format!(
        "event: message\ndata: {}\n\n",
        serde_json::to_string(resp).unwrap()
    );
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/event-stream"),
         (header::CACHE_CONTROL, "no-cache"),
         (header::CONNECTION, "keep-alive")],
        body,
    )
}

async fn handle_mcp(
    State(state): State<Arc<McpState>>,
    headers: axum::http::HeaderMap,
    body: String,
) -> impl IntoResponse {
    let agent_name = headers
        .get("x-agent-name")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();
    let req: JsonRpcRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            let resp = JsonRpcResponse::error(
                Value::Null,
                -32700,
                format!("Parse error: {}", e),
            );
            return sse_response(&resp).into_response();
        }
    };

    let id = req.id.clone().unwrap_or(Value::Null);

    match req.method.as_str() {
        "initialize" => sse_response(&handle_initialize(id)).into_response(),
        "tools/list" => sse_response(&handle_tools_list(&state, id)).into_response(),
        "tools/call" => {
            // Return SSE stream with keepalives for approval-gated tools
            handle_tools_call_streaming(state, id, req.params, agent_name).into_response()
        }
        "notifications/initialized" => {
            (StatusCode::NO_CONTENT, "").into_response()
        }
        method => {
            warn!("Unknown MCP method: {}", method);
            sse_response(&JsonRpcResponse::error(id, -32601, format!("Method not found: {}", method))).into_response()
        }
    }
}

fn handle_initialize(id: Value) -> JsonRpcResponse {
    JsonRpcResponse::success(
        id,
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": { "listChanged": true }
            },
            "serverInfo": {
                "name": "agent-bridge",
                "version": "0.1.0"
            }
        }),
    )
}

fn handle_tools_list(state: &McpState, id: Value) -> JsonRpcResponse {
    JsonRpcResponse::success(id, json!({ "tools": state.tools }))
}

/// Streaming handler for tools that need TUI approval (file_read, git_push).
/// Sends SSE keepalive comments every 15s while waiting for approval.
fn handle_tools_call_streaming(
    state: Arc<McpState>,
    id: Value,
    params: Value,
    agent_name: String,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    // Permission check (before stream, no mutex across yield)
    let denied = {
        let agent_role = "code-agent";
        let perms = state.permissions.lock().unwrap();
        match tool_name.as_str() {
            "read_host_file" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                match perms.check_file_read(agent_role, path) {
                    PermissionResult::Deny(reason) => Some(reason),
                    _ => None,
                }
            }
            "git_push" => {
                let remote = args.get("remote").and_then(|v| v.as_str()).unwrap_or("origin");
                match perms.check_git_push(agent_role, remote) {
                    PermissionResult::Deny(reason) => Some(reason),
                    _ => None,
                }
            }
            _ => None,
        }
    };

    // Immediate tools (no approval needed)
    let immediate_response: Option<String> = match tool_name.as_str() {
        "list_agents" => {
            let agents = state.registry.lock().unwrap().list_agents();
            let text = serde_json::to_string_pretty(&agents).unwrap_or_default();
            let resp = JsonRpcResponse::success(id.clone(), json!({"content": [{"type": "text", "text": text}]}));
            Some(serde_json::to_string(&resp).unwrap())
        }
        "message_agent" => {
            let agent_id = args.get("agentId").and_then(|v| v.as_str()).unwrap_or("");
            let message = args.get("message").and_then(|v| v.as_str()).unwrap_or("");
            let result = state.registry.lock().unwrap().route_message("mcp-client", agent_id, message);
            let text = match result {
                Ok(()) => format!("Message delivered to {}", agent_id),
                Err(e) => format!("Failed: {}", e),
            };
            let resp = JsonRpcResponse::success(id.clone(), json!({"content": [{"type": "text", "text": text}]}));
            Some(serde_json::to_string(&resp).unwrap())
        }
        _ => None,
    };

    // Set up approval-gated request (before stream)
    let rx = if denied.is_none() && immediate_response.is_none() {
        let (request_type, payload) = match tool_name.as_str() {
            "read_host_file" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
                ("file_read".to_string(), json!({"path": path}))
            }
            "git_push" => {
                let remote = args.get("remote").and_then(|v| v.as_str()).unwrap_or("origin").to_string();
                let branch = args.get("branch").and_then(|v| v.as_str()).unwrap_or("").to_string();
                ("git_push".to_string(), json!({"remote": remote, "branch": branch}))
            }
            _ => ("unknown".to_string(), json!({})),
        };

        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = state.pending.lock().unwrap();
            pending.insert(request_id.clone(), tx);
        }
        let _ = state.event_tx.send(OrchestratorEvent::RequestReceived {
            agent_id: format!("mcp-{}", agent_name),
            agent_name: agent_name.clone(),
            request_id: request_id.clone(),
            request_type,
            payload,
        });
        info!("MCP tool call: {} (request_id: {})", tool_name, request_id);
        Some((rx, request_id))
    } else {
        None
    };

    // Build the stream (no mutex guards held here)
    let tool = tool_name.clone();
    let stream = async_stream::stream! {
        // Denied
        if let Some(reason) = denied {
            let resp = JsonRpcResponse::success(id, json!({"content": [{"type": "text", "text": format!("Permission denied: {}", reason)}], "isError": true}));
            yield Ok::<_, Infallible>(Event::default().event("message").data(serde_json::to_string(&resp).unwrap()));
            return;
        }

        // Immediate
        if let Some(data) = immediate_response {
            yield Ok(Event::default().event("message").data(data));
            return;
        }

        // Approval-gated: stream keepalives
        if let Some((rx, _request_id)) = rx {
            let mut interval = tokio::time::interval(Duration::from_secs(15));
            let timeout = tokio::time::sleep(Duration::from_secs(300));
            tokio::pin!(timeout);
            tokio::pin!(rx);

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        yield Ok(Event::default().comment("keepalive"));
                    }
                    result = &mut rx => {
                        match result {
                            Ok(response_payload) => {
                                if response_payload.get("code").is_some() {
                                    let msg = response_payload.get("message").and_then(|v| v.as_str()).unwrap_or("Error");
                                    let resp = JsonRpcResponse::success(id, json!({"content": [{"type": "text", "text": format!("Error: {}", msg)}], "isError": true}));
                                    yield Ok(Event::default().event("message").data(serde_json::to_string(&resp).unwrap()));
                                } else {
                                    let text = match tool.as_str() {
                                        "read_host_file" => response_payload.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                        "git_push" => response_payload.get("output").and_then(|v| v.as_str()).unwrap_or("Push completed").to_string(),
                                        _ => serde_json::to_string(&response_payload).unwrap_or_default(),
                                    };
                                    let resp = JsonRpcResponse::success(id, json!({"content": [{"type": "text", "text": text}]}));
                                    yield Ok(Event::default().event("message").data(serde_json::to_string(&resp).unwrap()));
                                }
                            }
                            Err(_) => {
                                let resp = JsonRpcResponse::error(id, -32000, "Request cancelled".into());
                                yield Ok(Event::default().event("message").data(serde_json::to_string(&resp).unwrap()));
                            }
                        }
                        break;
                    }
                    _ = &mut timeout => {
                        let resp = JsonRpcResponse::error(id, -32000, "Request timed out".into());
                        yield Ok(Event::default().event("message").data(serde_json::to_string(&resp).unwrap()));
                        break;
                    }
                }
            }
        }
    };
    Sse::new(stream)
}

/// Create the axum router for the MCP HTTP server.
pub fn mcp_router(state: Arc<McpState>) -> Router {
    Router::new()
        .route("/mcp", post(handle_mcp))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_jsonrpc_request() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.method, "initialize");
        assert_eq!(req.id, Some(json!(1)));
    }

    #[test]
    fn parse_tools_call_request() {
        let json = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"read_host_file","arguments":{"path":"/etc/hosts"}}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.method, "tools/call");
        assert_eq!(req.params["name"], "read_host_file");
        assert_eq!(req.params["arguments"]["path"], "/etc/hosts");
    }

    #[test]
    fn format_success_response() {
        let resp = JsonRpcResponse::success(json!(1), json!({"tools": []}));
        let serialized = serde_json::to_string(&resp).unwrap();
        assert!(serialized.contains("\"result\""));
        assert!(!serialized.contains("\"error\""));
    }

    #[test]
    fn format_error_response() {
        let resp = JsonRpcResponse::error(json!(1), -32601, "Not found".into());
        let serialized = serde_json::to_string(&resp).unwrap();
        assert!(serialized.contains("\"error\""));
        assert!(serialized.contains("-32601"));
    }

    #[test]
    fn initialize_response_has_tools_capability() {
        let resp = handle_initialize(json!(1));
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert!(result["capabilities"]["tools"]["listChanged"].as_bool().unwrap());
    }

    #[test]
    fn tools_list_returns_all_tools() {
        let (event_tx, _) = mpsc::unbounded_channel();
        let state = McpState::new(event_tx);
        let resp = handle_tools_list(&state, json!(1));
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 4);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"read_host_file"));
        assert!(names.contains(&"git_push"));
        assert!(names.contains(&"list_agents"));
        assert!(names.contains(&"message_agent"));
    }

    #[tokio::test]
    async fn mcp_state_resolve_pending() {
        let (event_tx, _) = mpsc::unbounded_channel();
        let state = McpState::new(event_tx);

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = state.pending.lock().unwrap();
            pending.insert("req-1".into(), tx);
        }

        let resolved =         state.resolve("req-1", json!({"answer": "blue"}));
        assert!(resolved);

        let result = rx.await.unwrap();
        assert_eq!(result["answer"], "blue");
    }

    #[tokio::test]
    async fn mcp_state_resolve_nonexistent_returns_false() {
        let (event_tx, _) = mpsc::unbounded_channel();
        let state = McpState::new(event_tx);
        assert!(!        state.resolve("nonexistent", json!({})));
    }

    #[tokio::test]
    async fn integration_http_initialize() {
        let (event_tx, _) = mpsc::unbounded_channel();
        let state = Arc::new(McpState::new(event_tx));
        let app = mcp_router(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{}/mcp", addr))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body = resp.text().await.unwrap();
        assert!(body.contains("event: message"));
        assert!(body.contains("agent-bridge"));
        assert!(body.contains("2024-11-05"));
    }

    #[tokio::test]
    async fn integration_tools_call_with_resolution() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let state = Arc::new(McpState::new(event_tx));
        let app = mcp_router(state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let client = reqwest::Client::new();

        // Send read_host_file tool call in background
        let url = format!("http://{}/mcp", addr);
        let resp_future = tokio::spawn(async move {
            client
                .post(&url)
                .header("Content-Type", "application/json")
                .header("X-Agent-Name", "test-agent")
                .body(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"read_host_file","arguments":{"path":"/tmp/test"}}}"#)
                .send()
                .await
                .unwrap()
        });

        // Wait for the event
        let event = event_rx.recv().await.unwrap();
        let request_id = match event {
            OrchestratorEvent::RequestReceived { request_id, .. } => request_id,
            _ => panic!("Expected RequestReceived"),
        };

        // Resolve it with file content
        state.resolve(&request_id, json!({"content": "file data here"}));

        // Check response
        let resp = resp_future.await.unwrap();
        let body = resp.text().await.unwrap();
        assert!(body.contains("file data here"));
    }
}
