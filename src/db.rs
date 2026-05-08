// SQLite persistence: schema, migrations, query helpers.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json;
use std::path::Path;

use crate::session::{Session, SessionMode, SessionState};

pub struct Database {
    conn: Connection,
}

// --- Row structs ---

#[derive(Debug)]
pub struct StateTransition {
    pub id: i64,
    pub session_id: String,
    pub from_state: String,
    pub to_state: String,
    pub reason: Option<String>,
    pub triggered_by: Option<String>,
    pub at: String,
}

#[derive(Debug)]
pub struct PermissionDecision {
    pub id: i64,
    pub session_id: String,
    pub request: String,
    pub decision: String,
    pub decided_by: Option<String>,
    pub reason: Option<String>,
    pub at: String,
}

#[derive(Debug)]
pub struct TaskGraph {
    pub run_id: String,
    pub user_prompt: String,
    pub graph: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug)]
pub struct TaskNode {
    pub id: String,
    pub run_id: String,
    pub description: String,
    pub depends_on: Option<String>,
    pub status: String,
    pub session_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug)]
pub struct Review {
    pub id: i64,
    pub session_id: String,
    pub diff_hash: String,
    pub comments: Option<String>,
    pub overall: Option<String>,
    pub status: String,
    pub created_at: String,
    pub submitted_at: Option<String>,
}

// --- Helpers for SessionState serialization ---

fn state_to_name(state: &SessionState) -> &'static str {
    match state {
        SessionState::Running => "Running",
        SessionState::Blocked { .. } => "Blocked",
        SessionState::AwaitingReview { .. } => "AwaitingReview",
        SessionState::Done { .. } => "Done",
        SessionState::Failed { .. } => "Failed",
    }
}

fn state_to_data(state: &SessionState) -> Option<String> {
    match state {
        SessionState::Running => None,
        other => serde_json::to_string(other).ok(),
    }
}

fn state_from_db(name: &str, data: Option<&str>) -> Result<SessionState> {
    match name {
        "Running" => Ok(SessionState::Running),
        other => {
            let json = data.with_context(|| format!("state '{}' requires state_data", other))?;
            serde_json::from_str(json)
                .with_context(|| format!("failed to deserialize state '{}'", other))
        }
    }
}

fn mode_to_str(mode: SessionMode) -> &'static str {
    match mode {
        SessionMode::Watch => "Watch",
        SessionMode::Control => "Control",
    }
}

fn mode_from_str(s: &str) -> Result<SessionMode> {
    match s {
        "Watch" => Ok(SessionMode::Watch),
        "Control" => Ok(SessionMode::Control),
        other => anyhow::bail!("unknown session mode: {}", other),
    }
}

// --- Database impl ---

impl Database {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let db = Database { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                task TEXT NOT NULL,
                worktree_path TEXT NOT NULL,
                branch TEXT NOT NULL,
                base_commit TEXT NOT NULL,
                tmux_session TEXT NOT NULL,
                state TEXT NOT NULL DEFAULT 'Running',
                state_data TEXT,
                mode TEXT NOT NULL DEFAULT 'Watch',
                model TEXT NOT NULL DEFAULT 'sonnet',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                ended_at TEXT
            );

            CREATE TABLE IF NOT EXISTS state_transitions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                from_state TEXT NOT NULL,
                to_state TEXT NOT NULL,
                reason TEXT,
                triggered_by TEXT,
                at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS permission_decisions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                request TEXT NOT NULL,
                decision TEXT NOT NULL,
                decided_by TEXT,
                reason TEXT,
                at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS task_graphs (
                run_id TEXT PRIMARY KEY,
                user_prompt TEXT NOT NULL,
                graph TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS task_nodes (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL REFERENCES task_graphs(run_id),
                description TEXT NOT NULL,
                depends_on TEXT,
                status TEXT NOT NULL DEFAULT 'pending',
                session_id TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS reviews (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                diff_hash TEXT NOT NULL,
                comments TEXT,
                overall TEXT,
                status TEXT NOT NULL DEFAULT 'draft',
                created_at TEXT NOT NULL,
                submitted_at TEXT
            );
            ",
        )?;
        // m003: claude_session_id (used by R restart to resume a worker's
        // claude conversation). Idempotent — guarded by pragma table_info.
        let has_csid: bool = self
            .conn
            .prepare("SELECT 1 FROM pragma_table_info('sessions') WHERE name='claude_session_id'")?
            .query([])?
            .next()?
            .is_some();
        if !has_csid {
            self.conn.execute_batch(
                "ALTER TABLE sessions ADD COLUMN claude_session_id TEXT;",
            )?;
        }
        Ok(())
    }

    pub fn update_claude_session_id(&self, id: &str, claude_session_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET claude_session_id = ?1 WHERE id = ?2",
            params![claude_session_id, id],
        )?;
        Ok(())
    }

    // --- Sessions ---

    pub fn insert_session(&self, session: &Session) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sessions (id, name, task, worktree_path, branch, base_commit, tmux_session,
                state, state_data, mode, model, created_at, updated_at, ended_at, claude_session_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                session.id,
                session.name,
                session.task,
                session.worktree_path,
                session.branch,
                session.base_commit,
                session.tmux_session,
                state_to_name(&session.state),
                state_to_data(&session.state),
                mode_to_str(session.mode),
                session.model,
                session.created_at.to_rfc3339(),
                session.updated_at.to_rfc3339(),
                session.ended_at.map(|t| t.to_rfc3339()),
                session.claude_session_id,
            ],
        )?;
        Ok(())
    }

    /// Hard-delete a session and its dependent rows. Cascades manually because
    /// the schema does not declare ON DELETE CASCADE.
    pub fn delete_session(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM state_transitions WHERE session_id = ?1",
            params![id],
        )?;
        self.conn.execute(
            "DELETE FROM permission_decisions WHERE session_id = ?1",
            params![id],
        )?;
        self.conn.execute(
            "DELETE FROM reviews WHERE session_id = ?1",
            params![id],
        )?;
        self.conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn get_session(&self, id: &str) -> Result<Session> {
        self.conn
            .query_row(
                "SELECT id, name, task, worktree_path, branch, base_commit, tmux_session,
                        state, state_data, mode, model, created_at, updated_at, ended_at,
                        claude_session_id
                 FROM sessions WHERE id = ?1",
                params![id],
                row_to_session,
            )
            .with_context(|| format!("session '{}' not found", id))
    }

    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, task, worktree_path, branch, base_commit, tmux_session,
                    state, state_data, mode, model, created_at, updated_at, ended_at,
                    claude_session_id
             FROM sessions ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([], row_to_session)?;
        rows.map(|r| r.map_err(anyhow::Error::from)).collect()
    }

    pub fn update_session_state(&self, id: &str, state: &SessionState) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let ended_at: Option<String> = if matches!(state, SessionState::Done { .. } | SessionState::Failed { .. }) {
            Some(now.clone())
        } else {
            None
        };
        self.conn.execute(
            "UPDATE sessions SET state = ?1, state_data = ?2, updated_at = ?3, ended_at = COALESCE(ended_at, ?4)
             WHERE id = ?5",
            params![
                state_to_name(state),
                state_to_data(state),
                now,
                ended_at,
                id,
            ],
        )?;
        Ok(())
    }

    pub fn update_session_mode(&self, id: &str, mode: SessionMode) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE sessions SET mode = ?1, updated_at = ?2 WHERE id = ?3",
            params![mode_to_str(mode), now, id],
        )?;
        Ok(())
    }

    // --- State transitions ---

    pub fn record_transition(
        &self,
        session_id: &str,
        from: &str,
        to: &str,
        reason: &str,
        triggered_by: &str,
    ) -> Result<()> {
        let at = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO state_transitions (session_id, from_state, to_state, reason, triggered_by, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![session_id, from, to, reason, triggered_by, at],
        )?;
        Ok(())
    }

    pub fn get_transitions(&self, session_id: &str) -> Result<Vec<StateTransition>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, from_state, to_state, reason, triggered_by, at
             FROM state_transitions WHERE session_id = ?1 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(StateTransition {
                id: row.get(0)?,
                session_id: row.get(1)?,
                from_state: row.get(2)?,
                to_state: row.get(3)?,
                reason: row.get(4)?,
                triggered_by: row.get(5)?,
                at: row.get(6)?,
            })
        })?;
        rows.map(|r| r.map_err(anyhow::Error::from)).collect()
    }

    // --- Permission decisions ---

    pub fn record_permission(
        &self,
        session_id: &str,
        request: &str,
        decision: &str,
        decided_by: &str,
        reason: &str,
    ) -> Result<()> {
        let at = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO permission_decisions (session_id, request, decision, decided_by, reason, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![session_id, request, decision, decided_by, reason, at],
        )?;
        Ok(())
    }

    pub fn get_permissions(&self, session_id: &str) -> Result<Vec<PermissionDecision>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, request, decision, decided_by, reason, at
             FROM permission_decisions WHERE session_id = ?1 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(PermissionDecision {
                id: row.get(0)?,
                session_id: row.get(1)?,
                request: row.get(2)?,
                decision: row.get(3)?,
                decided_by: row.get(4)?,
                reason: row.get(5)?,
                at: row.get(6)?,
            })
        })?;
        rows.map(|r| r.map_err(anyhow::Error::from)).collect()
    }

    // --- Task graphs ---

    pub fn upsert_task_graph(&self, run_id: &str, user_prompt: &str, graph_json: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO task_graphs (run_id, user_prompt, graph, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(run_id) DO UPDATE SET
                user_prompt = excluded.user_prompt,
                graph = excluded.graph,
                updated_at = excluded.updated_at",
            params![run_id, user_prompt, graph_json, now],
        )?;
        Ok(())
    }

    pub fn get_task_graph(&self, run_id: &str) -> Result<Option<TaskGraph>> {
        self.conn
            .query_row(
                "SELECT run_id, user_prompt, graph, created_at, updated_at
                 FROM task_graphs WHERE run_id = ?1",
                params![run_id],
                |row| {
                    Ok(TaskGraph {
                        run_id: row.get(0)?,
                        user_prompt: row.get(1)?,
                        graph: row.get(2)?,
                        created_at: row.get(3)?,
                        updated_at: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(anyhow::Error::from)
    }

    // --- Task nodes ---

    pub fn insert_task_node(&self, node: &TaskNode) -> Result<()> {
        self.conn.execute(
            "INSERT INTO task_nodes (id, run_id, description, depends_on, status, session_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                node.id,
                node.run_id,
                node.description,
                node.depends_on,
                node.status,
                node.session_id,
                node.created_at,
                node.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn update_task_node_status(&self, id: &str, status: &str, session_id: Option<&str>) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE task_nodes SET status = ?1, session_id = COALESCE(?2, session_id), updated_at = ?3
             WHERE id = ?4",
            params![status, session_id, now, id],
        )?;
        Ok(())
    }

    pub fn get_task_nodes(&self, run_id: &str) -> Result<Vec<TaskNode>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, description, depends_on, status, session_id, created_at, updated_at
             FROM task_nodes WHERE run_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![run_id], |row| {
            Ok(TaskNode {
                id: row.get(0)?,
                run_id: row.get(1)?,
                description: row.get(2)?,
                depends_on: row.get(3)?,
                status: row.get(4)?,
                session_id: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })?;
        rows.map(|r| r.map_err(anyhow::Error::from)).collect()
    }

    // --- Reviews ---

    pub fn insert_review(&self, session_id: &str, diff_hash: &str) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO reviews (session_id, diff_hash, status, created_at)
             VALUES (?1, ?2, 'draft', ?3)",
            params![session_id, diff_hash, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update_review(&self, id: i64, comments: &str, overall: &str, status: &str) -> Result<()> {
        let submitted_at: Option<String> = if status == "submitted" {
            Some(Utc::now().to_rfc3339())
        } else {
            None
        };
        self.conn.execute(
            "UPDATE reviews SET comments = ?1, overall = ?2, status = ?3,
                submitted_at = COALESCE(?4, submitted_at)
             WHERE id = ?5",
            params![comments, overall, status, submitted_at, id],
        )?;
        Ok(())
    }

    pub fn get_review(&self, session_id: &str) -> Result<Option<Review>> {
        self.conn
            .query_row(
                "SELECT id, session_id, diff_hash, comments, overall, status, created_at, submitted_at
                 FROM reviews WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
                params![session_id],
                |row| {
                    Ok(Review {
                        id: row.get(0)?,
                        session_id: row.get(1)?,
                        diff_hash: row.get(2)?,
                        comments: row.get(3)?,
                        overall: row.get(4)?,
                        status: row.get(5)?,
                        created_at: row.get(6)?,
                        submitted_at: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(anyhow::Error::from)
    }
}

// --- Private row mapper ---

fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    let state_name: String = row.get(7)?;
    let state_data: Option<String> = row.get(8)?;
    let mode_str: String = row.get(9)?;
    let created_at_str: String = row.get(11)?;
    let updated_at_str: String = row.get(12)?;
    let ended_at_str: Option<String> = row.get(13)?;

    let state = state_from_db(&state_name, state_data.as_deref())
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(DbError(e.to_string()))))?;
    let mode = mode_from_str(&mode_str)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(9, rusqlite::types::Type::Text, Box::new(DbError(e.to_string()))))?;
    let created_at = DateTime::parse_from_rfc3339(&created_at_str)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(11, rusqlite::types::Type::Text, Box::new(DbError(e.to_string()))))?;
    let updated_at = DateTime::parse_from_rfc3339(&updated_at_str)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(12, rusqlite::types::Type::Text, Box::new(DbError(e.to_string()))))?;
    let ended_at = ended_at_str
        .map(|s| DateTime::parse_from_rfc3339(&s).map(|dt| dt.with_timezone(&Utc)))
        .transpose()
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(13, rusqlite::types::Type::Text, Box::new(DbError(e.to_string()))))?;

    let claude_session_id: Option<String> = row.get(14).ok();
    Ok(Session {
        id: row.get(0)?,
        name: row.get(1)?,
        task: row.get(2)?,
        worktree_path: row.get(3)?,
        branch: row.get(4)?,
        base_commit: row.get(5)?,
        tmux_session: row.get(6)?,
        state,
        mode,
        model: row.get(10)?,
        created_at,
        updated_at,
        ended_at,
        claude_session_id,
    })
}

// Small wrapper to make arbitrary errors compatible with rusqlite's Error type
#[derive(Debug)]
struct DbError(String);

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for DbError {}

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_db() -> Database {
        let dir = tempdir().unwrap();
        Database::open(dir.path().join("test.db")).unwrap()
    }

    fn sample_session() -> Session {
        Session::new(
            "agent-1".to_string(),
            "write some code".to_string(),
            "/tmp/wt/agent-1".to_string(),
            "feature/agent-1".to_string(),
            "abc123".to_string(),
            "sonnet".to_string(),
        )
    }

    #[test]
    fn tables_exist_after_open() {
        let dir = tempdir().unwrap();
        let db = Database::open(dir.path().join("test.db")).unwrap();
        let tables: Vec<String> = {
            let mut stmt = db
                .conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert!(tables.contains(&"sessions".to_string()));
        assert!(tables.contains(&"state_transitions".to_string()));
        assert!(tables.contains(&"permission_decisions".to_string()));
        assert!(tables.contains(&"task_graphs".to_string()));
        assert!(tables.contains(&"task_nodes".to_string()));
        assert!(tables.contains(&"reviews".to_string()));
    }

    #[test]
    fn insert_and_get_session() {
        let db = make_db();
        let s = sample_session();
        let id = s.id.clone();
        db.insert_session(&s).unwrap();

        let got = db.get_session(&id).unwrap();
        assert_eq!(got.id, s.id);
        assert_eq!(got.name, s.name);
        assert_eq!(got.task, s.task);
        assert_eq!(got.worktree_path, s.worktree_path);
        assert_eq!(got.branch, s.branch);
        assert_eq!(got.base_commit, s.base_commit);
        assert_eq!(got.tmux_session, s.tmux_session);
        assert_eq!(got.model, s.model);
        assert_eq!(got.state, SessionState::Running);
        assert_eq!(got.mode, SessionMode::Watch);
        assert!(got.ended_at.is_none());
    }

    #[test]
    fn update_session_state() {
        let db = make_db();
        let s = sample_session();
        let id = s.id.clone();
        db.insert_session(&s).unwrap();

        let new_state = SessionState::Done { summary: "all done".to_string() };
        db.update_session_state(&id, &new_state).unwrap();

        let got = db.get_session(&id).unwrap();
        assert_eq!(got.state, new_state);
        assert!(got.ended_at.is_some());
    }

    #[test]
    fn update_session_state_blocked() {
        let db = make_db();
        let s = sample_session();
        let id = s.id.clone();
        db.insert_session(&s).unwrap();

        let new_state = SessionState::Blocked {
            kind: crate::session::BlockKind::Permission,
            reason: "needs write access".to_string(),
        };
        db.update_session_state(&id, &new_state).unwrap();

        let got = db.get_session(&id).unwrap();
        assert_eq!(got.state, new_state);
        assert!(got.ended_at.is_none());
    }

    #[test]
    fn record_and_get_transitions() {
        let db = make_db();
        let s = sample_session();
        let id = s.id.clone();
        db.insert_session(&s).unwrap();

        db.record_transition(&id, "Running", "Blocked", "needs permission", "system").unwrap();
        db.record_transition(&id, "Blocked", "Running", "permission granted", "user").unwrap();

        let transitions = db.get_transitions(&id).unwrap();
        assert_eq!(transitions.len(), 2);
        assert_eq!(transitions[0].from_state, "Running");
        assert_eq!(transitions[0].to_state, "Blocked");
        assert_eq!(transitions[0].reason.as_deref(), Some("needs permission"));
        assert_eq!(transitions[1].from_state, "Blocked");
        assert_eq!(transitions[1].to_state, "Running");
    }

    #[test]
    fn record_and_get_permissions() {
        let db = make_db();
        let s = sample_session();
        let id = s.id.clone();
        db.insert_session(&s).unwrap();

        db.record_permission(&id, r#"{"tool":"write","path":"/etc"}"#, "allow", "orc", "safe operation").unwrap();
        db.record_permission(&id, r#"{"tool":"exec","cmd":"rm -rf"}"#, "deny", "orc", "dangerous").unwrap();

        let perms = db.get_permissions(&id).unwrap();
        assert_eq!(perms.len(), 2);
        assert_eq!(perms[0].decision, "allow");
        assert_eq!(perms[1].decision, "deny");
    }

    #[test]
    fn upsert_and_get_task_graph() {
        let db = make_db();
        let graph_json = r#"{"nodes":[]}"#;
        db.upsert_task_graph("run-1", "build a web app", graph_json).unwrap();

        let got = db.get_task_graph("run-1").unwrap().unwrap();
        assert_eq!(got.run_id, "run-1");
        assert_eq!(got.user_prompt, "build a web app");
        assert_eq!(got.graph, graph_json);

        // Upsert update
        let updated_graph = r#"{"nodes":["a","b"]}"#;
        db.upsert_task_graph("run-1", "build a web app", updated_graph).unwrap();
        let got2 = db.get_task_graph("run-1").unwrap().unwrap();
        assert_eq!(got2.graph, updated_graph);

        // Non-existent
        assert!(db.get_task_graph("run-nonexistent").unwrap().is_none());
    }

    #[test]
    fn insert_and_update_task_nodes() {
        let db = make_db();
        db.upsert_task_graph("run-1", "prompt", "{}").unwrap();

        let now = Utc::now().to_rfc3339();
        let node = TaskNode {
            id: "node-1".to_string(),
            run_id: "run-1".to_string(),
            description: "implement feature".to_string(),
            depends_on: None,
            status: "pending".to_string(),
            session_id: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        db.insert_task_node(&node).unwrap();

        let nodes = db.get_task_nodes("run-1").unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].status, "pending");

        db.update_task_node_status("node-1", "running", Some("sess-abc")).unwrap();
        let nodes2 = db.get_task_nodes("run-1").unwrap();
        assert_eq!(nodes2[0].status, "running");
        assert_eq!(nodes2[0].session_id.as_deref(), Some("sess-abc"));
    }

    #[test]
    fn insert_and_update_review() {
        let db = make_db();
        let s = sample_session();
        let sid = s.id.clone();
        db.insert_session(&s).unwrap();

        let review_id = db.insert_review(&sid, "deadbeef1234").unwrap();
        assert!(review_id > 0);

        let r = db.get_review(&sid).unwrap().unwrap();
        assert_eq!(r.diff_hash, "deadbeef1234");
        assert_eq!(r.status, "draft");
        assert!(r.comments.is_none());

        db.update_review(review_id, r#"["LGTM"]"#, "approve", "submitted").unwrap();
        let r2 = db.get_review(&sid).unwrap().unwrap();
        assert_eq!(r2.status, "submitted");
        assert_eq!(r2.comments.as_deref(), Some(r#"["LGTM"]"#));
        assert_eq!(r2.overall.as_deref(), Some("approve"));
        assert!(r2.submitted_at.is_some());
    }

    #[test]
    fn list_sessions_multiple() {
        let db = make_db();

        let s1 = Session::new("a1".to_string(), "task1".to_string(), "/wt/1".to_string(), "b1".to_string(), "c1".to_string(), "sonnet".to_string());
        let s2 = Session::new("a2".to_string(), "task2".to_string(), "/wt/2".to_string(), "b2".to_string(), "c2".to_string(), "opus".to_string());
        let s3 = Session::new("a3".to_string(), "task3".to_string(), "/wt/3".to_string(), "b3".to_string(), "c3".to_string(), "haiku".to_string());

        db.insert_session(&s1).unwrap();
        db.insert_session(&s2).unwrap();
        db.insert_session(&s3).unwrap();

        let sessions = db.list_sessions().unwrap();
        assert_eq!(sessions.len(), 3);
        let names: Vec<&str> = sessions.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"a1"));
        assert!(names.contains(&"a2"));
        assert!(names.contains(&"a3"));
    }
}
