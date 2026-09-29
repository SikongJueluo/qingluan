# 03 · 多语言 grammar 与决策节点映射

Status: resolved
Blocked by: 01

每语言一份函数定义识别 + 决策节点映射；语言模块只做 `Node -> Decision`
分类，算术留在内核。

| 语言 | 扩展名 | 函数边界 | 决策节点要点 |
| --- | --- | --- | --- |
| Rust | rs | `function_item` | `if_expression`/`else_clause`/`match_arm`/`loop`/`while`/`for`/`&&`,`\|\|`/`?` 不计 |
| TypeScript | ts, tsx, mts, cts | `function_declaration`, `method_definition`, `arrow_function`, `function_expression`, `generator_function_*` | `if_statement`(含 else-if 判定)/`switch_case`/`for*`/`while`/`do`/`catch_clause`/`ternary_expression`/`binary_expression` |
| JavaScript | js, jsx, mjs, cjs | 同 TS | 同 TS |
| Python | py, pyi | `function_definition` | `if_statement`/`elif_clause`/`else_clause`/`for_statement`/`while_statement`/`except_clause`/`conditional_expression`/`boolean_operator`/`case_clause` |
| Go | go | `function_declaration`, `method_declaration` | `if_statement`(else-if 判定)/`for_statement`/`expression_case`/`type_case`/`communication_case`/`&&`,`\|\|` |
| Java | java | `method_declaration`, `constructor_declaration` | `if_statement`/`switch_label`/`for_statement`/`enhanced_for_statement`/`while_statement`/`do_statement`/`catch_clause`/`ternary_expression`/`binary_expression` |

- 每语言至少一条 golden test（函数边界 + 一个已知 cc/cognitive 值）。
- TS/JS/Python/Go/Java 的 `else if` 不能既算 `else` 又算 `if`。

## Comments

- 2026-09-30 agent：完成。Rust/TS/TSX/JS/JSX/Python/Go/Java 全部接入
  （`src/langs/`）；TS 与 JS 共用 classify，只是 grammar 不同。
- Go / Java 由两个并行子代理实现，节点种类都用 dump 工具实测过，不是照猜：
  - Go 有独立的 `default_case`；receiver 取最内层 `type_identifier`
    （`(s *Service)` → `Service`）；`func f(a, b int, c string)` 记 3 个参数；
    `else if` 在语法树里直接就是外层 if 的 `alternative`。
  - Java 没有 `else_clause`：plain `else` 就是 `if_statement` 的 `alternative`，
    classify 用 parent+field 判定；`switch_expression` 是唯一的 switch kind；
    `case A, B:` 是单个 `switch_label`（记 1 分）；带 label 的 break/continue 的
    label 是普通 named child，不是 field。
- 踩过的坑（已修，测试钉住）：tree-sitter 的 `else` 判定不能靠 kind 名猜——
  JS/Rust 有 `else_clause`，Java/Go 没有；`_ =>` 的 Rust 通配臂是空
  `match_pattern`，不是 `wildcard_pattern`。
- 退出依据：crate 内 17 条 + `tests/golden.rs` 13 条测试全绿，逐语言覆盖边界、
  else-if 平链、switch/match、逻辑序列、嵌套函数归属、参数计数。
- 未支持：`.vue`/`.svelte`（本仓 `apps/web` 有 111 个 `.vue`）仍计 `unsupported`。
  若要支持，应按「抽出 `<script>` 块 + 按 TS 解析 + 行号偏移」单开一条。
