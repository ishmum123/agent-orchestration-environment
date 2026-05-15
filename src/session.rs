// Session lifecycle: state machine, block tracking, operating mode.

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    Running,
    Blocked { kind: BlockKind, reason: String },
    AwaitingReview { diff_hash: String },
    Done { summary: String },
    Failed { reason: String },
    /// Claude reported its account is over the rate limit. The session's
    /// claude child has been killed. `resets_at` is the wall-clock the
    /// bucket lifts; `bucket` names it (e.g. "five_hour"). When
    /// `resets_at` is `Some` and within auto-resume range, the quota
    /// scheduler will respawn the worker once the time passes. When
    /// `None`, the reset was further out than the auto-resume cap and
    /// the user must `R` restart manually.
    WaitingForQuota {
        resets_at: Option<DateTime<Utc>>,
        bucket: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockKind {
    Permission,   // waiting for permission decision
    OrcDecision,  // orc needs to decide something
    UserInput,    // user needs to answer
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionMode {
    Watch,
    Control,
}

/// Snapshot of a paused process's quota state. Shared shape between
/// `SessionState::WaitingForQuota` (for workers) and `OrcView` (for the
/// orchestrator brain). `resets_at == None` means the reset was past the
/// auto-resume cap; the user must restart manually.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaPause {
    pub resets_at: Option<DateTime<Utc>>,
    pub bucket: String,
}

/// Maximum wait the scheduler will auto-resume across. Resets further out
/// than this are recorded with `resets_at = None` and require a manual
/// `R` restart.
pub const QUOTA_AUTORESUME_CAP_SECS: i64 = 5 * 60 * 60;

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub task: String,
    pub worktree_path: String,
    pub branch: String,
    pub base_commit: String,
    pub tmux_session: String,
    pub state: SessionState,
    pub mode: SessionMode,
    pub model: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub claude_session_id: Option<String>,
}

#[derive(Debug, Clone)]
pub enum SessionEvent {
    Started,
    PermissionRequested { request: String },
    PermissionResolved,
    OrcDecisionNeeded { reason: String },
    OrcDecisionMade,
    UserInputNeeded { question: String },
    UserInputReceived,
    WorkCompleted { diff_hash: String },
    ReviewSubmitted { approved: bool, feedback: Option<String> },
    Finished { summary: String },
    Errored { reason: String },
    Restarted,
    /// Worker tripped the account quota. `resets_at` is `Some` if the
    /// scheduler should auto-resume, `None` otherwise (manual restart only).
    QuotaExceeded {
        resets_at: Option<DateTime<Utc>>,
        bucket: String,
    },
    /// Scheduler respawned a paused worker and it came back online.
    QuotaResumed,
}

pub fn transition(state: &SessionState, event: &SessionEvent) -> Result<SessionState> {
    match (state, event) {
        // Running transitions
        (SessionState::Running, SessionEvent::PermissionRequested { request }) => {
            Ok(SessionState::Blocked {
                kind: BlockKind::Permission,
                reason: request.clone(),
            })
        }
        (SessionState::Running, SessionEvent::OrcDecisionNeeded { reason }) => {
            Ok(SessionState::Blocked {
                kind: BlockKind::OrcDecision,
                reason: reason.clone(),
            })
        }
        (SessionState::Running, SessionEvent::UserInputNeeded { question }) => {
            Ok(SessionState::Blocked {
                kind: BlockKind::UserInput,
                reason: question.clone(),
            })
        }
        (SessionState::Running, SessionEvent::WorkCompleted { diff_hash }) => {
            Ok(SessionState::AwaitingReview {
                diff_hash: diff_hash.clone(),
            })
        }
        (SessionState::Running, SessionEvent::Finished { summary }) => {
            Ok(SessionState::Done {
                summary: summary.clone(),
            })
        }
        (SessionState::Running, SessionEvent::Errored { reason }) => {
            Ok(SessionState::Failed {
                reason: reason.clone(),
            })
        }

        // Blocked transitions — any BlockKind unblocks to Running
        (SessionState::Blocked { .. }, SessionEvent::PermissionResolved) => {
            Ok(SessionState::Running)
        }
        (SessionState::Blocked { .. }, SessionEvent::OrcDecisionMade) => {
            Ok(SessionState::Running)
        }
        (SessionState::Blocked { .. }, SessionEvent::UserInputReceived) => {
            Ok(SessionState::Running)
        }

        // AwaitingReview transitions
        (SessionState::AwaitingReview { .. }, SessionEvent::ReviewSubmitted { approved: true, .. }) => {
            Ok(SessionState::Done {
                summary: "approved".to_string(),
            })
        }
        (SessionState::AwaitingReview { .. }, SessionEvent::ReviewSubmitted { approved: false, .. }) => {
            Ok(SessionState::Running)
        }
        (SessionState::AwaitingReview { .. }, SessionEvent::Errored { reason }) => {
            Ok(SessionState::Failed {
                reason: reason.clone(),
            })
        }

        // Restart from any non-terminal state resets to Running. Used by
        // `respawn_session` after killing a stuck worker — the underlying
        // worktree is preserved, the conversation is fresh.
        (SessionState::Running, SessionEvent::Restarted) => Ok(SessionState::Running),
        (SessionState::Blocked { .. }, SessionEvent::Restarted) => Ok(SessionState::Running),
        (SessionState::AwaitingReview { .. }, SessionEvent::Restarted) => {
            Ok(SessionState::Running)
        }

        // Quota: Running → WaitingForQuota → Running.
        (
            SessionState::Running,
            SessionEvent::QuotaExceeded { resets_at, bucket },
        ) => Ok(SessionState::WaitingForQuota {
            resets_at: *resets_at,
            bucket: bucket.clone(),
        }),
        (SessionState::WaitingForQuota { .. }, SessionEvent::QuotaResumed) => {
            Ok(SessionState::Running)
        }
        // Manual restart from quota also returns to Running.
        (SessionState::WaitingForQuota { .. }, SessionEvent::Restarted) => {
            Ok(SessionState::Running)
        }

        // Terminal states — no transitions allowed
        (SessionState::Done { .. }, _) => {
            bail!("invalid transition: Done is a terminal state")
        }
        (SessionState::Failed { .. }, SessionEvent::Restarted) => {
            Ok(SessionState::Running)
        }
        (SessionState::Failed { .. }, _) => {
            bail!("invalid transition: Failed is a terminal state")
        }

        // Everything else is invalid
        _ => {
            let state_name = match state {
                SessionState::Running => "Running",
                SessionState::Blocked { .. } => "Blocked",
                SessionState::AwaitingReview { .. } => "AwaitingReview",
                SessionState::Done { .. } => "Done",
                SessionState::Failed { .. } => "Failed",
                SessionState::WaitingForQuota { .. } => "WaitingForQuota",
            };
            let event_name = match event {
                SessionEvent::Started => "Started",
                SessionEvent::PermissionRequested { .. } => "PermissionRequested",
                SessionEvent::PermissionResolved => "PermissionResolved",
                SessionEvent::OrcDecisionNeeded { .. } => "OrcDecisionNeeded",
                SessionEvent::OrcDecisionMade => "OrcDecisionMade",
                SessionEvent::UserInputNeeded { .. } => "UserInputNeeded",
                SessionEvent::UserInputReceived => "UserInputReceived",
                SessionEvent::WorkCompleted { .. } => "WorkCompleted",
                SessionEvent::ReviewSubmitted { .. } => "ReviewSubmitted",
                SessionEvent::Finished { .. } => "Finished",
                SessionEvent::Errored { .. } => "Errored",
                SessionEvent::Restarted => "Restarted",
                SessionEvent::QuotaExceeded { .. } => "QuotaExceeded",
                SessionEvent::QuotaResumed => "QuotaResumed",
            };
            bail!("invalid transition: {} + {}", state_name, event_name)
        }
    }
}

impl Session {
    pub fn new(
        name: String,
        task: String,
        worktree_path: String,
        branch: String,
        base_commit: String,
        model: String,
    ) -> Self {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let tmux_session = format!("orc-{}", name);
        Session {
            id,
            name,
            task,
            worktree_path,
            branch,
            base_commit,
            tmux_session,
            state: SessionState::Running,
            mode: SessionMode::Watch,
            model,
            created_at: now,
            updated_at: now,
            ended_at: None,
            claude_session_id: None,
        }
    }

    pub fn apply_event(&mut self, event: &SessionEvent) -> Result<()> {
        self.state = transition(&self.state, event)?;
        self.updated_at = Utc::now();
        if matches!(self.state, SessionState::Done { .. } | SessionState::Failed { .. }) {
            self.ended_at = Some(Utc::now());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn running_session() -> Session {
        Session::new(
            "test".to_string(),
            "do something".to_string(),
            "/tmp/worktree".to_string(),
            "feature/test".to_string(),
            "abc123".to_string(),
            "sonnet".to_string(),
        )
    }

    // --- Valid transitions ---

    #[test]
    fn running_permission_requested_blocks_permission() {
        let result = transition(
            &SessionState::Running,
            &SessionEvent::PermissionRequested { request: "write /etc".to_string() },
        )
        .unwrap();
        assert_eq!(
            result,
            SessionState::Blocked { kind: BlockKind::Permission, reason: "write /etc".to_string() }
        );
    }

    #[test]
    fn running_orc_decision_needed_blocks_orc_decision() {
        let result = transition(
            &SessionState::Running,
            &SessionEvent::OrcDecisionNeeded { reason: "ambiguous spec".to_string() },
        )
        .unwrap();
        assert_eq!(
            result,
            SessionState::Blocked { kind: BlockKind::OrcDecision, reason: "ambiguous spec".to_string() }
        );
    }

    #[test]
    fn running_user_input_needed_blocks_user_input() {
        let result = transition(
            &SessionState::Running,
            &SessionEvent::UserInputNeeded { question: "which branch?".to_string() },
        )
        .unwrap();
        assert_eq!(
            result,
            SessionState::Blocked { kind: BlockKind::UserInput, reason: "which branch?".to_string() }
        );
    }

    #[test]
    fn running_work_completed_awaiting_review() {
        let result = transition(
            &SessionState::Running,
            &SessionEvent::WorkCompleted { diff_hash: "deadbeef".to_string() },
        )
        .unwrap();
        assert_eq!(result, SessionState::AwaitingReview { diff_hash: "deadbeef".to_string() });
    }

    #[test]
    fn running_finished_done() {
        let result = transition(
            &SessionState::Running,
            &SessionEvent::Finished { summary: "all done".to_string() },
        )
        .unwrap();
        assert_eq!(result, SessionState::Done { summary: "all done".to_string() });
    }

    #[test]
    fn running_errored_failed() {
        let result = transition(
            &SessionState::Running,
            &SessionEvent::Errored { reason: "process crashed".to_string() },
        )
        .unwrap();
        assert_eq!(result, SessionState::Failed { reason: "process crashed".to_string() });
    }

    #[test]
    fn blocked_permission_resolved_running() {
        let state = SessionState::Blocked { kind: BlockKind::Permission, reason: "r".to_string() };
        let result = transition(&state, &SessionEvent::PermissionResolved).unwrap();
        assert_eq!(result, SessionState::Running);
    }

    #[test]
    fn blocked_orc_decision_made_running() {
        let state = SessionState::Blocked { kind: BlockKind::OrcDecision, reason: "r".to_string() };
        let result = transition(&state, &SessionEvent::OrcDecisionMade).unwrap();
        assert_eq!(result, SessionState::Running);
    }

    #[test]
    fn blocked_user_input_received_running() {
        let state = SessionState::Blocked { kind: BlockKind::UserInput, reason: "r".to_string() };
        let result = transition(&state, &SessionEvent::UserInputReceived).unwrap();
        assert_eq!(result, SessionState::Running);
    }

    #[test]
    fn awaiting_review_approved_done() {
        let state = SessionState::AwaitingReview { diff_hash: "abc".to_string() };
        let result = transition(&state, &SessionEvent::ReviewSubmitted { approved: true, feedback: None }).unwrap();
        assert_eq!(result, SessionState::Done { summary: "approved".to_string() });
    }

    #[test]
    fn awaiting_review_rejected_running() {
        let state = SessionState::AwaitingReview { diff_hash: "abc".to_string() };
        let result = transition(
            &state,
            &SessionEvent::ReviewSubmitted { approved: false, feedback: Some("needs work".to_string()) },
        )
        .unwrap();
        assert_eq!(result, SessionState::Running);
    }

    #[test]
    fn awaiting_review_errored_failed() {
        let state = SessionState::AwaitingReview { diff_hash: "abc".to_string() };
        let result = transition(&state, &SessionEvent::Errored { reason: "timeout".to_string() }).unwrap();
        assert_eq!(result, SessionState::Failed { reason: "timeout".to_string() });
    }

    // --- Cannot transition from Done ---

    #[test]
    fn done_rejects_all_events() {
        let state = SessionState::Done { summary: "done".to_string() };
        let events = vec![
            SessionEvent::Started,
            SessionEvent::PermissionRequested { request: "r".to_string() },
            SessionEvent::PermissionResolved,
            SessionEvent::OrcDecisionNeeded { reason: "r".to_string() },
            SessionEvent::OrcDecisionMade,
            SessionEvent::UserInputNeeded { question: "q".to_string() },
            SessionEvent::UserInputReceived,
            SessionEvent::WorkCompleted { diff_hash: "h".to_string() },
            SessionEvent::ReviewSubmitted { approved: true, feedback: None },
            SessionEvent::Finished { summary: "s".to_string() },
            SessionEvent::Errored { reason: "e".to_string() },
        ];
        for event in &events {
            assert!(
                transition(&state, event).is_err(),
                "expected error for Done + {:?}",
                std::mem::discriminant(event)
            );
        }
    }

    // --- Cannot transition from Failed ---

    #[test]
    fn failed_rejects_all_events() {
        let state = SessionState::Failed { reason: "crash".to_string() };
        let events = vec![
            SessionEvent::Started,
            SessionEvent::PermissionRequested { request: "r".to_string() },
            SessionEvent::PermissionResolved,
            SessionEvent::OrcDecisionNeeded { reason: "r".to_string() },
            SessionEvent::OrcDecisionMade,
            SessionEvent::UserInputNeeded { question: "q".to_string() },
            SessionEvent::UserInputReceived,
            SessionEvent::WorkCompleted { diff_hash: "h".to_string() },
            SessionEvent::ReviewSubmitted { approved: true, feedback: None },
            SessionEvent::Finished { summary: "s".to_string() },
            SessionEvent::Errored { reason: "e".to_string() },
        ];
        for event in &events {
            assert!(
                transition(&state, event).is_err(),
                "expected error for Failed + {:?}",
                std::mem::discriminant(event)
            );
        }
    }

    // --- Invalid transitions ---

    #[test]
    fn running_permission_resolved_is_invalid() {
        let err = transition(&SessionState::Running, &SessionEvent::PermissionResolved)
            .unwrap_err();
        assert!(err.to_string().contains("Running"));
        assert!(err.to_string().contains("PermissionResolved"));
    }

    #[test]
    fn running_orc_decision_made_is_invalid() {
        let err = transition(&SessionState::Running, &SessionEvent::OrcDecisionMade).unwrap_err();
        assert!(err.to_string().contains("Running"));
        assert!(err.to_string().contains("OrcDecisionMade"));
    }

    #[test]
    fn running_user_input_received_is_invalid() {
        let err = transition(&SessionState::Running, &SessionEvent::UserInputReceived).unwrap_err();
        assert!(err.to_string().contains("Running"));
        assert!(err.to_string().contains("UserInputReceived"));
    }

    #[test]
    fn blocked_errored_is_invalid() {
        let state = SessionState::Blocked { kind: BlockKind::Permission, reason: "r".to_string() };
        let err = transition(&state, &SessionEvent::Errored { reason: "e".to_string() }).unwrap_err();
        assert!(err.to_string().contains("Blocked"));
    }

    // --- Session::new defaults ---

    #[test]
    fn session_new_sets_correct_defaults() {
        let s = Session::new(
            "myagent".to_string(),
            "write tests".to_string(),
            "/tmp/wt".to_string(),
            "feature/x".to_string(),
            "deadbeef".to_string(),
            "opus".to_string(),
        );
        assert!(!s.id.is_empty());
        assert_eq!(s.name, "myagent");
        assert_eq!(s.task, "write tests");
        assert_eq!(s.worktree_path, "/tmp/wt");
        assert_eq!(s.branch, "feature/x");
        assert_eq!(s.base_commit, "deadbeef");
        assert_eq!(s.tmux_session, "orc-myagent");
        assert_eq!(s.state, SessionState::Running);
        assert_eq!(s.mode, SessionMode::Watch);
        assert_eq!(s.model, "opus");
        assert!(s.ended_at.is_none());
        assert_eq!(s.created_at, s.updated_at);
    }

    // --- Session::apply_event timestamps ---

    #[test]
    fn apply_event_updates_updated_at() {
        let mut s = running_session();
        let before = s.updated_at;
        // Sleep briefly so timestamps differ
        std::thread::sleep(std::time::Duration::from_millis(2));
        s.apply_event(&SessionEvent::Finished { summary: "done".to_string() }).unwrap();
        assert!(s.updated_at >= before);
    }

    #[test]
    fn apply_event_sets_ended_at_on_done() {
        let mut s = running_session();
        s.apply_event(&SessionEvent::Finished { summary: "ok".to_string() }).unwrap();
        assert!(s.ended_at.is_some());
    }

    #[test]
    fn apply_event_sets_ended_at_on_failed() {
        let mut s = running_session();
        s.apply_event(&SessionEvent::Errored { reason: "crash".to_string() }).unwrap();
        assert!(s.ended_at.is_some());
    }

    #[test]
    fn apply_event_does_not_set_ended_at_when_not_terminal() {
        let mut s = running_session();
        s.apply_event(&SessionEvent::WorkCompleted { diff_hash: "h".to_string() }).unwrap();
        assert!(s.ended_at.is_none());
    }

    #[test]
    fn apply_event_returns_error_on_invalid_transition() {
        let mut s = running_session();
        s.apply_event(&SessionEvent::Finished { summary: "ok".to_string() }).unwrap();
        // Now in Done — any further event should error
        let err = s.apply_event(&SessionEvent::Started).unwrap_err();
        assert!(err.to_string().contains("terminal"));
    }
}
