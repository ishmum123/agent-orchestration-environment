// Backchannel: a sealed-off "scratch" claude conversation rendered as an
// overlay above orc's UI. Reached via `?`. Spawns a stream-json claude
// child (sonnet) in the project cwd with no MCP, no policy, no system
// prompt — a single persistent sidekick the user can interrupt orc with
// to ask questions, read code, brainstorm.
//
// Trust-root isolation: nothing in this module touches `orc_view`,
// session state, or the orchestrator's event stream. Events flow only
// into `Backchannel::history`.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, Mutex};

use crate::input_attachments::{Attachment, AttachmentSet};
use crate::worker::claude_bin;

/// One entry in the backchannel transcript.
#[derive(Debug, Clone)]
pub enum BackchannelEntry {
    User(String),
    Assistant(String),
    /// Tool calls are flattened to `[tool: name]` — sonnet rarely calls
    /// tools when asked questions, but we collapse rather than render.
    Tool(String),
    System(String),
}

/// Events emitted from the reader task back to the main event loop.
#[derive(Debug, Clone)]
pub enum BackchannelEvent {
    Assistant(String),
    Tool(String),
    /// Turn finished — used to clear the spinner.
    TurnEnd,
    Exited(Option<i32>),
}

/// Reason we asked the channel for a summary mid-stream. Drives the
/// follow-up action when the next assistant turn lands.
#[derive(Debug, Clone, Copy)]
pub enum PendingPromotion {
    /// Ctrl-B: respawn channel on the given model with the summary as
    /// initial context.
    Model,
    /// Ctrl-N: kill the channel and spawn a fresh regular worker with
    /// the summary as its task.
    AttachAsWorker,
}

/// Parsed slug + task extracted from the model's summary turn. Used by
/// the Ctrl+N attach flow to name and brief the new worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummarySpec {
    pub slug: String,
    pub task: String,
}

/// Pull a `{slug, task}` pair out of the model's summary reply. The
/// prompt (see `request_attach_summary`) asks for strict JSON, but
/// claude sometimes wraps it in prose or a code fence — we extract the
/// first balanced `{…}` and parse that. On any failure we fall back to
/// a timestamp slug + the raw text as the task, so a non-JSON reply
/// (e.g. the echo-mode fake_claude shim) still produces a usable worker.
pub fn parse_summary(raw: &str) -> SummarySpec {
    let trimmed = raw.trim();
    if let Some((start, end)) = find_json_object(trimmed) {
        if let Ok(v) = serde_json::from_str::<Value>(&trimmed[start..=end]) {
            let slug = v.get("slug").and_then(|x| x.as_str()).map(sanitize_slug);
            let task = v
                .get("task")
                .and_then(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            if let (Some(slug), Some(task)) = (slug, task) {
                if !slug.is_empty() {
                    return SummarySpec { slug, task };
                }
            }
        }
    }
    SummarySpec {
        slug: fallback_slug(),
        task: if trimmed.is_empty() {
            "(scratch chat had no summary)".to_string()
        } else {
            trimmed.to_string()
        },
    }
}

fn find_json_object(s: &str) -> Option<(usize, usize)> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((start, i));
                }
            }
            _ => {}
        }
    }
    None
}

fn sanitize_slug(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.trim().chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if !out.is_empty() && !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 20 {
        out.truncate(20);
        while out.ends_with('-') {
            out.pop();
        }
    }
    out
}

fn fallback_slug() -> String {
    use chrono::Local;
    format!("scratch-{}", Local::now().format("%H%M%S"))
}

/// Persistent overlay state. Lives in `App` as `Option<Backchannel>`.
/// Survives overlay close (Esc); only torn down on orc quit, Ctrl-X,
/// Ctrl-B promotion, Ctrl-N promotion, or when the child exits.
pub struct Backchannel {
    /// Visible chat log.
    pub history: Vec<BackchannelEntry>,
    /// Multi-line composing buffer.
    pub input: String,
    /// Scroll offset from top of history (wrapped lines, like worker tabs).
    pub scroll: usize,
    /// Tail-follow the latest entry.
    pub stick_to_bottom: bool,
    /// True while a turn is in flight (assistant hasn't replied / finished).
    pub thinking: bool,
    /// True while the overlay is rendered. False == background.
    pub open: bool,
    /// Current model alias (e.g. "sonnet", "opus").
    pub model: String,
    /// Mid-flight promotion request. When set, the next assistant text
    /// completes the action instead of going into history.
    pub pending: Option<PendingPromotion>,
    /// Promotion target model (used when `pending == Some(Model)`).
    pub pending_model: Option<String>,
    /// Accumulator for the summary turn's text blocks.
    pub pending_buffer: String,
    /// Pending image attachments staged via Ctrl+V or bracketed paste.
    /// Drained into the next outgoing user message.
    pub attachments: AttachmentSet,

    child: Arc<Mutex<Child>>,
    stdin: Arc<Mutex<ChildStdin>>,
}

impl Backchannel {
    /// Spawn the backchannel claude child and start the reader task.
    /// Returns the state + the receiver the main loop polls for events.
    pub async fn spawn(
        project_dir: &Path,
    ) -> Result<(Self, mpsc::UnboundedReceiver<BackchannelEvent>)> {
        Self::spawn_with_model(project_dir, "sonnet", None).await
    }

    /// Spawn variant with explicit model and optional seed prompt (used
    /// by Ctrl-B promotion to carry the prior conversation summary into
    /// the new child).
    pub async fn spawn_with_model(
        project_dir: &Path,
        model: &str,
        seed: Option<String>,
    ) -> Result<(Self, mpsc::UnboundedReceiver<BackchannelEvent>)> {
        let mut cmd = Command::new(claude_bin());
        cmd.args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--model",
            model,
        ]);
        cmd.current_dir(project_dir);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::null());

        let mut child = cmd.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("backchannel stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("backchannel stdout unavailable"))?;

        let child_arc = Arc::new(Mutex::new(child));
        let stdin_arc = Arc::new(Mutex::new(stdin));

        let (tx, rx) = mpsc::unbounded_channel();

        {
            let child_arc = child_arc.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let raw: Value = match serde_json::from_str(trimmed) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    for ev in parse_events(&raw) {
                        if tx.send(ev).is_err() {
                            return;
                        }
                    }
                }
                let code = {
                    let mut c = child_arc.lock().await;
                    c.wait().await.ok().and_then(|s| s.code())
                };
                let _ = tx.send(BackchannelEvent::Exited(code));
            });
        }

        let mut bc = Self {
            history: Vec::new(),
            input: String::new(),
            scroll: 0,
            stick_to_bottom: true,
            thinking: false,
            open: true,
            model: model.to_string(),
            pending: None,
            pending_model: None,
            pending_buffer: String::new(),
            attachments: AttachmentSet::new(),
            child: child_arc,
            stdin: stdin_arc,
        };

        if let Some(seed_text) = seed {
            // Hand the prior summary to the new child as a user message
            // so the conversation has continuity. Recorded as a System
            // entry so the user can see what was carried over.
            bc.history.push(BackchannelEntry::System(format!(
                "(carried over from previous channel) {seed_text}"
            )));
            bc.send_raw(&seed_text).await.ok();
            bc.thinking = true;
        }

        Ok((bc, rx))
    }

    /// Write a user message to stdin without recording it in history.
    /// Used by the promotion summary prompts and seed messages.
    async fn send_raw(&self, message: &str) -> Result<()> {
        self.send_raw_with(message, &[]).await
    }

    /// Like `send_raw` but with image attachments stitched into the
    /// stream-json content-block array. Empty `attachments` keeps the
    /// plain text-content shape.
    async fn send_raw_with(&self, message: &str, attachments: &[Attachment]) -> Result<()> {
        let msg = crate::input_attachments::build_user_message(message, attachments);
        let line = serde_json::to_string(&msg)?;
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    /// Issue a hidden summary request for the Ctrl+B model-promotion
    /// flow. The next assistant turn lands in `pending_buffer` so the
    /// main loop can seed a fresh child with it.
    pub async fn request_summary(&mut self, kind: PendingPromotion) -> Result<()> {
        self.pending = Some(kind);
        self.pending_buffer.clear();
        self.thinking = true;
        self.send_raw(
            "Summarize this conversation in 1-3 sentences so it can be handed off. Reply with only the summary.",
        )
        .await
    }

    /// Ctrl+N attach: ask the channel for both a short kebab-case slug
    /// and a 1-3 sentence task description, as JSON so we can parse
    /// them apart. The fresh worker uses the slug as its session name
    /// and the task description as its initial brief.
    pub async fn request_attach_summary(&mut self) -> Result<()> {
        self.pending = Some(PendingPromotion::AttachAsWorker);
        self.pending_buffer.clear();
        self.thinking = true;
        self.send_raw(
            "I'm spinning up a regular worker agent to take this on. Reply with ONLY a JSON object on a single line, no prose, no code fence, in this shape:\n\
             {\"slug\":\"<short kebab-case identifier, <=20 chars, [a-z0-9-] only>\",\"task\":\"<1-3 sentence task brief for the worker>\"}\n\
             The slug names the worker (e.g. \"rate-limit\", \"ci-fix\", \"add-flag\"). The task tells the worker what to do.",
        )
        .await
    }

    /// Send a user message into the child + record it in history.
    /// Drains the pending `attachments` into the same message.
    pub async fn send(&mut self, message: &str) -> Result<()> {
        let imgs = self.attachments.take_all();
        self.send_raw_with(message, &imgs).await?;
        let suffix = if imgs.is_empty() {
            String::new()
        } else {
            format!(
                " [+{} image{}]",
                imgs.len(),
                if imgs.len() == 1 { "" } else { "s" }
            )
        };
        self.history
            .push(BackchannelEntry::User(format!("{message}{suffix}")));
        self.thinking = true;
        self.stick_to_bottom = true;
        Ok(())
    }

    pub async fn kill(&self) {
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
    }

    /// Apply an event from the reader task into history. Returns the
    /// completed promotion summary when a `pending` request just
    /// finished (so the main loop can act on it).
    pub fn apply_event(&mut self, ev: BackchannelEvent) -> Option<(PendingPromotion, String)> {
        match ev {
            BackchannelEvent::Assistant(text) => {
                if self.pending.is_some() {
                    if !self.pending_buffer.is_empty() {
                        self.pending_buffer.push('\n');
                    }
                    self.pending_buffer.push_str(&text);
                } else {
                    self.history.push(BackchannelEntry::Assistant(text));
                }
            }
            BackchannelEvent::Tool(name) => {
                if self.pending.is_none() {
                    self.history
                        .push(BackchannelEntry::Tool(format!("[tool: {name}]")));
                }
            }
            BackchannelEvent::TurnEnd => {
                self.thinking = false;
                if let Some(kind) = self.pending.take() {
                    let summary = std::mem::take(&mut self.pending_buffer);
                    return Some((kind, summary));
                }
            }
            BackchannelEvent::Exited(code) => {
                let msg = match code {
                    Some(0) => "backchannel exited cleanly".to_string(),
                    Some(c) => format!("backchannel exited with code {c}"),
                    None => "backchannel exited".to_string(),
                };
                self.history.push(BackchannelEntry::System(msg));
                self.thinking = false;
            }
        }
        None
    }
}

fn parse_events(raw: &Value) -> Vec<BackchannelEvent> {
    let mut out = Vec::new();
    let t = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match t {
        "assistant" => {
            if let Some(content) = raw.pointer("/message/content").and_then(|v| v.as_array()) {
                for block in content {
                    let bt = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match bt {
                        "text" => {
                            if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                                if !t.is_empty() {
                                    out.push(BackchannelEvent::Assistant(t.to_string()));
                                }
                            }
                        }
                        "tool_use" => {
                            let name = block
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?")
                                .to_string();
                            out.push(BackchannelEvent::Tool(name));
                        }
                        _ => {}
                    }
                }
            }
        }
        "result" => {
            out.push(BackchannelEvent::TurnEnd);
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a Backchannel without going through `spawn` — for state
    /// transition tests that don't need IPC. We spawn a trivial
    /// short-lived child (`true`) just so we have valid Child/ChildStdin
    /// handles to stash; nothing reads or writes them.
    fn fake_bc() -> Backchannel {
        let mut tchild = tokio::process::Command::new("true")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn true");
        let stdin = tchild.stdin.take().unwrap();
        Backchannel {
            history: Vec::new(),
            input: String::new(),
            scroll: 0,
            stick_to_bottom: true,
            thinking: false,
            open: true,
            model: "sonnet".to_string(),
            pending: None,
            pending_model: None,
            pending_buffer: String::new(),
            attachments: AttachmentSet::new(),
            child: Arc::new(Mutex::new(tchild)),
            stdin: Arc::new(Mutex::new(stdin)),
        }
    }

    #[tokio::test]
    async fn assistant_event_appends_to_history() {
        let mut bc = fake_bc();
        bc.thinking = true;
        let completed = bc.apply_event(BackchannelEvent::Assistant("hi".into()));
        assert!(completed.is_none());
        assert_eq!(bc.history.len(), 1);
        assert!(matches!(&bc.history[0], BackchannelEntry::Assistant(s) if s == "hi"));
    }

    #[tokio::test]
    async fn pending_summary_captured_then_returned_on_turn_end() {
        let mut bc = fake_bc();
        bc.pending = Some(PendingPromotion::AttachAsWorker);
        // Assistant during pending goes to buffer, not history.
        bc.apply_event(BackchannelEvent::Assistant("a brief summary".into()));
        assert!(bc.history.is_empty());
        assert_eq!(bc.pending_buffer, "a brief summary");
        // Turn end yields the captured summary + clears pending.
        let completed = bc.apply_event(BackchannelEvent::TurnEnd);
        assert!(matches!(
            completed,
            Some((PendingPromotion::AttachAsWorker, ref s)) if s == "a brief summary"
        ));
        assert!(bc.pending.is_none());
        assert!(bc.pending_buffer.is_empty());
    }

    #[tokio::test]
    async fn turn_end_clears_thinking() {
        let mut bc = fake_bc();
        bc.thinking = true;
        bc.apply_event(BackchannelEvent::TurnEnd);
        assert!(!bc.thinking);
    }

    #[tokio::test]
    async fn close_preserves_history() {
        // Simulates open → close → reopen with prior conversation intact.
        let mut bc = fake_bc();
        bc.history.push(BackchannelEntry::User("hello".into()));
        bc.history.push(BackchannelEntry::Assistant("hi".into()));
        bc.open = false;
        assert_eq!(bc.history.len(), 2);
        bc.open = true;
        assert_eq!(bc.history.len(), 2);
    }

    #[test]
    fn parse_assistant_text() {
        let raw = serde_json::json!({
            "type": "assistant",
            "message": { "content": [
                { "type": "text", "text": "hello" }
            ]}
        });
        let evs = parse_events(&raw);
        assert_eq!(evs.len(), 1);
        assert!(matches!(&evs[0], BackchannelEvent::Assistant(s) if s == "hello"));
    }

    #[test]
    fn parse_tool_use_collapses_to_name() {
        let raw = serde_json::json!({
            "type": "assistant",
            "message": { "content": [
                { "type": "tool_use", "name": "Read", "input": {} }
            ]}
        });
        let evs = parse_events(&raw);
        assert_eq!(evs.len(), 1);
        assert!(matches!(&evs[0], BackchannelEvent::Tool(s) if s == "Read"));
    }

    #[test]
    fn parse_summary_extracts_slug_and_task() {
        let s = parse_summary(r#"{"slug":"rate-limit","task":"Add a rate limiter to the public API."}"#);
        assert_eq!(s.slug, "rate-limit");
        assert_eq!(s.task, "Add a rate limiter to the public API.");
    }

    #[test]
    fn parse_summary_strips_prose_and_fence() {
        let s = parse_summary(
            "Sure! Here you go:\n```json\n{\"slug\":\"Add Flag\",\"task\":\"Add the --foo flag.\"}\n```\n",
        );
        assert_eq!(s.slug, "add-flag");
        assert_eq!(s.task, "Add the --foo flag.");
    }

    #[test]
    fn parse_summary_truncates_long_slug() {
        let s = parse_summary(
            r#"{"slug":"this-is-a-really-really-long-slug","task":"do it"}"#,
        );
        assert!(s.slug.len() <= 20);
        assert!(s.slug.starts_with("this-is-a-really"));
    }

    #[test]
    fn parse_summary_falls_back_when_not_json() {
        // Echo-mode fake_claude reply: no JSON at all.
        let s = parse_summary("[fake-claude turn 3] received: please summarize this");
        assert!(s.slug.starts_with("scratch-"));
        assert!(s.task.contains("[fake-claude"));
    }

    #[test]
    fn parse_summary_falls_back_when_empty() {
        let s = parse_summary("   ");
        assert!(s.slug.starts_with("scratch-"));
        assert!(!s.task.is_empty());
    }

    #[test]
    fn parse_result_is_turn_end() {
        let raw = serde_json::json!({ "type": "result", "subtype": "success" });
        let evs = parse_events(&raw);
        assert_eq!(evs.len(), 1);
        assert!(matches!(&evs[0], BackchannelEvent::TurnEnd));
    }
}
