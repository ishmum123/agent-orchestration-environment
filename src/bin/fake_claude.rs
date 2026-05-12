// Deterministic stand-in for the `claude` CLI used during orc's TUI/UX
// harness runs. Real `claude` invocations cost tokens and produce
// non-deterministic output; this shim emits the same stream-json shape orc
// parses, with hard-coded responses, so harness runs are free and stable.
//
// Activated via the `ORC_CLAUDE_BIN` env var. Set it to the path of the
// installed `fake_claude` binary before launching orc in tests.
//
// Two modes:
// 1. Echo mode (default — when ORC_FAKE_SCRIPT is unset). Each stdin line
//    produces one assistant turn that echoes the input back. Used by the
//    bare TUI smoke tests.
// 2. Scripted mode (when ORC_FAKE_SCRIPT points to a JSON file). The shim
//    auto-detects whether it's the orc orchestrator or a worker by inspecting
//    its cwd, picks the matching script section, and on each stdin line
//    executes a list of actions: write_file, run_cmd, mcp_call, text. Used by
//    the full FLOW e2e harness to drive the entire user journey.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};

/// Set by SIGINT handler. Action loop checks between actions and during
/// sleep_ms; when set, the rest of the current turn is dropped but the
/// process keeps running (mirrors how real claude handles Ctrl-C).
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// CLI arg helpers
// ---------------------------------------------------------------------------

fn arg_value(args: &[String], key: &str) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == key {
            return args.get(i + 1).cloned();
        }
        if let Some(rest) = args[i].strip_prefix(&format!("{key}=")) {
            return Some(rest.to_string());
        }
        i += 1;
    }
    None
}

/// Extract user text + image count from a stream-json `user` message
/// body. Supports both the legacy text-only `content: "..."` shape and
/// the content-block array shape introduced for image attachments.
fn extract_user_text_and_images(v: &Value) -> (String, usize) {
    let content = match v.pointer("/message/content") {
        Some(c) => c,
        None => return ("<no content>".to_string(), 0),
    };
    if let Some(s) = content.as_str() {
        return (s.to_string(), 0);
    }
    if let Some(arr) = content.as_array() {
        let mut text = String::new();
        let mut images = 0usize;
        for block in arr {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(t);
                    }
                }
                Some("image") => images += 1,
                _ => {}
            }
        }
        return (text, images);
    }
    ("<unknown shape>".to_string(), 0)
}

/// Pull `session_id: <uuid>` out of the worker system prompt. Real claude
/// gets the same string and uses it to call MCP tools; we mirror that.
fn extract_session_id(prompt: &str) -> Option<String> {
    for line in prompt.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("- session_id:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// MCP config + client
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct McpClient {
    host: String,
    port: u16,
    path: String,
}

impl McpClient {
    fn from_config(config_path: &Path) -> Option<Self> {
        let raw = fs::read_to_string(config_path).ok()?;
        let v: Value = serde_json::from_str(&raw).ok()?;
        let url = v
            .pointer("/mcpServers/orc/url")
            .and_then(|x| x.as_str())?;
        // Parse http://127.0.0.1:PORT/path
        let stripped = url.strip_prefix("http://")?;
        let (hostport, path) = match stripped.find('/') {
            Some(idx) => (&stripped[..idx], &stripped[idx..]),
            None => (stripped, "/"),
        };
        let (host, port) = hostport.rsplit_once(':')?;
        Some(McpClient {
            host: host.to_string(),
            port: port.parse().ok()?,
            path: path.to_string(),
        })
    }

    /// Synchronous JSON-RPC call to `tools/call`. Returns the JSON `result`
    /// value (or Err on transport / RPC failure).
    fn tools_call(&self, tool: &str, args: &Value) -> Result<Value, String> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool, "arguments": args },
        });
        let body_str = body.to_string();
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
            path = self.path,
            host = self.host,
            port = self.port,
            len = body_str.len(),
            body = body_str,
        );
        let mut stream = TcpStream::connect((self.host.as_str(), self.port))
            .map_err(|e| format!("connect: {e}"))?;
        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("write: {e}"))?;
        let mut buf = String::new();
        stream
            .read_to_string(&mut buf)
            .map_err(|e| format!("read: {e}"))?;
        // Split headers from body.
        let body_start = buf.find("\r\n\r\n").map(|i| i + 4).unwrap_or(buf.len());
        let body = &buf[body_start..];
        let v: Value = serde_json::from_str(body).map_err(|e| format!("parse: {e} | {body}"))?;
        if let Some(err) = v.get("error") {
            return Err(format!("rpc error: {err}"));
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }
}

// ---------------------------------------------------------------------------
// Script execution
// ---------------------------------------------------------------------------

struct Ctx {
    self_session_id: String,
    last_spawned_session_id: String,
    mcp: Option<McpClient>,
    /// Set true when an action requested process exit; main loop honors it.
    exit_requested: Option<i32>,
}

fn substitute(s: &str, ctx: &Ctx) -> String {
    s.replace("$SELF", &ctx.self_session_id)
        .replace("$WORKER_ID", &ctx.last_spawned_session_id)
}

fn substitute_value(v: &Value, ctx: &Ctx) -> Value {
    match v {
        Value::String(s) => Value::String(substitute(s, ctx)),
        Value::Array(arr) => Value::Array(arr.iter().map(|x| substitute_value(x, ctx)).collect()),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map.iter() {
                out.insert(k.clone(), substitute_value(val, ctx));
            }
            Value::Object(out)
        }
        _ => v.clone(),
    }
}

/// Emit one tool_use + matching tool_result pair as stream-json. The
/// tool_use block goes inside the assistant message; the tool_result block
/// goes in a synthetic user message. orc parses both shapes.
fn emit_tool_round<W: Write>(
    out: &mut W,
    name: &str,
    input: &Value,
    output: &str,
    is_error: bool,
) {
    let tool_id = format!("toolu_{}", uuid_v4().replace('-', ""));
    let assistant = json!({
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": tool_id,
                "name": name,
                "input": input,
            }],
            "usage": {
                "input_tokens": 50u64,
                "output_tokens": 10u64,
                "cache_read_input_tokens": 0u64,
                "cache_creation_input_tokens": 0u64,
            }
        }
    });
    writeln!(out, "{assistant}").ok();
    let user = json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": tool_id,
                "content": output,
                "is_error": is_error,
            }]
        }
    });
    writeln!(out, "{user}").ok();
    // Flush so orc renders mid-turn output in real time. Without this the
    // pipe stays block-buffered and the user only sees results at end of
    // turn — which defeats the point of streaming.
    out.flush().ok();
}

fn execute_action<W: Write>(action: &Value, ctx: &mut Ctx, log: &mut Vec<String>, stream: &mut W) {
    let kind = action.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match kind {
        "text" => {
            let content = action.get("content").and_then(|v| v.as_str()).unwrap_or("");
            log.push(substitute(content, ctx));
        }
        "write_file" => {
            let path = action.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let content = action.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let p = PathBuf::from(path);
            if let Some(parent) = p.parent() {
                if !parent.as_os_str().is_empty() {
                    let _ = fs::create_dir_all(parent);
                }
            }
            match fs::write(&p, content) {
                Ok(_) => log.push(format!("[wrote] {}", p.display())),
                Err(e) => log.push(format!("[write_file ERR {}] {}", p.display(), e)),
            }
        }
        "run_cmd" => {
            let cmd = action.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
            let args: Vec<String> = action
                .get("args")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| substitute(s, ctx)))
                        .collect()
                })
                .unwrap_or_default();
            let out = Command::new(cmd).args(&args).output();
            match out {
                Ok(o) => {
                    let code = o.status.code().unwrap_or(-1);
                    log.push(format!("[cmd] {cmd} {} -> exit {code}", args.join(" ")));
                    if code != 0 {
                        let stderr = String::from_utf8_lossy(&o.stderr);
                        log.push(format!("[cmd-stderr] {}", stderr.trim()));
                    }
                }
                Err(e) => log.push(format!("[cmd ERR] {cmd}: {e}")),
            }
        }
        "mcp_call" => {
            let tool = action.get("tool").and_then(|v| v.as_str()).unwrap_or("");
            let raw_args = action.get("args").cloned().unwrap_or(Value::Object(Default::default()));
            let args = substitute_value(&raw_args, ctx);
            match &ctx.mcp {
                None => log.push(format!("[mcp_call SKIP — no mcp client] {tool}")),
                Some(client) => match client.tools_call(tool, &args) {
                    Ok(result) => {
                        log.push(format!("[mcp:{tool}] ok"));
                        // If this was spawn_session, capture the new session_id.
                        if tool == "spawn_session" {
                            // Result shape: { content: [{ type: "text", text: "{json}" }], isError: false }
                            let text = result
                                .pointer("/content/0/text")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            match serde_json::from_str::<Value>(text) {
                                Ok(parsed) => match parsed
                                    .get("session_id")
                                    .and_then(|v| v.as_str())
                                {
                                    Some(sid) => {
                                        ctx.last_spawned_session_id = sid.to_string();
                                        log.push(format!("[spawn_session -> {sid}]"));
                                    }
                                    None => log.push(format!(
                                        "[spawn_session no sid] parsed={parsed} text={text}"
                                    )),
                                },
                                Err(e) => log.push(format!(
                                    "[spawn_session text-not-json] err={e} text={text} full={result}"
                                )),
                            }
                        }
                    }
                    Err(e) => log.push(format!("[mcp:{tool} ERR] {e}")),
                },
            }
        }
        "sleep_ms" => {
            // Break the sleep into 100ms slices so SIGINT can interrupt.
            let total = action.get("ms").and_then(|v| v.as_u64()).unwrap_or(0);
            let slice = 100u64;
            let mut elapsed = 0u64;
            while elapsed < total {
                if INTERRUPTED.load(Ordering::SeqCst) {
                    log.push(format!("[sleep interrupted at {elapsed}ms / {total}ms]"));
                    return;
                }
                let step = slice.min(total - elapsed);
                std::thread::sleep(std::time::Duration::from_millis(step));
                elapsed += step;
            }
        }
        "simulate_tool" => {
            // Emits a tool_use + tool_result pair to the stream so orc renders
            // the step as if real claude called the tool. If `run_cmd` is
            // also given, the tool is actually executed and its stdout is
            // returned as the tool_result content; otherwise `output` is used.
            let name = action.get("name").and_then(|v| v.as_str()).unwrap_or("Bash");
            let raw_input = action
                .get("input")
                .cloned()
                .unwrap_or(Value::Object(Default::default()));
            let input = substitute_value(&raw_input, ctx);
            let real_cmd = action.get("run_cmd").and_then(|v| v.as_str());
            let real_args: Vec<String> = action
                .get("run_args")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| substitute(s, ctx)))
                        .collect()
                })
                .unwrap_or_default();
            let (output_text, is_error) = if let Some(cmd) = real_cmd {
                match Command::new(cmd).args(&real_args).output() {
                    Ok(o) => {
                        let code = o.status.code().unwrap_or(-1);
                        let body = format!(
                            "exit {}\nstdout:\n{}\nstderr:\n{}",
                            code,
                            String::from_utf8_lossy(&o.stdout),
                            String::from_utf8_lossy(&o.stderr)
                        );
                        (body, code != 0)
                    }
                    Err(e) => (format!("[exec error] {cmd}: {e}"), true),
                }
            } else {
                let body = action
                    .get("output")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(simulated)")
                    .to_string();
                let err = action
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                (body, err)
            };
            emit_tool_round(stream, name, &input, &output_text, is_error);
            log.push(format!(
                "[tool:{name}] simulated{}",
                if is_error { " (is_error)" } else { "" }
            ));
        }
        "exit_code" => {
            // Request fake_claude to exit with the given code after this turn
            // completes. Used to drive the orc Restart-Worker flow.
            let code = action.get("code").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            ctx.exit_requested = Some(code);
            log.push(format!("[exit_requested {code}]"));
        }
        other => log.push(format!("[unknown action type] {other}")),
    }
}

// ---------------------------------------------------------------------------
// Stream-json emission helpers
// ---------------------------------------------------------------------------

fn emit_init<W: Write>(out: &mut W, session_id: &str, model: &str) {
    let v = json!({
        "type": "system",
        "subtype": "init",
        "session_id": session_id,
        "model": model,
    });
    writeln!(out, "{v}").ok();
}

fn emit_assistant_text<W: Write>(out: &mut W, text: &str) {
    let v = json!({
        "type": "assistant",
        "message": {
            "role": "assistant",
            "content": [{ "type": "text", "text": text }],
            "usage": {
                "input_tokens": 100u64,
                "output_tokens": 20u64,
                "cache_read_input_tokens": 0u64,
                "cache_creation_input_tokens": 0u64,
            }
        }
    });
    writeln!(out, "{v}").ok();
}

fn emit_result<W: Write>(out: &mut W) {
    let v = json!({ "type": "result", "subtype": "success", "total_cost_usd": 0.0 });
    writeln!(out, "{v}").ok();
}

// ---------------------------------------------------------------------------
// uuid v4 (no crate dep here)
// ---------------------------------------------------------------------------

fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let mut bytes = [0u8; 16];
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u128)
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let mut x = nanos.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(pid);
    for b in bytes.iter_mut() {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *b = (x >> 56) as u8;
    }
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    let h: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version") {
        println!("orc-fake-claude 0.1.0 (stub)");
        return;
    }

    let model = arg_value(&args, "--model").unwrap_or_else(|| "fake".to_string());
    let session_id = uuid_v4();
    let mut stdout = io::stdout().lock();
    emit_init(&mut stdout, &session_id, &model);
    stdout.flush().ok();

    let script_path = std::env::var("ORC_FAKE_SCRIPT").ok();
    let stdin = io::stdin();

    if script_path.is_none() {
        // Echo mode — preserve original behavior for existing tests.
        let mut turn: u32 = 0;
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            turn += 1;
            let parsed = serde_json::from_str::<Value>(&line).ok();
            let (user_text, image_count) = match parsed {
                Some(v) => extract_user_text_and_images(&v),
                None => ("<unparseable>".to_string(), 0),
            };
            let snippet: String = user_text.chars().take(80).collect();
            let suffix = if image_count > 0 {
                format!("; [{image_count} images attached]")
            } else {
                String::new()
            };
            emit_assistant_text(
                &mut stdout,
                &format!("[fake-claude turn {turn}] received: {snippet}{suffix}"),
            );
            emit_result(&mut stdout);
            stdout.flush().ok();
        }
        return;
    }

    // Scripted mode.
    let script_path = script_path.unwrap();
    let script_raw = match fs::read_to_string(&script_path) {
        Ok(s) => s,
        Err(e) => {
            emit_assistant_text(
                &mut stdout,
                &format!("[fake-claude] failed to read ORC_FAKE_SCRIPT={script_path}: {e}"),
            );
            emit_result(&mut stdout);
            return;
        }
    };
    let script: Value = match serde_json::from_str(&script_raw) {
        Ok(v) => v,
        Err(e) => {
            emit_assistant_text(&mut stdout, &format!("[fake-claude] script parse error: {e}"));
            emit_result(&mut stdout);
            return;
        }
    };

    // Detect mode: worker if cwd contains `.orc-worktrees`, else orchestrator.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd_str = cwd.to_string_lossy();
    let mode: &'static str = if cwd_str.contains(".orc-worktrees") {
        "worker"
    } else {
        "orchestrator"
    };
    // For workers, the basename of the cwd is the worker name (orc creates
    // worktrees at `<project>/.orc-worktrees/<name>`). This lets a script
    // ship per-worker sections under `worker_<name>` keys; we fall back to
    // the generic `worker` key when no per-worker section exists.
    let worker_name = if mode == "worker" {
        cwd.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string()
    } else {
        String::new()
    };

    // For workers, pull session_id out of --append-system-prompt.
    let prompt_arg = arg_value(&args, "--append-system-prompt")
        .or_else(|| arg_value(&args, "--system-prompt"))
        .unwrap_or_default();
    let self_sid = if mode == "worker" {
        extract_session_id(&prompt_arg).unwrap_or_else(|| session_id.clone())
    } else {
        "orc".to_string()
    };

    // MCP client.
    let mcp_config_path = arg_value(&args, "--mcp-config").map(PathBuf::from);
    let mcp = mcp_config_path.as_deref().and_then(McpClient::from_config);

    let mut ctx = Ctx {
        self_session_id: self_sid,
        last_spawned_session_id: String::new(),
        mcp,
        exit_requested: None,
    };

    // Install SIGINT handler that flips INTERRUPTED. The action loop reads
    // it between actions and during sleep slices.
    install_sigint_handler();

    let per_worker_key = format!("worker_{worker_name}");
    let turns = script
        .get(per_worker_key.as_str())
        .or_else(|| script.get(mode))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Optionally allow workers to keep session_id stash across turns via env.
    let stash_path = std::env::var("ORC_FAKE_STASH").ok();
    if let Some(ref p) = stash_path {
        if let Ok(raw) = fs::read_to_string(p) {
            if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&raw) {
                if let Some(v) = map.get("last_spawned_session_id") {
                    ctx.last_spawned_session_id = v.clone();
                }
            }
        }
    }

    let mut turn_idx: usize = 0;
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let turn_def = turns.get(turn_idx).cloned();
        turn_idx += 1;

        let mut log = vec![format!(
            "[fake-claude {mode} turn {turn_idx}]",
        )];

        // Reset interrupt flag at the start of every turn.
        INTERRUPTED.store(false, Ordering::SeqCst);

        if let Some(turn) = turn_def {
            if let Some(actions) = turn.get("actions").and_then(|v| v.as_array()) {
                for action in actions {
                    if INTERRUPTED.load(Ordering::SeqCst) {
                        log.push(format!("[turn {turn_idx} interrupted by SIGINT]"));
                        break;
                    }
                    execute_action(action, &mut ctx, &mut log, &mut stdout);
                }
            }
        } else {
            log.push(format!("[no scripted turn {turn_idx} — idle]"));
        }

        // Persist stash so a re-spawned process inherits state (workers can
        // be restarted via R; orchestrator is the long-lived process and
        // doesn't need this, but it's cheap to keep symmetric).
        if let Some(ref p) = stash_path {
            let mut map = HashMap::new();
            map.insert(
                "last_spawned_session_id".to_string(),
                ctx.last_spawned_session_id.clone(),
            );
            let _ = fs::write(p, serde_json::to_string(&map).unwrap_or_default());
        }

        emit_assistant_text(&mut stdout, &log.join("\n"));
        emit_result(&mut stdout);
        stdout.flush().ok();

        // Honor exit_code action: leave the loop and exit with the code.
        if let Some(code) = ctx.exit_requested.take() {
            std::process::exit(code);
        }
    }
}

#[cfg(unix)]
fn install_sigint_handler() {
    // Bare libc signal install. Set the atomic; main loop polls it. We do
    // not use `signal_hook` here to avoid pulling in another dep into the
    // shim binary.
    extern "C" fn handler(_: libc::c_int) {
        INTERRUPTED.store(true, Ordering::SeqCst);
    }
    unsafe {
        libc::signal(libc::SIGINT, handler as libc::sighandler_t);
    }
    // `Arc` import sees use only here on some build configs; reference it
    // explicitly so the build doesn't warn when configured down to nothing.
    let _: Option<Arc<()>> = None;
}

#[cfg(not(unix))]
fn install_sigint_handler() {}
