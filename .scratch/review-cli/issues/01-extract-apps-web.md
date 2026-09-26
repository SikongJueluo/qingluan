# 01 · 抽取 apps/web

Status: resolved

把 `apps/desktop/frontend` 抽为根级 `apps/web`，与 `apps/desktop` 平级；
Tauri（`apps/desktop`）改为消费 `apps/web` 的构建产物，review UI 保持
`pnpm quality` 全绿、`just tauri-dev` 可用。

注意：这是 monorepo 结构调整，先看 `devenv.nix` / flake / pnpm workspace
里所有引用该路径的地方。

参考：`docs/adr/0003-review-cli-architecture.md`。

## Comments

- 2026-09-26 agent：完成。`apps/desktop/frontend` → `apps/web`；更新
  `.justfile`、`.github/workflows/quality.yml`、根 `.gitignore`、两处
  nix 包（`frontend.nix` src+hash、`qingluan-desktop.nix` substitute
  模式）、`tauri.conf.json`（hooks 改 `cd ../web`，`frontendDist`
  `../../web/dist`）与两篇架构文档。
- tauri CLI 从 cwd 向上找 src-tauri，`apps/web` 下无祖先可命中，故
  `just tauri-dev` 改为在 `apps/desktop` 调用；旧 recipe 的 `bun tauri`
  本就不可用（无 script/binary），改为
  `bunx --package @tauri-apps/cli tauri dev`（单独提交）。
- 验收：apps/web `pnpm quality` 绿；`just quality` 绿（注意：本机 cgroup
  测试需在 `systemd-run --user --scope` 下跑，登录 scope 未委派）；
  `just tauri-dev` 冒烟通过（vite 从 apps/web 起 + cargo 编译开始）；
  `nix build .#frontend` 在干净树通过（fetchPnpmDeps hash 因 CodeMirror
  提交漏更新而早前已失效，本次一并修正）。
- 附带修复（独立提交）：浮动 stable clippy 的新 lint（terminal/storage）；
  cgroup 委派探测把 EACCES 归为 DelegationUnavailable。
