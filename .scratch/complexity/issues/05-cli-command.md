# 05 · `qingluan complexity` 子命令

Status: resolved
Blocked by: 04

命令面（v1，不做 gate，接口不给 `--from/--to` 留位以外的东西）：

```bash
qingluan complexity [PATH...]     # 默认：摘要 + top-10
qingluan complexity --all         # 全部函数
qingluan complexity --threshold   # 只列超阈值
qingluan complexity --sort cognitive|cc|nloc
qingluan complexity --top N
qingluan complexity --json        # 全量、不截断
qingluan complexity --quiet       # 只出摘要
```

- 输出契约严格按 `.scratch/complexity/spec.md`「输出契约」：摘要
  （scanned/skipped + 分布 p50/p90/p99/max/超阈值计数）+ top-K 表格。
- 排序确定：`cognitive desc → cc desc → path asc → startLine asc`。
- `--top` 只影响人类表格，不影响 `--json`。
- 退出码：0 正常；路径不存在/无可分析文件时非零 + 人话错误。
- 本地直接调 `qingluan-complexity`，不走 daemon。
- 单测：`--json` 形状（schemaVersion/root/scanned/distribution/functions）；
  跑一次仓库自身 root，断言打印出摘要行且 top-K 行数 ≤ K。

## Comments

- 2026-09-30 agent：完成。`Commands::Complexity`（`[PATHS]... --all --threshold
  --sort cognitive|cc|nloc --top N --json --quiet`），本地直接调
  `qingluan-complexity`，不走 daemon；`--sort` 用 clap `ValueEnum`。
- 输出契约照 spec：一行摘要（files/functions/skipped 三桶）+ 两行分布
  （p50/p90/p99/max/超阈值）+ top-K 表（表头 `cog/cc/nloc/params/nesting/location`，
  位置为 `path:startLine` + 限定名）。
- 排序确定：`metric desc → cc desc → path asc → startLine asc`，单测
  `complexity_order_is_metric_then_cc_then_path_then_line` 钉住。
- `--top` 只影响表格；`--json` 全量、形状与 spec 逐字段一致（单测
  `complexity_json_matches_the_documented_shape` 检查 schemaVersion/root/
  scanned/distribution/functions 及 camelCase 键名）。
- 退出码：路径不存在 → `path_not_found`；无可分析文件 → `no_files`；坏配置 →
  `config_invalid`；都是 stderr JSON + exit 1。
- 实测本仓 `qingluan complexity --top 8`：164 文件 / 1819 函数，cognitive
  p50 0 / p90 4 / p99 18 / max 73，top-1 是
  `crates/qingluan-storage/src/frame.rs:287 scan_frames`（cog 73 / cc 36 / nloc 223）。
  注意 CLI 自身新加的 `cmd_complexity` 也在榜上（nloc 144）——正是这个工具想让人
  看见的东西。
- 分布用 nearest-rank 分位数（整数、可复现），不做插值。
