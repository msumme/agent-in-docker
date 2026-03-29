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

CLAUDE_ARGS="--dangerously-skip-permissions --mcp-config /tmp/mcp-config.json"

# --dangerously-skip-permissions refuses to run as root.
# Create a user matching the workspace owner's uid so file permissions work.
if [ "$(id -u)" = "0" ]; then
    WORKSPACE_UID=$(stat -c '%u' /workspace 2>/dev/null || stat -f '%u' /workspace)
    WORKSPACE_GID=$(stat -c '%g' /workspace 2>/dev/null || stat -f '%g' /workspace)
    USERNAME="agent"

    # Create user with matching uid so bind-mounted files are accessible
    if ! id -u "${WORKSPACE_UID}" >/dev/null 2>&1; then
        adduser -D -u "${WORKSPACE_UID}" -h /home/agent -s /bin/sh "${USERNAME}" 2>/dev/null || \
            echo "${USERNAME}:x:${WORKSPACE_UID}:${WORKSPACE_GID}::/home/agent:/bin/sh" >> /etc/passwd
    else
        USERNAME=$(id -un "${WORKSPACE_UID}")
    fi

    # Copy claude config to the agent user's home
    mkdir -p /home/agent/.claude
    cp -r ~/.claude/. /home/agent/.claude/
    [ -f ~/.claude.json ] && cp ~/.claude.json /home/agent/.claude.json
    ln -sf /home/agent/.claude/.claude.json /home/agent/.claude.json 2>/dev/null || true
    cp /tmp/mcp-config.json /home/agent/mcp-config.json
    chown -R "${WORKSPACE_UID}:${WORKSPACE_GID}" /home/agent

    CLAUDE_ARGS="--dangerously-skip-permissions --mcp-config /home/agent/mcp-config.json"

    if [ "${AGENT_MODE:-oneshot}" = "oneshot" ]; then
        exec su -s /bin/bash "${USERNAME}" -c "HOME=/home/agent claude ${CLAUDE_ARGS} -p '${AGENT_PROMPT}'"
    else
        echo "[entrypoint] Starting Claude Code (interactive)..." >&2
        exec su -s /bin/bash "${USERNAME}" -c "HOME=/home/agent claude ${CLAUDE_ARGS}"
    fi
else
    if [ "${AGENT_MODE:-oneshot}" = "oneshot" ]; then
        exec claude ${CLAUDE_ARGS} -p "${AGENT_PROMPT}"
    else
        echo "[entrypoint] Starting Claude Code (interactive)..." >&2
        exec claude ${CLAUDE_ARGS}
    fi
fi
