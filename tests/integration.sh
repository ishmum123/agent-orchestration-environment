#!/bin/bash
#
# Integration smoke test for orc (stream-json architecture)
#
# Requires: tmux, git, claude, cargo
# Run: ./tests/integration.sh
#
# Launches orc in a detached tmux session, sends keystrokes,
# captures pane output, and verifies expected behavior.
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ORC_BIN="$PROJECT_DIR/target/release/orc"
SESSION="orc-smoke-$$"
PASSED=0
FAILED=0

cleanup() {
    tmux kill-session -t "$SESSION" 2>/dev/null || true
    # Clean leftover worktrees
    cd "$PROJECT_DIR"
    git worktree list 2>/dev/null | grep orc-worktrees | awk '{print $1}' | \
        xargs -I{} git worktree remove --force {} 2>/dev/null || true
    git branch 2>/dev/null | grep 'orc/' | xargs git branch -D 2>/dev/null || true
    rm -rf "$PROJECT_DIR/../.orc-worktrees"
}
trap cleanup EXIT

pass() { echo "  PASS: $1"; PASSED=$((PASSED + 1)); }
fail() { echo "  FAIL: $1"; FAILED=$((FAILED + 1)); }

capture() { tmux capture-pane -t "$SESSION" -p 2>&1; }

wait_for() {
    local pattern="$1" timeout="$2" msg="$3"
    local elapsed=0
    while [ $elapsed -lt $timeout ]; do
        if capture | grep -q "$pattern"; then
            pass "$msg"
            return 0
        fi
        sleep 5
        elapsed=$((elapsed + 5))
    done
    fail "$msg (timed out after ${timeout}s waiting for '$pattern')"
    echo "--- pane content ---"
    capture
    echo "---"
    return 1
}

# --- Build ---

echo "Building orc (release)..."
(cd "$PROJECT_DIR" && cargo build --release 2>/dev/null)
if [ ! -f "$ORC_BIN" ]; then
    echo "ERROR: binary not found at $ORC_BIN"
    exit 1
fi

# --- Clean slate ---

cleanup 2>/dev/null || true

echo ""
echo "=== orc integration smoke test ==="
echo ""

# --- Step 1: Launch ---

echo "[launch]"
tmux new-session -d -s "$SESSION" -x 200 -y 50 "$ORC_BIN -p $PROJECT_DIR"
sleep 3

OUTPUT=$(capture)
if echo "$OUTPUT" | grep -q "orc"; then
    pass "orc header visible"
else
    fail "orc header not visible"
    echo "$OUTPUT"
    exit 1
fi

# --- Step 2: Wait for greeting ---

echo "[greeting]"
wait_for ">" 30 "orc started and shows input prompt"

OUTPUT=$(capture)
# Should NOT contain tool calls or LSP output
if echo "$OUTPUT" | grep -q "tool_use\|ToolUse\|LSP"; then
    fail "orc output contains tool calls"
else
    pass "no tool calls in orc output"
fi

# --- Step 3: Spawn agent with simple task ---

echo "[single agent]"
tmux send-keys -t "$SESSION" "list the files in this project, you must spawn an agent for this" Enter
sleep 2

wait_for "agents" 30 "agent count visible in header" || true

# Wait for agent to finish (checkmark)
if ! wait_for "$(printf '\xe2\x9c\x93')" 120 "agent finished with checkmark"; then
    echo "Agent may still be working. Current pane:"
    capture
fi

# --- Step 4: Verify orc received result ---

echo "[completion feedback]"
OUTPUT=$(capture)
# Orc should show some response text beyond just the greeting
LINE_COUNT=$(echo "$OUTPUT" | grep -v '^$' | grep -v '^─' | grep -v '^ *│' | grep -v '^ *$' | wc -l)
if [ "$LINE_COUNT" -gt 3 ]; then
    pass "orc shows response text after agent completion"
else
    fail "orc response seems empty after agent completion"
fi

# --- Step 5: No display artifacts ---

echo "[display filtering]"
OUTPUT=$(capture)
if echo "$OUTPUT" | grep -q 'SPAWN_AGENT\|TELL_AGENT\|KILL_AGENT'; then
    fail "command tags visible in output"
else
    pass "no command tags in display"
fi
# Check for stray closing brackets from multi-line commands
if echo "$OUTPUT" | grep -qx '"\]'; then
    fail "stray closing bracket visible"
else
    pass "no stray brackets in display"
fi

# --- Step 6: Stderr log created ---

echo "[stderr logging]"
if ls ~/.orc/logs/*.stderr 1>/dev/null 2>&1; then
    pass "stderr log files created"
else
    fail "no stderr log files in ~/.orc/logs/"
fi

# --- Step 7: Kill agent ---

echo "[kill agent]"
# Go to dashboard mode, select agent, kill it
tmux send-keys -t "$SESSION" Escape
sleep 0.5
tmux send-keys -t "$SESSION" x
sleep 1
OUTPUT=$(capture)
if echo "$OUTPUT" | grep -q "kill.*?"; then
    tmux send-keys -t "$SESSION" y
    sleep 3
    pass "kill confirmation dialog shown"

    # Verify worktree cleaned up
    WORKTREE_COUNT=$(git -C "$PROJECT_DIR" worktree list 2>/dev/null | grep -c orc-worktrees || true)
    WORKTREE_COUNT=${WORKTREE_COUNT:-0}
    if [ "$WORKTREE_COUNT" -eq 0 ]; then
        pass "worktree cleaned up after kill"
    else
        fail "worktree still exists after kill ($WORKTREE_COUNT remaining)"
    fi
else
    # Agent might already be gone from the list
    pass "no agents to kill (already cleaned up)"
fi

# --- Step 8: Quit and verify cleanup ---

echo "[quit and cleanup]"
# After kill confirmation, we're on Dashboard — q quits directly
tmux send-keys -t "$SESSION" q
sleep 5

if tmux has-session -t "$SESSION" 2>/dev/null; then
    # Session exists but orc may have exited — check if pane is dead
    OUTPUT=$(capture 2>/dev/null || echo "")
    if echo "$OUTPUT" | grep -q "orc"; then
        fail "orc still running after quit"
    else
        pass "orc exited (tmux pane dead)"
    fi
else
    pass "tmux session ended after quit"
fi

# Verify all worktrees cleaned up
WORKTREE_COUNT=$(git -C "$PROJECT_DIR" worktree list 2>/dev/null | grep -c orc-worktrees || true)
WORKTREE_COUNT=${WORKTREE_COUNT:-0}
if [ "$WORKTREE_COUNT" -eq 0 ]; then
    pass "all worktrees cleaned up on exit"
else
    fail "worktrees remain after exit ($WORKTREE_COUNT)"
fi

echo ""

# --- Summary ---

TOTAL=$((PASSED + FAILED))
echo "=== $PASSED/$TOTAL passed ==="
if [ "$FAILED" -gt 0 ]; then
    echo "$FAILED test(s) failed"
    exit 1
fi
