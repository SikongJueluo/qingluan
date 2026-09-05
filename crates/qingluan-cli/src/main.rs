use std::collections::HashMap;
use std::process::Command;

use clap::{Parser, Subcommand};
use dialoguer::{FuzzySelect, theme::ColorfulTheme};
use qingluan_core::workspace::{WorkspaceCatalog, discover};
use qingluan_protocol::{ApiResponse, HealthResponse};
use reqwest::Client;

const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:47129";

/// Qingluan CLI — stable agent entry point for the Qingluan task platform.
///
/// Every command here is backed by a working implementation; task-related
/// commands are intentionally absent until the daemon execution engine lands.
#[derive(Parser, Debug)]
#[command(name = "qingluan", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Daemon base URL (default: http://127.0.0.1:47129).
    #[arg(long, global = true, default_value = DEFAULT_DAEMON_URL)]
    daemon_url: String,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Check daemon health (always prints machine-readable JSON).
    Health,

    /// Local workspace and Pi session switching (no daemon involved).
    Workspace {
        #[command(subcommand)]
        action: WorkspaceAction,
    },
}

#[derive(Subcommand, Debug)]
enum WorkspaceAction {
    /// List workspaces of the current JJ repository and their Pi sessions.
    List {
        /// Output as machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Interactively open a Pi session in one of the workspaces.
    Open,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Health => {
            cmd_health(&cli.daemon_url).await;
        }
        Commands::Workspace { action } => {
            cmd_workspace(action);
        }
    }
}

/// Print a machine-readable error to stderr and exit nonzero.
///
/// Keeps stdout clean for machine consumers.
fn machine_error(code: &str, message: impl std::fmt::Display) -> ! {
    let payload = serde_json::json!({
        "ok": false,
        "error": code,
        "message": message.to_string(),
    });
    eprintln!(
        "{}",
        serde_json::to_string(&payload).unwrap_or_else(|_| "{\"ok\":false}".to_owned())
    );
    std::process::exit(1);
}

/// One selectable entry of `workspace open`.
#[derive(Debug, Clone, PartialEq)]
enum SessionChoice {
    /// Start a fresh Pi session in this workspace root.
    New { root: String },
    /// Resume this Pi session file in its workspace root.
    Resume { root: String, file: String },
    /// Informational row for an unavailable workspace/session: selecting it
    /// only prints the reason and reopens the selector.
    Unavailable { name: String, reason: String },
}

fn cmd_workspace(action: WorkspaceAction) {
    match action {
        WorkspaceAction::List { json } => cmd_workspace_list(json),
        WorkspaceAction::Open => cmd_workspace_open(),
    }
}

fn cmd_workspace_list(json: bool) {
    match discover(None) {
        Ok(catalog) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&catalog).expect("catalog serializes")
                );
            } else {
                print_catalog_human(&catalog);
            }
        }
        Err(e) => machine_error(e.code(), e),
    }
}

fn print_catalog_human(catalog: &WorkspaceCatalog) {
    for ws in &catalog.workspaces {
        match &ws.unavailable_reason {
            Some(reason) => println!("{}\t{}\t(unavailable: {})", ws.name, ws.root, reason),
            None => println!("{}\t{}\t{} session(s)", ws.name, ws.root, ws.sessions.len()),
        }
        for session in &ws.sessions {
            println!(
                "  {}\t{} msgs\t{}",
                display_title(&session.title, 80),
                session.message_count,
                session.modified
            );
        }
    }
}

fn cmd_workspace_open() {
    let catalog = match discover(None) {
        Ok(catalog) => catalog,
        Err(e) => machine_error(e.code(), e),
    };

    let (labels, choices) = build_session_choices(&catalog);
    if labels.is_empty() {
        machine_error(
            "no_workspaces",
            "no workspace registered in this repository",
        );
    }

    loop {
        let selection = FuzzySelect::with_theme(&ColorfulTheme::default())
            .with_prompt("Open a Pi session")
            .items(&labels)
            .default(0)
            .interact_opt();

        let index = match selection {
            Ok(Some(index)) => index,
            // Esc / q: cancelled, not an error.
            Ok(None) => std::process::exit(0),
            Err(e) => machine_error("selector_failed", e),
        };

        match &choices[index] {
            // Informational row: show why the workspace is unusable, then
            // reopen the selector so the user can pick something else.
            SessionChoice::Unavailable { name, reason } => {
                eprintln!("× {name}: {reason}");
                continue;
            }
            SessionChoice::New { root } => launch_pi(root, &[]),
            SessionChoice::Resume { root, file } => launch_pi(root, &["--session", file]),
        }
    }
}

/// Build the flat selector labels and their choices for `workspace open`.
///
/// Available workspaces contribute their sessions plus one `✚ new session`
/// entry. An unavailable workspace contributes one informational row per
/// known session so its history remains discoverable; if it has no sessions,
/// the workspace itself contributes one row. None can launch Pi while the
/// root is missing. Labels are globally unique.
fn build_session_choices(catalog: &WorkspaceCatalog) -> (Vec<String>, Vec<SessionChoice>) {
    let mut labels: Vec<String> = Vec::new();
    let mut choices: Vec<SessionChoice> = Vec::new();
    let mut seen: HashMap<String, u32> = HashMap::new();
    for ws in &catalog.workspaces {
        if !ws.available {
            let reason = ws
                .unavailable_reason
                .clone()
                .unwrap_or_else(|| "unavailable".to_owned());
            if ws.sessions.is_empty() {
                labels.push(unique_label(
                    &mut seen,
                    format!("{} ── × {}", ws.name, reason),
                ));
                choices.push(SessionChoice::Unavailable {
                    name: ws.name.clone(),
                    reason,
                });
            } else {
                for session in &ws.sessions {
                    labels.push(unique_label(
                        &mut seen,
                        format!(
                            "{} ── × {} ({} msgs, {}) [{}]",
                            ws.name,
                            display_title(&session.title, 80),
                            session.message_count,
                            session.modified,
                            reason
                        ),
                    ));
                    choices.push(SessionChoice::Unavailable {
                        name: ws.name.clone(),
                        reason: reason.clone(),
                    });
                }
            }
            continue;
        }
        for session in &ws.sessions {
            labels.push(unique_label(
                &mut seen,
                format!(
                    "{} ── {} ({} msgs, {})",
                    ws.name,
                    display_title(&session.title, 80),
                    session.message_count,
                    session.modified
                ),
            ));
            choices.push(SessionChoice::Resume {
                root: ws.root.clone(),
                file: session.file.clone(),
            });
        }
        labels.push(unique_label(
            &mut seen,
            format!("{} ── ✚ new session", ws.name),
        ));
        choices.push(SessionChoice::New {
            root: ws.root.clone(),
        });
    }
    (labels, choices)
}

/// Make every flat label unique by suffixing a counter on repeats.
fn unique_label(seen: &mut HashMap<String, u32>, label: String) -> String {
    let count = seen.entry(label.clone()).or_insert(0);
    *count += 1;
    if *count == 1 {
        label
    } else {
        format!("{label} [#{count}]")
    }
}

/// Collapse a session title into one short display line.
///
/// Titles fall back to the first user message, which can be a whole
/// document; the selector stays usable with a truncated single line.
fn display_title(title: &str, max_chars: usize) -> String {
    let single_line = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= max_chars {
        return single_line;
    }
    let head: String = single_line
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect();
    format!("{head}…")
}

/// Launch `pi` in the workspace root, resuming `--session <file>` when given.
/// The child inherits the terminal; its exit code becomes ours.
fn launch_pi(root: &str, args: &[&str]) -> ! {
    match Command::new("pi").args(args).current_dir(root).status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(e) => machine_error(
            "spawn_pi_failed",
            format!("failed to launch pi in {root}: {e}"),
        ),
    }
}

async fn cmd_health(daemon_url: &str) {
    let client = Client::new();
    match client.get(format!("{}/health", daemon_url)).send().await {
        Ok(resp) => match resp.json::<ApiResponse<HealthResponse>>().await {
            Ok(body) => {
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
                if !body.ok {
                    std::process::exit(1);
                }
            }
            Err(e) => machine_error(
                "parse_error",
                format!("failed to parse health response: {e}"),
            ),
        },
        Err(e) => machine_error(
            "daemon_unreachable",
            format!("Daemon is not running at {daemon_url} ({e}). Start it with: qingluan-daemon"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qingluan_core::workspace::{SessionSummary, WorkspaceSummary};

    fn session(title: &str, message_count: u32, modified: &str) -> SessionSummary {
        SessionSummary {
            file: format!("/sessions/{title}.jsonl"),
            id: Some(format!("id-{title}")),
            title: title.to_owned(),
            message_count,
            modified: modified.to_owned(),
        }
    }

    fn workspace(
        name: &str,
        root: &str,
        available: bool,
        reason: Option<&str>,
        sessions: Vec<SessionSummary>,
    ) -> WorkspaceSummary {
        WorkspaceSummary {
            name: name.to_owned(),
            root: root.to_owned(),
            available,
            unavailable_reason: reason.map(str::to_owned),
            sessions,
        }
    }

    #[test]
    fn choices_keep_unavailable_sessions_visible() {
        let catalog = WorkspaceCatalog {
            schema_version: 1,
            workspaces: vec![
                workspace(
                    "default",
                    "/w/main",
                    true,
                    None,
                    vec![session("t", 1, "2026-08-16T10:00:00.000Z")],
                ),
                workspace(
                    "gone",
                    "/w/gone",
                    false,
                    Some("workspace root not found on disk"),
                    vec![session("stale", 2, "2026-08-16T11:00:00.000Z")],
                ),
            ],
        };

        let (labels, choices) = build_session_choices(&catalog);

        assert_eq!(
            labels,
            vec![
                "default ── t (1 msgs, 2026-08-16T10:00:00.000Z)".to_owned(),
                "default ── ✚ new session".to_owned(),
                "gone ── × stale (2 msgs, 2026-08-16T11:00:00.000Z) [workspace root not found on disk]".to_owned(),
            ]
        );
        assert_eq!(
            choices[2],
            SessionChoice::Unavailable {
                name: "gone".into(),
                reason: "workspace root not found on disk".into(),
            }
        );
        assert!(matches!(choices[0], SessionChoice::Resume { .. }));
        assert!(matches!(choices[1], SessionChoice::New { .. }));
        assert!(labels.iter().any(|label| label.contains("stale")));
    }

    #[test]
    fn duplicate_labels_get_unique_suffixes() {
        let catalog = WorkspaceCatalog {
            schema_version: 1,
            workspaces: vec![
                workspace(
                    "same",
                    "/w/a",
                    true,
                    None,
                    vec![
                        session("dup", 1, "2026-08-16T10:00:00.000Z"),
                        session("dup", 1, "2026-08-16T10:00:00.000Z"),
                    ],
                ),
                workspace(
                    "same",
                    "/w/b",
                    false,
                    Some("workspace root not found on disk"),
                    vec![],
                ),
                workspace(
                    "same",
                    "/w/b",
                    false,
                    Some("workspace root not found on disk"),
                    vec![],
                ),
            ],
        };

        let (labels, choices) = build_session_choices(&catalog);

        assert_eq!(
            labels,
            vec![
                "same ── dup (1 msgs, 2026-08-16T10:00:00.000Z)".to_owned(),
                "same ── dup (1 msgs, 2026-08-16T10:00:00.000Z) [#2]".to_owned(),
                "same ── ✚ new session".to_owned(),
                "same ── × workspace root not found on disk".to_owned(),
                "same ── × workspace root not found on disk [#2]".to_owned(),
            ]
        );
        assert_eq!(choices.len(), labels.len());
        assert_eq!(
            choices[4],
            SessionChoice::Unavailable {
                name: "same".into(),
                reason: "workspace root not found on disk".into(),
            }
        );
    }
}
