# Complexity CLI — spec

目标：`qingluan complexity [PATH...]`，扫描匹配文件、按函数计算复杂度、输出分布摘要 + 最差 K 个。

算法选型、完整计数规则与一手依据见 `docs/research/code-complexity-metrics.md`，勿在此重复。
范围与契约经本次讨论定案（见文末「决策记录」）。

## 已定范围

| 决策 | 结论 |
| --- | --- |
| 模式 | 两种、分阶段：**v1 只做全仓体检**；diff 增量守门（`--from/--to`）第二阶段 |
| CI 闸门 | **不做**：无 `--fail-above`、无 baseline、无跨运行函数标识 |
| 默认输出 | 分布摘要 + top-K（K 默认 10）；`--all` / `--threshold` 出全量；`--json` 全量不截断 |

不做 gate 的直接收益：**跨运行的稳定函数标识可以整个推迟**。那是本功能最贵、语义最容易做错的部分
（同名重载、嵌套同名、匿名函数/闭包无名字、改参数算不算同一个、重命名即「新函数」）。
v1 只在单次运行内用行号区间定位函数，不承诺任何跨运行身份。

## 命令面

```bash
qingluan complexity [PATH...]          # 体检：摘要 + top-K（默认）
qingluan complexity --all              # 全部函数（按 --sort 排序）
qingluan complexity --threshold        # 只列超阈值的函数（cc/cognitive/nloc 并集）
qingluan complexity --sort cognitive   # cc | cognitive | nloc（默认 cognitive）
qingluan complexity --files            # 附「最长文件」榜（file nloc 只排名不拦截）
qingluan complexity --density          # 可选派生列 cc/nloc（只展示，不作判定）
qingluan complexity --json             # 机器可读，全量不截断
qingluan complexity --quiet            # 只出摘要统计，不出列表

# 第二阶段（本期不实现，接口先留位）
qingluan complexity --from main --to @ # diff 增量守门：只报变差的函数
```

**默认排序为什么是 cognitive 而不是 cc**：CC 会把「30 个 case 的 switch」排到 top-1，而那恰好是最好读的
代码（调研文档 §1.5、§2.4）。cc 作为可选项保留。

## 输出契约

人类可读默认输出 = **分布语境 + top-K**。有 p50/p90/max，「87」才知道是离群值还是常态：

```
scanned 412 files, 3187 functions (skipped 38: 21 generated, 9 unsupported, 8 too large)

cognitive  p50 2   p90  8   p99 19   max 61   >15: 96
cc         p50 3   p90 11   p99 24   max 87   >10: 214
nloc       p50 7   p90 29   p99 94   max 1132 >100: 160

worst by cognitive:
  cog  cc  nloc  params  nesting  location
   61  87   412       3        6  crates/qingluan-daemon/src/router.rs:214  handle_request
  ...
```

硬性规则：

- **排序必须确定**：`cognitive desc → cc desc → path asc → startLine asc`。否则 CI/多次运行输出不可 diff。
- **`--top` 只影响人类表格，绝不影响 `--json`**。JSON 要么全量，要么显式带 `truncated` / `shown` / `total`。
- **跳过统计必须打印**（`skipped: N (generated / unsupported / tooLarge)`）。静默漏报比报错更糟。
- **位置用 1-based 行列**：`path:startLine`；JSON 里给 `startLine`/`endLine`/`startCol`，另附 `startByte` 备 UI 用。

JSON 形状（`schemaVersion` 用于后续演进）：

```json
{
  "schemaVersion": 2,
  "root": "/abs/path",
  "scanned": { "files": 412, "functions": 3187,
               "skipped": { "generated": 21, "unsupported": 9, "tooLarge": 8 } },
  "distribution": {
    "cognitive": { "p50": 2, "p90": 8, "p99": 19, "max": 61, "overThreshold": 96 },
    "cc":        { "p50": 3, "p90": 11, "p99": 24, "max": 87, "overThreshold": 214 },
    "nloc":      { "p50": 7, "p90": 29, "p99": 94, "max": 1132, "overThreshold": 160 }
  },
  "files": [
    { "path": "crates/x/src/big.rs", "language": "rust", "nloc": 2085,
      "functions": 86, "worstCognitive": 31, "worstCc": 24 }
  ],
  "functions": [
    { "path": "crates/qingluan-daemon/src/router.rs", "startLine": 214, "endLine": 318,
      "name": "handle_request", "qualifiedName": "Router::handle_request",
      "cc": 87, "cognitive": 61, "nloc": 412, "params": 3, "maxNesting": 6 }
  ]
}
```

## 指标与计数口径

输出指标向量，**不发明复合分数**（如 `cc × nloc`）——那是重蹈 Halstead/MI 覆辙（调研文档 §3）。
哪个更糟（500 行 CC=20 vs 10 行 CC=20）由用户看 `nloc` 自己判断。

| 字段 | 口径 |
| --- | --- |
| `cc` | CC1（每个原子条件 +1，短路布尔 +1）；switch 按 case 计（classic） |
| `cognitive` | 白皮书 Appendix B；不跟进 JS 方言（逻辑或 / 空值合并免计）偏差；递归 +1 不做 |
| `nloc` / `params` / `maxNesting` | 遍历时顺带统计 |
| 函数边界 | 嵌套函数/闭包**计入外层**（与 Sonar/ESLint 一致，写死）；无函数的脚本给文件级兜底 |

阈值默认：`cc > 10`、`cognitive > 15`（Sonar S1541 / S3776）、函数 `nloc > 100`
（Clippy `too_many_lines` 同口径默认）、文件 `nloc > 1000`（Sonar S104 多数语言默认；
文件轴只排名不拦截），均可配置。长度轴的依据与「为什么不做复合分」见
`docs/research/code-length-metrics.md`。

## 扫描与排除

必须默认排除，否则 minified JS（一行上千决策点）与生成代码会占据整个 top-K，第一次运行就失去信任：

- 尊重 `.gitignore` / `.ignore`（`ignore` crate；若 jj 有额外 ignore 文件名，用 `add_custom_ignore_filename` 补）
- 默认排除：`target/`、`node_modules/`、vendor 目录、常见生成物（`*.min.js`、`*_pb2.py`、`.pb.go`、lockfile 等）
- 体积上限：单文件超过阈值（如 1 MiB）跳过并计入 `tooLarge`
- 只分析有 grammar 的语言；无 grammar 的计入 `unsupported`（不静默丢）
- 配置面（`qingluan.toml`，经 qingluan-config 四层合并）：`[complexity] top / cc_threshold / cognitive_threshold / nloc_threshold / file_nloc_threshold / exclude / include`

## 架构

新 crate **`qingluan-complexity`** 承载引擎；**CLI 本地直接调用，不走 daemon**——
纯函数、无会话态、无浏览器会话，走 daemon round-trip 只是白加延迟和故障点
（`review` 走 daemon 是因为要建会话、算 diff、存评论）。

结构：tree-sitter 全文解析（按扩展名选 grammar）→ 每语言一份函数定义 query 拿到函数边界
→ 语言无关计数器内核遍历子树出指标（详见调研文档 §4.2 的四步架构）。
daemon 后续复用同一 crate 给 review UI 打复杂度标（`GET /reviews/<id>/files` 已提供全文）。

并行为 `rayon` 按文件并行即可；**v1 不做缓存**（全仓扫描秒级，缓存是过早优化）。

## 关键既有资产

- CLI 子命令：`crates/qingluan-cli/src/main.rs`（clap `enum Commands`）
- 配置：`qingluan-config` 已是 CLI 直接依赖，四层合并可本地用上
- 工作区**尚无** `ignore` / `walkdir` / `globset` / `rayon`，需新增依赖（或复用 `ignore` 自带的并行遍历）
- 调研文档 §5.4 的手算示例可直接做 golden test（CC=7、cognitive=15、白皮书 `sumOfPrimes`=7）

## 实现顺序（已落 `issues/`，01–05 已完成）

1. `01-crate-and-rust-grammar` — 新建 crate + tree-sitter 接入 + 单语言（Rust）跑通函数边界
2. `02-counter-kernel` — 语言无关计数器 + golden test（含白皮书逻辑序列边界：`a&&b&&c`=1、`a||b&&c||d`=3）
3. `03-more-grammars` — TS/JS、Python、Go、Java 的 query 与决策节点映射
4. `04-file-walk` — 扫描、排除、超限跳过、跳过统计
5. `05-cli-command` — clap 子命令 + 摘要/top-K/`--json`/`--sort`
6. `06-review-integration` — daemon 复用（第二阶段，与本 spec 的 v1 解耦）

## 决策记录

| 决策 | 结论 | 理由 |
| --- | --- | --- |
| 首要场景 | 体检 + 增量守门，分阶段 | 全仓 top-K 每次运行结果相同，对「这次改动有没有变糟」零信息量；但体检更简单、立刻有用 |
| v1 是否 gate | 不做 | 推迟跨运行函数标识（本功能最贵的部分），v1 只需单次运行内的行号区间匹配 |
| 默认输出 | 摘要 + top-K | 纯阈值在遗留仓库不可读、纯 top-K 在干净仓库无参照；分布语境让数字可判断 |

## 落地补记（2026-09-30，01–05 已实现）

实现细节与实测证据在 `issues/01`–`issues/05` 的 Comments。三处 spec 没写到、
但实现时必须定的事：

1. **逻辑运算符序列的确切算法**：以 sonar-java `CognitiveComplexityVisitor` 为准
   （flatten 整棵 component 后比相邻 operator；括号透明、`!`/调用/三元是边界），
   与白皮书全部公开例子一致。SonarJS 2024-10 起 `||`/`??` 免计的偏差**不跟进**；
   `??` 按普通短路运算符计。推导、源码引用与 7 个表达式对照表见
   `research/logical-sequences.md`。
2. **`else` 的层级**：按 sonar-java，`else` 只 +1，块内层级由外层 `if` 一次性抬升。
   kernel 因此在遍历 `if` 子树时就降层，代价是 `if` 条件里的三元比 sonar-java 多
   算一层嵌套（已在 `src/kernel.rs` 顶部注明）。
3. **`<script>` 型文件仍算 unsupported**：本仓 `apps/web` 有 111 个 `.vue`，会全部
   计入 `unsupported`（可见而非静默）。要覆盖得做「抽 script 块 + 行号偏移」，
   不在本期范围。

## 长度轴（2026-09-30 调研，issue 07 已实现）

调研：`docs/research/code-length-metrics.md`。结论：**作为独立轴纳入、不折进
cc/cognitive**；阈值 `nloc > 100`（函数，Clippy `too_many_lines` 同口径默认）、
`nloc > 1000`（文件，Sonar S104 多数语言默认），两者都是**约定**而非实证发现。
已实现（细节见 `issues/07` 的 Comments）：

- `--threshold` = cc ∪ cognitive ∪ 函数 nloc **三规则并集**，`flags` 列标出命中轴
  （`cog`/`cc`/`len`），只在 `--threshold` 模式显示；
- `--files` 文件榜（`nloc / funcs / worstCog / worstCc / path`，nloc desc → path asc，
  行数同 `top`，`--all` 全量）+ `file nloc` 分布行；文件轴**只排名不拦截**；
- `--density` 可选派生列 cc/nloc（cyclomatic density，Gill & Kemerer 1991），
  只展示、不进判定、不进 JSON，默认关；
- JSON `schemaVersion` 升到 2：`distribution` 增 `nloc`，新增顶层 `files` 数组
  （全量，nloc desc → path asc）。

可见行为变化：默认 `--threshold` 的命中数从两规则变三规则并集（本仓实测 66）。
本 spec 其余部分（v1 的口径与实现顺序）不变。

## 耦合/依赖轴（2026-09-30，issue 08 已实现）

调研：`docs/research/coupling-as-complexity.md`。结论：fan-in 不是缺陷（高 fan-in
是稳定性定义，且是下界）；唯一的「违规」判据是**依赖环**（ADP + 工具共识）；
唯一有实测支撑的组合是 `fan-in × churn`。这是**仓库级报告**，不是函数表的加列：

```bash
qingluan deps [PATH...]             # 摘要 + 环列表 + fan-in 榜
qingluan deps --min-fanin 3 --churn # 加 fan-in × churn 热点表（读 VCS）
qingluan deps --json                # schemaVersion 1，全量；有环仍 exit 1
qingluan deps --quiet               # 只出摘要与环
```

- **退出码**：有环 → 1（工具共识：环可以 fail build）；fan-in/fan-out/I
  只展示永不拦截。
- **三分类解析语义**：每 specifier 记 `resolved`（仓库内边）/ `external`
  （仓库外）/ `unresolved`（应有目标但失败）；只有 resolved 边进图，自环丢弃，
  **不可解析的边绝不造环**。每语言输出 accounted 率。
- **语言边界**（照调研 §6）：Rust 全量模块树（crate 根发现 + `#[path]` 规则
  + 跨 crate 名；本仓 100% accounted）；Java 类型索引 + static 取容器、
  通配不造边；Python 相对上行 + from-import 先探子模块、绝对 miss=external、
  相对 miss=unresolved；Go 按 go.mod 前缀（包=目录归因）；TS/JS 只解析相对
  specifier（`@/`、`#` 记 unresolved）。
- **churn**：CLI 层读取（git 优先、jj 兜底），引擎保持纯函数（注入
  `ChurnEntry`）；rate = commits / max(0.5月, 距首见月数)；热点 = fan_in ≥
  min_fanin 且 rate ≥ 全体 p90，按 fan_in × rate 排序。
- **JSON v1**：`root / scanned{files,languages[]} / cycles[] / files[] /
  hotspots[]`（后者仅 `--churn`）；files 全量、fanIn desc → path asc。
- 明确不做：fan-in 阈值/闸门、Zone of Pain、分层违规（另开 issue）、
  tsconfig `paths` 别名解析（phase 2 候选）。
