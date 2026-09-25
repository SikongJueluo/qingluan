# Terminal 协议 v1 草案

状态：行为契约已逐轮确认，执行语义已按 Gate A/B/C 运行证据对齐（[技术验证结果](../research/terminal-technical-validation.md)）。S6 已实现 `proto/qingluan/terminal/v1/` 下首批九个 unary RPC，并为该最小子集赋号；完整 v1、后续 RPC 与兼容承诺仍未冻结。S8 才加入事件 RPC，S9 才加入观察与清理 RPC。

范围与选型依据：[Agent terminal 设计基线](agent-terminal.md)。两份文档冲突时需回到已确认决策核对，不能用本草案中的候选结构静默覆盖基线。

## 1. 定位与兼容

- 一个 `TerminalService`，不抽象通用 Session 服务。
- 使用 proto3 + gRPC over Unix socket；S6 package 为 `qingluan.terminal.v1`。
- Rust：tonic + prost；Node.js TS：grpc-js + ts-proto；两端均构建时静态生成，生成产物不提交仓库。构建后使用生成类型，不要求为 LSP 支持提交生成代码。
- Rust 生成已接入 `qingluan-protocol/build.rs`；S6 的 TS 生成、typecheck 与跨语言验证由 `just terminal-grpc-interop` 执行。生产 TS client 的生成前置仍属于 S7。
- 使用 protoc 与现有 just 组织生成，首版不引入 Buf；生成工具已通过 Nix/devenv 声明。
- `google.rpc.Status` 与 `google.protobuf.Any` 定义已固定版本随仓库保存，来源与许可记录在 `third_party/README.md`；构建不临时下载协议定义。
- 原有 HTTP 调用暂时保留，terminal 首版不捆绑 CLI/Tauri 整体迁移。
- GetServerInfo 区分 daemon 软件版本与支持的协议主版本；主版本不兼容则拒绝执行。
- 同一 v1 优先增量兼容；需要区分缺省与零值的字段使用 optional，删除字段后保留编号，不能重新赋予其他含义。
- 可选能力必须表示实际实现，不以占位接口声明支持。能力标识及未知能力处理的最终表示待定。
- gRPC 连接不代表 session 或控制权；连接重建不保证调用没有执行。

## 2. 通用消息形状

下列是语义形状，不是已冻结的字段编号或 Protobuf 语法。`?` 表示可缺省，`A | B` 表示互斥变体候选。字段名称可在生成接口定稿时调整，但不得改变语义。

```text
SessionRef {
  source: string
  external_id: string
}

ControlContext {
  session: SessionRef
  control_token: string
}

TerminalRef {
  session: SessionRef
  terminal_id: string
}

EnvironmentSnapshot {
  variables: map<string, string>
}

TerminalSize { rows, columns }
QueryLimits { max_lines?, max_bytes? }
```

- SessionRef 在当前 OS 用户下唯一；cwd、workspace、连接身份不参与标识。
- EnvironmentSnapshot 外层必须存在，内部空 map 合法；不能因为 map 为空而继承 daemon 环境。
- 令牌仅适用于对应 session；每次修改还需检查 terminal 的归属。
- terminal_id 不复用。令牌不得进入日志、工具结果或模型上下文。
- 行号、字节偏移、事件序号使用 uint64；TS 使用 bigint，工具 JSON 使用十进制字符串。
- 规范化文本使用 string，Send 的最终输入使用 bytes。原始日志暂不增加导出 RPC。
- 默认尺寸 120×30；查询默认 200 行 / 32 KiB，硬上限 1000 行 / 256 KiB，同时生效。
- 字节预算具体涵盖文本还是完整编码响应、附加字段的硬上限，须在 `.proto` 定稿前明确，不能出现“文本有界、元数据无限”的实现。

## 3. RPC 全表

普通方法为 unary；仅 ObserveTerminal 与 WatchSessionEvents 为 server streaming。名称为候选，语义来自基线。`C` 表示携带 ControlContext，`T` 表示 TerminalRef。

| RPC | 请求核心字段 | 成功响应／成功时机 | 控制权 |
| --- | --- | --- | --- |
| GetServerInfo | 无 | daemon 版本、协议主版本、实际可选能力 | 无 |
| AcquireControl | SessionRef | 控制令牌、有效期、SessionEventState；首次创建 session 记录 | 获取 |
| RenewControl | C | 成功续租及有效期 | 必须有效 |
| ReleaseControl | C | 释放当前控制权，不终止终端 | 校验当前令牌 |
| Start | C、program、args、绝对 cwd、EnvironmentSnapshot、尺寸 | PTY 创建且根进程启动成功，返回 terminal_id；并非仅排队 | 必须有效 |
| Send | C、terminal_id、data: bytes | 全量写入 PTY master，返回写入字节数 | 必须有效 |
| Resize | C、terminal_id、尺寸 | 目标尺寸调整成功；不由观察窗口自动触发 | 必须有效 |
| Stop | C、terminal_id | 已进入停止流程及当前状态；不等待完整退出 | 必须有效 |
| List | SessionRef、候选筛选／分页参数 | 有界终端快照列表；分页字段待定 | 无 |
| Read | T、初始位置或续读游标、QueryLimits | 固定历史范围的一页内容及后续位置 | 无 |
| Tail | T、QueryLimits | 近期历史、未完成行快照、观察衔接位置 | 无 |
| Grep | T、搜索词／选项、初始范围或扫描续点、QueryLimits | 匹配、有限上下文、扫描进度及续点 | 无 |
| ObserveTerminal | T、观察位置、候选批次限制 | 先追赶再跟随的输出／状态消息流 | 无 |
| WatchSessionEvents | SessionRef、after_event_seq | 指定序号之后的持久化生命周期事件流 | 无 |
| AckSessionEvents | C、up_to_seq | session 级累计消费确认位置 | 必须有效 |
| ClearTerminalLogs | C、terminal_id、待定历史选择范围 | 实际清理范围与类型化最早可用位置；不停止进程 | 必须有效 |
| DeleteTerminalRecord | C、terminal_id | 仅在资源回收、事件提交完结且无未确认事件时删除记录 | 必须有效 |
| PruneSessionEvents | C、through_seq | 清理至指定已确认序号的连续前缀；保留高水位及确认进度 | 必须有效 |

最后三个清理 RPC 的名称和范围参数尚未冻结，但日志清理、记录删除、已确认事件清理不得被一个含糊的强制 Delete 混合。

首版没有 CreateSession、DeleteSession、抢占控制权、恢复旧进程或强制删除并杀进程 RPC。也不要求每个 RPC 都对应 agent 工具或 CLI 命令。

## 4. 控制权转换

租期初始为 30 秒，client 每 10 秒续租，server 用单调时钟判定。

| 当前情况 | 操作／事件 | 结果 |
| --- | --- | --- |
| session 不存在 | AcquireControl | 创建记录并授予控制权 |
| 无有效租约 | AcquireControl | 授予控制权 |
| 已有有效控制端 | 第二个控制端 AcquireControl | 拒绝，不抢占 |
| 有效租约 | RenewControl | 延长有效期 |
| 正常退出、reload、切换 session | ReleaseControl | 释放控制权；终端继续 |
| 意外断线 | 未成功续租 | 不立即释放，至租期结束；client 停止修改 |
| 断线后重连 | 先确认／续租 | 成功后恢复修改；失败则重新申请 |
| 租约过期或已释放 | 使用旧令牌修改 | 拒绝 |
| daemon 重启 | 任何旧令牌 | 失效，重新获取 |

不存在的 session 的只读查询报不存在，不隐式创建。

每次新授权使用不同的租约代际，令牌绑定该代际；代际可为 server 内部状态，不要求 client 解码。到期、释放、重新授予及副作用提交须在同一个 session 控制权协调机制下线性化。

- Start：校验与进程启动授权作为一个有序提交步骤；失效代际尚未提交的启动不得执行。已获启动授权的操作可完成，返回丢失不意味着撤销；后续操作仍要求有效新控制权。
- Resize：在实际修改尺寸的提交点检查代际，不能仅在入队时检查。
- Send：只在有效代际下提交有界的非阻塞写入片段；等待可写状态不能占住控制权协调机制。到期／释放与每片实际写入有明确顺序，失效后不得继续提交剩余片段；已写入部分保留并报告。
- Stop：停止意图成功提交后由 daemon 负责完成，不因发起者的租约失效、连接断开或 RPC 取消而撤销。旧代际尚未提交的 Stop 则拒绝。
- Ack 和显式清理同样需要有效代际的有序提交，不能在检查后被新控制端接管时继续提交旧操作。

AcquireControl 的响应包含已保存的事件确认位置及可用范围，使新 client 无需猜测从哪里恢复。S6 中获取响应丢失后，同一 session 在剩余租期内继续返回 `CONTROL_BUSY`，不自动抢占；重复 ReleaseControl／旧令牌 ReleaseControl 返回 `CONTROL_EXPIRED`，不得据此盲目重试有副作用操作。

## 5. 执行与顺序

### Start

成功只证明 PTY 创建和根进程启动；程序可能在响应抵达前退出。cwd 或程序错误必须明确失败，不替换路径或隐式套 shell。

环境来自 agent 的完整快照，不混入服务环境、不持久化环境变量值。TS client 不能将缺省环境误编码成显式空环境。

Start 在分配 PTY／启动进程前，原子预留 session 与全局两个名额；任一级不足都不能留下另一侧的孤立预留。预留内部化，不要求 List 暴露尚未创建成功的终端。

失败启动是否保留单独审计记录仍待确定；失败后须完成已分配资源回收再释放预留。Start 响应丢失仍可能留下已启动终端，不承诺靠自动重试消除不确定性。

### Send

TS client 将文本、提交和控制键确定性转换为最终字节，RPC 只接收 bytes。adapter 要保证审批与最终字节一致。

- 完整成功：请求字节已全部写入 PTY master，不代表程序读取或命令成功。
- 部分写入：有错误详情时报告已知写入量；不能自动重发整段或假定剩余字节可安全补发。
- 连接断开：未收到详情则结果未知，不能当成写入零字节。
- 控制字符受终端模式影响，不等价于直接发送 OS 信号。

### 修改顺序

普通 Send/Resize 按终端有序执行，同一终端的两个 Send 不交错写入；调用方要求普通操作顺序时等待前一次响应，并发调用不按客户端发起时间排序。Stop 是明确的中止屏障，不是排在所有输入之后的普通 FIFO 项。

Stop 经有效代际提交后，立即拒绝新 Send，取消尚未开始的输入，并通知进行中的 Send 停止剩余片段。Stop 不等待整个 Send 写完或客户端 deadline 到期，必须能在有界时间内进入停止流程。已知写入量通过结构化错误报告；响应已无法送达时 client 仍只能判定结果未知。

服务端使用有界输入消息、有界分块和独立的写入期限；等待可写时同时响应 Stop、租约失效和服务关闭。单个非阻塞写入步骤与控制权切换须有确定次序，不允许跨代际补写。查询／观察不进入输入队列。已提交的停止流程继续完成，已写入的字节不回滚。

有界参数已验证并确认为初始值（报告 §5）：分块 4 KiB；单条 Send ≤ 256 KiB，越界类型化拒绝；有界队列 2 条 + 1 条 in-flight，accepted payload backing ≤ 768 KiB；写入期限 10 s。Stop 调度上界：SIGTERM 宽限 600 ms，`cgroup.kill` 后最长等 3 s；输出关闭最长等 1 s，超时提交唯一 `OutputClosed(Forced)`。Stop 内部 detached 且幂等：提交后首个调用者取消不撤销，重复调用共享同一结果；阻塞写入的中止实测为毫秒级并精确报告已写入字节。清理按每终端独立 cgroup v2 执行，依赖部署前置 `Delegate=yes`（缺失即硬失败）；普通 fork、新进程组与 `setsid` 后代仍在 cgroup 内即被覆盖，主动迁出 cgroup 的进程与远端进程不保证（有意非保证）。

## 6. 终端状态与转换

候选快照形状：

```text
TerminalSnapshot {
  terminal: TerminalRef
  process: Running | Exited(ExitResult) | Interrupted
  output: Open | Closed(OutputEnd)
  stopping: bool
  size: TerminalSize
  retained_history: HistoryRange
}

ExitResult = ExitCode(code) | Signal(signal)
OutputEnd = Eof | ForcedClose(reason) | ReadError(reason) | Interrupted
```

枚举名称、启动中／启动失败的表示尚未冻结。只在确实知道时填退出码或终止信号，不用 -1 表示全部未知情况。

| 触发 | 进程状态 | 输出状态 | 说明 |
| --- | --- | --- | --- |
| 启动成功 | Running，或已观察到 Exited | 通常 Open | 不承诺响应抵达时仍运行 |
| 根进程退出 | Exited | 保留当前输出状态 | 记录 ProcessExited；不立刻丢弃缓冲输出 |
| PTY 正常 EOF | 不因此推断进程退出 | Closed(Eof) | 固定最后未换行尾部；记录 OutputClosed |
| Stop 开始 | 保留已知状态，stopping=true | 保留至关闭 | 尽力清理终端 cgroup，先温和后强制 |
| 根已退出但输出仍打开时 Stop | 保留 Exited | 最终 ForcedClose | 清理残留 PTY；可能不完整，不重复完成通知 |
| 读取故障 | 不伪造进程退出 | Closed(ReadError) | 报不完整，不假装正常 EOF |
| daemon 重启后旧活动记录 | 无可信退出结果则 Interrupted | 未正常结束则 Interrupted | 不恢复进程、不凭旧 PID 贸然发信号 |
| 进程、输出和管理资源均结束后 Stop | 保留终态 | 保留终态 | 返回已有状态 |

配额使用内部状态机 `Reserved → Active → Cleaning → Released`，启动失败则 `Reserved → Cleaning → Released`。除 Released 外均占 session 与全局名额（默认 8 / 32，可配置），每个名额只释放一次。

根进程运行或输出打开时必然占位；即使快照已为 Exited + Closed，仍要等进程回收、PTY/读写任务及停止流程等管理资源结束后才能释放。仅收到退出事件不能释放配额。预留和清理状态可以完全内部化，不必引入额外的公开终端枚举。

ProcessExited 和 OutputClosed 无固定先后保证，不能只用一个 RUNNING/EXITED 枚举推断所有资源状态。仅根退出触发正常完成通知，OutputClosed 不额外唤醒一次任务完成通知。Linux 上最后一个 slave 关闭后 master read 返回 EIO（errno 5）而非 EOF，实现按正常输出结束处理；root exit 与 output close 独立提交、各一次（已验证）。

## 7. 查询消息

```text
LogIdentity { terminal: TerminalRef, log_epoch: string }
HistoryPosition { line: uint64, byte_offset: uint64 }
TailPosition { tail_id: string, revision: uint64, byte_offset: uint64 }

ReadCursor {
  log: LogIdentity
  next: HistoryPosition
  end_line: uint64
}

ObservationCursor {
  log: LogIdentity
  next_history: HistoryPosition
  tail_seen: TailPosition?
  state_revision: uint64
}

LineFragment {
  position: HistoryPosition
  text: string
  prefix_omitted: bool
  suffix_remaining: bool
}

TailSnapshot {
  log: LogIdentity
  position: TailPosition
  text: string
  truncated: bool
}
```

这些类型共享日志身份与位置类型，但 Read 的固定查询上界不能被误用为实时 Observe 的永久上界。Grep 续点同样绑定 LogIdentity、固定范围和查询参数。

log_epoch 是持久化日志代际，普通重启与轮转不改变；破坏性重建导致旧位置不再可解释时才换代，并拒绝旧游标。server 验证游标中的 session、terminal_id 与日志代际，不接受跨终端使用。恢复语义已验证（报告 §6）：未提交尾部截断回 committed 边界并隔离、永不采纳；索引指向缺失／截短／损坏段时返回显式 gap 与降级状态；行号水位不回退、编号不复用；同一损坏样本连续两次恢复结果一致；运行中清理日志不杀进程、不重置行号。尾部 revision 固化为历史行后跨轮转仍可精确续读，持有段被清理时同事务失效并按 CURSOR_EXPIRED 拒绝。

最早可用位置必须带行内偏移：超长行前缀已被清理时不能错误报告该行从 byte_offset=0 可读。可变尾部用独立 TailPosition，覆写后旧 revision 失效。

Tail 返回可直接传给 ObserveTerminal 的 ObservationCursor，历史切面、尾部版本和衔接游标必须来自一致状态。尾部固定为历史行时，保留 tail_id／最终 revision 到稳定行号的转换信息，直至相关历史清理；匹配的旧位置可以准确衔接。若中间发生覆写且旧版本不可恢复，则显式报位置失效，不悄悄拼接新尾部。

状态更新使用独立 state_revision，不因没有新日志行而遗漏 ProcessExited／OutputClosed 状态。CURSOR_EXPIRED 的恢复详情区分 Read、Observe、Grep 的类型化位置，并提供缺失范围；恢复需要调用方显式选择，不自动扩展原 Read 的固定范围。

### Read

响应候选：`fragments + next_cursor? + truncation_reason + retained_range`。

首次读取必须显式选择 earliest、newest 或具体历史位置；缺失 position 是无效请求，不能隐式回退到 earliest。首次读取确定 end_line，后续分页不随新输出扩展；固定范围不阻止轮转。Gate C 实测覆盖的是活位置续读：钉在当前末尾的 `(line, byte_offset)` 游标跨后续 append 与轮转仍精确续读（探针 `ReadCursor` 无 `end_line`，报告 §6）；固定 `end_line` 分页不随新输出扩展**尚未经运行验证**，留待[生产实现切片计划](terminal-production-implementation-plan.md) S4 验收。长行按 byte_offset 续读，文本尽量在完整 UTF-8 字符边界截断（跨帧、跨行与跨轮转续读已验证）。

历史被轮转／显式清理时，返回游标失效和最早可用位置，不自动跳过。查询期间发生清理也必须明确处理，不能报告一个看似连续但实际缺失的范围。

### Tail

返回预算内的最近历史、单独的未完成行快照及观察衔接位置。长行只能返回尾部时标示省略前缀，提供 Read 可用的位置。

历史行游标不能替代可变尾部的版本与位置；旧尾部被覆写或清理后，旧位置必须明确失效。

### Grep

响应候选：`matches + contexts + scanned_range + next_scan_cursor? + scan_complete`。

只搜索固定历史行；匹配包含行号与位置。分页绑定搜索词、大小写选项、上下文选项及历史范围。改变查询不能继续沿用旧扫描位置。

扫描预算与返回预算分开；零匹配且 scan_complete=false 不代表整个范围无匹配。跨扫描块的长行匹配、上下文截断和 Unicode 大小写规则需要测试与字段定稿，不得因分块漏检。

## 8. 输出观察与生命周期事件

### ObserveTerminal

候选流消息：`HistoryChunk | TailSnapshot | TerminalStateUpdate`，每批带 ObservationCursor；精确字段编号与流消息变体尚未冻结。恢复位置推进到该批完整接收／应用之后，不能在只收到部分消息时提前推进。

- 从 Tail／既有观察位置追赶再跟随，不能在“读取近期内容→订阅”之间悄悄丢数据。
- 历史片段、未完成行替换与状态更新要能区分，不能把每次尾部重绘都追加成新历史行。
- 慢观察端超过有界缓冲时终止流并报明确原因，不拖住 PTY。
- client 从最后成功接收的位置重订阅；若已清理则报缺口，不自动跳最新。
- 关闭观察流只释放订阅；不停止终端，也不推进 agent 事件确认位置。
- 输出读取结束不等于整个终端进程已退出；观察流最终何时自然关闭须与状态消息一起定稿。

### WatchSessionEvents

```text
SessionEventState {
  acked_through_seq: uint64
  last_committed_seq: uint64
  pruned_through_seq: uint64
}

SessionEvent {
  session: SessionRef
  event_seq: uint64
  terminal_id: string
  payload: ProcessExited | OutputClosed | <其他生命周期事件待定>
}
```

AcquireControl 返回 SessionEventState 的一致快照；新控制端从 acked_through_seq 恢复。三个水位均为累计边界，满足 `pruned_through_seq <= acked_through_seq <= last_committed_seq`，初始值为 0。

事件不包含输出正文。event_seq 在 SQLite 事务内分配，退出状态与事件同事务提交；只有提交成功后才能向持久化流公开该序号。不承诺事务失败的内部候选编号不复用，但任何已公开事件序号均不得重用。提交先于发布的顺序、ack 单调与越界拒绝、Prune 与 `pruned_through_seq` 同事务且只删已确认连续前缀，均已通过 Gate C 验证（报告 §6）。

事件自身保存解释该事件所需的终端标识及完整事件内容，重放不要求重新查询仍存在的终端记录；具体呈现元数据需有界，不附带环境变量或原始输入。

订阅显式指定 after_event_seq。事件清理后订阅不能静默跳最新；返回已清理范围及可恢复位置。

一个 session 共用一个 agent 确认位置，新控制客户端继承它。AckSessionEvents(up_to_seq) 累计确认、重复无害、不回退、不超过已有上界；只有控制端可确认。

adapter 处理完成后确认，不代表模型或人类已读。崩溃边界允许重发；不承诺 exactly-once。重连时 adapter 汇总积压事件，避免逐事件唤醒。

磁盘故障时不承诺事件持久化。提交失败后不得向 WatchSessionEvents 发送带持久化 event_seq 的未提交事件；该流以明确的存储降级错误终止／拒绝新订阅，携带最后已提交上界（若已知）。降级状态可以通过非持久化的终端状态观察或查询报告，不伪装成可确认、可重放的事件。

仍在内存中且恢复后可补写的事件，重新提交成功后才分配公开序号；若崩溃或缓冲丢弃已失去事实，则重连时明确暴露可能不完整／中断的状态，不能伪造正常退出或声称期间无事件。恢复状态与缺失报告的最终消息字段还需定稿。

PruneSessionEvents 只清理 `through_seq <= acked_through_seq` 的连续前缀，更新 pruned_through_seq 与删除在同一事务中完成；不降低 last_committed_seq 或 acked_through_seq。`after_event_seq < pruned_through_seq` 明确报历史已清理，不能静默跳过。

## 9. 清理边界

| 操作 | 可否作用于活动终端 | 必须保留 |
| --- | --- | --- |
| ClearTerminalLogs | 可以 | 进程、行号水位、退出记录、事件 |
| DeleteTerminalRecord | 不可以；资源须已回收、事件提交完结且无未确认事件 | session 身份、全部未 Prune 事件、序号与确认水位；terminal_id 不复用 |
| PruneSessionEvents | 仅已确认事件的连续前缀；不改变任务运行 | last_committed_seq、acked_through_seq、pruned_through_seq |

DeleteTerminalRecord 不级联删除事件，也不要求先清理已确认事件；事件自解释，删除终端后仍可重放。删除前必须确认该终端没有仍待发布的生命周期事件，避免“先删记录，后出现新的未确认事件”。

日志清理的具体范围参数、原始与规范化段如何对应仍待存储定稿；不能通过级联删除偷偷清掉未确认事件。

首版不提供 DeleteSession。数据库被人为删除／替换不是普通服务重启，其旧游标处理方式需在恢复实现中显式区分。

## 10. 错误与重试

使用 gRPC status + 结构化错误详情，不沿用 HTTP ApiResponse 包装。错误载体采用 Google richer error model：trailer `grpc-status-details-bin` 的值是二进制编码的 `google.rpc.Status`，其 details 中通过 `google.protobuf.Any` 携带本服务的 ErrorDetail。不能把 ErrorDetail 裸字节直接放进该标准 trailer，也不再包一层应用层 JSON/base64。

`google.rpc.Status.code` 必须匹配外层 gRPC status；message 仅作安全的人类说明，client 不解析其文案判定恢复策略。ErrorDetail 使用稳定 reason 枚举和 typed oneof payload，表达 CONTROL_BUSY、CONTROL_EXPIRED、CURSOR_EXPIRED、TERMINAL_NOT_WRITABLE、PARTIAL_WRITE 等原因；最终编号及包路径仍待 `.proto` 定稿。

- BusyDetails：剩余租期用 optional 标量，未知不能被当成 0。
- CursorExpiredDetails：使用对应查询类型的恢复位置与缺失范围；未知／无可恢复位置用缺省表示，不构造虚假的零游标。
- PartialWriteDetails：外层消息存在且包含已知写入量，0 是已知零写入；缺少、未知或畸形详情表示量未知，不能按 proto3 默认值推断零。
- reason 与 oneof 不匹配、多个相互冲突的本服务详情、载体损坏或内外 status 不一致时，client 保留外层失败结果，将详情视为不可用；不自动重试或自动恢复。
- 未识别 Any 可忽略，未识别 reason 则按通用失败处理；不把未知详情当成功。错误详情不得包含令牌、环境值或输入内容。S6 将完整 `google.rpc.Status` 二进制载体上限定为 8 KiB，超限时服务端只保留外层通用失败。

TS client 需要显式从 grpc-js 的 ServiceError.metadata 读取二进制 trailer，再用生成代码解码，不能假定 grpc-js 自动提供 rich error 对象。Rust 可通过 tonic 的 binary details 能力发送编码后的 google.rpc.Status；具体 helper 依赖尚未额外选定。

错误载体的 tonic ↔ grpc-js 互操作已通过 Gate A 验证（`grpc-status-details-bin` 编解码、内外 status 一致性校验、未知／畸形／冲突详情降级为通用失败且不补零、不自动重试，报告 §4）；下面的常规 status 映射值仍为候选，待 `.proto` 定稿。PARTIAL_WRITE 采用原因码，外层 status 根据中止原因选择。

| 场景 | 候选 gRPC status | 详情 |
| --- | --- | --- |
| 参数不合法／缺少环境快照 | INVALID_ARGUMENT | 安全的字段原因，不回显秘密 |
| session／terminal 不存在 | NOT_FOUND | 目标标识 |
| 控制端已被占用 | FAILED_PRECONDITION | CONTROL_BUSY，参考剩余租期 |
| 控制令牌失效 | FAILED_PRECONDITION | CONTROL_EXPIRED，不返回令牌 |
| 终端不可写 | FAILED_PRECONDITION | TERMINAL_NOT_WRITABLE、当前状态 |
| 历史游标失效 | FAILED_PRECONDITION | CURSOR_EXPIRED、最早可用位置／缺失范围 |
| 活动数量超限／观察缓冲超限 | RESOURCE_EXHAUSTED | 区分资源类型及恢复方式 |
| 存储／服务暂不可用 | UNAVAILABLE | 降级状态，不承诺无副作用 |
| Stop 中止进行中写入 | CANCELLED | PARTIAL_WRITE、已知写入字节数 |
| 租约失效中止写入 | FAILED_PRECONDITION | PARTIAL_WRITE、控制权失效原因及已知写入字节数 |
| 服务端写入期限到达 | DEADLINE_EXCEEDED | PARTIAL_WRITE、已知写入字节数 |
| PTY 写入故障 | INTERNAL | PARTIAL_WRITE、已知写入字节数；安全的故障分类 |

- Start/Send 等有副作用调用不盲目重试；UNAVAILABLE、超时或取消不证明没有执行。
- 重复 Stop 的状态语义安全，不等于所有修改 RPC 都可重试。
- 取消审批则不发送；取消发送后的等待不撤销已执行操作。
- 输出流恢复与事件流恢复分别携带应用层位置，不能只依靠 gRPC 的重连／重试。
- RPC deadline、消息硬上限、输入队列和订阅缓冲上限待实现前确定；RPC 等待超时不等于终端运行超时。

## 11. 定稿前的技术验证

这些是剩余字段／实现设计和验证点，不是重新开放已确认的产品范围：

1. 租约代际提交点、在途写入中止、Stop 屏障与原子配额回收已由 Gate B 验证（100 次代际切换竞态、`log_stale_writes=0`）。
2. 类型化日志身份／位置、尾部转历史行映射、UTF-8 边界与轮转中的读取已由 Gate C 在存储层验证；Tail→Observe 一致切面待实现层验证。
3. 提交后发布事件、文件段提交顺序、崩溃恢复、事件自解释与连续前缀 Prune 的事务性已由 Gate C 验证；磁盘降级的运行时报告待实现层验证。
4. google.rpc.Status / Any / ErrorDetail 的 tonic 与 grpc-js 互操作已由 Gate A 验证（含 presence、畸形详情、未知枚举／字段与能力协商容忍）。
5. 消息与队列上限、写入期限与停止宽限期已确认为初始值（§5）；各 RPC deadline、List 分页和清理范围参数待定稿。
6. 确定 protobuf 文件组织、字段编号与代码生成流程后进入生产实现（[生产实现切片计划](terminal-production-implementation-plan.md)）；当前不创建这些实现文件。

## 12. 对应验收场景

- agent 环境完整传递、显式空环境与缺失环境可区分；服务环境不泄漏进入子进程。
- 两个控制端竞争、心跳过期、重连、旧令牌迟到及 daemon 重启。
- 根进程快速退出、无换行尾部、根退出但子进程保留 PTY、EOF 早于根退出。
- 部分写入、响应丢失、停止与写入竞争；无自动重复输入。
- 长行续读、进度条覆写、查询中轮转、grep 扫描预算耗尽、慢观察端断开。
- 退出事件重放与累计确认，观察端不确认，清理后旧游标明确失效。
- 已退出但输出打开的终端仍占配额、Stop 清理残留、未确认事件阻止记录删除。

本文件仅整理设计；执行与存储语义已由探针验证（见[验证报告](../research/terminal-technical-validation.md)），上述端到端 RPC 场景待生产实现后验证。

## 13. 错误载体核验来源

以下来源用于载体定义核验；Rust/TS 联调已由 Gate A 覆盖（报告 §4）：

- [gRPC richer error model](https://grpc.io/docs/guides/error/#richer-error-model)
- [google.rpc.Status 定义](https://github.com/googleapis/googleapis/blob/master/google/rpc/status.proto)
- [tonic Status 的 binary details 接口](https://docs.rs/tonic/latest/tonic/struct.Status.html)
- [grpc-js Metadata 的二进制读取实现](https://github.com/grpc/grpc-node/blob/master/packages/grpc-js/src/metadata.ts)
- [grpc-node error details 示例](https://github.com/grpc/grpc-node/tree/master/examples/error_details)

grpc-js 的 metadata 支持不等于自动支持本协议的 ErrorDetail；相关解码、校验和降级行为属于 TS client 的职责。
