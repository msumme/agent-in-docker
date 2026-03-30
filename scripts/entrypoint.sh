#!/bin/bash
set -euo pipefail

# Restore .claude.json (lives outside .claude/ dir but we only mount .claude/)
if [ ! -f ~/.claude.json ] && [ -f ~/.claude/.claude.json ]; then
    ln -s ~/.claude/.claude.json ~/.claude.json
elif [ ! -f ~/.claude.json ]; then
    BACKUP=$(ls -t ~/.claude/backups/.claude.json.backup.* 2>/dev/null | head -1)
    if [ -n "${BACKUP:-}" ]; then
        cp "${BACKUP}" ~/.claude/.claude.json
        ln -s ~/.claude/.claude.json ~/.claude.json
    fi
fi

# Verify credentials
if [ -z "${ANTHROPIC_API_KEY:-}" ] && [ ! -f ~/.claude/.credentials.json ]; then
    echo "[entrypoint] No credentials. Run './run-agent.sh login' first." >&2
    exit 1
fi

# Pre-accept workspace trust (pure JSON edit, no node)
if [ -f ~/.claude.json ]; then
    python3 -c "
import json, os
f = os.path.expanduser('~/.claude.json')
d = json.load(open(f))
d.setdefault('projects', {})['/workspace'] = d.get('projects', {}).get('/workspace', {})
d['projects']['/workspace']['hasTrustDialogAccepted'] = True
d['hasCompletedOnboarding'] = True
json.dump(d, open(f, 'w'))
" 2>/dev/null || true
fi

# Beads: connect to host dolt server if provided
if [ -n "${DOLT_HOST:-}" ] && [ -n "${DOLT_PORT:-}" ]; then
    export BEADS_DOLT_SERVER_HOST="${DOLT_HOST}"
    export BEADS_DOLT_SERVER_PORT="${DOLT_PORT}"
fi

# MCP config
MCP_PORT="${MCP_PORT:-9801}"
cat > /tmp/mcp-config.json <<MCPEOF
{
  "mcpServers": {
    "agent-bridge": {
      "type": "http",
      "url": "http://host.containers.internal:${MCP_PORT}/mcp"
    }
  }
}
MCPEOF

echo "[entrypoint] ${AGENT_NAME} (${AGENT_ROLE}, ${AGENT_MODE:-oneshot})" >&2

# IS_SANDBOX=1 allows --dangerously-skip-permissions as root.
# No uid matching, no privilege dropping, no user creation needed.
export IS_SANDBOX=1
CLAUDE_ARGS="--dangerously-skip-permissions --mcp-config /tmp/mcp-config.json"

if [ "${AGENT_MODE:-oneshot}" = "oneshot" ]; then
    # Write prompt to temp file to avoid shell injection
    PROMPT_FILE=$(mktemp)
    printf '%s' "${AGENT_PROMPT}" > "${PROMPT_FILE}"
    exec claude ${CLAUDE_ARGS} -p "$(cat "${PROMPT_FILE}")"
else
    echo "[entrypoint] Starting Claude Code (interactive)..." >&2
    exec claude ${CLAUDE_ARGS}
fi
