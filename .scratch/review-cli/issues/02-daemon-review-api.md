# 02 · daemon：review 会话与 diff API

Status: resolved
Blocked by: 01

- `POST /reviews { path, from?, to? }`：在目标目录起子进程
  `jj diff --from <rev> --to <rev>`（默认 `main..@`），解析出变更文件
  元信息（path/status/±行数），返回 session id。会话存内存 map。
  纯 git 目录返回明确错误，提示先进 workspace。
- `GET /reviews/<id>/files`：元信息列表。
- `GET /reviews/<id>/files/<n>?side=old|new`：old/new 全文（用于
  CodeMirror unifiedMergeView）。
- 评论端点：`GET/POST /reviews/<id>/comments`（增删改，内存态）。

数据形状参考前端 `apps/web/src/components/code-review/types.ts`
（`ChangedFile` / `ReviewAnchor` / `ReviewComment`），保持两边一致。

参考：`docs/adr/0003-review-cli-architecture.md`。

## Comments

- 2026-09-26 agent：完成，`crates/qingluan-daemon/src/review.rs`（新增
  lib target 供集成测试链接）。实现要点：
  - diff 一次子进程：`jj -R <dir> diff --git --context 1000000
    --from <rev> --to <rev>`（默认 main..@），无限上下文使 unified
    diff 同时充当 old/new 全文；解析器纯函数 + 单测（含 added/deleted/
    binary/无尾换行）。
  - 会话存 `ReviewStore`（Mutex<HashMap>）；评论 GET/POST/PATCH/DELETE，
  wire 形状对齐前端 types.ts（camelCase）。
  - 非 jj 目录 → 422 `not_a_jj_repo`；revset 错误 → 400
    `jj_diff_failed`；未知会话/评论 → 404。
  - 集成测试（tests/review.rs）用真实 jj 临时仓库，无 jj 时跳过
    （CI 无 jj 仍绿）；测试仓库显式 `signing.behavior=drop` 避开全局
    GPG 签名配置。
  - E2E 冒烟：本仓库 main..@ 258 文件、全文/评论增删改查/错误码全过。
