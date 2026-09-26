# 02 · daemon：review 会话与 diff API

Status: ready-for-agent
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
