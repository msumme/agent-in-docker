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
    fn check_gh_pr_create(&self, role: &str, base: &str) -> PermissionResult;
}

/// No-op permission checker that allows everything (needs TUI approval anyway).
pub struct AllowAllPermissions;
impl PermissionCheck for AllowAllPermissions {
    fn check_gh_pr_create(&self, _role: &str, _base: &str) -> PermissionResult { PermissionResult::NeedsApproval }
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

/// Delivers cross-agent messages into the target agent's interactive session
/// (typically via tmux send-keys), respecting the target's working state.
pub trait MessageDispatcher: Send + Sync {
    /// Deliver an agent-to-agent message. Queued when the target is Working,
    /// sent immediately otherwise.
    fn deliver_agent_message(&self, to: &str, from: &str, content: &str) -> Result<(), String>;
}

/// Default dispatcher — drops messages. Used in tests and when no agent
/// manager is wired.
pub struct NoOpDispatcher;
impl MessageDispatcher for NoOpDispatcher {
    fn deliver_agent_message(&self, _to: &str, _from: &str, _content: &str) -> Result<(), String> {
        Ok(())
    }
}

/// A pending MCP request awaiting human approval. Stores enough metadata
/// for the TUI-approval path to actually execute the handler — without
/// this, MCP-originated approvals were silently timing out because the
/// approval flow only knew how to operate on `ServerState.pending_requests`.
pub struct McpPendingRequest {
    pub tx: oneshot::Sender<Value>,
    pub request_type: String,
    pub payload: Value,
    pub agent_id: String,
    pub agent_name: String,
}

/// Shared state for the MCP HTTP server.
pub struct McpState {
    pub event_tx: mpsc::UnboundedSender<OrchestratorEvent>,
    pub pending: Mutex<std::collections::HashMap<String, McpPendingRequest>>,
    pub tools: Vec<ToolDef>,
    pub registry: Mutex<Box<dyn AgentRegistry>>,
    pub permissions: Mutex<Box<dyn PermissionCheck>>,
    pub dispatcher: Mutex<Box<dyn MessageDispatcher>>,
    pub executor: std::sync::Arc<dyn crate::server::RequestExecutor>,
}

impl McpState {
    pub fn new(
        event_tx: mpsc::UnboundedSender<OrchestratorEvent>,
        permissions: Box<dyn PermissionCheck>,
    ) -> Self {
        Self::with_executor(
            event_tx,
            permissions,
            std::sync::Arc::new(crate::server::RealRequestExecutor),
        )
    }

    pub fn with_executor(
        event_tx: mpsc::UnboundedSender<OrchestratorEvent>,
        permissions: Box<dyn PermissionCheck>,
        executor: std::sync::Arc<dyn crate::server::RequestExecutor>,
    ) -> Self {
        Self {
            event_tx,
            pending: Mutex::new(std::collections::HashMap::new()),
            tools: default_tools(),
            registry: Mutex::new(Box::new(NoOpRegistry)),
            permissions: Mutex::new(permissions),
            dispatcher: Mutex::new(Box::new(NoOpDispatcher)),
            executor,
        }
    }

    pub fn set_registry(&self, registry: Box<dyn AgentRegistry>) {
        *self.registry.lock().unwrap() = registry;
    }

    pub fn set_dispatcher(&self, dispatcher: Box<dyn MessageDispatcher>) {
        *self.dispatcher.lock().unwrap() = dispatcher;
    }

    /// Take a pending MCP request out of the map (for the TUI-approval path
    /// to execute and respond). Used by `ServerState`'s approval handler so
    /// MCP-originated requests are routed through the same execute machinery
    /// as WS-originated ones.
    pub fn take_pending(&self, request_id: &str) -> Option<McpPendingRequest> {
        self.pending.lock().unwrap().remove(request_id)
    }

    /// Resolve a pending MCP request. Returns true if a pending request was found.
    pub fn resolve(&self, request_id: &str, payload: Value) -> bool {
        let mut pending = self.pending.lock().unwrap();
        if let Some(req) = pending.remove(request_id) {
            let _ = req.tx.send(payload);
            true
        } else {
            false
        }
    }
}

fn default_tools() -> Vec<ToolDef> {
    vec![
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
        ToolDef {
            name: "gh_pr_create".into(),
            description: "Create a GitHub pull request using the host's gh credentials. Requires human approval.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "base": {"type": "string", "description": "Base branch to merge into (default: main)"},
                    "head": {"type": "string", "description": "Head branch to create PR from"},
                    "title": {"type": "string", "description": "PR title"},
                    "body": {"type": "string", "description": "PR body text"},
                    "draft": {"type": "boolean", "description": "Create as draft PR (default: false)"}
                },
                "required": ["head", "title"]
            }),
        },
        ToolDef {
            name: "gh_pr_view".into(),
            description: "View details of a GitHub pull request. Returns JSON with PR metadata.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "ref": {"type": "string", "description": "PR number, URL, or branch name"},
                    "workspace": {"type": "string", "description": "Workspace path (optional)"}
                },
                "required": ["ref"]
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
    let agent_role = headers
        .get("x-agent-role")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("code-agent")
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
            handle_tools_call_streaming(state, id, req.params, agent_name, agent_role).into_response()
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

/// Streaming handler for tools that need TUI approval (gh_pr_create).
/// Sends SSE keepalive comments every 15s while waiting for approval.
fn handle_tools_call_streaming(
    state: Arc<McpState>,
    id: Value,
    params: Value,
    agent_name: String,
    agent_role: String,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    // Permission check (before stream, no mutex across yield)
    let denied = {
        let agent_role = &agent_role;
        let perms = state.permissions.lock().unwrap();
        match tool_name.as_str() {
            "gh_pr_create" => {
                let base = args.get("base").and_then(|v| v.as_str()).unwrap_or("main");
                match perms.check_gh_pr_create(agent_role, base) {
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
            let ws_result = state.registry.lock().unwrap().route_message(&agent_name, agent_id, message);
            // Also push the message into the target agent's interactive session
            // (queued if target is Working). The sender's agent_name comes from
            // the x-agent-name header.
            let dispatch_result = state
                .dispatcher
                .lock()
                .unwrap()
                .deliver_agent_message(agent_id, &agent_name, message);
            let text = match (ws_result, dispatch_result) {
                (Ok(()), Ok(())) => format!("Message delivered to {}", agent_id),
                (Err(e), _) => format!("Failed: {}", e),
                (_, Err(e)) => format!("Routed over WS but tmux delivery failed: {}", e),
            };
            let resp = JsonRpcResponse::success(id.clone(), json!({"content": [{"type": "text", "text": text}]}));
            Some(serde_json::to_string(&resp).unwrap())
        }
        "gh_pr_view" => {
            let workspace = args.get("workspace").and_then(|v| v.as_str()).unwrap_or("");
            let ref_ = args.get("ref").and_then(|v| v.as_str()).unwrap_or("");
            let result = state.executor.execute_gh_pr_view(workspace, ref_);
            let resp = match result {
                Ok(v) => {
                    let text = serde_json::to_string(&v).unwrap_or_default();
                    JsonRpcResponse::success(id.clone(), json!({"content": [{"type": "text", "text": text}]}))
                }
                Err(e) => JsonRpcResponse::success(
                    id.clone(),
                    json!({"content": [{"type": "text", "text": format!("Error: {}", e)}], "isError": true}),
                ),
            };
            Some(serde_json::to_string(&resp).unwrap())
        }
        _ => None,
    };

    // Set up approval-gated request (before stream)
    let rx = if denied.is_none() && immediate_response.is_none() {
        let (request_type, payload) = match tool_name.as_str() {
            "gh_pr_create" => {
                let base = args.get("base").and_then(|v| v.as_str()).unwrap_or("main").to_string();
                let head = args.get("head").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let title = args.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let body = args.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let draft = args.get("draft").and_then(|v| v.as_bool()).unwrap_or(false);
                ("gh_pr_create".to_string(), json!({"base": base, "head": head, "title": title, "body": body, "draft": draft}))
            }
            _ => ("unknown".to_string(), json!({})),
        };

        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        // Resolve to the real agent_id from the registry so the execute path
        // can look up the agent's workspace. Falls back to "mcp-{name}" only
        // if the agent isn't registered (which means workspace lookup will
        // fail downstream — that's the right failure: no agent, no workspace).
        let agent_id = state
            .registry
            .lock()
            .unwrap()
            .list_agents()
            .into_iter()
            .find(|p| p.name == agent_name)
            .map(|p| p.id)
            .unwrap_or_else(|| format!("mcp-{}", agent_name));
        {
            let mut pending = state.pending.lock().unwrap();
            pending.insert(
                request_id.clone(),
                McpPendingRequest {
                    tx,
                    request_type: request_type.clone(),
                    payload: payload.clone(),
                    agent_id: agent_id.clone(),
                    agent_name: agent_name.clone(),
                },
            );
        }
        let _ = state.event_tx.send(OrchestratorEvent::RequestReceived {
            agent_id: agent_id.clone(),
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
                                        "gh_pr_create" => response_payload.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
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
        let state = McpState::new(event_tx, Box::new(AllowAllPermissions));
        let resp = handle_tools_list(&state, json!(1));
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.len() >= 2, "at least two tools must remain listed");
        assert!(names.contains(&"list_agents"));
        assert!(names.contains(&"message_agent"));
        assert!(!names.contains(&"team_spawn"), "team_spawn must be removed");
        assert!(!names.contains(&"team_suspend"), "team_suspend must be removed");
        assert!(!names.contains(&"team_resume"), "team_resume must be removed");
        assert!(!names.contains(&"team_complete"), "team_complete must be removed");
        assert!(!names.contains(&"team_kill"), "team_kill must be removed");
    }

    #[tokio::test]
    async fn mcp_gh_pr_view_dispatches_via_executor() {
        use std::sync::Arc;

        struct FakeExec;
        impl crate::server::RequestExecutor for FakeExec {
            fn execute_gh_pr_create(&self, _: &str, _: &str, _: &str, _: &str, _: &str, _: bool) -> Result<Value, String> {
                Ok(json!({"url": "https://github.com/o/r/pull/1", "number": 1}))
            }
            fn execute_gh_pr_view(&self, _: &str, _: &str) -> Result<Value, String> {
                Ok(json!({"number": 42, "url": "https://github.com/o/r/pull/42", "title": "Fake View PR"}))
            }
        }

        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let state = Arc::new(McpState::with_executor(
            event_tx,
            Box::new(AllowAllPermissions),
            Arc::new(FakeExec),
        ));
        let app = mcp_router(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let tool_call = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "gh_pr_view",
                "arguments": {"ref": "42"}
            }
        });

        let resp = reqwest::Client::new()
            .post(format!("http://{}/mcp", addr))
            .header("Content-Type", "application/json")
            .body(serde_json::to_string(&tool_call).unwrap())
            .send()
            .await
            .unwrap();

        // gh_pr_view is immediate — no RequestReceived event
        assert!(event_rx.try_recv().is_err(), "gh_pr_view must not emit a RequestReceived event");

        let body = resp.text().await.unwrap();
        assert!(body.contains("Fake View PR"), "Response should contain PR title: {}", body);
        assert!(body.contains("42"), "Response should contain PR number: {}", body);
    }

    #[tokio::test]
    async fn mcp_state_resolve_pending() {
        let (event_tx, _) = mpsc::unbounded_channel();
        let state = McpState::new(event_tx, Box::new(AllowAllPermissions));

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = state.pending.lock().unwrap();
            pending.insert(
                "req-1".into(),
                McpPendingRequest {
                    tx,
                    request_type: "user_prompt".into(),
                    payload: json!({}),
                    agent_id: "agent-1".into(),
                    agent_name: "alice".into(),
                },
            );
        }

        let resolved =         state.resolve("req-1", json!({"answer": "blue"}));
        assert!(resolved);

        let result = rx.await.unwrap();
        assert_eq!(result["answer"], "blue");
    }

    #[tokio::test]
    async fn mcp_state_resolve_nonexistent_returns_false() {
        let (event_tx, _) = mpsc::unbounded_channel();
        let state = McpState::new(event_tx, Box::new(AllowAllPermissions));
        assert!(!        state.resolve("nonexistent", json!({})));
    }

    #[tokio::test]
    async fn integration_http_initialize() {
        let (event_tx, _) = mpsc::unbounded_channel();
        let state = Arc::new(McpState::new(event_tx, Box::new(AllowAllPermissions)));
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
    async fn message_agent_passes_real_sender_not_mcp_client() {
        struct CapturingRegistry {
            calls: Arc<Mutex<Vec<(String, String, String)>>>,
        }
        impl AgentRegistry for CapturingRegistry {
            fn list_agents(&self) -> Vec<PeerInfo> { vec![] }
            fn route_message(&self, from: &str, to: &str, content: &str) -> Result<(), String> {
                self.calls.lock().unwrap().push((from.to_string(), to.to_string(), content.to_string()));
                Ok(())
            }
        }

        let calls: Arc<Mutex<Vec<(String, String, String)>>> = Arc::new(Mutex::new(vec![]));
        let (event_tx, _) = mpsc::unbounded_channel();
        let state = Arc::new(McpState::new(event_tx, Box::new(AllowAllPermissions)));
        state.set_registry(Box::new(CapturingRegistry { calls: calls.clone() }));
        let app = mcp_router(state);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });

        reqwest::Client::new()
            .post(format!("http://{}/mcp", addr))
            .header("Content-Type", "application/json")
            .header("x-agent-name", "prod-1")
            .body(serde_json::to_string(&json!({
                "jsonrpc": "2.0", "id": 1,
                "method": "tools/call",
                "params": {"name": "message_agent", "arguments": {"agentId": "rev-1", "message": "done!"}}
            })).unwrap())
            .send().await.unwrap();

        let recorded = calls.lock().unwrap();
        assert_eq!(recorded.len(), 1, "route_message must be called once");
        let (from, to, _) = &recorded[0];
        assert_eq!(from, "prod-1", "from must be the real sender header, not 'mcp-client'");
        assert_eq!(to, "rev-1");
    }
}
