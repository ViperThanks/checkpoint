#!/usr/bin/env bash
# smoke-mac-shell.sh — Basic smoke checks for the AgentAspect macOS app
#
# Checks:
#   1. swift build succeeds (debug)
#   2. Binary exists after build
#   3. Binary is executable
#   4. Resources/Binaries/ directory exists
#   5. agent-aspect binary is findable via PATH (optional)
#
# Usage: ./scripts/smoke-mac-shell.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
APP_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PASS=0
FAIL=0

pass() { ((PASS++)); echo "  PASS: $1"; }
fail() { ((FAIL++)); echo "  FAIL: $1"; }

echo "==> AgentAspect smoke checks"
echo ""

# 1. swift build
echo "[1/5] swift build (debug)..."
cd "$APP_DIR"
if swift build 2>&1; then
    pass "swift build succeeded"
else
    fail "swift build failed"
fi

# 2. Binary exists
echo ""
echo "[2/5] Binary exists..."
BINARY="$APP_DIR/.build/debug/AgentAspect"
if [[ -f "$BINARY" ]]; then
    pass "Binary found at $BINARY"
else
    fail "Binary not found at $BINARY"
fi

# 3. Binary is executable
echo ""
echo "[3/5] Binary is executable..."
if [[ -x "$BINARY" ]]; then
    pass "Binary is executable"
else
    fail "Binary is not executable"
fi

# 4. Resources/Binaries/ directory
echo ""
echo "[4/5] Resources/Binaries/ directory..."
BIN_DIR="$APP_DIR/Resources/Binaries"
if [[ -d "$BIN_DIR" ]]; then
    pass "Resources/Binaries/ exists"
else
    fail "Resources/Binaries/ not found"
fi

# 5. agent-aspect binary in PATH (optional)
echo ""
echo "[5/5] agent-aspect binary in PATH..."
if command -v agent-aspect &>/dev/null; then
    CP_PATH=$(command -v agent-aspect)
    pass "agent-aspect found at $CP_PATH"
else
    echo "  SKIP: agent-aspect not in PATH (ok for CI, needed for runtime)"
fi

echo ""
echo "==> Results: $PASS passed, $FAIL failed"
if [[ "$FAIL" -gt 0 ]]; then
    exit 1
fi
