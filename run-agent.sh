#!/bin/bash
# Thin wrapper that delegates to the Rust CLI binary.
# Usage: ./run-agent.sh <project-path> "<prompt>" [options]
#        ./run-agent.sh login

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
AGENT_BIN="${SCRIPT_DIR}/orchestrator/target/debug/agent"

if [ ! -f "${AGENT_BIN}" ]; then
    echo "==> Building CLI..."
    (cd "${SCRIPT_DIR}/orchestrator" && cargo build 2>&1)
fi

if [ "${1:-}" = "login" ]; then
    exec "${AGENT_BIN}" login
else
    exec "${AGENT_BIN}" run "$@"
fi
