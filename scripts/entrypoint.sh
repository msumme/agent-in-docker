#!/bin/bash
set -e

# Write Claude Code OAuth credentials if provided via env var
if [ -n "${CLAUDE_CREDENTIALS:-}" ]; then
    mkdir -p ~/.claude
    echo "${CLAUDE_CREDENTIALS}" > ~/.claude/.credentials.json
    chmod 600 ~/.claude/.credentials.json
    echo "[entrypoint] Wrote OAuth credentials to ~/.claude/.credentials.json" >&2
fi

# ~/.claude.json lives OUTSIDE ~/.claude/ but we only mount ~/.claude/.
# We store a copy inside the mount and symlink it on startup.
if [ ! -f ~/.claude.json ] && [ -f ~/.claude/.claude.json ]; then
    ln -s ~/.claude/.claude.json ~/.claude.json
    echo "[entrypoint] Symlinked ~/.claude.json from mount" >&2
elif [ ! -f ~/.claude.json ]; then
    BACKUP=$(ls -t ~/.claude/backups/.claude.json.backup.* 2>/dev/null | head -1)
    if [ -n "${BACKUP}" ]; then
        cp "${BACKUP}" ~/.claude/.claude.json
        ln -s ~/.claude/.claude.json ~/.claude.json
        echo "[entrypoint] Restored ~/.claude.json from backup" >&2
    fi
fi

# Check if Claude Code has valid credentials
if [ -z "${ANTHROPIC_API_KEY:-}" ] && [ ! -f ~/.claude/.credentials.json ]; then
    echo "[entrypoint] No credentials found. Running 'claude login'..." >&2
    echo "[entrypoint] Follow the URL below to authenticate." >&2
    claude login
    echo "[entrypoint] Login complete." >&2
fi

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

# --dangerously-skip-permissions cannot run as root.
# If we're root, copy credentials to the 'node' user and drop privileges.
if [ "$(id -u)" = "0" ]; then
    # Set up node user's home with credentials
    mkdir -p /home/node/.claude
    cp -a ~/.claude/. /home/node/.claude/
    cp ~/.claude.json /home/node/.claude.json 2>/dev/null || true
    ln -sf /home/node/.claude/.claude.json /home/node/.claude.json 2>/dev/null || true
    chown -R node:node /home/node/.claude /home/node/.claude.json 2>/dev/null || true
    chown node:node /tmp/mcp-config.json

    exec su -s /bin/bash node -c "HOME=/home/node exec claude \
        --dangerously-skip-permissions \
        --mcp-config /tmp/mcp-config.json \
        -p '${AGENT_PROMPT}'"
else
    exec claude \
        --dangerously-skip-permissions \
        --mcp-config /tmp/mcp-config.json \
        -p "${AGENT_PROMPT}"
fi
