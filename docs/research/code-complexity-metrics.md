# Research: 函数级代码复杂度度量算法

> 调研日期：2026-03 · 目标：为 qingluan 的代码审查功能加入函数级复杂度计算——选型度量算法与多语言实现方案。
> 全部结论基于一手来源（原始论文、官方规范、工具源码），由 4 个并行子代理分头核实：McCabe 1976 原文与 NIST SP 500-235 全文、SonarSource 认知复杂度白皮书 v1.7 与 SonarJS/sonar-java 源码、Halstead/MI 原始论文与 radon/lizard/SonarQube 文档、lizard 源码与 tree-sitter 官方文档及 crates.io 实查。

## TL;DR（推荐路线）

1. **主指标两个**：圈复杂度（CC，McCabe）+ 认知复杂度（SonarSource）。前者回答"这个函数要多少条测试路径"（阈值 10），后者回答"这个函数多难读懂"（阈值 15，嵌套敏感）。两者互补，Sonar 官方 recommended 配置只含认知复杂度，但 CC 是 50 年来事实标准、生态参照最多。
2. **辅助指标**：函数 NLOC、参数个数、最大嵌套深度——实现成本低（遍历语法树时顺手算），SonarQube 把它们做成规则阈值（S107/S134）。
3. **不建议**把 Halstead / MI 作为主指标：学术效度受质疑（Curtis 1979、Lassez 1983），主流工具只做文件级参考值，radon 自己都标注"很实验性"。
4. **实现路线**：tree-sitter + 每语言一份函数定义 query（S-expression）+ 语言无关的计数器内核。tree-sitter 的错误恢复（含 `(ERROR)` 节点的树仍可 query）契合"审查未必能编译通过的 diff"场景；ast-grep 已验证 Rust + tree-sitter + query 技术栈。新增语言 = 加一个 grammar crate + 一份 query。
5. **注意实现口径**：计数规则的最大分歧点是 switch（每 case vs 整体 1）、短路布尔运算符（CC1/CC2）、嵌套 lambda 归属、认知复杂度的逻辑序列"换段"规则（`a||b&&c||d`=3）与 JS 方言 2024-10 起 `||`/`??` 免计偏差；建议在实现里跟随白皮书 Appendix B 口径并文档化自己的选择。

## 1. 圈复杂度（Cyclomatic Complexity, McCabe 1976）

**结论**：最经典、工程采纳最广的函数级复杂度算法。图论定义 `V(G) = e − n + 2`，工程上等价于**决策点计数 +1**；它是线性独立路径数的**上界**，直接关联分支测试成本。缺陷是嵌套不敏感、switch/布尔运算符各工具口径不一。

### 1.1 原始定义（McCabe 1976, IEEE TSE 2(4)）

- **定义**：图 G 有 n 顶点、e 边、p 个连通分量，圈数 `V(G) = e − n + p`；程序控制流图（唯一入口/出口）单连通，故 **`V(G) = e − n + 2`**。节点 = 顺序代码块，边 = 分支。[来源: [McCabe 1976 全文存档](http://web.archive.org/web/20030416191708/http://www.literateprogramming.com/mccabe.pdf)、[DOI](https://doi.org/10.1109/TSE.1976.233837)]
- **+2 的由来**：把出口连一条回边到入口使图强连通后应用"圈数 = 线性独立环路最大数"定理；`V(G) = (e+1) − n + 1 = e − n + 2`，即"从入口出发再回到入口"这一基准回路。原文性质 2：**V(G) 是线性独立路径的最大条数（basis set 大小）**；性质 6：只依赖决策结构。
- **阈值 10**：原文 "a reasonable, but not magical, upper limit"；超限须模块化拆分，唯一豁免是大型 case/switch（"modified" 变体的先声）。
- **与测试的关系（上界性质）**：NIST SP 500-235（Watson & McCabe）明确：V(G) 条基路径**必达全边覆盖（上界）**，最少边覆盖路径数通常更少；完整路径覆盖可无穷（25 个连续 IF 可达 3300 万条路径）。[来源: [NIST SP 500-235](http://www.mccabe.com/pdf/mccabe-nist235r.pdf)、[NIST 页面](https://www.nist.gov/publications/structured-testing-testing-methodology-using-cyclomatic-complexity-metric-0)]

### 1.2 决策点计数法与变体

- **π + 1 的证明**（论文 Section V，引 Mills）：结构化程序中 e = 1 + δ + 3π、n = δ + 2π + 2，代入得 **V = π + 1**。复合谓词 `IF c1 AND c2` 计 2（等价嵌套两个 IF），"count conditions instead of predicates"。
- **布尔运算符差异**（NIST 235）：短路运算符（`&&`、Ada `and then`）+1；全求值运算符 +0。忽略/展开布尔运算即 "suppressing"/"expanding"——**CC1**（每个原子条件 +1，标准型）与 **CC2**（复合布尔整体 +1）变体的根源；"modified CC" 把整个 switch 记 1。[来源: [NIST SP 500-235](http://www.mccabe.com/pdf/mccabe-nist235r.pdf)、[CC1 术语佐证（DTIC）](https://apps.dtic.mil/sti/trecms/pdf/AD1201543.pdf)]

### 1.3 工具实现对照（均一手核实）

| 工具 | 规则要点 |
| --- | --- |
| **SonarQube** | `cyclomaticComplexity = 1 + numberOfConditionalBranches`，每函数最低 1。Java：if/for/while/case/`&&`/`||`/`?`/lambda 箭头各 +1；JS/TS：函数声明、if、`&&`、`||`、三元、循环、case、`throw`、`catch` 各 +1。[来源: [Metric Definitions](https://docs.sonarsource.com/sonarqube-server/10.7/user-guide/code-metrics/metrics-definition/)] |
| **ESLint `complexity`** | 默认阈值 20，variant `classic`/`modified`。源码：初始 1；CatchClause、三元、LogicalExpression、For/ForIn/ForOf、If、While、DoWhile、默认参数（AssignmentPattern）、逻辑赋值（`&&=`/`||=`/`??=`，因短路）、可选链 `?.` 各 +1；classic 每个case +1，modified 整个 switch +1（官方示例：3 case + 1 if → classic=5、modified=3）。仅函数级计算。[来源: [规则文档](https://eslint.org/docs/latest/rules/complexity)、[源码 complexity.js](https://github.com/eslint/eslint/blob/main/lib/rules/complexity.js)] |
| **radon**（Python） | if/elif 各 +1、else +0、`case` 模式 +1、`case _` +0、for/while/except/with/assert/推导式/每个布尔运算符各 +1、finally +0。[来源: [radon intro](https://radon.readthedocs.io/en/stable/intro.html)] |
| **lizard** | 多语言 CCN，默认告警阈值 15；`-m/--modified` 把 switch 计 1；`-Ecognitive` 另算认知复杂度。[来源: [lizard README](https://raw.githubusercontent.com/terryyin/lizard/master/README.rst)] |

关键分歧点：switch（每 case vs 整体 1）、布尔短路运算符、lambda/嵌套函数归属（ESLint/SonarQube 记入外层函数）。

### 1.4 手算示例（ESLint classic 语义）

```js
function classify(a, b) {          // 1  （基础）
  if (a > 0 && b > 0) return 1;    // if +1, && +1
  if (a < 0) return 2;             // if +1
  for (let i = 0; i < 3; i++) {    // for +1
    if (b === i) return 3;         // if +1
  }
  return a > b ? 4 : 5;            // ?: +1
}
```

决策点 6 个 → **V(G) = 1 + 6 = 7**。测试含义：全边/分支覆盖最多需 7 条基路径（上界），实际可更少（2 条即覆盖全部分支边）；基路径集能张成所有路径，完整路径覆盖因循环迭代次数不同而无界。

### 1.5 已知局限（认知复杂度的动机）

1. **对"可理解性"不是好代理**：switch 拆多态、巨型 if 提炼小函数，可读性提升而逐函数 CC 不降反可能升；SonarSource 指出 CC "把方法和运算符混为一谈"、switch 分数随 case 数线性膨胀、方法拆分不受奖励。[来源: [SonarSource 白皮书](https://www.sonarsource.com/resources/white-papers/cognitive-complexity/)、[Campbell 论文 DOI](https://dl.acm.org/doi/10.1145/3194164.3194186)]
2. **布尔运算符口径不一**：CC1/CC2/modified 变体导致同一代码不同工具分数不同。
3. **嵌套不敏感**：3 个平铺 if 与 3 层嵌套 if 的 V(G) 相同（都是 4），认知负担迥异——认知复杂度引入嵌套增量的直接动机。
4. McCabe 原文性质 3：V(G) ≥ 1、与功能语句无关，纯规模/数据复杂度不在度量范围。


## 2. 认知复杂度（Cognitive Complexity, SonarSource）

**结论**：SonarSource 2016 推出、现为工业界"可理解性"主流指标（clang-tidy、ESLint 插件、SonarQube 均实现）。核心设计：**基础增量 + 嵌套增量**——平铺结构只 +1，嵌套结构按层级加罚，奖扁平、罚深嵌套。Sonar 官方 recommended 配置含它而不含圈复杂度。

### 2.1 原始文献与动机

权威一手文献是 G. Ann Campbell 官方白皮书《Cognitive Complexity: a new way of measuring understandability》（2016 首发，现行 v1.7 / 2023-08-29，**Appendix B 即语言无关规范**），2018 年经同行评审进入 ACM/IEEE TechDebt [来源: [白皮书 PDF](https://www.sonarsource.com/docs/CognitiveComplexity.pdf)、[DOI](https://dl.acm.org/doi/10.1145/3194164.3194186)]。

动机（白皮书）：圈复杂度度量"可测性"准确、度量"可理解性"失真（"cry wolf"）；未覆盖 try/catch、lambda；方法底分 1 使类/应用级数值无意义。三原则：①忽略把多条语句简写的"简写结构"；②对线性流中断 +1；③对嵌套的中断结构按层级加罚。增量四类：Structural（受嵌套罚且抬层级）、Hybrid（不罚但抬层级）、Fundamental / Nesting。

### 2.2 完整计数规则表（白皮书 Appendix B；总分 = Σ 各项）

| 结构 | 计分 | 受嵌套罚 | 抬高嵌套层级 |
| --- | --- | --- | --- |
| `if`（含 `#if`） | +1+层级 | ✓ | ✓ |
| `else if` / `elif` | +1 | ✗ | ✓ |
| `else` | +1 | ✗ | ✓ |
| 三元 `?:` | +1+层级 | ✓ | ✓ |
| `switch`（**全部 case 合计仅 1 次**，default 不计） | +1+层级 | ✓ | ✓ |
| `for` / `foreach` / `while` / `do-while` | +1+层级 | ✓ | ✓ |
| `catch`（多异常类型仅 1 次；try/finally 0 分） | +1+层级 | ✓ | ✓ |
| `goto`、带标签 `break`/`continue`（多级跳转） | +1 | ✗ | ✗ |
| 二元逻辑运算符序列（`&&` / `||`） | 每段"新的同类序列"+1 | ✗ | ✗ |
| 递归环中每个方法（含间接递归） | +1 | ✗ | ✗ |

豁免（0 分、不抬层级）：方法本身（顶层 nesting=0）、无标签 break/continue、提前 return、一元 `!`、可选链 `?.` 与空值合并 `??`。嵌套方法/lambda 自身 0 分但抬高层级。

逻辑序列细则（白皮书原文例）：`a&&b&&c`=1；`a||b||c||d`=1；`a||b&&c||d`=3（`||`、`&&`、`||` 三段）；`a && !(b && c)`=2——**括号/子表达式构成独立新序列**。else-if 链不逐级加深：链上每个 else-if 仅 +1（Hybrid），内部不因链长加罚。

### 2.3 实现核对（官方源码实读）

- **SonarJS S3776**：[rule.ts](https://github.com/SonarSource/SonarJS/blob/master/packages/analysis/src/jsts/rules/S3776/rule.ts)（`DEFAULT_THRESHOLD=15`；else-if 只 +1、else 分支抬层级、switch 计一次结构增量、仅带 label 的 break/continue 计分）与官方单测 [unit.test.ts](https://github.com/SonarSource/SonarJS/blob/master/packages/analysis/src/jsts/rules/S3776/unit.test.ts)（期望值：`switch(a){}` = +1、`foo(1&&2||3&&4)` = +2、`foo(1&&2&&!(3&&4))` = +2、`foo(1||2||3||4)` = +0）。
- **JS 方言偏差**：2024-10-18 v2.0.4 起（ESLINTJS-62）JS 实现将 `||` 与 `??` 完全免计，故 JS 里 `a&&b||c`=1、`a||b&&c||d`=1（白皮书口径为 2 / 3）[来源: [SonarJS CHANGELOG](https://github.com/SonarSource/SonarJS/blob/master/packages/analysis/src/jsts/rules/CHANGELOG.md)]。
- **sonar-java**：[CognitiveComplexityMethodCheck.java](https://github.com/SonarSource/sonar-java/blob/master/java-checks/src/main/java/org/sonar/java/checks/CognitiveComplexityMethodCheck.java)（`DEFAULT_MAX=15`，豁免 equals/hashCode）与 [CognitiveComplexityVisitor.java](https://github.com/SonarSource/sonar-java/blob/master/java-frontend/src/main/java/org/sonar/java/ast/visitors/CognitiveComplexityVisitor.java)。Java 版无 JS 式豁免，与白皮书一致。
- **注意**：JS 与 Java 官方实现源码中均无"递归 +1"逻辑（规范有此条，实现未落地）。
- **clang-tidy** `readability-function-cognitive-complexity` 按规范 v1.2 实现，默认阈值 25 [来源: [clang-tidy 文档](https://clang.llvm.org/extra/clang-tidy/checks/readability/function-cognitive-complexity.html)]；npm 的 `eslint-plugin-cognitive-complexity` 已是 security 占位包，不可用。
- 圈复杂度对照 S1541：[rule.ts](https://github.com/SonarSource/SonarJS/blob/master/packages/analysis/src/jsts/rules/S1541/rule.ts)（`DEFAULT_THRESHOLD=10`）。

### 2.4 手算示例（官方规则，eslint-plugin-sonarjs 实测 = 15）

```js
function classify(user, items) {
  let result = "";
  if (user.isActive && user.verified) {      // if +1(nest0)；&&序列 +1
    for (const it of items) {                // +2 (nest1)
      if (it.price > 100 && it.stock > 0) {  // +3 (nest2)；&& +1
        result += "A";
      } else {                               // else +1（else 块内 nest=3）
        result += it.banned ? "B" : "C";     // 三元 +4 (nest3)
      }
    }
  } else if (user.isAdmin) {                 // else-if +1
    result = "admin";
  } else {                                   // else +1
    result = "guest";
  }
  return result;                             // 合计 1+1+2+3+1+1+4+1+1 = 15
}
```

同函数 CC=8。对比：扁平 else-if 链 5 分 vs 3 层嵌套 if 7 分（CC 却同为 4–5）——**认知复杂度罚嵌套、奖扁平**；白皮书开篇同 CC=4 的两函数：switch 版 `getWords` 认知=1，带标签跳转的 `sumOfPrimes` 认知=7。

### 2.5 阈值

- 认知复杂度 S3776：默认 **15**（[RSPEC S3776](https://sonarsource.github.io/rspec/#/rspec/S3776)），按函数级计算，另聚合文件级度量；Sonar 官方 recommended 配置含 cognitive-complexity、**不含** cyclomatic-complexity [来源: [SonarJS rules README](https://github.com/SonarSource/SonarJS/blob/master/packages/analysis/src/jsts/rules/README.md)]。
- 圈复杂度 S1541：默认 **10**。两者量纲不同互不可比：CC≈测试路径数（McCabe 建议），认知复杂度=理解负担（嵌套敏感、尺度更陡，独立标定）。


## 3. Halstead 与其他函数级度量

**结论**：Halstead 与 MI 学术上受质疑、工程上多作文件级参考值，**不建议作为 qingluan 的主指标**；真正有函数级工程价值的是嵌套深度、参数个数、NLOC 这类简单规模度量（SonarQube 把它们做成规则阈值而非数值度量）。

### 3.1 Halstead 复杂度（Halstead, 1977）

对代码静态统计操作符/操作数，四个基础量：`η1`=不同操作符数、`η2`=不同操作数数、`N1`=操作符总数、`N2`=操作数总数。派生公式（radon 官方文档逐条列出）：

- 词汇表 `η = η1 + η2`；程序长度 `N = N1 + N2`
- 计算长度 `N̂ = η1·log2(η1) + η2·log2(η2)`
- Volume `V = N·log2(η)`
- Difficulty `D = (η1/2)·(N2/η2)`
- Effort `E = D·V`；编程时间 `T = E/18` 秒；交付缺陷数 `B = V/3000`

[来源: [radon intro](https://radon.readthedocs.io/en/latest/intro.html)]

函数级应用：radon 的 `hal` 命令默认按文件计算，`-f/--functions` 改为按顶层函数计算 [来源: [radon commandline](https://radon.readthedocs.io/en/latest/commandline.html)]；lizard 用 `-Ehalstead` 扩展实现 [来源: [lizard README](https://raw.githubusercontent.com/terryyin/lizard/master/README.rst)]。

经验效度批评：Curtis et al. 1979（IEEE TSE）用 Halstead/McCabe 度量程序员心理复杂度，发现相关性弱且不稳定 [来源: [DOI](https://doi.org/10.1109/tse.1979.234165)]；Lassez et al. 1983 "A critical examination of software science" 系统性检验并质疑其"科学定律" [来源: [Semantic Scholar](https://www.semanticscholar.org/paper/5fcb2a295a3c51cb303020fcae5cc6329bfd83c7)]；Shepperd 1988 对圈复杂度的著名批评同属此类质疑 [来源: [Semantic Scholar](https://www.semanticscholar.org/paper/a4b522d7d55c0ed38c825c4fb9fe28c14d659c0a)]。

### 3.2 维护指数 MI（Oman & Coleman 1990s，SEI 推广）

```
MI = 171 - 5.2·ln(V) - 0.23·G - 16.2·ln(L)
```

V=Halstead Volume，G=圈复杂度，L=SLOC；SEI 变体再加注释项 `+50·sin(√(2.4·C))`（C 为注释百分比）[来源: [radon intro](https://radon.readthedocs.io/en/latest/intro.html)（引原始论文 Oman & Hagemeister ICSM 1992 [DOI](http://dx.doi.org/10.1109/ICSM.1992.242525)、Coleman et al. IEEE Computer 1994 [原文](http://www.ecs.csun.edu/~rlingard/comp589/ColemanPaper.pdf)）、[SEI 手册 CMU/SEI-97-HB-001](https://insights.sei.cmu.edu/documents/1625/1997_002_001_16523.pdf)]。

微软 Visual Studio 归一化到 0–100：

```
MI = MAX(0,(171 - 5.2·ln(V) - 0.23·G - 16.2·ln(L)) * 100 / 171)
```

阈值：0–9 红（低）、10–19 黄、20–100 绿 [来源: [Microsoft Learn](https://learn.microsoft.com/en-us/visualstudio/code-quality/code-metrics-maintainability-index-range-and-meaning?view=vs-2022)]。

粒度：MI 本质是模块级；radon `mi` 只按文件输出，官方文档自标注"MI 仍是很实验性的指标" [来源: [radon commandline](https://radon.readthedocs.io/en/latest/commandline.html)]。技术上可按函数聚合输入，但主流工具均未做函数级 MI。

### 3.3 其他函数级度量

- **最大嵌套深度**：控制结构嵌套层数上限；lizard 以 `-ENS` 扩展计数 [来源: [lizard README](https://raw.githubusercontent.com/terryyin/lizard/master/README.rst)]。
- **参数个数**：lizard 默认输出 parameter count，`-a` 设阈值告警 [来源: 同上]。
- **NLOC/LLOC**：lizard 默认输出 nloc（不含注释的行数）[来源: 同上]；radon 区分 LOC/LLOC/SLOC [来源: [radon intro](https://radon.readthedocs.io/en/latest/intro.html)]。
- **Fan-in/fan-out**（信息流耦合）：Henry & Kafura 1981 "Software Structure Metrics Based on Information Flow"（IEEE TSE SE-7(5)），过程复杂度 = `length·(fan-in×fan-out)²` [来源: [ACM DL](https://dl.acm.org/doi/abs/10.1109/TSE.1981.231113)]。需要跨函数调用图，依赖语义分析。
- **Code churn**：微软 Nagappan/Ball/Zeller 用 churn 类度量预测组件缺陷 [来源: [论文 PDF](http://pdfs.semanticscholar.org/1ab5/b09d0352f4527a6180eef616c9c4162c133e.pdf)]。属于仓库历史维度，与 AST 静态分析互补。

### 3.4 工具生态对照

- **lizard**（多语言，约 28 种）：默认按**函数**输出 NLOC、CCN、token count、parameter count；阈值：CCN 默认 15（`-C`）、函数长度默认 1000（`-L`）、参数个数（`-a`）；扩展 `-ENS`/`-Ehalstead`/`-Ecognitive`；不要求完整编译 [来源: [lizard README](https://raw.githubusercontent.com/terryyin/lizard/master/README.rst)]。
- **radon**（Python）：`cc`=圈复杂度（函数/方法/类粒度，A–F 分级，41+ 为 F）；`mi`=维护指数（**文件级**）；`raw`=LOC 类（文件级）；`hal`=Halstead（文件级，`-f` 后函数级）[来源: [radon commandline](https://radon.readthedocs.io/en/latest/commandline.html)]。
- **SonarQube**：`complexity`=圈复杂度，按函数计算（`1 + 条件分支数`，函数最低 1，整体≈各函数求和，各语言分裂点逐条列出）；`cognitive_complexity`（函数级）；size 类 functions/classes/statements/lines 等计数 [来源: [SonarQube Metric Definitions](https://docs.sonarsource.com/sonarqube-server/2025.2/user-guide/code-metrics/metrics-definition/)]。**纠偏**：SonarQube 没有"参数个数/嵌套层级"数值 metric——它们以代码规则实现（S107 参数上限、S134 嵌套深度上限），违规报 issue 而非数值 [来源: 同上，已全文核对无 parameter/nesting 度量键]。


## 4. 工具生态与多语言实现方案

**结论**：多语言函数级复杂度计算的边界识别有两条成熟路线——lizard 的「正则 tokenizer + 手写状态机」与 tree-sitter 的统一语法树；对 qingluan（Rust daemon、面向未必能编译通过的 diff）推荐 **tree-sitter + 每语言一份函数定义 query + 语言无关的决策节点计数器**。

### 4.1 现有工具如何界定「函数」并计算复杂度

- **lizard**（terryyin/lizard，2.5k★）：**不是 PLY，无 parser 生成器**。`setup.py` 的依赖仅 `pygments`+`pathspec` [来源: [setup.py](https://github.com/terryyin/lizard/blob/master/setup.py)]。核心是自研「正则 tokenizer + 每语言一个手写 Reader/状态机」：基类 `CodeReader.generate_tokens` 用一个大正则把源码切成 token 流，`CodeStateMachine` 按 token 驱动状态迁移 [来源: [code_reader.py](https://github.com/terryyin/lizard/blob/master/lizard_languages/code_reader.py)]。函数边界即状态机逻辑——C 系语言中 `CLikeStates._state_global` 遇标识符调 `try_new_function`，读完 `()` 形参后等待 `{`，`_state_entering_imp` 调 `confirm_new_function()`，随后用 `read_inside_brackets_then("{}")` 括号配对吞掉整个函数体 [来源: [clike.py](https://github.com/terryyin/lizard/blob/master/lizard_languages/clike.py)]。CCN 即数条件 token（基类定义 `if/for/while/catch`、`&&/||`、`case`、`?` 四类集合）。扩展机制：`lizard_languages/` 每语言一个模块（30+），`lizard_ext/` 提供 `-E` 扩展（含 cognitive complexity）。README「Limitations」明确承认对错误语法只保证软失败（漏报/误报），因本质是 partial parser [来源: [lizard](https://github.com/terryyin/lizard)]。
- **SonarQube / SonarSource**：工业级路线，每语言一个独立 analyzer 仓库（sonar-java、sonar-js 等），各自带完整 parser 前端；sonar-java 自述提供 cognitive complexity 等 metrics、600+ 规则 [来源: [sonar-java](https://github.com/SonarSource/sonar-java)]、[sonar-js](https://github.com/SonarSource/sonar-js)]。质量最高但每语言一套代码，投入巨大，不适合轻量借鉴。
- **tokei / scc**：只按语言统计文件数/代码/注释/空白行，没有函数边界概念，无法产出函数级复杂度 [来源: [tokei](https://github.com/XAMPPRocky/tokei)]、[scc](https://github.com/boyter/scc)]。

### 4.2 tree-sitter 路线（推荐）

官方定位：parser 生成器 + 增量解析库，四目标 General / Fast / **Robust（有语法错误也能给出有用结果）** / Dependency-free（纯 C11 可嵌入）[来源: [tree-sitter.github.io](https://tree-sitter.github.io/tree-sitter/)]。对本场景最关键的是**错误恢复**：解析写一半/有错的代码仍得到含 `(ERROR)`、`(MISSING)` 节点的树，且这些节点可被 query 捕获 [来源: [Queries: Syntax](https://tree-sitter.github.io/tree-sitter/using-parsers/queries/1-syntax)]。增量解析服务于编辑器逐键更新树；一次性全文分析用不到，只用 `parse` 一次即可。

**Query 机制**：S-expression 模式匹配，`(node_type (child) field: (x) @capture)`，支持字段约束、通配 `_`、匿名节点、supertype [来源: [Queries](https://tree-sitter.github.io/tree-sitter/using-parsers/queries/1-syntax)]。Rust API：<https://docs.rs/tree-sitter>。

**可行架构**（四步，核心逻辑语言无关）：

1. 按扩展名选 grammar crate，`Parser::parse` 全文得 CST；
2. 每语言一份小 query 找函数定义并捕获节点：Rust `(function_item name: (identifier) @name)`、TS `(function_declaration)`/`(method_definition)`、Python `(function_definition name: (identifier) @name)`、Go `(function_declaration)`/`(method_declaration)`、Java `(method_declaration)`——由 `@fn` 节点取字节区间即函数边界；
3. 对每个函数节点用 `Node::walk` 游标遍历子树，按语言映射决策节点（if/for/while/case/catch/`&&`/`||`/三元/Python `if` expr 等）各 +1，CCN = 1 + 决策数（与 lizard 同构，但边界由语法树保证，不怕嵌套括号/字符串干扰）；
4. 嵌套函数/闭包按最内层归属或独立计。新增语言 = 加一个 grammar crate + 一份 query。

**现实参照**：ast-grep（Rust 写的 CLI 结构化搜索/lint/重写工具）核心即「基于 tree-sitter 产出的 AST 做搜索替换」，YAML 写规则、多核并行 [来源: [ast-grep](https://github.com/ast-grep/ast-grep)]、[工作原理](https://ast-grep.github.io/advanced/how-ast-grep-works.html)]。验证了 Rust + tree-sitter + query 规则技术栈生产可用。

### 4.3 Rust 侧 crate 清单（crates.io 实查，均 tree-sitter 官方 org）

| crate | 最新版 | 最近发布 | 总下载 |
| --- | --- | --- | --- |
| tree-sitter | 0.27.0 | 2026-08 | 41.4M |
| tree-sitter-typescript | 0.23.2 | 2024-11 | 15.2M |
| tree-sitter-python | 0.25.0 | 2025-09 | 16.3M |
| tree-sitter-rust | 0.24.2 | 2026-03 | 20.3M |
| tree-sitter-java | 0.23.5 | 2024-12 | 11.7M |
| tree-sitter-go | 0.25.0 | 2025-08 | 13.3M |
| tree-sitter-bash | 0.25.1 | 2025-12 | 13.8M |

[来源: [tree-sitter](https://crates.io/crates/tree-sitter) 等 crates.io 页面]

注意：grammar crate 与 core 版本节奏不同步（TS/Java 停在 0.23.x，Rust 已 0.24.x），集成时锁定相互兼容的组合。

### 4.4 备选路线对比

- **正则/启发式**（lizard 路线）：零原生依赖、极快，但正确性靠人肉状态机，lizard 官方自认软失败；每加一种语言都要重写一遍函数边界识别。
- **每语言独立 parser crate**：`syn` 只覆盖 Rust，`swc`/`oxc` 只覆盖 JS/TS；覆盖 5 种语言 = 5 套 API / 5 份维护负担，仅当需要类型/宏等语义信息时才值得。
- **tree-sitter 统一方案**：一套 C API + 一种 query DSL 覆盖全部主流语言；错误恢复契合「审查未必能编译通过的 diff」场景；ast-grep 已验证工程可行性。

## 5. 选型建议与后续步骤

**结论**：qingluan 做 **CC + 认知复杂度双指标、tree-sitter 实现**。以下为落地建议（结合 qingluan 现状：Rust workspace、review 流程基于 `jj diff`、无语法分析基建）。

### 5.1 指标组合

| 指标 | 角色 | 阈值参考 | 实现成本 |
| --- | --- | --- | --- |
| 圈复杂度（CC1 口径） | 主指标：测试负担 | 10（McCabe/Sonar）或 15（lizard） | 低：数决策节点 |
| 认知复杂度（白皮书 Appendix B） | 主指标：可理解性 | 15（Sonar S3776） | 中：嵌套层级状态机 |
| NLOC / 参数个数 / 最大嵌套深度 | 辅助：顺手输出 | 1000 行 / ~7 个（S107）/ 3–4 层（S134） | 极低：遍历时顺带 |
| Halstead / MI | 不做 | — | 性价比低，见 §3 |

计数口径预先定死并写进文档（§1.3、§2.2 的分歧点）：switch 按 case 计（classic CC）、短路布尔运算符 +1（CC1）、嵌套函数/lambda 计入外层、认知复杂度逻辑序列按"换段 +1"。

### 5.2 架构（新 crate，如 `qingluan-complexity`）

1. tree-sitter 全文解析（按扩展名选 grammar crate，首批 Rust/TS/JS/Python/Go/Java）；
2. 每语言一份函数定义 query（S-expression，§4.2 的四步架构）；
3. 语言无关计数器内核：输入函数子树 + 语言决策节点映射表，输出 `{cc, cognitive, nloc, params, max_nesting, span}`；
4. 嵌套函数/闭包归属外层（与 ESLint/Sonar 一致），或独立输出——选一个写死。

### 5.3 与 review 流程的整合

- review 会话已在 daemon 侧拿全文（`GET /reviews/<id>/files`），复杂度按文件算一次缓存在内存会话里即可，与"评论不持久化"的现有设计一致；
- 对 review 更有价值的输出是**增量视角**：diff 中被改动函数的复杂度与新值（"这次改动把 `classify` 的认知复杂度从 9 提到 15"），比绝对值更适合人审 agent 产出；
- 超阈值在 review UI 标记，等价于 Sonar 的 S3776/S1541 issue。

### 5.4 验证策略

- 用各来源的手算示例做 golden test：本文 §1.4（CC=7）、§2.4（cognitive=15）、白皮书 `sumOfPrimes`=7、ESLint 官方示例（classic=5 / modified=3）；
- 逻辑序列边界用白皮书原例：`a&&b&&c`=1、`a||b&&c||d`=3、`a && !(b && c)`=2；
- tree-sitter 错误恢复路径要专门测（截断文件、语法错误文件不 panic、结果降级）。

### 5.5 已知取舍

- grammar crate 与 tree-sitter core 版本节奏不同步（§4.3），锁定兼容组合；
- JS 认知复杂度是否跟进 2024-10 起 SonarJS 的 `||`/`??` 免计偏差：建议**不跟进**、全语言统一白皮书口径（qingluan 是跨语言工具，口径一致比贴合某方言重要）；
- 递归 +1：规范有、官方 JS/Java 实现均未落地，第一版不做；
- fan-in/fan-out 需要 call graph（语义信息），tree-sitter 给不了，超出第一版范围。
