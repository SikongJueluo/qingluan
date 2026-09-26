# Review CLI — spec

目标：`qingluan review <dir>` 薄入口 + daemon serve review Web UI。
设计已定案并经 grill 确认，全部决策见 `docs/adr/0003-review-cli-architecture.md`，
勿在此重复。实现顺序见 `issues/`。

## 关键既有资产

- review UI 已完成：`apps/desktop/frontend/src/views/CodeReviewView.vue` 等，
  数据源是 `stub-diff.ts`（本 effort 替换）
- daemon：`crates/qingluan-daemon/src/main.rs`（axum REST, 127.0.0.1:47129）
- CLI：`crates/qingluan-cli/src/main.rs`（clap + reqwest）
- jj workspace 工具：`qingluan_core::workspace`
