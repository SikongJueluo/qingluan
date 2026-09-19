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
| `qingluan-core` | lib | 业务核心，已有 workspace/Pi session 管理；不依赖 Tauri/Axum/CLI/sandbox |
| `qingluan-config` | lib | 分层配置加载、校验与默认值 |
| `qingluan-sandbox` | lib | 沙箱执行环境抽象（SandboxProvider trait + Local/Cube 实现） |
| `qingluan-daemon` | bin | 本地控制平面：任务编排、沙箱管理、事件流 |
| `qingluan-cli` | bin | 面向人类的操作入口（clap），保留机器可读模式；不是 Agent 的唯一入口 |
| `qingluan-storage` | lib | 本地 SQLite 持久化（Phase 1: 占位） |

## Terminal 目标结构（已确认，尚未实现）

详见 [Agent terminal 设计基线](../design/agent-terminal.md) 和 [协议 v1 草案](../design/terminal-protocol-v1.md)。下列是规划，不代表相应文件或实现已经存在。

| 路径 | 目标职责 |
| --- | --- |
| `proto/` | 唯一的 Protobuf 定义来源，Rust/TS 共用 |
| `crates/qingluan-terminal/`（新增） | PTY、规范化、控制租约、配额、状态机、输入调度及订阅 |
| `crates/qingluan-core/src/terminal/`（新增模块） | terminal/storage 共用领域类型与纯规则，不依赖 gRPC/SQLx/PTY |
| `crates/qingluan-storage/` | SQLx、迁移、分段日志、索引、事件事务及恢复 |
| `crates/qingluan-protocol/` | 生成的 Protobuf/RPC 类型；暂时保留既有 HTTP DTO |
| `crates/qingluan-daemon/` | 组装、服务启动、RPC 接入与领域类型转换 |
| `crates/qingluan-config/` | terminal socket、容量、租期等配置加载与校验 |
| `crates/qingluan-cli/` | 人类管理和只读观察入口 |
| `packages/qingluan-client/`（新增） | TS 生成代码、异步调用、租约、重连及错误解码 |
| `packages/qingluan-pi/`（已有） | 保留 /ws，新增 terminal 的宿主审批、工具及通知接入 |

依赖方向：

```text
daemon → terminal → storage → core
   │         └──────────────→ core
   ├──→ protocol
   └──→ config

qingluan-pi → qingluan-client → 生成的 TS 协议代码
```

- storage 不反向依赖 terminal；领域类型不直接使用生成的 Protobuf 类型。
- terminal 是终端状态机的唯一负责模块；daemon 不维护第二套状态，storage 负责持久化事务而非进程调度。
- 保留 CLI/daemon 两个二进制；旧 HTTP 调用首版暂时保留。
- 不改造 `qingluan-sandbox`，不提前创建通用远端执行框架。

## `apps/desktop/` 结构

| 路径 | 说明 |
|------|------|
| `frontend/` | Vue 3 + Vite + shadcn-vue 前端 |
| `src-tauri/` | Tauri v2 Rust 壳（薄封装，调用 daemon API） |
| `src-tauri/binaries/` | Tauri sidecar 外部二进制预留目录 |
