# Terminal technology probes

Throwaway validation code for the design in
[`docs/design/terminal-technical-validation-plan.md`](../../../docs/design/terminal-technical-validation-plan.md).
It is not production terminal code and does not freeze the v1 wire protocol.

## Confirmed Gate 0 versions

| Dependency | Version |
| --- | --- |
| tonic / tonic-prost-build | 0.14.6 |
| prost | 0.14.4 |
| pty-process | 0.5.3 |
| nix | 0.31.3 |
| sqlx | 0.9.0 |
| vte | 0.15.0 |
| @grpc/grpc-js | 1.14.5 |
| ts-proto | 2.12.4 |
| TypeScript | 6.0.3 |
| protoc (pinned nixpkgs) | 35.1 |

Node.js minimum candidate: 22.12.0. The probe passed on the pinned dev-shell
Node 24.19.0 and, with the same generated output and lockfile, pinned-nixpkgs
Node 22.23.2. The exact 22.12.0 patch release was not separately executed.

## Vendored external protos

| File | Source | Tag / revision | License record |
| --- | --- | --- | --- |
| `third_party/google/rpc/status.proto` | `googleapis/googleapis` | `9f99764bb7841a50f0e46c41fd958a296b108e60` | Apache-2.0 (`third_party/LICENSE.googleapis`) |
| `third_party/google/protobuf/any.proto` | `protocolbuffers/protobuf` | `v35.1` | BSD-3-Clause (`third_party/LICENSE.protobuf`) |

Generation resolves `google/protobuf/any.proto` exclusively from the vendored
copy under `third_party/` (matching the pinned protoc 35.1); it never falls
back to the protoc installation's include directory, and no network access
happens at build time.

## Probe-local limits

- `MAX_STATUS_DETAILS_BYTES = 4096` in the TS client: a
  `grpc-status-details-bin` value above this size is rejected before any
decoding work. The server's `OVERSIZED_STATUS` fixture is ~6 KiB, above the
probe limit but still deliverable by the gRPC transport.

## Coverage

- Unix socket path safety: regular file rejected and preserved, active socket
  rejected without unlinking (first server keeps serving), stale socket
  reclaimed, duplicate run after clean shutdown, graceful-shutdown socket
  removal.
- Same-v1 tolerance: unknown capability advertised by the server, unknown
  request field (server side), unknown response fields decoded by the older
  TS decoder (varint and length-delimited), unknown enum value roundtrip.
- Streaming: cancel observed by the server, resume from last applied
  sequence, client deadline.
- Richer errors (`google.rpc.Status` + `Any<ProbeErrorDetail>`): known-zero
  and known-large written counts; valid detail coexisting with an unknown
  Any is accepted; degraded cases (missing payload, missing written_bytes,
  unknown reason, duplicate known details, malformed detail, malformed
  status, outer/inner code mismatch, no details at all, over-limit trailer).

## Run

From the repository dev shell:

```console
just terminal-probe-grpc
just terminal-probe-pty
just terminal-probe-storage
just terminal-probe-all   # grpc + pty + storage
```

Generated Rust and TypeScript files live under Cargo `OUT_DIR` and
`target/terminal-probes/`; they are not committed.

## Probe B：PTY 生命周期与 Stop 屏障（probes/terminal/pty）

Throwaway 探针（探针模型 + fixture 二进制），验证
`agent-terminal.md` 的 PTY 生命周期规则；不是生产模块。入口
`just terminal-probe-pty`：cargo build + 6 个单测 + crash/recovery 阶段 +
主阶段（25 个场景）。cold/warm 均通过；成功后无残留进程、cgroup 或临时目录，
失败保留 workdir（`events.jsonl`、`registry.json`、`spawned.log`、
`summary.json`）。

Gate B 已确认的候选：

- 采用 `pty-process` 0.5.3 负责 open/spawn/read/resize；为保证 Stop 与租约代际
  的提交顺序，写端使用 master fd duplicate + 自管 `AsyncFd` 非阻塞写入。
- 每 terminal 使用独立 cgroup v2；生产 user service 必须配置
  `Delegate=yes`，缺失 delegation 或 `cgroup.kill` 时硬失败，不降级到
  `/proc` session 快照。
- 温和阶段以 pidfd 向 cgroup 当前成员发送 `SIGTERM`，宽限 600 ms；之后
  `cgroup.kill`，最长等待 3 s 至 `cgroup.events populated=0`。普通 fork、
  new pgrp 与 `setsid` 后代均被覆盖；主动迁出 cgroup 和远端进程不保证。
- 输入块 4 KiB；单条 Send 最大 256 KiB；有界队列 2 条，加 1 条 in-flight
  后 payload backing 上界 768 KiB。accepted payload 会规范化为 exact-length
  `Box<[u8]>`，不保留 caller `Vec` 的 spare capacity。默认写入期限 10 s。
- 输出关闭最多等待 1 s，超时提交唯一 `OutputClosed(Forced)`；Stop 在内部
  detached 执行，首个调用者取消不会撤销，后续调用共享同一成功或失败结果。
- 配额只在 cgroup 已空、root 已回收、reader/writer/monitor 已 join 且 cgroup
  已删除后进入 Released；任一清理失败保持 Cleaning。

关键实测（NixOS Linux，cold/warm）：

- `pid == sid == pgid`，controlling tty 与 shell 前后台 job 行为符合预期；
  resize、原始字节、环境三态和无换行尾部均通过。
- 不读 stdin 时约 12–16 KiB 后阻塞；Stop/代际切换中止延迟实测 0 ms，
  精确报告已写入字节。256 KiB 边界接受，256 KiB + 1 类型化拒绝。
- generation race 记录 100 次 SwitchCommit，并在同一 coordinator 序列扫描
  WriteCommit；`log_stale_writes=0`，且最终代在最后一次切换后成功提交。
- TERM-immune late-fork fixture 在新 pgrp 中持续派生子进程；温和阶段后由
  `cgroup.kill` 固定点回收。主动 `setsid` 仍留在 terminal cgroup，也被回收。
- Linux 最后一个 slave 关闭后 master read 返回 EIO（errno 5，而非 EOF）；
  映射为正常 OutputClosed。root exit 与 output close 独立且各提交一次。
- registry recovery 只标记 Interrupted；注入 signal backend 的调用数为 0，
  不向旧 PID 发信号、不伪造退出码。
- Stop 并发、成功/失败共享结果、leader cancellation、monitor fault、五个启动
  故障点、reap error/timeout 与 cleanup failure 均有有界场景；最终
  cgroup/process 残留为 0。

## 探针 C：混合存储与恢复（probes/terminal/storage，Gate C）

Throwaway 探针，验证 `terminal-technical-validation-plan.md` §6 的提交顺序
假设（append frame → `sync_data` → 短 SQLite 事务 → 发布）。入口
`just terminal-probe-storage`（build + 35 个单测 + 单进程 selfcheck +
`gate-c` harness）；workdir 内 `summary.json` 记录逐点/逐场景结果；成功后
整个 workdir 原样移出 /tmp，保留在
`target/terminal-probes/storage-evidence/<run-id>/workdir`（git-ignored；
含逐点 sqlite DB、segment 文件、quarantine 工件、trace、两份 summary.json
与 transcript），`storage-evidence/LATEST` 指向最新 run，/tmp 无成功残留；
失败保留 /tmp workdir 供调试。`gate-c` 阶段自带 `timeout 100` 内部上界；
整条命令按外部 `timeout 120 just terminal-probe-storage` 验证。

实测（本机 NixOS Linux，cargo 1.97.0，16 核）：cold（全新
`target/terminal-probes/storage-rust`）27.7 s、warm 7.6 s，均通过
`GATE-C-OK points=20 scenarios=19 power_loss_variants=3 crashed_children=25
recover_children=68`；结束后无 storage writer/recover 进程残留、/tmp 无成功
workdir 残留（run.sh 内置 pgrep 检查 + 失败保留）；每次成功 run 保留完整
证据约 6.5 MiB（gate-c matrix 2.0 MiB、scenarios 4.3 MiB、selfcheck
364 KiB，含逐点 terminal.db、seg-*.log、quarantine 工件与 trace.log），
位于 `storage-evidence/<run-id>/workdir`。

Gate C crash matrix（writer 子进程在精确边界 `libc::_exit(70)`，recover
一律新进程、连跑两次验证幂等）：

- 段创建：seg_before / seg_header_written / seg_header_synced /
  seg_before_db_row（文件+头+目录 fsync 持久但无 DB 行 → 整文件 discover
  + quarantine，绝不采纳）。目录 fsync 在首次段创建后强制执行
  （trace `seg_dir_fsynced`）。
- 帧写入：frame_before_write / frame_mid_write（半帧 torn write）/
  frame_after_write / frame_after_sync。
- SQLite 事务：txn_begin / txn_update / txn_before_commit / txn_after_commit
  （进程死亡 → 未提交事务真实回滚）。
- 发布：publish_before / publish_after；事件：event_insert / event_commit /
  event_publish（stdout 发布记录证明 commit-before-publish 顺序）。
- 断电证据：`_exit` 仅覆盖进程崩溃；另加 3 个 power-loss 变体，在 `_exit`
  后把文件物理截断到 fsynced checkpoint（seg_header_written→目录项未持久
  即移除；frame_mid_write / frame_after_write→截到 committed_bytes），
  恢复结果与崩溃变体状态一致、尾部永不采纳。
- 迁移回滚：0009 迁移中途失败 → 事务回滚，无半套 DDL、版本不落账；
  同一 DB 随后 0001→0002 干净升级。

恢复实测（每个 matrix 点 + 损伤场景均验证）：有界帧长/CRC/seq/line-offset；
坏/半/完整未索引尾部一律截断回 committed 边界并隔离，永不采纳；索引指向
缺失/截短/损坏 → 显式 `log_gap` + degraded，`line_watermark` 绝不回退、
行号绝不复用（继续写入编号 = watermark+1 实测）；连续两次 recover 幂等
（第二次零动作、状态快照逐字节一致）；正常重启/轮转保持 epoch；删除
DB+WAL+SHM 属破坏性重建：换新 epoch、旧文件隔离、旧游标带最早位置拒绝。

场景实测：超长 UTF-8 行跨帧 + (line, byte_offset) 读续传跨帧/跨行字节精确；
4096 字节阈值 ≥3 次轮转（实测 5-6 段）跨段读续传；固定读端（钉在当前
末尾的 cursor）不随后续 append 扩展；cursor 过期返回最早可用 (line, byte) 位置；
tail revision 轮转后固化为历史可读、cleanup 删除持有段后同事务失效并显式
CURSOR_EXPIRED；退出状态 + 每会话事件 seq 同一事务提交、事务后才发布
（trace 顺序验证）；ack 单调、越界 ack 拒绝、prune 只删已确认连续前缀且与
`pruned_through_seq` 同事务；替身 producer 跨 append/sync/commit 注入失败
持续生产，8 帧有界内存溢出 → 精确 gap [9,14) + degraded + refuse-new-start
（新 writer 以专用退出码拒绝），12 个弃用段整文件隔离；运行中清空日志不杀
进程（gate 文件同步）、行号不重置，process marker / terminal 记录 / 未 prune
事件保留，删除范围显式 missing gap。

单测（35 个，含 frame_seq/line_offset u64 溢出回归：溢出返回
CorruptAt，绝不 panic 或回绕）与 foundation 覆盖保持不变；以上均为运行
测量结果，非文档推断。
