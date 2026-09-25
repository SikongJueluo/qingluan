//! Qingluan daemon library: HTTP, embedded web UI, reviews, and terminal gRPC.
//!
//! The daemon is started through the unified CLI entry (`qingluan daemon
//! start`). HTTP serves the task, review, and embedded web surfaces. Terminal
//! execution is exposed only through a mode-0600 Unix socket and generated
//! protobuf types; domain and runtime crates remain independent of tonic/prost.

pub mod backend;
mod http;
pub mod lease;
pub mod review;
mod server;
pub mod service;
pub mod socket;
pub mod web;
pub mod wire;

use review::ReviewStore;
use tokio::signal::unix::{SignalKind, signal};

/// Shared state for HTTP routes and in-memory review sessions.
#[derive(Default)]
pub struct AppState {
    pub version: String,
    /// Live code-review sessions (daemon memory, ADR-0003).
    pub reviews: ReviewStore,
}

pub use server::{DaemonResult, run};

/// Run the complete daemon until SIGINT or SIGTERM.
///
/// The caller supplies the fully layered configuration, including any CLI
/// host/port overrides. Startup failures propagate without falling back to a
/// partial HTTP-only daemon.
pub async fn serve(config: qingluan_config::Config) -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    run(config, shutdown_signal())
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

async fn shutdown_signal() {
    let terminate = async {
        match signal(SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = terminate => {},
    }
}
