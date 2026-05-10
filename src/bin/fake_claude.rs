// Deterministic stand-in for the `claude` CLI used during orc's TUI/UX
// harness runs. Real `claude` invocations cost tokens and produce
// non-deterministic output; this shim emits the same stream-json shape orc
// parses, with hard-coded responses, so harness runs are free and stable.
//
// Activated via the `ORC_CLAUDE_BIN` env var. Set it to the path of the
// installed `orc-fake-claude` binary before launching orc in tests.
//
// Args mirror real claude (-p, --model, --system-prompt, --mcp-config, etc.)
// and are mostly ignored. Behavior: emit init line, then on each stdin
// line emit one assistant turn + result. Exits 0 on stdin EOF.

use std::io::{self, BufRead, Write};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let model = arg_value(&args, "--model").unwrap_or_else(|| "fake".to_string());

    // Support `--version` so `orc doctor` and `which claude` smoke-checks pass.
    if args.iter().any(|a| a == "--version") {
        println!("orc-fake-claude 0.1.0 (stub)");
        return;
    }

    let session_id = uuid_v4();

    let mut stdout = io::stdout().lock();

    // Init line — orc captures session_id and model from this.
    let init = serde_json::json!({
        "type": "system",
        "subtype": "init",
        "session_id": session_id,
        "model": model,
    });
    writeln!(stdout, "{init}").ok();
    stdout.flush().ok();

    let stdin = io::stdin();
    let mut turn: u32 = 0;
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        turn += 1;

        // Echo the input back as a tiny acknowledgement. Keeps tests
        // deterministic but gives the parser real text to chew on.
        let user_text = serde_json::from_str::<serde_json::Value>(&line)
            .ok()
            .and_then(|v| {
                v.pointer("/message/content")
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| "<unparseable>".to_string());
        let snippet: String = user_text.chars().take(80).collect();

        let assistant = serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [
                    {
                        "type": "text",
                        "text": format!("[fake-claude turn {turn}] received: {snippet}"),
                    }
                ],
                "usage": {
                    "input_tokens": 100u64,
                    "output_tokens": 20u64,
                    "cache_read_input_tokens": 0u64,
                    "cache_creation_input_tokens": 0u64,
                }
            }
        });
        writeln!(stdout, "{assistant}").ok();

        let result = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "total_cost_usd": 0.0f64,
        });
        writeln!(stdout, "{result}").ok();
        stdout.flush().ok();
    }
}

/// Tiny v4-uuid producer — avoids pulling the uuid crate into this binary.
/// Format: 8-4-4-4-12 hex chars.
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
    bytes[6] = (bytes[6] & 0x0F) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3F) | 0x80; // variant
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
