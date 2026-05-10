// End-to-end tests: full worker process lifecycle using fake_claude.
//
// Covers the complete pipeline:
//   MCP spawn_session → spawn_worker (fake_claude child) → WorkerEvents
//   → instruct_session (multi-turn) → mark_done → Done state
//
// ORC_CLAUDE_BIN is pointed at fake_claude so tests are deterministic and free.

use orc::db::Database;
use orc::mcp::{JsonRpcRequest, McpServer};
use orc::policy::PolicyEngine;
use orc::session::SessionState;
use orc::state::StateManager;
use orc::worker::WorkerEvent;
use orc::worker_registry::WorkerRegistry;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn unique_suffix() -> String {
    uuid::Uuid::new_v4().to_string()[..8].to_string()
}

/// Create a temp git repo suitable for use as orc's project_dir.
/// The caller receives the path; the tempdir is leaked (cleaned up by OS).
fn setup_git_repo() -> std::path::PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();

    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&path)
            .output()
            .unwrap()
    };

    run(&["init", "-b", "main"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);
    run(&["commit", "--allow-empty", "-m", "init"]);

    std::mem::forget(dir);
    path
}

/// Unwrap the JSON text payload from a tools/call MCP response.
fn extract_json(result: &Value) -> Value {
    let text = result["content"][0]["text"]
        .as_str()
        .expect("expected text content in MCP response");
    serde_json::from_str(text).expect("expected valid JSON in MCP tool response text")
}

// ---------------------------------------------------------------------------
// E2E tests
// ---------------------------------------------------------------------------

/// Full pipeline: spawn_session creates a real worker process (fake_claude),
/// events flow through the WorkerEvent channel, instruct_session drives a
/// second turn, and mark_done transitions the session to Done.
#[tokio::test]
async fn e2e_worker_lifecycle_with_fake_claude() {
    // Route worker spawning through fake_claude instead of real claude.
    let fake_bin = env!("CARGO_BIN_EXE_fake_claude");
    // Safety: single-threaded tokio test; no other thread reads ORC_CLAUDE_BIN.
    unsafe { std::env::set_var("ORC_CLAUDE_BIN", fake_bin) };

    let project_dir = setup_git_repo();

    // --- state ---
    let db_dir = tempfile::tempdir().unwrap();
    let db = Database::open(db_dir.path().join("e2e.db")).unwrap();
    std::mem::forget(db_dir);
    let policy = PolicyEngine::default_policy();
    let (state_handle, manager) = StateManager::new(db, policy);
    tokio::spawn(manager.run());

    // Keep the receiver alive so events are buffered and observable.
    let (worker_tx, mut worker_rx) = mpsc::unbounded_channel::<WorkerEvent>();

    // --- MCP HTTP server ---
    // fake_claude ignores --mcp-config, but spawn_session writes a config file
    // that requires a valid port number.
    let (listener, mcp_port) = orc::mcp::bind_http_listener().await.unwrap();
    let hook_socket = project_dir.join("hooks.sock");
    let (orc_inject_tx, _orc_inject_rx) = mpsc::channel::<String>(8);
    let server = Arc::new(McpServer::new(
        state_handle.clone(),
        project_dir,
        hook_socket,
        mcp_port,
        WorkerRegistry::new(),
        worker_tx,
        orc_inject_tx,
    ));
    orc::mcp::serve_http(listener, server.clone());

    // ── 1. spawn_session ────────────────────────────────────────────────────

    let worker_name = format!("e2e-{}", unique_suffix());
    let spawn_resp = server
        .handle_request(&JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(1.into())),
            method: "tools/call".into(),
            params: serde_json::json!({
                "name": "spawn_session",
                "arguments": {
                    "name": &worker_name,
                    "task": "hello from e2e",
                    "model": "sonnet"
                }
            }),
        })
        .await
        .unwrap();
    assert!(
        spawn_resp.error.is_none(),
        "spawn_session returned error: {:?}",
        spawn_resp.error
    );
    let spawn_data = extract_json(&spawn_resp.result.unwrap());
    assert_eq!(spawn_data["status"], "created");
    let session_id = spawn_data["session_id"].as_str().unwrap().to_string();

    // ── 2. collect turn-1 events ─────────────────────────────────────────────
    //
    // fake_claude emits on startup:
    //   system → ClaudeSessionId
    // Then for the first stdin message (the task):
    //   assistant (text + usage) → Text + Usage
    //   result                   → Result

    let mut got_claude_session_id = false;
    let mut got_text_turn1 = false;
    let mut got_result_turn1 = false;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let ev = tokio::time::timeout_at(deadline, worker_rx.recv())
            .await
            .expect("timed out waiting for turn-1 worker events")
            .expect("worker_rx channel closed unexpectedly");

        match &ev {
            WorkerEvent::ClaudeSessionId {
                claude_session_id, ..
            } => {
                assert!(
                    !claude_session_id.is_empty(),
                    "ClaudeSessionId should be non-empty"
                );
                got_claude_session_id = true;
            }
            WorkerEvent::Text { text, .. } if !got_text_turn1 => {
                assert!(
                    text.contains("[fake-claude turn 1]"),
                    "unexpected turn-1 text: {text}"
                );
                got_text_turn1 = true;
            }
            WorkerEvent::Result { is_error, .. } if !got_result_turn1 => {
                assert!(!is_error, "turn-1 result should succeed");
                got_result_turn1 = true;
            }
            _ => {}
        }

        if got_claude_session_id && got_text_turn1 && got_result_turn1 {
            break;
        }
    }

    // ── 3. session must be Running ───────────────────────────────────────────

    let session = state_handle.get_session(&session_id).await.unwrap();
    assert!(
        matches!(session.state, SessionState::Running),
        "expected Running after spawn, got: {:?}",
        session.state
    );

    // ── 4. instruct_session: multi-turn ─────────────────────────────────────

    let instruct_resp = server
        .handle_request(&JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(2.into())),
            method: "tools/call".into(),
            params: serde_json::json!({
                "name": "instruct_session",
                "arguments": {
                    "session_id": &session_id,
                    "message": "follow-up instruction"
                }
            }),
        })
        .await
        .unwrap();
    assert!(
        instruct_resp.error.is_none(),
        "instruct_session returned error: {:?}",
        instruct_resp.error
    );

    // Wait for the turn-2 Text event that proves the message reached fake_claude.
    let deadline2 = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut got_text_turn2 = false;
    loop {
        let ev = tokio::time::timeout_at(deadline2, worker_rx.recv())
            .await
            .expect("timed out waiting for turn-2 worker text event")
            .expect("worker_rx channel closed");

        if let WorkerEvent::Text { text, .. } = &ev {
            if text.contains("[fake-claude turn 2]") {
                got_text_turn2 = true;
                break;
            }
        }
    }
    assert!(got_text_turn2, "never received turn-2 text event from worker");

    // ── 5. mark_done ────────────────────────────────────────────────────────

    let done_resp = server
        .handle_request(&JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(3.into())),
            method: "tools/call".into(),
            params: serde_json::json!({
                "name": "mark_done",
                "arguments": {
                    "session_id": &session_id,
                    "summary": "e2e pipeline verified"
                }
            }),
        })
        .await
        .unwrap();
    assert!(done_resp.error.is_none());
    let done_data = extract_json(&done_resp.result.unwrap());
    assert_eq!(done_data["ok"], true);

    // ── 6. verify Done state ─────────────────────────────────────────────────

    let session_after = state_handle.get_session(&session_id).await.unwrap();
    assert!(
        matches!(session_after.state, SessionState::Done { .. }),
        "expected Done after mark_done, got: {:?}",
        session_after.state
    );
}
