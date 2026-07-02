# Sandbox Provider 抽象

## 设计

`qingluan-sandbox` 定义了 `SandboxProvider` trait：

```rust
#[async_trait]
pub trait SandboxProvider: Send + Sync {
    async fn create(&self, req: CreateSandboxRequest) -> Result<SandboxHandle>;
    async fn run_command(&self, sandbox_id: &str, req: RunCommandRequest) -> Result<CommandResult>;
    async fn upload_workspace(&self, sandbox_id: &str, local_path: &str) -> Result<()>;
    async fn download_artifacts(&self, sandbox_id: &str, paths: Vec<String>) -> Result<()>;
    async fn destroy(&self, sandbox_id: &str) -> Result<()>;
}
```

## Provider 实现

### LocalSandboxProvider
- 使用 `tokio::process::Command` 在本机执行
- 开发和降级场景
- workspace 无需上传（已在本地磁盘）

### CubeSandboxProvider
- 基于 RustVMM/KVM 的远程沙箱服务
- 兼容 E2B SDK
- **Phase 1:** 仅保留接口和配置结构，所有方法返回 `cube_provider_not_implemented`
- **后续：** 接入真实 API、文件同步、快照、克隆、artifact 下载
- CubeSandbox 独立部署，不打包进 Tauri
