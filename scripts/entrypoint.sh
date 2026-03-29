#!/bin/bash
set -e

# ~/.claude.json lives OUTSIDE ~/.claude/ but we only mount ~/.claude/.
if [ ! -f ~/.claude.json ] && [ -f ~/.claude/.claude.json ]; then
    ln -s ~/.claude/.claude.json ~/.claude.json
elif [ ! -f ~/.claude.json ]; then
    BACKUP=$(ls -t ~/.claude/backups/.claude.json.backup.* 2>/dev/null | head -1)
    if [ -n "${BACKUP}" ]; then
        cp "${BACKUP}" ~/.claude/.claude.json
        ln -s ~/.claude/.claude.json ~/.claude.json
    fi
fi

if [ -z "${ANTHROPIC_API_KEY:-}" ] && [ ! -f ~/.claude/.credentials.json ]; then
    echo "[entrypoint] No credentials found. Run './run-agent.sh login' first." >&2
    exit 1
fi

# Pre-accept workspace trust
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

# Privilege drop
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

# MCP config: connect to the orchestrator's built-in MCP server on the host
MCP_PORT="${MCP_PORT:-9801}"
BRIDGE_URL="http://host.containers.internal:${MCP_PORT}/mcp"

cat > /tmp/mcp-config.json <<MCPEOF
{
  "mcpServers": {
    "agent-bridge": {
      "type": "http",
      "url": "${BRIDGE_URL}"
    }
  }
}
MCPEOF
[ "$(id -u)" = "0" ] && chown node:node /tmp/mcp-config.json

CLAUDE_ARGS="--dangerously-skip-permissions --mcp-config /tmp/mcp-config.json"

if [ "${AGENT_MODE:-oneshot}" = "oneshot" ]; then
    run_as_node "claude ${CLAUDE_ARGS} -p '${AGENT_PROMPT}'"
else
    # Long-running: run Claude Code interactively.
    # The host-side run-agent.sh handles auto-accepting dialogs via tmux send-keys.
    echo "[entrypoint] Starting Claude Code (interactive)..." >&2
    run_as_node "exec claude ${CLAUDE_ARGS}"
fi
