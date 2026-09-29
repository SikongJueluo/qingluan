use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use console::{Alignment, Style, Term, measure_text_width, pad_str, style, truncate_str};
use dialoguer::{Confirm, FuzzySelect, Input, theme::ColorfulTheme};
use qingluan_complexity::{Distribution, FunctionMetrics, ScanReport, distribution};
use qingluan_core::workspace::{
    SessionSummary, WorkspaceCatalog, WorkspaceSummary, add_workspace, discover, forget_workspace,
    jj_root, list_jj_workspaces, parse_iso8601_ms, workspace_clean,
};
use qingluan_protocol::{ApiResponse, HealthResponse};
use reqwest::Client;
use serde::{Deserialize, Serialize};

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

    /// Start a code review session; the daemon does the actual work
    /// (diff, web UI) — this is a thin entry that prints the URL.
    Review {
        #[command(subcommand)]
        action: Option<ReviewAction>,

        /// Directory to review (any path inside a jj repository).
        dir: Option<PathBuf>,

        /// Base revision (jj revset; default `main`).
        #[arg(long)]
        from: Option<String>,

        /// Target revision (jj revset; default `@`).
        #[arg(long)]
        to: Option<String>,

        /// Open the review URL in the system browser.
        #[arg(long)]
        open: bool,

        /// Machine-readable JSON on stdout.
        #[arg(long)]
        json: bool,
    },

    /// Function-level complexity scan (local; no daemon involved).
    ///
    /// Prints a distribution summary plus the worst K functions. With no
    /// PATH, scans the current directory.
    Complexity {
        /// Files or directories to scan (default: the current directory).
        paths: Vec<PathBuf>,

        /// List every function instead of the worst K.
        #[arg(long)]
        all: bool,

        /// List only functions above the configured thresholds.
        #[arg(long)]
        threshold: bool,

        /// Metric to rank by.
        #[arg(long, value_enum, default_value_t = ComplexitySort::Cognitive)]
        sort: ComplexitySort,

        /// Rows in the human-readable table (config: complexity.top).
        #[arg(long)]
        top: Option<usize>,

        /// Machine-readable JSON on stdout; always complete, never truncated.
        #[arg(long)]
        json: bool,

        /// Print only the summary and distribution lines.
        #[arg(long)]
        quiet: bool,
    },

    /// Run the Qingluan daemon (HTTP/web plus terminal gRPC) in the foreground.
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },

    /// Generate shell completions (bash, fish, zsh, elvish, powershell)
    Completions {
        /// Shell to generate completions for
        shell: clap_complete::Shell,
    },
}

/// Ranking metric for `qingluan complexity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ComplexitySort {
    /// Sonar cognitive complexity (nesting-sensitive).
    Cognitive,
    /// McCabe cyclomatic complexity.
    Cc,
    /// Non-blank, non-comment lines.
    Nloc,
}

impl ComplexitySort {
    fn label(self) -> &'static str {
        match self {
            Self::Cognitive => "cognitive",
            Self::Cc => "cc",
            Self::Nloc => "nloc",
        }
    }
}

#[derive(Subcommand, Debug)]
enum DaemonAction {
    /// Start the daemon (binds daemon.host:daemon.port from the config).
    Start {
        /// Override daemon.host from the config (e.g. 0.0.0.0 for LAN).
        #[arg(long)]
        host: Option<String>,

        /// Override daemon.port from the config.
        #[arg(long)]
        port: Option<u16>,
    },
}

#[derive(Subcommand, Debug)]
enum ReviewAction {
    /// Export a review session's comments as markdown (agent handoff).
    Export {
        /// Review session id.
        id: String,
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
            // Config is loaded lazily per action: workspace list/open keep
            // working when the config file is broken (only the open-flow
            // `✚ new workspace` entry needs it, same as `workspace add`).
            cmd_workspace(action);
        }
        Commands::Review {
            action,
            dir,
            from,
            to,
            open,
            json,
        } => {
            let daemon_url = resolve_daemon_url(cli.daemon_url.as_deref());
            match action {
                Some(ReviewAction::Export { id }) => {
                    cmd_review_export(&daemon_url, &id).await;
                }
                None => {
                    let Some(dir) = dir else {
                        machine_error(
                            "missing_directory",
                            "pass the directory to review: qingluan review <dir>",
                        );
                    };
                    cmd_review(
                        &daemon_url,
                        &dir,
                        from.as_deref(),
                        to.as_deref(),
                        open,
                        json,
                    )
                    .await;
                }
            }
        }
        Commands::Complexity {
            paths,
            all,
            threshold,
            sort,
            top,
            json,
            quiet,
        } => cmd_complexity(&paths, all, threshold, sort, top, json, quiet),
        Commands::Daemon { action } => {
            // Same hard-fail semantics the daemon binary used to have:
            // malformed config must abort startup, never fall back.
            let mut config = qingluan_config::load().unwrap_or_else(|e| {
                eprintln!("invalid configuration: {e}");
                std::process::exit(1);
            });
            match action {
                DaemonAction::Start { host, port } => {
                    if let Some(host) = host {
                        config.daemon.host = host;
                    }
                    if let Some(port) = port {
                        config.daemon.port = port;
                    }
                    if let Err(e) = qingluan_daemon::serve(config).await {
                        eprintln!("daemon failed: {e}");
                        std::process::exit(1);
                    }
                }
            }
        }
        Commands::Completions { shell } => {
            // Buffer, then write once: a closed pipe (`| head`) exits quietly
            // instead of panicking inside the generator.
            let mut buf: Vec<u8> = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "qingluan", &mut buf);
            use std::io::Write as _;
            let _ = std::io::stdout().lock().write_all(&buf);
        }
    }
}

/// Explicit `--daemon-url`, else `http://<host>:<port>` from the loaded
/// config. Malformed config is a hard error (never silent defaults).
fn resolve_daemon_url(daemon_url: Option<&str>) -> String {
    daemon_url.map(str::to_owned).unwrap_or_else(|| {
        let config = qingluan_config::load().unwrap_or_else(|e| machine_error("config_invalid", e));
        // A wildcard bind is a server-side notion; clients on the same
        // machine always reach the daemon through loopback.
        let host = match config.daemon.host.as_str() {
            "0.0.0.0" | "::" | "" => "127.0.0.1",
            h => h,
        };
        format!("http://{}:{}", host, config.daemon.port)
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

/// One selectable entry of the level-2 session selector.
#[derive(Debug, Clone, PartialEq)]
enum SessionChoice {
    /// Start a fresh Pi session in the selected workspace.
    New,
    /// Resume this Pi session file in the selected workspace.
    Resume { file: String },
}

/// Visible-row cap for the interactive fuzzy selectors. Unbounded lists
/// repaint the whole screen on every keystroke (flickering once they exceed
/// the terminal height), so selectors stay paginated.
const SELECTOR_MAX_ROWS: usize = 15;

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

/// `workspace add`: shared creation path plus command output.
fn cmd_workspace_add(name: &str, revision: Option<&str>, at: Option<PathBuf>, json: bool) {
    let destination = create_workspace(name, revision, at.as_deref())
        .unwrap_or_else(|e| machine_error(e.code, e.message));

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

/// Failure of the shared workspace-creation path: a machine-readable
/// code plus a human message.
struct CreateError {
    code: &'static str,
    message: String,
}

/// Shared creation path of `workspace add` and the open-flow `✚ new
/// workspace` entry: pre-check name and destination, place the directory,
/// delegate to `jj workspace add`. Returns the new workspace root.
fn create_workspace(
    name: &str,
    revision: Option<&str>,
    at: Option<&Path>,
) -> Result<PathBuf, CreateError> {
    let config = qingluan_config::load().map_err(|e| CreateError {
        code: "config_invalid",
        message: e.to_string(),
    })?;

    let registered = list_jj_workspaces().map_err(|e| CreateError {
        code: e.code(),
        message: e.to_string(),
    })?;
    if registered.iter().any(|ws| ws.name == name) {
        return Err(CreateError {
            code: "workspace_exists",
            message: format!("workspace named '{name}' already exists in this repository"),
        });
    }

    let repo_root = jj_root().map_err(|e| CreateError {
        code: e.code(),
        message: e.to_string(),
    })?;
    let destination = compute_destination(&config.workspace.root, &repo_root, name, at);

    // Parent directories are ours to create; the destination itself must be
    // an empty (or absent) directory — jj rejects non-empty ones, but we
    // pre-check to keep the error structured.
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CreateError {
            code: "mkdir_failed",
            message: format!("failed to create {}: {e}", parent.display()),
        })?;
    }
    if destination.is_dir() {
        let empty = std::fs::read_dir(&destination).is_ok_and(|mut d| d.next().is_none());
        if !empty {
            return Err(CreateError {
                code: "destination_not_empty",
                message: format!(
                    "destination {} exists and is not empty",
                    destination.display()
                ),
            });
        }
    } else if destination.exists() {
        return Err(CreateError {
            code: "destination_not_empty",
            message: format!(
                "destination {} exists and is not a directory",
                destination.display()
            ),
        });
    } else {
        std::fs::create_dir(&destination).map_err(|e| CreateError {
            code: "mkdir_failed",
            message: format!("failed to create {}: {e}", destination.display()),
        })?;
    }

    let revisions: Vec<String> = revision.map(|r| vec![r.to_owned()]).unwrap_or_default();
    add_workspace(&destination, name, &revisions).map_err(|e| CreateError {
        code: e.code(),
        message: e.to_string(),
    })?;
    Ok(destination)
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
    let now_ms = unix_now_ms();
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
    let max_title = title_col_width(cols);
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

/// Title-column budget: terminal width minus the fixed columns (indent,
/// gaps, msgs, time), clamped to a readable range.
fn title_col_width(cols: usize) -> usize {
    cols.saturating_sub(2 + MSGS_COL + 2 + TIME_COL + 2)
        .clamp(16, 60)
}

/// Current unix time in milliseconds (0 on clock skew).
fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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

/// `workspace open`: two-level interactive flow. Level 1 picks a
/// workspace (or creates one via the trailing `✚ new workspace` entry,
/// which enters Pi straight away — a fresh workspace needs no session
/// pick); level 2 picks one of that workspace's sessions (or a new one);
/// a flat all-sessions list grows past the terminal and flickers.
fn cmd_workspace_open() {
    let catalog = match discover(None) {
        Ok(catalog) => catalog,
        Err(e) => machine_error(e.code(), e),
    };

    let labels = workspace_open_labels(&catalog.workspaces, std::env::var("HOME").ok().as_deref());
    let create_index = labels.len() - 1;

    loop {
        // Level 1: pick a workspace.
        let index = match select(&labels, "Select a workspace") {
            Some(index) => index,
            // Esc / q: cancelled, not an error.
            None => std::process::exit(0),
        };
        if index == create_index {
            match create_workspace_flow() {
                // Fresh workspace: straight into a new Pi session.
                Some(root) => launch_pi(&root, &[]),
                // Cancelled: back to the workspace selector.
                None => continue,
            }
        }
        let ws = &catalog.workspaces[index];

        // Unavailable roots cannot host Pi; keep the reason visible, retry.
        if !ws.available {
            let reason = ws.unavailable_reason.as_deref().unwrap_or("unavailable");
            eprintln!("× {}: {reason}", ws.name);
            continue;
        }

        // Level 2: pick a session within the workspace.
        match choose_session(ws) {
            SessionPick::Cancelled => continue, // Esc: back to level 1.
            SessionPick::New => launch_pi(&ws.root, &[]),
            SessionPick::Resume { file } => launch_pi(&ws.root, &["--session", &file]),
        }
    }
}

/// Outcome of the level-2 session selector.
enum SessionPick {
    /// Esc / q: return to the workspace selector.
    Cancelled,
    /// Start a fresh Pi session.
    New,
    /// Resume this session file.
    Resume { file: String },
}

/// Open-flow `✚ new workspace` entry: prompt for a name, create the
/// workspace with `workspace add` defaults, return its root. Ctrl+C
/// cancels back to the workspace selector; creation failures are
/// reported and the prompt retried.
fn create_workspace_flow() -> Option<String> {
    loop {
        // dialoguer 0.11's Input has no opt variant: Esc is swallowed by
        // the key loop, and Ctrl+C surfaces as an Err — treat that as a
        // cancel back to the selector.
        let name = match Input::<String>::with_theme(&ColorfulTheme::default())
            .with_prompt("New workspace name (Ctrl+C: cancel)")
            .interact_text()
        {
            Ok(name) => name.trim().to_owned(),
            Err(_) => return None,
        };
        if name.is_empty() {
            eprintln!("{}", style("× workspace name must not be empty").red());
            continue;
        }
        match create_workspace(&name, None, None) {
            Ok(destination) => {
                println!(
                    "added workspace {} at {}",
                    style(&name).bold(),
                    destination.display()
                );
                return Some(destination.to_string_lossy().into_owned());
            }
            Err(e) => eprintln!("{}", style(format!("× {}", e.message)).red()),
        }
    }
}

/// Level-2 selector: one workspace's sessions plus `✚ new session`.
fn choose_session(ws: &WorkspaceSummary) -> SessionPick {
    let (labels, choices) = session_choices(&ws.sessions, term_cols(), unix_now_ms());
    let prompt = format!("{}: pick a session (Esc: back)", ws.name);
    match select(&labels, &prompt).map(|index| &choices[index]) {
        Some(SessionChoice::New) => SessionPick::New,
        Some(SessionChoice::Resume { file }) => SessionPick::Resume { file: file.clone() },
        None => SessionPick::Cancelled,
    }
}

/// Run one fuzzy-select round; `None` = cancelled (Esc / q).
fn select(labels: &[String], prompt: &str) -> Option<usize> {
    FuzzySelect::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .items(labels)
        .default(0)
        .max_length(SELECTOR_MAX_ROWS)
        .interact_opt()
        .unwrap_or_else(|e| machine_error("selector_failed", e))
}

/// Level-1 selector labels: one row per workspace, name column aligned,
/// session count (or the unavailable reason) on the right.
fn workspace_labels(workspaces: &[WorkspaceSummary], home: Option<&str>) -> Vec<String> {
    let name_w = workspaces
        .iter()
        .map(|ws| measure_text_width(&ws.name))
        .max()
        .unwrap_or(0);
    workspaces
        .iter()
        .map(|ws| {
            let name = pad_str(&ws.name, name_w, Alignment::Left, None);
            let root = tilde(&ws.root, home);
            if ws.available {
                let suffix = match ws.sessions.len() {
                    0 => "no sessions".to_owned(),
                    1 => "1 session".to_owned(),
                    n => format!("{n} sessions"),
                };
                format!("{name}  {root}  {suffix}")
            } else {
                let reason = ws.unavailable_reason.as_deref().unwrap_or("unavailable");
                format!("{name}  {root}  × {reason}")
            }
        })
        .collect()
}

/// Trailing entry of the level-1 open selector: create a workspace.
const NEW_WORKSPACE_LABEL: &str = "✚ new workspace";

/// Level-1 selector rows for the open flow: one row per workspace (see
/// [`workspace_labels`]) plus a trailing `✚ new workspace` entry, mirroring
/// the session selector's `✚ new session`. An empty catalog still offers
/// creation instead of erroring.
fn workspace_open_labels(workspaces: &[WorkspaceSummary], home: Option<&str>) -> Vec<String> {
    let mut labels = workspace_labels(workspaces, home);
    labels.push(NEW_WORKSPACE_LABEL.to_owned());
    labels
}

/// Level-2 selector rows for one workspace: aligned three-column session
/// lines (title, msgs, relative time) plus a trailing `✚ new session`
/// entry. Rows fit the terminal width (wrapping breaks dialoguer's repaint
/// math); duplicate rows get `[#n]` suffixes.
fn session_choices(
    sessions: &[SessionSummary],
    cols: usize,
    now_ms: u64,
) -> (Vec<String>, Vec<SessionChoice>) {
    let title_w = sessions
        .iter()
        .map(|s| measure_text_width(&single_line(&s.title)))
        .max()
        .unwrap_or(0)
        .min(title_col_width(cols));

    let mut seen: HashMap<String, u32> = HashMap::new();
    let mut labels: Vec<String> = Vec::new();
    let mut choices: Vec<SessionChoice> = Vec::new();
    for session in sessions {
        let single = single_line(&session.title);
        let title = truncate_str(&single, title_w, "…");
        let row = format!(
            "{}  {}  {}",
            pad_str(&title, title_w, Alignment::Left, None),
            pad_str(
                &format!("{} msgs", session.message_count),
                MSGS_COL,
                Alignment::Right,
                None,
            ),
            pad_str(
                &relative_time(&session.modified, now_ms),
                TIME_COL,
                Alignment::Right,
                None,
            ),
        );
        labels.push(unique_label(&mut seen, row));
        choices.push(SessionChoice::Resume {
            file: session.file.clone(),
        });
    }
    labels.push("✚ new session".to_owned());
    choices.push(SessionChoice::New);
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

/// `POST /reviews` response body.
#[derive(Debug, Deserialize)]
struct CreateReviewResponse {
    id: String,
}

/// Wire shape of a review comment (daemon `ReviewComment`, camelCase).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewCommentDto {
    file: String,
    side: String,
    line_from: u32,
    line_to: u32,
    author: String,
    content: String,
}

/// `qingluan review <dir>`: create a session, print its URL.
async fn cmd_review(
    daemon_url: &str,
    dir: &Path,
    from: Option<&str>,
    to: Option<&str>,
    open: bool,
    json: bool,
) {
    // Absolute path: the daemon may run from anywhere; also lexical (no
    // symlink resolution) like the workspace tooling.
    let abs = qingluan_core::workspace::lexical_absolute(dir);
    let client = Client::new();
    let response = client
        .post(format!("{daemon_url}/reviews"))
        .json(&serde_json::json!({ "path": abs, "from": from, "to": to }))
        .send()
        .await;

    let body = match response {
        Ok(resp) => match resp.json::<ApiResponse<CreateReviewResponse>>().await {
            Ok(body) if body.ok => body,
            Ok(body) => {
                let error = body
                    .error
                    .as_ref()
                    .map(|e| format!("{}: {}", e.code, e.message))
                    .unwrap_or_else(|| "unknown error".into());
                machine_error("review_create_failed", error);
            }
            Err(e) => machine_error(
                "parse_error",
                format!("failed to parse review response: {e}"),
            ),
        },
        Err(e) => machine_error(
            "daemon_unreachable",
            format!(
                "Daemon is not running at {daemon_url} ({e}). Start it with: qingluan daemon start"
            ),
        ),
    };
    let id = body.data.expect("ok response carries data").id;
    let from = from.unwrap_or("main");
    let to = to.unwrap_or("@");
    let url = format!("{daemon_url}/review/{id}?from={from}&to={to}");

    if open && let Err(e) = std::process::Command::new("xdg-open").arg(&url).spawn() {
        machine_error(
            "open_failed",
            format!("cannot open browser via xdg-open: {e}"),
        );
    }

    if json {
        println!(
            "{}",
            serde_json::json!({ "ok": true, "id": id, "url": url })
        );
    } else {
        // First line is the bare URL (agent-friendly); human text follows.
        println!("{url}");
        println!(
            "review session {id} ({from}..{to}) — comment in the browser, then `qingluan review export {id}`"
        );
    }
}

/// `qingluan review export <id>`: print comments as markdown handoff.
async fn cmd_review_export(daemon_url: &str, id: &str) {
    let client = Client::new();
    let response = client
        .get(format!("{daemon_url}/reviews/{id}/comments"))
        .send()
        .await;
    let comments = match response {
        Ok(resp) => match resp.json::<ApiResponse<Vec<ReviewCommentDto>>>().await {
            Ok(body) if body.ok => body.data.unwrap_or_default(),
            Ok(body) => {
                let error = body
                    .error
                    .as_ref()
                    .map(|e| format!("{}: {}", e.code, e.message))
                    .unwrap_or_else(|| "unknown error".into());
                machine_error("review_export_failed", error);
            }
            Err(e) => machine_error("parse_error", format!("failed to parse comments: {e}")),
        },
        Err(e) => machine_error(
            "daemon_unreachable",
            format!(
                "Daemon is not running at {daemon_url} ({e}). Start it with: qingluan daemon start"
            ),
        ),
    };
    print!("{}", comments_markdown(&comments));
}

/// Render comments as the markdown agent-handoff document.
///
/// Format (mirrored by the web UI's 复制为 Markdown):
/// `# Review comments` / `## <path>` / `- [<side> L<from>-L<to>] <author>: <content>`
/// with multiline content indented two spaces.
fn comments_markdown(comments: &[ReviewCommentDto]) -> String {
    let mut sorted: Vec<&ReviewCommentDto> = comments.iter().collect();
    sorted.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line_from.cmp(&b.line_from))
            .then(a.line_to.cmp(&b.line_to))
    });

    let mut out = String::from("# Review comments\n\n");
    let mut current_file: Option<&str> = None;
    for comment in sorted {
        if current_file != Some(comment.file.as_str()) {
            if current_file.is_some() {
                out.push('\n');
            }
            out.push_str(&format!("## {}\n\n", comment.file));
            current_file = Some(comment.file.as_str());
        }
        let content = comment.content.replace('\n', "\n  ");
        out.push_str(&format!(
            "- [{} L{}-L{}] {}: {}\n",
            comment.side, comment.line_from, comment.line_to, comment.author, content
        ));
    }
    out
}

/// One function plus the file it lives in, ready for display or JSON.
#[derive(Debug, Clone)]
struct ComplexityRow {
    path: String,
    function: FunctionMetrics,
}

/// `qingluan complexity [PATH...]`: local scan, summary plus worst K.
///
/// Deliberately daemon-free: the engine is a pure function over file contents,
/// so a round trip would only add latency and a failure mode.
fn cmd_complexity(
    paths: &[PathBuf],
    all: bool,
    threshold_only: bool,
    sort: ComplexitySort,
    top: Option<usize>,
    json: bool,
    quiet: bool,
) {
    let config = qingluan_config::load().unwrap_or_else(|e| machine_error("config_invalid", e));
    let settings = config.complexity;

    // Paths are reported relative to the invocation directory, so output does
    // not depend on the argument shape the user picked.
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut options = qingluan_complexity::ScanOptions::new(&root);
    if !paths.is_empty() {
        options.paths = paths.to_vec();
    }
    options.include = settings.include.clone();
    options.exclude = settings.exclude.clone();

    let report = qingluan_complexity::scan(&options).unwrap_or_else(|error| {
        let code = match error.kind() {
            std::io::ErrorKind::NotFound => "path_not_found",
            std::io::ErrorKind::InvalidInput => "invalid_glob",
            _ => "scan_failed",
        };
        machine_error(code, error);
    });
    if report.files.is_empty() {
        machine_error(
            "no_files",
            format!("no analyzable files under {}", report.root.display()),
        );
    }

    let mut rows: Vec<ComplexityRow> = report
        .files
        .iter()
        .flat_map(|file| {
            let path = qingluan_complexity::scan::relative_path(&report.root, &file.path);
            file.functions.iter().map(move |function| ComplexityRow {
                path: path.clone(),
                function: function.clone(),
            })
        })
        .collect();

    // Distributions describe the whole scan, not the filtered view: "is 87 an
    // outlier or the norm?" has to be answered before any threshold is applied.
    let cognitive_distribution = distribution(
        &rows
            .iter()
            .map(|row| row.function.metrics.cognitive)
            .collect::<Vec<_>>(),
        settings.cognitive_threshold,
    );
    let cc_distribution = distribution(
        &rows
            .iter()
            .map(|row| row.function.metrics.cc)
            .collect::<Vec<_>>(),
        settings.cc_threshold,
    );

    if threshold_only {
        rows.retain(|row| {
            row.function.metrics.cc > settings.cc_threshold
                || row.function.metrics.cognitive > settings.cognitive_threshold
        });
    }
    sort_complexity_rows(&mut rows, sort);

    if json {
        // Complete by contract: `--top` never truncates JSON.
        println!(
            "{}",
            complexity_json(&report, &rows, cognitive_distribution, cc_distribution,)
        );
        return;
    }

    let skipped = report.skipped;
    println!(
        "scanned {} files, {} functions (skipped {}: {} generated, {} unsupported, {} too large)",
        report.files.len(),
        report.function_count(),
        skipped.total(),
        skipped.generated,
        skipped.unsupported,
        skipped.too_large,
    );
    println!();
    println!(
        "{}",
        distribution_line(
            "cognitive",
            cognitive_distribution,
            settings.cognitive_threshold
        )
    );
    println!(
        "{}",
        distribution_line("cc", cc_distribution, settings.cc_threshold)
    );

    if quiet {
        return;
    }
    if rows.is_empty() {
        println!();
        println!("no functions above the thresholds");
        return;
    }

    let heading = if all {
        format!("all functions by {}", sort.label())
    } else if threshold_only {
        format!("over threshold, by {}", sort.label())
    } else {
        format!("worst by {}", sort.label())
    };
    let shown = if all {
        rows.len()
    } else {
        top.unwrap_or(settings.top).min(rows.len())
    };

    println!();
    println!("{}", style(format!("{heading}:")).bold());
    println!(
        "{}",
        style(format!(
            "{:>8} {:>5} {:>6} {:>7} {:>8}  location",
            "cog", "cc", "nloc", "params", "nesting"
        ))
        .dim()
    );
    for row in rows.iter().take(shown) {
        let metrics = row.function.metrics;
        println!(
            "{:>8} {:>5} {:>6} {:>7} {:>8}  {}:{}  {}",
            metrics.cognitive,
            metrics.cc,
            metrics.nloc,
            metrics.params,
            metrics.max_nesting,
            row.path,
            row.function.start_line,
            row.function.qualified_name,
        );
    }
}

/// Deterministic ordering: metric desc, then CC desc, then path, then line.
///
/// Repeated runs over unchanged code must produce byte-identical output, or
/// the numbers cannot be eyeballed against a previous run.
fn sort_complexity_rows(rows: &mut [ComplexityRow], sort: ComplexitySort) {
    let primary = |row: &ComplexityRow| match sort {
        ComplexitySort::Cognitive => row.function.metrics.cognitive,
        ComplexitySort::Cc => row.function.metrics.cc,
        ComplexitySort::Nloc => row.function.metrics.nloc,
    };
    rows.sort_by(|a, b| {
        primary(b)
            .cmp(&primary(a))
            .then_with(|| b.function.metrics.cc.cmp(&a.function.metrics.cc))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.function.start_line.cmp(&b.function.start_line))
    });
}

fn distribution_line(label: &str, dist: Distribution, threshold: u32) -> String {
    format!(
        "{label:<9}  p50 {:>4}   p90 {:>4}   p99 {:>4}   max {:>4}   >{threshold}: {}",
        dist.p50, dist.p90, dist.p99, dist.max, dist.over_threshold
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ComplexityReport {
    schema_version: u32,
    root: String,
    scanned: ComplexityScanned,
    distribution: ComplexityDistribution,
    functions: Vec<ComplexityFunction>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ComplexityScanned {
    files: usize,
    functions: usize,
    skipped: qingluan_complexity::SkipStats,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ComplexityDistribution {
    cognitive: Distribution,
    cc: Distribution,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ComplexityFunction {
    path: String,
    #[serde(flatten)]
    function: FunctionMetrics,
}

#[allow(clippy::too_many_arguments)]
fn complexity_json(
    report: &ScanReport,
    rows: &[ComplexityRow],
    cognitive: Distribution,
    cc: Distribution,
) -> String {
    let payload = ComplexityReport {
        schema_version: 1,
        root: report.root.display().to_string(),
        scanned: ComplexityScanned {
            files: report.files.len(),
            functions: report.function_count(),
            skipped: report.skipped,
        },
        distribution: ComplexityDistribution { cognitive, cc },
        functions: rows
            .iter()
            .map(|row| ComplexityFunction {
                path: row.path.clone(),
                function: row.function.clone(),
            })
            .collect(),
    };
    serde_json::to_string(&payload).expect("complexity report is serializable")
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
            format!(
                "Daemon is not running at {daemon_url} ({e}). Start it with: qingluan daemon start"
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qingluan_core::workspace::{SessionSummary, WorkspaceSummary};

    fn complexity_row(path: &str, source: &str) -> ComplexityRow {
        let function = qingluan_complexity::analyze_source(
            qingluan_complexity::Language::Rust,
            source.as_bytes(),
        )
        .into_iter()
        .next()
        .expect("sample has a function");
        ComplexityRow {
            path: path.into(),
            function,
        }
    }

    #[test]
    fn complexity_json_matches_the_documented_shape() {
        let row = complexity_row(
            "src/lib.rs",
            "fn pick(a: u32) -> u32 { if a > 0 && a < 9 { 1 } else { 0 } }",
        );
        let report = ScanReport {
            root: PathBuf::from("/repo"),
            files: vec![qingluan_complexity::FileComplexity {
                path: PathBuf::from("/repo/src/lib.rs"),
                language: qingluan_complexity::Language::Rust,
                functions: vec![row.function.clone()],
            }],
            skipped: qingluan_complexity::SkipStats {
                generated: 1,
                unsupported: 2,
                too_large: 3,
            },
        };
        let json: serde_json::Value = serde_json::from_str(&complexity_json(
            &report,
            std::slice::from_ref(&row),
            distribution(&[1], 15),
            distribution(&[3], 10),
        ))
        .expect("complexity report is JSON");

        assert_eq!(json["schemaVersion"], 1);
        assert_eq!(json["root"], "/repo");
        assert_eq!(json["scanned"]["files"], 1);
        assert_eq!(json["scanned"]["functions"], 1);
        assert_eq!(json["scanned"]["skipped"]["generated"], 1);
        assert_eq!(json["scanned"]["skipped"]["unsupported"], 2);
        assert_eq!(json["scanned"]["skipped"]["tooLarge"], 3);
        assert_eq!(json["distribution"]["cognitive"]["max"], 1);
        assert_eq!(json["distribution"]["cc"]["max"], 3);
        assert_eq!(json["distribution"]["cc"]["overThreshold"], 0);
        assert_eq!(json["functions"][0]["path"], "src/lib.rs");
        assert_eq!(json["functions"][0]["name"], "pick");
        assert_eq!(json["functions"][0]["startLine"], 1);
        assert_eq!(json["functions"][0]["cc"], 3);
        assert_eq!(json["functions"][0]["cognitive"], 3);
        assert_eq!(json["functions"][0]["maxNesting"], 1);
    }

    fn ranked_row(path: &str, line: usize, cc: u32) -> ComplexityRow {
        ComplexityRow {
            path: path.into(),
            function: FunctionMetrics {
                name: "f".into(),
                qualified_name: "f".into(),
                start_line: line,
                end_line: line,
                start_col: 1,
                start_byte: 0,
                end_byte: 1,
                metrics: qingluan_complexity::Metrics {
                    cc,
                    cognitive: 5,
                    nloc: 1,
                    params: 0,
                    max_nesting: 0,
                },
            },
        }
    }

    #[test]
    fn complexity_order_is_metric_then_cc_then_path_then_line() {
        let mut rows = vec![
            ranked_row("b.rs", 1, 2),
            ranked_row("a.rs", 10, 2),
            ranked_row("a.rs", 1, 9),
            ranked_row("a.rs", 2, 2),
        ];
        sort_complexity_rows(&mut rows, ComplexitySort::Cognitive);

        // Equal cognitive everywhere, so CC desc, then path asc, then line asc.
        let order: Vec<(String, usize)> = rows
            .iter()
            .map(|row| (row.path.clone(), row.function.start_line))
            .collect();
        assert_eq!(
            order,
            vec![
                ("a.rs".to_string(), 1),
                ("a.rs".to_string(), 2),
                ("a.rs".to_string(), 10),
                ("b.rs".to_string(), 1),
            ]
        );
    }

    fn comment(file: &str, side: &str, from: u32, to: u32, content: &str) -> ReviewCommentDto {
        ReviewCommentDto {
            file: file.into(),
            side: side.into(),
            line_from: from,
            line_to: to,
            author: "你".into(),
            content: content.into(),
        }
    }

    #[test]
    fn markdown_export_groups_by_file_and_sorts_by_line() {
        let md = comments_markdown(&[
            comment("src/b.rs", "new", 5, 5, "second"),
            comment("src/a.rs", "new", 10, 12, "later in file"),
            comment("src/a.rs", "old", 1, 3, "first"),
        ]);
        assert_eq!(
            md,
            "# Review comments\n\n\
             ## src/a.rs\n\n\
             - [old L1-L3] 你: first\n\
             - [new L10-L12] 你: later in file\n\
             \n## src/b.rs\n\n\
             - [new L5-L5] 你: second\n"
        );
    }

    #[test]
    fn markdown_export_indents_multiline_content() {
        let md = comments_markdown(&[comment("src/a.rs", "new", 1, 1, "line one\nline two")]);
        assert!(md.contains("- [new L1-L1] 你: line one\n  line two\n"));
    }

    #[test]
    fn markdown_export_empty_is_header_only() {
        assert_eq!(comments_markdown(&[]), "# Review comments\n\n");
    }

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
    fn workspace_labels_align_names_and_flag_unavailable() {
        let workspaces = vec![
            workspace(
                "default",
                "/home/me/Projects/qingluan",
                true,
                None,
                vec![session("t", 1, "2026-08-16T10:00:00.000Z")],
            ),
            workspace(
                "gone",
                "/home/me/Projects/gone",
                false,
                Some("workspace root not found on disk"),
                vec![],
            ),
        ];

        let labels = workspace_labels(&workspaces, Some("/home/me"));

        assert_eq!(labels[0], "default  ~/Projects/qingluan  1 session");
        assert_eq!(
            labels[1],
            "gone     ~/Projects/gone  × workspace root not found on disk"
        );
    }

    #[test]
    fn open_labels_append_new_workspace_entry() {
        let workspaces = vec![workspace(
            "default",
            "/home/me/Projects/qingluan",
            true,
            None,
            vec![],
        )];

        let labels = workspace_open_labels(&workspaces, Some("/home/me"));

        assert_eq!(labels.len(), 2);
        assert_eq!(labels[0], "default  ~/Projects/qingluan  no sessions");
        assert_eq!(labels[1], "✚ new workspace");
        // An empty catalog still offers creation instead of erroring.
        assert_eq!(workspace_open_labels(&[], None), vec!["✚ new workspace"]);
    }

    #[test]
    fn session_choices_align_columns_and_append_new_session() {
        let sessions = vec![
            session("implement the frobnicator", 123, "2026-08-16T10:55:34.971Z"),
            session("b", 1, "2026-08-16T07:55:34.971Z"),
        ];
        let now = 1_786_877_734_971u64; // 2026-08-16T10:55:34.971Z

        let (labels, choices) = session_choices(&sessions, 80, now);

        assert_eq!(labels.len(), 3);
        assert_eq!(labels[2], "✚ new session");
        assert_eq!(
            choices[0],
            SessionChoice::Resume {
                file: "/sessions/implement the frobnicator.jsonl".to_owned()
            }
        );
        assert_eq!(choices[2], SessionChoice::New);
        assert!(labels[0].contains("123 msgs"));
        assert!(labels[0].contains("just now"));
        assert!(labels[1].contains("3h ago"));
        // Aligned session rows share one display width.
        assert_eq!(
            measure_text_width(&labels[0]),
            measure_text_width(&labels[1])
        );
        // The whole row fits the terminal budget.
        assert!(measure_text_width(&labels[0]) <= 80);
    }

    #[test]
    fn long_titles_truncate_to_fit_terminal() {
        let long = "x".repeat(200);
        let (labels, choices) = session_choices(
            &[session(&long, 5, "2026-08-16T10:00:00.000Z")],
            60,
            1_786_877_734_971,
        );

        assert_eq!(
            choices[0],
            SessionChoice::Resume {
                file: format!("/sessions/{long}.jsonl")
            }
        );
        assert!(labels[0].contains('…'));
        assert!(measure_text_width(&labels[0]) <= 60);
    }

    #[test]
    fn duplicate_session_rows_get_unique_suffixes() {
        let sessions = vec![
            session("dup", 1, "2026-08-16T10:00:00.000Z"),
            session("dup", 1, "2026-08-16T10:00:00.000Z"),
        ];

        let (labels, choices) = session_choices(&sessions, 80, 1_786_877_734_971);

        assert_eq!(choices[0], choices[1]);
        assert_eq!(choices[2], SessionChoice::New);
        assert_ne!(labels[0], labels[1]);
        assert!(labels[1].ends_with("[#2]"));
    }
}
