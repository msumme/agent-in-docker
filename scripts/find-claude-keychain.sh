#!/bin/bash
# Diagnostic script: finds Claude Code OAuth credentials in macOS Keychain.
# Run this manually to discover the keychain entry name and format.
# It does NOT modify anything.

set -euo pipefail

echo "=== Step 1: Search for Claude-related keychain entries ==="
echo "(This will list entry metadata, NOT the actual secrets)"
echo ""
security dump-keychain 2>/dev/null | grep -i -B5 -A5 "claude\|anthropic" || echo "No entries found matching 'claude' or 'anthropic'"

echo ""
echo "=== Step 2: If you found an entry above, try extracting it ==="
echo "Replace SERVICE_NAME below with the 'svce' value from the output above."
echo ""
echo "  security find-generic-password -s 'SERVICE_NAME' -w"
echo ""
echo "That will print the password/token value to stdout."
echo "If it's JSON, it's likely the full credentials blob."
echo ""
echo "=== Step 3: Check what format Claude Code expects on Linux ==="
echo "On Linux, Claude Code reads: ~/.claude/.credentials.json"
echo "The keychain value is likely the exact contents of that file."
