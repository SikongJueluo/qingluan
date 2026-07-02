# Tauri + CLI + Daemon 架构

## 设计原则

```
Agent ──▶ CLI (qingluan) ──HTTP──▶ Daemon (qingluan-daemon) ──▶ Sandbox Provider
                                         ▲
User  ──▶ Tauri Desktop ──IPC──▶  Tauri Commands (薄封装)
```

1. **CLI 不依赖 UI**：Agent 只通过 CLI JSON 接口交互
2. **Tauri 不承载重业务**：Tauri commands 只做薄封装，转发到本地 daemon
3. **Daemon 集中编排**：管理 task/sandbox/event/artifact 生命周期
4. **Frontend 双向连接**：通过 Tauri IPC 或直接 HTTP 访问 daemon

## CLI (`qingluan`)

- stdout = 机器可读 JSON
- stderr = 日志
- exit code: 0 = 成功，非 0 = 失败
- daemon 未启动时提示用户手动启动

## Daemon (`qingluan-daemon`)

- 本地 HTTP 服务（默认 `127.0.0.1:47129`）
- Axum 路由：`/health`, `/tasks`, `/tasks/:id`, `/tasks/:id/events`, `/sandboxes`

## Tauri Desktop

- Tauri v2 shell，`apps/desktop/src-tauri/`
- 前端 `apps/desktop/frontend/`（Vue/Vite/bun）
- Tauri commands 调用 daemon HTTP API
- 预留 sidecar 打包 `qingluan-daemon` 和 `qingluan-cli`
