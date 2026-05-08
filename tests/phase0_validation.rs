// Phase 0 validation: integration tests proving MCP, hooks, tmux, and full pipeline
// work with real processes and real I/O.

use orc::db::Database;
use orc::hooks::{create_hook_script, HookEvent, HookKind, HookServer};
use orc::mcp::{JsonRpcRequest, McpServer};
use orc::policy::PolicyEngine;
use orc::session::SessionState;
use orc::state::{StateCommand, StateHandle, StateManager};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create an in-memory state manager and return the handle.
/// Spawns the manager run loop as a background task.
fn setup_state() -> StateHandle {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("test.db")).unwrap();
    let policy = PolicyEngine::default_policy();
    let (handle, manager) = StateManager::new(db, policy);
    tokio::spawn(manager.run());
    // Leak tempdir so it outlives the test
    std::mem::forget(dir);
    handle
}

/// Build an MCP server backed by a fresh state manager.
fn setup_mcp() -> McpServer {
    McpServer::new(setup_state())
}

/// Send a JSON-RPC request and return the response, asserting no JSON-RPC error.
async fn call_ok(server: &McpServer, req: &JsonRpcRequest) -> Value {
    let resp = server.handle_request(req).await.unwrap();
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
    resp.result.unwrap()
}

/// Extract the text content from a tools/call response and parse as JSON.
fn extract_tool_text(result: &Value) -> Value {
    let text = result["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap()
}

// ===========================================================================
// Test 1: MCP stdio round-trip
// ===========================================================================

#[tokio::test]
async fn mcp_initialize_returns_protocol_version() {
    let server = setup_mcp();
    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(Value::Number(1.into())),
        method: "initialize".into(),
        params: serde_json::json!({"protocolVersion": "2024-11-05"}),
    };
    let result = call_ok(&server, &req).await;
    assert_eq!(result["protocolVersion"], "2024-11-05");
    assert_eq!(result["serverInfo"]["name"], "orc");
    assert_eq!(result["serverInfo"]["version"], "0.2.0");
    assert!(result["capabilities"]["tools"].is_object());
}

#[tokio::test]
async fn mcp_tools_list_returns_all_seven_tools() {
    let server = setup_mcp();
    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(Value::Number(1.into())),
        method: "tools/list".into(),
        params: Value::Object(Default::default()),
    };
    let result = call_ok(&server, &req).await;
    let tools = result["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 7, "expected 7 tools, got {}", tools.len());

    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    let expected = [
        "spawn_session",
        "instruct_session",
        "kill_session",
        "ask_user",
        "list_sessions",
        "mark_done",
        "update_task_graph",
    ];
    for name in &expected {
        assert!(names.contains(name), "missing tool: {}", name);
    }

    // Every tool has a valid inputSchema with type: object
    for tool in tools {
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert!(!tool["description"].as_str().unwrap().is_empty());
    }
}

#[tokio::test]
async fn mcp_spawn_list_markdone_lifecycle() {
    let server = setup_mcp();

    // Step 1: spawn a session
    let spawn_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(Value::Number(1.into())),
        method: "tools/call".into(),
        params: serde_json::json!({
            "name": "spawn_session",
            "arguments": { "name": "lifecycle-test", "task": "prove MCP works", "model": "sonnet" }
        }),
    };
    let spawn_result = call_ok(&server, &spawn_req).await;
    let spawn_data = extract_tool_text(&spawn_result);
    assert_eq!(spawn_data["status"], "created");
    assert!(spawn_data["session_id"].is_string());
    let session_id = spawn_data["session_id"].as_str().unwrap().to_string();

    // Step 2: list sessions — should include the spawned one
    let list_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(Value::Number(2.into())),
        method: "tools/call".into(),
        params: serde_json::json!({ "name": "list_sessions", "arguments": {} }),
    };
    let list_result = call_ok(&server, &list_req).await;
    let list_data = extract_tool_text(&list_result);
    let sessions = list_data["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["name"], "lifecycle-test");
    assert_eq!(sessions[0]["state"], "Running");

    // Step 3: mark done — state transition
    let done_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(Value::Number(3.into())),
        method: "tools/call".into(),
        params: serde_json::json!({
            "name": "mark_done",
            "arguments": { "session_id": &session_id, "summary": "MCP lifecycle verified" }
        }),
    };
    let done_result = call_ok(&server, &done_req).await;
    let done_data = extract_tool_text(&done_result);
    assert_eq!(done_data["ok"], true);

    // Step 4: verify state changed to Done
    let list_result2 = call_ok(&server, &list_req).await;
    let list_data2 = extract_tool_text(&list_result2);
    let sessions2 = list_data2["sessions"].as_array().unwrap();
    assert_eq!(sessions2.len(), 1);
    // State should show Done (the Debug format includes the variant)
    let state_str = sessions2[0]["state"].as_str().unwrap();
    assert!(
        state_str.contains("Done"),
        "expected Done state, got: {}",
        state_str
    );
}

#[tokio::test]
async fn mcp_json_serialization_roundtrip() {
    // Prove request/response survive serialize -> deserialize -> serialize
    let req_json = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 42,
        "method": "tools/call",
        "params": {
            "name": "spawn_session",
            "arguments": { "name": "roundtrip", "task": "test serialization" }
        }
    });

    // Serialize to string, deserialize back
    let serialized = serde_json::to_string(&req_json).unwrap();
    let deserialized: JsonRpcRequest = serde_json::from_str(&serialized).unwrap();
    assert_eq!(deserialized.method, "tools/call");
    assert_eq!(deserialized.jsonrpc, "2.0");

    // Handle it and verify response also round-trips
    let server = setup_mcp();
    let resp = server.handle_request(&deserialized).await.unwrap();
    let resp_json = serde_json::to_string(&resp).unwrap();
    let reparsed: Value = serde_json::from_str(&resp_json).unwrap();
    assert_eq!(reparsed["jsonrpc"], "2.0");
    assert!(reparsed["result"].is_object());
}

// ===========================================================================
// Test 2: Hook socket integration
// ===========================================================================

#[tokio::test]
async fn hook_server_receives_events_from_tokio_client() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("hooks.sock");
    let (tx, mut rx) = mpsc::channel(32);

    let server = HookServer::bind(&sock_path, tx).await.unwrap();
    tokio::spawn(server.run());
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Send multiple event types over separate connections
    let events = vec![
        serde_json::json!({
            "session_id": "s1", "hook": "PreToolUse",
            "tool_name": "Write", "input": {"file": "test.rs"},
            "timestamp": "2025-01-01T00:00:00Z"
        }),
        serde_json::json!({
            "session_id": "s2", "hook": "PostToolUse",
            "tool_name": "Read", "input": {}, "output": {"content": "hello"},
            "timestamp": "2025-01-01T00:00:01Z"
        }),
        serde_json::json!({
            "session_id": "s3", "hook": "Stop",
            "reason": "task complete", "result": "success",
            "timestamp": "2025-01-01T00:00:02Z"
        }),
    ];

    for event_json in &events {
        use tokio::io::AsyncWriteExt;
        let mut stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let payload = serde_json::to_string(event_json).unwrap();
        stream.write_all(payload.as_bytes()).await.unwrap();
        stream.write_all(b"\n").await.unwrap();
        stream.shutdown().await.unwrap();
    }

    // Receive and verify all three
    let e1 = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(e1.session_id, "s1");
    assert!(matches!(e1.kind, HookKind::PreToolUse { .. }));

    let e2 = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(e2.session_id, "s2");
    assert!(matches!(e2.kind, HookKind::PostToolUse { .. }));

    let e3 = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(e3.session_id, "s3");
    match e3.kind {
        HookKind::Stop { reason, result } => {
            assert_eq!(reason.as_deref(), Some("task complete"));
            assert_eq!(result.as_deref(), Some("success"));
        }
        _ => panic!("expected Stop"),
    }
}

#[tokio::test]
async fn hook_relay_script_sends_to_socket() {
    // This test verifies that create_hook_script() generates a working Python script
    // that can send events to our socket from an external process.
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("relay.sock");
    let (tx, mut rx) = mpsc::channel(32);

    let server = HookServer::bind(&sock_path, tx).await.unwrap();
    tokio::spawn(server.run());
    tokio::time::sleep(Duration::from_millis(50)).await;

    let script_path = create_hook_script(&sock_path, "relay-test-session")
        .await
        .unwrap();

    // Pipe a hook payload into the relay script via a child process.
    // The relay script injects session_id but expects hook type in the input.
    let hook_input = serde_json::json!({
        "hook": "PreToolUse",
        "tool_name": "Bash",
        "input": {"command": "echo hello"},
        "timestamp": "2025-06-01T00:00:00Z"
    });

    let mut child = tokio::process::Command::new("python3")
        .arg(&script_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("python3 must be available");

    use tokio::io::AsyncWriteExt;
    let mut stdin = child.stdin.take().unwrap();
    let payload = serde_json::to_string(&hook_input).unwrap();
    stdin.write_all(payload.as_bytes()).await.unwrap();
    drop(stdin); // close stdin so python3 reads EOF

    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("python3 relay timed out")
        .expect("failed to wait on child");
    assert!(status.success(), "relay script exited with: {}", status);

    // Verify the server received the event with session_id injected by the script
    let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.session_id, "relay-test-session");

    // Clean up
    let _ = tokio::fs::remove_file(&script_path).await;
}

#[tokio::test]
async fn hook_subprocess_nc_or_bash() {
    // Validates that a plain bash subprocess can send NDJSON to our socket.
    // Uses python3 since it's more portable than nc -U.
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("subprocess.sock");
    let (tx, mut rx) = mpsc::channel(32);

    let server = HookServer::bind(&sock_path, tx).await.unwrap();
    tokio::spawn(server.run());
    tokio::time::sleep(Duration::from_millis(50)).await;

    let sock_str = sock_path.to_string_lossy().to_string();
    let payload = r#"{"session_id":"bash-test","hook":"Notification","message":"from subprocess","timestamp":"2025-01-01T00:00:00Z"}"#;

    // Use python3 one-liner to connect and send
    let script = format!(
        r#"import socket; s=socket.socket(socket.AF_UNIX); s.connect("{}"); s.sendall(b'{}\n'); s.close()"#,
        sock_str, payload
    );

    let output = tokio::process::Command::new("python3")
        .args(["-c", &script])
        .output()
        .await
        .expect("python3 must be available");
    assert!(
        output.status.success(),
        "python3 failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.session_id, "bash-test");
    match event.kind {
        HookKind::Notification { message } => {
            assert_eq!(message.as_deref(), Some("from subprocess"));
        }
        _ => panic!("expected Notification, got {:?}", event.kind),
    }
}

// ===========================================================================
// Test 3: tmux session lifecycle (requires tmux, run with --ignored)
// ===========================================================================

#[tokio::test]
#[ignore]
async fn tmux_full_lifecycle() {
    let session_name = "orc-phase0-test";
    let dir = std::env::temp_dir();

    // 1. Create session
    assert!(
        !orc::tmux::has_session(session_name).await,
        "session should not exist before test"
    );
    orc::tmux::create_session(session_name, &dir)
        .await
        .expect("create_session failed");
    assert!(
        orc::tmux::has_session(session_name).await,
        "session should exist after create"
    );

    // 2. Send a command and capture output
    orc::tmux::send_keys(session_name, &["echo 'hello from orc'", "Enter"])
        .await
        .expect("send_keys failed");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let pane_content = orc::tmux::capture_pane(session_name)
        .await
        .expect("capture_pane failed");
    assert!(
        pane_content.contains("hello from orc"),
        "pane did not contain expected output. got: {}",
        pane_content
    );

    // 3. Test pipe-pane logging
    let logfile = dir.join("orc-phase0-pipe.log");
    orc::tmux::start_pipe_pane(session_name, &logfile)
        .await
        .expect("start_pipe_pane failed");
    orc::tmux::send_keys(session_name, &["echo 'pipe-test-marker'", "Enter"])
        .await
        .expect("send_keys for pipe test failed");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let log_lines = orc::tmux::read_pipe_log(&logfile, 20)
        .await
        .expect("read_pipe_log failed");
    assert!(
        log_lines.iter().any(|l| l.contains("pipe-test-marker")),
        "pipe log did not contain marker. got: {:?}",
        log_lines
    );

    // 4. Kill session
    orc::tmux::kill_session(session_name)
        .await
        .expect("kill_session failed");
    assert!(
        !orc::tmux::has_session(session_name).await,
        "session should be gone after kill"
    );

    // Cleanup
    let _ = tokio::fs::remove_file(&logfile).await;
}

// ===========================================================================
// Test 4: Full pipeline integration
// ===========================================================================

#[tokio::test]
async fn full_pipeline_mcp_hooks_state() {
    // 1-3: Create Database, PolicyEngine, StateManager
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open(dir.path().join("pipeline.db")).unwrap();
    let policy = PolicyEngine::default_policy();
    let (handle, manager) = StateManager::new(db, policy);
    tokio::spawn(manager.run());
    std::mem::forget(dir);

    // 4: Start a HookServer
    let hook_dir = tempfile::tempdir().unwrap();
    let sock_path = hook_dir.path().join("pipeline.sock");
    let _hook_handle = handle.clone();
    let (hook_tx, mut hook_rx) = mpsc::channel::<HookEvent>(64);
    let hook_server = HookServer::bind(&sock_path, hook_tx).await.unwrap();
    tokio::spawn(hook_server.run());

    // Forward hook events into state manager
    let forward_handle = handle.clone();
    tokio::spawn(async move {
        while let Some(event) = hook_rx.recv().await {
            let _ = forward_handle
                .send(StateCommand::HandleHook {
                    event: event.clone(),
                })
                .await;
        }
    });

    // 5: Create MCP server with the same StateHandle
    let mcp = McpServer::new(handle.clone());

    // 6a: MCP call to spawn_session
    let spawn_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(Value::Number(1.into())),
        method: "tools/call".into(),
        params: serde_json::json!({
            "name": "spawn_session",
            "arguments": { "name": "pipeline-worker", "task": "end-to-end test" }
        }),
    };
    let spawn_result = call_ok(&mcp, &spawn_req).await;
    let spawn_data = extract_tool_text(&spawn_result);
    let session_id = spawn_data["session_id"].as_str().unwrap().to_string();

    // 6b: Verify session in state
    let session = handle.get_session(&session_id).await.unwrap();
    assert_eq!(session.name, "pipeline-worker");
    assert!(matches!(session.state, SessionState::Running));

    // 6c: Simulate hook event (Stop) by sending to the socket
    {
        use tokio::io::AsyncWriteExt;
        let mut stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        let stop_event = serde_json::json!({
            "session_id": &session_id,
            "hook": "Stop",
            "reason": "task finished",
            "timestamp": "2025-01-01T00:00:00Z"
        });
        let payload = serde_json::to_string(&stop_event).unwrap();
        stream.write_all(payload.as_bytes()).await.unwrap();
        stream.write_all(b"\n").await.unwrap();
        stream.shutdown().await.unwrap();
    }

    // Wait for event to propagate through hook -> state manager
    tokio::time::sleep(Duration::from_millis(200)).await;

    // 6d: Verify session state transitioned to Done
    let session_after = handle.get_session(&session_id).await.unwrap();
    assert!(
        matches!(session_after.state, SessionState::Done { .. }),
        "expected Done, got: {:?}",
        session_after.state
    );

    // 6e: MCP list_sessions should reflect the Done state
    let list_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(Value::Number(2.into())),
        method: "tools/call".into(),
        params: serde_json::json!({ "name": "list_sessions", "arguments": {} }),
    };
    let list_result = call_ok(&mcp, &list_req).await;
    let list_data = extract_tool_text(&list_result);
    let sessions = list_data["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    let state_str = sessions[0]["state"].as_str().unwrap();
    assert!(
        state_str.contains("Done"),
        "list_sessions state should be Done, got: {}",
        state_str
    );
}

#[tokio::test]
async fn full_pipeline_ask_user_flow() {
    // 7: Test ask_user: spawn a task that calls ask_user, answer from another task
    let handle = setup_state();
    let mcp = McpServer::new(handle.clone());

    // Create a session (so ask_user has something to associate with)
    let spawn_req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(Value::Number(1.into())),
        method: "tools/call".into(),
        params: serde_json::json!({
            "name": "spawn_session",
            "arguments": { "name": "asker", "task": "ask questions" }
        }),
    };
    call_ok(&mcp, &spawn_req).await;

    // Subscribe to state changes to detect the question
    let mut sub = handle.subscribe();

    // Spawn ask_user in a background task (it blocks)
    let ask_handle = handle.clone();
    let ask_task = tokio::spawn(async move {
        ask_handle
            .ask_user("orc", "What branch should we target?", Some("for the PR".into()))
            .await
            .unwrap()
    });

    // Wait for the UserQuestionPending broadcast
    let mut question_received = false;
    for _ in 0..20 {
        match tokio::time::timeout(Duration::from_millis(100), sub.recv()).await {
            Ok(Ok(orc::state::StateChange::UserQuestionPending { question, .. })) => {
                assert!(question.contains("branch"));
                question_received = true;
                break;
            }
            _ => continue,
        }
    }
    assert!(question_received, "never received UserQuestionPending");

    // Answer the question
    handle.answer_user("orc", "main").await.unwrap();

    // Verify the response flows through
    let answer = tokio::time::timeout(Duration::from_secs(2), ask_task)
        .await
        .expect("ask_user timed out")
        .expect("ask task panicked");
    assert_eq!(answer, "main");
}
