# 调研：被广泛依赖的模块该怎么判，真正的病态该标什么（fan-in vs 环 / 不稳定 / 分层违规）

> 调研日期：2026-09-30 · 目标：qingluan 已有函数级复杂度（cc / cognitive / nloc / params / nesting，向量、无复合分）。
> 用户提议把**文件级 fan-in**（被很多文件 import）也当作一种复杂度。本文用**架构文献的一手来源**回答：
> 一手文献到底把什么判为病态，以及在没有 call graph 的前提下，这些判据里哪些是可计算的。
>
> 方法：5 个并行子代理各自核对一手来源（Martin 原始 PDF、Parnas CACM 原文、PLoP 原文、期刊/会议论文、
> 各工具官方文档与源码、语言官方规范与 tree-sitter grammar 实测）。**主报告只保留结论与短引文**；
> 子代理完整报告（逐条 URL + 引文 + 证据强度）在：
> `.scratch/complexity/research/coupling-lineage.md`（度量谱系：Henry & Kafura → Martin → CK）、
> `coupling-evidence.md`（fan-in 与缺陷/变更成本的实证）、
> `coupling-erosion.md`（Parnas / Big Ball of Mud / 侵蚀综述 / 环的实证）、
> `coupling-static-analysis.md`（六语言语法可解析性 + grammar 实测）、
> `coupling-tooling.md`（Structure101/NDepend/JDepend/dependency-cruiser/SonarQube 等工具呈现与阈值）。
> 另有两份并行兄弟报告（同一 workspace）：`_raw/02-coupling-defects-faninout.md`、兄弟代理的耦合谱系调研。

## TL;DR

1. **文献里没有一条原则说"被很多模块依赖"本身是病态。** Martin 的原话把高 fan-in 定义为**稳定（stable）**，
   是"好依赖"的**目标形态**；SDP 要求的不是"降低 fan-in"，而是**依赖方向朝稳定性**——即**不稳定**的模块
   不该被稳定模块依赖。高 fan-in + **不稳定**（或高 fan-in + **具体/不可扩展**）才是 Zone of Pain。
2. **被点名最一致、最锋利的病态是依赖环（cycles）。** ADP 原文（1996 年 C++ Report 专栏，已核实全文）：
   "THE DEPENDENCY STRUCTURE BETWEEN PACKAGES MUST BE A DIRECTED ACYCLIC GRAPH (DAG). THAT IS, THERE MUST BE
   NO CYCLES IN THE DEPENDENCY STRUCTURE."；Martin 2000 明确说环会让"每个模块依赖其它每个模块"；
   "morning after syndrome" 的原始出处也已定位到同一篇 1996 年专栏（不是 2017 年的书），它把环与
   "每天被别人的改动打断"直接绑定。环也是唯一一个**所有工具都敢让构建失败**的判据
   （`no-circular`、`beFreeOfCycles()`、tangle、namespace-cycle）。
3. **raw fan-in 的实证信号是弱的、且方向常常对不上直觉。** Zimmermann & Nagappan 2008：ingoing（fan-in）
   Spearman **.283**，outgoing（fan-out）**.440**，而论文自述 "Most complexity metrics have slightly higher
   correlations than network measures"（>.50）。Tahir et al. 2021：fan-in 是显著性最差的指标之一。
   我们本机数据同向：**fan-in 与文件 churn 负相关**（ρ = −0.227 / −0.311），与文件内最差 cognitive **几乎无关**（+0.025）。
   按 raw fan-in 排名会把"最稳、最少改的接口文件"排到最前面。
4. **最强的一条"耦合 → 成本"实证恰恰是"环 + 高耦合"，不是纯 fan-in。** MacCormack & Sturtevant 2016
   （两个 ~2 万文件系统）：Core/Central 文件占 26%，却贡献 **62%** 的 defect-related activity；每行维护成本
   是外围文件的 **3–15×**。但该文的 "Core" 定义就是**最大的环状依赖组**，样本只有 2 个系统、横截面。
5. **没有一家的 fan-in 数字自带阈值。** 查遍 Structure101 / NDepend / JDepend / dependency-cruiser / ArchUnit /
   JDeps / SonarQube / Lattix / Sonargraph：**fan-in（afferent coupling）没有任何一方给默认数字阈值**。
   唯一的耦合类默认数字是 SonarQube **S1200 `max = 20`**，而它数的是 **fan-out**（"classes a single class is
   allowed to depend upon"）。唯一与 instability 有关的默认数字是 NDepend **ND1407 `NormDistFromMainSeq > 0.7`**。
   主流呈现是**图 + DSM + 违规/环列表**，数字是次要的 metrics 视图。
6. **从 import 语句能可靠算的：unresolved 引用清单、fan-out、以及"带项目配置后"的边与 fan-in 下界；
   环可算但最不可信（漏一条边毁掉真环、多一条边造出假环）。分层违规根本不可算——层策略是项目配置。**
   本机语料实测（同一兄弟报告）边解析上限：Java 53.5% / Python 41.3% / TS-JS 23.3% / **Rust 2.7%**。
7. **净建议：不要把 raw fan-in 叫"复杂度"。** 报告应是向量：`fan-in / fan-out / I` 作为中性事实展示；
   **环**作为唯一"违规"标记（并附解析率/置信度）；`fan-in × churn` 作为风险交集；`fan-in + 低 A`（Zone of Pain）
   作为候选线索；分层违规留给用户配置。**不要在 fan-in 上装闸门**，因为一手文献里没有任何阈值依据。

---

## 1. SDP / SAP：原文、精确表述，以及"高 fan-in 应该稳定且抽象吗"

### 1.1 一手文本（按年代，最早且最强的在第一位）

**A. 两篇原始 C++ Report 专栏（1996，Martin 本人托管的 PDF，全文可读）——这是三原则最权威的完整表述：**

- Robert C. Martin, **"Granularity"**, *The C++ Report*, "Engineering Notebook" 专栏 #5（1996-11/12）
  —— **ADP 与 "morning after syndrome" 的原始出处**：
  <https://web.archive.org/web/20030405064407/http://www.objectmentor.com/resources/articles/granularity.pdf>
- Robert C. Martin, **"Stability"**, *The C++ Report*, "Engineering Notebook" 专栏 #6
  —— **SDP 与 SAP 的原始出处**：
  <https://web.archive.org/web/20030405111751/http://www.objectmentor.com/resources/articles/stability.pdf>

> **SDP（"Stability", PDF p.8，原文全大写）**：
> "THE DEPENDENCIES BETWEEN PACKAGES IN A DESIGN SHOULD BE IN THE DIRECTION OF THE STABILITY OF THE
> PACKAGES. A PACKAGE SHOULD ONLY DEPEND UPON PACKAGES THAT ARE MORE STABLE THAT IT IS."
> （"THAT IT IS" 是原文印刷错误，转录时保留原样或加 `[sic]`）
>
> **SDP 的操作化重述（同文 PDF p.10）**：
> "The SDP says that the I metric of a package should be larger than the I metrics of the packages that
> it depends upon. i.e. I metrics should decrease in the direction of dependency."
>
> **SAP（"Stability", PDF p.11）**：
> "PACKAGES THAT ARE MAXIMALLY STABLE SHOULD BE MAXIMALLY ABSTRACT. INSTABLE PACKAGES SHOULD BE
> CONCRETE. THE ABSTRACTION OF A PACKAGE SHOULD BE IN PROPORTION TO ITS STABILITY."
>
> **SAP 与 SDP 的合并（同节）**：
> "The SAP and the SDP combined amount to the Dependency Inversion Principle for Packages."
>
> **ADP（"Granularity", PDF p.6）**：
> "THE DEPENDENCY STRUCTURE BETWEEN PACKAGES MUST BE A DIRECTED ACYCLIC GRAPH (DAG). THAT IS, THERE MUST
> BE NO CYCLES IN THE DEPENDENCY STRUCTURE."

**B. 一句话规范版**（Martin 本人 2005 年 "Principles of OOD" 页，逐条实证）：
<http://www.butunclebob.com/ArticleS.UncleBob.PrinciplesOfOod>

**C. 后续书稿版本**：Martin 2000《Design Principles and Design Patterns》
（<https://web.archive.org/web/20150906155800id_/http://www.objectmentor.com/resources/articles/Principles_and_Patterns.pdf>）
与 *Clean Architecture* (2017) Ch.14 —— 用于读**展开论证**（稳定性定义、Zone of Pain、环的发布后果），
措辞与 A 一致但更啰嗦。

| 原则 | 一句话版（Martin 2005 页） | 1996 原始专栏（全大写原文） | 2000 书稿章节 |
| --- | --- | --- | --- |
| ADP | "The dependency graph of packages must have no cycles." | "THE DEPENDENCY STRUCTURE BETWEEN PACKAGES MUST BE A DIRECTED ACYCLIC GRAPH (DAG). THAT IS, THERE MUST BE NO CYCLES IN THE DEPENDENCY STRUCTURE." | "The dependencies betwen packages must not form cycles."（拼写如此） |
| SDP | "Depend in the direction of stability." | "THE DEPENDENCIES BETWEEN PACKAGES IN A DESIGN SHOULD BE IN THE DIRECTION OF THE STABILITY OF THE PACKAGES. A PACKAGE SHOULD ONLY DEPEND UPON PACKAGES THAT ARE MORE STABLE THAT IT IS." | "Depend in the direction of stability." |
| SAP | "Abstractness increases with stability." | "PACKAGES THAT ARE MAXIMALLY STABLE SHOULD BE MAXIMALLY ABSTRACT. INSTABLE PACKAGES SHOULD BE CONCRETE. THE ABSTRACTION OF A PACKAGE SHOULD BE IN PROPORTION TO ITS STABILITY." | "Stable packages should be abstract packages." |

**引用纪律（核实结论）**：*Clean Architecture* Ch.14 的 SDP/SAP 句子**未能核实**（O'Reilly 公开预览只到
ADP 段落，Google Books 本次被配额/验证码挡住，IA 借阅本受 access-restricted）——**不要把该书的 SDP/SAP
措辞当 verbatim 引用**。另：网上常见的 **"A component should be as abstract as it is stable."** 只在第三方
文献里出现（TU Wien 2020 学位论文 §4.15 引 PPP）——**secondary-only，勿当 Martin 原句**。
该书的 ADP 句 "Allow no cycles in the component dependency graph." 是 `verified-via-publisher-preview`。

**SAP 的正式定义句（2000，§The Stable Abstractions Principle）**：

> "Stable packages should be abstract packages."

以及同节的重述：

> "Thus, the SAP is just a restatement of the DIP. It states the packages that are the most depended upon
> (i.e. stable) should also be the most abstract."

### 1.2 "稳定"在原文里的定义——不是"很少改"，是"改起来要费很多功夫"

> "Stability is related to the amount of work required to make a change. The penny is not stable because it
> requires very little work to topple it. On the other hand, a table is very stable because it takes a
> considerable amount of effort to turn it over."

> "One sure way to make a software package difficult to change, is to make lots of other software packages
> depend upon it. A package with lots of incomming dependencies is very stable because it requires a great
> deal of work to reconcile any changes with all the dependent packages."

（Martin 1994 年的原始论文早已说过同一件事，并给了这类模块一个名字——"Responsible"：
"Another reason that 'Reader' and 'Writer' are stable is that they are depended upon by many other classes. …
I call classes that are heavily depended upon, 'Responsible'. Responsible classes tend to be stable because any
change has a large impact." —— *OO Design Quality Metrics* 1994, p.4–5，
<https://web.archive.org/web/2007id_/http://www.objectmentor.com/resources/articles/oodmetrc.pdf>）

### 1.3 度量原式（1994 论文与 2000 章一致）

> "Ca Afferent Coupling. The number of classes outside the package that depend upon classes inside the package.
> (i.e. incomming dependencies)"
> "Ce Efferent Coupling. The number of classes outside the package that classes inside the package depend upon.
> (i.e. outgoing dependencies)"
> "I Instability. I = Ce / (Ca + Ce). This is a metric that has the range: [0,1]."
> "A Abstractness. A = Na / Nc."

Instability 的读法（同节）：

> "If there are no outgoing dependencies, then I will be zero and the package is stable. If there are no
> incomming dependencies then I will be one and the package is instable."

SDP 的操作化重述：

> "Now we can rephrase the SDP as follows: 'Depend upon packages whose I metric is lower than yours.'"

### 1.4 直接回答三个问题

**Q1：高 afferent coupling（很多依赖者）的模块，应该被要求稳定吗？**
**是。** 而且在 Martin 的框架里，高 fan-in 就是稳定的**定义**（"A package with lots of incomming dependencies
is very stable"）。所以"被很多人依赖"不是需要被降低的指标，而是**需要被遵守的约束**。

**Q2：它应该同时是抽象的（高 A）吗？**
**是，如果它稳定。** SAP 原文 "Stable packages should be abstract packages."；重述句 "the packages that are
the most depended upon (i.e. stable) should also be the most abstract"。理由是要用 OCP 化解"稳定 = 难改"：
"Stable categories that are extensible are flexible and do not constrain the design."（1994）。

**Q3：一个被广泛依赖的模块**不稳定**时，原则说会发生什么？**
这正是 SDP 的违规定义。2000 年原文给了一个具名例子：

> "Figure 2-27 shows how the SDP can be violated. Flexible is a package that we intend to be easy to change.
> We want Flexible to be instable. However, some engineer, working in the package named Stable, hung a
> dependency upon Flexible. This violates the SDP since the I metric for Stable is much lower than the
> I metric for Flexible. As a result, Flexible will no longer be easy to change. A change to Flexible will
> force us to deal with Stable and all its dependents."

代价在 1994 年论文里写得更早、更直白：

> "A design is rigid if it cannot be easily changed. Such rigidity is due to the fact that a single change to
> heavily interdependent software begins a cascade of changes in dependent modules. When the extent of that
> cascade of change cannot be predicted by the designers or maintainers the impact of the change cannot be
> estimated."

**A–I 图上的两个坏角落（原文）**：

> "The upper right corner of the AI graph represents packages that are highly abstract and that nobody depends
> upon. This is the zone of uselessness."
> "the lower left point of the AI graph represents packages that are concrete and have lots of incomming
> dependencies. This point represents the worst case for a package. Since the elements there are concrete,
> they cannot be extended the way abstract entities can; and since they have lots of incomming dependencies,
> the change will be very painful. This is the zone of pain, and we certainly don't want our package to live there."

**注意原文的自我保留**（引用时不要当实证）：

> "These metrics measure object oriented architecture. They are imperfect, and reliance upon them as the sole
> indicator of a sturdy architecture would be foolhardy."

1994 年论文结尾同调："a metric is not a god; it is merely a measurement against an arbitrary standard."

**对本议题的结论**：**fan-in 本身是"责任/稳定"事实，不是病态。** 病态是**依赖方向反了**：不稳定（易变、
具体、或处在活跃变更中）的模块被稳定模块依赖。如果一定要把 fan-in 写进报告，它必须与"是否稳定"
（I、A、或实际 churn）**成对出现**，单独出现就是误读原原则。

---

## 2. ADP 与 "morning after syndrome"：环才是被点名的病理

### 2.1 原文表述

> "THE DEPENDENCY STRUCTURE BETWEEN PACKAGES MUST BE A DIRECTED ACYCLIC GRAPH (DAG). THAT IS, THERE MUST BE
> NO CYCLES IN THE DEPENDENCY STRUCTURE."
> （Martin, "Granularity", *The C++ Report* 1996，§The Acyclic Dependencies Principle，PDF p.6，`verified-in-full-text`）

> "The dependencies betwen packages must not form cycles."
> （Martin 2000, §The Acyclic Dependencies Principle）

> "Allow no cycles in the component dependency graph."
> （*Clean Architecture*, 2017, Ch.14 §The Acyclic Dependencies Principle；`verified-via-publisher-preview`）

Clean Architecture 的 SDP 一节把 fan-in 直接定义为稳定性（与 §1.3 一致）：

> "Fan-in: Incoming dependencies. This metric identifies the number of classes outside this component that
> depend on classes within the component. … I: Instability: I = Fan-out / (Fan-in + Fan-out). This metric has
> the range [0, 1]. I = 0 indicates a maximally stable component."

### 2.2 为什么环是更锋利的问题（原文给的三条后果）

**(a) 环把"独立可测试、可发布"直接摧毁。** Martin 2000 用 Protocol 包举例（图 2-21/2-22）：

> "Consider what would be required to release the Protocol package. The engineers would have to build it with
> the latest release of the CommError package, and run their tests. Protocol has no other dependencies, so no
> other package is needed. This is nice. We can test and release with a minimal amount of work."

加进一条边形成环之后：

> "Now what happens when the guys who are working on Protocol want to release their package. They have to build
> their test suite with CommError, GUI, Comm, ModemControl, Analysis, and Database! This is clearly disastrous.
> The workload of the engineers has been increased by an abhorent amount, due to one single little dependency
> that got out of control."

**(b) 环会自我放大成"每个模块依赖每个模块"。**

> "This means that someone needs to be watching the package dependency structure with regularity, and breaking
> cycles wherever they appear. Otherwise the transitive dependencies between modules will cause every module
> to depend upon every other module."

**(c) 环是团队协作层面的每日疼痛（"morning after syndrome"，1996 年原始出处已核实）。**

> "Have you ever worked all day, gotten some stuff working and then gone home; only to arrive the next
> morning at to find that your stuff no longer works? Why doesn't it work? Because somebody stayed later
> than you! I call this: 'the morning after syndrome'."

> "The 'morning after syndrome' occurs in development environments where many developers are modifying the
> same source files. … It is not uncommon for weeks to go by without being able to build a stable version
> of the project."

> "If there are cycles in the dependency structure then the 'morning after syndrome' cannot be avoided."

**出处**：Robert C. Martin, **"Granularity"**, *The C++ Report*, "Engineering Notebook" 专栏 #5（1996-11/12），
ADP 一节，PDF p.6–7：
<https://web.archive.org/web/20030405064407/http://www.objectmentor.com/resources/articles/granularity.pdf>
（`verified-in-full-text`；原文的 "at to find" 与 "Because somebody stayed later than you!" 为原样转录）。
**这是该术语可核实的原始出处**——此前常被归到 2002/2017 的书里，实际 1996 年专栏就有。
*Clean Architecture* Ch.14 复用了这个词（其 ADP 段落来自出版商公开预览，`verified-via-publisher-preview`）。

**结论**：**ADP 针对的是环，不是 fan-in。** Martin 给的解法是打破环（抽新包、或 DIP 反转 + 把接口放到使用方），
从来不是"降低被依赖次数"。

---

## 3. 分层 / 架构侵蚀：一手文献点名的病态与代价

### 3.1 Parnas 1972：判据是"隐藏会变化的设计决策"，不是"降低被使用次数"

出处：D. L. Parnas, *On the Criteria To Be Used in Decomposing Systems into Modules*, CACM 15(12):1053–1058, 1972,
DOI [10.1145/361598.361623](https://doi.org/10.1145/361598.361623)，全文 PDF
<https://www.win.tue.nl/~wstomv/edu/2ip30/references/criteria_for_modularization.pdf>。

判据（Conclusion）：

> "We propose instead that one begins with a list of difficult design decisions or design decisions which are
> likely to change. Each module is then designed to hide such a decision from the others."

**关键的反 fan-in 事实**：被广泛使用在他那里是**正面**的——

> "The line storage module, for example, is used in almost every action by the system."

他真正反对的是**共享内部数据结构/格式**（"The Criteria" 的具体建议之 1）：

> "A data structure, its internal linkings, accessing procedures and modifying procedures are part of a single
> module. They are not shared by many modules as is conventionally done."

变更成本来自**暴露的是具体格式**（§Comparison of the Two Modularizations）：

> "For the first decomposition the second change would result in changes in every module! The same is true of
> the third change. In the first decomposition the format of the line storage in core must be used by all of
> the programs."

> "In the second decomposition the story is entirely different. Knowledge of the exact way that the lines are
> stored is entirely hidden from all but module 1. Any change in the manner of storage can be confined to that module!"

**纠错**：任务描述里引的短语 `a module that is used by many others` **不在** Parnas 1972 原文中（CACM PDF 与
另一 HTML 版全文检索，`many others` 零命中）。若要引 Parnas 论证 fan-in，正确的说法是"fan-in 高的文件若把
内部数据格式暴露给调用者，变更才会昂贵"——判据是**接口的抽象度**，不是被依赖次数。

### 3.2 Big Ball of Mud（Foote & Yoder 1997）：病因是侵蚀与信息全局化

出处：PLoP '97，全文 <https://www.laputan.org/mud/>。

> "A BIG BALL OF MUD is haphazardly structured, sprawling, sloppy, duct-tape and bailing wire, spaghetti code jungle."

> "Information is shared promiscuously among distant elements of the system, often to the point where nearly
> all the important information becomes global or duplicated. The overall structure of the system may never
> have been well defined. If it was, it may have eroded beyond recognition."

> "Even systems with well-defined architectures are prone to structural erosion. The relentless onslaught of
> changing requirements that any successful system attracts can gradually undermine its structure."

它命名的机制是 `THROWAWAY CODE` / `PIECEMEAL GROWTH` / `KEEP IT WORKING` / `SHEARING LAYERS` /
`SWEEPING IT UNDER THE RUG` / `RECONSTRUCTION`。**没有** fan-in 机制；`cycl` 全文命中均为 lifecycle / boom-bust /
feedback cycle，**不是依赖环**。**纠错**：任务描述里的 `grinding it to dust` **不在该文中**（`grind` 零命中）。

值得注意的是 `SHEARING LAYERS` 与 SDP 是同一直觉的不同表述——按**变更速率**分层：

> "Systems and their constituent elements evolve at different rates. As they do, things that change quickly tend
> to become distinct from things that change more slowly. The SHEARING LAYERS that develop between them are like
> fault lines or facets that help foster the emergence of enduring abstractions."

### 3.3 侵蚀 / 违规的定义与代价：有权威汇总，但实证薄弱

- **Perry & Wolf 1992**（*Foundations for the Study of Software Architecture*, SIGSOFT SEN 17(4):40–52,
  DOI [10.1145/141874.141884](https://doi.org/10.1145/141874.141884)）给出术语与因果叙事：
  > "Architectural erosion is due to violations of the architecture. These violations often lead to an increase
  > in problems in the system and contribute to the increasing brittleness of a system—for example, removing
  > load-bearing walls often leads to disastrous results."
  这是**立场论文**：有定义、无测量。
- **Li, Liang, Soliman & Avgeriou 2022**，*Understanding software architecture erosion: A systematic mapping
  study*, JSSE 34(3):e2423, DOI [10.1002/smr.2423](https://doi.org/10.1002/smr.2423)，arXiv 全文
  <https://arxiv.org/pdf/2112.10934>，**纳入 73 篇研究**。其症状表里，**structural symptom 明确包含
  "cyclic dependencies"**；违规视角是 73 篇里最常见的定义（30 篇）：
  > "Violation perspective is the most common description of AEr, which denotes that the implemented architecture
  > of a software system violates the design principles or architecture constraints."
  但它汇总的是**各研究自己的主张**，作者自承 "there is limited empirical evidence regarding their effectiveness
  and productivity." ——**不能当独立量化实证引用**。

### 3.4 环/耦合的实证：最强证据支持"环 + 高耦合"，弱证据支持"纯 fan-in"

| 研究 | 对象 / 结局 | 关键数字与引文 | 强度 |
| --- | --- | --- | --- |
| **MacCormack & Sturtevant 2016**, JSS 120:170–182, DOI [10.1016/j.jss.2016.06.007](https://doi.org/10.1016/j.jss.2016.06.007) | 2 个系统（20,270 / 19,225 文件）；结局 = defect-related activity（DRA）+ 是否出缺陷；控制 LOC 与最大 CC | "Core files account for 26% of system components, but 62% of defect-related activity. Over the period, 30% of Core files experience a defect, compared to 5.8% for peripheral files." / "each line of code in a Central file costs over 15 times as much to maintain as a line of code in a Peripheral-M file" | **强但 n=2、横截面、相关性**；且其 "Core" = **最大的环状依赖组**，不是纯 fan-in |
| **Zimmermann & Nagappan 2008**, ICSE '08:531–540, DOI [10.1145/1368088.1368161](https://doi.org/10.1145/1368088.1368161) | Windows Server 2003 binaries；结局 = post-release defects | Spearman：`Degree` ingoing **.283**、outgoing **.440**、symmetric **.462**；"outgoing dependencies are more related to defects than ingoing dependencies." / "Most complexity metrics have slightly higher correlations than network measures." | 强（大工业系统），但**对"单看 fan-in"是弱/反向支持** |
| **MacCormack, Rusnak & Baldwin 2006**, Management Science 52(7):1015–1030, DOI [10.1287/mnsc.1060.0552](https://doi.org/10.1287/mnsc.1060.0552) | Linux vs Mozilla（DSM propagation cost） | "The propagation cost for Mozilla is 17.35% versus 5.16% for Linux" / 重设计后 "reduced propagation cost from a level varying between 15-18% to a level varying between 2-6%" | 强（**变更传播成本**可测）；**对缺陷是 null/未测**——原文把预测缺陷列为 ongoing work |
| **Tahir et al. 2021**, EMSE, DOI [10.1007/s10664-021-09991-3](https://doi.org/10.1007/s10664-021-09991-3)（arXiv:2106.04687），23 个系统 | 类级缺陷 + 尺寸中介分析 | "The Fan-in metric has most of the insignificant correlation values whereas LOC, Fan-out, RFC and WMC metrics had more significant correlation values" | 强（最干净的 fan-in vs fan-out 分离），**fan-in 显著性最弱** |
| **Melton & Tempero 2007**, EMSE 12(4):389–415, DOI [10.1007/s10664-006-9033-1](https://doi.org/10.1007/s10664-006-9033-1)（作者稿全文 <https://citeseerx.ist.psu.edu/viewdoc/download?doi=10.1.1.141.5362&rep=rep1&type=pdf>） | 78 个 Java 应用的类级依赖环 | 约 **45% 的应用存在 ≥100 个类参与的环**，最大 SCC **2145 个类**；只测了环的普遍程度与最小反馈集（mEFS）重构负担 | **对"环的普遍性"是强证据；对"环 → 缺陷/成本"是 null（未测）** —— 作者把 "confirming the extent to which cycles effect quality" 明确列为 future work |
| **Cataldo, Mockus, Roberts & Herbsleb 2009**, IEEE TSE 35(6):864–878, DOI [10.1109/TSE.2009.42](https://doi.org/10.1109/TSE.2009.42) | 2 家公司 2 个项目、8 年、154 名开发者；结局 = fault proneness | 所有依赖都提高 fault proneness，但 **syntactic（≈ import）解释力最弱**，logical（工作/沟通）依赖最强 | 中（**仅摘要可核**，IEEE 非 OA）。对本议题关键：**import 层信号是三类依赖里最弱的** |

**读法**：证据支持"依赖**结构**（尤其环）与成本/缺陷相关"，**不支持**"一个文件被 import 的次数本身是复杂度"。
另需钉死一条**否定性结论**：**没有任何一手研究把"依赖环"直接与缺陷/成本量化关联**——Melton & Tempero 明确未测；
MacCormack & Sturtevant 的 "Core" 虽是环状组，但其结论落在"耦合"层面。所以"环是病态"的一手依据是
**规范/原则与工具共识**（ADP + 所有工具的 `no-circular`），**不是**实证回归结果，报告里不要写成后者。

---

## 4. 没有 call graph 时到底能算什么（六语言，tree-sitter，一手规范核实）

完整逐构造表见 `.scratch/complexity/research/coupling-static-analysis.md`（89 条引用 URL 全部实测 200，
grammar 节点名由**实际解析**验证并钉在 grammar HEAD commit 上）。以下为结论。

### 4.1 按可信度排序：能算什么

| # | 输出 | 可信度 | 为什么 |
| --- | --- | --- | --- |
| 1 | **带字面相对/限定 specifier 的 out-edge** + 文件系统探测 | **可靠** | 字面量在 CST 里，目标是（当前文件, 字面量, 项目布局）的确定性函数；唯一"单文件 + 探测"就够的一类 |
| 2 | **每文件 specifier 清单**（引用了几种模块、几条相对路径） | **可靠** | 纯 CST 提取，无需解析；本身就是"这个文件牵扯太多模块"的信号 |
| 3 | **fan-out（每文件出边数）** | **带配置可靠**；误差局部 | 只需解析**本文件**的 specifier；一个坏 specifier = 丢一条出边 |
| 4 | **import 图边（已解析）** | **带配置可靠，但各语言 recall 差异巨大** | 需要全仓扫描 + 配置；上限由各语言"NO"构造决定 |
| 5 | **fan-in（每文件入边数）** | **只是下界** | 需要**所有** importer 都解析成功；且偏差非随机：被漏掉的是"用别名/barrel/通配"的那些 importer |
| 6 | **环 / SCC（Tarjan 可算）** | **最不可信** | 环要一圈边全对：**漏一条边毁掉真环，多一条边造出假环**，六语言两种错误都真实存在 |
| 7 | **分层违规** | **不可算** | 没有任何语言规范定义"层"；层策略是项目配置（ArchUnit 的写法就是声明式：`layeredArchitecture().layer("Controller").definedBy("..controller..")`） |
| 8 | **任何函数级信息**（符号、调用、动态分派） | **不可算** | 需要 call graph，超出范围 |

**核心不对称**：**fan-out 便宜且诚实，fan-in 昂贵且有偏。** fan-out 只需一个文件解析；fan-in 需要全仓解析，
而且恰恰"被最多人 import 的文件"最容易通过别名、barrel re-export、通配 import 被引用——正是解析会失败的那些构造。

### 4.2 各语言最大的"静默谎言"（哪条语法形式打死静态解析）

| 语言 | 单文件 CST 无法解析的构造（举例） | 一手依据 |
| --- | --- | --- |
| **Rust** | `#[cfg(feature="x")] mod gated;` / `#[cfg_attr(target_os="linux", path="linux.rs")] mod os;`；`macro_rules!` 生成的 `mod`（解析为 `macro_rule → token_tree`，**没有 `mod_item` 节点**）；`include!("generated.rs")`；`use foo::bar;` 的 `foo` 是 Cargo 依赖名 | [Conditional compilation](https://doc.rust-lang.org/reference/conditional-compilation.html)、[macros-by-example](https://doc.rust-lang.org/reference/macros-by-example.html)、[`include!`](https://doc.rust-lang.org/std/macro.include.html)、[extern prelude](https://doc.rust-lang.org/reference/names/preludes.html#extern-prelude) |
| **TypeScript** | `paths` 别名（`@/lib/x`）与 `baseUrl`；`package.json` 的 `"exports"`/`"imports"`（`#utils`）；`declare module "ambient" {}`（无文件目标）；`/// <reference path="./x.d.ts" />`（解析为**普通 comment 节点**） | [TS module resolution](https://www.typescriptlang.org/docs/handbook/modules/reference.html)、[Node ESM resolution](https://nodejs.org/api/esm.html) |
| **JavaScript** | 非字面 `require('./' + name)`、`` require(`${d}/m`) ``、`import(expr)`（`ImportCall : import ( AssignmentExpression )`；`require` 在 JS grammar 里根本不是节点类型，只是 identifier）；ESM 省略扩展名/目录导入；CJS vs ESM 由 `package.json` `"type"` 而非语法决定 | [Node CJS](https://nodejs.org/api/modules.html)、[Node ESM](https://nodejs.org/api/esm.html) |
| **Python** | `from x import *`（名字集合不可枚举）；`from a.b import c` 的 `c` 可能是子模块也可能是运行时属性（官方定义就是两步）；`importlib.import_module("pkg."+name)` / `__import__(name)`；PEP 420 命名空间包（一个包名映射到 N 个目录，目标不唯一） | [import system](https://docs.python.org/3/reference/import.html)、[`import__`](https://docs.python.org/3/library/functions.html#import__)、[PEP 420](https://peps.python.org/pep-0420/) |
| **Go** | `//go:build linux && amd64` + `foo_linux_amd64.go` 文件名（前者是 **comment 节点**，后者根本不在文件里）；import path 语义"implementation-dependent"；unit 是**包 = 目录**不是文件 | [Go spec: Import declarations](https://go.dev/ref/spec#Import_declarations)、[build constraints](https://pkg.go.dev/cmd/go#hdr-Build_constraints)、[Go modules reference](https://go.dev/ref/mod) |
| **Java** | **同包引用完全不写 import**（CST 里没有边可找，这一条就解释了 53.5% 的上限）；`import a.b.*;`（没有具体类型）；`import static a.b.C.member;`（末段是**成员**不是类）；JAR/classpath；注解处理器生成源码；JLS §7.6 允许一个文件多个顶层类型 | [JLS §7.5 imports](https://docs.oracle.com/javase/specs/jls/se21/html/jls-7.html#jls-7.5)、[JLS §7.6](https://docs.oracle.com/javase/specs/jls/se21/html/jls-7.html#jls-7.6)、[javac](https://docs.oracle.com/en/java/javase/21/docs/specs/man/javac.html) |

**必须靠 call graph 的东西**（不可由文件图暗示）：动态分派 / 虚方法（Java `obj.f()`、Rust `dyn Trait`、
Go interface values、Python `self.m()`）、反射（`Class.forName`、`getattr`）、DI 装配（Spring `@Autowired`）、
Python `importlib`/`__import__`、JS 计算 `require`、Rust trait object 与宏展开、Go 接口满足关系（结构化判定，无 `implements`）。
**反向**：Rust trait object 与宏还额外污染"文件级"结论——宏可合成 item、module 乃至整个 impl。

### 4.3 本机语料的实测解析上限（同仓兄弟测量，`coupling/local-measurements.md`）

| 语言 | "≥1 个 importer 被找到"的文件占比 | 主要损失原因 |
| --- | --- | --- |
| Java | 53.5% | 同包引用不需要 import |
| Python | 41.3% | 动态 import；命名空间包 |
| TypeScript/JS | 23.3% | bare specifier 与 `tsconfig` `paths` 别名 |
| Rust | **2.7%** | `crate::`/`super::` 解析过于天真——**不可用，是 bug 不是下界** |
| Go | 未单列 | 仓库内 package 目录后缀匹配 |

同一批数据的相关性：`fan-in ↔ 改动次数` ρ = **−0.227**，`fan-in ↔ 月变更率` ρ = **−0.311**，
`fan-in ↔ 文件内最差 cognitive` ρ = **+0.025**，`fan-in ↔ 文件 nloc` ρ = −0.250。
即：**raw fan-in 在本机数据上既不是复杂度信号，也不是风险信号**；真正尖锐的集合是
`fan-in ≥ 3 ∧ 变更率前 10%`（1014 个文件里 18 个）的交集。

---

## 5. 病理 → 一手来源 → 静态可算性

| # | 判据 / 病理 | 一手来源（URL） | 能否只靠 import 静态判定 | 备注 |
| --- | --- | --- | --- | --- |
| 1 | **依赖环**（cycle / tangle） | ADP：Martin 2000 <https://web.archive.org/web/20150906155800id_/http://www.objectmentor.com/resources/articles/Principles_and_Patterns.pdf>；Clean Architecture Ch.14（公开转载见 §2.2） | **能算（SCC），但最脆弱** | 所有工具唯一敢 fail build 的耦合判据；须附解析率/置信度 |
| 2 | **不稳定模块被稳定模块依赖**（SDP 违规） | 同上，§SDP；1994 <https://web.archive.org/web/2007id_/http://www.objectmentor.com/resources/articles/oodmetrc.pdf> | **部分能算**：需要 Ca、Ce、以及一个"稳定性"代理（I，或更好的实际 churn） | 需要全仓解析 → fan-in 只是下界；`moreUnstable` 是**相对比较**，不需要绝对阈值 |
| 3 | **Zone of Pain**（高 Ca + 低 A，具体且被广泛依赖） | Martin 2000 §The I vs A graph | **部分能算**：A 需要"抽象类型占比"；文件级近似（接口/抽象类/trait 占比）很粗糙 | NDepend 用 `NormDistFromMainSeq > 0.7` 作为唯一默认数字 |
| 4 | **分层 / 架构违规**（erosion 的 violation 视角） | Perry & Wolf 1992 <https://doi.org/10.1145/141874.141884>；Li et al. 2022 <https://doi.org/10.1002/smr.2423> | **不可算**（层策略是项目配置；ArchUnit 是用户写规则） | 工具只能提供机制（SCC + 声明的层 glob） |
| 5 | **变更传播成本 / 影响半径**（propagation cost） | MacCormack, Rusnak & Baldwin 2006 <https://doi.org/10.1287/mnsc.1060.0552>；Baldwin et al. 2014 <https://doi.org/10.1016/j.respol.2014.05.004> | **能算**（可达性 / 传递闭包密度），但同样吃解析误差 | SciTools 官方口径："a measure of how hard it is to modify the project" |
| 6 | **fan-in 本身** | **无**一手来源把它单独判为病态；Martin 把它定义为**稳定**（§1.2/§1.3）；Parnas 把"被广泛使用"当正面（§3.1） | 能算（下界） | 只应作中性事实或与 churn/稳定性配对 |
| 7 | **fan-out 过高** | 无架构原则直接给；但 SonarQube S1200 = 20、NDepend `TypeCe > 50` 是工程默认 | 能算（最可靠的耦合数字） | 唯一有第一方数字阈值的方向 |
| 8 | **共享可变设计决策 / 内部数据格式外泄** | Parnas 1972 §The Criteria | **不可算** | 需要接口与数据流语义 |
| 9 | **信息全局化 / piecemeal growth** | Foote & Yoder 1997 <https://www.laputan.org/mud/> | **不可算**（模式级判断） | 无测量 |
| 10 | **纯规模**（文件长度） | 见 `code-length-metrics.md`（另一轴，已有结论） | 能算 | 与耦合共线，勿重复计数（Zimmermann & Nagappan 的回归正说明体量类已覆盖大部分信号） |

---

## 6. 工具怎么把它呈现给人类（以及有没有 fan-in 阈值）

完整逐工具引文见 `.scratch/complexity/research/coupling-tooling.md`。结论：

**没有任何一家对 fan-in（afferent coupling）给默认数字阈值。** 查证范围：Structure101、NDepend、JDepend、
dependency-cruiser、ArchUnit、JDeps、SonarQube、Lattix、Sonargraph、CodeScene、Sourcetrail。

| 工具 | 耦合指标 | 有内置数字阈值？ | 呈现形式 |
| --- | --- | --- | --- |
| **Structure101** | Fat（边数 / CC）、design tangle = 最小反馈集引用占比、XS | **有**：Fat **15**（方法 CC）/ **120**（边）/ design tangle **0%**；**无 fan-in 概念** | 图 + **DSM 矩阵** + tangle 隔离框 + %Fat/%Tangled 图 + offender 列表 + CLI XML/CSV |
| **NDepend** | Ca, Ce, I = Ce/(Ce+Ca), A, D′ = ‖A+I−1‖, H | **有**：ND1407 `NormDistFromMainSeq > 0.7`；ND1410 fan-out ≥ **40/90**；H < **0.8**；文档 `TypeCe > 50`。**Ca 无阈值、无建议** | DSM + 依赖图 + 违规列表 + Quality Gate + 排名榜（`Most used types (#TypesUsingMe)` top-100） |
| **JDepend** | Ca, Ce, A, I, D + 环标记 | **无**（源码/文档里没有任何常量；所谓 "0.3–0.7 理想区间" **未核实存在**） | 每包**数字块** + **环列表** + Swing 树 + DOT |
| **dependency-cruiser** | Ca, Ce, I（`--metrics`，明确说改编自 Martin 的书） | **无**：SDP 用 `to: { moreUnstable: true }`——**相对比较**，不是数字 | 图 reporter（dot/mermaid/d2/html）+ 按 I 降序的 metrics 表 + 违规列表 |
| **SonarQube** | 当前无耦合指标；S1200 = CBO（fan-out） | **有**：S1200 `max = 20`（数的是 fan-out） | 架构地图 + intended-architecture 编辑器 + 优先级问题列表 + tangle 可视化 |
| **ArchUnit** | 计算 Lakos CCD/ACD/NCCD 与 Martin Ce/Ca/I/A/D | **无**（metrics 是 compute-only；仅有环报告数量上限 100/20） | 项目自写规则 → 失败的测试 / 违规列表 |
| **JDeps**（OpenJDK） | 无 | **无**（`-cycles` **不是真实选项**） | 包/类文本依赖列表 + `-summary` + DOT |
| **Lattix / Sonargraph / CodeScene** | Lattix：Coupling、Cyclicality、NCCD；Sonargraph：SDI、Cyclicity、ACD/CCD；CodeScene：Code Health + **temporal coupling** | Lattix：规则无数字；Sonargraph：无公开默认值；CodeScene：`coupling_threshold_percent` = **80%**（变化耦合，非结构 fan-in） | DSM（Lattix 主视图）+ 热图 + 违规报告；CodeScene 热力图 + 排名表 + CI 闸门 |

**三个具有第一方默认阈值的例外**：Structure101（Fat 15/120、tangle 0%）、NDepend 默认规则集（ND1407/1410/1405-06）、
SonarQube S1200 = 20。其余全都是"用户自己写规则"。

**规范 UI 的层级**：① 依赖**图**（通用基线）；② **DSM 矩阵**（架构工具的主视图——注意 DSM 里"环"就是对角线以上的格子，
Structure101 原话："If there are any dependencies above the diagonal, then the graph contains at least one tangle."）；
③ tangle / SCC **违规列表**（环的标准呈现）；④ 每文件/每包**数字**只是次要 metrics 视图，
**没有任何工具把耦合做成一个裸的 per-file 数字**。

**引用纠错**：Structure101 **没有 "XLM"**（官方指标是 XS = Excessive Structural Complexity），也**没有 NCCD**
（NCCD 是 Lakos 的指标，出现在 ArchUnit 与 Lattix 文档里）。**不要**在报告里用这两个词。

---

## 7. 结论与建议：per-file 报告到底该标什么

### 7.1 一句话

**"被很多人依赖"是责任（responsibility）与稳定性事实，不是复杂度。** 真正的病态按锋利程度排：
**环 > 不稳定模块被稳定模块依赖（SDP 违规）> Zone of Pain（高 Ca + 低 A）> 分层违规（需项目配置）**。
raw fan-in 单独出现，既没有一手依据，本机数据里也不与 churn/复杂度相关。

### 7.2 具体建议（与现有"向量、无复合分、文件不拦截"的契约一致）

**做（按优先级）**

1. **环检测作为唯一的"违规"条目。** Tarjan SCC 跑在 import 图上，输出 SCC 成员环（不是每文件一个数字），
   并**强制附上该语言的解析率/置信度**（"Rust: 边解析 2.7%，本结果不可引用"）。对应文献：ADP；对应工具先例：
   `no-circular`（error 级）、`beFreeOfCycles()`、structure101 tangle 0%、SonarQube "tangles"。
2. **每文件耦合向量 `fan-in / fan-out / I`——展示，不拦截。** 三个数必须同时出现（只有 I 有意义：
   `I = Ce/(Ca+Ce)`，Martin 原式），并在文案里说明 fan-in 是**下界**。对应文献：SDP；对应工具先例：
   JDepend 的 per-package 数字块、dependency-cruiser 的 instability 表。
3. **`fan-in × churn` 的风险交集榜**，而不是 fan-in 榜。这是唯一有实测支撑的组合信号
   （本机：18/1014；Zimmermann & Nagappan 的组合模型 recall 比纯复杂度模型高 10 个百分点），
   也正好呼应 Big Ball of Mud 的 `SHEARING LAYERS`——按**变更速率**而不是 import 次数看模块。
4. **Zone of Pain 作为候选线索（可选、需明确标注为近似）**：高 fan-in + 低抽象度。
   对应唯一的工具默认数字 NDepend `NormDistFromMainSeq > 0.7`；但文件级抽象度是粗代理，建议只做排序展示。

**不做**

5. **不把 raw fan-in 当复杂度、不进任何阈值并集、不做闸门。** 一手文献无阈值依据；所有工具都不在 Ca 上设数字；
   本机数据里它与 churn 负相关——按它排名会系统性地把最稳的接口文件顶到最前。
6. **不把 SDP 只实现成"fan-in 高就报警"。** SDP 是**相对**约束（dependency-cruiser 用 `moreUnstable: true` 正是这个意思），
   要报的是"依赖了一个比自己更不稳定的模块"。
7. **不做层/分层违规检测**，除非用户显式提供层配置（照 ArchUnit 的模式：声明层 glob → 违规列表）。
   语言规范里没有"层"，这不是解析能力问题，是**没有输入**。
8. **不把 fan-in 与文件长度/函数数混进同一个分数**——Zimmermann & Nagappan 的回归显示体量类指标已覆盖大部分信号，
   叠加会重复计数。

**诚实性约束（硬要求）**

9. **输出必须带解析率**（per-language），并且 Rust 在修好 `#[cfg]` / 宏生成 `mod` / `include!` / `#[path]` 之前
   **不应输出任何 Rust 耦合数字**（2.7% 是 bug）。
10. **环与 fan-in 都受"假边/漏边"影响，且方向相反**：漏边 → 环消失、fan-in 低估；假边 → 假环、fan-in 高估。
    报告里应写明这一对失败模式（Java 通配 import、Python `from x import *`、Go `import _ "..."` 是典型假边来源）。
11. **不把 SDP/SAP 当实证引用。** 它们是设计约定，Martin 自己写 "reliance upon them as the sole indicator of a
    sturdy architecture would be foolhardy"。

### 7.3 与函数级复杂度报告的关系

保持两套东西**互不折算**：函数级 `{cc, cognitive, nloc, params, nesting}` 与文件级 `{fan-in, fan-out, I, 环成员}`
是两个不同粒度、不同语义的向量。若要合并呈现，应该是**一张每文件表带多列 + 违规集合（环）**，
而不是把 fan-in 加进复杂度分数——这与 `code-length-metrics.md` §4 已经定下的"不做复合分"是同一个决定。

---

## 附录 A：证据强度自评（诚实声明）

**强（有样本、有数字、可复述）**：Martin 2000 与 1994 的原则/度量原文；Parnas 1972 原文；Foote & Yoder 1997 原文；
Zimmermann & Nagappan 2008；MacCormack & Sturtevant 2016；MacCormack, Rusnak & Baldwin 2006（仅"传播成本可测"）；
工具阈值与呈现（官方文档/源码逐条核实）。

**中**：Li et al. 2022（73 篇的**主张汇总**，非独立测量，作者自承实证有限）；Tahir et al. 2021（arXiv 版全文）；
Perry & Wolf 1992（定义权威，无实证）。

**弱 / 未能核实（不要当结论用）**：

- *Clean Architecture* (2017) Ch.14 的 **SDP/SAP 措辞未能核实**（O'Reilly 公开预览只暴露 ADP 段落；Google Books
  本次被配额/验证码挡住；IA 借阅本 access-restricted，`_djvu.txt` 返回 401，未绕过）。该书 ADP 句经
  `verified-via-publisher-preview` 核实。**三原则的权威引文请用 1996 年 C++ Report 两篇原始专栏**（§1.1 A）。
- 网上流行的 **"A component should be as abstract as it is stable."** 只在第三方文献出现（TU Wien 2020 学位论文
  §4.15 引 PPP）——`secondary-only`，**不是 Martin 原句**。
- **"依赖环 → 缺陷/成本"没有可引用的量化实证**：Melton & Tempero 2007 取得了作者稿全文，但**只测了环的普遍程度**
  （78 个 Java 应用中约 45% 有 ≥100 类的环，最大 SCC 2145 类）与最小反馈集重构负担，**未测缺陷/成本**，
  作者自己把 "confirming the extent to which cycles effect quality" 列为 future work。因此第 7 节建议把环作为
  **违规条目**，其依据是 ADP 与工具共识，**不是**实证回归结果。
- Sarkar, Kak & Rama 2008（TSE 34(5):700–720, DOI [10.1109/TSE.2008.43](https://doi.org/10.1109/TSE.2008.43)）
  提出了模块化质量指标并在开源系统上验证，但**没有缺陷/成本结果变量 → null**（仅摘要可核）。
- Cataldo et al. 2009 与 Sarkar et al. 2008 **仅摘要可核**（IEEE 非 OA）；Baldwin et al. 2014 全文未取得。
- JDepend 的 "I 理想区间 0.3–0.7" **不存在于源码/文档**（只在示例 JUnit 测试里）。
- Structure101 的 "XLM"、SonarQube 历史 Ca/Ce/I 指标：**未能核实**（分别是不存在的名词 / 已被移除但无一手页面）。
- `jdeps -cycles` 不是真实选项。
- 任务描述中的 `a module that is used by many others`（Parnas）与 `grinding it to dust`（Big Ball of Mud）
  **均在原文中不存在**，本文已按原文改写。
- 未系统排查早于 Martin 的 "morning after syndrome" 用法（Lakos 1996 等）。

**复现提示（网络）**：本机 `web.archive.org` **经 mihomo 代理不可达**（TLS 超时 / 502），必须绕过代理才通：
`curl -sL --noproxy '*' 'https://web.archive.org/web/...'`。`sdp.pdf`/`sap.pdf`/`adp.pdf` 三个存档是
1197 字节的 HTML 停车页，**真正的正文在 `granularity.pdf` 与 `stability.pdf`**。

**未在本报告展开但已落盘的相关测量**（属"变更历史"轴而非架构原则轴，故只在此指向）：
sibling 报告 `coupling-evidence.md` §3–§5（change history vs static structure、hotspot 的第一方定义与验证、
Oliva & Gerosa 2015 / Oliva 2016 的结构依赖 vs 实际共变），以及 `_raw/` 下的原始摘录。
需要的话应另开一节，而不是塞进本报告。

## 附录 B：来源卫生

- 本报告所有下载均为 PDF/HTML，且全部解压在**一次性 `/tmp` 目录**（本环境的 `/tmp` 在两次 bash 调用之间被清空），
  仓库内**未留下本报告产生的任何 PDF**。清理动作：删除了当时已落盘的兄弟代理临时目录
  `.scratch/coupling-lineage-tmp/`（49 个 PDF，约 38MB）与根目录的 GitHub tree dump `jd-tree.json`。
- **残留提醒**：交付时仓库内仍有**并发兄弟代理**正在写入的临时下载：`.scratch/coupling-lineage-tmp2/*.pdf`
  （时间戳持续变化，说明仍在写）、`.scratch/complexity/research/_raw/` 下的下载文本、以及根目录偶发的
  `J5*.pdf` / `J6*.pdf` 扫描件（含 0 字节占位）。它们**不是本报告产生的**，且在写入中，故未删除——
  交付前需由发起方在并发代理全部停止后统一清理。本报告作者未修改任何源文件
  （`git status -- crates docs Cargo.toml` 为空）。
- 子代理完整报告（可追溯每条 URL 与引文）：`coupling-lineage.md`、`coupling-evidence.md`、`coupling-erosion.md`、
  `coupling-static-analysis.md`、`coupling-tooling.md`、`coupling-principles-verified.md`（三原则逐句核实 + dead-end 表）。

## 附录 C：参考文献

1. Martin, R. C. (1996). *Granularity* (The C++ Report, Engineering Notebook #5). ADP 与 "morning after syndrome" 的原始出处。 <https://web.archive.org/web/20030405064407/http://www.objectmentor.com/resources/articles/granularity.pdf>
2. Martin, R. C. (1996). *Stability* (The C++ Report, Engineering Notebook #6). SDP 与 SAP 的原始出处。 <https://web.archive.org/web/20030405111751/http://www.objectmentor.com/resources/articles/stability.pdf>
3. Martin, R. C. (1994). *OO Design Quality Metrics: An Analysis of Dependencies*. <https://web.archive.org/web/2007id_/http://www.objectmentor.com/resources/articles/oodmetrc.pdf>
4. Martin, R. C. (2000). *Design Principles and Design Patterns*. <https://web.archive.org/web/20150906155800id_/http://www.objectmentor.com/resources/articles/Principles_and_Patterns.pdf>
5. Martin, R. C. (2002). *Agile Software Development: Principles, Patterns, and Practices*. Prentice Hall, Ch.20 "Principles of Package Design".
6. Martin, R. C. (2017). *Clean Architecture*. Prentice Hall, Ch.14 "Component Coupling".（SDP/SAP 措辞未核实；ADP 句经出版商预览核实）
7. Martin, R. C. "The Principles of OOD". <http://www.butunclebob.com/ArticleS.UncleBob.PrinciplesOfOod>
8. Parnas, D. L. (1972). On the Criteria To Be Used in Decomposing Systems into Modules. *CACM* 15(12):1053–1058. <https://doi.org/10.1145/361598.361623>
   （全文 PDF <https://www.win.tue.nl/~wstomv/edu/2ip30/references/criteria_for_modularization.pdf>）
9. Foote, B., & Yoder, J. (1997). Big Ball of Mud. *PLoP '97*. <https://www.laputan.org/mud/>
10. Perry, D. E., & Wolf, A. L. (1992). Foundations for the Study of Software Architecture. *SIGSOFT SEN* 17(4):40–52. <https://doi.org/10.1145/141874.141884>
11. Li, R., Liang, P., Soliman, M., & Avgeriou, P. (2022). Understanding software architecture erosion: A systematic mapping study. *JSSE* 34(3):e2423. <https://doi.org/10.1002/smr.2423>
12. MacCormack, A., Rusnak, J., & Baldwin, C. Y. (2006). Exploring the Structure of Complex Software Designs. *Management Science* 52(7):1015–1030. <https://doi.org/10.1287/mnsc.1060.0552>
13. MacCormack, A., & Sturtevant, D. J. (2016). Technical debt and system architecture. *JSS* 120:170–182. <https://doi.org/10.1016/j.jss.2016.06.007>
14. Zimmermann, T., & Nagappan, N. (2008). Predicting defects using network analysis on dependency graphs. *ICSE '08*:531–540. <https://doi.org/10.1145/1368088.1368161>
15. Tahir, A., Bennin, K. E., Xiao, X., & MacDonell, S. G. (2021). Does class size matter? *EMSE*. <https://doi.org/10.1007/s10664-021-09991-3>
16. Henry, S., & Kafura, D. (1981). Software Structure Metrics Based on Information Flow. *IEEE TSE* SE-7(5). <https://doi.org/10.1109/TSE.1981.231113>
17. Melton, H., & Tempero, E. (2007). An empirical study of cycles among classes in Java. *EMSE* 12(4):389–415. <https://doi.org/10.1007/s10664-006-9033-1>
18. Cataldo, M., Mockus, A., Roberts, J. A., & Herbsleb, J. D. (2009). Software Dependencies, Work Dependencies, and Their Impact on Failures. *IEEE TSE* 35(6):864–878. <https://doi.org/10.1109/TSE.2009.42>
19. Baldwin, C., MacCormack, A., & Rusnak, J. (2014). Hidden structure: Using network methods to map system architecture. *Research Policy* 43(8):1381–1397. <https://doi.org/10.1016/j.respol.2014.05.004>（全文未取得；指标口径见 <https://docs.scitools.com/metrics/DirPropagationCost.html>）
20. SciTools Understand, *Directory Propagation Cost*. <https://docs.scitools.com/metrics/DirPropagationCost.html>
