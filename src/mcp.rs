// MCP server: expose orchestrator tools to Claude agents via JSON-RPC 2.0.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use std::path::PathBuf;

use crate::session::SessionEvent;
use crate::state::{StateCommand, StateHandle};

// ---------------------------------------------------------------------------
// JSON-RPC types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>, // None for notifications
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

// ---------------------------------------------------------------------------
// McpServer
// ---------------------------------------------------------------------------

pub struct McpServer {
    state: StateHandle,
    project_dir: PathBuf,
    hook_socket_path: PathBuf,
}

impl McpServer {
    pub fn new(state: StateHandle, project_dir: PathBuf, hook_socket_path: PathBuf) -> Self {
        McpServer {
            state,
            project_dir,
            hook_socket_path,
        }
    }

    /// Tool definitions exposed to the MCP client.
    pub fn tool_definitions() -> Vec<ToolDef> {
        vec![
            ToolDef {
                name: "spawn_session".into(),
                description: "Spawn a new worker session with its own worktree".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Short name for the session" },
                        "task": { "type": "string", "description": "Task description for the worker" },
                        "model": { "type": "string", "description": "Model to use (sonnet/opus/haiku)", "default": "sonnet" }
                    },
                    "required": ["name", "task"]
                }),
            },
            ToolDef {
                name: "instruct_session".into(),
                description: "Send a message/instruction to a running worker session".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "Session ID to instruct" },
                        "message": { "type": "string", "description": "Message to send to the worker" }
                    },
                    "required": ["session_id", "message"]
                }),
            },
            ToolDef {
                name: "kill_session".into(),
                description: "Kill a worker session and clean up its resources".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "Session ID to kill" }
                    },
                    "required": ["session_id"]
                }),
            },
            ToolDef {
                name: "ask_user".into(),
                description: "Ask the user a question. BLOCKS until the user responds in the TUI."
                    .into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "question": { "type": "string", "description": "Question to ask the user" },
                        "context": { "type": "string", "description": "Additional context for the question" }
                    },
                    "required": ["question"]
                }),
            },
            ToolDef {
                name: "list_sessions".into(),
                description: "List all current worker sessions with their status".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
            },
            ToolDef {
                name: "mark_done".into(),
                description: "Mark a task/session as completed".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "Session to mark done" },
                        "summary": { "type": "string", "description": "Summary of what was accomplished" }
                    },
                    "required": ["session_id", "summary"]
                }),
            },
            ToolDef {
                name: "update_task_graph".into(),
                description: "Update the task plan/graph".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "graph": { "type": "object", "description": "Task graph as JSON" }
                    },
                    "required": ["graph"]
                }),
            },
        ]
    }

    // ── request dispatch ─────────────────────────────────────────────────────

    /// Handle a single JSON-RPC request. Returns None for notifications.
    pub async fn handle_request(&self, request: &JsonRpcRequest) -> Option<JsonRpcResponse> {
        match request.method.as_str() {
            "initialize" => Some(self.handle_initialize(request)),
            "notifications/initialized" => None,
            "tools/list" => Some(self.handle_tools_list(request)),
            "tools/call" => Some(self.handle_tools_call(request).await),
            _ => Some(JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id.clone().unwrap_or(Value::Null),
                result: None,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: "Method not found".into(),
                }),
            }),
        }
    }

    fn handle_initialize(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: req.id.clone().unwrap_or(Value::Null),
            result: Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "orc", "version": "0.2.0" }
            })),
            error: None,
        }
    }

    fn handle_tools_list(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        let tools = Self::tool_definitions();
        JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: req.id.clone().unwrap_or(Value::Null),
            result: Some(serde_json::json!({ "tools": tools })),
            error: None,
        }
    }

    async fn handle_tools_call(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        let tool_name = req
            .params
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let args = req
            .params
            .get("arguments")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));

        let result = match tool_name {
            "spawn_session" => self.tool_spawn_session(&args).await,
            "instruct_session" => self.tool_instruct_session(&args).await,
            "kill_session" => self.tool_kill_session(&args).await,
            "ask_user" => self.tool_ask_user(&args).await,
            "list_sessions" => self.tool_list_sessions(&args).await,
            "mark_done" => self.tool_mark_done(&args).await,
            "update_task_graph" => self.tool_update_task_graph(&args).await,
            _ => Err(anyhow::anyhow!("unknown tool: {}", tool_name)),
        };

        let id = req.id.clone().unwrap_or(Value::Null);
        match result {
            Ok(text) => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id,
                result: Some(serde_json::json!({
                    "content": [{ "type": "text", "text": text }]
                })),
                error: None,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id,
                result: Some(serde_json::json!({
                    "content": [{ "type": "text", "text": format!("Error: {}", e) }],
                    "isError": true
                })),
                error: None,
            },
        }
    }

    // ── tool implementations ─────────────────────────────────────────────────

    async fn tool_spawn_session(&self, args: &Value) -> Result<String> {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'name'"))?;
        let task = args
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'task'"))?;
        let model = args
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("sonnet");

        let project_dir = self.project_dir.to_str().unwrap_or(".");

        // 1. Create worktree
        let worktree_path = crate::worktree::create_worktree(project_dir, name).await?;
        let worktree_str = worktree_path.to_str().unwrap_or("").to_string();

        // 2. Get base commit
        let base_output = tokio::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(project_dir)
            .output()
            .await?;
        let base_commit = String::from_utf8_lossy(&base_output.stdout)
            .trim()
            .to_string();

        let branch = format!("orc/{}", name);
        let tmux_name = format!("orc-{}", name);

        // 3. Create tmux session (best-effort — may not be available in tests)
        let tmux_ok = crate::tmux::create_session(&tmux_name, &worktree_path)
            .await
            .is_ok();

        // 4. Create hook relay script
        let _hook_script = crate::hooks::create_hook_script(
            &self.hook_socket_path,
            &tmux_name,
        )
        .await?;

        if tmux_ok {
            // 5. Start Claude Code inside tmux with the task
            let claude_cmd = format!(
                "claude -p --model {} --permission-mode plan \"{}\"",
                model,
                task.replace('"', "\\\"")
            );
            let _ = crate::tmux::send_text(&tmux_name, &claude_cmd).await;
            let _ = crate::tmux::send_keys(&tmux_name, &["Enter"]).await;

            // 6. Start pipe-pane for log capture
            let log_path = std::env::temp_dir().join(format!("orc-{}.log", name));
            let _ = crate::tmux::start_pipe_pane(&tmux_name, &log_path).await;
        }

        // 7. Create state entry with real paths
        let session = self
            .state
            .create_session(name, task, &worktree_str, &branch, &base_commit, model)
            .await?;

        Ok(serde_json::to_string(&serde_json::json!({
            "session_id": session.id,
            "name": session.name,
            "worktree_path": worktree_str,
            "branch": branch,
            "status": "created"
        }))?)
    }

    async fn tool_instruct_session(&self, args: &Value) -> Result<String> {
        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'session_id'"))?;
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'message'"))?;

        let session = self
            .state
            .get_session(session_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("session not found: {}", session_id))?;

        crate::tmux::send_text(&session.tmux_session, message).await?;

        Ok(serde_json::to_string(&serde_json::json!({
            "delivered": true,
            "session_id": session_id
        }))?)
    }

    async fn tool_kill_session(&self, args: &Value) -> Result<String> {
        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'session_id'"))?;

        let session = self
            .state
            .get_session(session_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("session not found: {}", session_id))?;

        // Kill tmux session
        let _ = crate::tmux::kill_session(&session.tmux_session).await;

        // Remove worktree if it exists
        if !session.worktree_path.is_empty() {
            let _ = crate::worktree::remove_worktree(
                self.project_dir.to_str().unwrap_or("."),
                std::path::Path::new(&session.worktree_path),
                &session.name,
            )
            .await;
        }

        // Remove from state
        self.state
            .send(StateCommand::RemoveSession {
                session_id: session_id.to_string(),
            })
            .await?;

        Ok(serde_json::to_string(&serde_json::json!({
            "killed": true,
            "session_id": session_id
        }))?)
    }

    async fn tool_ask_user(&self, args: &Value) -> Result<String> {
        let question = args
            .get("question")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'question'"))?;
        let context = args
            .get("context")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let answer = self.state.ask_user("orc", question, context).await?;

        Ok(serde_json::to_string(&serde_json::json!({
            "response": answer
        }))?)
    }

    async fn tool_list_sessions(&self, _args: &Value) -> Result<String> {
        let sessions = self.state.list_sessions().await;
        let list: Vec<Value> = sessions
            .iter()
            .map(|s| {
                serde_json::json!({
                    "session_id": s.id,
                    "name": s.name,
                    "state": format!("{:?}", s.state),
                    "mode": format!("{:?}", s.mode),
                    "model": s.model,
                    "worktree_path": s.worktree_path,
                    "created_at": s.created_at.to_rfc3339(),
                })
            })
            .collect();

        Ok(serde_json::to_string(&serde_json::json!({ "sessions": list }))?)
    }

    async fn tool_mark_done(&self, args: &Value) -> Result<String> {
        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'session_id'"))?;
        let summary = args
            .get("summary")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'summary'"))?;

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.state
            .send(StateCommand::ApplyEvent {
                session_id: session_id.to_string(),
                event: SessionEvent::Finished {
                    summary: summary.to_string(),
                },
                reply: tx,
            })
            .await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("state manager dropped reply"))??;

        Ok(serde_json::to_string(&serde_json::json!({
            "ok": true,
            "session_id": session_id
        }))?)
    }

    async fn tool_update_task_graph(&self, args: &Value) -> Result<String> {
        let graph = args
            .get("graph")
            .ok_or_else(|| anyhow::anyhow!("missing 'graph'"))?;

        let run_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.state
            .send(StateCommand::UpdateTaskGraph {
                run_id: run_id.clone(),
                user_prompt: String::new(),
                graph_json: serde_json::to_string(graph)?,
                reply: tx,
            })
            .await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("state manager dropped reply"))??;

        Ok(serde_json::to_string(&serde_json::json!({
            "ok": true,
            "run_id": run_id
        }))?)
    }
}

// ---------------------------------------------------------------------------
// Stdio runner
// ---------------------------------------------------------------------------

/// Run the MCP server reading JSON-RPC from stdin, writing responses to stdout.
pub async fn run_stdio(server: McpServer) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("mcp: failed to parse request: {}", e);
                continue;
            }
        };

        if let Some(response) = server.handle_request(&request).await {
            let json = serde_json::to_string(&response)?;
            stdout.write_all(json.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP transport
// ---------------------------------------------------------------------------

/// Start the MCP server as an HTTP endpoint on localhost.
/// Returns the bound port. The server runs as a background tokio task.
/// Claude Code connects to `http://127.0.0.1:{port}/mcp` via HTTP transport.
pub async fn start_http_server(server: std::sync::Arc<McpServer>) -> Result<u16> {
    use axum::{extract::State, routing::post, Json, Router};

    async fn mcp_handler(
        State(server): State<std::sync::Arc<McpServer>>,
        Json(request): Json<JsonRpcRequest>,
    ) -> Json<Value> {
        match server.handle_request(&request).await {
            Some(resp) => Json(serde_json::to_value(resp).unwrap_or(Value::Null)),
            None => Json(serde_json::json!({})),
        }
    }

    let app = Router::new()
        .route("/mcp", post(mcp_handler))
        .with_state(server);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("[mcp-http] server error: {}", e);
        }
    });

    Ok(port)
}

// ---------------------------------------------------------------------------
// MCP config generation
// ---------------------------------------------------------------------------

/// Generate MCP config JSON for Claude Code's --mcp-config flag (HTTP transport).
/// The MCP server runs in-process on localhost, Claude Code connects to it.
pub fn generate_mcp_config(port: u16) -> Value {
    serde_json::json!({
        "mcpServers": {
            "orc": {
                "type": "http",
                "url": format!("http://127.0.0.1:{}/mcp", port)
            }
        }
    })
}

/// Generate MCP config for stdio transport (used for testing / fallback).
pub fn generate_mcp_config_stdio(orc_binary: &str) -> Value {
    serde_json::json!({
        "mcpServers": {
            "orc": {
                "command": orc_binary,
                "args": ["--mcp-server"],
                "env": {}
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_server() -> McpServer {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Database::open(dir.path().join("test.db")).unwrap();
        let policy = crate::policy::PolicyEngine::default_policy();
        let (handle, manager) = crate::state::StateManager::new(db, policy);
        tokio::spawn(manager.run());

        // Put git repo inside a "repo" subdir so worktrees go into the tempdir
        // (not /tmp/.orc-worktrees which collides across tests)
        let project_dir = dir.path().join("repo");
        std::fs::create_dir_all(&project_dir).unwrap();
        let hook_socket_path = dir.path().join("hooks.sock");

        // Initialize a git repo so worktree creation works in tests
        let _ = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&project_dir)
            .output();
        let _ = std::process::Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(&project_dir)
            .output();

        // Leak tempdir so it outlives the test
        std::mem::forget(dir);
        McpServer::new(handle, project_dir, hook_socket_path)
    }

    #[tokio::test]
    async fn initialize_handshake() {
        let server = test_server().await;
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(0.into())),
            method: "initialize".into(),
            params: serde_json::json!({"protocolVersion": "2024-11-05"}),
        };
        let resp = server.handle_request(&req).await.unwrap();
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert_eq!(result["serverInfo"]["name"], "orc");
    }

    #[tokio::test]
    async fn notification_returns_none() {
        let server = test_server().await;
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: None,
            method: "notifications/initialized".into(),
            params: Value::Object(Default::default()),
        };
        let resp = server.handle_request(&req).await;
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn tools_list_returns_all_tools() {
        let server = test_server().await;
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(1.into())),
            method: "tools/list".into(),
            params: Value::Object(Default::default()),
        };
        let resp = server.handle_request(&req).await.unwrap();
        assert!(resp.error.is_none());
        let tools = resp.result.unwrap()["tools"].as_array().unwrap().len();
        assert_eq!(tools, 7);
    }

    #[tokio::test]
    async fn spawn_session_tool() {
        let server = test_server().await;
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(2.into())),
            method: "tools/call".into(),
            params: serde_json::json!({
                "name": "spawn_session",
                "arguments": { "name": "test-worker", "task": "do stuff" }
            }),
        };
        let resp = server.handle_request(&req).await.unwrap();
        assert!(resp.error.is_none());
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert!(parsed["session_id"].is_string());
        assert_eq!(parsed["name"], "test-worker");
        assert_eq!(parsed["status"], "created");
    }

    #[tokio::test]
    async fn spawn_session_with_model() {
        let server = test_server().await;
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(1.into())),
            method: "tools/call".into(),
            params: serde_json::json!({
                "name": "spawn_session",
                "arguments": { "name": "opus-worker", "task": "hard task", "model": "opus" }
            }),
        };
        let resp = server.handle_request(&req).await.unwrap();
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["branch"], "orc/opus-worker");
    }

    #[tokio::test]
    async fn list_sessions_tool() {
        let server = test_server().await;

        // Spawn a session first
        let spawn_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(1.into())),
            method: "tools/call".into(),
            params: serde_json::json!({
                "name": "spawn_session",
                "arguments": { "name": "worker-1", "task": "task 1" }
            }),
        };
        server.handle_request(&spawn_req).await;

        let list_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(2.into())),
            method: "tools/call".into(),
            params: serde_json::json!({ "name": "list_sessions", "arguments": {} }),
        };
        let resp = server.handle_request(&list_req).await.unwrap();
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["sessions"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn mark_done_tool() {
        let server = test_server().await;

        // Spawn
        let spawn_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(1.into())),
            method: "tools/call".into(),
            params: serde_json::json!({
                "name": "spawn_session",
                "arguments": { "name": "finisher", "task": "finish it" }
            }),
        };
        let resp = server.handle_request(&spawn_req).await.unwrap();
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        let sid = parsed["session_id"].as_str().unwrap();

        // Mark done
        let done_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(2.into())),
            method: "tools/call".into(),
            params: serde_json::json!({
                "name": "mark_done",
                "arguments": { "session_id": sid, "summary": "all done" }
            }),
        };
        let resp = server.handle_request(&done_req).await.unwrap();
        assert!(resp.error.is_none());
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["ok"], true);
    }

    #[tokio::test]
    async fn update_task_graph_tool() {
        let server = test_server().await;
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(1.into())),
            method: "tools/call".into(),
            params: serde_json::json!({
                "name": "update_task_graph",
                "arguments": { "graph": { "nodes": [], "edges": [] } }
            }),
        };
        let resp = server.handle_request(&req).await.unwrap();
        assert!(resp.error.is_none());
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["ok"], true);
        assert!(parsed["run_id"].is_string());
    }

    #[tokio::test]
    async fn unknown_method_returns_error() {
        let server = test_server().await;
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(1.into())),
            method: "nonexistent".into(),
            params: Value::Null,
        };
        let resp = server.handle_request(&req).await.unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[tokio::test]
    async fn unknown_tool_returns_error_content() {
        let server = test_server().await;
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(1.into())),
            method: "tools/call".into(),
            params: serde_json::json!({ "name": "nonexistent", "arguments": {} }),
        };
        let resp = server.handle_request(&req).await.unwrap();
        assert!(resp.error.is_none()); // tool errors go in content, not JSON-RPC error
        let result = resp.result.unwrap();
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Error"));
    }

    #[tokio::test]
    async fn spawn_session_missing_name_returns_error() {
        let server = test_server().await;
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(1.into())),
            method: "tools/call".into(),
            params: serde_json::json!({
                "name": "spawn_session",
                "arguments": { "task": "do stuff" }
            }),
        };
        let resp = server.handle_request(&req).await.unwrap();
        let result = resp.result.unwrap();
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("missing 'name'"));
    }

    #[tokio::test]
    async fn kill_session_nonexistent_returns_error() {
        let server = test_server().await;
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(1.into())),
            method: "tools/call".into(),
            params: serde_json::json!({
                "name": "kill_session",
                "arguments": { "session_id": "does-not-exist" }
            }),
        };
        let resp = server.handle_request(&req).await.unwrap();
        let result = resp.result.unwrap();
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("session not found"));
    }

    #[test]
    fn generate_mcp_config_http() {
        let config = generate_mcp_config(9123);
        assert_eq!(config["mcpServers"]["orc"]["type"], "http");
        assert_eq!(
            config["mcpServers"]["orc"]["url"],
            "http://127.0.0.1:9123/mcp"
        );
    }

    #[test]
    fn generate_mcp_config_stdio_structure() {
        let config = generate_mcp_config_stdio("/usr/local/bin/orc");
        assert_eq!(config["mcpServers"]["orc"]["command"], "/usr/local/bin/orc");
        assert_eq!(config["mcpServers"]["orc"]["args"][0], "--mcp-server");
    }

    #[tokio::test]
    async fn http_server_handles_request() {
        let server = std::sync::Arc::new(test_server().await);
        let port = start_http_server(server).await.unwrap();

        // Give server a moment to start
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Make HTTP request to the MCP endpoint
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{}/mcp", port))
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {"protocolVersion": "2024-11-05"}
            }))
            .send()
            .await
            .unwrap();

        assert!(resp.status().is_success());
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(body["result"]["serverInfo"]["name"], "orc");
    }

    #[test]
    fn tool_definitions_have_required_fields() {
        let tools = McpServer::tool_definitions();
        for tool in &tools {
            assert!(!tool.name.is_empty());
            assert!(!tool.description.is_empty());
            assert_eq!(tool.input_schema["type"], "object");
        }
    }
}
