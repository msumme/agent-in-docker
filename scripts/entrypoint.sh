#!/bin/bash
set -e

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
    echo "[entrypoint] No credentials found. Run './run-agent.sh login' first." >&2
    exit 1
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
        "AGENT_ROLE": "${AGENT_ROLE}",
        "AGENT_MODE": "${AGENT_MODE:-oneshot}"
      }
    }
  }
}
EOF

echo "[entrypoint] Starting agent: ${AGENT_NAME} (role: ${AGENT_ROLE})" >&2
echo "[entrypoint] Orchestrator URL: ${ORCHESTRATOR_URL}" >&2
echo "[entrypoint] Mode: ${AGENT_MODE:-oneshot}" >&2

# --dangerously-skip-permissions cannot run as root.
run_claude() {
    local extra_args="$*"
    if [ "$(id -u)" = "0" ]; then
        mkdir -p /home/node/.claude
        cp -a ~/.claude/. /home/node/.claude/
        [ -f ~/.claude.json ] && cp ~/.claude.json /home/node/.claude.json 2>/dev/null || true
        ln -sf /home/node/.claude/.claude.json /home/node/.claude.json 2>/dev/null || true
        chown -R node:node /home/node/.claude /home/node/.claude.json 2>/dev/null || true
        chown node:node /tmp/mcp-config.json
        su -s /bin/bash node -c "HOME=/home/node claude \
            --dangerously-skip-permissions \
            --mcp-config /tmp/mcp-config.json \
            ${extra_args}"
    else
        claude \
            --dangerously-skip-permissions \
            --mcp-config /tmp/mcp-config.json \
            ${extra_args}
    fi
}

if [ "${AGENT_MODE:-oneshot}" = "oneshot" ]; then
    run_claude "-p '${AGENT_PROMPT}'"
else
    # Long-running mode: start a persistent bridge for the task queue,
    # then loop: run Claude Code for each task.

    # Start bridge as a standalone process for the task queue HTTP server
    ORCHESTRATOR_URL="${ORCHESTRATOR_URL}" \
    AGENT_NAME="${AGENT_NAME}" \
    AGENT_ROLE="${AGENT_ROLE}" \
    AGENT_MODE="long-running" \
    node /opt/bridge/dist/index.js &
    BRIDGE_PID=$!
    echo "[entrypoint] Started persistent bridge (PID: ${BRIDGE_PID})" >&2

    # Wait for bridge to be ready
    for i in $(seq 1 10); do
        if curl -sf http://127.0.0.1:9801/next-task -o /dev/null --max-time 1 2>/dev/null; then
            break
        fi
        sleep 1
    done

    trap "kill ${BRIDGE_PID} 2>/dev/null" EXIT

    RESUME_FLAG=""

    # Run initial prompt if provided
    if [ -n "${AGENT_PROMPT:-}" ]; then
        echo "[entrypoint] Running initial prompt..." >&2
        run_claude "-p '${AGENT_PROMPT}'"
        RESUME_FLAG="--resume"
    fi

    echo "[entrypoint] Waiting for tasks from orchestrator..." >&2

    while true; do
        # Long-poll the bridge's task queue
        TASK=$(curl -sf http://127.0.0.1:9801/next-task --max-time 35 2>/dev/null) || {
            sleep 1
            continue
        }

        if [ -z "${TASK}" ]; then
            continue
        fi

        echo "[entrypoint] Received task: ${TASK}" >&2
        run_claude "${RESUME_FLAG} -p '${TASK}'"
        RESUME_FLAG="--resume"
    done
fi
