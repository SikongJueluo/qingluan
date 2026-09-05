# Tauri + CLI + Daemon 架构

## 设计原则

```
Agent ──▶ CLI (qingluan) ──HTTP──▶ Daemon (qingluan-daemon) ──▶ Sandbox Provider
                                         ▲
User  ──▶ Tauri Desktop ──IPC──▶  Tauri Commands (薄封装)
```

1. **CLI 不依赖 UI**：Agent 只通过 CLI JSON 接口交互（例外：`workspace open`，见下）
2. **Tauri 不承载重业务**：Tauri commands 只做薄封装，转发到本地 daemon
3. **Daemon 集中编排**：管理 task/sandbox/event/artifact 生命周期
4. **Frontend 双向连接**：通过 Tauri IPC 或直接 HTTP 访问 daemon
5. **Workspace 切换是纯本地能力**：`workspace` 子命令直接读 JJ/Pi 本地状态，不经 daemon、无缓存/租约

## CLI (`qingluan`)

命令树每一支都必须有真实可用的实现。当前命令面：

- `health` — daemon 健康检查，始终输出机器可读 JSON（调试命令，无
  `--json` 开关）
- `workspace list/open` — 纯本地，不经 daemon（见下）

daemon 的 task/sandbox 执行引擎尚未实现，CLI 不暴露 `task`/`ui`
占位命令；task 系命令待执行引擎落地后随 daemon 端点一起引入。

- 非交互命令的 stdout = 机器可读 JSON（稳定契约）
- 唯一例外：`workspace open` 占用终端做交互选择（dialoguer），此时 stdout
  不是机器 JSON；这是 CLI 中唯一的交互式命令
- stderr = 日志（及 `workspace open` 的 × 原因提示）
- exit code: 0 = 成功，非 0 = 失败；daemon 未启动时提示用户手动启动

## 本地 Workspace 命令（daemon 例外）

`qingluan workspace` 子命令不经 daemon，直接在本地执行（qingluan-core
`workspace` 模块，深模块：JJ workspace 枚举 + Pi session JSONL 扫描 +
路径关联）。

### `workspace add <name> [--revision REV] [--at <path>] [--json]`

封装 `jj workspace add`，消除长路径记忆：

- 默认落点 `<[workspace] root>/<repo 目录名>/<name>`（config 层级见
  「配置」节）；`--at` 显式覆盖；`--revision` 透传
- 预检：名字已注册 → `workspace_exists`；目标目录非空 →
  `destination_not_empty`；父目录自动创建，空目标目录放行
- human 输出一行结果 + cd 提示；`--json` 输出 name/root/revision；
  不自动启动 pi（那是 `workspace open` 的职责）

### `workspace remove <name> [--purge] [--force] [--json]`

封装 `jj workspace forget`（目录不动），`--purge` 才删目录：

- 预检：未知名 → `workspace_not_found`（jj 对未知名静默 exit 0，必须预检）
- 脏检测（`jj -R <root> log -r @ -T empty`，需 snapshot，无
  `--ignore-working-copy`）：有未提交变更时 `--purge` 拒绝，`--force` 放行
- `--purge` 交互确认；`--json` 模式无交互 → 必须 `--force`
- forget 前提示关联 Pi 会话数（文件永不触碰 `~/.pi`）；当前 cwd 在
  被删目录内时警告 cd away；先 forget 后删目录，删失败如实报
  `purge_failed`

### `workspace list [--json]`

- 默认 human 视图：每个 workspace 一个块（bold 名称 + `~` 缩写 root(dim)
  + 会话数 / 红色 `×` 不可用原因），块内会话行对齐三列（标题、
  msgs、相对时间 `just now`/`5m ago`/`2d ago`，超过一周显式日期）。
  对齐按显示宽度计算（CJK 安全，console crate，dialoguer 同源零新增
  传递依赖）；非 TTY（管道）时自动去色，stdout 仍可安全行处理
- 在当前目录 shell out `jj workspace list`（`--ignore-working-copy`，
  只读，不 snapshot），模板输出 name/root 流
- 扫描 `~/.pi/agent/sessions`：与 Pi `listAll` 兼容但保持本地 —— 仅一层
  目录不递归，但跟随顶层符号链接目录，接受 `.jsonl` 符号链接/文件；
  按 Pi 0.84.2 显示语义派生 title/messageCount/modified
- 会话头校验：首条有效 JSON（跳过空行/坏行及 `null`/`false`/`0`/`""`
  等 falsey 解析值）必须是 `session` 头且 `id` 为字符串（与 Pi 的头部
  验证一致）；额外要求 `cwd` 为非空绝对路径，否则整个文件判为非会话
  （安全关联：缺失/空/相对 cwd 绝不会关联到进程当前目录）
- session 按规范化 cwd 与 workspace 精确关联，关联不上的不出现
  （绝不错归属）；workspace 目录缺失时仍保留，`available: false` +
  `unavailableReason`
- `--json`：stdout 输出 `schemaVersion: 1` 的 camelCase 语义 JSON；
  失败时 stdout 保持干净，stderr 输出 `{"ok":false,"error":<code>,"message":...}`
  并非零退出（如 `not_in_jj_repository`）

### `workspace open`（交互式终端例外）

这是 CLI 中唯一的交互式命令，直接占用终端：

- dialoguer `FuzzySelect` 扁平选择器：每个可用 workspace 列出其会话
  （`name ── title (N msgs, modified)`，标题截断单行，标签全局去重）
  与一条 `✚ new session`
- 不可用 workspace 的历史 session 各占一行 `name ── × title […]`，便于
  识别和未来迁移；若没有历史 session，workspace 本身占一行。缺失的 root
  无法承载启动，因此这些行仅提供信息
- 选中 `×` 行：仅在 stderr 打印原因并重新打开选择器（不退出、不报错）；
  Esc 退出码 0（取消不是错误）
- 选中后以目标 workspace root 为 `current_dir` 启动子进程 `pi`：
  resume 传 `--session <file>`，new 不带参数；子进程退出码即退出码

### Pi 扩展 `packages/qingluan-pi`（`/ws`）

- 在 `ctx.cwd` 执行 `pi.exec("qingluan", ["workspace", "list", "--json"])`，
  校验 `schemaVersion === 1`，用纯函数 helper `src/catalog.ts` 构建唯一
  扁平标签（选择器选项构建可脱离 Pi 运行时单测：
  `node --test packages/qingluan-pi/src/catalog.test.ts`）
- 选择器循环：不可用 workspace 的历史 session 各保留一行 `×` 项（无
  session 时显示 workspace 行），选中只 notify 原因并重开选择器；Esc 取消
- 选已有会话 → `ctx.switchSession(file)`（会话头部的 cwd 会把 agent
  切到目标 workspace）
- 选其他 workspace 的 `✚ new session` → `SessionManager.forkFrom(当前
  session 文件, 目标 root)` 后 `switchSession` 到 fork（携带当前上下文）；
  当前 workspace 则 `ctx.newSession()`（无 workspace 切换，无需 fork）
- 守卫：无 UI（`ctx.hasUI`）、当前 session 未持久化（无法 fork）

## Daemon (`qingluan-daemon`)

- 本地 HTTP 服务，监听地址来自 [`qingluan-config`](#配置-qingluan-config)
  （默认 `127.0.0.1:47129`；配置损坏时启动即硬失败）
- Axum 路由：`/health`, `/tasks`, `/tasks/:id`, `/tasks/:id/events`, `/sandboxes`

## 配置 (`qingluan-config`)

- 四层合并，后者覆盖前者：内置默认 < 全局
  `~/.config/qingluan/config.toml` < 项目当前目录 `qingluan.toml` <
  env `QINGLUAN_*`（嵌套键双下划线，如 `QINGLUAN_DAEMON__PORT`）
- figment + dirs 实现；serde 派生配置结构，`deny_unknown_fields`
  开启——未知键/段/类型错误全部硬失败（静默降级 = 配置漂移温床）
- v1 键集：`[workspace] root`（workspace add 默认落点，`~` 展开）与
  `[daemon] host/port`（daemon 绑定 + CLI 默认 --daemon-url，已消费）；
  `[sandbox]`/`[cube]` 仅定 schema 待执行引擎消费
- 示例见 `config/qingluan.example.toml`

## Tauri Desktop

- Tauri v2 shell，`apps/desktop/src-tauri/`
- 前端 `apps/desktop/frontend/`（Vue/Vite/bun）
- Tauri commands 调用 daemon HTTP API
- 预留 sidecar 打包 `qingluan-daemon` 和 `qingluan-cli`
