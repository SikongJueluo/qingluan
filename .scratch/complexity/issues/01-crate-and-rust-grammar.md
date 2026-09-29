# 01 · 新建 qingluan-complexity crate 与 Rust grammar 接入

Status: resolved

- 新建 `crates/qingluan-complexity`，注册进 workspace members；锁定
  tree-sitter core 与各 grammar crate 互相兼容的版本组合（core 0.25+，
  grammar 走 `tree-sitter-language` ABI）。
- 公开入口 `analyze_source(lang, source) -> Vec<FunctionMetrics>`：按扩展名
  选 grammar，`Parser::parse` 全文，产出每个函数的边界与指标。
- Rust 函数边界：`function_item`。只产出**最外层**函数条目；嵌套函数/闭包
  的决策点计入外层（spec「函数边界」）。
- 无函数的文件给文件级兜底条目（`name = <module>`）。
- 行列 1-based，另存 `startByte`。
- 单测：一个含 2 个函数的 Rust 源，校验条数、name、startLine/endLine。
- 语法错误输入不 panic（tree-sitter 错误恢复路径）。

## Comments

- 2026-09-30 agent：完成。`crates/qingluan-complexity` 已进 workspace members，
  锁定的组合（Cargo.lock 已更新）：tree-sitter 0.25.10 + tree-sitter-rust 0.24.2、
  typescript 0.23.2、javascript 0.25.0、python 0.25.0、go 0.25.0、java 0.23.5。
  注意本机裸 `cargo` 是坏掉的 rustup proxy，构建要走 `direnv exec . cargo ...`。
- 公开入口 `analyze_source(Language, &[u8]) -> Vec<FunctionMetrics>` 与
  `analyze_path(&Path)`；行列 1-based，另有 `startByte`/`endByte`。
- 只产出最外层函数；嵌套函数/闭包归入外层并抬高其内部嵌套层级；无函数的文件给
  `<module>` 兜底条目（空文件不出条目）。
- 语法错误输入不 panic：`tests/golden.rs::broken_syntax_still_recovers_and_never_panics`。
- 开发期用过的 `examples/dump.rs`（打印 s-expression）已删除，未留在仓库里。
