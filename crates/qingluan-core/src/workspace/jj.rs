//! JJ workspace discovery via the `jj` CLI.
//!
//! No jj-lib: we shell out to `jj workspace list` in the caller's cwd with a
//! robust JSON template and parse the streamed JSON string pairs. The write
//! operations (add/forget) are thin, pre-checked wrappers: callers perform
//! semantic validation (name/directory checks) and rely on jj for the rest.

use std::path::{Path, PathBuf};
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
            // Errors must stay plain regardless of the user's global jj
            // color config (e.g. ui.color = "always"), since we embed jj's
            // stderr in machine-readable errors.
            "--color=never",
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

/// Absolute path of the repository containing the caller's cwd
/// (`jj root`).
pub fn jj_root() -> Result<PathBuf, WorkspaceError> {
    let output = Command::new("jj")
        .args(["--no-pager", "--color=never", "root"])
        .output()
        .map_err(|e| WorkspaceError::JjSpawn {
            message: e.to_string(),
        })?;
    if !output.status.success() {
        return Err(WorkspaceError::JjCommandFailed {
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}

/// Create a workspace at `destination` registered as `name`.
///
/// Callers pre-check name/destination (registered names, non-empty dirs);
/// jj still enforces both, and its error surfaces as `JjCommandFailed`.
/// Not `--ignore-working-copy`: creating a workspace writes to the repo.
pub fn add_workspace(
    destination: &Path,
    name: &str,
    revisions: &[String],
) -> Result<(), WorkspaceError> {
    let mut args = vec![
        "--no-pager".to_owned(),
        "--color=never".to_owned(),
        "workspace".to_owned(),
        "add".to_owned(),
        destination.to_string_lossy().into_owned(),
        format!("--name={name}"),
    ];
    for rev in revisions {
        args.push(format!("--revision={rev}"));
    }
    run_jj(&args)
}

/// Stop tracking a workspace by name. The workspace directory is untouched.
///
/// jj exits 0 even for unknown names (prints "Nothing changed"), so callers
/// must pre-check against `list_jj_workspaces`.
pub fn forget_workspace(name: &str) -> Result<(), WorkspaceError> {
    run_jj(&["--no-pager", "--color=never", "workspace", "forget", name])
}

/// Whether the working-copy commit at `root` is empty (no uncommitted
/// changes). Used to guard `--purge` directory deletion.
///
/// Deliberately NOT `--ignore-working-copy`: without a snapshot, `@` is the
/// last committed state and every probe would report clean.
pub fn workspace_clean(root: &Path) -> Result<bool, WorkspaceError> {
    let output = Command::new("jj")
        .args(["--no-pager", "--color=never", "-R"])
        .arg(root)
        .args(["log", "-r", "@", "--no-graph", "-T", "empty"])
        .output()
        .map_err(|e| WorkspaceError::JjSpawn {
            message: e.to_string(),
        })?;
    if !output.status.success() {
        return Err(WorkspaceError::JjCommandFailed {
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim() == "true")
}

/// Run jj with `--no-pager --color=never` and the given args; map failure to
/// `JjCommandFailed` carrying trimmed stderr.
fn run_jj(args: &[impl AsRef<str>]) -> Result<(), WorkspaceError> {
    let mut command = Command::new("jj");
    command.arg("--no-pager").arg("--color=never");
    command.args(args.iter().map(|a| a.as_ref()));
    let output = command.output().map_err(|e| WorkspaceError::JjSpawn {
        message: e.to_string(),
    })?;
    if !output.status.success() {
        return Err(WorkspaceError::JjCommandFailed {
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(())
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
