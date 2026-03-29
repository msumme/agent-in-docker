use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Mutex;
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::types::OrchestratorEvent;

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

/// Shared state for the MCP HTTP server.
pub struct McpState {
    pub event_tx: mpsc::UnboundedSender<OrchestratorEvent>,
    pub pending: Mutex<std::collections::HashMap<String, oneshot::Sender<Value>>>,
    pub tools: Vec<ToolDef>,
}

impl McpState {
    pub fn new(event_tx: mpsc::UnboundedSender<OrchestratorEvent>) -> Self {
        Self {
            event_tx,
            pending: Mutex::new(std::collections::HashMap::new()),
            tools: default_tools(),
        }
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
            name: "ask_user".into(),
            description: "Ask the user a question and get their answer.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "question": {"type": "string", "description": "The question to ask the user"}
                },
                "required": ["question"]
            }),
        },
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
    body: String,
) -> impl IntoResponse {
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

    let resp = match req.method.as_str() {
        "initialize" => handle_initialize(id),
        "tools/list" => handle_tools_list(&state, id),
        "tools/call" => handle_tools_call(&state, id, req.params).await,
        "notifications/initialized" => {
            // Client notification, no response needed
            return (StatusCode::NO_CONTENT, "").into_response();
        }
        method => {
            warn!("Unknown MCP method: {}", method);
            JsonRpcResponse::error(id, -32601, format!("Method not found: {}", method))
        }
    };

    sse_response(&resp).into_response()
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

async fn handle_tools_call(
    state: &McpState,
    id: Value,
    params: Value,
) -> JsonRpcResponse {
    let tool_name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or(json!({}));

    let (request_type, payload) = match tool_name {
        "ask_user" => {
            let question = args.get("question").and_then(|v| v.as_str()).unwrap_or("");
            ("user_prompt", json!({"question": question}))
        }
        "read_host_file" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            ("file_read", json!({"path": path}))
        }
        "git_push" => {
            let remote = args.get("remote").and_then(|v| v.as_str()).unwrap_or("origin");
            let branch = args.get("branch").and_then(|v| v.as_str()).unwrap_or("");
            ("git_push", json!({"remote": remote, "branch": branch}))
        }
        _ => {
            return JsonRpcResponse::error(id, -32602, format!("Unknown tool: {}", tool_name));
        }
    };

    let request_id = uuid::Uuid::new_v4().to_string();

    // Create oneshot channel for the response
    let (tx, rx) = oneshot::channel();
    {
        let mut pending = state.pending.lock().unwrap();
        pending.insert(request_id.clone(), tx);
    }

    // Emit event to TUI
    let _ = state.event_tx.send(OrchestratorEvent::RequestReceived {
        agent_id: "mcp-http".into(),
        agent_name: "mcp-client".into(),
        request_id: request_id.clone(),
        request_type: request_type.into(),
        payload: payload.clone(),
    });

    info!("MCP tool call: {} (request_id: {})", tool_name, request_id);

    // Wait for response (5 minute timeout)
    let result = tokio::time::timeout(std::time::Duration::from_secs(300), rx).await;

    match result {
        Ok(Ok(response_payload)) => {
            // Check for error response
            if let Some(error_msg) = response_payload.get("message").and_then(|v| v.as_str()) {
                if response_payload.get("code").is_some() {
                    return JsonRpcResponse::success(
                        id,
                        json!({
                            "content": [{"type": "text", "text": format!("Error: {}", error_msg)}],
                            "isError": true,
                        }),
                    );
                }
            }

            // Map response to MCP tool result
            let text = match tool_name {
                "ask_user" => response_payload
                    .get("answer")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                "read_host_file" => response_payload
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                "git_push" => response_payload
                    .get("output")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Push completed")
                    .to_string(),
                _ => serde_json::to_string(&response_payload).unwrap_or_default(),
            };

            JsonRpcResponse::success(
                id,
                json!({ "content": [{"type": "text", "text": text}] }),
            )
        }
        Ok(Err(_)) => {
            JsonRpcResponse::error(id, -32000, "Request cancelled".into())
        }
        Err(_) => {
            // Clean up timed-out request
            let mut pending = state.pending.lock().unwrap();
            pending.remove(&request_id);
            JsonRpcResponse::error(id, -32000, "Request timed out".into())
        }
    }
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
        let json = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ask_user","arguments":{"question":"What color?"}}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.method, "tools/call");
        assert_eq!(req.params["name"], "ask_user");
        assert_eq!(req.params["arguments"]["question"], "What color?");
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
        assert_eq!(tools.len(), 3);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"ask_user"));
        assert!(names.contains(&"read_host_file"));
        assert!(names.contains(&"git_push"));
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

        // Send ask_user tool call in background
        let url = format!("http://{}/mcp", addr);
        let resp_future = tokio::spawn(async move {
            client
                .post(&url)
                .header("Content-Type", "application/json")
                .body(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ask_user","arguments":{"question":"Color?"}}}"#)
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

        // Resolve it
                state.resolve(&request_id, json!({"answer": "red"}));

        // Check response
        let resp = resp_future.await.unwrap();
        let body = resp.text().await.unwrap();
        assert!(body.contains("red"));
    }
}
