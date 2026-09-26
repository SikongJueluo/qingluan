# 04 · daemon 托管前端产物

Status: ready-for-agent
Blocked by: 01

- `apps/web` 构建产物用 `include_dir!` 编进 daemon 二进制，axum 挂
  静态路由；`/review/<id>` 落到 SPA 入口。
- Nix/devenv 侧保证 daemon 构建前先 build 前端（声明式，参考
  `nix/` 与 flake 中 qingluan-desktop 的打包方式）。

参考：`docs/adr/0003-review-cli-architecture.md`。
