// Session lifecycle: state machine, block tracking, operating mode.

use serde::{Deserialize, Serialize};

/// Top-level session state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    Idle,
    Planning,
    Executing,
    Reviewing,
    Paused,
    Done,
}

/// What the orchestrator is currently doing within a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockKind {
    Think,
    Spawn,
    Wait,
    Review,
    Report,
}

/// How the orchestrator interacts with the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionMode {
    /// Orchestrator decides everything, user observes.
    Autonomous,
    /// Orchestrator proposes, user approves each step.
    Supervised,
    /// User drives, orchestrator assists.
    Manual,
}
