# 04 · daemon 托管前端产物

Status: resolved
Blocked by: 01

- `apps/web` 构建产物用 `include_dir!` 编进 daemon 二进制，axum 挂
  静态路由；`/review/<id>` 落到 SPA 入口。
- Nix/devenv 侧保证 daemon 构建前先 build 前端（声明式，参考
  `nix/` 与 flake 中 qingluan-desktop 的打包方式）。

参考：`docs/adr/0003-review-cli-architecture.md`。

## Comments

- 2026-09-26 agent：完成。`web.rs`：`include_dir!` 嵌入 apps/web/dist，
  axum fallback 服务静态文件；未命中路径回落 `index.html`（SPA 硬刷新
  存活），API 路由先于 fallback。`build.rs` 声明
  `rerun-if-changed=apps/web/dist`（include_dir 不会因目录内容变化重编译）。
- nix：qingluan.nix postPatch 注入 frontend 产物到 apps/web/dist；
  flake 与 system/home 两 module 传 frontend 参数。
- E2E：/、/assets/*（正确 mime）、/review/<id>、/health 优先级全过。
