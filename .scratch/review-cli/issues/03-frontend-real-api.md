# 03 · 前端接真实 API

Status: ready-for-agent
Blocked by: 02

- 删 `stub-diff.ts`，改为拉 daemon：列表页元信息 → 点开文件按需拉
  old/new 全文（两级加载，ADR-0003）。
- 评论从纯 pinia 内存改为走 daemon 端点（store 变薄封装）。
- URL `/review/<id>` 从路由参数取 session id。

参考：`docs/adr/0003-review-cli-architecture.md`、issue 02。
