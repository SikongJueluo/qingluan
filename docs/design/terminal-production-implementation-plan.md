# Agent terminal：生产实现切片计划

状态：草案，待用户批准后开始实现。本计划不冻结生产 `.proto` 字段编号，也不以实现全部 18 个候选 RPC 为目标；RPC 面随切片需要扩张，冻结需要单独批准。

依据：[设计基线](agent-terminal.md)（行为契约与已验证选型）、[协议 v1 草案](terminal-protocol-v1.md)（候选语义，未冻结）、[技术验证结果](../research/terminal-technical-validation.md)（Gate A/B/C 运行证据）、[验证计划](terminal-technical-validation-plan.md)（历史）。

## 1. 模块边界：`qingluan-terminal` 拥有的外部缝

深模块口径：接口窄、实现藏得深；每片验收都包含"没有变宽"。

- **`qingluan-terminal` 是唯一的执行外部缝**。对上层只暴露终端生命周期与观察操作：`start(program, args, cwd, env, size)`、有界 `send`、`resize`、幂等 `stop`、状态快照、带类型化游标的输出读取与订阅、生命周期事件流。签名使用 `qingluan-core::terminal` 的领域类型。
- **缝内私藏**：PTY master fd 与自管 `AsyncFd` 写路径、每终端 cgroup v2 与 pidfd 信号、输入分块／队列／期限、行式规范化、存储批处理与恢复。这些细节不出现在任何公开签名。
- **gRPC 是适配器**：`qingluan-protocol` 的生成类型只活在 `qingluan-daemon` lib 的转换层。protobuf 概念（字段编号、oneof、trailer）不得穿透进 terminal／core；反向，daemon 不接触 PTY／cgroup／SQLite 细节，不维护第二套终端状态机。
- **存储实现私有**：terminal 依赖 `qingluan-storage` 的窄接口（追加、提交水位、恢复、游标查询、事件事务）。SQLite、segment 帧、CRC、迁移全部是 storage 内部。除非出现真实的第二后端，不引入存储 trait 抽象。
- **Pi adapter 与 TS client 是 gRPC 之上的适配器**：TS client 不依赖 pi；pi adapter 只做宿主 session 映射、审批与通知，不实现第二套状态机。
- 依赖方向不变：daemon → terminal → storage → core；terminal 直接使用 core。

**泄漏防线（每片验收的一部分）**：`qingluan-core::terminal` 与 `qingluan-terminal` 的公开 API 不得出现 tonic／prost／sqlx／libc／pty-process 类型；用依赖检查纳入测试。

## 2. 参数口径：生产默认值 vs 探针机制

**生产默认值（用户确认；设计值，探针未在边界实测）**：

- segment 4 MiB 起步轮转；持久化批处理 64 KiB 或 50 ms（先到为准）；每个 terminal 最多保留 64 条 segment 元数据；合并后 gap 记录上限 1024。

**Gate 确认的运行参数（语义已验证，作为生产初始值）**：

- 输入分块 4 KiB；单条 Send ≤ 256 KiB；有界队列 2 条 + 1 条 in-flight（accepted payload backing ≤ 768 KiB）；写入期限 10 s；SIGTERM 宽限 600 ms；`cgroup.kill` 等待 ≤ 3 s；输出关闭等待 ≤ 1 s；cgroup `Delegate=yes` 缺失即硬失败。

**仅探针机制（不带入生产）**：256 KiB 探针轮转阈值、4096 B 场景轮转阈值、300 ms 探针写入期限、client 侧 `MAX_STATUS_DETAILS_BYTES = 4096` 尺寸守卫（探针局部，协议上限另定）、外部 `timeout 120` 包裹。

## 3. 切片

每片独立可合入、有可观察验收；验收不过即停在该片，不带已知缺陷进入下一片。

### S1 领域类型与 crate 骨架

- 产出：`qingluan-core::terminal` 领域类型（HistoryPosition／LogIdentity／游标、事件与三水位、reason 码、快照枚举）；`qingluan-terminal` 空骨架与依赖方向。
- 可观察验收：workspace 构建通过；依赖方向检查通过（core 无 tonic／sqlx／pty 依赖）。
- 任务相关测试：类型不变量单测（`pruned ≤ acked ≤ committed`、游标绑定日志身份）。
- 停止条件：领域类型若无法在不携带存储细节的前提下表达已验证语义，停下重审报告 §6。
- 回滚／清理：纯新增 crate，直接移除。

### S2 存储与恢复

- 产出：`qingluan-storage` 内 segment 帧格式（64 B 段头／40 B 帧头／CRC32、payload ≤ 64 KiB）；提交顺序 append → `sync_data` → 短事务 → 提交后发布；恢复（截断回 committed、隔离、显式 gap + 降级、水位不回退、幂等）；epoch 规则；4 MiB 轮转、64 KiB/50 ms 批处理、每 terminal 64 条 segment 元数据上限（创建第 65 段前须事务性回收最旧 sealed segment 并推进 retained range；无法安全回收时进入 degraded、继续 drain 并以显式 gap 记录丢失，同时拒绝新 Start）、1024 gap 合并上限；版本化迁移（运行时 SQLx 查询）。
- 可观察验收：Gate C crash matrix 与断电变体移植为可重复测试且全绿；连续两次恢复结果一致。
- 任务相关测试：crash point 注入、损坏索引三态、迁移中途失败回滚、frame_seq／line_offset u64 溢出回归、第 65 个 segment 的 retained-range 推进、1024 条 gap 上限处的合并与降级。
- 停止条件：任何恢复路径伪造连续性或复用行号。
- 回滚／清理：迁移版本化且失败必须整体回滚；测试用临时目录，结束即删。

### S3 PTY 生命周期与 cgroup

- 产出：`qingluan-terminal` 内 `pty-process` 0.5.3 + 自管 `AsyncFd` 写路径；每终端 cgroup v2；pidfd SIGTERM 600 ms → `cgroup.kill` ≤ 3 s；输出关闭 ≤ 1 s（超时唯一 `OutputClosed(Forced)`）；Stop detached 幂等；配额状态机与原子名额回收；registry 恢复只标 Interrupted；EIO → 正常 OutputClosed 映射。
- 可观察验收：Gate B 的 25 场景 + 6 单测移植为 crate 集成测试；运行结束无残留进程、cgroup、临时目录。
- 任务相关测试：阻塞写 Stop 中止、100 次代际切换竞态、detached `setsid` 回收、TERM-immune fork、配额竞争、registry 恢复不发信号。
- 停止条件：目标环境无法获得 cgroup delegation（按设计硬失败，不降级到 `/proc` 快照）。
- 回滚／清理：cgroup 按身份清理（`cgroup.kill` + rmdir），不用裸 PID／pgrep。

### S4 规范化与查询

- 产出：`vte` 之上的有限行式规范化（换行、CR 覆写、退格、制表、行内移动与清除；丢弃样式与 OSC）；未完成行 revision；长行 `(line, byte_offset)` 续读；固定读取范围；grep 字面量扫描与扫描预算。
- 可观察验收：中文宽字符、组合字符、超长行、跨数据块的黄金样本全部通过；固定 end 不随后续 append 扩展（Gate C 探针 `ReadCursor` 无 `end_line`，此项在本片首验）。
- 任务相关测试：黄金样本比对；屏幕折行不产生新历史行的属性测试。
- 停止条件：`vte` 无法在有界内存内表达所需行为（此域未经探针验证，是本计划最大未验证面）。
- 回滚／清理：纯函数层，可整体替换。

### S5 持久化事件与确认

- 产出：session 事件序列（与退出状态同事务分配、提交后才发布）；ack 单调与越界拒绝；Prune 只删已确认连续前缀且与 `pruned_through_seq` 同事务；`after_event_seq` 订阅与清理后显式报错。本片在存储接口层验证这些语义；WatchSessionEvents／AckSessionEvents 的 daemon RPC 适配在 S8 消费时接入。
- 可观察验收：提交先于发布的顺序断言；ack／prune 事务性测试；崩溃后公开序号不复用。
- 任务相关测试：事件插入／提交／发布三点的崩溃注入。
- 停止条件：任何未提交序号外泄到持久化流。
- 回滚／清理：事件不自动过期，仅显式清理。

### S6 daemon gRPC 接入与服务包装

- 产出：生产 `.proto` 首批最小子集（GetServerInfo、控制租约三件套、Start/Send/Stop、Read/Tail 级别），字段编号此时赋值但不宣布冻结；后续 RPC 面只在消费它的切片扩张（S8：WatchSessionEvents／AckSessionEvents；S9 验收前：ObserveTerminal 与显式清理三件套，见对应切片），仍不冻结全量 18 个候选 RPC；daemon 组装 terminal + storage；UDS `0600`、stale 回收、活动 socket 拒绝；richer error 编解码；systemd user unit 模板含 `Delegate=yes` 并在启动时探测 delegation／`cgroup.kill`、缺失即快速失败；Nix／devenv 集成。
- 可观察验收：Gate A 场景移植为 Rust↔TS 互操作测试（socket 安全、bigint、presence、未知字段／枚举、取消、deadline、rich error 降级）；无 delegation 的真实 unit 下 daemon 快速失败。
- 任务相关测试：互操作测试 + unit 文件静态断言（`Delegate=yes` 存在）。
- 停止条件：tonic/grpc-js 出现探针未覆盖的语义分歧。
- 回滚／清理：gRPC 为新增入口，既有 HTTP 不动；unit 可整体移除。
- S6 实现记录：生产面严格停在上述九个 unary RPC；`qingluan-daemon` 同时服务既有 HTTP 与 mode `0600` UDS，租约只驻内存并以 runtime generation 线性化 Start/Send/Stop；Rust UDS 集成、控制权竞态、socket 安全、rich error 与 systemd 静态测试已落地，`just terminal-grpc-interop` 使用固定 grpc-js/ts-proto 版本覆盖 bigint、presence、bytes、未知字段／枚举、deadline、取消与 richer error。

### S7 TS client

- 产出：`packages/qingluan-client`：生成代码接入 build／typecheck 前置；bigint → 十进制字符串；租约续租与重连；显式游标；rich error 显式解码与降级；结果未知语义。
- 可观察验收：对 S6 daemon 的端到端测试（断线重连、租约过期、部分写入报告、控制权竞争）。
- 任务相关测试：mock transport 解码单测 + 真 daemon 集成测试。
- 停止条件：Node 最低版本（候选 ≥ 22.12.0，实测 22.23.2／24.19.0）与目标环境冲突。
- 回滚／清理：独立包，可单独摘除。

### S8 Pi adapter

- 产出：`packages/qingluan-pi` 接入：宿主 session 映射、完整环境快照传递、审批内容与最终字节一致、退出事件按 event_seq 去重与确认、断线重连汇总积压；daemon RPC 面扩张：WatchSessionEvents／AckSessionEvents（把 S5 的事件重放与累计确认贯通到协议层，本片经 TS client 消费，不新增语义）。
- 可观察验收：审批—发送一致性测试；退出事件重放去重测试。
- 任务相关测试：对 TS client 的集成测试。
- 停止条件：宿主审批语义无法保证审批与最终字节一致。
- 回滚／清理：adapter 层可禁用而不影响 client。

### S9 最终加固

- 产出：磁盘满／只读演练（降级、显式 gap、拒绝新 Start、不杀任务）；服务重启／升级演练（旧活动记录只标 Interrupted、epoch 保持）；日志预算轮转；只读观察 CLI；文档收口；daemon RPC 面扩张（在本片逐项验收前完成）：ObserveTerminal（供只读观察 CLI）与显式清理三件套 ClearTerminalLogs／DeleteTerminalRecord／PruneSessionEvents（供基线验收第 1 项“停止与显式清理”）。
- 可观察验收：设计基线"验收顺序与停止条件"1–5 逐项跑通并留记录。
- 任务相关测试：故障注入演练脚本。
- 停止条件：任一演练伪造成功或残留资源。
- 回滚／清理：演练脚本可丢弃，不进生产路径。

## 4. 非目标

- 不冻结字段编号与全部枚举；不以本计划为授权实现全部 18 个候选 RPC——RPC 面随切片需要扩张，冻结需要单独批准。
- 不做 Web UI、远端调度、SSH 凭证管理、通用权限框架、任意全屏 TUI 的可靠观察。
- 不把探针代码升级为生产代码；`probes/` 保持 throwaway，生产 crate 不依赖它。
- 不引入存储 trait 抽象、ORM、Buf 或第二数据库。
- 不承诺回收主动迁出 cgroup 的进程或 SSH 远端任务。

## 5. 全局停止条件

- 任一切片发现已验证语义在生产实现中不成立（Stop 无法有界中止、恢复伪造连续性、代际切换漏写）：停下，带证据回到设计讨论，不静默换实现。
- cgroup delegation 在目标部署不可得：停下重议清理策略，不静默降级。
- 生产默认值（4 MiB／64 KiB／50 ms／每 terminal 64 条 segment 元数据／1024 gap）在真实负载下失效：按报告 §10.2 补边界实验后再调整，不在切片内顺手改值。
