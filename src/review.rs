// Diff parsing, review state, merge decisions.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use tokio::process::Command;

// ---------------------------------------------------------------------------
// Parsed diff structures
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParsedDiff {
    pub files: Vec<DiffFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffFile {
    pub path: String,
    pub hunks: Vec<Hunk>,
    pub additions: usize,
    pub deletions: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hunk {
    pub id: String,
    pub header: String,
    pub lines: Vec<DiffLine>,
    pub old_start: usize,
    pub new_start: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: LineKind,
    pub content: String,
    pub old_lineno: Option<usize>,
    pub new_lineno: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LineKind {
    Context,
    Add,
    Remove,
}

// ---------------------------------------------------------------------------
// Review state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct DiffCursor {
    pub file_idx: usize,
    pub line_idx: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftComment {
    pub file: String,
    pub line: usize,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct ReviewState {
    pub session_id: String,
    pub diff: ParsedDiff,
    pub diff_hash: String,
    pub cursor: DiffCursor,
    pub comments: Vec<DraftComment>,
    pub hunk_approvals: HashSet<String>,
    pub overall: Option<String>,
}

// ---------------------------------------------------------------------------
// Review payload (serializable for submission)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPayload {
    pub session_id: String,
    pub diff_hash: String,
    pub reviews: Vec<ReviewItem>,
    pub overall: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReviewItem {
    Comment {
        file: String,
        line: usize,
        body: String,
    },
    HunkApproval {
        file: String,
        hunk_id: String,
    },
}

// ---------------------------------------------------------------------------
// Diff computation
// ---------------------------------------------------------------------------

/// Compute the worker's diff against `base_commit`, including uncommitted and
/// untracked changes. Workers typically don't commit — they edit the worktree
/// directly. So a plain `git diff base..HEAD` would miss everything.
///
/// Strategy: `git add -N` makes untracked files visible to `git diff` as
/// intent-to-add (no content staged, just the path), then `git diff base` shows
/// every change the worktree has accumulated since the base commit.
pub async fn compute_diff(worktree_path: &str, base_commit: &str) -> Result<ParsedDiff> {
    // Make untracked files visible to `git diff` without staging their contents.
    let _ = Command::new("git")
        .args(["add", "-N", "."])
        .current_dir(worktree_path)
        .output()
        .await;

    let output = Command::new("git")
        .args(["diff", "--no-color", base_commit])
        .current_dir(worktree_path)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git diff failed: {stderr}");
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    parse_diff(&raw)
}

/// Parse unified diff text into structured `ParsedDiff`.
pub fn parse_diff(raw: &str) -> Result<ParsedDiff> {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut current_file: Option<DiffFile> = None;
    let mut current_hunk: Option<Hunk> = None;
    let mut hunk_counter: usize = 0;
    let mut old_lineno: usize = 0;
    let mut new_lineno: usize = 0;

    for line in raw.lines() {
        // File boundary
        if line.starts_with("diff --git ") {
            if let Some(ref mut file) = current_file {
                if let Some(hunk) = current_hunk.take() {
                    file.hunks.push(hunk);
                }
                files.push(file.clone());
            }
            current_file = Some(DiffFile {
                path: parse_file_path(line),
                hunks: Vec::new(),
                additions: 0,
                deletions: 0,
            });
            current_hunk = None;
            hunk_counter = 0;
            continue;
        }

        // Binary files — keep entry, skip content
        if line.starts_with("Binary files ") {
            continue;
        }

        // Skip metadata header lines
        if line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("old mode ")
            || line.starts_with("new mode ")
            || line.starts_with("new file mode ")
            || line.starts_with("deleted file mode ")
            || line.starts_with("similarity index ")
            || line.starts_with("rename from ")
            || line.starts_with("rename to ")
            || line.starts_with("copy from ")
            || line.starts_with("copy to ")
        {
            continue;
        }

        // Hunk header
        if line.starts_with("@@ ") {
            if let Some(ref mut file) = current_file {
                if let Some(hunk) = current_hunk.take() {
                    file.hunks.push(hunk);
                }
            }
            let (os, ns) = parse_hunk_header(line);
            old_lineno = os;
            new_lineno = ns;
            current_hunk = Some(Hunk {
                id: format!("h{hunk_counter}"),
                header: line.to_string(),
                lines: Vec::new(),
                old_start: os,
                new_start: ns,
            });
            hunk_counter += 1;
            continue;
        }

        // Diff content lines (only inside a hunk)
        if let Some(ref mut hunk) = current_hunk {
            if let Some(stripped) = line.strip_prefix('+') {
                hunk.lines.push(DiffLine {
                    kind: LineKind::Add,
                    content: stripped.to_string(),
                    old_lineno: None,
                    new_lineno: Some(new_lineno),
                });
                if let Some(ref mut file) = current_file {
                    file.additions += 1;
                }
                new_lineno += 1;
            } else if let Some(stripped) = line.strip_prefix('-') {
                hunk.lines.push(DiffLine {
                    kind: LineKind::Remove,
                    content: stripped.to_string(),
                    old_lineno: Some(old_lineno),
                    new_lineno: None,
                });
                if let Some(ref mut file) = current_file {
                    file.deletions += 1;
                }
                old_lineno += 1;
            } else if let Some(stripped) = line.strip_prefix(' ') {
                hunk.lines.push(DiffLine {
                    kind: LineKind::Context,
                    content: stripped.to_string(),
                    old_lineno: Some(old_lineno),
                    new_lineno: Some(new_lineno),
                });
                old_lineno += 1;
                new_lineno += 1;
            } else if line == "\\ No newline at end of file" {
                // Skip
            } else {
                // Empty context line (no leading space)
                hunk.lines.push(DiffLine {
                    kind: LineKind::Context,
                    content: line.to_string(),
                    old_lineno: Some(old_lineno),
                    new_lineno: Some(new_lineno),
                });
                old_lineno += 1;
                new_lineno += 1;
            }
        }
    }

    // Flush final hunk + file
    if let Some(ref mut file) = current_file {
        if let Some(hunk) = current_hunk.take() {
            file.hunks.push(hunk);
        }
        files.push(file.clone());
    }

    Ok(ParsedDiff { files })
}

/// Extract b-side file path from `diff --git a/path b/path`.
fn parse_file_path(line: &str) -> String {
    line.split(" b/").last().unwrap_or(line).to_string()
}

/// Parse `@@ -old_start[,count] +new_start[,count] @@` into (old_start, new_start).
fn parse_hunk_header(line: &str) -> (usize, usize) {
    let inner = line
        .trim_start_matches("@@ ")
        .split(" @@")
        .next()
        .unwrap_or("");

    let mut old_start = 1;
    let mut new_start = 1;

    for part in inner.split_whitespace() {
        if let Some(s) = part.strip_prefix('-') {
            old_start = s.split(',').next().and_then(|n| n.parse().ok()).unwrap_or(1);
        } else if let Some(s) = part.strip_prefix('+') {
            new_start = s.split(',').next().and_then(|n| n.parse().ok()).unwrap_or(1);
        }
    }

    (old_start, new_start)
}

/// Deterministic hash of diff content for change detection.
pub fn diff_hash(diff: &ParsedDiff) -> String {
    let mut hasher = DefaultHasher::new();
    for file in &diff.files {
        file.path.hash(&mut hasher);
        for hunk in &file.hunks {
            hunk.header.hash(&mut hasher);
            for line in &hunk.lines {
                line.content.hash(&mut hasher);
            }
        }
    }
    format!("{:016x}", hasher.finish())
}

// ---------------------------------------------------------------------------
// ParsedDiff helpers
// ---------------------------------------------------------------------------

impl ParsedDiff {
    pub fn empty() -> Self {
        ParsedDiff { files: Vec::new() }
    }
}

// ---------------------------------------------------------------------------
// DiffCursor helpers
// ---------------------------------------------------------------------------

impl DiffCursor {
    pub fn new() -> Self {
        DiffCursor::default()
    }
}

// ---------------------------------------------------------------------------
// ReviewState implementation
// ---------------------------------------------------------------------------

impl ReviewState {
    pub fn new(session_id: String, diff: ParsedDiff) -> Self {
        let hash = diff_hash(&diff);
        Self {
            session_id,
            diff,
            diff_hash: hash,
            cursor: DiffCursor::default(),
            comments: Vec::new(),
            hunk_approvals: HashSet::new(),
            overall: None,
        }
    }

    /// Total additions across all files.
    pub fn total_additions(&self) -> usize {
        self.diff.files.iter().map(|f| f.additions).sum()
    }

    /// Total deletions across all files.
    pub fn total_deletions(&self) -> usize {
        self.diff.files.iter().map(|f| f.deletions).sum()
    }

    /// Total line count in the given file (sum of all hunk line counts).
    fn file_line_count(&self, file_idx: usize) -> usize {
        self.diff
            .files
            .get(file_idx)
            .map(|f| f.hunks.iter().map(|h| h.lines.len()).sum())
            .unwrap_or(0)
    }

    // -- Cursor navigation ------------------------------------------------

    pub fn move_line_down(&mut self) {
        let max = self.file_line_count(self.cursor.file_idx);
        if max > 0 && self.cursor.line_idx < max - 1 {
            self.cursor.line_idx += 1;
        }
    }

    pub fn move_line_up(&mut self) {
        if self.cursor.line_idx > 0 {
            self.cursor.line_idx -= 1;
        }
    }

    /// Jump to the start of the next hunk in the current file.
    pub fn move_hunk_down(&mut self) {
        if let Some(file) = self.diff.files.get(self.cursor.file_idx) {
            let mut offset = 0;
            for hunk in &file.hunks {
                let next = offset + hunk.lines.len();
                if offset > self.cursor.line_idx {
                    self.cursor.line_idx = offset;
                    return;
                }
                offset = next;
            }
        }
    }

    /// Jump to the start of the previous hunk in the current file.
    pub fn move_hunk_up(&mut self) {
        if let Some(file) = self.diff.files.get(self.cursor.file_idx) {
            let mut offsets: Vec<usize> = Vec::new();
            let mut offset = 0;
            for hunk in &file.hunks {
                offsets.push(offset);
                offset += hunk.lines.len();
            }
            for &start in offsets.iter().rev() {
                if start < self.cursor.line_idx {
                    self.cursor.line_idx = start;
                    return;
                }
            }
            self.cursor.line_idx = 0;
        }
    }

    pub fn move_file_down(&mut self) {
        if self.cursor.file_idx + 1 < self.diff.files.len() {
            self.cursor.file_idx += 1;
            self.cursor.line_idx = 0;
        }
    }

    pub fn move_file_up(&mut self) {
        if self.cursor.file_idx > 0 {
            self.cursor.file_idx -= 1;
            self.cursor.line_idx = 0;
        }
    }

    // -- Accessors --------------------------------------------------------

    pub fn current_file(&self) -> Option<&DiffFile> {
        self.diff.files.get(self.cursor.file_idx)
    }

    /// Returns (hunk, line_index_within_hunk) for the current cursor position.
    pub fn current_hunk(&self) -> Option<(&Hunk, usize)> {
        let file = self.current_file()?;
        let mut offset = 0;
        for hunk in &file.hunks {
            let end = offset + hunk.lines.len();
            if self.cursor.line_idx < end {
                return Some((hunk, self.cursor.line_idx - offset));
            }
            offset = end;
        }
        None
    }

    pub fn current_line(&self) -> Option<&DiffLine> {
        let (hunk, idx) = self.current_hunk()?;
        hunk.lines.get(idx)
    }

    // -- Hunk approval ----------------------------------------------------

    fn hunk_key(&self, file_idx: usize, hunk_id: &str) -> String {
        format!("{file_idx}:{hunk_id}")
    }

    /// Toggle approval for the hunk under the cursor.
    pub fn toggle_hunk_approval(&mut self) {
        if let Some((hunk, _)) = self.current_hunk() {
            let key = self.hunk_key(self.cursor.file_idx, &hunk.id);
            if !self.hunk_approvals.remove(&key) {
                self.hunk_approvals.insert(key);
            }
        }
    }

    pub fn is_hunk_approved(&self, file_idx: usize, hunk_id: &str) -> bool {
        self.hunk_approvals.contains(&self.hunk_key(file_idx, hunk_id))
    }

    // -- Comments ---------------------------------------------------------

    /// Add a draft comment at the current cursor position.
    pub fn add_comment(&mut self, body: String) {
        if let Some(file) = self.current_file() {
            if let Some(line) = self.current_line() {
                let lineno = line
                    .new_lineno
                    .or(line.old_lineno)
                    .unwrap_or(self.cursor.line_idx + 1);
                self.comments.push(DraftComment {
                    file: file.path.clone(),
                    line: lineno,
                    body,
                });
            }
        }
    }

    pub fn remove_comment(&mut self, index: usize) {
        if index < self.comments.len() {
            self.comments.remove(index);
        }
    }

    // -- Payload serialization --------------------------------------------

    pub fn to_payload(&self) -> ReviewPayload {
        let mut reviews: Vec<ReviewItem> = Vec::new();

        for c in &self.comments {
            reviews.push(ReviewItem::Comment {
                file: c.file.clone(),
                line: c.line,
                body: c.body.clone(),
            });
        }

        for key in &self.hunk_approvals {
            if let Some((idx_str, hunk_id)) = key.split_once(':') {
                if let Ok(idx) = idx_str.parse::<usize>() {
                    if let Some(file) = self.diff.files.get(idx) {
                        reviews.push(ReviewItem::HunkApproval {
                            file: file.path.clone(),
                            hunk_id: hunk_id.to_string(),
                        });
                    }
                }
            }
        }

        ReviewPayload {
            session_id: self.session_id.clone(),
            diff_hash: self.diff_hash.clone(),
            reviews,
            overall: self.overall.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_DIFF: &str = "\
diff --git a/src/main.rs b/src/main.rs
index abc1234..def5678 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,4 +1,5 @@
 use std::io;
+use std::fs;

 fn main() {
-    println!(\"hello\");
+    println!(\"world\");
 }
@@ -10,3 +11,4 @@ fn other() {
     let x = 1;
+    let y = 2;
     let z = 3;
diff --git a/README.md b/README.md
new file mode 100644
--- /dev/null
+++ b/README.md
@@ -0,0 +1,2 @@
+# Project
+A description.
";

    // -- Parsing ----------------------------------------------------------

    #[test]
    fn parse_diff_files() {
        let diff = parse_diff(SAMPLE_DIFF).unwrap();
        assert_eq!(diff.files.len(), 2);
        assert_eq!(diff.files[0].path, "src/main.rs");
        assert_eq!(diff.files[1].path, "README.md");
    }

    #[test]
    fn parse_diff_hunks() {
        let diff = parse_diff(SAMPLE_DIFF).unwrap();
        let file = &diff.files[0];
        assert_eq!(file.hunks.len(), 2);
        assert_eq!(file.hunks[0].id, "h0");
        assert_eq!(file.hunks[1].id, "h1");
        assert_eq!(file.hunks[0].old_start, 1);
        assert_eq!(file.hunks[0].new_start, 1);
        assert_eq!(file.hunks[1].old_start, 10);
        assert_eq!(file.hunks[1].new_start, 11);
    }

    #[test]
    fn parse_diff_additions_deletions() {
        let diff = parse_diff(SAMPLE_DIFF).unwrap();
        // +std::fs, +println world, +let y = 3 additions; -println hello = 1 deletion
        assert_eq!(diff.files[0].additions, 3);
        assert_eq!(diff.files[0].deletions, 1);
        assert_eq!(diff.files[1].additions, 2);
        assert_eq!(diff.files[1].deletions, 0);
    }

    #[test]
    fn parse_diff_line_kinds() {
        let diff = parse_diff(SAMPLE_DIFF).unwrap();
        let hunk = &diff.files[0].hunks[0];
        assert_eq!(hunk.lines[0].kind, LineKind::Context); // use std::io;
        assert_eq!(hunk.lines[1].kind, LineKind::Add); // use std::fs;
        assert_eq!(hunk.lines[2].kind, LineKind::Context); // blank
        assert_eq!(hunk.lines[3].kind, LineKind::Context); // fn main()
        assert_eq!(hunk.lines[4].kind, LineKind::Remove); // println hello
        assert_eq!(hunk.lines[5].kind, LineKind::Add); // println world
        assert_eq!(hunk.lines[6].kind, LineKind::Context); // }
    }

    #[test]
    fn parse_diff_line_numbers() {
        let diff = parse_diff(SAMPLE_DIFF).unwrap();
        let hunk = &diff.files[0].hunks[0];
        // Context: use std::io — old=1, new=1
        assert_eq!(hunk.lines[0].old_lineno, Some(1));
        assert_eq!(hunk.lines[0].new_lineno, Some(1));
        // Add: use std::fs — old=None, new=2
        assert_eq!(hunk.lines[1].old_lineno, None);
        assert_eq!(hunk.lines[1].new_lineno, Some(2));
        // Remove: println hello — old=4, new=None
        assert_eq!(hunk.lines[4].old_lineno, Some(4));
        assert_eq!(hunk.lines[4].new_lineno, None);
    }

    #[test]
    fn parse_second_hunk_line_numbers() {
        let diff = parse_diff(SAMPLE_DIFF).unwrap();
        let hunk1 = &diff.files[0].hunks[1];
        assert_eq!(hunk1.lines[0].old_lineno, Some(10));
        assert_eq!(hunk1.lines[0].new_lineno, Some(11));
        assert_eq!(hunk1.lines[1].old_lineno, None);
        assert_eq!(hunk1.lines[1].new_lineno, Some(12));
    }

    #[test]
    fn parse_empty_diff() {
        let diff = parse_diff("").unwrap();
        assert!(diff.files.is_empty());
    }

    #[test]
    fn parse_binary_file() {
        let raw = "\
diff --git a/image.png b/image.png
Binary files /dev/null and b/image.png differ
";
        let diff = parse_diff(raw).unwrap();
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].path, "image.png");
        assert!(diff.files[0].hunks.is_empty());
        assert_eq!(diff.files[0].additions, 0);
    }

    // -- Hunk header parsing ----------------------------------------------

    #[test]
    fn hunk_header_basic() {
        assert_eq!(parse_hunk_header("@@ -1,4 +1,5 @@"), (1, 1));
    }

    #[test]
    fn hunk_header_large() {
        assert_eq!(parse_hunk_header("@@ -100,20 +200,30 @@ fn foo()"), (100, 200));
    }

    #[test]
    fn hunk_header_no_count() {
        assert_eq!(parse_hunk_header("@@ -1 +1 @@"), (1, 1));
    }

    // -- File path parsing ------------------------------------------------

    #[test]
    fn file_path_normal() {
        assert_eq!(
            parse_file_path("diff --git a/src/main.rs b/src/main.rs"),
            "src/main.rs"
        );
    }

    #[test]
    fn file_path_with_spaces() {
        assert_eq!(
            parse_file_path("diff --git a/my file.rs b/my file.rs"),
            "my file.rs"
        );
    }

    // -- Diff hash --------------------------------------------------------

    #[test]
    fn hash_deterministic() {
        let diff = parse_diff(SAMPLE_DIFF).unwrap();
        assert_eq!(diff_hash(&diff), diff_hash(&diff));
        assert_eq!(diff_hash(&diff).len(), 16);
    }

    #[test]
    fn hash_differs_for_different_diffs() {
        let d1 = parse_diff(SAMPLE_DIFF).unwrap();
        let d2 = parse_diff("").unwrap();
        assert_ne!(diff_hash(&d1), diff_hash(&d2));
    }

    // -- ReviewState: totals (preserved from original) --------------------

    #[test]
    fn empty_diff_totals() {
        let state = ReviewState::new("s1".into(), ParsedDiff::empty());
        assert_eq!(state.total_additions(), 0);
        assert_eq!(state.total_deletions(), 0);
    }

    #[test]
    fn totals_sum_across_files() {
        let diff = ParsedDiff {
            files: vec![
                DiffFile {
                    path: "a.rs".into(),
                    hunks: Vec::new(),
                    additions: 10,
                    deletions: 3,
                },
                DiffFile {
                    path: "b.rs".into(),
                    hunks: Vec::new(),
                    additions: 5,
                    deletions: 2,
                },
            ],
        };
        let state = ReviewState::new("s1".into(), diff);
        assert_eq!(state.total_additions(), 15);
        assert_eq!(state.total_deletions(), 5);
    }

    // -- Cursor navigation ------------------------------------------------

    fn sample_review() -> ReviewState {
        let diff = parse_diff(SAMPLE_DIFF).unwrap();
        ReviewState::new("test-session".into(), diff)
    }

    #[test]
    fn cursor_starts_at_origin() {
        let rs = sample_review();
        assert_eq!(rs.cursor.file_idx, 0);
        assert_eq!(rs.cursor.line_idx, 0);
    }

    #[test]
    fn move_line_down_up() {
        let mut rs = sample_review();
        rs.move_line_down();
        assert_eq!(rs.cursor.line_idx, 1);
        rs.move_line_down();
        assert_eq!(rs.cursor.line_idx, 2);
        rs.move_line_up();
        assert_eq!(rs.cursor.line_idx, 1);
    }

    #[test]
    fn move_line_up_at_zero_stays() {
        let mut rs = sample_review();
        rs.move_line_up();
        assert_eq!(rs.cursor.line_idx, 0);
    }

    #[test]
    fn move_line_down_clamps_at_end() {
        let mut rs = sample_review();
        let max = rs.file_line_count(0);
        for _ in 0..max + 5 {
            rs.move_line_down();
        }
        assert_eq!(rs.cursor.line_idx, max - 1);
    }

    #[test]
    fn move_hunk_down_jumps_to_next() {
        let mut rs = sample_review();
        let hunk0_len = rs.diff.files[0].hunks[0].lines.len();
        rs.move_hunk_down();
        assert_eq!(rs.cursor.line_idx, hunk0_len);
    }

    #[test]
    fn move_hunk_up_jumps_to_prev() {
        let mut rs = sample_review();
        let hunk0_len = rs.diff.files[0].hunks[0].lines.len();
        rs.cursor.line_idx = hunk0_len + 1;
        rs.move_hunk_up();
        assert_eq!(rs.cursor.line_idx, hunk0_len);
    }

    #[test]
    fn move_hunk_up_from_first_goes_to_zero() {
        let mut rs = sample_review();
        rs.cursor.line_idx = 3;
        rs.move_hunk_up();
        assert_eq!(rs.cursor.line_idx, 0);
    }

    #[test]
    fn move_file_down_up() {
        let mut rs = sample_review();
        rs.cursor.line_idx = 5;
        rs.move_file_down();
        assert_eq!(rs.cursor.file_idx, 1);
        assert_eq!(rs.cursor.line_idx, 0);
        rs.move_file_up();
        assert_eq!(rs.cursor.file_idx, 0);
        assert_eq!(rs.cursor.line_idx, 0);
    }

    #[test]
    fn move_file_down_at_last_stays() {
        let mut rs = sample_review();
        rs.cursor.file_idx = rs.diff.files.len() - 1;
        rs.move_file_down();
        assert_eq!(rs.cursor.file_idx, rs.diff.files.len() - 1);
    }

    #[test]
    fn move_file_up_at_first_stays() {
        let mut rs = sample_review();
        rs.move_file_up();
        assert_eq!(rs.cursor.file_idx, 0);
    }

    // -- Accessors --------------------------------------------------------

    #[test]
    fn current_file_accessor() {
        let rs = sample_review();
        assert_eq!(rs.current_file().unwrap().path, "src/main.rs");
    }

    #[test]
    fn current_hunk_accessor() {
        let rs = sample_review();
        let (hunk, idx) = rs.current_hunk().unwrap();
        assert_eq!(hunk.id, "h0");
        assert_eq!(idx, 0);
    }

    #[test]
    fn current_line_accessor() {
        let rs = sample_review();
        let line = rs.current_line().unwrap();
        assert_eq!(line.kind, LineKind::Context);
        assert_eq!(line.content, "use std::io;");
    }

    // -- Hunk approval ----------------------------------------------------

    #[test]
    fn toggle_hunk_approval() {
        let mut rs = sample_review();
        assert!(!rs.is_hunk_approved(0, "h0"));
        rs.toggle_hunk_approval();
        assert!(rs.is_hunk_approved(0, "h0"));
        rs.toggle_hunk_approval();
        assert!(!rs.is_hunk_approved(0, "h0"));
    }

    // -- Comments ---------------------------------------------------------

    #[test]
    fn add_comment() {
        let mut rs = sample_review();
        rs.add_comment("looks good".into());
        assert_eq!(rs.comments.len(), 1);
        assert_eq!(rs.comments[0].file, "src/main.rs");
        assert_eq!(rs.comments[0].body, "looks good");
        assert_eq!(rs.comments[0].line, 1);
    }

    #[test]
    fn remove_comment() {
        let mut rs = sample_review();
        rs.add_comment("first".into());
        rs.add_comment("second".into());
        rs.remove_comment(0);
        assert_eq!(rs.comments.len(), 1);
        assert_eq!(rs.comments[0].body, "second");
    }

    #[test]
    fn remove_comment_out_of_bounds_noop() {
        let mut rs = sample_review();
        rs.remove_comment(99);
        assert!(rs.comments.is_empty());
    }

    // -- Payload serialization --------------------------------------------

    #[test]
    fn payload_empty_reviews() {
        let rs = sample_review();
        let p = rs.to_payload();
        assert_eq!(p.session_id, "test-session");
        assert!(!p.diff_hash.is_empty());
        assert!(p.reviews.is_empty());
        assert!(p.overall.is_none());
    }

    #[test]
    fn payload_with_comments_and_approvals() {
        let mut rs = sample_review();
        rs.add_comment("fix this".into());
        rs.toggle_hunk_approval();
        rs.overall = Some("LGTM".into());

        let p = rs.to_payload();
        assert_eq!(p.overall, Some("LGTM".into()));

        let comments: Vec<_> = p
            .reviews
            .iter()
            .filter(|r| matches!(r, ReviewItem::Comment { .. }))
            .collect();
        assert_eq!(comments.len(), 1);

        let approvals: Vec<_> = p
            .reviews
            .iter()
            .filter(|r| matches!(r, ReviewItem::HunkApproval { .. }))
            .collect();
        assert_eq!(approvals.len(), 1);
    }

    #[test]
    fn payload_serializes_to_json() {
        let mut rs = sample_review();
        rs.add_comment("nit".into());
        rs.toggle_hunk_approval();
        let p = rs.to_payload();
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"session_id\":\"test-session\""));
        assert!(json.contains("\"nit\""));
    }

    // -- Empty diff state safety ------------------------------------------

    #[test]
    fn empty_diff_accessors_return_none() {
        let rs = ReviewState::new("s".into(), ParsedDiff::empty());
        assert!(rs.current_file().is_none());
        assert!(rs.current_hunk().is_none());
        assert!(rs.current_line().is_none());
    }

    #[test]
    fn empty_diff_navigation_no_panic() {
        let mut rs = ReviewState::new("s".into(), ParsedDiff::empty());
        rs.move_line_down();
        rs.move_line_up();
        rs.move_hunk_down();
        rs.move_hunk_up();
        rs.move_file_down();
        rs.move_file_up();
        rs.toggle_hunk_approval();
        rs.add_comment("test".into());
        assert_eq!(rs.cursor.file_idx, 0);
        assert_eq!(rs.cursor.line_idx, 0);
        assert!(rs.comments.is_empty()); // no file → comment not added
    }
}
