use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use qingluan_protocol::{ApiResponse, CreateTaskRequest, HealthResponse, TaskEvent, TaskId};
use serde::Deserialize;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use uuid::Uuid;

/// Daemon configuration.
#[derive(Debug, Clone, Deserialize)]
struct DaemonConfig {
    host: String,
    port: u16,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 47129,
        }
    }
}

/// Shared application state.
#[derive(Clone)]
struct AppState {
    version: String,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let config = DaemonConfig::default();
    let state = AppState {
        version: qingluan_core::version().to_string(),
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/tasks", post(create_task))
        .route("/tasks/{id}", get(get_task))
        .route("/tasks/{id}/events", get(get_task_events))
        .route("/sandboxes", post(create_sandbox))
        .layer(CorsLayer::permissive())
        .with_state(Arc::new(state));

    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!("qingluan-daemon listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
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
