# Fix ask_user MCP timeout (agent-in-docker-1gd)

## Problem
Claude Code has a ~60s timeout on MCP tool calls. The `ask_user` tool blocks the HTTP request waiting for the TUI user to respond. If the user takes longer than 60s, Claude Code times out and the response is lost.

## Root Cause
The MCP handler in `mcp.rs` returns a single SSE event when the response is ready:
```
event: message
data: {"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"blue"}]}}
```
Between the request and this response, nothing is sent. The HTTP connection is idle, and Claude Code's client-side timeout fires.

## Fix: SSE Keepalive Stream
Instead of returning a single SSE event, stream the response as a series of SSE events:
1. Immediately start sending SSE keepalive comments (`: keepalive\n\n`) every 15 seconds
2. When the TUI resolves the request, send the actual `event: message` with the result
3. Close the stream

SSE comments (lines starting with `:`) are ignored by clients but keep the connection alive. This is standard SSE behavior.

## Implementation

### Change `handle_tools_call` return type
Currently returns `JsonRpcResponse` (serialized to a single SSE event). Change to return an axum `Sse<impl Stream>` for tool calls that need TUI interaction.

### New response flow for ask_user/file_read/git_push:
```rust
async fn handle_mcp(...) -> impl IntoResponse {
    // For tools/call that need TUI approval:
    let (tx, rx) = oneshot::channel();
    // store in pending...
    // emit event to TUI...

    // Return an SSE stream
    let stream = async_stream::stream! {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    yield Ok::<_, Infallible>(Event::default().comment("keepalive"));
                }
                result = &mut rx => {
                    let resp = build_response(result);
                    yield Ok(Event::default().event("message").data(serde_json::to_string(&resp).unwrap()));
                    break;
                }
            }
        }
    };
    Sse::new(stream)
}
```

### Files to modify
- `orchestrator/crates/core/src/mcp.rs` -- Split `handle_mcp` into immediate responses (initialize, tools/list) and streaming responses (tools/call)
- `orchestrator/crates/core/Cargo.toml` -- Add `async-stream`, `axum::response::sse`

### Tests
- Unit: SSE stream sends keepalive comments at interval
- Unit: SSE stream sends result when oneshot resolves
- Integration: HTTP client receives keepalive comments before result

### Risk
- Need to verify Claude Code's MCP client actually respects SSE keepalive comments (it should -- standard SSE)
- axum's `Sse` type may need different routing than the current `post()` handler
- The `handle_mcp` function currently returns `impl IntoResponse` -- may need to become an enum or Box<dyn> to handle both immediate and streaming cases
