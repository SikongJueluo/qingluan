# Qingluan 项目目录结构

## 顶层

| 目录/文件 | 说明 |
|-----------|------|
| `Cargo.toml` | Rust workspace 定义，统一管理所有 crate 依赖版本 |
| `crates/` | 所有 Rust 库和二进制 crate |
| `apps/desktop/` | Tauri 桌面应用（前端 + Rust 壳） |
| `docs/architecture/` | 架构文档 |
| `config/` | 用户级配置模板 |
| `qingluan.example.toml` | 项目级配置模板 |

## `crates/` 仓库

| Crate | 类型 | 职责 |
|-------|------|------|
| `qingluan-protocol` | lib | 跨 CLI/Daemon/Tauri/前端共享的 DTO 定义 |
| `qingluan-core` | lib | 纯业务核心，不依赖 Tauri/Axum/CLI/sandbox |
| `qingluan-sandbox` | lib | 沙箱执行环境抽象（SandboxProvider trait + Local/Cube 实现） |
| `qingluan-daemon` | bin | 本地控制平面：任务编排、沙箱管理、事件流 |
| `qingluan-cli` | bin | Agent 稳定调用入口（clap + JSON stdout） |
| `qingluan-storage` | lib | 本地 SQLite 持久化（Phase 1: 占位） |

## `apps/desktop/` 结构

| 路径 | 说明 |
|------|------|
| `frontend/` | Vue 3 + Vite + shadcn-vue 前端 |
| `src-tauri/` | Tauri v2 Rust 壳（薄封装，调用 daemon API） |
| `src-tauri/binaries/` | Tauri sidecar 外部二进制预留目录 |
