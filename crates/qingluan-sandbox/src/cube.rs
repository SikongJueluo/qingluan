use anyhow::{bail, Result};
use async_trait::async_trait;
use serde::Deserialize;

use crate::{CommandResult, CreateSandboxRequest, RunCommandRequest, SandboxHandle, SandboxProvider};

/// Configuration for the CubeSandbox provider.
#[derive(Debug, Clone, Deserialize)]
pub struct CubeConfig {
    pub endpoint: String,
    pub api_key: String,
    pub template: String,
}

/// A sandbox provider that delegates to a CubeSandbox service (RustVMM/KVM-based).
/// Phase 1: reserved interface only. All methods return `not_implemented`.
pub struct CubeSandboxProvider {
    #[allow(dead_code)]
    config: CubeConfig,
}

impl CubeSandboxProvider {
    pub fn new(config: CubeConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl SandboxProvider for CubeSandboxProvider {
    async fn create(&self, _req: CreateSandboxRequest) -> Result<SandboxHandle> {
        bail!("cube_provider_not_implemented: CubeSandbox integration is pending")
    }

    async fn run_command(&self, _sandbox_id: &str, _req: RunCommandRequest) -> Result<CommandResult> {
        bail!("cube_provider_not_implemented: CubeSandbox integration is pending")
    }

    async fn upload_workspace(&self, _sandbox_id: &str, _local_path: &str) -> Result<()> {
        bail!("cube_provider_not_implemented: CubeSandbox integration is pending")
    }

    async fn download_artifacts(&self, _sandbox_id: &str, _paths: Vec<String>) -> Result<()> {
        bail!("cube_provider_not_implemented: CubeSandbox integration is pending")
    }

    async fn destroy(&self, _sandbox_id: &str) -> Result<()> {
        bail!("cube_provider_not_implemented: CubeSandbox integration is pending")
    }
}
