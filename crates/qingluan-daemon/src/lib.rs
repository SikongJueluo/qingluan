//! Qingluan daemon library: HTTP API server and review sessions.
//!
//! Lib-only crate — the server is started through the unified CLI entry
//! (`qingluan daemon start`), which calls [`serve`]. A lib target also lets
//! integration tests exercise the review extraction against a real jj
//! subprocess.

pub mod review;
pub mod web;

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use qingluan_protocol::{ApiResponse, CreateTaskRequest, HealthResponse, TaskEvent, TaskId};
use review::ReviewStore;
use tower_http::cors::CorsLayer;
use uuid::Uuid;

/// Shared application state.
#[derive(Default)]
pub struct AppState {
    pub version: String,
    /// Live code-review sessions (daemon memory, ADR-0003).
    pub reviews: ReviewStore,
}

/// Bind the daemon and serve until the process is stopped.
///
/// `tracing` is initialized here (single-purpose app library); bind failures
/// and server errors propagate to the caller as `Err`.
pub async fn serve(host: &str, port: u16) -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let state = AppState {
        version: qingluan_core::version().to_string(),
        ..Default::default()
    };
    let app = Router::new()
        .route("/health", get(health))
        .route("/tasks", post(create_task))
        .route("/tasks/{id}", get(get_task))
        .route("/tasks/{id}/events", get(get_task_events))
        .route("/sandboxes", post(create_sandbox))
        .merge(review::router())
        .fallback(web::fallback)
        .layer(CorsLayer::permissive())
        .with_state(Arc::new(state));

    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("qingluan daemon listening on {addr}");

    // Wildcard binds are invisible in URLs; surface the reachable LAN
    // address so other machines know what to point their CLI at.
    if matches!(host, "0.0.0.0" | "::" | "")
        && let Some(ip) = primary_lan_ip()
    {
        tracing::info!("reachable on the local network: http://{ip}:{port}");
    }

    axum::serve(listener, app).await?;
    Ok(())
}

/// The host's primary outbound IPv4 address, without sending any traffic
/// (a connected-but-unused UDP socket only consults the routing table).
fn primary_lan_ip() -> Option<std::net::IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    // Private-range target so the route resolves via the LAN interface.
    sock.connect("10.255.255.255:1").ok()?;
    Some(sock.local_addr().ok()?.ip())
}

/// GET /health — returns version and ok.
async fn health(State(state): State<Arc<AppState>>) -> Json<ApiResponse<HealthResponse>> {
    Json(ApiResponse::success(HealthResponse {
        ok: true,
        version: state.version.clone(),
    }))
}

/// POST /tasks — create a new task.
async fn create_task(Json(payload): Json<CreateTaskRequest>) -> Json<ApiResponse<TaskEvent>> {
    let task_id = TaskId(Uuid::now_v7().to_string());

    tracing::info!(
        "Task created: {} (kind={:?}, provider={:?})",
        task_id,
        payload.kind,
        payload.sandbox.provider
    );

    Json(ApiResponse::success(TaskEvent::TaskQueued { task_id }))
}

/// GET /tasks/:id — get task status.
async fn get_task(Path(task_id): Path<String>) -> Json<ApiResponse<serde_json::Value>> {
    Json(ApiResponse::success(serde_json::json!({
        "task_id": task_id,
        "status": "queued",
        "message": "task status endpoint — full implementation pending"
    })))
}

/// GET /tasks/:id/events — SSE event stream (placeholder).
async fn get_task_events(
    Path(task_id): Path<String>,
) -> (StatusCode, Json<ApiResponse<serde_json::Value>>) {
    (
        StatusCode::OK,
        Json(ApiResponse::error(
            "events_not_implemented",
            format!("SSE event stream for task {} not yet implemented", task_id),
        )),
    )
}

/// POST /sandboxes — create a sandbox (placeholder).
async fn create_sandbox() -> Json<ApiResponse<serde_json::Value>> {
    Json(ApiResponse::success(serde_json::json!({
        "sandbox_id": "sandbox-placeholder",
        "provider": "local",
        "status": "ready"
    })))
}
