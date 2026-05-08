mod app;
mod db;
mod hooks;
mod mcp;
mod orc;
mod policy;
mod review;
mod session;
mod state;
mod tmux;
mod ui;
mod worktree;

use anyhow::{bail, Result};
use clap::Parser;
use std::process::Command;

#[derive(Parser)]
#[command(name = "orc", about = "Parallel Claude Code agents with an intelligent orchestrator")]
struct Cli {
    /// Project directory
    #[arg(short, long, default_value = ".")]
    project: String,
}

fn check_dependencies() -> Result<()> {
    for dep in ["git", "claude", "tmux"] {
        let status = Command::new("which")
            .arg(dep)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => {}
            _ => bail!("required dependency not found: {dep}"),
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    check_dependencies()?;

    let _app = app::App::new(&cli.project);

    println!("orc v2 starting");
    Ok(())
}
