# Agent terminal：最小技术验证结果（Gate A / B / C）

状态：三个 Gate 均已通过运行验证，本文只记录环境、精确版本、命令、判定标准与实测结果，以及仍需保留的限制。**本文不代表批准生产实现**；探针代码保持 throwaway，生产实现尚未开始。

设计依据：[最小技术验证计划](../design/terminal-technical-validation-plan.md)、[Agent terminal 设计基线](../design/agent-terminal.md)、[协议 v1 草案](../design/terminal-protocol-v1.md)。

## 1. 验证范围与证据来源

计划（§1）要求依次回答三个问题：gRPC over UDS 互操作、`pty-process` 生命周期与 Stop 屏障、分段日志 + SQLite 的崩溃恢复。计划（§7、§8）要求每个结论都有可重复命令与运行证据，文档推断、接口存在或编译通过都不能替代运行验证。

| Gate | 问题 | 主要证据 |
| --- | --- | --- |
| A | Rust `tonic` server ↔ Node `@grpc/grpc-js` client 通过 Unix socket 互操作，保留 64 位整数、presence、取消与 richer error 语义 | `probes/terminal/grpc/{generate.sh,run.sh}`、`probes/terminal/grpc/rust/**`、`probes/terminal/grpc/ts/**`（含 `Cargo.toml`／`Cargo.lock`／`package.json`／`pnpm-lock.yaml`）及同一实验 `.proto` |
| B | `pty-process` 能否支持已确认的 PTY 生命周期、Stop 屏障与进程组清理 | `probes/terminal/pty/**`、原始运行日志 `/tmp/pty-verify-cold.log` 与 `/tmp/pty-verify-warm.log`、`probes/terminal/README.md` 的 Gate B 记录 |
| C | 分段日志 + SQLite 在非原子双写、崩溃与清理后是否恢复到不伪造连续性的状态 | `probes/terminal/storage/**`、原始证据目录 `~/Projects/.workspace/qingluan/terminal-validation/target/terminal-probes/storage-evidence/20260920T123205Z-2678639` 与 `…/20260920T123255Z-2684721`（`storage-evidence/LATEST` 指向后者） |

Gate C 的每条结论都来自保留的 `workdir`（逐点 sqlite DB、segment 文件、quarantine 工件、trace、两份 `summary.json` 与 `transcript.txt`），不是文档推断。Gate A 的逐场景断言来自已提交探针源码；见 §7 的证据限制。

## 2. 环境与精确版本

主机：Gate C transcript 保留的是 `host=Linux 6.12.108 x86_64`（`storage/run.sh` 仅采集 `uname -srm`，未采集发行版与核数）；“NixOS Linux、16 核”为操作者环境注记，不是保留证据中的实测采集。

| 项 | 版本 | 来源 |
| --- | --- | --- |
| cargo | 1.97.0 (c980f4866 2026-06-30) | `…/20260920T123205Z-2678639/workdir/transcript.txt` |
| rustc | 1.97.1 (8bab26f4f 2026-07-14) | 同上 |
| Node.js（dev shell，已实测） | 24.19.0 | `probes/terminal/README.md` |
| Node.js（pinned nixpkgs，已实测） | 22.23.2 | 同上 |
| Node.js 最低候选 | 22.12.0 | `probes/terminal/grpc/ts/package.json` 的 `engines.node: ">=22.12.0"`；**未单独执行该 patch 版本** |
| pnpm | 11.21.0 | 同上 |
| TypeScript | 6.0.3 | 同上 |
| ts-proto | 2.12.4 | 同上 |
| @grpc/grpc-js | 1.14.5 | 同上 |
| @bufbuild/protobuf（ts-proto 运行时） | 2.14.1 | 同上 |
| @types/node | 24.12.2 | 同上 |
| protoc（pinned nixpkgs） | 35.1 | `probes/terminal/README.md` |
| tonic / tonic-prost / tonic-prost-build | 0.14.6 | `probes/terminal/grpc/rust/Cargo.toml` |
| prost / prost-types | 0.14.4 | 同上 |
| pty-process（`async`） | 0.5.3 | `probes/terminal/pty/rust/Cargo.toml` |
| nix（`process,signal,term,ioctl`） | 0.31.3 | 同上 |
| sqlx（`runtime-tokio,sqlite,migrate`） | =0.9.0 | `probes/terminal/storage/rust/Cargo.toml` |
| crc32fast | 1.5.2（构建日志） | `…/20260920T123205Z-2678639/workdir/transcript.txt` |
| tokio / uuid | 1.53.1 / 1.26.1（构建日志） | 同上 |

Node 说明：同一份生成产物与同一 lockfile 在 24.19.0 与 22.23.2 上均通过；**22.12.0 只作为最低候选（`engines` 约束），没有单独运行**，因此“22.12.0 可用”未被运行证据覆盖。

外部协议定义固定 revision 随仓库保存（`probes/terminal/third_party/`），来源与许可：

| 文件 | 上游 | revision/tag | 许可记录 |
| --- | --- | --- | --- |
| `third_party/google/rpc/status.proto` | `googleapis/googleapis` | `9f99764bb7841a50f0e46c41fd958a296b108e60` | Apache-2.0 |
| `third_party/google/protobuf/any.proto` | `protocolbuffers/protobuf` | `v35.1` | BSD-3-Clause |

生成只从 `third_party/` 解析 `google/protobuf/any.proto`（与 pinned protoc 35.1 一致），不回退到 protoc 安装目录的 include，构建期不联网。

## 3. 执行形态与统一入口

探针在独立 workspace `~/Projects/.workspace/qingluan/terminal-validation` 中执行，避免干扰主线日常工作区；入口在根 `.justfile`：

```console
just terminal-probe-generate   # protoc + ts-proto + cargo build + tsc typecheck
just terminal-probe-grpc
just terminal-probe-pty
just terminal-probe-storage
just terminal-probe-all        # grpc + pty + storage
```

Rust 生成产物走 Cargo `OUT_DIR`，TS 生成产物在 `target/terminal-probes/generated-ts/`，两者均不提交。临时 socket、数据库与日志都在 `/tmp` 下的 mktemp workdir；成功即清理（Gate C 改为把整个 workdir 移入 `target/terminal-probes/storage-evidence/<run-id>/`），失败保留 workdir 供调试。

## 4. Gate A：Rust ↔ Node gRPC/UDS

### 方法

`probes/terminal/grpc/generate.sh`：

1. `pnpm --dir probes/terminal/grpc/ts install --frozen-lockfile`；
2. 重建 `target/terminal-probes/generated-ts/`，写入 `{"type":"module"}` 的 `package.json`（NodeNext 判定 ESM，否则生成代码按 CJS 输出、命名 ESM import 失败）；
3. `protoc --proto_path=probes/terminal/proto --proto_path=probes/terminal/third_party --plugin=protoc-gen-ts_proto=…/node_modules/.bin/protoc-gen-ts_proto`，编译 `probe.proto` 与 `third_party/google/rpc/status.proto`；
4. ts-proto 参数（照抄）：`outputServices=grpc-js,forceLong=bigint,env=node,esModuleInterop=true,importSuffix=.js,useOptionals=none,oneof=unions-value`；
5. `CARGO_TARGET_DIR=target/terminal-probes/rust cargo build --manifest-path probes/terminal/grpc/rust/Cargo.toml`；
6. `pnpm --dir probes/terminal/grpc/ts run typecheck`。

Rust 与 TS 来自同一实验 `.proto`，无手写重复 wire 类型。UDS 地址形式为 `unix:<absolute-path>`（`probes/terminal/grpc/ts/src/client.ts`），凭证为 insecure，socket 权限必须为 `0600`。

`probes/terminal/grpc/run.sh` 先做 socket 路径安全前置检查，再启动 Rust server 并运行编译后的 Node ESM client，最后验证优雅关闭后的 socket 清理与重复启动。

### PASS 判据（client 断言，全部为硬断言）

- socket 必须存在且 mode `0600`；
- **socket 路径安全**：普通文件被拒绝且内容不变（`refusing to replace non-socket path`）；活动 socket 被拒绝且不 unlink、第一个 server 继续服务（`refusing to unlink active socket`）；SIGKILL 后的 stale socket 可被回收；干净关闭后可再次绑定；
- `GetServerInfo` 返回 `protocolMajor == 1`，包含 `echo/stream/rich-error`，且 fixture 必须广告至少一个本 client 未知的能力（同一 v1 容忍未知能力）；
- unary echo：`bytes` 含 NUL 与非 UTF-8（`00 ff 80 41`）逐字节一致；`sequence = 9007199254740993`（> 2^53）保持精度；`optional_count` 缺失与显式 `0` 可区分；
- 未知字段：请求侧未知字段被 server 忽略；响应侧追加未知 varint 与未知 length-delimited 字段后由较旧 decoder 解码，已知字段不丢、不伪造；
- 未知枚举：数值 `777` 在二进制往返后原样保留；
- deadline：过期调用得到 `DEADLINE_EXCEEDED`，不被当作业务成功；
- 取消与续订：client 在第 3 条后 `call.cancel()`，client 侧为 `CANCELLED`，server 侧写出取消 marker（证明 server 观察到取消），client 从最后完整应用的序号重新订阅得到 `[last+1, last+2]`；
- richer error：`google.rpc.Status` + `Any<ProbeErrorDetail>` 走 `grpc-status-details-bin`，3 个合法场景被接受为类型化 `partial-write`（含 `written_bytes = 0`、`= 9007199254740993`，以及“合法 detail 与未知 Any 共存”）；10 个降级场景各自映射到固定通用原因（`reason-payload-mismatch`／`missing-or-conflicting-detail`／`malformed-status`／`outer-inner-status-mismatch`／`missing-or-ambiguous-status`／`unknown-reason`／`unknown-written-count`／`malformed-detail`／`status-details-too-large`），不补 0、不自动重试。

### richer error 解码流程（已确认，来自 `client.ts`）

`error.metadata.get("grpc-status-details-bin")` → 必须恰好 1 个 Buffer → 超过探针上限 `MAX_STATUS_DETAILS_BYTES = 4096` 直接判 `status-details-too-large`（解码前拒绝）→ `google.rpc.Status.decode` → `status.code` 必须等于 `error.code` → 按 `typeUrl` 过滤本服务 detail 且必须恰好 1 个 → `ProbeErrorDetail.decode` → `reason` 必须是 `PARTIAL_WRITE` → oneof 必须是 `partial-write` → `written_bytes` 必须存在。任一步失败降级为通用失败。未知 `Any` 条目被忽略，只有冲突的**已知** detail 才降级。

### 通过的证据与保留项

成功标记：`run.sh` 以 `PROBE_OK socket-path-safety regular-file-preserved active-socket-preserved stale-socket-reclaimed duplicate-run-clean` 结束，client 输出 JSON（`ok`、`node`、`addressForm`、`socketMode`、`bigint`、`optionalAbsent`、`optionalZero`、`unknownEnum`、`cancellationObserved`、`unknownCapabilitiesTolerated`、`responseUnknownFieldsDecoded`、`richErrorAccepted: 3`、`richErrorFallbacks: 10`）。

探针局部限制：`MAX_STATUS_DETAILS_BYTES = 4096`，server 的 `OVERSIZED_STATUS` fixture 约 6 KiB（高于探针上限、但 gRPC 传输仍可投递），因此该场景验证的是 client 的尺寸守卫，不是传输层拒绝。

## 5. Gate B：PTY 生命周期与 Stop 屏障

### 已确认的决策

- 采用 `pty-process` 0.5.3 负责 `open／spawn／read／resize`；为固定 Stop 与租约代际的提交顺序，**写端使用 master fd duplicate + 自管 `AsyncFd` 非阻塞写入**，不复用其写路径。
- 每个 terminal 一个独立 cgroup v2；生产 user service **必须配置 `Delegate=yes`**；缺失 delegation 或 `cgroup.kill` 时硬失败，不降级到 `/proc` session 快照。
- 温和阶段用 **pidfd** 向 cgroup 当前成员发 `SIGTERM`，宽限 **600 ms**；随后 `cgroup.kill`，最长等待 **3 s** 至 `cgroup.events populated=0`。
- 输入块 **4 KiB**；单条 Send 最大 **256 KiB**；有界队列 **2 条** + **1 条 in-flight**，accepted payload backing 上界 **768 KiB**（accepted payload 规范化为 exact-length `Box<[u8]>`，不保留 caller `Vec` spare capacity）；默认写入期限 **10 s**。
- 输出关闭最长等待 **1 s**，超时提交唯一 `OutputClosed(Forced)`；Stop 在内部 detached 执行，首个调用者取消不撤销，后续调用共享同一结果。

### 方法

`just terminal-probe-pty`（`probes/terminal/pty/run.sh`）分三段：`cargo build`；**6 个单测**（配额状态机：double release、竞争、启动失败）；crash 阶段（留下仍存活的 fixture + registry，probe 进程随即退出）；主阶段 **25 个场景**。清理按 cgroup 身份执行（`cgroup.kill` + 删除 probe cgroup root），不使用 pgrep，不使用裸 PID；成功不留残留，失败保留 workdir（`events.jsonl`、`registry.json`、`spawned.log`、`summary.json`）。fixture 为本地轻量程序，不运行真实训练任务。

### 实测（原始日志 `/tmp/pty-verify-warm.log`，本机 NixOS Linux）

| 场景 | 实测值 |
| --- | --- |
| identity | `term_phase_ms=21`、`kill_phase_ms=21`、`signalled=1`、`forced=false`、`total_ms=3023`（pid == sid == pgid） |
| env | `full=marker+path`、`empty=no marker, no PATH`、`missing=rejected`、`probe_env_leak=false` |
| write_bytes | payload 300 B、checksum 21710、`written=300`、accepted backing `exact-length`（caller 有 8 MiB spare capacity 仍不保留） |
| resize | 120×30 → 100×40 → 80×30（子进程读取回报） |
| trailing | `end=eof`、`exit_code=0`、`tail_preserved=true`（无换行尾部不丢） |
| child_holds | 根已退出、子进程仍持 PTY，`end=forced`，配额未提前释放 |
| shell_jobs | 前台 pgrp=2415968，3 个成员，pgrps `[2415966,2415967,2415968]`，`signalled=3` |
| kill_escalation | `term_phase_ms=601`、`term_rounds=6`、`kill_phase_ms=613`、`forced=true`、`signalled=6` |
| blocked-write-stop | 不读 stdin 时 `blocked_at=15360` B（约 12–16 KiB 后阻塞）、`written=15360`、`abort_latency_ms=0`、stop `total_ms=627` |
| generation | `blocked_at=13824`、`written=13824`、`abort_latency_ms=0`、`log_stale_writes=0` |
| generation-race | `bumps=100`、`log_switches=100`、`log_writes=312`、`sends_complete=255`、`sends_aborted_control_lost=1`、`sends_rejected_stale=0`、`total_bytes=1048576`、`final_write_generation=101`、`last_write_after_last_switch=101`、`stream_matches_accounting=true` |
| write-deadline | 探针用 300 ms 期限，`latency_ms=301`（有界提交，非无限等待） |
| oversize | `max_send_bytes=262144` 接受（照常阻塞）；`rejected_bytes=262145` 类型化拒绝；`queue_capacity=2`；`accepted_queued_inflight_bound_bytes=786432` |
| queue-full | 队列满时 `rejected=1` |
| stop-idempotent | `intent_count=1`、`completion_count=1`、`shared_result=true` |
| close-race | a：`end=eof`；b：`forced=true`、`output_events=1` |
| monitor-fault | `fabricated_exit=false`（不伪造退出） |
| start-faults | 5 个启动故障点均回滚：`BeforePty/AfterPtyOpen/AfterSpawn/AfterProcStat/AfterTaskStart` |
| quota | `session_limit=2`、`global_limit=4`、`final_occupying=0`，失败启动与 Stop 竞争后每名额只释放一次，只有 Released 不占位 |
| registry-interrupted | `recovered=crash-t1`、`exit_fabricated=false`、`recovery_signal_calls=0`（不向旧 PID 发信号） |
| detached | `escaped_session=true` 且 `stayed_in_cgroup=true`、`reclaimed_by_cgroup=true`（主动 `setsid` 仍留在 terminal cgroup，被回收） |
| term-fork | TERM-immune 子进程 `children_immune_to_term=true`、`children_per_term=3`、`signalled=51`，温和阶段后由 `cgroup.kill` 固定点回收 |
| stop-cancellation | leader 在 `StopIntentCommitted` 后被中止，`detached_completion=true`、`shared_result=true`；故障变体 `bounded_ms=589`、`completion_count=0`、`shared_failure=true`、slot 保持 `Active` |
| rollback-cleanup-failure | reap error／timeout 后 `slot=Cleaning`、`released=false`、cgroup 保留交 probe-root sweep（故意展示失败语义） |

cgroup 归属（同日志）：`/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/app-ghostty-surface-transient-1736175.scope/qingluan-terminal-probe-qingluan-terminal-pty-probe.zd4egP/…`。

### PASS 判据与结论

- 6 个单测全过；`PROBE_OK scenarios=25`；`PROBE_OK no-leftover-cgroups no-leftover-processes no-leftover-tempdirs`；`PROBE_CLEAN workdir-removed`。
- 阻塞写入不会让 Stop 等待整个写入或 client deadline：中止延迟 0 ms，且精确报告已知写入量。
- Linux 最后一个 slave 关闭后 master read 返回 **EIO（errno 5，而非 EOF）**，探针映射为正常 `OutputClosed`；root exit 与 output close 独立且各提交一次。
- 结论：`pty-process` 提供所需生命周期原语且各场景通过，但**写路径必须由探针侧自管 `AsyncFd`**，这是已确认的采用条件，不是可选优化。

## 6. Gate C：混合存储与恢复

### 已确认的决策

**文件格式（`probes/terminal/storage/rust/src/frame.rs`，整数一律小端）**

| 结构 | 布局 |
| --- | --- |
| segment header（**64 B**，创建时写一次） | `[0..4]` magic `QLSG`；`[4..6]` version u16=1；`[6]` kind；`[7]` flags；`[8..24]` terminal UUID16；`[24..40]` log epoch UUID16；`[40..48]` segment_id u64；`[48..56]` created_ms u64；`[56..60]` crc32(0..56)；`[60..64]` reserved=0 |
| frame header（**40 B**） | `[0..4]` magic `QLFR`；`[4..6]` version u16=1；`[6]` kind；`[7]` flags（bit0 = 本行最后一帧）；`[8..16]` frame_seq u64；`[16..24]` line u64；`[24..32]` line_offset u64；`[32..36]` payload_len u32（≤ **64 KiB**）；`[36..40]` crc32(0..36) |
| payload | ≤ **64 KiB**，只在 UTF-8 字符边界切分 |
| payload 尾 | **4 B payload CRC32** |

校验使用 `crc32fast`（IEEE CRC32），**只用于检测意外损坏，不提供防篡改或加密保证**（设计定性，非实测强度结论）。

**提交顺序（已通过 crash point 对比验证）**：append 完整帧 → `sync_data` → 短 SQLite 事务（segment/terminal/event/exit 同一事务）→ 事务提交后才对查询与事件流公开。首次创建 segment 时先 `sync_data` header，**并强制 fsync 父目录**（trace 步骤 `seg_dir_fsynced`）；目录项持久化是强制的，不是优化项。

**恢复规则**：坏／半／完整但未索引的尾部一律截断回 `committed_bytes` 边界并隔离，永不采纳；索引指向缺失／截短／损坏 segment → 显式 `log_gap` + degraded；`line_watermark` 绝不回退、行号绝不复用（继续写入编号 = watermark+1）；连续两次 recover 幂等；正常重启与轮转保持 `log_epoch`；删除 DB+WAL+SHM 属破坏性重建 → 换新 epoch、旧文件隔离、旧游标带最早位置拒绝；退出状态与 session event seq 同一事务提交、提交后才发布；ack 单调且不越界，Prune 只删已确认连续前缀并与 `pruned_through_seq` 同事务。

**生产初始默认值（设计确认，非本探针实测）**：segment **4 MiB** 起步；批处理 **64 KiB 或 50 ms**（先到者为准）；每个 terminal 最多保留 **64 条 segment 元数据**；合并后的 gap 记录上限 **1024**。探针本地使用 256 KiB 轮转阈值（`MAX_SEGMENT_BYTES = 256 * 1024`），场景内另用 4096 B 阈值制造多次轮转，因此**上述生产值本身没有作为边界被跑过**。

### 方法

`just terminal-probe-storage`（`probes/terminal/storage/run.sh`）= `cargo build` + **35 个单测** + 单进程 `selfcheck`（`timeout 30`）+ `gate-c` harness（自带 `timeout 100`；整条命令按外部 `timeout 120 just terminal-probe-storage` 验证）。writer 子进程在精确边界以 `libc::_exit(70)` 死亡（覆盖进程崩溃），另有 3 个 power-loss 变体在 `_exit` 后把文件物理截断到 fsynced checkpoint（模拟断电后未落盘的部分），`recover` 一律在新进程执行且连跑两次验证幂等。SQLx 只用运行时查询 + 版本化迁移，不使用编译时 query 宏。

### 实测（两次运行，Gate C 原始证据）

| 指标 | run `20260920T123205Z-2678639` | run `20260920T123255Z-2684721` |
| --- | --- | --- |
| gate-c 墙钟（`summary.json` `duration_ms`） | 6882 ms | 6849 ms |
| `cargo build` | 16.13 s（cold） | 0.05 s（warm） |
| 单测 | 35 passed（0.29 s） | 35 passed（0.29 s） |
| selfcheck | `SELF-CHECK-OK` | `SELF-CHECK-OK` |
| gate 结果 | `GATE-C-OK points=20 scenarios=19 power_loss_variants=3 crashed_children=25 recover_children=68` | 同左 |
| 检查项 | — | `total_checks=186`、`ok=true` |

README 另记录整条命令的 cold 27.7 s / warm 7.6 s，以及每次成功 run 保留约 6.5 MiB 证据（gate-c 2.0 MiB、scenarios 4.3 MiB、selfcheck 364 KiB）。

crash matrix 逐点结果（`gate-c/summary.json`，两次运行归类一致）：

| 崩溃点 | 恢复归类 | 关键观察 |
| --- | --- | --- |
| `seg_before` | clean，watermark 0 | 无文件、无行 |
| `seg_header_written` | pending | 头已写；power-loss 变体（checkpoint 0）目录项未持久 → 移除，仍 pending |
| `seg_header_synced` | pending | trace 证明 `seg_dir_fsynced` |
| `seg_before_db_row` | orphan | 文件+头+目录 fsync 均持久但无 DB 行 → 整文件 discover + quarantine，绝不采纳 |
| `frame_before_write` | clean，watermark 2 | 编号继续 = 3 |
| `frame_mid_write` | partial（1 action），watermark 2 | 半帧 torn write 被截断；power-loss 变体截到 checkpoint 392 → clean |
| `frame_after_write` | partial，watermark 2 | 同上；power-loss 变体截到 392 → clean |
| `frame_after_sync` | partial，watermark 2 | 已 fsync 但未索引 → 不采纳 |
| `txn_begin` / `txn_update` / `txn_before_commit` | partial，watermark 2 | 未提交事务真实回滚 |
| `txn_after_commit` | clean，watermark 3 | 编号继续 = 4 |
| `publish_before` | clean，watermark 3 | 未发布 |
| `publish_after` | clean，`stdout_published_line=3` | commit-before-publish 顺序被 stdout 记录证明 |
| `event_insert` | partial，watermark 2 | — |
| `event_commit` | clean，`events_after=1` | 事件已提交 |
| `event_publish` | clean，`stdout_published_line=3`、`events_after=1` | 顺序证明 |

每个 matrix 点都验证：连续两次 recover 幂等（第二次零动作、状态快照一致）、行号不复用、`lines 1..=N byte-exact`。

19 个场景（全部 `ok=true`）与关键检查：

- `utf8_frames`：超长 UTF-8 行跨 2 帧重组逐字节一致；`(line, byte_offset)` 续读跨帧、跨行边界精确；钉在当前末尾的 `(line, byte_offset)` 游标在后续 append 后仍从原位精确续读（活位置续读，探针 check 记录为 "fixed read end follows new appends"）。探针 `ReadCursor` 只含 `(line, byte_offset)`、没有 `end_line`，**固定 `end_line` 分页不随后续输出扩展未在本探针验证**，留待生产切片计划 S4 的验收测试补验。
- `rotations`：4096 B 阈值下实测 **5 段**（README 记 5–6 段）；从第 1 行跨所有轮转读回逐字节一致；`(line, byte_offset)` 续读跨 ≥3 次轮转。
- `cursor_cleanup`：已持久化 revision 12 固定为 `(line 12, byte 1500, seg 6)`；轮转后仍可精确读；cleanup 事务性删除 6 个 sealed segment 并让 revision 12 失效；过期游标显式返回最早可用位置 `(line 13, byte 0)`。
- `tail_revision_multiframe`：多帧行的 revision 记录行尾精确字节 `90000`（不是某个 chunk 起点）。
- `commit_before_query`：并发读取在 parked 于 fsync 与 commit 之间时只见已提交行（`file 556 > committed 392`）；`frame_after_sync` 与 `txn_before_commit` 恢复前都读不到未提交帧。
- `exit_event`：退出状态 + 每会话事件 seq 同一事务提交，trace 证明提交后才发布。
- `ack_prune`：ack 单调、越界 ack 拒绝、prune 只删已确认连续前缀、未 prune 事件与 ack 状态跨重启保留、清空后事件序号从 6 继续。
- `drain_append` / `drain_sync` / `drain_commit`：替身 producer 跨 append／sync／commit 注入失败持续生产，8 帧有界内存溢出后 5 次丢弃 → 精确 `gap [9,14)` + degraded + refuse-new-start；12 个弃用段整文件隔离、绝不采纳；gap 前的行可读、gap 显式过期、gap 后的行可读。
- `drain_overflow`：多帧行在峰值 8 帧 / 400000 B 真实 payload 处触发 `gap [3,7)`；最大记录 600000 B > 524288 B 上界，在空队列上即被拒（`gap [1,4)` + degraded + refuse-new-start）。
- `recovery_crash`：quarantine 工件先持久化，之后才截断 live 文件；quarantine rename 期间断电不复活数据、产生显式 gap、重跑幂等。
- `clear_logs`：运行中清空日志不杀进程，substitute writer 发现 segment 消失后继续编号；process marker、`line_watermark`、terminal 记录与未 prune 事件保留，删除范围记显式 gap。
- `destructive`：删除 DB+WAL+SHM → 破坏性重建、新 epoch、旧文件隔离、旧 epoch 游标带 `(line 1, byte 0)` 拒绝；编号重启是已记录的破坏性代价。
- `migration_rollback`：失败迁移中止 `Store::open`，事务回滚无半套 DDL、版本不落账；同一 DB 随后 `0001 → 0002 → 0003` 干净升级。
- `damage_missing` / `damage_truncated` / `damage_corrupt`：索引指向缺失／截短／损坏数据 → 显式 `log_gap [1,4)` / `[2,4)` / `[2,4)` + degraded，watermark 保留，编号从 4 继续、不复用。
- `damage_bad_tail`：坏尾部截断回 committed 边界、不采纳、不产生 gap，编号从 4 继续。

selfcheck 记录：2 个 segment（发生轮转）、`line_watermark=8`、长行 2 帧、2 个事件、未提交尾行 8 被丢弃且编号可重新分配、重启后 epoch 保持不变。

### fsync 成本（定性）

提交路径的同步成本是**每批次一次 `sync_data`**（外加首次建段的 header + 父目录 fsync），与批次数线性相关，不是每字节成本；因此批处理阈值直接摊薄固定同步开销。探针**没有单独微基准单次 fsync 延迟**，只有整条 gate 的墙钟时间（6.882 s / 6.849 s，含 25 个 crash writer 子进程与 68 次 recover 子进程）以及命令级 cold/warm 时间（cold 27.7 s、warm 7.6 s）。因此**4 MiB 段 / 64 KiB 或 50 ms 批处理是设计默认值，不是吞吐基准结论**。

### PASS 判据

`just terminal-probe-storage` 自动遍历 crash matrix，每个恢复结果被归类为完整（clean）、明确缺口（gap/degraded）或明确 Interrupted/orphan，不允许伪造成功；SQLite 事务内不含文件扫描或等待 client；恢复幂等；`GATE-C-OK` 之后无 writer/recover 进程残留、/tmp 无成功 workdir 残留。上述全部满足。

## 7. 明确非保证与限制

- **探针代码是 throwaway，生产实现未开始**：没有生成的生产 `.proto`、`qingluan-terminal` 正式实现、daemon gRPC 接入、正式 TS client 包、pi 工具、CLI、UI、远端能力或权限框架。
- **进程回收**：不保证回收主动迁出 cgroup 的进程，也不保证停止 SSH 远端任务；`detached` 场景证明的是“仍在 cgroup 内”的 `setsid` 后代可回收。
- **cgroup 前置**：探针运行在已被 delegate 的 systemd scope 内（`user@1000.service/app.slice/...`）。**生产 user service 单元的 `Delegate=yes` 未被真实单元文件验证**；缺失 delegation 或 `cgroup.kill` 按设计硬失败，不降级。
- **Node 版本**：只实测 24.19.0 与 22.23.2；22.12.0 是 `engines` 最低候选，未执行。
- **Gate A 证据形态**：逐场景断言来自已提交探针源码与 lockfile；本轮材料中**没有 Gate A 运行的原始 stdout 日志**（Gate B/C 均有），因此 Gate A 的“已运行通过”依赖提交的实现与 Gate 确认记录。detached 探针上限 `MAX_STATUS_DETAILS_BYTES = 4096` 是探针局部限制，不是协议上限。
- **Gate B 的停止耗时**是本机、本内核下的 fixture 测量（600 ms 宽限确实会走满：`term_phase_ms` 600–621 ms；`cgroup.kill` 阶段 613–633 ms，远低于 3 s 上界）；16 核为操作者注记，非保留日志采集；未在负载或不同内核上复测。写入期限场景用 300 ms 而非生产的 10 s。
- **Gate C 的边界未被走到**：生产 4 MiB 段、64 KiB / 50 ms 批量、每个 terminal 64 条 segment 元数据上限、1024 条合并 gap 上限都是设计确认值；探针用 256 KiB 段与 4096 B 场景阈值，且探针内 segment 数最多 12，未触及 64／1024 上限。断电由**物理截断到 fsynced checkpoint** 模拟，不等价于真实硬件断电（无写缓存重排、无介质损坏）。
- **CRC32 只针对意外损坏**；不防篡改，不是加密完整性保证。
- **无跨介质原子性**：文件与 SQLite 不构成同一事务，本报告只证明“按 append → sync_data → 事务 → 发布”的顺序可恢复，且恢复不伪造连续性；不声称任意顺序都安全。
- **change ID 未在本轮重新推导**：探针变更 `nltxtlot` 与初始设计变更 `lopvvvnu` 按已批准的 Gate 记录登记，本报告与设计修订属于当前变更 `tlsmrkts`；本轮材料中没有 jj 元数据可供再核对。

## 8. 矛盾与未决证据

本轮没有发现相互矛盾的测量结果；两次 Gate C 运行的归类与检查完全一致（唯一差异是 epoch 值与墙钟时间）。以下是有意保留的分叉或未取证项：

- 未决定是否把探针采用的自管 `AsyncFd` 写路径下沉进 `pty-process` 使用方式以外的抽象层（属生产实现设计，不属本次验证）。
- 未验证 `crc32fast` 之外的校验方案收益（未做对比实验）。
- 未验证 4 MiB 段在真实负载下的轮转频率与索引规模。
- 未验证 50 ms 批处理上限在持续高输出下的实际延迟分布。

## 9. 变更标识与证据路径

- 探针变更 ID：**`nltxtlot`**（throwaway 探针代码；已并入线性历史，生产实现不依赖探针代码）。
- 初始设计变更 ID：**`lopvvvnu`**（设计基线、协议草案与验证计划的初始落盘）。
- 本报告与后续设计修订属于当前变更 **`tlsmrkts`**；`lopvvvnu → nltxtlot → tlsmrkts` 构成线性历史。
- Gate B 原始日志：`/tmp/pty-verify-cold.log`（cold 运行，含完整冷构建，`PROBE_OK scenarios=25`）与 `/tmp/pty-verify-warm.log`（warm 运行，`PROBE_OK scenarios=25`；§5 实测表数值取自 warm 日志）。
- Gate C 原始证据：
  - `~/Projects/.workspace/qingluan/terminal-validation/target/terminal-probes/storage-evidence/20260920T123205Z-2678639/workdir/{transcript.txt,gate-c/summary.json,selfcheck/summary.json}`（cold）
  - `~/Projects/.workspace/qingluan/terminal-validation/target/terminal-probes/storage-evidence/20260920T123255Z-2684721/workdir/{transcript.txt,gate-c/summary.json,selfcheck/summary.json}`（warm，`storage-evidence/LATEST` 指向此目录）
- 仓库相对证据：`docs/design/terminal-technical-validation-plan.md`、`probes/terminal/README.md`、`probes/terminal/{grpc,pty,storage}/**`、`probes/terminal/third_party/**`、根 `.justfile`。

## 10. 下一步（需单独批准）

1. 只做已被运行证据支持的最小设计修订：PTY 写路径自管、cgroup `Delegate=yes` 硬要求、存储格式与提交／恢复规则、生产默认值标注为“初始值、非基准”。
2. 为 Gate C 生产默认值补一组针对性实验（真实 4 MiB 段、持续高输出下的批量延迟分布、第 65 个 segment 触发 retained-range 推进的行为、gap 合并到 1024 上限的行为）——当前这些值只有设计论证，没有边界实测。
3. 生产实现切片计划与 `Delegate=yes` 的真实 systemd 单元验证；在获批准前不开始生产代码。
