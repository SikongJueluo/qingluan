use clap::{Parser, Subcommand};
use qingluan_protocol::{
    ApiResponse, CreateTaskRequest, HealthResponse, SandboxProfile, SandboxProviderKind, TaskKind,
};
use reqwest::Client;

const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:47129";

/// Qingluan CLI — stable agent entry point for the Qingluan task platform.
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
    /// Check daemon health.
    Health {
        /// Output as machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Create and queue a task.
    Task {
        #[command(subcommand)]
        action: TaskAction,
    },

    /// Open the Qingluan UI.
    Ui,
}

#[derive(Subcommand, Debug)]
enum TaskAction {
    /// Create a task by kind.
    Create {
        /// Workspace root directory.
        #[arg(long, default_value = ".")]
        workspace: String,

        /// Task kind: code-review, build, test, or custom.
        #[arg(long)]
        kind: String,

        /// Sandbox provider: local or cube.
        #[arg(long, default_value = "local")]
        sandbox: String,

        /// Output as machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Run an arbitrary command as a task.
    Run {
        /// Workspace root directory.
        #[arg(long, default_value = ".")]
        workspace: String,

        /// Command to execute inside the sandbox.
        #[arg(long)]
        command: String,

        /// Sandbox provider: local or cube.
        #[arg(long, default_value = "local")]
        sandbox: String,

        /// Output as machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Health { json: _ } => {
            cmd_health(&cli.daemon_url).await;
        }
        Commands::Task { action } => {
            cmd_task(&cli.daemon_url, action).await;
        }
        Commands::Ui => {
            tracing::error!(
                "UI not yet implemented. Run the Tauri desktop app or open the web frontend."
            );
            std::process::exit(1);
        }
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
            Err(e) => {
                tracing::error!("Failed to parse health response: {e}");
                eprintln!(
                    "{{\"ok\":false,\"error\":\"parse_error\",\"message\":\"{}\"}}",
                    e
                );
                std::process::exit(1);
            }
        },
        Err(e) => {
            tracing::error!("Daemon unreachable at {}: {e}", daemon_url);
            eprintln!(
                "{{\"ok\":false,\"error\":\"daemon_unreachable\",\"message\":\"Daemon is not running at {}. Start it with: qingluan-daemon\"}}",
                daemon_url
            );
            std::process::exit(1);
        }
    }
}

async fn cmd_task(daemon_url: &str, action: TaskAction) {
    let (workspace, kind, sandbox_provider, _json) = match action {
        TaskAction::Create {
            workspace,
            kind,
            sandbox,
            json,
        } => (workspace, kind, sandbox, json),
        TaskAction::Run {
            workspace,
            command,
            sandbox,
            json,
        } => {
            // Wrap bare command into CustomCommand
            let kind = format!("custom:{}", command);
            (workspace, kind, sandbox, json)
        }
    };

    let task_kind = match kind.as_str() {
        "code-review" => TaskKind::CodeReview,
        "build" => TaskKind::Build,
        "test" => TaskKind::Test,
        s if s.starts_with("custom:") => {
            let cmd = s.strip_prefix("custom:").unwrap_or("").to_string();
            TaskKind::CustomCommand { command: cmd }
        }
        other => {
            eprintln!(
                "{{\"ok\":false,\"error\":\"invalid_kind\",\"message\":\"Unknown task kind: {}\"}}",
                other
            );
            std::process::exit(1);
        }
    };

    let provider = match sandbox_provider.as_str() {
        "local" => SandboxProviderKind::Local,
        "cube" => SandboxProviderKind::Cube,
        other => {
            eprintln!(
                "{{\"ok\":false,\"error\":\"invalid_provider\",\"message\":\"Unknown provider: {}\"}}",
                other
            );
            std::process::exit(1);
        }
    };

    let req = CreateTaskRequest {
        workspace_path: workspace,
        kind: task_kind,
        sandbox: SandboxProfile {
            provider,
            template: None,
        },
    };

    let client = Client::new();
    match client
        .post(format!("{}/tasks", daemon_url))
        .json(&req)
        .send()
        .await
    {
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Ok(body) => {
                println!("{}", serde_json::to_string_pretty(&body).unwrap());
            }
            Err(e) => {
                eprintln!(
                    "{{\"ok\":false,\"error\":\"parse_error\",\"message\":\"{}\"}}",
                    e
                );
                std::process::exit(1);
            }
        },
        Err(e) => {
            tracing::error!("Daemon unreachable at {}: {e}", daemon_url);
            eprintln!(
                "{{\"ok\":false,\"error\":\"daemon_unreachable\",\"message\":\"Daemon is not running at {}. Start it with: qingluan-daemon\"}}",
                daemon_url
            );
            std::process::exit(1);
        }
    }
}
