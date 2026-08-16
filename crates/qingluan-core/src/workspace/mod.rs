//! Workspace and Pi session discovery.
//!
//! This module is a deep module: it owns JJ workspace discovery, Pi session
//! JSONL scanning, path normalization/association, and the serializable
//! workspace/session catalog exposed to adapters (CLI, Pi extension).

mod associate;
mod catalog;
mod jj;
mod pi_sessions;
mod time;

pub use associate::association_key;
pub use catalog::{
    RegisteredWorkspace, SessionSummary, WorkspaceCatalog, WorkspaceSummary, build_catalog,
};
pub use jj::{WORKSPACE_LIST_TEMPLATE, list_jj_workspaces, parse_workspace_list_output};
pub use pi_sessions::{ScannedSession, scan_sessions_root, summarize_session_lines};
pub use time::{format_iso8601_ms, parse_iso8601_ms};

use std::path::{Path, PathBuf};

/// Errors produced by workspace/session discovery.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    /// The caller's working directory is not inside a JJ repository.
    #[error("not inside a jj repository: {stderr}")]
    NotInJjRepository {
        /// Trimmed stderr produced by `jj workspace list`.
        stderr: String,
    },
    /// The `jj` binary could not be executed.
    #[error("failed to run jj: {message}")]
    JjSpawn {
        /// Underlying io error message.
        message: String,
    },
    /// `jj workspace list` exited with a nonzero status for another reason.
    #[error("jj workspace list failed (exit code {code:?}): {stderr}")]
    JjCommandFailed {
        /// Exit code, if any.
        code: Option<i32>,
        /// Trimmed stderr produced by `jj`.
        stderr: String,
    },
}

impl WorkspaceError {
    /// Machine-readable error code for adapters.
    pub fn code(&self) -> &'static str {
        match self {
            WorkspaceError::NotInJjRepository { .. } => "not_in_jj_repository",
            WorkspaceError::JjSpawn { .. } => "jj_spawn_failed",
            WorkspaceError::JjCommandFailed { .. } => "jj_command_failed",
        }
    }
}

/// Default Pi sessions root (`~/.pi/agent/sessions`).
pub fn default_sessions_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".pi")
            .join("agent")
            .join("sessions"),
    )
}

/// Discover workspaces and their Pi sessions from the caller's cwd.
///
/// `sessions_root` overrides the default Pi sessions root (mainly for tests).
/// Workspaces come from `jj workspace list` run in the current directory;
/// sessions are matched to workspaces by normalized cwd. Sessions belonging
/// to unregistered directories are omitted (never misattributed).
pub fn discover(sessions_root: Option<&Path>) -> Result<WorkspaceCatalog, WorkspaceError> {
    let registered = list_jj_workspaces()?;
    let root = sessions_root
        .map(Path::to_path_buf)
        .or_else(default_sessions_root);
    let sessions = root.as_deref().map(scan_sessions_root).unwrap_or_default();
    Ok(catalog::build_catalog(&registered, sessions))
}
