//! Existing HTTP surface, retained beside the terminal gRPC listener.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use qingluan_protocol::{ApiResponse, CreateTaskRequest, HealthResponse, TaskEvent, TaskId};
use tower_http::cors::CorsLayer;
use uuid::Uuid;

use crate::{AppState, review, web};

pub fn router(version: impl Into<String>) -> Router {
    let state = AppState {
        version: version.into(),
        ..Default::default()
    };
    Router::new()
        .route("/health", get(health))
        .route("/tasks", post(create_task))
        .route("/tasks/{id}", get(get_task))
        .route("/tasks/{id}/events", get(get_task_events))
        .route("/sandboxes", post(create_sandbox))
        .merge(review::router())
        .fallback(web::fallback)
        .layer(CorsLayer::permissive())
        .with_state(Arc::new(state))
}

async fn health(State(state): State<Arc<AppState>>) -> Json<ApiResponse<HealthResponse>> {
    Json(ApiResponse::success(HealthResponse {
        ok: true,
        version: state.version.clone(),
    }))
}

async fn create_task(Json(payload): Json<CreateTaskRequest>) -> Json<ApiResponse<TaskEvent>> {
    let task_id = TaskId(Uuid::now_v7().to_string());
    tracing::info!(
        task_id = %task_id,
        kind = ?payload.kind,
        provider = ?payload.sandbox.provider,
        "task created"
    );
    Json(ApiResponse::success(TaskEvent::TaskQueued { task_id }))
}

async fn get_task(Path(task_id): Path<String>) -> Json<ApiResponse<serde_json::Value>> {
    Json(ApiResponse::success(serde_json::json!({
        "task_id": task_id,
        "status": "queued",
        "message": "task status endpoint — full implementation pending"
    })))
}

async fn get_task_events(
    Path(task_id): Path<String>,
) -> (StatusCode, Json<ApiResponse<serde_json::Value>>) {
    (
        StatusCode::OK,
        Json(ApiResponse::error(
            "events_not_implemented",
            format!("SSE event stream for task {task_id} not yet implemented"),
        )),
    )
}

async fn create_sandbox() -> Json<ApiResponse<serde_json::Value>> {
    Json(ApiResponse::success(serde_json::json!({
        "sandbox_id": "sandbox-placeholder",
        "provider": "local",
        "status": "ready"
    })))
}
