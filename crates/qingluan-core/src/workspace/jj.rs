//! JJ workspace discovery via the `jj` CLI.
//!
//! No jj-lib: we shell out to `jj workspace list` in the caller's cwd with a
//! robust JSON template and parse the streamed JSON string pairs.

use std::process::Command;

use serde_json::Deserializer;

use crate::workspace::WorkspaceError;
use crate::workspace::catalog::RegisteredWorkspace;

/// Template emitting one `"<name>"\t"<root>"\n` line per workspace.
pub const WORKSPACE_LIST_TEMPLATE: &str =
    r#"json(self.name()) ++ "\t" ++ json(self.root()) ++ "\n""#;

/// List workspaces of the JJ repository containing the caller's cwd.
///
/// `--ignore-working-copy` keeps the listing read-only: `jj` otherwise
/// snapshots the working copy first, which can fail on read-only checkouts
/// even though listing never needs to write.
pub fn list_jj_workspaces() -> Result<Vec<RegisteredWorkspace>, WorkspaceError> {
    let output = Command::new("jj")
        .args([
            "--no-pager",
            "--ignore-working-copy",
            "workspace",
            "list",
            "-T",
            WORKSPACE_LIST_TEMPLATE,
        ])
        .output()
        .map_err(|e| WorkspaceError::JjSpawn {
            message: e.to_string(),
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if stderr.contains("no jj repo") {
            return Err(WorkspaceError::NotInJjRepository { stderr });
        }
        return Err(WorkspaceError::JjCommandFailed {
            code: output.status.code(),
            stderr,
        });
    }
    Ok(parse_workspace_list_output(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

/// Parse `jj workspace list` template output into registered workspaces.
///
/// The template emits a stream of JSON strings: name, root, name, root, …
/// Non-string or odd trailing values are ignored defensively.
pub fn parse_workspace_list_output(output: &str) -> Vec<RegisteredWorkspace> {
    let mut names: Vec<String> = Vec::new();
    let mut roots: Vec<String> = Vec::new();
    for value in Deserializer::from_str(output).into_iter::<serde_json::Value>() {
        let Ok(value) = value else { continue };
        let Some(s) = value.as_str() else {
            continue;
        };
        if names.len() > roots.len() {
            roots.push(s.to_owned());
        } else {
            names.push(s.to_owned());
        }
    }
    names
        .into_iter()
        .zip(roots)
        .map(|(name, root)| RegisteredWorkspace { name, root })
        .collect()
}
