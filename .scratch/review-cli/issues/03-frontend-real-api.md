# 03 · 前端接真实 API

Status: resolved
Blocked by: 02

- 删 `stub-diff.ts`，改为拉 daemon：列表页元信息 → 点开文件按需拉
  old/new 全文（两级加载，ADR-0003）。
- 评论从纯 pinia 内存改为走 daemon 端点（store 变薄封装）。
- URL `/review/<id>` 从路由参数取 session id。

参考：`docs/adr/0003-review-cli-architecture.md`、issue 02。

## Comments

- 2026-09-26 agent：完成。删 `stub-diff.ts`；新增
  `src/lib/review-api.ts`（同源相对 URL 的类型化客户端）、vite dev
  代理 `/reviews`+`/health` → 127.0.0.1:47129（生产由 daemon 同源服务）。
- 路由 `/review/:id` 取 session id；store 变 daemon 薄封装
  （open/add/update/remove → GET/POST/PATCH/DELETE），新增
  `reviewComments.spec.ts`（mock fetch，6 用例）。
- 两级加载落地：文件默认折叠，首次展开才拉 old/new 全文
  （CodeReviewDiff 懒加载 + binary 占位）。
- 侧栏「代码审查」导航入口移除：入口是 CLI 产出的 URL，无 id 的
  空入口没有意义。
- 验收：apps/web `pnpm quality` 15 用例绿；`just quality` 全绿；
  daemon E2E：新 bundle 哈希（build.rs 重嵌入生效）、/review/<id>
  SPA、266 文件 API。
