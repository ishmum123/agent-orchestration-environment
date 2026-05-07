#!/bin/bash
#
# Integration tests for orc
#
# Requires: tmux, git, claude (or mock), cargo
# Run: ./tests/integration.sh
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ORC_BIN="$PROJECT_DIR/target/debug/orc"
TMUX_SOCK="/tmp/orc-integration-test-$$"
SESSION="orc-itest"
TEST_REPO=""
PASSED=0
FAILED=0

cleanup() {
    # Kill test tmux server
    tmux -S "$TMUX_SOCK" kill-server 2>/dev/null || true
    rm -f "$TMUX_SOCK"
    # Kill any orc session we created
    tmux kill-session -t "$SESSION" 2>/dev/null || true
    # Remove test repo
    if [ -n "$TEST_REPO" ] && [ -d "$TEST_REPO" ]; then
        rm -rf "$TEST_REPO"
    fi
    # Clean orc state
    rm -rf ~/.orc/orc ~/.orc/agents
}
trap cleanup EXIT

pass() {
    echo "  PASS: $1"
    PASSED=$((PASSED + 1))
}

fail() {
    echo "  FAIL: $1"
    FAILED=$((FAILED + 1))
}

assert_contains() {
    local haystack="$1"
    local needle="$2"
    local msg="$3"
    if echo "$haystack" | grep -q "$needle"; then
        pass "$msg"
    else
        fail "$msg (expected to find '$needle')"
    fi
}

assert_not_contains() {
    local haystack="$1"
    local needle="$2"
    local msg="$3"
    if echo "$haystack" | grep -q "$needle"; then
        fail "$msg (found '$needle' but shouldn't have)"
    else
        pass "$msg"
    fi
}

assert_file_exists() {
    if [ -f "$1" ]; then
        pass "$2"
    else
        fail "$2 (file not found: $1)"
    fi
}

# --- Setup ---

echo "Building orc..."
(cd "$PROJECT_DIR" && cargo build 2>/dev/null)

if [ ! -f "$ORC_BIN" ]; then
    echo "ERROR: binary not found at $ORC_BIN"
    exit 1
fi

# Create a temp git repo for testing
TEST_REPO="$(mktemp -d)"
git -C "$TEST_REPO" init -b main >/dev/null 2>&1
git -C "$TEST_REPO" commit --allow-empty -m "init" >/dev/null 2>&1

echo ""
echo "=== orc integration tests ==="
echo ""

# --- Test 1: Launcher script generation ---

echo "[launcher script generation]"

# Run orc briefly to generate files, then kill
# We need a PTY, so use script + tmux
tmux -S "$TMUX_SOCK" new-session -d -s runner -x 120 -y 40 2>/dev/null || {
    # Retry with script for PTY
    script -q /dev/null tmux -S "$TMUX_SOCK" new-session -d -s runner -x 120 -y 40 2>/dev/null
}
tmux -S "$TMUX_SOCK" send-keys -t runner "$ORC_BIN -p $TEST_REPO -s $SESSION" Enter
sleep 5

assert_file_exists "$HOME/.orc/orc/instructions.md" "instructions.md created"
assert_file_exists "$HOME/.orc/orc/launch.sh" "launch.sh created"

# Check instructions content
INSTRUCTIONS=$(cat "$HOME/.orc/orc/instructions.md" 2>/dev/null || echo "")
assert_contains "$INSTRUCTIONS" "$SESSION" "instructions contain session name"
assert_contains "$INSTRUCTIONS" "tmux split-window" "instructions contain spawn commands"
assert_contains "$INSTRUCTIONS" "~/.orc/agents/NAME.json" "instructions contain metadata file path"
assert_contains "$INSTRUCTIONS" "capture-pane" "instructions contain monitor commands"
assert_contains "$INSTRUCTIONS" "~/.orc/locked" "instructions contain lock file rule"

# Check launcher content
LAUNCHER=$(cat "$HOME/.orc/orc/launch.sh" 2>/dev/null || echo "")
assert_contains "$LAUNCHER" "#!/bin/sh" "launcher has shebang"
assert_contains "$LAUNCHER" "cd '$TEST_REPO'" "launcher cd's to project dir"
assert_contains "$LAUNCHER" "append-system-prompt-file" "launcher uses append-system-prompt-file"
assert_contains "$LAUNCHER" "exec claude" "launcher exec's claude"

# Check launcher is executable
if [ -x "$HOME/.orc/orc/launch.sh" ]; then
    pass "launch.sh is executable"
else
    fail "launch.sh is not executable"
fi

echo ""

# --- Test 2: TUI starts clean (no shell noise) ---

echo "[clean startup]"

# Capture the TUI pane output
TUI_OUTPUT=$(tmux -S "$TMUX_SOCK" capture-pane -t runner -p -J -S -40 2>/dev/null || echo "")

assert_not_contains "$TUI_OUTPUT" "default interactive shell" "no zsh switch message"
assert_not_contains "$TUI_OUTPUT" "bash-3.2" "no bash prompt"
assert_not_contains "$TUI_OUTPUT" "exec.*launch.sh" "no launch.sh command visible"
assert_not_contains "$TUI_OUTPUT" "Read.*CLAUDE.md" "no bootstrapping prompt"
assert_contains "$TUI_OUTPUT" "orc" "header shows orc"

echo ""

# --- Test 3: Agent metadata discovery ---

echo "[agent metadata discovery]"

# Write a fake agent metadata file
mkdir -p ~/.orc/agents
cat > ~/.orc/agents/test-agent.json << 'EOF'
{
  "name": "test-agent",
  "task": "test task for integration",
  "session": "orc-itest",
  "window": 0,
  "pane": 99,
  "worktree": "/tmp/fake-worktree"
}
EOF

# Wait for next poll cycle to pick it up
sleep 4

TUI_OUTPUT2=$(tmux -S "$TMUX_SOCK" capture-pane -t runner -p -J -S -40 2>/dev/null || echo "")
assert_contains "$TUI_OUTPUT2" "test-agent" "discovered agent appears in TUI"

# Clean up fake agent
rm -f ~/.orc/agents/test-agent.json

echo ""

# --- Test 4: lazygit config ---

echo "[lazygit config]"

assert_file_exists "$HOME/.orc/lazygit.yml" "lazygit config created"
LG_CONFIG=$(cat "$HOME/.orc/lazygit.yml" 2>/dev/null || echo "")
assert_contains "$LG_CONFIG" "showBottomLine: false" "lazygit config has expected content"

echo ""

# --- Test 5: Cleanup on exit ---

echo "[cleanup]"

# Send quit command to orc
tmux -S "$TMUX_SOCK" send-keys -t runner "q" 2>/dev/null
sleep 2

# Check that orc session was killed
if tmux has-session -t "$SESSION" 2>/dev/null; then
    fail "orc tmux session still exists after quit"
else
    pass "orc tmux session cleaned up"
fi

echo ""

# --- Summary ---

TOTAL=$((PASSED + FAILED))
echo "=== $PASSED/$TOTAL passed ==="
if [ "$FAILED" -gt 0 ]; then
    echo "$FAILED test(s) failed"
    exit 1
fi
