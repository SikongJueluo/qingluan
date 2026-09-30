# Research: 耦合与稳定性作为一条轴（"import 多 = 复杂度"？）

> 调研日期：2026-09-30 · 起因：用户提出「一个文件被很多文件 import，改它就是牵一发动全身，
> 这何尝不是一种复杂度」。
> 方法：三个并行子代理各自核对一手来源（原始论文 / 作者原文 / 规则源码 / 工具文档），
> 加上一次本地实测（本机 33 个仓库的 import 图 + git 历史 + 本仓引擎的真实复杂度）。
> 子代理完整报告（逐条 URL 与引文、验证等级标注）在
> `coupling-lineage.md`（824 行）、`coupling-evidence.md`（796 行）、`coupling-static-analysis.md`（755 行）、
> `coupling-architecture.md`（581 行）、`coupling-tooling.md`（460 行）、`coupling-erosion.md`（444 行）、
> `coupling-principles-verified.md`（273 行，SDP/SAP/ADP 逐句核实与 dead-end 记录），共 4133 行；
> 本地实测在 `coupling/local-measurements.md` + 三个脚本。

## TL;DR

1. **这个观察是对的，但它不是「复杂度」，而是「耦合 / 变更影响面」。** 而且**不能直接拿 fan-in 当指标**：
   一手来源里高 fan-in 就是**稳定性的定义**（Martin 原文：「One sure way to make a software package
   difficult to change, is to make lots of other software packages depend upon it… very stable」），
   是 SDP 追求的目标形态，不是病态。
2. **本地实测与文献一致**：`fan-in ↔ churn` 的 Spearman 是 **−0.227**（年龄归一后 −0.311），
   `fan-in ↔ 文件内最差 cognitive` 只有 **+0.025**。本机被依赖最多的文件是
   `ComponentTypeId.java`（188 个依赖者、历史上只改过 1 次、最大 cognitive 4）——
   raw fan-in 排序会把**最不可能出事**的代码排到第一。
3. **唯一被文献和工具共同当作「缺陷」的耦合判据是依赖环（ADP），不是高 fan-in。**
   所有工具都敢因为环而 fail build（`no-circular`、`beFreeOfCycles()`、tangle 0%），
   **没有任何工具对 fan-in 设默认阈值**；现有的数字上限全是 fan-**out**
   （Sonar S1200/S6539 = 20、NDepend `TypeCe > 50`、ESLint `import/max-dependencies` = 10）。
4. **高 fan-in 只在两种被明确写下的情形里是问题**：(a) 高 fan-in + 低抽象度（Martin 的 Zone of Pain）；
   (b) fan-in 落在**本来设计成易变**的模块上——而这时**错在依赖方，不在被依赖方**。
5. **唯一有实测支撑的组合是 `fan-in × churn`**：本机 `fan_in>=3 且变更率最高十分位` 只有 18/1014 个文件，
   且一眼可辨（`BuddycardsItems.java`：21 个依赖者、89 次修改、cognitive 573、1150 nloc）。
6. **结构耦合不总等于历史耦合**：「A import B ⇒ A、B 一起改」在本机三个仓库的 lift 是
   ×5.6 / ×1.8 / **×0.7**；Oliva & Gerosa（ISSRE 2015，45 个 Apache 项目）测得
   `P(共变 | A 依赖 B 且 B 变了) ≈ 32%`。所以「依赖多」是**设计事实**，「牵一发动全身」是**历史行为**。

## 1. 这条度量的家谱：三个独立源头 + 一个分叉

| 源头 | 单位 | 定义 | 声称预测什么 |
| --- | --- | --- | --- |
| **信息流复杂度** — Henry & Kafura 1981, IEEE TSE SE-7(5):510–518 | 过程 | `length × (fan-in × fan-out)²`，其中 fan-in/fan-out 是**信息流**（经参数与全局变量的读写，**不是 import**） | 变更次数（UNIX v6，165 个过程 / 80 次变更；全式 r=0.94，而**只有连通项时 r=0.98——`length` 因子没有贡献**） |
| **变更影响 / 涟漪效应** — Haney 1972 定义传播模型 `T = A(I−P)⁻¹`（常被误gloss成「稳定性比值」）；Yau & Collofello 1980 定义 ripple-effect 稳定性 `LS_k = 1/LRE_k`，在受影响模块集合 `RIPPLEM` 上按概率与复杂度加权 | 模块 | **给定 M 变了，还有多少必须跟着变** | 可维护性、变更成本 |
| **包耦合** — Martin 1994《OO Design Quality Metrics》→ 2000《Design Principles and Design Patterns》 | 包 | `Ca`（afferent）、`Ce`（efferent）、`I = Ce/(Ca+Ce)`、`A = Na/Nc`、`D′ = ‖A+I−1‖` | 设计质量（**声明式约定，零验证**——Martin 自己写 "reliance upon them as the sole indicator of a sturdy architecture would be foolhardy"） |
| **CBO / RFC** — Chidamber & Kemerer 1994, IEEE TSE | 类 | CBO 把 fan-in/fan-out 合成一个数 | 缺陷、维护负担 |

**纠正一条流传的说法**：CK 的 CBO **不是**从 Henry & Kafura 推出来的（CK94 的参考文献里没有
Henry & Kafura，他们自称的理论基础是 Bunge 本体论 + Pressman 的「模块间相互依赖程度」）。
信息流家谱是后来综述作者的追溯性归因。

**粒度警告**：这三套东西的单位分别是**过程 / 模块 / 包 / 类**，而我们能可靠拿到的是**文件**。
文件 ≠ 包 ≠ 类，跨粒度直接搬数字会失真（我们在 CC vs LOC 上已经吃过一次聚合制造相关性的教训）。

## 2. 一手来源怎么看「高 fan-in」

Martin 的三条原则目前**最权威的可读一手文本是 1996 年的 C++ Report 专栏**（比 2002 年的书更早，
后者在网上只有片段；核实细节见 `coupling-principles-verified.md`）：

- **SDP** — Martin, *"Stability"*, The C++ Report, Engineering Notebook #6, p.8
  <https://web.archive.org/web/20030405111751/http://www.objectmentor.com/resources/articles/stability.pdf>
  > "THE DEPENDENCIES BETWEEN PACKAGES IN A DESIGN SHOULD BE IN THE DIRECTION OF THE STABILITY OF THE
  > PACKAGES. A PACKAGE SHOULD ONLY DEPEND UPON PACKAGES THAT ARE MORE STABLE THAT IT IS."
  > （"THAT IT IS" 是原文印刷错误）
  同文 p.10 把它操作化：
  > "The SDP says that the I metric of a package should be larger than the I metrics of the packages
  > that it depends upon. i.e. I metrics should decrease in the direction of dependency."
- **SAP** — 同文 p.11
  > "PACKAGES THAT ARE MAXIMALLY STABLE SHOULD BE MAXIMALLY ABSTRACT. INSTABLE PACKAGES SHOULD BE
  > CONCRETE. THE ABSTRACTION OF A PACKAGE SHOULD BE IN PROPORTION TO ITS STABILITY."
  同节：「The SAP and the SDP combined amount to the Dependency Inversion Principle for Packages.」
- **ADP** — Martin, *"Granularity"*, The C++ Report 1996-11/12（专栏 #5）, p.6
  <https://web.archive.org/web/20030405064407/http://www.objectmentor.com/resources/articles/granularity.pdf>
  > "THE DEPENDENCY STRUCTURE BETWEEN PACKAGES MUST BE A DIRECTED ACYCLIC GRAPH (DAG). THAT IS, THERE
  > MUST BE NO CYCLES IN THE DEPENDENCY STRUCTURE."

稳定性本身的定义与「Responsible」的措辞出自 Martin 1994《OO Design Quality Metrics》与
2000《Design Principles and Design Patterns》（objectmentor 存档）：

> "Stability is related to the amount of work required to make a change."
>
> "One sure way to make a software package difficult to change, is to make lots of other software
> packages depend upon it. A package with lots of incomming dependencies is very stable…"
>
> "I call classes that are heavily depended upon, 'Responsible'…"；`I=0` 是
> "responsible and independent… as stable as it can get"。

> **引用纪律（两条易错点）**：① *Clean Architecture* (2017) Ch.14 的 **SDP/SAP 措辞未被核实**
> （公开预览只暴露 ADP 段落）——引 SDP/SAP 请用上面的 1996 专栏，不要引那本书。
> ② 网上常见的 "A component should be as abstract as it is stable." **只在第三方文献里出现**
> （某学位论文转引 PPP），不是 Martin 的原句。

也就是说：**高 afferent coupling 在原文里就是「稳定」的定义**。SDP 要求的不是降低 fan-in，
而是**依赖方向正确**（依赖指向更稳定的一侧）。Henry & Kafura 也把高 fan-in 读作
「stress point / 抽象不足」，而不是一类缺陷；Parnas 1972 更把「被几乎每个动作使用」当作**正面**例子。

**高 fan-in 变成问题的两种情形**（都有原文依据）：

1. **高 fan-in + 低抽象度 = Zone of Pain**：具体实现被所有人依赖——想改时既动不了（稳定）又没有
   接口可以替换（不抽象）。这是 Martin 度量里唯一被认可的「坏象限」。
2. **fan-in 落在设计上本应易变的模块上**（SDP 违规）。此时**错在依赖方**：是它依赖了一个不该依赖的
   不稳定模块，而不是那个模块「太被依赖」。

## 3. 实证记录：fan-in 是耦合族里最弱的那个

| 研究 | 测什么 | 结果 |
| --- | --- | --- |
| Kitchenham, Pickard & Linkman 1990, *Software Eng. J.* | 耦合 → 维护 | fan-**out** 有预测力，fan-**in** 没有；LOC/分支数更好 |
| Zimmermann & Nagappan 2008, ICSE, Table 4 | 依赖图度数 → 缺陷 | **in-degree .283 < out-degree .440 < LOC .516** |
| Bhattacharya et al. 2012, ICSE | 图度量 → bug 严重度 | 「in- and out-degrees are poor bug severity predictors」 |
| Nagappan, Ball & Zeller 2006, ICSE（5 个微软系统） | 耦合/规模 → 发布后缺陷 | 「not a single metric that would correlate with post-release defects in all five projects」；project A 的 FanIn/FanOut Max 系数为**负**，project D 的 ClassCoupling 为负 |
| Tahir, Bennin, Xiao & MacDonell 2021, EMSE | 耦合族 → 缺陷（含尺寸中介） | 「The Fan-in metric has most of the insignificant correlation values」——**fan-in 是最不显著的** |
| Child, Rosner & Counsell 2019, JSS | ~10 种 CBO 变体 → 缺陷 | 一半「have no practical application to the prediction of defects」——**结论取决于工具实现的哪种 CBO 定义** |
| Olague et al. 2007, IEEE TSE | MOOD 耦合 vs CK | MOOD 耦合**零结果**，CK 族不是 |
| Subramanyam & Krishnan 2003 | CBO 的偏效应 | C++ 为 **+0.173**，Java 为 **−0.011**——同一定义换个语言就翻号 |
| Gil & Lalouche 2017 | 度量的「唯一有效性」 | 「code size is the only 'unique' valid metric」；度量的效度可由它与尺寸的相关性预测，R² 高达 0.97 |
| Tahir et al. 2018, ESEM（常被误引） | 尺寸中介 | 「fully mediates」只针对 **Apache Lucene 2.4 单一系统**，全文结论是 *"We are unable to confirm…"*；作者是 Tahir/Bennin/MacDonell/Marsland |

一句话：**fan-in 单用没有预测力，而且它和复杂度的相关系数在本机是 +0.025（正交）**——
它带着新信息，但那个信息不是「风险」，是「这是个共享抽象」。

## 4. 环才是那个「缺陷」

- ADP 原文就是禁环。Martin 在 1996 年的同一篇专栏里给出了它为什么致命的一手描述——
  **"morning after syndrome" 最早的可核实文本就是这里**（不是 2017 年的书）：

  > "Have you ever worked all day, gotten some stuff working and then gone home; only to arrive the
  > next morning at to find that your stuff no longer works? Why doesn't it work? Because somebody
  > stayed later than you! I call this: 'the morning after syndrome'."
  >
  > "If there are cycles in the dependency structure then the 'morning after syndrome' cannot be
  > avoided."

  以及他举的具体后果：
  > "They have to build their test suite with CommError, GUI, Comm, ModemControl, Analysis, and
  > Database! This is clearly disastrous."
  >
  > "Otherwise the transitive dependencies between modules will cause every module to depend upon
  > every other module."
  即环直接摧毁**独立测试 / 独立发布 / 独立推理**。
- **工具共识**：环是所有工具里**唯一敢 fail build** 的耦合判据——dependency-cruiser 的
  `no-circular` 是 `error` 级、madge 遇环 exit 1、jdepend 称之为 "deadly embrace"、
  ESLint 说环是 "always a dangerous anti-pattern"、ArchUnit 的头牌切片规则是 `beFreeOfCycles()`、
  Structure101 的 tangle 默认要求 0%。**没有任何工具对 fan-in 设默认阈值。**
- **实证（但要诚实）**：MacCormack & Sturtevant 2016, JSS（两个约 2 万文件的系统）：
  Core（= **最大环状依赖组**）占 26% 的文件，却贡献 62% 的缺陷相关活动，每行维护成本 3–15×
  （n=2、横截面，别当定律）。**强否定式警告**：没有任何一手研究把「环」直接回归到缺陷/成本；
  Melton & Tempero 2007 测了 78 个 Java 应用（~45% 存在 ≥100 类的环，最大 SCC 有 2145 个类），
  但只测了普遍性与重构负担，缺陷被明确列为 future work。**环作为违规的依据是 ADP + 工具共识，
  不是回归结果。**

## 5. 结构耦合不总等于历史耦合（本机实测 + 文献）

本机：把 import 边两端的提交集合取 Jaccard，对比 4000 组随机配对：

| 仓库 | 结构边 | 边上均值 | 随机均值 | lift |
| --- | --- | --- | --- | --- |
| BaseUI | 5078 | 0.612 | 0.109 | **×5.6** |
| PlayerSync_hfc | 109 | 0.100 | 0.057 | ×1.8 |
| Buddycards-Core | 337 | 0.116 | 0.176 | **×0.7（低于随机）** |

文献同向：Oliva & Gerosa, ISSRE 2015 + Oliva 2016 博士论文（45 个 Apache 项目、77,286 个快照）：
`P(共变 | A 依赖 B 且 B 变了) ≈ 32%`（σ 13.6%），分类器 AUC 0.52–0.76，
「the majority of co-changes do not correlate with structural dependencies」；
Ajienka & Capiluppi 2017, JSS 在 79 个项目里只有 <10 个显著。
Buddycards-Core 那种「低于随机」通常来自**提交粒度**（一次提交带上几十个无关文件，
把随机基线抬到 0.176）。

## 6. 六种语言从 import 到底能算什么

**结论：out-edge（字面 specifier）+ 文件系统探测可靠；fan-in 只是下界；环可算但最不可信。**

| 语言 | 解析率（本机实测，至少找到一个 importer 的文件占比） | 致命构造 |
| --- | --- | --- |
| Java | 53.5% | **同包引用完全不写 import**（这一条就解释了这个上限）、通配/静态 import、JAR/注解处理器 |
| Python | 41.3% | `from x import *`、`importlib`、PEP 420 命名空间包 |
| TS/JS | 23.3% | `paths` 别名、`package.json` exports/imports、`/// <reference>`、计算式 `require` |
| Rust | **2.7%（这是解析 bug，不是语言极限）** | `#[cfg] mod`、`macro_rules!` 生成的 mod、`include!`、Cargo 依赖名 |

**fan-out 便宜且诚实（只需解析本文件）；fan-in 昂贵且有偏**——因为被最多人 import 的文件，
恰恰最常经别名 / barrel / 通配被引用，也就是**最容易解析失败**的那批。
强语义的度量（Henry & Kafura 的数据流 fan-in/fan-out、CK 的 CBO、RFC、Martin 的**类**计数 `Ca`/`Ce`）
import 语句给不了。

这也意味着**静态影响面永远只能当上界/下界用**：Cai 2018 的原话是
「Static analysis can produce safe but overly-conservative impact sets」——
保守在这里是**好事**（不漏报），但拿它当精确的「影响 N 个文件」去排序就是在卖不存在精度。

## 7. 本地实测（33 个仓库 / 1935 个有历史的文件）

| 关系（Spearman） | 值 |
| --- | --- |
| fan_in ↔ 被改次数 | **−0.227** |
| fan_in ↔ 变更率（年龄归一） | **−0.311** |
| fan_in ↔ 文件内最差 cognitive | **+0.025** |
| fan_in ↔ 文件 nloc | −0.250 |
| 变更率 ↔ 文件 nloc | **+0.399** |

- 被依赖最多的 3 个文件：`ComponentTypeId.java`（188 / 1 次修改 / maxCog 4）、
  `NodeId.java`（170 / 1 / 4）、`PropertyKey.java`（121 / 1 / 16）——**全是稳定的小类型定义**。
- `fan_in>=3 且变更率在最高十分位`：**18 / 1014**。榜首 `BuddycardsItems.java`
  （21 依赖 / 89 次修改 / 1150 nloc / cognitive 573）同时是本仓复杂度榜首、长度榜首与 churn 榜首。
- 诚实限制：Java/Python 的排名可用，TS 严重低估，Rust 不可用；churn 只在 3 个历史够长的仓库上算；
  BaseUI 历史始于 2026-03，低 churn 部分是「新」而不是「稳」（年龄归一后方向反而更强）。

## 8. 对 qingluan 的建议

**不做**：「fan-in 复杂度」这个指标本身、任何 fan-in 阈值/闸门、把 fan-in 折进 cc/cognitive。

**可以做（按性价比排序）**：

1. **环检测作为唯一的「违规」条目**：输出强连通分量（SCC）成员，按「是否跨包/跨层」排序——
   光数环没意义。**必须同时标注该语言的解析覆盖率**；Rust 修好解析前不输出任何数字。
2. **每文件 `fan-in / fan-out / I` 向量**，只展示不拦截，文案写明 **fan-in 是下界**
   且「高 fan-in 通常意味着稳定抽象」。fan-out 可以更自信（便宜、偏低更少）。
3. **`fan-in × churn` 交集榜**（本机 18/1014）：这是唯一有实测支撑的组合，
   也正好是用户直觉真正指向的东西。
4. **Zone of Pain（高 Ca + 低抽象度）作为可选线索**：需要识别 `trait`/`interface`/`abstract` vs 具体类型，
   tree-sitter 能分。注意它会把我们那些稳定 ID 文件也标上（实测反例）——是**设计话题的引子**，不是缺陷预言机。
5. **分层违规留给用户声明**（照 ArchUnit 的声明式做法）；层策略无法自动推断。

**架构后果**：1 和 2 需要**全项目 import 图**（v1 引擎是「一个文件进、指标出」的纯函数），
3 还需要**读 VCS 历史**（v1 刻意不读）。所以这不是在函数表里加一列，而是**另一种报告**（仓库级）。
这个决定应该由用户拍板：接受读历史吗？还是先只做纯静态的依赖报告？

### 落地补记（2026-09-30，issue 08 已实现）

用户拍板：纯静态 + churn 都做、新子命令 `qingluan deps`、先修 Rust、有环非零退出。
建议 1/2/3 全部落地（4/5 未做，照本节结论）。与本文预测的差异与实测：

1. **Rust 解析修好了**：crate 根发现（Cargo.toml 手扫 + `tests/` 等目录文件）+
   `mod` 模块树（`#[path]` 四条目录规则经 rustc 实测）+ `use` 路径解析
   （含跨 crate 名与自引用 crate 名 ≡ `crate::`）。本仓 1263 specifiers、
   **0 unresolved、100% accounted**（§7.6 记录的 2.7% 确是 bug）。
   `#[cfg]` 门控 mod 照实包含（过近似）；macro 生成 mod 不可见（诚实漏检）。
2. **三分类语义**（比「解析率」更细）：`resolved`（仓库内边）/ `external`
   （仓库外）/ `unresolved`（应有目标但失败）。Java `import a.b.*` 记
   unresolved 而非造边；Python 绝对 miss → external、相对 miss → unresolved
   （只有相对导入没有外部语义）；TS 的 `@/`、`#` 记 unresolved、bare 记
   external。本仓 TS 46% accounted：unresolved 全是 `./generated/**`（生成物
   不进扫描集）与 `.vue`（语言盲区）——都是可见的诚实 miss。
3. **churn 语义修正**：§7 的 hotspots.py 存在 first-wins bug——其 `first[line]`
   实为**最新**提交时间（= 距上次修改），与「since first commit」的注释不符。
   工具按文档语义实现（距首见）。用旧脚本复现 §7 数字时注意这一点。
4. **环的输出**：SCC≥2（自环丢弃：文件内引用不是耦合），跨目录环排前
   （「跨目录」= 成员父目录数 >1）。**有环 exit 1**（含 `--json`），
   照工具共识。本仓首跑即报 7 个环，包括 `qingluan-complexity/src/deps/`
   自身的 5 文件环与 `kernel.rs↔langs/mod.rs`——工具照见了自己。
5. **churn 读取**：git 优先（colocated jj 仓库 git HEAD 覆盖全 jj 链，本仓
   实测 63/65），jj `log --summary` 兜底非 colocated 仓库。

## 9. 修正记录（本文写作过程中改掉的自家错误）

- `docs/research/code-length-metrics.md` §3.4 的三处：El Emam 2003 的评论作者是 **Evanco**（不是 Briand）；
  `10.1145/2556777` 是 **Zhou/Xu/Leung/Chen 2014**，结论是「去混淆后**更好**」；
  Tahir 2018 的「fully mediates」只针对单一系统。已在同一提交里改正并加了补丁注记。
- 本仓 `code-complexity-metrics.md` §3.3 把 Henry & Kafura 的 fan-in/fan-out 列为「需要调用图」——
  更准确的说法是：它需要**数据流**（参数与全局变量），比调用图更难，且其 `r=0.94` 是聚合区间上的。
- 引用纪律两条（核实过程见 `coupling-principles-verified.md`）：*Clean Architecture* (2017) 的
  SDP/SAP 措辞**未能核实**，引原则请用 1996 年 C++ Report 专栏；网上流行的
  「A component should be as abstract as it is stable.」是第三方转述，**不是 Martin 原句**。
- 复现提示：本机经 mihomo 代理访问 `web.archive.org` 会 TLS 超时/502，需 `curl --noproxy '*'`；
  且 `sdp.pdf`/`sap.pdf`/`adp.pdf` 的存档都是 1197 字节的停车页，正文在 `stability.pdf` 与
  `granularity.pdf` 里。
