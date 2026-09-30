# 08 · 耦合/依赖报告：`qingluan deps`

Status: resolved
Blocked by: 05

调研已定案（`docs/research/coupling-as-complexity.md` §8）：fan-in 不是缺陷指标
（高 fan-in 是稳定性定义），唯一的「违规」判据是**依赖环**（ADP + 工具共识）；
唯一有实测支撑的组合是 `fan-in × churn`。本 issue 落地一条新的仓库级报告轴。

用户已拍板（2026-09-30）：纯静态图 + churn 榜都要；新子命令 `qingluan deps`；
先修 Rust 解析；**发现环即非零退出**（工具共识：环可以 fail build）。

验收点（全部满足才算完成）：

1. 引擎（`qingluan-complexity` 新 deps 模块）：全项目 import 图；每条 specifier 三分类
   `resolved`（仓库内边）/ `external`（仓库外）/ `unresolved`（应有目标但解析失败），
   按语言统计解析率；Tarjan SCC 输出环成员（跨目录环排前，成员字典序）；
   每文件 `fan-in / fan-out / I = Ce/(Ca+Ce)` 向量（fan-in 明标**下界**）。
   churn 是**注入输入**（引擎不 spawn 进程，保持纯函数）。
2. **Rust 解析修复**（当前 2.7% 是 bug）：crate 根发现（`Cargo.toml` 默认布局 +
   手扫 `[lib]`/`[[bin]]`/`[[test]]`/`[[example]]`/`[[bench]]` 显式 `path` + `tests/`
   等目录文件即独立 crate 根）；mod 树（`mod.rs` 与兄弟文件两种拼法、`#[path]` 属性、
   内联 mod 目录栈、`#[cfg]` 门控 mod 照实包含并标注为过近似）；`use` 路径解析
   （`crate::`/`self::`/`super::`/裸段先查本地模块树、查不到判外部 crate）。
   验收数字：本仓 Rust 解析率（resolved+external 占比）≥ 90%，resolved 边要能覆盖
   `crates/qingluan-complexity` 自身的真实 import 关系。
3. 其余语言解析边界（照 research §6，别过度承诺）：
   - Java：类型索引（`package` + 顶层类型声明 → FQN → 文件）；static import 取**容器**；
     `import a.b.*;` **不造边**（记 unresolved——展开即虚假归因）；同包引用无 import
     语句，是漏检上限，文档标注；包索引里找不到包 → external（JAR/stdlib）。
   - Python：相对导入按点数上行；`from x import name` 先探 `x/name.py` 子模块、
     不存在则归母模块文件；绝对导入自导入文件目录向上模拟 sys.path（最近优先）；
     `from __future__` → external。
   - Go：`go.mod` `module` 前缀内 → 目标目录全部 `.go` 文件（包 = 目录，归因选择）；
     无 `go.mod` → 全部 external；`_`/`.` import 照记（真实边）。
   - TS/JS：只解析相对 specifier（`./`、`../`；扩展名探测 `.ts/.tsx/.d.ts/.js/...` 与
     `/index.*`）；`import x = require(...)` 的 source 在 `import_require_clause` 上
     （TS 陷阱）；bare 与别名一律 external、`#subpath` 记 unresolved；tsconfig `paths`
     不解析（phase 2 候选，文档标注）。
4. **不可解析的边绝不凭空造环**：只有落在扫描集内的 resolved 边进图；自环丢弃；
   SCC 只在 resolved 边上跑。
5. churn（CLI 层，`--churn` 开启，默认关）：git 优先（colocated jj 仓库 git HEAD 覆盖
   全链，已实测本仓 63/65）、`jj log --summary` 兜底；`rate = commits / max(0.5月,
   距首见月数)`；热点 = `fan_in ≥ --min-fanin`（默认 3）且 `rate ≥` 全仓 p90，
   按 `fan_in × rate` 排序。
6. 命令面：`qingluan deps [PATH...] [--top N] [--min-fanin N] [--churn] [--json]
   [--quiet]`。默认输出：摘要行（含每语言 resolved/external/unresolved 与解析率）、
   环列表（无环时一行「no cycles」）、fan-in 榜（标注「fan-in 是下界；高 fan-in 通常
   意味着稳定抽象」）。**有环 → exit 1**（`--json` 同样）。
7. JSON `schemaVersion: 1`：`root / scanned / languages[] / cycles[] / files[] /
   hotspots[]`（后者仅 `--churn`）；`files` 全量、fan_in desc → path asc。
8. 测试：SCC golden（两节点环 / 无环 / 多 SCC / 未解析边不造环 / 空仓 / 单文件）；
   每语言解析 golden；Rust 模块树（mod.rs、兄弟文件、`#[path]`、super/self、裸段
   本地 vs 外部、cfg 门控包含）；churn 注入的热点选取；CLI JSON 形状。
9. 文档：`coupling-as-complexity.md` §8 记落地差异；spec.md 补记新命令面。

明确不做：分层违规（留给用户声明，另开 issue）、tsconfig `paths` 别名解析、
Zone of Pain、任何 fan-in 阈值/闸门、把耦合折进 complexity 的函数表。

## Comments

## Comments

- 2026-09-30 agent：完成，验收点 1–9 全部落地。
- **引擎**（`qingluan-complexity` 新 `deps` 模块：`extract.rs` / `resolve.rs` /
  `graph.rs` / `mod.rs`）：抽取与解析严格分层——抽取层只出 specifier（六语言
  逐构造的 CST 陷阱都按研究 §1–§6 处理：TS `import x = require(...)` 的 source
  在 `import_require_clause` 上、Java `import_declaration` 无字段、Python
  `import_prefix` 是子节点不是字段）；解析层三分类
  `resolved / external / unresolved`，**只有落在扫描集内的 resolved 边进图**，
  自环丢弃。Tarjan 用迭代式（5 万节点链验证不炸栈）；环 = SCC≥2，排序
  跨目录优先 → 成员数 desc → 字典序。churn 是注入输入（`ChurnEntry` map +
  now），引擎保持纯函数。
- **Rust 修复**（此前 2.7%）：crate 根发现 = `Cargo.toml` 手扫（`[package] name`、
  `[lib] path/name`、`[[bin]]/[[test]]/[[example]]/[[bench]]` 显式 path，隐式
  `src/main.rs` 只在无 `[[bin]]` 时生效）+ `tests/examples/benches` 顶层 `.rs`
  + `build.rs`；模块树按 `mod` 声明递归，`#[path]` 四条目录规则**经 rustc
  实测确认**（顶层=文件自身目录、内联=child_dir/inline 链虚拟目录）；
  `use` 解析支持 `crate::`/`self::`/`super::`（含 `super::super::`）/裸段
  （先 crate 根后当前模块，2018 uniform paths）/跨 crate 名（→ 对方 lib 根）/
  **自引用 crate 名 ≡ `crate::`**（walk 而非指向自身）。`#[cfg]` 门控 mod 照实
  包含（文件存在即边，过近似已注明）；macro 生成的 mod 天然不可见（诚实漏检）。
  **实测本仓：1263 specifiers、0 unresolved、100% accounted**（修复前 2.7%）。
- **其余语言**：Java 类型索引（package+顶层类型 FQN→文件），static import 取
  容器，`import a.b.*` 记 unresolved（展开即虚假归因，绝不造边）；Python
  相对导入按点数上行、`from x import y` 先探子模块再归母模块、绝对导入自文件
  向上模拟 sys.path（**绝对 miss → external、相对 miss → unresolved**）；
  Go 按 `go.mod` 前缀（最长匹配），目标目录全部 `.go` 文件收边（包=目录的
  归因选择），无 `go.mod` 全 external；TS/JS 只解析相对 specifier
  （`.js`→`.ts/.tsx/.d.ts` 替换 + `/index.*` 探测），`@/`、`~/`、`#` 记
  unresolved，bare（含 `@scope/pkg`）记 external。
- **CLI**：`qingluan deps [PATH...] [--top N] [--min-fanin 3] [--churn] [--json]
  [--quiet]`；默认输出 = 摘要（每语言 resolved/external/unresolved 与
  accounted 率）+ 环列表（跨目录优先）+ fan-in 榜（表头明标「fan-in 是下界；
  高 fan-in 通常意味着稳定抽象」）；`--churn` 加 fan-in × churn 热点表
  （fan_in≥min_fanin 且 rate≥全体 p90，按 fan_in×rate 排序）；**有环 exit 1
  （`--json` 同样）**。JSON `schemaVersion: 1`（root/scanned.languages/cycles/
  files/hotspots，hotspots 仅 `--churn` 时出现）。
- **churn 读取**（CLI 层 `churn.rs`）：git 优先（colocated jj 仓库 git HEAD
  覆盖全链，已实测本仓 rev-list HEAD 63 ≈ jj 65；`rev-parse --show-toplevel`
  定位 repo 根再 canonicalize），jj `log --summary` 兜底（非 colocated）。
  **发现研究脚本 `hotspots.py` 的 first-wins bug**：其 `first[line]` 记的是
  *最新*提交时间（=距上次修改），与 docstring「since first commit」不符；
  工具按文档语义实现（距首见，oldest stamp），已在 `churn.rs` 顶部注明。
- **本仓实测**（`qingluan deps`）：170 文件（rust 101 / ts 69）；
  rust 257 resolved / 1006 external / 0 unresolved；ts 31/82/135（46% accounted
  ——unresolved 全是 `./generated/**`（生成物被扫描跳过，目标不在节点集）与
  `.vue` 组件（v1 语言盲区），均为诚实 miss）。**7 个环**，工具照见自己：
  `deps` 模块自身 5 文件环（lib↔deps/mod↔resolve↔extract↔scan）、
  `kernel.rs↔langs/mod.rs`、storage 5 文件环、daemon 4 文件环、
  core/terminal 6 文件环、workspace/jj↔mod、client ids.ts↔wire.ts。
  fan-in 榜首 `qingluan-core/src/lib.rs`（fan-in 31 / I=0，稳定抽象的典型）。
- **测试**：complexity 34 单测 + 15 deps 集成（六语言 fixture、SCC golden、
  「未解析边不造环」、`#[path]` 规则、跨 crate 名、空仓/单文件、I 端点、
  churn 注入）+ 14 golden + 6 scan；cli 20 单测（新增 deps JSON 形状、churn
  解析器）+ 2 集成；config 7。clippy `-D warnings` 干净；`cargo fmt -p` 自有
  crate 干净（`grpc_transport.rs` 是既有问题，未动）。
