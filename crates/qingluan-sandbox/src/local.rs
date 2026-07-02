use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::process::Command;

use crate::{CommandResult, CreateSandboxRequest, RunCommandRequest, SandboxHandle, SandboxProvider};

/// A sandbox provider that runs commands locally via `tokio::process::Command`.
pub struct LocalSandboxProvider;

#[async_trait]
impl SandboxProvider for LocalSandboxProvider {
    async fn create(&self, _req: CreateSandboxRequest) -> Result<SandboxHandle> {
        Ok(SandboxHandle {
            id: "local".to_string(),
        })
    }

    async fn run_command(&self, _sandbox_id: &str, req: RunCommandRequest) -> Result<CommandResult> {
        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(&req.command);
            c
        } else {
            let mut c = Command::new("sh");
            c.arg("-c").arg(&req.command);
            c
        };

        if let Some(cwd) = &req.cwd {
            cmd.current_dir(cwd);
        }

        let output = cmd.output().await.context("failed to execute local command")?;

        Ok(CommandResult {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }

    async fn upload_workspace(&self, _sandbox_id: &str, _local_path: &str) -> Result<()> {
        // Local sandbox — no upload needed; workspace is already on disk.
        Ok(())
    }

    async fn download_artifacts(&self, _sandbox_id: &str, _paths: Vec<String>) -> Result<()> {
        // Local sandbox — artifacts are already on disk.
        Ok(())
    }

    async fn destroy(&self, _sandbox_id: &str) -> Result<()> {
        Ok(())
    }
}
