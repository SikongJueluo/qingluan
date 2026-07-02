use serde::{Deserialize, Serialize};

/// Unified API response wrapper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiResponse<T> {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

impl<T> ApiResponse<T> {
    pub fn success(data: T) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(ApiError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}

/// Structured API error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

/// Unique task identifier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TaskId(pub String);

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Request to create a new task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTaskRequest {
    pub workspace_path: String,
    pub kind: TaskKind,
    pub sandbox: SandboxProfile,
}

/// Kinds of tasks the system can execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskKind {
    CodeReview,
    Build,
    Test,
    CustomCommand { command: String },
}

/// Sandbox configuration for a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxProfile {
    pub provider: SandboxProviderKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

/// Which sandbox provider to use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SandboxProviderKind {
    Local,
    Cube,
}

/// Events emitted during task execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TaskEvent {
    TaskQueued { task_id: TaskId },
    SandboxCreating { provider: String },
    SandboxReady { sandbox_id: String },
    CommandStarted { command: String, cwd: String },
    Stdout { line: String },
    Stderr { line: String },
    ArtifactCreated { path: String, kind: String },
    TaskSucceeded,
    TaskFailed { error: String },
}

/// Health check response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub version: String,
}
