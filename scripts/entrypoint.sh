#!/bin/bash
set -e

# ~/.claude.json lives OUTSIDE ~/.claude/ but we only mount ~/.claude/.
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

if [ -z "${ANTHROPIC_API_KEY:-}" ] && [ ! -f ~/.claude/.credentials.json ]; then
    echo "[entrypoint] No credentials found. Run './run-agent.sh login' first." >&2
    exit 1
fi

# Pre-accept workspace trust and onboarding so Claude Code doesn't prompt
if [ -f ~/.claude.json ]; then
    node -e "
      const fs = require('fs');
      const f = process.env.HOME + '/.claude.json';
      const d = JSON.parse(fs.readFileSync(f, 'utf8'));
      if (!d.projects) d.projects = {};
      d.projects['/workspace'] = d.projects['/workspace'] || {};
      d.projects['/workspace'].hasTrustDialogAccepted = true;
      d.hasCompletedOnboarding = true;
      fs.writeFileSync(f, JSON.stringify(d, null, 2));
    " 2>/dev/null || true
fi

echo "[entrypoint] Agent: ${AGENT_NAME} (${AGENT_ROLE}, ${AGENT_MODE:-oneshot})" >&2

# Privilege drop helper
setup_node_user() {
    if [ "$(id -u)" = "0" ]; then
        mkdir -p /home/node/.claude
        cp -a ~/.claude/. /home/node/.claude/
        [ -f ~/.claude.json ] && cp ~/.claude.json /home/node/.claude.json 2>/dev/null || true
        ln -sf /home/node/.claude/.claude.json /home/node/.claude.json 2>/dev/null || true
        chown -R node:node /home/node/.claude /home/node/.claude.json 2>/dev/null || true
    fi
}

run_as_node() {
    if [ "$(id -u)" = "0" ]; then
        su -s /bin/bash node -c "HOME=/home/node $*"
    else
        eval "$@"
    fi
}

setup_node_user

# MCP config
cat > /tmp/mcp-config.json <<MCPEOF
{
  "mcpServers": {
    "agent-bridge": {
      "command": "node",
      "args": ["/opt/bridge/dist/index.js"],
      "env": {
        "ORCHESTRATOR_URL": "${ORCHESTRATOR_URL}",
        "AGENT_NAME": "${AGENT_NAME}",
        "AGENT_ROLE": "${AGENT_ROLE}",
        "AGENT_MODE": "oneshot"
      }
    }
  }
}
MCPEOF
[ "$(id -u)" = "0" ] && chown node:node /tmp/mcp-config.json

CLAUDE_ARGS="--dangerously-skip-permissions --mcp-config /tmp/mcp-config.json"

if [ "${AGENT_MODE:-oneshot}" = "oneshot" ]; then
    run_as_node "claude ${CLAUDE_ARGS} -p '${AGENT_PROMPT}'"
    exit 0
fi

# === Long-running mode ===

# Start persistent bridge (WS to orchestrator + task queue)
ORCHESTRATOR_URL="${ORCHESTRATOR_URL}" \
AGENT_NAME="${AGENT_NAME}" \
AGENT_ROLE="${AGENT_ROLE}" \
AGENT_MODE="long-running" \
node /opt/bridge/dist/index.js &
BRIDGE_PID=$!

for i in $(seq 1 15); do
    curl -sf http://127.0.0.1:9801/next-task -o /dev/null --max-time 1 2>/dev/null && break
    sleep 1
done
echo "[entrypoint] Bridge ready (PID: ${BRIDGE_PID})" >&2
trap "kill ${BRIDGE_PID} 2>/dev/null" EXIT

# Run Claude Code interactively -- this IS the main process.
# The host-side run-agent.sh handles auto-accepting dialogs via tmux send-keys.
echo "[entrypoint] Starting Claude Code (interactive)..." >&2
run_as_node "exec claude ${CLAUDE_ARGS}"
