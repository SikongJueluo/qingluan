pub mod cube;
pub mod local;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Request to create a new sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSandboxRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

/// Handle to a running sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxHandle {
    pub id: String,
}

/// Request to run a command inside a sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunCommandRequest {
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// Result of a command execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Abstract sandbox provider — the interface that all providers implement.
#[async_trait]
pub trait SandboxProvider: Send + Sync {
    /// Create a new sandbox instance.
    async fn create(&self, req: CreateSandboxRequest) -> Result<SandboxHandle>;

    /// Run a command inside the sandbox.
    async fn run_command(&self, sandbox_id: &str, req: RunCommandRequest) -> Result<CommandResult>;

    /// Upload a local workspace directory to the sandbox.
    async fn upload_workspace(&self, sandbox_id: &str, local_path: &str) -> Result<()>;

    /// Download generated artifacts from the sandbox.
    async fn download_artifacts(&self, sandbox_id: &str, paths: Vec<String>) -> Result<()>;

    /// Destroy the sandbox and release resources.
    async fn destroy(&self, sandbox_id: &str) -> Result<()>;
}
