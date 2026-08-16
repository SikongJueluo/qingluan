//! Catalog construction: associate scanned Pi sessions with registered JJ
//! workspaces and produce the serializable catalog.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

use crate::workspace::associate::association_key;
use crate::workspace::pi_sessions::ScannedSession;

/// A workspace registered in the current JJ repository.
#[derive(Debug, Clone, PartialEq)]
pub struct RegisteredWorkspace {
    /// Workspace name as reported by `jj workspace list`.
    pub name: String,
    /// Workspace root path as reported by `jj workspace list`.
    pub root: String,
}

/// Semantic catalog of workspaces and their Pi sessions.
///
/// JSON is semantic data only (camelCase, `schemaVersion`); display labels
/// are built by adapters.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceCatalog {
    /// Catalog schema version; consumers must reject unknown versions.
    pub schema_version: u32,
    /// Registered workspaces in `jj workspace list` order.
    pub workspaces: Vec<WorkspaceSummary>,
}

/// One workspace and its associated sessions.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSummary {
    /// Workspace name.
    pub name: String,
    /// Workspace root path as registered.
    pub root: String,
    /// Whether the registered root currently exists on disk.
    pub available: bool,
    /// Why the workspace is unavailable (semantic reason string).
    pub unavailable_reason: Option<String>,
    /// Sessions whose cwd matches this workspace, most recently modified
    /// first.
    pub sessions: Vec<SessionSummary>,
}

/// One Pi session associated with a workspace.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    /// Absolute path of the session `.jsonl` file.
    pub file: String,
    /// Session id from the header, when present.
    pub id: Option<String>,
    /// Derived title (Pi display semantics).
    pub title: String,
    /// Count of all message entries.
    pub message_count: u32,
    /// Effective modified time (UTC ISO-8601).
    pub modified: String,
}

/// Build the catalog from registered workspaces and scanned sessions.
///
/// Association is an exact path match after safe normalization (canonicalize
/// existing paths, lexical otherwise). Sessions matching no registered
/// workspace are omitted — foreign sessions are never misattributed, and
/// forgotten workspaces simply do not appear.
pub fn build_catalog(
    registered: &[RegisteredWorkspace],
    mut sessions: Vec<ScannedSession>,
) -> WorkspaceCatalog {
    let mut keys: HashMap<std::path::PathBuf, usize> = HashMap::new();
    let mut workspaces: Vec<WorkspaceSummary> = Vec::with_capacity(registered.len());
    for (idx, ws) in registered.iter().enumerate() {
        let root_path = Path::new(&ws.root);
        let available = root_path.exists();
        keys.entry(association_key(root_path)).or_insert(idx);
        workspaces.push(WorkspaceSummary {
            name: ws.name.clone(),
            root: ws.root.clone(),
            available,
            unavailable_reason: (!available).then(|| "workspace root not found on disk".to_owned()),
            sessions: Vec::new(),
        });
    }

    for session in sessions.drain(..) {
        let key = association_key(Path::new(&session.cwd));
        let Some(&idx) = keys.get(&key) else {
            continue; // unassociated: omitted, never misattributed
        };
        workspaces[idx].sessions.push(SessionSummary {
            file: session.file.to_string_lossy().into_owned(),
            id: session.session_id.clone(),
            title: session.title.clone(),
            message_count: session.message_count,
            modified: session.modified_iso(),
        });
    }

    for ws in &mut workspaces {
        ws.sessions.sort_by(|a, b| b.modified.cmp(&a.modified));
    }

    WorkspaceCatalog {
        schema_version: 1,
        workspaces,
    }
}
