// MCP server: expose orchestrator tools to Claude agents via JSON-RPC 2.0.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use std::path::PathBuf;

use crate::session::SessionEvent;
use crate::state::{StateCommand, StateHandle};
use crate::worker::WorkerEvent;
use crate::worker_registry::WorkerRegistry;
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Self-MCP tool name filter
// ---------------------------------------------------------------------------

/// Names of MCP tools served by orc itself. Used to filter agent → orc
/// tool calls out of the user-facing event log (the user already sees
/// the effect of these tools elsewhere — modals, panel, state badges).
pub const ORC_MCP_TOOL_NAMES: &[&str] = &[
    "spawn_session",
    "instruct_session",
    "kill_session",
    "ask_user",
    "list_sessions",
    "mark_done",
    "submit_for_review",
    "answer_worker",
    "current_summary",
    "update_task_graph",
];

/// Returns true if `name` is an orc-served MCP tool. Matches both the
/// bare name and the `mcp__orc__<name>` form Claude reports.
pub fn is_orc_mcp_tool(name: &str) -> bool {
    let bare = name.strip_prefix("mcp__orc__").unwrap_or(name);
    ORC_MCP_TOOL_NAMES.iter().any(|n| *n == bare)
}

/// Names of harness-level meta tools the user shouldn't see in agent logs.
/// These come from the Claude harness itself, not orc's MCP server, so we
/// filter by name rather than via `is_orc_mcp_tool`.
pub const HARNESS_META_TOOLS: &[&str] = &[
    "ToolSearch",
    "ScheduleWakeup",
    "TaskCreate",
    "TaskUpdate",
    "TaskList",
    "TaskGet",
    "TaskOutput",
    "TaskStop",
    "TodoWrite",
    "ExitPlanMode",
    "EnterPlanMode",
    "EnterWorktree",
    "ExitWorktree",
    "Skill",
    "SendMessage",
    "AskUserQuestion",
    "PushNotification",
    "Monitor",
    "RemoteTrigger",
    "ListMcpResourcesTool",
    "ReadMcpResourceTool",
];

/// Returns true if `name` is a harness meta tool. Matches the bare name.
pub fn is_harness_meta_tool(name: &str) -> bool {
    HARNESS_META_TOOLS.iter().any(|n| *n == name)
}

/// Returns true if a tool call should be hidden from the user-facing log:
/// either an orc MCP call or a harness meta tool.
pub fn should_hide_tool(name: &str) -> bool {
    is_orc_mcp_tool(name) || is_harness_meta_tool(name)
}

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
    mcp_port: u16,
    workers: WorkerRegistry,
    worker_tx: mpsc::UnboundedSender<WorkerEvent>,
    orc_inject: Option<mpsc::Sender<String>>,
}

impl McpServer {
    pub fn new(
        state: StateHandle,
        project_dir: PathBuf,
        hook_socket_path: PathBuf,
        mcp_port: u16,
        workers: WorkerRegistry,
        worker_tx: mpsc::UnboundedSender<WorkerEvent>,
        orc_inject: mpsc::Sender<String>,
    ) -> Self {
        McpServer {
            state,
            project_dir,
            hook_socket_path,
            mcp_port,
            workers,
            worker_tx,
            orc_inject: Some(orc_inject),
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
                        "context": { "type": "string", "description": "Additional context for the question" },
                        "session_id": { "type": "string", "description": "Worker session_id (lets orc race to answer first); omit if you ARE orc" }
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
                name: "submit_for_review".into(),
                description: "Submit a worker's completed work for human review. Transitions the session to AwaitingReview so the user can inspect the diff and approve or reject. Use this instead of mark_done when changes need human verification.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "Session whose work is ready for review" },
                        "summary": { "type": "string", "description": "Summary of changes made (shown to user)" }
                    },
                    "required": ["session_id", "summary"]
                }),
            },
            ToolDef {
                name: "answer_worker".into(),
                description: "Answer a worker's pending ask_user question. First-responder wins (user OR orc). Pass the worker's session_id and your answer."
                    .into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "Worker session_id whose question you are answering" },
                        "answer": { "type": "string", "description": "Your answer to the worker's question" }
                    },
                    "required": ["session_id", "answer"]
                }),
            },
            ToolDef {
                name: "current_summary".into(),
                description: "Record a one-sentence summary of what you are currently working on. Pass session_id (omit if you ARE orc) and a short summary string. Call this periodically after meaningful progress, not after every line.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "summary": { "type": "string", "description": "One-sentence summary of current work" },
                        "session_id": { "type": "string", "description": "Worker session_id; omit if you ARE orc" }
                    },
                    "required": ["summary"]
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
            "submit_for_review" => self.tool_submit_for_review(&args).await,
            "answer_worker" => self.tool_answer_worker(&args).await,
            "current_summary" => self.tool_current_summary(&args).await,
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

        let session = self.spawn_session(name, task, model).await?;

        Ok(serde_json::to_string(&serde_json::json!({
            "session_id": session.id,
            "name": session.name,
            "worktree_path": session.worktree_path,
            "branch": session.branch,
            "status": "created"
        }))?)
    }

    /// Spawn a fully-equipped worker (worktree + MCP wiring + system
    /// prompt + child process). Same flow `tool_spawn_session` uses
    /// behind the JSON-RPC layer, exposed so user-initiated paths
    /// (e.g. the Ctrl+N attach promotion from the scratch overlay) can
    /// produce workers indistinguishable from orchestrator-spawned ones.
    pub(crate) async fn spawn_session(
        &self,
        name: &str,
        task: &str,
        model: &str,
    ) -> Result<crate::session::Session> {
        let project_dir = self.project_dir.to_str().unwrap_or(".");

        let worktree_path = crate::worktree::create_worktree(project_dir, name).await?;
        let worktree_str = worktree_path.to_str().unwrap_or("").to_string();

        let base_output = tokio::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&worktree_path)
            .output()
            .await?;
        let base_commit = String::from_utf8_lossy(&base_output.stdout)
            .trim()
            .to_string();

        let branch = format!("orc/{}", name);

        // hook relay script — keyed by session name (legacy tmux_session
        // string), kept so PreToolUse/PostToolUse hooks still wire back.
        let tmux_name = format!("orc-{}", name);
        let _hook_script =
            crate::hooks::create_hook_script(&self.hook_socket_path, &tmux_name).await?;

        // Create state entry first so we have the session_id for the worker prompt.
        let session = self
            .state
            .create_session(name, task, &worktree_str, &branch, &base_commit, model)
            .await?;

        // Spawn the stream-json worker child via the registry.
        let mcp_cfg = generate_mcp_config(self.mcp_port);
        let mcp_cfg_path = std::env::temp_dir().join(format!("orc-worker-mcp-{}.json", name));
        let _ = tokio::fs::write(
            &mcp_cfg_path,
            serde_json::to_string_pretty(&mcp_cfg).unwrap_or_default(),
        )
        .await;

        let worker_prompt =
            worker_system_prompt(&session.id, name, task, &worktree_str, &branch);
        match crate::worker::spawn_worker(
            session.id.clone(),
            worktree_path.clone(),
            model.to_string(),
            mcp_cfg_path,
            worker_prompt,
            task.to_string(),
            self.worker_tx.clone(),
        )
        .await
        {
            Ok(handle) => {
                self.workers.insert(handle).await;
            }
            Err(e) => {
                eprintln!("[mcp] failed to spawn worker {}: {}", name, e);
            }
        }

        Ok(session)
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

        let _session = self
            .state
            .get_session(session_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("session not found: {}", session_id))?;

        self.workers.send(session_id, message).await?;

        // Surface the instruction in the worker's UI log so the user can
        // see what orc just told this worker.
        let _ = self.worker_tx.send(WorkerEvent::OrcInstruction {
            session_id: session_id.to_string(),
            text: message.to_string(),
        });

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

        // Kill the worker stream-json child if registered.
        let _ = self.workers.kill(session_id).await;

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
        // The session_id of the asking worker, if provided. Falls back to "orc"
        // for back-compat — the orc brain can also call ask_user directly.
        let asker_session = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("orc")
            .to_string();

        // Fire-and-forget: inject a synthetic user message into orc so it can
        // race the user to answer via the `answer_worker` MCP tool.
        if asker_session != "orc" {
            if let Some(tx) = &self.orc_inject {
                let ctx_part = context
                    .as_deref()
                    .map(|c| format!(" context={c}"))
                    .unwrap_or_default();
                let msg = format!(
                    "[worker {asker} asked]: {question}{ctx_part}\n\
                     You may answer it by calling the MCP tool \
                     `answer_worker` with arguments {{\"session_id\": \"{asker}\", \"answer\": \"...\"}}. \
                     If you don't have enough context, stay quiet and let the user respond.",
                    asker = asker_session,
                    question = question,
                    ctx_part = ctx_part,
                );
                let _ = tx.send(msg).await;
            }
        }

        let answer = self
            .state
            .ask_user(&asker_session, question, context)
            .await?;

        Ok(serde_json::to_string(&serde_json::json!({
            "response": answer
        }))?)
    }

    async fn tool_answer_worker(&self, args: &Value) -> Result<String> {
        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'session_id'"))?;
        let answer = args
            .get("answer")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'answer'"))?;

        self.state
            .send(StateCommand::AnswerUser {
                session_id: session_id.to_string(),
                answer: answer.to_string(),
                answered_by: crate::state::AnsweredBy::Orc,
            })
            .await?;

        Ok(serde_json::to_string(&serde_json::json!({
            "ok": true,
            "session_id": session_id
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

        // Look up the worker name + worktree so we can inspect what
        // actually changed on disk before letting it mark itself done.
        let session = self
            .state
            .get_session(session_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("unknown session: {}", session_id))?;
        let name = session.name.clone();

        let no_changes =
            Self::worktree_has_no_changes_impl(&session.worktree_path, &session.base_commit).await;

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

        let no_changes_tag = if no_changes {
            " (no file changes detected on the worktree — verify this was intentional)"
        } else {
            ""
        };
        self.inject_orc_event(&format!(
            "[internal status — do NOT reply or narrate to the user; \
             the UI has already notified them]\n\
             Worker {name} marked itself done: {summary}{no_changes_tag}\n\
             Track it internally. Only respond if you need to spawn the \
             next task. If you do speak about this worker later, use its \
             name ({name})."
        ))
        .await;

        Ok(serde_json::to_string(&serde_json::json!({
            "ok": true,
            "session_id": session_id,
            "no_changes": no_changes,
        }))?)
    }

    /// Fire-and-forget inject of a synthetic user message into the orc
    /// brain's stream. No-op when the inject channel is absent (test
    /// fixtures, MCP server running outside the orc process).
    async fn inject_orc_event(&self, msg: &str) {
        if let Some(tx) = &self.orc_inject {
            let _ = tx.send(msg.to_string()).await;
        }
    }

    /// Returns true if this worker's worktree has no changes vs its
    /// base commit. Used to warn when mark_done is called with nothing
    /// written.
    async fn worktree_has_no_changes_impl(worktree_path: &str, base_commit: &str) -> bool {
        match crate::review::compute_diff(worktree_path, base_commit).await {
            Ok(d) => d.files.is_empty(),
            Err(_) => false,
        }
    }

    async fn tool_submit_for_review(&self, args: &Value) -> Result<String> {
        let session_id = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'session_id'"))?;
        let summary = args
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let session = self
            .state
            .get_session(session_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("unknown session: {}", session_id))?;

        // Reject empty-diff submissions outright. There is no point
        // reviewing nothing, and historically workers have called
        // submit_for_review without saving any files — the diff was
        // literally empty. Force them to actually do the work first.
        let diff = match crate::review::compute_diff(
            &session.worktree_path,
            &session.base_commit,
        )
        .await
        {
            Ok(d) => d,
            Err(e) => anyhow::bail!("could not compute diff: {e}"),
        };
        if diff.files.is_empty() {
            anyhow::bail!(
                "refusing to submit for review: no file changes on this worker's worktree. \
                 Make the edits, write the files, and commit before calling submit_for_review."
            );
        }
        let diff_hash = crate::review::diff_hash(&diff);

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.state
            .send(StateCommand::ApplyEvent {
                session_id: session_id.to_string(),
                event: SessionEvent::WorkCompleted {
                    diff_hash: diff_hash.clone(),
                },
                reply: tx,
            })
            .await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("state manager dropped reply"))??;

        // Broadcast a friendly review-submitted event so the orc tab can
        // render one human line ("foo ready for review: ...") instead of
        // the raw "session abc12345: AwaitingReview" badge.
        self.state.broadcast(crate::state::StateChange::WorkerReviewSubmitted {
            session_id: session_id.to_string(),
            name: session.name.clone(),
            summary: summary.to_string(),
        });

        // Wake the orc brain so it sees the submission immediately
        // (without this it would only learn when the user next types).
        let name = &session.name;
        let summary_part = if summary.is_empty() {
            String::new()
        } else {
            format!(": {summary}")
        };
        // Silent inject: the UI already shows the user a notification
        // (LogEntry::Notify in handle_state_change). The brain should
        // ingest this as state, not narrate it back — otherwise the
        // user sees the same fact twice.
        self.inject_orc_event(&format!(
            "[internal status — do NOT reply or narrate to the user; \
             the UI has already notified them]\n\
             Worker {name} submitted its work for review{summary_part}.\n\
             Track it internally. Do not write anything in response \
             unless you need to spawn the next dependent task. \
             If you do speak about this worker later, use its name ({name})."
        ))
        .await;

        Ok(serde_json::to_string(&serde_json::json!({
            "ok": true,
            "session_id": session_id,
            "diff_hash": diff_hash,
            "summary": summary
        }))?)
    }

    async fn tool_current_summary(&self, args: &Value) -> Result<String> {
        let summary = args
            .get("summary")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'summary'"))?;
        let key = args
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("orc")
            .to_string();

        self.state
            .send(StateCommand::SetSummary {
                session_id: key.clone(),
                summary: summary.to_string(),
            })
            .await?;

        // When a worker reports progress, surface it to the orc brain so
        // it can stay aware without having to poll list_sessions. Framed
        // as informational — orc decides whether to act. Skips when orc
        // itself is the reporter (we don't echo orc's own summary back
        // at it).
        if key != "orc" {
            let name = self
                .state
                .get_session(&key)
                .await
                .map(|s| s.name)
                .unwrap_or_else(|| key.clone());
            self.inject_orc_event(&format!(
                "[internal status — do NOT reply or narrate to the user; \
                 the UI already shows this in the agents panel]\n\
                 Worker {name} reports progress: {summary}\n\
                 Track it. Only respond if you need to redirect {name}."
            ))
            .await;
        }

        Ok(serde_json::to_string(&serde_json::json!({
            "ok": true,
            "session_id": key
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
pub async fn bind_http_listener() -> Result<(tokio::net::TcpListener, u16)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    Ok((listener, port))
}

pub fn serve_http(listener: tokio::net::TcpListener, server: std::sync::Arc<McpServer>) {
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

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("[mcp-http] server error: {}", e);
        }
    });
}

/// Backwards-compat helper: bind + serve.
pub async fn start_http_server(server: std::sync::Arc<McpServer>) -> Result<u16> {
    let (listener, port) = bind_http_listener().await?;
    serve_http(listener, server);
    Ok(port)
}

// ---------------------------------------------------------------------------
// MCP config generation
// ---------------------------------------------------------------------------

/// Generate MCP config JSON for Claude Code's --mcp-config flag (HTTP transport).
/// The MCP server runs in-process on localhost, Claude Code connects to it.
/// System prompt appended to a worker's claude session.
/// Tells the worker its identity, where it lives, and how to use MCP
/// tools to communicate back.
pub fn worker_system_prompt(
    session_id: &str,
    name: &str,
    task: &str,
    worktree_path: &str,
    branch: &str,
) -> String {
    format!(
        r#"You are an orc worker session.

## Identity
- session_id: {session_id}
- name: {name}

## Your sandbox (READ THIS CAREFULLY)

You are running inside a **git worktree** that is isolated from the user's main project. Your current working directory is already set to it:

    {worktree_path}

This worktree is checked out on branch `{branch}`. **Every file you create or edit MUST live inside this worktree.**

Hard rules about file location:

- Use **relative paths** for Write/Edit/Read whenever possible. `src/foo.rs` lands in the worktree. `./README.md` lands in the worktree. That is what you want.
- If you ever write an **absolute path**, it MUST start with `{worktree_path}`. Never write to paths outside this prefix.
- Do **not** `cd` out of the worktree. Do **not** edit files in the user's main project directory — even if you can see those paths via Read. Read-only inspection of the parent repo (via absolute paths) is fine; writes are not.
- The user reviews your work by diffing this worktree against its base commit. Files written outside the worktree are invisible to them — they will see "no changes" and reject your submission.

## Task
{task}

## How to communicate back
You have access to these MCP tools (server: orc):
- submit_for_review(session_id, summary): Call when your work is ready for human review. Pass your session_id ({session_id}). The user will inspect the diff and approve or reject. **Rejected automatically if your worktree has no changes vs base.**
- mark_done(session_id, summary): Call only if no review is needed (rare). Will warn if no file changes detected.
- ask_user(question, context?, session_id?): Ask the human a question. BLOCKS until they respond. Pass your session_id ({session_id}) so orc can race to answer first if it has context.
- current_summary(summary, session_id?): Record a one-sentence summary of what you are currently working on. Pass your session_id ({session_id}). Call this periodically after meaningful progress, not after every line — the user reads this in the agents panel.

## Rules

**You MUST write your work to disk.** Talking about edits is not editing. Reasoning about what a file should contain is not creating that file. If your task involves producing code, documentation, or any file at all:

1. Use the `Write` or `Edit` tool. Every. Time.
2. Verify with `Read` after writing — confirm the bytes are on disk.
3. Run `git status` to confirm the worktree shows your changes (you should see them under "Untracked files" or "Changes not staged for commit"). If git status is empty, you have not done the work yet.
4. Only then call `submit_for_review` or `mark_done`.

`submit_for_review` will be **rejected** by the harness if your worktree has no changes vs the base commit. There is no exception. "I described what to do" or "I planned it out" is not a valid completion state.

Other rules:

- When done, ALWAYS call submit_for_review first. Don't ask the user to confirm; let them review the diff.
- Keep your responses concise.
- Periodically call `current_summary` to keep the user oriented. After meaningful progress, not after every line.
"#,
        session_id = session_id,
        name = name,
        task = task,
        worktree_path = worktree_path,
        branch = branch,
    )
}

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
        test_server_with_inject().await.0
    }

    /// Variant that returns the orc_inject receiver so a test can
    /// assert what synthetic messages would have been pushed to the
    /// orc brain.
    async fn test_server_with_inject() -> (McpServer, mpsc::Receiver<String>) {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Database::open(dir.path().join("test.db")).unwrap();
        let policy = crate::policy::PolicyEngine::default_policy();
        let (handle, manager) = crate::state::StateManager::new(db, policy);
        tokio::spawn(manager.run());

        let project_dir = dir.path().join("repo");
        std::fs::create_dir_all(&project_dir).unwrap();
        let hook_socket_path = dir.path().join("hooks.sock");

        let _ = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&project_dir)
            .output();
        let _ = std::process::Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(&project_dir)
            .output();

        std::mem::forget(dir);
        let (worker_tx, _worker_rx) = mpsc::unbounded_channel();
        let (inj_tx, inj_rx) = mpsc::channel(8);
        let server = McpServer::new(
            handle,
            project_dir,
            hook_socket_path,
            0,
            WorkerRegistry::new(),
            worker_tx,
            inj_tx,
        );
        (server, inj_rx)
    }

    /// Spawn a session via the MCP tool and return its session_id.
    async fn spawn_session(server: &McpServer, name: &str) -> String {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(1.into())),
            method: "tools/call".into(),
            params: serde_json::json!({
                "name": "spawn_session",
                "arguments": { "name": name, "task": "test" }
            }),
        };
        let resp = server.handle_request(&req).await.unwrap();
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        parsed["session_id"].as_str().unwrap().to_string()
    }

    async fn call_tool(server: &McpServer, name: &str, args: Value) -> Value {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(99.into())),
            method: "tools/call".into(),
            params: serde_json::json!({ "name": name, "arguments": args }),
        };
        let resp = server.handle_request(&req).await.unwrap();
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        serde_json::from_str(&text).unwrap()
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
        assert_eq!(tools, 10);
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
    fn is_orc_mcp_tool_recognizes_names() {
        assert!(is_orc_mcp_tool("spawn_session"));
        assert!(is_orc_mcp_tool("current_summary"));
        assert!(is_orc_mcp_tool("mcp__orc__ask_user"));
        assert!(!is_orc_mcp_tool("Bash"));
        assert!(!is_orc_mcp_tool("Read"));
        assert!(!is_orc_mcp_tool("mcp__other__foo"));
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

    // -----------------------------------------------------------------
    // Worker → orc-brain injection: every tool that signals worker
    // progress must wake the orchestrator so it doesn't go silent when
    // a worker reports done / submits / publishes a summary.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn mark_done_injects_into_orc() {
        let (server, mut inj) = test_server_with_inject().await;
        let sid = spawn_session(&server, "finisher").await;

        // Drain any spawn-time injects (currently none, but be robust).
        while inj.try_recv().is_ok() {}

        let resp = call_tool(
            &server,
            "mark_done",
            serde_json::json!({ "session_id": sid, "summary": "all green" }),
        )
        .await;
        assert_eq!(resp["ok"], true);

        let msg = inj
            .recv()
            .await
            .expect("expected an inject when worker marks done");
        assert!(
            msg.contains("Worker finisher marked itself done"),
            "inject text: {msg}"
        );
        assert!(msg.contains("all green"), "summary missing: {msg}");
    }

    /// submit_for_review now refuses empty-diff workers. Drop a file
    /// into the worker's worktree so the diff isn't empty.
    async fn touch_worktree_file(server: &McpServer, session_id: &str) {
        let session = server
            .state
            .get_session(session_id)
            .await
            .expect("session exists");
        let path = std::path::Path::new(&session.worktree_path).join("scratch.txt");
        std::fs::write(path, "edit").unwrap();
    }

    #[tokio::test]
    async fn submit_for_review_injects_into_orc() {
        let (server, mut inj) = test_server_with_inject().await;
        let sid = spawn_session(&server, "reviewer").await;
        touch_worktree_file(&server, &sid).await;
        while inj.try_recv().is_ok() {}

        let resp = call_tool(
            &server,
            "submit_for_review",
            serde_json::json!({ "session_id": sid, "summary": "added the OAuth flow" }),
        )
        .await;
        assert_eq!(resp["ok"], true);

        let msg = inj
            .recv()
            .await
            .expect("expected an inject when worker submits for review");
        assert!(
            msg.contains("Worker reviewer submitted its work for review"),
            "inject text: {msg}"
        );
        assert!(msg.contains("added the OAuth flow"), "summary missing: {msg}");
    }

    #[tokio::test]
    async fn submit_for_review_inject_omits_summary_when_empty() {
        let (server, mut inj) = test_server_with_inject().await;
        let sid = spawn_session(&server, "quiet").await;
        touch_worktree_file(&server, &sid).await;
        while inj.try_recv().is_ok() {}

        call_tool(
            &server,
            "submit_for_review",
            serde_json::json!({ "session_id": sid }),
        )
        .await;

        let msg = inj.recv().await.expect("inject expected");
        assert!(
            msg.contains("Worker quiet submitted its work for review"),
            "inject text: {msg}"
        );
        // No trailing ": something" segment when summary was missing.
        assert!(
            !msg.contains("for review:"),
            "should not have colon when summary empty: {msg}"
        );
    }

    #[tokio::test]
    async fn submit_for_review_rejects_empty_diff() {
        // Workers that haven't written anything should not be able to
        // claim review-ready. The user reported seeing workers report
        // done without saving files — this guard catches the submit-
        // for-review case at the MCP boundary.
        let (server, _inj) = test_server_with_inject().await;
        let sid = spawn_session(&server, "lazy").await;

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(Value::Number(42.into())),
            method: "tools/call".into(),
            params: serde_json::json!({
                "name": "submit_for_review",
                "arguments": { "session_id": sid, "summary": "claims done" }
            }),
        };
        let resp = server.handle_request(&req).await.unwrap();
        let result = resp.result.expect("tool errors go in content");
        assert_eq!(result["isError"], true, "expected error response");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("no file changes"),
            "expected empty-diff error, got: {text}"
        );
    }

    #[tokio::test]
    async fn current_summary_from_worker_injects_into_orc() {
        let (server, mut inj) = test_server_with_inject().await;
        let sid = spawn_session(&server, "scout").await;
        while inj.try_recv().is_ok() {}

        call_tool(
            &server,
            "current_summary",
            serde_json::json!({ "session_id": sid, "summary": "scanning files" }),
        )
        .await;

        let msg = inj.recv().await.expect("inject expected for worker summary");
        assert!(
            msg.contains("Worker scout reports progress"),
            "inject text: {msg}"
        );
        assert!(msg.contains("scanning files"), "summary missing: {msg}");
        // Internal-status framing tells the brain to ingest silently.
        assert!(msg.contains("internal status"), "framing missing: {msg}");
    }

    #[tokio::test]
    async fn current_summary_from_orc_does_not_inject() {
        // Orc itself sets a summary — we must not echo it back into orc's
        // own message stream (that would create a loop).
        let (server, mut inj) = test_server_with_inject().await;
        while inj.try_recv().is_ok() {}

        call_tool(
            &server,
            "current_summary",
            serde_json::json!({ "session_id": "orc", "summary": "planning" }),
        )
        .await;

        // Default session_id is "orc" when omitted — same path.
        call_tool(
            &server,
            "current_summary",
            serde_json::json!({ "summary": "still planning" }),
        )
        .await;

        // Should be empty. Use a short timeout to avoid hanging.
        let got = tokio::time::timeout(std::time::Duration::from_millis(50), inj.recv()).await;
        assert!(got.is_err(), "did not expect any inject for orc's own summary, got: {got:?}");
    }
}
