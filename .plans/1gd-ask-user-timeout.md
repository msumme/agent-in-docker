# Fix MCP timeout for approval-gated tools (agent-in-docker-1gd)

## Problem
MCP tool calls that need TUI approval (file_read, git_push) can timeout if the user takes >60s to approve. Claude Code's HTTP client gives up waiting.

## Changes

### 1. Remove ask_user MCP tool
Users interact with agents directly via tmux. No need for an MCP round-trip. Drop ask_user from the tool list and handler.

### 2. SSE keepalive for approval-gated tools
file_read and git_push go through MCP HTTP and need TUI approval. Use SSE streaming with keepalive comments to hold the connection open:
- Immediately start sending `: keepalive\n\n` every 15s
- When TUI approves/denies, send the actual result event
- Close the stream

### Files
- `mcp.rs` -- Remove ask_user tool, switch tools/call to SSE stream response
- `Cargo.toml` -- Add async-stream
- `entrypoint.sh` -- Remove ask_user from docs/comments
- `ARCHITECTURE_DECISIONS.md` -- Update: ask_user removed, direct tmux interaction instead
