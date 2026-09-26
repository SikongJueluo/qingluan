//! Code-review sessions: jj diff extraction plus an in-memory comment
//! store (ADR-0003).
//!
//! A session is a one-shot snapshot: `POST /reviews` runs
//! `jj diff --from <rev> --to <rev>` (default `main..@`) in the target
//! directory with maximum context, so the single unified diff doubles as
//! the full old/new contents of every file. Nothing is persisted — a
//! daemon restart drops all sessions by design.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Json, Router,
    extract::{Path as AxPath, State},
    http::StatusCode,
    routing::{get, patch, post},
};
use qingluan_protocol::ApiResponse;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;

// ─── Domain types ───────────────────────────────────────────────────────────
//
// The wire shapes mirror the frontend contract
// (apps/web/src/components/code-review/types.ts); camelCase on the wire.

/// File change status. The frontend `ChangeStatus` union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeStatus {
    Modified,
    Added,
    Deleted,
}

/// Per-file metadata served by `GET /reviews/{id}/files`.
///
/// File text is NOT part of the list response (two-level loading):
/// `GET /reviews/{id}/files/{n}?side=old|new` returns full text on demand.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChangedFileMeta {
    pub path: String,
    pub status: ChangeStatus,
    pub additions: u64,
    pub deletions: u64,
    /// Binary files have no servable text; the UI renders a placeholder.
    pub binary: bool,
}

/// Side of a file version (`?side=` query parameter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Old,
    New,
}

/// One review comment (frontend `ReviewComment`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewComment {
    pub id: String,
    /// `ChangedFileMeta.path` this comment belongs to.
    pub file: String,
    pub side: Side,
    /// 1-based inclusive line range.
    pub line_from: u32,
    pub line_to: u32,
    /// side 'old': document position of the deleted chunk widget.
    pub chunk_pos: Option<u32>,
    pub author: String,
    pub content: String,
    /// Unix epoch milliseconds.
    pub created_at: u64,
    pub updated_at: u64,
}

/// Old/new text of one file, captured when the session is created.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileContents {
    pub old: String,
    pub new: String,
}

/// An in-memory review session.
#[derive(Debug)]
pub struct ReviewSession {
    pub id: String,
    /// Directory the diff was computed in.
    pub root: PathBuf,
    pub from: String,
    pub to: String,
    pub files: Vec<ChangedFileMeta>,
    contents: Vec<FileContents>,
    comments: Vec<ReviewComment>,
}

// ─── Errors ─────────────────────────────────────────────────────────────────

/// Failures surfaced by review endpoints.
#[derive(Debug)]
pub enum ReviewError {
    /// The target directory is not inside a jj repository (e.g. plain git).
    NotARepo { dir: String },
    /// jj exists but the diff command failed (bad revset, io error, …).
    JjFailed { code: Option<i32>, stderr: String },
}

impl ReviewError {
    /// Stable API error code.
    pub fn api_code(&self) -> &'static str {
        match self {
            ReviewError::NotARepo { .. } => "not_a_jj_repo",
            ReviewError::JjFailed { .. } => "jj_diff_failed",
        }
    }

    /// HTTP status for the error.
    pub fn status(&self) -> StatusCode {
        match self {
            ReviewError::NotARepo { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            ReviewError::JjFailed { .. } => StatusCode::BAD_REQUEST,
        }
    }
}

impl std::fmt::Display for ReviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReviewError::NotARepo { dir } => write!(
                f,
                "{dir} is not a jj repository — move into a jj workspace \
                 (e.g. `qingluan workspace add`) and retry"
            ),
            ReviewError::JjFailed { code, stderr } => {
                write!(f, "jj diff failed (exit={code:?}): {stderr}")
            }
        }
    }
}

impl std::error::Error for ReviewError {}

// ─── jj diff extraction ─────────────────────────────────────────────────────

/// Context lines to request from jj: effectively unbounded, so every hunk
/// spans the whole file and the unified diff doubles as full contents.
const FULL_FILE_CONTEXT: &str = "1000000";

/// Run `jj diff --from <from> --to <to>` in `root` and return the
/// `--git --context=FULL_FILE_CONTEXT` output.
pub fn run_jj_diff(root: &Path, from: &str, to: &str) -> Result<String, ReviewError> {
    let output = Command::new("jj")
        .args(["--no-pager", "--color=never", "-R"])
        .arg(root)
        .args([
            "diff",
            "--git",
            "--context",
            FULL_FILE_CONTEXT,
            "--from",
            from,
            "--to",
            to,
        ])
        .output()
        .map_err(|e| ReviewError::JjFailed {
            code: None,
            stderr: format!("cannot spawn jj: {e}"),
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if stderr.contains("no jj repo") {
            return Err(ReviewError::NotARepo {
                dir: root.display().to_string(),
            });
        }
        return Err(ReviewError::JjFailed {
            code: output.status.code(),
            stderr,
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse a `--git --context=<full>` unified diff into per-file metadata
/// plus reconstructed old/new contents.
///
/// Lines are joined with `\n`; a trailing `\ No newline at end of file`
/// marker clears the final newline on the respective side.
pub fn parse_git_diff(output: &str) -> Vec<(ChangedFileMeta, FileContents)> {
    let mut files: Vec<(ChangedFileMeta, FileContents)> = Vec::new();
    // Per-file parse state.
    let mut current: Option<(ChangedFileMeta, FileContents, FileState)> = None;

    enum Line<'a> {
        Header(&'a str),
        Body(&'a str),
    }

    for raw in output.lines() {
        let line = match raw.strip_prefix("diff --git ") {
            Some(rest) => Line::Header(rest),
            None => Line::Body(raw),
        };
        match line {
            Line::Header(rest) => {
                flush(&mut current, &mut files);
                current = Some((
                    ChangedFileMeta {
                        path: diff_header_path(rest),
                        status: ChangeStatus::Modified,
                        additions: 0,
                        deletions: 0,
                        binary: false,
                    },
                    FileContents::default(),
                    FileState::default(),
                ));
            }
            Line::Body(body) => {
                let Some((meta, contents, state)) = current.as_mut() else {
                    continue;
                };
                if body.starts_with("new file mode") {
                    meta.status = ChangeStatus::Added;
                } else if body.starts_with("deleted file mode") {
                    meta.status = ChangeStatus::Deleted;
                } else if let Some(to) = body.strip_prefix("rename to ") {
                    // No copy tracking in our diffs; a rename surfaces as a
                    // modified file under its new path.
                    meta.path = unquote(to).to_owned();
                    meta.status = ChangeStatus::Modified;
                } else if body.starts_with("Binary files") {
                    meta.binary = true;
                } else if let Some(hunk) = body.strip_prefix("@@") {
                    // Hunk header: consume to end-of-header (we reconstruct
                    // from line markers only, but must not misread the
                    // header text as body lines).
                    let _ = hunk;
                    state.in_hunk = true;
                } else if state.in_hunk {
                    if let Some(text) = body.strip_prefix('+') {
                        meta.additions += 1;
                        state.no_newline_new = false;
                        push_line(&mut contents.new, text);
                    } else if let Some(text) = body.strip_prefix('-') {
                        meta.deletions += 1;
                        state.no_newline_old = false;
                        push_line(&mut contents.old, text);
                    } else if let Some(text) = body.strip_prefix(' ') {
                        push_line(&mut contents.old, text);
                        push_line(&mut contents.new, text);
                        state.no_newline_new = false;
                        state.no_newline_old = false;
                    } else if let Some(marker) = body.strip_prefix('\\') {
                        // "\ No newline at end of file" applies to whichever
                        // side was last emitted; track both conservatively.
                        let _ = marker;
                        if state.last_emit == Emit::Old {
                            state.no_newline_old = true;
                        } else if state.last_emit == Emit::New {
                            state.no_newline_new = true;
                        }
                    } else if body.is_empty() {
                        // Empty context line: git renders " " + "" as a
                        // single space, but guard against stripped trailing
                        // whitespace anyway.
                        push_line(&mut contents.old, "");
                        push_line(&mut contents.new, "");
                    }
                    if body.starts_with('+') {
                        state.last_emit = Emit::New;
                    } else if body.starts_with('-') || body.starts_with(' ') {
                        state.last_emit = Emit::Old;
                    }
                }
            }
        }
    }
    flush(&mut current, &mut files);
    files
}

/// Per-file scratch state while parsing hunks.
#[derive(Default)]
struct FileState {
    in_hunk: bool,
    no_newline_old: bool,
    no_newline_new: bool,
    last_emit: Emit,
}

#[derive(Default, PartialEq)]
enum Emit {
    #[default]
    None,
    Old,
    New,
}

/// Append one diff line to a side's text. Every unified-diff line
/// implies a trailing newline unless a `\ No newline` marker follows it.
fn push_line(text: &mut String, line: &str) {
    text.push_str(line);
    text.push('\n');
}

/// Finalize the file being parsed: trim the phantom trailing newline when
/// the last line had no newline marker.
fn flush(
    current: &mut Option<(ChangedFileMeta, FileContents, FileState)>,
    files: &mut Vec<(ChangedFileMeta, FileContents)>,
) {
    if let Some((meta, mut contents, state)) = current.take() {
        if state.no_newline_old {
            contents.old.pop();
        }
        if state.no_newline_new {
            contents.new.pop();
        }
        files.push((meta, contents));
    }
}

/// Extract the b-side path from a `diff --git a/X b/Y` header remainder.
fn diff_header_path(rest: &str) -> String {
    // Common form: `a/path b/path`. Paths with spaces are quoted.
    if let Some((a, b)) = rest.split_once(" b/")
        && (a.starts_with("a/") || a.starts_with('"'))
    {
        return unquote(b).to_owned();
    }
    unquote(rest).to_owned()
}

/// Strip surrounding C-style quoting from a git diff path, if present.
fn unquote(path: &str) -> &str {
    let path = path.trim();
    if path.len() >= 2 && path.starts_with('"') && path.ends_with('"') {
        // Minimal: drop the quotes; embedded \" escapes are vanishingly
        // rare for reviewed source trees.
        &path[1..path.len() - 1]
    } else {
        path
    }
}

/// Create a review session by snapshotting the diff of `root`.
pub fn create_session(root: &Path, from: &str, to: &str) -> Result<ReviewSession, ReviewError> {
    let output = run_jj_diff(root, from, to)?;
    let mut files = Vec::new();
    let mut contents = Vec::new();
    for (meta, content) in parse_git_diff(&output) {
        files.push(meta);
        contents.push(content);
    }
    Ok(ReviewSession {
        id: Uuid::now_v7().to_string(),
        root: root.to_path_buf(),
        from: from.to_owned(),
        to: to.to_owned(),
        files,
        contents,
        comments: Vec::new(),
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ─── Session store ──────────────────────────────────────────────────────────

/// All live review sessions (daemon memory only).
#[derive(Default)]
pub struct ReviewStore {
    sessions: Mutex<HashMap<String, ReviewSession>>,
}

/// Errors from comment operations.
#[derive(Debug)]
pub enum CommentError {
    SessionNotFound(String),
    CommentNotFound(String),
    UnknownFile(String),
    InvalidRange,
}

impl CommentError {
    pub fn api_code(&self) -> &'static str {
        match self {
            CommentError::SessionNotFound(_) => "review_not_found",
            CommentError::CommentNotFound(_) => "comment_not_found",
            CommentError::UnknownFile(_) => "unknown_file",
            CommentError::InvalidRange => "invalid_line_range",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            CommentError::SessionNotFound(_) | CommentError::CommentNotFound(_) => {
                StatusCode::NOT_FOUND
            }
            CommentError::UnknownFile(_) | CommentError::InvalidRange => StatusCode::BAD_REQUEST,
        }
    }
}

impl std::fmt::Display for CommentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommentError::SessionNotFound(id) => write!(f, "review session {id} not found"),
            CommentError::CommentNotFound(id) => write!(f, "comment {id} not found"),
            CommentError::UnknownFile(path) => {
                write!(f, "file {path} is not part of this review")
            }
            CommentError::InvalidRange => write!(f, "lineFrom must be <= lineTo and >= 1"),
        }
    }
}

impl std::error::Error for CommentError {}

impl ReviewStore {
    pub fn insert(&self, session: ReviewSession) -> String {
        let id = session.id.clone();
        self.sessions
            .lock()
            .expect("review store poisoned")
            .insert(id.clone(), session);
        id
    }

    pub fn files(&self, id: &str) -> Option<Vec<ChangedFileMeta>> {
        self.sessions
            .lock()
            .expect("review store poisoned")
            .get(id)
            .map(|s| s.files.clone())
    }

    /// Full text of file `index` on `side`, 0-based into the file list.
    pub fn file_text(&self, id: &str, index: usize, side: Side) -> Option<(String, String)> {
        self.sessions
            .lock()
            .expect("review store poisoned")
            .get(id)
            .and_then(|s| {
                let meta = s.files.get(index)?;
                let content = s.contents.get(index)?;
                let text = match side {
                    Side::Old => content.old.clone(),
                    Side::New => content.new.clone(),
                };
                Some((meta.path.clone(), text))
            })
    }

    pub fn comments(&self, id: &str) -> Option<Vec<ReviewComment>> {
        self.sessions
            .lock()
            .expect("review store poisoned")
            .get(id)
            .map(|s| s.comments.clone())
    }

    pub fn add_comment(&self, id: &str, new: NewComment) -> Result<ReviewComment, CommentError> {
        if new.line_from == 0 || new.line_from > new.line_to {
            return Err(CommentError::InvalidRange);
        }
        let mut sessions = self.sessions.lock().expect("review store poisoned");
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| CommentError::SessionNotFound(id.to_owned()))?;
        if !session.files.iter().any(|f| f.path == new.file) {
            return Err(CommentError::UnknownFile(new.file.clone()));
        }
        let now = now_ms();
        let comment = ReviewComment {
            id: Uuid::now_v7().to_string(),
            file: new.file,
            side: new.side,
            line_from: new.line_from,
            line_to: new.line_to,
            chunk_pos: new.chunk_pos,
            author: new.author,
            content: new.content,
            created_at: now,
            updated_at: now,
        };
        session.comments.push(comment.clone());
        Ok(comment)
    }

    /// Replace a comment's content (author and anchor stay fixed).
    pub fn update_comment(
        &self,
        id: &str,
        comment_id: &str,
        content: String,
    ) -> Result<ReviewComment, CommentError> {
        let mut sessions = self.sessions.lock().expect("review store poisoned");
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| CommentError::SessionNotFound(id.to_owned()))?;
        let comment = session
            .comments
            .iter_mut()
            .find(|c| c.id == comment_id)
            .ok_or_else(|| CommentError::CommentNotFound(comment_id.to_owned()))?;
        comment.content = content;
        comment.updated_at = now_ms();
        Ok(comment.clone())
    }

    pub fn delete_comment(&self, id: &str, comment_id: &str) -> Result<(), CommentError> {
        let mut sessions = self.sessions.lock().expect("review store poisoned");
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| CommentError::SessionNotFound(id.to_owned()))?;
        let before = session.comments.len();
        session.comments.retain(|c| c.id != comment_id);
        if session.comments.len() == before {
            return Err(CommentError::CommentNotFound(comment_id.to_owned()));
        }
        Ok(())
    }
}

// ─── HTTP layer ─────────────────────────────────────────────────────────────

/// Request body of `POST /reviews`.
#[derive(Debug, Deserialize)]
pub struct CreateReviewRequest {
    /// Directory to review (jj working copy or any path inside the repo).
    pub path: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateReviewResponse {
    pub id: String,
}

/// Request body of `POST /reviews/{id}/comments`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewComment {
    pub file: String,
    pub side: Side,
    pub line_from: u32,
    pub line_to: u32,
    pub chunk_pos: Option<u32>,
    pub author: String,
    pub content: String,
}

/// Request body of `PATCH /reviews/{id}/comments/{cid}`.
#[derive(Debug, Deserialize)]
pub struct UpdateCommentRequest {
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct FileSideQuery {
    pub side: Side,
}

#[derive(Debug, Serialize)]
pub struct FileTextResponse {
    pub path: String,
    pub text: String,
}

/// Review routes, mounted under the app router.
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/reviews", post(create_review))
        .route("/reviews/{id}/files", get(list_files))
        .route("/reviews/{id}/files/{index}", get(get_file_text))
        .route(
            "/reviews/{id}/comments",
            get(list_comments).post(create_comment),
        )
        .route(
            "/reviews/{id}/comments/{comment_id}",
            patch(update_comment).delete(delete_comment),
        )
}

/// POST /reviews — snapshot a jj diff into a new session.
async fn create_review(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<CreateReviewRequest>,
) -> Result<Json<ApiResponse<CreateReviewResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let root = PathBuf::from(&payload.path);
    let from = payload.from.unwrap_or_else(|| "main".to_owned());
    let to = payload.to.unwrap_or_else(|| "@".to_owned());
    // Diff extraction is blocking subprocess work; keep the handler cheap
    // by running it on the blocking-friendly path (repos are small).
    let session = tokio::task::spawn_blocking(move || create_session(&root, &from, &to))
        .await
        .expect("review task panicked")
        .map_err(api_error)?;
    let id = state.reviews.insert(session);
    tracing::info!("review session {id} created");
    Ok(Json(ApiResponse::success(CreateReviewResponse { id })))
}

/// GET /reviews/{id}/files — per-file metadata.
async fn list_files(
    State(state): State<Arc<AppState>>,
    AxPath(id): AxPath<String>,
) -> Result<Json<ApiResponse<Vec<ChangedFileMeta>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let files = state
        .reviews
        .files(&id)
        .ok_or_else(|| api_error(CommentError::SessionNotFound(id.clone())))?;
    Ok(Json(ApiResponse::success(files)))
}

/// GET /reviews/{id}/files/{index}?side=old|new — full file text.
async fn get_file_text(
    State(state): State<Arc<AppState>>,
    AxPath((id, index)): AxPath<(String, usize)>,
    axum::extract::Query(query): axum::extract::Query<FileSideQuery>,
) -> Result<Json<ApiResponse<FileTextResponse>>, (StatusCode, Json<ApiResponse<()>>)> {
    let (path, text) = state
        .reviews
        .file_text(&id, index, query.side)
        .ok_or_else(|| api_error(CommentError::SessionNotFound(id.clone())))?;
    Ok(Json(ApiResponse::success(FileTextResponse { path, text })))
}

/// GET /reviews/{id}/comments — all comments of a session.
async fn list_comments(
    State(state): State<Arc<AppState>>,
    AxPath(id): AxPath<String>,
) -> Result<Json<ApiResponse<Vec<ReviewComment>>>, (StatusCode, Json<ApiResponse<()>>)> {
    let comments = state
        .reviews
        .comments(&id)
        .ok_or_else(|| api_error(CommentError::SessionNotFound(id.clone())))?;
    Ok(Json(ApiResponse::success(comments)))
}

/// POST /reviews/{id}/comments — add a comment.
async fn create_comment(
    State(state): State<Arc<AppState>>,
    AxPath(id): AxPath<String>,
    Json(payload): Json<NewComment>,
) -> Result<Json<ApiResponse<ReviewComment>>, (StatusCode, Json<ApiResponse<()>>)> {
    let comment = state.reviews.add_comment(&id, payload).map_err(api_error)?;
    Ok(Json(ApiResponse::success(comment)))
}

/// PATCH /reviews/{id}/comments/{cid} — edit comment content.
async fn update_comment(
    State(state): State<Arc<AppState>>,
    AxPath((id, comment_id)): AxPath<(String, String)>,
    Json(payload): Json<UpdateCommentRequest>,
) -> Result<Json<ApiResponse<ReviewComment>>, (StatusCode, Json<ApiResponse<()>>)> {
    let comment = state
        .reviews
        .update_comment(&id, &comment_id, payload.content)
        .map_err(api_error)?;
    Ok(Json(ApiResponse::success(comment)))
}

/// DELETE /reviews/{id}/comments/{cid} — remove a comment.
async fn delete_comment(
    State(state): State<Arc<AppState>>,
    AxPath((id, comment_id)): AxPath<(String, String)>,
) -> Result<Json<ApiResponse<serde_json::Value>>, (StatusCode, Json<ApiResponse<()>>)> {
    state
        .reviews
        .delete_comment(&id, &comment_id)
        .map_err(api_error)?;
    Ok(Json(ApiResponse::success(
        serde_json::json!({ "deleted": true }),
    )))
}

/// Map a domain error into the (status, body) error response shape.
fn api_error<E: Into<Box<dyn std::error::Error + Send + Sync>>>(
    error: E,
) -> (StatusCode, Json<ApiResponse<()>>) {
    // Downcast-free fast paths for the concrete error types we produce.
    let error = error.into();
    let any = &*error as &(dyn std::error::Error + Send + Sync);
    let (status, code, message) = if let Some(e) = any.downcast_ref::<ReviewError>() {
        (e.status(), e.api_code(), e.to_string())
    } else if let Some(e) = any.downcast_ref::<CommentError>() {
        (e.status(), e.api_code(), e.to_string())
    } else {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            any.to_string(),
        )
    };
    (status, Json(ApiResponse::error(code, message)))
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const MODIFIED_DIFF: &str = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 1,4 @@
 fn main() {
-    println!(\"old\");
+    println!(\"new\");
+    println!(\"more\");
 }
";

    #[test]
    fn parses_modified_file_meta_and_full_contents() {
        let files = parse_git_diff(MODIFIED_DIFF);
        assert_eq!(files.len(), 1);
        let (meta, contents) = &files[0];
        assert_eq!(meta.path, "src/lib.rs");
        assert_eq!(meta.status, ChangeStatus::Modified);
        assert_eq!(meta.additions, 2);
        assert_eq!(meta.deletions, 1);
        assert!(!meta.binary);
        assert_eq!(contents.old, "fn main() {\n    println!(\"old\");\n}\n");
        assert_eq!(
            contents.new,
            "fn main() {\n    println!(\"new\");\n    println!(\"more\");\n}\n"
        );
    }

    #[test]
    fn parses_added_file_with_no_old_text() {
        let diff = "\
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..abc1234 100644
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+one
+two
";
        let files = parse_git_diff(diff);
        let (meta, contents) = &files[0];
        assert_eq!(meta.status, ChangeStatus::Added);
        assert_eq!(contents.old, "");
        assert_eq!(contents.new, "one\ntwo\n");
        assert_eq!(meta.additions, 2);
        assert_eq!(meta.deletions, 0);
    }

    #[test]
    fn parses_deleted_file_with_no_new_text() {
        let diff = "\
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index abc1234..0000000 100644
--- a/gone.txt
+++ /dev/null
@@ -1,1 +0,0 @@
-goodbye
";
        let files = parse_git_diff(diff);
        let (meta, contents) = &files[0];
        assert_eq!(meta.status, ChangeStatus::Deleted);
        assert_eq!(contents.old, "goodbye\n");
        assert_eq!(contents.new, "");
    }

    #[test]
    fn parses_multiple_files_and_binary() {
        let diff = "\
diff --git a/a.rs b/a.rs
index 1..2 100644
--- a/a.rs
+++ b/a.rs
@@ -1,1 +1,1 @@
-a
+b
diff --git a/logo.png b/logo.png
index 3..4 100644
Binary files a/logo.png and b/logo.png differ
";
        let files = parse_git_diff(diff);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].0.path, "a.rs");
        assert!(files[1].0.binary);
        assert_eq!(files[1].1.old, "");
    }

    #[test]
    fn handles_missing_trailing_newline() {
        let diff = "\
diff --git a/x.txt b/x.txt
index 1..2 100644
--- a/x.txt
+++ b/x.txt
@@ -1,1 +1,1 @@
-no-newline-old
\\ No newline at end of file
+no-newline-new
\\ No newline at end of file
";
        let files = parse_git_diff(diff);
        let (_, contents) = &files[0];
        assert_eq!(contents.old, "no-newline-old");
        assert_eq!(contents.new, "no-newline-new");
    }

    #[test]
    fn empty_diff_yields_no_files() {
        assert!(parse_git_diff("").is_empty());
    }

    #[test]
    fn comment_lifecycle_in_store() {
        let store = ReviewStore::default();
        let session = ReviewSession {
            id: "s1".into(),
            root: PathBuf::from("/tmp/repo"),
            from: "main".into(),
            to: "@".into(),
            files: vec![ChangedFileMeta {
                path: "a.rs".into(),
                status: ChangeStatus::Modified,
                additions: 1,
                deletions: 1,
                binary: false,
            }],
            contents: vec![FileContents::default()],
            comments: Vec::new(),
        };
        store.insert(session);

        let created = store
            .add_comment(
                "s1",
                NewComment {
                    file: "a.rs".into(),
                    side: Side::New,
                    line_from: 1,
                    line_to: 2,
                    chunk_pos: None,
                    author: "reviewer".into(),
                    content: "nit".into(),
                },
            )
            .expect("add");

        assert_eq!(store.comments("s1").unwrap().len(), 1);
        assert_eq!(created.file, "a.rs");
        assert!(created.created_at > 0);

        let updated = store
            .update_comment("s1", &created.id, "nit (fixed wording)".into())
            .expect("update");
        assert_eq!(updated.content, "nit (fixed wording)");
        assert!(updated.updated_at >= updated.created_at);

        store.delete_comment("s1", &created.id).expect("delete");
        assert!(store.comments("s1").unwrap().is_empty());
        assert!(matches!(
            store.delete_comment("s1", &created.id),
            Err(CommentError::CommentNotFound(_))
        ));
    }

    #[test]
    fn comment_validation_rejects_unknown_file_and_bad_range() {
        let store = ReviewStore::default();
        store.insert(ReviewSession {
            id: "s2".into(),
            root: PathBuf::from("/tmp/repo"),
            from: "main".into(),
            to: "@".into(),
            files: vec![ChangedFileMeta {
                path: "known.rs".into(),
                status: ChangeStatus::Modified,
                additions: 0,
                deletions: 0,
                binary: false,
            }],
            contents: vec![FileContents::default()],
            comments: Vec::new(),
        });
        let mk = |file: &str, from: u32, to: u32| NewComment {
            file: file.into(),
            side: Side::Old,
            line_from: from,
            line_to: to,
            chunk_pos: None,
            author: "a".into(),
            content: "c".into(),
        };
        assert!(matches!(
            store.add_comment("s2", mk("missing.rs", 1, 1)),
            Err(CommentError::UnknownFile(_))
        ));
        assert!(matches!(
            store.add_comment("s2", mk("known.rs", 3, 2)),
            Err(CommentError::InvalidRange)
        ));
        assert!(matches!(
            store.add_comment("s2", mk("known.rs", 0, 1)),
            Err(CommentError::InvalidRange)
        ));
        assert!(store.comments("missing").is_none());
    }
}
