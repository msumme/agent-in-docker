#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ORCHESTRATOR_PORT="${ORCHESTRATOR_PORT:-9800}"
ORCHESTRATOR_PID_FILE="/tmp/agent-in-docker-orchestrator.pid"
ORCHESTRATOR_BIN="${SCRIPT_DIR}/orchestrator/target/debug/orchestrator"
IMAGE_NAME="agent-in-docker"
NETWORK_NAME="agent-net"

# Defaults
ROLE="code-agent"
AGENT_NAME="agent-$(date +%s)"

usage() {
    echo "Usage: $0 <project-path> \"<prompt>\" [options]"
    echo ""
    echo "Options:"
    echo "  --role <role>       Agent role (default: code-agent)"
    echo "  --name <name>       Agent name (default: agent-<timestamp>)"
    echo "  --no-tui            Start orchestrator in background without TUI"
    echo "  --build             Force rebuild of container image"
    echo ""
    echo "Examples:"
    echo "  $0 ./my-project \"Fix the failing tests\""
    echo "  $0 ./my-project \"Review this code\" --role review-agent --name reviewer"
    exit 1
}

# Parse arguments
if [ $# -lt 2 ]; then
    usage
fi

PROJECT_PATH="$(cd "$1" && pwd)"
PROMPT="$2"
shift 2

FORCE_BUILD=false
NO_TUI=false

while [ $# -gt 0 ]; do
    case "$1" in
        --role) ROLE="$2"; shift 2 ;;
        --name) AGENT_NAME="$2"; shift 2 ;;
        --build) FORCE_BUILD=true; shift ;;
        --no-tui) NO_TUI=true; shift ;;
        *) echo "Unknown option: $1"; usage ;;
    esac
done

echo "==> Project: ${PROJECT_PATH}"
echo "==> Prompt: ${PROMPT}"
echo "==> Agent: ${AGENT_NAME} (role: ${ROLE})"

# Check for ANTHROPIC_API_KEY
if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
    echo "Error: ANTHROPIC_API_KEY environment variable is not set"
    exit 1
fi

# Step 1: Build orchestrator if needed
if [ ! -f "${ORCHESTRATOR_BIN}" ]; then
    echo "==> Building orchestrator..."
    (cd "${SCRIPT_DIR}/orchestrator" && cargo build 2>&1)
fi

# Step 2: Build container image if needed
if [ "${FORCE_BUILD}" = true ] || ! podman image exists "${IMAGE_NAME}" 2>/dev/null; then
    echo "==> Building container image..."
    podman build -f "${SCRIPT_DIR}/Containerfile" -t "${IMAGE_NAME}" "${SCRIPT_DIR}"
fi

# Step 3: Create network if needed
podman network create "${NETWORK_NAME}" 2>/dev/null || true

# Step 4: Start orchestrator if not running
orchestrator_running() {
    if [ -f "${ORCHESTRATOR_PID_FILE}" ]; then
        local pid
        pid=$(cat "${ORCHESTRATOR_PID_FILE}")
        kill -0 "${pid}" 2>/dev/null && return 0
    fi
    return 1
}

if ! orchestrator_running; then
    echo "==> Starting orchestrator on port ${ORCHESTRATOR_PORT}..."
    if [ "${NO_TUI}" = true ]; then
        # Run in background without TUI (for testing)
        "${ORCHESTRATOR_BIN}" "0.0.0.0:${ORCHESTRATOR_PORT}" &
        ORCH_PID=$!
        echo "${ORCH_PID}" > "${ORCHESTRATOR_PID_FILE}"
        sleep 1
        echo "==> Orchestrator started (PID: ${ORCH_PID})"
    else
        # The TUI needs the terminal. Launch it in a tmux session if available,
        # otherwise tell the user to start it separately.
        if command -v tmux &>/dev/null; then
            tmux new-session -d -s orchestrator "${ORCHESTRATOR_BIN} 0.0.0.0:${ORCHESTRATOR_PORT}"
            # Get the PID of the orchestrator process inside tmux
            sleep 1
            ORCH_PID=$(tmux list-panes -t orchestrator -F '#{pane_pid}' 2>/dev/null | head -1)
            echo "${ORCH_PID}" > "${ORCHESTRATOR_PID_FILE}"
            echo "==> Orchestrator TUI started in tmux session 'orchestrator'"
            echo "    Attach with: tmux attach -t orchestrator"
        else
            echo "==> No tmux found. Starting orchestrator in foreground."
            echo "    The agent container will run in the background."
            echo "    Press Ctrl-C to quit the orchestrator when done."
            echo ""

            # Launch container in background first
            CONTAINER_ID=$(podman run -d --rm \
                --name "${AGENT_NAME}" \
                --network "${NETWORK_NAME}" \
                -v "${PROJECT_PATH}:/workspace:Z" \
                -e "ORCHESTRATOR_URL=ws://host.containers.internal:${ORCHESTRATOR_PORT}" \
                -e "AGENT_NAME=${AGENT_NAME}" \
                -e "AGENT_ROLE=${ROLE}" \
                -e "AGENT_PROMPT=${PROMPT}" \
                -e "ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}" \
                "${IMAGE_NAME}")
            echo "==> Container started: ${CONTAINER_ID:0:12}"
            echo "    Logs: podman logs -f ${AGENT_NAME}"

            # Run orchestrator TUI in foreground
            exec "${ORCHESTRATOR_BIN}" "0.0.0.0:${ORCHESTRATOR_PORT}"
        fi
    fi
else
    echo "==> Orchestrator already running"
fi

# Step 5: Launch container
echo "==> Launching agent container..."
podman run --rm \
    --name "${AGENT_NAME}" \
    --network "${NETWORK_NAME}" \
    -v "${PROJECT_PATH}:/workspace:Z" \
    -e "ORCHESTRATOR_URL=ws://host.containers.internal:${ORCHESTRATOR_PORT}" \
    -e "AGENT_NAME=${AGENT_NAME}" \
    -e "AGENT_ROLE=${ROLE}" \
    -e "AGENT_PROMPT=${PROMPT}" \
    -e "ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}" \
    "${IMAGE_NAME}"
