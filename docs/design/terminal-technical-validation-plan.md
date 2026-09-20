# Agent terminal：最小技术验证计划

状态：已完成；Gate A：PASS，Gate B：PASS，Gate C：PASS。环境、版本、命令与逐项运行证据见[技术验证结果报告](../research/terminal-technical-validation.md)。本文为历史计划，正文保留批准时原文，不再更新；据此的设计修订见[设计基线](agent-terminal.md)与[协议草案](terminal-protocol-v1.md)，后续实施见[生产实现切片计划](terminal-production-implementation-plan.md)。

原文计划范围：只规划三个可丢弃探针，不批准完整 terminal 实现，也不冻结生产 `.proto` 字段。

设计依据：

- [Agent terminal 设计基线](agent-terminal.md)
- [Terminal 协议 v1 草案](terminal-protocol-v1.md)
- [项目目录结构](../architecture/project-layout.md)

## 1. 目标与停止条件

依次回答三个问题：

1. Rust `tonic` server 与 Node.js `grpc-js` client 能否通过 Unix socket 正确互操作，并保留本协议依赖的 64 位整数、presence、流取消和 richer error 语义？
2. `pty-process` 能否支持已确认的 PTY 生命周期，尤其是根进程退出与输出关闭分离、阻塞写入可中止、Stop 屏障和进程组清理？
3. 分段日志文件 + SQLite 能否在非原子双写、崩溃和清理后恢复到明确且不伪造连续性的状态？

每个探针都必须产出可重复命令、原始测试结果和明确结论。某一探针失败时停在该层，带证据返回选型讨论，不静默更换库或扩展产品范围。

三个问题得到证据、剩余分叉经用户确认后即停止。不要继续实现 18 个 RPC、pi adapter、观察 CLI、完整输出规范化或生产迁移。

## 2. 执行形态

- 在独立 jj workspace `~/Projects/.workspace/qingluan/terminal-validation` 中执行；不直接污染当前工作区。
- 探针代码放在临时分支的 `probes/terminal/`，明确标记为非生产代码。
- 使用根 `.justfile` 提供以下临时入口：
  - `terminal-probe-generate`
  - `terminal-probe-grpc`
  - `terminal-probe-pty`
  - `terminal-probe-storage`
  - `terminal-probe-all`
- Rust 生成产物使用 Cargo `OUT_DIR`；TS 生成产物放入 `target/terminal-probes/generated-ts/`。两者均不提交。
- 探针只使用最小实验消息和 fixture，不复制完整候选 RPC 表。
- 临时数据库、socket 和日志全部位于测试临时目录，测试后可整目录删除。
- 探针完成后在临时分支保留一个可定位的 jj change；主线只保留验证结论和用户批准的设计修订，不合入探针实现。

## 3. Gate 0：版本与工具链确认

编码前先形成一张候选版本表并请用户确认：

- Rust：`tonic`、`prost`、`tonic-prost-build`／对应构建 crate、`pty-process`、`nix`、`sqlx`。
- Node.js：运行时基线、`@grpc/grpc-js`、`ts-proto`、TypeScript。
- 工具：`protoc` 及 ts-proto 插件调用方式。
- 外部协议：固定 revision 的 `google/protobuf/*.proto` 与 `google/rpc/status.proto`，记录上游 URL、revision 和许可。
- richer error helper：优先使用生成类型直接编码；若要新增 helper 依赖，单独说明收益并再次确认。

版本依据为官方当前文档、crate/package 元数据及仓库现有工具链，不凭旧示例猜版本。`protoc` 等工具声明式加入 `devenv.nix`／flake dev shell；遇到网络问题先询问用户，不调整 mihomo。

Gate 0 通过标准：在干净 dev shell 中，一个 `just` 命令可完成生成和两端类型检查，且生成目录保持未跟踪。

## 4. 探针 A：Rust ↔ Node gRPC/UDS

### 最小协议

只定义四类行为：

- unary echo：`bytes`、大于 `2^53` 的 `uint64`、`optional uint64`、枚举。
- server stream：单调序号和可恢复位置。
- structured failure：`google.rpc.Status` + `Any<ProbeErrorDetail>`。
- server info：协议主版本与一组能力值。

### 场景

1. Rust server 监听临时 Unix socket；Node client 连接、调用并正常关闭。
2. `uint64` 往返保持 `bigint` 精度；进入 JSON 展示层时才转十进制字符串。
3. `optional` 的“缺失”和“显式 0”在 Node → Rust → Node 往返后仍可区分。
4. `bytes` 包含 NUL 和非 UTF-8 数据时不被字符串化。
5. Node 取消 server stream 后 Rust 观察到取消，任务和连接不泄漏；client 能从最后完整应用的序号重新订阅。
6. deadline 到期得到明确 gRPC status，不被 client 解释为业务成功。
7. Rust 返回标准 `grpc-status-details-bin`；Node 显式解码 `google.rpc.Status`、`Any` 和本服务 detail。
8. 覆盖已知 0 写入量、detail 缺失、损坏载体、未知 `Any`、未知 reason、内外 status 冲突；只有合法详情进入类型化错误，其余降级为通用失败。
9. 未知字段和未知能力不会导致同一主版本内的基础调用失败；未知枚举的实际生成行为被记录，不先假定。
10. socket 已存在、server 关闭及 client 取消后的清理行为可重复执行；记录最终 socket 权限和启动清理策略候选。

### 通过标准

- `just terminal-probe-grpc` 从生成开始，在 Rust server + Node client 上自动完成上述场景。
- 无手写重复 wire 类型；Rust/TS 都来自同一实验 `.proto`。
- 能明确给出 ts-proto 的实际生成参数、UDS 地址形式、取消方式和 richer error 解码流程。
- 任何畸形 rich detail 都不会触发自动重试，也不会把未知值补成 0。

### 决策 Gate A

向用户提交证据并确认：

- 依赖版本与 Node 最低版本候选；
- ts-proto 参数及 ESM/CJS 输出形式；
- 外部 proto 固定 revision；
- richer error 是否需要 helper；
- UDS socket 生命周期和权限策略。

通过前不定稿生产 `.proto`。

## 5. 探针 B：PTY 生命周期与 Stop 屏障

探针直接测试 PTY 模块，不经过 gRPC 或 SQLite，避免把失败归因混在一起。fixture 使用本地轻量程序，不运行真实训练任务。

### 最小探针接口

只暴露 `spawn`、`write`、`resize`、`stop`、`next_event`；完整内部状态始终打印到测试日志。控制代际、输入分块和配额只实现足以验证既定规则的最小模型，不作为生产模块复用。

### 场景

1. 显式 `program + args + cwd` 启动；完整环境、显式空环境与缺失环境三者可区分，daemon/probe 自身环境不意外混入。
2. 发送 UTF-8、原始控制字节及多行数据；响应记录实际写入量。
3. resize 后由子进程读取并回报真实终端尺寸。
4. 根进程快速退出但留下无换行尾部；ProcessExited 与正常 OutputClosed 独立观测，最终尾部不丢失。
5. 根进程退出、子进程仍持有 PTY；状态保持“根已退出、输出仍打开”，配额不提前释放。
6. shell 启动前台任务；Stop 先温和信号、到期后强制停止，验证前台进程组和仍持有 PTY 的同组进程被处理。
7. 后台同组进程与主动脱离进程分别测试；主动脱离者仍存活应被记录为已知非保证，而不是探针失败。
8. 子进程不读取输入，制造阻塞写入；Stop 必须绕过普通 FIFO，在有界时间内提交停止意图、中止剩余写入，并报告已知写入量。
9. 写入期间租约代际失效；失效后不再提交新片段，已写入字节不回滚。
10. 并发 Start 预留 session/global 配额；启动失败、停止和清理竞争后每个名额只释放一次，只有 Released 不占位。
11. probe 进程异常结束后重新启动，只把旧活动记录判为 Interrupted；不向旧 PID 发信号、不伪造退出码。

### 通过标准

- `just terminal-probe-pty` 可重复运行，事件日志能证明退出、EOF、Stop 和资源释放的先后关系。
- 阻塞写入不会让 Stop 等待整个写入或 client deadline；实际调度延迟和温和／强制停止耗时被测量，不凭感觉定数值。
- `pty-process` 能提供所需生命周期原语；若必须绕过其核心抽象或依赖不稳定内部行为，则判定选型未通过。
- 探针明确列出 Linux 下能回收与不能保证回收的进程集合。

### 决策 Gate B

向用户提交证据并确认：

- 是否正式采用 `pty-process`；
- 温和停止信号、宽限期、强制停止方式；
- 输入块大小、队列上限、写入期限和 Stop 调度上界的候选值；
- 进程组／session 清理的准确承诺。

## 6. 探针 C：混合存储与恢复

探针使用 scratch SQLite 和分段文件，只实现验证不变量所需的最小格式。SQLx 使用运行时查询和版本化迁移，不使用编译时 query 宏。

### 待验证的提交假设

首选候选顺序：

1. 向带长度与校验信息的 segment frame 追加数据；
2. flush，并在持久化提交点执行必要的同步；
3. 用短 SQLite 事务提交可见索引／行号水位；
4. 事务提交后才向查询或事件流公开。

探针必须通过 crash point 对比证明该顺序可恢复；它不是未经验证的生产结论。若需要反向顺序或 WAL 式协调，应先停下讨论。

### 场景

1. 在“文件写入前／中／后、同步前／后、SQLite 事务前／中／后、发布前／后”逐点杀死 writer 子进程，再重启恢复。
2. 文件尾出现半 frame、校验失败 frame、已同步但未索引的孤儿 frame；恢复只能截断／隔离不可见尾部，不能重复历史行号。
3. SQLite 索引指向缺失、截短或损坏 segment；标记缺失／降级，不返回伪造的连续日志。
4. 普通重启保持 `log_epoch` 和稳定行号；只有破坏性、无法解释的重建才切换 epoch 并拒绝旧游标。
5. 超长行跨 frame 和 UTF-8 边界；最早可用位置包含 `line + byte_offset`，轮转后可继续读或明确 CURSOR_EXPIRED。
6. 尾部变成历史行的映射在轮转前可恢复；旧尾部 revision 已不可解释时明确失效。
7. 退出状态与 session event sequence 在同一 SQLite 事务提交，事务后发布；崩溃后已公开序号不重用。
8. Ack 单调、不超过 committed 上界；Prune 只删除已确认连续前缀，并与 `pruned_through_seq` 更新同事务完成。
9. 模拟 append、sync、SQLite commit 失败；PTY drain 的替身 producer 继续运行，有界内存溢出后记录明确缺失并拒绝新 Start。
10. 清理运行中日志不杀进程、不重置行号；记录删除仍受资源回收和未确认事件约束。

### 通过标准

- `just terminal-probe-storage` 自动遍历 crash matrix；每个恢复结果归类为完整、明确缺口或明确 Interrupted，不允许伪造成功。
- 数据库事务中不包含文件扫描或等待 client。
- 恢复是幂等的：同一损坏样本连续恢复两次结果一致，不继续扩大损坏。
- 可以写出确定的 segment frame、索引、提交顺序、恢复和 epoch 规则；不能回答则不进入生产存储实现。

### 决策 Gate C

向用户提交证据并确认：

- segment frame 和校验方式；
- flush/fsync 策略及性能代价；
- 文件／SQLite 提交顺序；
- 孤儿尾部、损坏索引和缺失 segment 的恢复策略；
- 轮转粒度与首版元数据上限。

## 7. 最终综合与主线产物

三个 Gate 均通过后，仅在主线保留：

1. `docs/research/terminal-technical-validation.md`：环境、精确版本、命令、通过／失败证据、已知限制和临时分支 change ID。
2. 对 [设计基线](agent-terminal.md) 与 [协议草案](terminal-protocol-v1.md) 的最小修订：只写用户确认且被运行证据支持的结论。
3. 下一阶段的生产实现切片计划；它需要单独批准。

不把以下内容当作本轮成果：完整生产 `.proto`、`qingluan-terminal` 正式实现、daemon gRPC 接入、TS client 正式包、pi 工具、CLI、UI、远端能力或权限框架。

## 8. 总体验收矩阵

| 证据 | gRPC | PTY | 存储 |
| --- | --- | --- | --- |
| 一个 just 命令可重复 | 必须 | 必须 | 必须 |
| 正常路径 | UDS unary/stream | spawn/send/resize/exit | append/read/restart |
| 取消／停止 | stream cancel/deadline | blocked write + Stop | writer crash |
| 精确类型／位置 | bigint/presence/bytes | written bytes/events | line+offset/epoch |
| 故障降级 | malformed rich error | read/kill failure | corruption/I/O failure |
| 重启恢复 | 重新连接，不假定调用未执行 | 旧活动项 Interrupted | crash matrix + event replay |
| 明确非保证 | 不自动重试副作用 | detached process | 双写不具备跨介质原子事务 |

只有矩阵全部有运行证据，才认为技术验证完成。文档推断、库接口存在或编译通过不能替代运行验证。
