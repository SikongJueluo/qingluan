# 01 · 抽取 apps/web

Status: ready-for-agent

把 `apps/desktop/frontend` 抽为根级 `apps/web`，与 `apps/desktop` 平级；
Tauri（`apps/desktop`）改为消费 `apps/web` 的构建产物，review UI 保持
`pnpm quality` 全绿、`just tauri-dev` 可用。

注意：这是 monorepo 结构调整，先看 `devenv.nix` / flake / pnpm workspace
里所有引用该路径的地方。

参考：`docs/adr/0003-review-cli-architecture.md`。
