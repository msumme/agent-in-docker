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
AGENT_NAME=""
NAMED_AGENT=false
ANTHROPIC_API_KEY="${ANTHROPIC_API_KEY:-}"
CLAUDE_CREDENTIALS="${CLAUDE_CREDENTIALS:-}"
SEED_DIR="${SCRIPT_DIR}/.claude-container"
AGENTS_DIR="${SCRIPT_DIR}/.claude-agents"

usage() {
    echo "Usage: $0 <project-path> \"<prompt>\" [options]"
    echo "       $0 login"
    echo ""
    echo "Commands:"
    echo "  login               Authenticate Claude Code (opens browser)"
    echo ""
    echo "Options:"
    echo "  --role <role>       Agent role (default: code-agent)"
    echo "  --name <name>       Agent name (default: agent-<timestamp>)"
    echo "  --no-tui            Start orchestrator in background without TUI"
    echo "  --build             Force rebuild of container image"
    echo ""
    echo "Examples:"
    echo "  $0 login"
    echo "  $0 ./my-project \"Fix the failing tests\""
    echo "  $0 ./my-project \"Review this code\" --role review-agent --name reviewer"
    exit 1
}

# Handle 'login' subcommand
if [ "${1:-}" = "login" ]; then
    echo "==> Starting Claude Code login flow..."

    # Ensure container image exists
    if ! podman image exists "${IMAGE_NAME}" 2>/dev/null; then
        echo "==> Building container image first..."
        podman build -f "${SCRIPT_DIR}/Containerfile" -t "${IMAGE_NAME}" "${SCRIPT_DIR}"
    fi

    mkdir -p "${SEED_DIR}"

    # Restore .claude.json from backup if it exists
    BACKUP=$(ls -t "${SEED_DIR}"/backups/.claude.json.backup.* 2>/dev/null | head -1)
    if [ -n "${BACKUP}" ] && [ ! -f "${SEED_DIR}/.claude.json" ]; then
        cp "${BACKUP}" "${SEED_DIR}/.claude.json"
        echo "==> Restored .claude.json from backup"
    fi

    # Run claude login in a container with a TTY and script to capture output
    LOGIN_LOG=$(mktemp)

    # Use 'script' to capture PTY output while keeping TTY for claude login
    podman run -t --rm \
        --entrypoint bash \
        -v "${SEED_DIR}:/root/.claude:Z" \
        "${IMAGE_NAME}" \
        -c '
            # Symlink .claude.json if present in mount
            if [ -f ~/.claude/.claude.json ] && [ ! -f ~/.claude.json ]; then
                ln -s ~/.claude/.claude.json ~/.claude.json
            fi
            claude login
        ' 2>&1 | tee "${LOGIN_LOG}" &
    LOGIN_PID=$!

    # Watch for the OAuth URL to appear, then open browser
    echo "==> Waiting for OAuth URL..."
    for i in $(seq 1 30); do
        sleep 1
        if grep -q "claude.com" "${LOGIN_LOG}" 2>/dev/null; then
            sleep 1  # Wait for full URL to be written
            # Extract URL: join all lines, find the https://claude.com URL
            URL=$(cat "${LOGIN_LOG}" | tr -d '\n\r' | sed 's/.*\(https:\/\/claude\.com[^ ]*\).*/\1/' | tr -d ' ')
            if [ -n "${URL}" ] && echo "${URL}" | grep -q "^https://claude.com"; then
                echo ""
                echo "==> Opening browser for authentication..."
                if command -v open &>/dev/null; then
                    open "${URL}"
                elif command -v xdg-open &>/dev/null; then
                    xdg-open "${URL}"
                else
                    echo "==> Open this URL in your browser:"
                    echo "${URL}"
                fi
                echo "==> Complete the login in your browser..."
            fi
            break
        fi
    done

    # Wait for login to complete
    wait "${LOGIN_PID}" 2>/dev/null
    LOGIN_EXIT=$?
    rm -f "${LOGIN_LOG}"

    if [ -f "${SEED_DIR}/.credentials.json" ]; then
        echo "==> Login successful! Credentials saved to ${SEED_DIR}/"
    else
        echo "==> Login may have failed. Check ${SEED_DIR}/ for credentials."
    fi
    exit 0
fi

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
        --name) AGENT_NAME="$2"; NAMED_AGENT=true; shift 2 ;;
        --build) FORCE_BUILD=true; shift ;;
        --no-tui) NO_TUI=true; shift ;;
        *) echo "Unknown option: $1"; usage ;;
    esac
done

# Assign ephemeral name if none given
if [ -z "${AGENT_NAME}" ]; then
    AGENT_NAME="agent-$(date +%s)"
fi

echo "==> Project: ${PROJECT_PATH}"
echo "==> Prompt: ${PROMPT}"
echo "==> Agent: ${AGENT_NAME} (role: ${ROLE}, $([ "${NAMED_AGENT}" = true ] && echo "persistent" || echo "ephemeral"))"

# Set up per-agent claude config directory.
# .claude-container/ is the seed (shared credentials from 'claude login').
# Named agents get persistent dirs under .claude-agents/<name>/.
# Ephemeral agents get fresh dirs that are cleaned up on exit.
AGENT_CLAUDE_DIR=""
CLEANUP_AGENT_DIR=false

mkdir -p "${AGENTS_DIR}"

if [ ! -f "${SEED_DIR}/.credentials.json" ]; then
    echo "Error: No credentials found in ${SEED_DIR}/"
    echo "Run: podman run -it --rm -v \"${SEED_DIR}:/root/.claude:Z\" agent-in-docker bash"
    echo "Then: claude login"
    exit 1
fi

if [ "${NAMED_AGENT}" = true ]; then
    AGENT_CLAUDE_DIR="${AGENTS_DIR}/${AGENT_NAME}"
    if [ ! -d "${AGENT_CLAUDE_DIR}" ]; then
        echo "==> Creating persistent config for agent '${AGENT_NAME}'"
        mkdir -p "${AGENT_CLAUDE_DIR}"
        # Copy seed config (not credentials -- those are symlinked)
        for f in "${SEED_DIR}"/*; do
            [ -e "$f" ] && [ "$(basename "$f")" != ".credentials.json" ] && cp -a "$f" "${AGENT_CLAUDE_DIR}/"
        done
        for f in "${SEED_DIR}"/.*; do
            case "$(basename "$f")" in
                .|..) continue ;;
                .credentials.json) continue ;;
                *) cp -a "$f" "${AGENT_CLAUDE_DIR}/" ;;
            esac
        done
    fi
else
    AGENT_CLAUDE_DIR=$(mktemp -d "${AGENTS_DIR}/ephemeral-${AGENT_NAME}-XXXXXX")
    CLEANUP_AGENT_DIR=true
    # Copy seed config
    for f in "${SEED_DIR}"/*; do
        [ -e "$f" ] && [ "$(basename "$f")" != ".credentials.json" ] && cp -a "$f" "${AGENT_CLAUDE_DIR}/"
    done
    for f in "${SEED_DIR}"/.*; do
        case "$(basename "$f")" in
            .|..) continue ;;
            .credentials.json) continue ;;
            *) cp -a "$f" "${AGENT_CLAUDE_DIR}/" ;;
        esac
    done
fi

# Always symlink shared credentials into agent dir
ln -sf "${SEED_DIR}/.credentials.json" "${AGENT_CLAUDE_DIR}/.credentials.json"

cleanup_agent_dir() {
    if [ "${CLEANUP_AGENT_DIR}" = true ] && [ -n "${AGENT_CLAUDE_DIR}" ]; then
        rm -rf "${AGENT_CLAUDE_DIR}"
    fi
}
trap cleanup_agent_dir EXIT

# Resolve credentials: ANTHROPIC_API_KEY > Keychain OAuth > error
if [ -n "${ANTHROPIC_API_KEY}" ]; then
    echo "==> Using ANTHROPIC_API_KEY from environment"
elif [ -z "${CLAUDE_CREDENTIALS}" ] && command -v security &>/dev/null; then
    echo "==> Extracting Claude Code OAuth credentials from macOS Keychain..."
    CLAUDE_CREDENTIALS=$(security find-generic-password -s "Claude Code-credentials" -w 2>/dev/null) || {
        echo "Error: No ANTHROPIC_API_KEY and could not read 'Claude Code-credentials' from Keychain."
        echo "Set ANTHROPIC_API_KEY or run 'claude login' first."
        exit 1
    }
    echo "==> Got OAuth credentials from Keychain"
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
                -v "${AGENT_CLAUDE_DIR}:/root/.claude:Z" \
                ${ANTHROPIC_API_KEY:+-e "ANTHROPIC_API_KEY=$ANTHROPIC_API_KEY"} \
                ${CLAUDE_CREDENTIALS:+-e "CLAUDE_CREDENTIALS=$CLAUDE_CREDENTIALS"} \
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
    -v "${SCRIPT_DIR}/.claude-container:/root/.claude:Z" \
    -e "ORCHESTRATOR_URL=ws://host.containers.internal:${ORCHESTRATOR_PORT}" \
    -e "AGENT_NAME=${AGENT_NAME}" \
    -e "AGENT_ROLE=${ROLE}" \
    -e "AGENT_PROMPT=${PROMPT}" \
    ${ANTHROPIC_API_KEY:+-e "ANTHROPIC_API_KEY=$ANTHROPIC_API_KEY"} \
    ${CLAUDE_CREDENTIALS:+-e "CLAUDE_CREDENTIALS=$CLAUDE_CREDENTIALS"} \
    "${IMAGE_NAME}"
