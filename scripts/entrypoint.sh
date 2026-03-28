#!/bin/bash
set -e

# Generate MCP config for Claude Code pointing to the bridge
cat > /tmp/mcp-config.json <<EOF
{
  "mcpServers": {
    "agent-bridge": {
      "command": "node",
      "args": ["/opt/bridge/dist/index.js"],
      "env": {
        "ORCHESTRATOR_URL": "${ORCHESTRATOR_URL}",
        "AGENT_NAME": "${AGENT_NAME}",
        "AGENT_ROLE": "${AGENT_ROLE}"
      }
    }
  }
}
EOF

echo "[entrypoint] Starting agent: ${AGENT_NAME} (role: ${AGENT_ROLE})" >&2
echo "[entrypoint] Orchestrator URL: ${ORCHESTRATOR_URL}" >&2
echo "[entrypoint] MCP config written to /tmp/mcp-config.json" >&2

exec claude \
  --dangerously-skip-permissions \
  --mcp-config /tmp/mcp-config.json \
  -p "${AGENT_PROMPT}"
