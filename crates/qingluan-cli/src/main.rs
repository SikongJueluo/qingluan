use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use console::{Alignment, Style, Term, measure_text_width, pad_str, style, truncate_str};
use dialoguer::{Confirm, FuzzySelect, theme::ColorfulTheme};
use qingluan_core::workspace::{
    WorkspaceCatalog, WorkspaceSummary, add_workspace, discover, forget_workspace, jj_root,
    list_jj_workspaces, parse_iso8601_ms, workspace_clean,
};
use qingluan_protocol::{ApiResponse, HealthResponse};
use reqwest::Client;

/// Qingluan CLI — stable agent entry point for the Qingluan task platform.
///
/// Every command here is backed by a working implementation; task-related
/// commands are intentionally absent until the daemon execution engine lands.
#[derive(Parser, Debug)]
#[command(name = "qingluan", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Daemon base URL (default: http://<daemon.host>:<daemon.port> from
    /// the Qingluan config).
    #[arg(long, global = true)]
    daemon_url: Option<String>,
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

    /// Create a workspace (wraps `jj workspace add`).
    ///
    /// Destination defaults to <workspace root>/<repo name>/<name>; the
    /// root comes from the Qingluan config ([workspace] root, default
    /// ~/Projects/.workspace).
    Add {
        /// Workspace name (also the destination directory name).
        name: String,

        /// Parent revisions for the new working-copy commit (jj revset).
        #[arg(long)]
        revision: Option<String>,

        /// Explicit destination path (overrides the default layout).
        #[arg(long)]
        at: Option<PathBuf>,

        /// Output as machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Stop tracking a workspace (wraps `jj workspace forget`).
    ///
    /// The workspace directory is kept on disk unless --purge is given.
    Remove {
        /// Workspace name to forget.
        name: String,

        /// Also delete the workspace directory (default: forget only).
        #[arg(long)]
        purge: bool,

        /// Skip the dirty-check and the confirmation for --purge.
        #[arg(long)]
        force: bool,

        /// Output as machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Health => {
            let daemon_url = resolve_daemon_url(cli.daemon_url.as_deref());
            cmd_health(&daemon_url).await;
        }
        Commands::Workspace { action } => {
            // No config consumption here: workspace list/open must keep
            // working when the config file is broken.
            cmd_workspace(action);
        }
    }
}

/// Explicit `--daemon-url`, else `http://<host>:<port>` from the loaded
/// config. Malformed config is a hard error (never silent defaults).
fn resolve_daemon_url(daemon_url: Option<&str>) -> String {
    daemon_url.map(str::to_owned).unwrap_or_else(|| {
        let config = qingluan_config::load().unwrap_or_else(|e| machine_error("config_invalid", e));
        format!("http://{}:{}", config.daemon.host, config.daemon.port)
    })
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
        WorkspaceAction::Add {
            name,
            revision,
            at,
            json,
        } => cmd_workspace_add(&name, revision.as_deref(), at, json),
        WorkspaceAction::Remove {
            name,
            purge,
            force,
            json,
        } => cmd_workspace_remove(&name, purge, force, json),
    }
}

/// `workspace add`: pre-check, place the destination, delegate to jj.
fn cmd_workspace_add(name: &str, revision: Option<&str>, at: Option<PathBuf>, json: bool) {
    let config = qingluan_config::load().unwrap_or_else(|e| machine_error("config_invalid", e));

    let registered = list_jj_workspaces().unwrap_or_else(|e| machine_error(e.code(), e));
    if registered.iter().any(|ws| ws.name == name) {
        machine_error(
            "workspace_exists",
            format!("workspace named '{name}' already exists in this repository"),
        );
    }

    let repo_root = jj_root().unwrap_or_else(|e| machine_error(e.code(), e));
    let destination = compute_destination(&config.workspace.root, &repo_root, name, at.as_deref());

    // Parent directories are ours to create; the destination itself must be
    // an empty (or absent) directory — jj rejects non-empty ones, but we
    // pre-check to keep the error structured.
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).unwrap_or_else(|e| {
            machine_error(
                "mkdir_failed",
                format!("failed to create {}: {e}", parent.display()),
            )
        });
    }
    if destination.is_dir() {
        let empty = std::fs::read_dir(&destination).is_ok_and(|mut d| d.next().is_none());
        if !empty {
            machine_error(
                "destination_not_empty",
                format!(
                    "destination {} exists and is not empty",
                    destination.display()
                ),
            );
        }
    } else if destination.exists() {
        machine_error(
            "destination_not_empty",
            format!(
                "destination {} exists and is not a directory",
                destination.display()
            ),
        );
    } else {
        std::fs::create_dir(&destination).unwrap_or_else(|e| {
            machine_error(
                "mkdir_failed",
                format!("failed to create {}: {e}", destination.display()),
            )
        });
    }

    let revisions: Vec<String> = revision.map(|r| vec![r.to_owned()]).unwrap_or_default();
    add_workspace(&destination, name, &revisions).unwrap_or_else(|e| machine_error(e.code(), e));

    if json {
        println!(
            "{}",
            serde_json::json!({
                "ok": true,
                "name": name,
                "root": destination,
                "revision": revision,
            })
        );
    } else {
        println!(
            "added workspace {} at {}",
            style(name).bold(),
            destination.display()
        );
        println!(
            "{}",
            style(format!(
                "cd {} or: qingluan workspace open",
                destination.display()
            ))
            .dim()
        );
    }
}

/// `workspace remove`: forget via jj; optionally purge the directory with
/// a dirty-check and confirmation. Never touches ~/.pi.
#[allow(clippy::too_many_arguments)]
fn cmd_workspace_remove(name: &str, purge: bool, force: bool, json: bool) {
    let registered = list_jj_workspaces().unwrap_or_else(|e| machine_error(e.code(), e));
    // jj forget exits 0 even for unknown names — pre-check for a clean error.
    let Some(ws) = registered.iter().find(|ws| ws.name == name) else {
        machine_error(
            "workspace_not_found",
            format!("no workspace named '{name}' in this repository"),
        );
    };
    let root = PathBuf::from(&ws.root);

    // Informational: associated Pi sessions drop out of `workspace list`
    // once the workspace is forgotten (files stay under ~/.pi, untouched).
    let sessions_affected = discover(None)
        .ok()
        .and_then(|catalog| {
            catalog
                .workspaces
                .into_iter()
                .find(|w| w.name == name)
                .map(|w| w.sessions.len())
        })
        .unwrap_or(0);

    let mut warnings: Vec<String> = Vec::new();
    if let Ok(cwd) = std::env::current_dir()
        && cwd.starts_with(&root)
    {
        warnings.push("you are inside the removed workspace; cd away".to_owned());
    }

    if purge {
        // The dirty-check lets jj snapshot the working copy, so uncommitted
        // changes are actually observed (an --ignore-working-copy probe
        // would always report clean).
        let clean = workspace_clean(&root).unwrap_or_else(|e| machine_error(e.code(), e));
        if !clean && !force {
            machine_error(
                "workspace_dirty",
                format!("workspace '{name}' has uncommitted changes; use --force to purge anyway"),
            );
        }
        if !force {
            if json {
                machine_error(
                    "confirmation_required",
                    "--purge with --json requires --force (no interactive confirmation)",
                );
            }
            let confirmed = Confirm::new()
                .with_prompt(format!("Remove {} and all its contents?", root.display()))
                .interact_opt()
                .unwrap_or(None);
            if confirmed != Some(true) {
                std::process::exit(0); // cancelled, not an error
            }
        }
    }

    forget_workspace(name).unwrap_or_else(|e| machine_error(e.code(), e));

    if purge {
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| {
            machine_error(
                "purge_failed",
                format!(
                    "workspace forgotten, but directory {} could not be removed: {e}",
                    root.display()
                ),
            )
        });
    }

    if json {
        println!(
            "{}",
            serde_json::json!({
                "ok": true,
                "name": name,
                "purged": purge,
                "sessionsAffected": sessions_affected,
                "warnings": warnings,
            })
        );
    } else {
        println!("removed workspace {}", style(name).bold());
        if purge {
            println!("{}", style("workspace directory deleted").dim());
        } else {
            println!(
                "{}",
                style(format!(
                    "directory kept at {} (delete manually if unwanted)",
                    root.display()
                ))
                .dim()
            );
        }
        if sessions_affected > 0 {
            println!(
                "{}",
                style(format!(
                    "{sessions_affected} Pi session(s) no longer listed (files kept under ~/.pi)"
                ))
                .dim()
            );
        }
        for warning in warnings {
            println!("{}", style(warning).red());
        }
    }
}

/// Destination layout for `workspace add`:
/// explicit `--at` wins, else `<config root>/<repo directory name>/<name>`.
fn compute_destination(
    config_root: &Path,
    repo_root: &Path,
    name: &str,
    at: Option<&Path>,
) -> PathBuf {
    at.map(Path::to_path_buf).unwrap_or_else(|| {
        let repo_name = repo_root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("repo");
        config_root.join(repo_name).join(name)
    })
}

/// Width of the right-aligned message-count column in `workspace list`.
const MSGS_COL: usize = 9;
/// Width of the right-aligned time column in `workspace list`.
const TIME_COL: usize = 10;

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

/// Render the catalog for humans: one block per workspace, header line
/// (name + root + status), then aligned session rows (title, messages,
/// relative time). Alignment is display-width aware (CJK safe); ANSI colors
/// are dropped automatically when stdout is not a TTY.
fn print_catalog_human(catalog: &WorkspaceCatalog) {
    if catalog.workspaces.is_empty() {
        println!(
            "{}",
            Style::new()
                .dim()
                .apply_to("No workspaces registered in this repository.")
        );
        return;
    }
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let cols = term_cols();
    for (i, ws) in catalog.workspaces.iter().enumerate() {
        if i > 0 {
            println!();
        }
        print_workspace_block(ws, cols, now_ms);
    }
}

/// Print one workspace header plus its session rows.
fn print_workspace_block(ws: &WorkspaceSummary, cols: usize, now_ms: u64) {
    let root = tilde(&ws.root, std::env::var("HOME").ok().as_deref());
    match ws.available {
        true => {
            let n = ws.sessions.len();
            let suffix = match n {
                0 => "no sessions yet".to_owned(),
                1 => "1 session".to_owned(),
                _ => format!("{n} sessions"),
            };
            println!(
                "{}  {}  {}",
                style(&ws.name).bold(),
                style(&root).dim(),
                style(suffix).dim()
            );
        }
        false => {
            let reason = ws.unavailable_reason.as_deref().unwrap_or("unavailable");
            println!(
                "{}  {}  {}",
                style(&ws.name).red().bold(),
                style(&root).dim(),
                style(format!("× {reason}")).red()
            );
        }
    }

    // Uniform title column: widest single-line title, clamped to fit the
    // terminal (indent + title + 2 gaps + msgs + time).
    let max_title = cols
        .saturating_sub(2 + MSGS_COL + 2 + TIME_COL + 2)
        .clamp(16, 60);
    let rows: Vec<(String, String, String)> = ws
        .sessions
        .iter()
        .map(|s| {
            (
                single_line(&s.title),
                format!("{} msgs", s.message_count),
                relative_time(&s.modified, now_ms),
            )
        })
        .collect();
    let title_w = rows
        .iter()
        .map(|(title, _, _)| measure_text_width(title))
        .max()
        .unwrap_or(0)
        .min(max_title);
    for (title, msgs, time) in rows {
        let title = truncate_str(&title, title_w, "…");
        let line = format!(
            "{}  {}  {}",
            pad_str(&title, title_w, Alignment::Left, None),
            pad_str(&msgs, MSGS_COL, Alignment::Right, None),
            pad_str(&time, TIME_COL, Alignment::Right, None),
        );
        match ws.available {
            true => println!("  {line}"),
            false => println!("{} {}", style("×").red(), style(line).dim()),
        }
    }
}

/// Terminal width in columns; at least 40 when stdout is not a TTY.
fn term_cols() -> usize {
    usize::from(Term::stdout().size().1).max(40)
}

/// Shorten `path` by replacing a home prefix with `~`.
fn tilde(path: &str, home: Option<&str>) -> String {
    let Some(home) = home.filter(|h| !h.is_empty()) else {
        return path.to_owned();
    };
    if path == home {
        return "~".to_owned();
    }
    path.strip_prefix(&format!("{home}/"))
        .map_or_else(|| path.to_owned(), |rest| format!("~/{rest}"))
}

/// Human-friendly age for a UTC ISO-8601 timestamp.
///
/// Relative ("just now", "5m ago", "3h ago", "2d ago") within a week,
/// then the plain `YYYY-MM-DD` date. Unparseable input falls back to the
/// first 10 characters (still `YYYY-MM-DD` when well-formed).
fn relative_time(modified: &str, now_ms: u64) -> String {
    let Some(ms) = parse_iso8601_ms(modified) else {
        return modified.get(..10).unwrap_or(modified).to_owned();
    };
    let diff_min = now_ms.saturating_sub(ms) / 60_000;
    match diff_min {
        0..=1 => "just now".to_owned(),
        2..=59 => format!("{diff_min}m ago"),
        60..=1_439 => format!("{}h ago", diff_min / 60),
        1_440..=10_079 => format!("{}d ago", diff_min / 1_440),
        _ => modified.get(..10).unwrap_or(modified).to_owned(),
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
    let single = single_line(title);
    if single.chars().count() <= max_chars {
        return single;
    }
    let head: String = single.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{head}…")
}

/// Collapse whitespace (incl. newlines) into single spaces.
fn single_line(title: &str) -> String {
    title.split_whitespace().collect::<Vec<_>>().join(" ")
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
    fn destination_defaults_to_config_layout_and_at_overrides() {
        assert_eq!(
            compute_destination(
                Path::new("/ws"),
                Path::new("/home/me/Projects/qingluan"),
                "fix",
                None
            ),
            PathBuf::from("/ws/qingluan/fix")
        );
        assert_eq!(
            compute_destination(
                Path::new("/ws"),
                Path::new("/home/me/Projects/qingluan"),
                "fix",
                Some(Path::new("/elsewhere/x"))
            ),
            PathBuf::from("/elsewhere/x")
        );
    }

    #[test]
    fn relative_time_buckets() {
        let now = 1_786_877_734_971u64; // 2026-08-16T10:55:34.971Z
        assert_eq!(relative_time("2026-08-16T10:55:34.971Z", now), "just now");
        assert_eq!(relative_time("2026-08-16T10:50:34.971Z", now), "5m ago");
        assert_eq!(relative_time("2026-08-16T07:55:34.971Z", now), "3h ago");
        assert_eq!(relative_time("2026-08-14T10:55:34.971Z", now), "2d ago");
        // Beyond a week: plain date, even when parseable.
        assert_eq!(relative_time("2026-07-30T10:55:34.971Z", now), "2026-07-30");
        // Unparseable input: first 10 chars.
        assert_eq!(relative_time("garbage-long", now), "garbage-lo");
        // Clock skew (future timestamp): clamp to just now.
        assert_eq!(relative_time("2026-08-16T11:00:00.000Z", now), "just now");
    }

    #[test]
    fn tilde_shortens_home_prefix_only() {
        assert_eq!(
            tilde("/home/me/Projects/qingluan", Some("/home/me")),
            "~/Projects/qingluan"
        );
        assert_eq!(tilde("/home/me", Some("/home/me")), "~");
        // Prefix must be a path boundary, not a string prefix.
        assert_eq!(tilde("/home/metal/x", Some("/home/me")), "/home/metal/x");
        assert_eq!(tilde("/opt/other", Some("/home/me")), "/opt/other");
        assert_eq!(tilde("/home/me/x", None), "/home/me/x");
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
