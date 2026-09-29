# 07 · 长度轴：函数 nloc 阈值与文件 nloc 榜

Status: resolved
Blocked by: 05

调研已定案（`docs/research/code-length-metrics.md` §5）：长度作为**独立轴**纳入，
不折进 cc/cognitive、不做复合分。本 issue 不改计数内核，只加呈现层与阈值。

验收点（全部满足才算完成）：

1. 引擎：`analyze_source` 返回文件级 `nloc`（复用 kernel 的注释/空行规则，span 为
   整棵树）；`FileComplexity` 带 `nloc`；无函数的 `<module>` 兜底文件同样有文件 nloc。
2. 配置：`[complexity]` 增 `nloc_threshold`（默认 100；Clippy `too_many_lines` 同口径
   默认）与 `file_nloc_threshold`（默认 1000；Sonar S104 多数语言默认）。两者都是
   **约定**而非实证发现，帮助文本与注释须照此标注。
3. `--threshold` = 三规则**并集**（cc ∪ cognitive ∪ 函数 nloc）；该模式下表格加
   flags 列（`cog`/`cc`/`len` 逗号分隔）标出命中轴。
4. `--files`：文件表 `nloc / functions / worstCog / worstCc / path`，按 nloc desc →
   path asc，行数同 top（`--all` 时全量）；并打印 file nloc 分布行。文件轴**只排名
   不拦截**：不进 `--threshold` 并集。
5. `--density`：可选展示列 cc/nloc（cyclomatic density，Gill & Kemerer 1991）。
   派生值：只展示、不作为判定、不进并集、不进 JSON，默认关。
6. JSON：`schemaVersion` 升到 2；`distribution` 增 `nloc`；新增顶层 `files` 数组
   （`path/language/nloc/functions/worstCognitive/worstCc`，nloc desc → path asc，全量）。
   函数分布行从两行变三行（cognitive / cc / nloc）。
7. `EXCLUDED_DIRS` 补 `third-party` 与 `open_source`（vendored 目录；MG-Nav
   `third-party/` 与 Hi3863 `prj/open_source/mbedtls` 实证）。
8. 行为变化写回 spec 落地补记：`--threshold` 命并集后命中数会变（本地实测 561 → 613），
   JSON v1 → v2。

明确不做：复合分与派生比率判定、`--sort flags`（后续候选）、per-language 阈值表、
疑似生成物启发式（`APIClient.ts` 类漏网交给 `complexity.exclude`，需要时另开 issue）。

## Comments

- 2026-09-30 agent：完成，验收点 1–8 全部落地。
- 引擎：`analyze_source` 改返回 `SourceAnalysis { functions, nloc }`；文件级 nloc
  就是 `kernel::nloc` 以树根为 span 的调用（同一条注释/空行规则，零复制）；
  `FileComplexity` 增 `nloc` + `worst_cognitive()`/`worst_cc()`。golden/单测钉住
  「注释与空行不计」「`<module>` 兜底也有文件 nloc」「空 buffer = 0」。
- 并集与 flags：`threshold_tags()` 单一来源（`cog`/`cc`/`len` 固定序），
  `--threshold` 的 retain 与 flags 列共用它；单测覆盖「长而平」（len 独有）与
  「短而绕」（cc 独有）两类命中——正是长度轴存在的理由。
- `--files`：分布行 + 榜（nloc desc → path asc），`--all` 全量；JSON 侧
  `schemaVersion: 2`、`distribution.nloc`、顶层 `files`（含 language/functions/
  worstCognitive/worstCc），单测断言 v2 全部新字段与排序。
- `--density`：`cc/nloc` 两位小数展示列，nloc=0 记 0.00；不进判定、不进 JSON。
- `EXCLUDED_DIRS` 补 `third-party` 与 `open_source`（集成测试
  `vendored_dir_aliases_are_never_walked`）。
- 测试：complexity 20 + golden 14 + scan 6 + cli 20 + config 7，全绿；
  `cargo check --workspace --all-targets` 过；改动 crate clippy `-D warnings` 干净。
- 实测本仓（164 文件 / 1833 函数）：`>15` 25、`>10` 54、函数 `nloc>100` 27；
  两规则并集 56，**三规则并集 66（长度独有 10）**；文件 `nloc>1000` 8 个。最长文件
  `terminal.rs`（2085 nloc / 86 函数 / worstCog 31）是「小函数堆一起」型；
  `terminal/tests/runtime.rs`（1523 nloc / worstCog 9）是函数指标完全看不见的那类。
  `qingluan-cli/src/main.rs` 自身以 1651 nloc 居榜眼——工具先照见了自己。
- spec 同步：输出契约 JSON v2、命令面、阈值与配置面行、末节改为「issue 07 已实现」。
