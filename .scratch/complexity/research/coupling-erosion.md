# 分层 / 架构侵蚀（Layering / Architecture Erosion）：一手文献调研

> 主题 3：architectural violations and their cost。目标是为决策服务：**per-file 复杂度报告是否应把 "file-level fan-in"（一个文件被很多其他文件 import）当成一种复杂度来标记？**

---

## TL;DR

1. **一手文献点名的是"病症"是：依赖环（cycles）、架构违规（violations）、侵蚀（erosion）/漂移（drift）、以及"共享的设计决策"（Parnas）。没有任何一篇一手文献把"一个文件被很多其他文件依赖"（fan-in 高）本身当作病态。**
2. **Parnas (1972)** 的判据是"隐藏会变化的决策"。他明确说"被系统里几乎每个动作使用"的 line storage 模块是**好**设计（decomposition 2）；要避免的是把**设计决策**（数据格式、控制块格式）暴露成接口。"被很多模块共享"在他那里是**问题症状**（`They are not shared by many modules as is conventionally done`），但共享的是**内部数据结构**，不是 import 计数。
3. **Big Ball of Mud (1997)** 命名的是 erosion、piecemeal growth、throwaway code、sweeping it under the rug、shearing layers、keep it working、reconstruction；病因是"信息变成全局的或被复制"和"结构从未被定义过 / 已被侵蚀到认不出来"。**没有**把 fan-in 当作机制。（注意：任务里提到的 `grinding it to dust` 这个短语**不在**该文中。）
4. **"Morning after syndrome"** = Robert C. Martin 的术语，可核实的最早文本是 *Clean Architecture* (2017) Ch.14 ADP 一节（2002 年 *Agile Software Development* Ch.20 已有同名 ADP 章节，但该短语在 2002 版正文中是否出现**未能核实**）。它描述的病理是**依赖环**（"If there are cycles in the dependency structure, then the 'morning after syndrome' cannot be avoided"），**不是 fan-in**。
5. **Martin 自己把 fan-in 定义为"稳定性"**（SDP：`I = Fan-out/(Fan-in+Fan-out)`，`I = 0 indicates a maximally stable component`）。真正的风险区是他说的 **Zone of Pain**：稳定 + 具体 + 多变。所以用 Martin 支持"高 fan-in = 复杂度"是**反向引用**。
6. **最强实证（coupling → 成本）：** MacCormack & Sturtevant (2016, JSS)，两个约 2 万文件的系统。Core/Central（高耦合、含环）文件占 26% 的文件，却贡献 62% 的 defect-related activity；每行代码维护成本是外围文件的 3–15 倍。**但样本只有 2 个系统，横截面、相关性。**
7. **次强实证（fan-in → 缺陷）：** Zimmermann & Nagappan (2008, ICSE)，Windows Server 2003。degree centrality 与 post-release defects 显著正相关（symmetric degree r=.462）。**但对本议题是关键反证/弱支持：** 论文自己写 "Most complexity metrics have slightly higher correlations than network measures"，且 **outgoing（fan-out）比 ingoing（fan-in）更相关**（Degree Outgoing .440 vs Ingoing .283）。也就是说，**单看 fan-in 的信号比 LOC/圈复杂度更弱**。
8. **MacCormack, Rusnak & Baldwin (2006, Management Science)** 只提出并测量 **propagation cost**（变更传播成本），**没有**测量缺陷。它把"用该指标预测缺陷"列为**未来工作**。
9. **关于"环"的最常引用实证——Melton & Tempero (2007, ESE)——是描述性的：** 78 个 Java 应用中约 45% 有 ≥100 类的环、最大的一个 SCC 达 2145 类；但作者**没有**测缺陷/成本，并把"环是否伤害质量"明确列为 **future work**。所以"实证证明环导致缺陷"用这篇是错的。
10. **Cataldo et al. (2009, TSE)**：所有依赖都提高 fault proneness，但**语法依赖（最接近 import）解释力最弱**，逻辑依赖最强 —— 又一个"数 import 不够"的证据。
11. **净结论（对本议题）：** "被很多人 import" 本身在文献里**不是**被命名的复杂度病态；被命名的是**环**、**违规/侵蚀**、**共享的可变设计决策**。若要 flag fan-in，文献只支持一个**弱的、需与其他指标合并**的信号，且必须区分 fan-in / fan-out，并注意 fan-in 弱于体量类指标。

---

## (a) Parnas, D. L. (1972), "On the Criteria To Be Used in Decomposing Systems into Modules"

- 出处：*Communications of the ACM* 15(12): 1053–1058, Dec 1972。
- DOI：<https://doi.org/10.1145/361598.361623>
- 取文本的 PDF（作者/大学镜像，全文 6 页，与 CACM 排版一致）：<https://www.win.tue.nl/~wstomv/edu/2ip30/references/criteria_for_modularization.pdf>
- 交叉核对用的 HTML 版：<https://math.pku.edu.cn/teachers/qiuzy/plan/lits/On%20the%20Criteria%20To%20Be%20Used%20in%20Decomposing%20Systems%20into%20Modules.htm>

### 关于 information hiding（第 "The Criteria" 节，p.1056）

> "The second decomposition was made using 'information hiding' as a criterion. The modules no longer correspond to steps in the processing. The line storage module, for example, is used in almost every action by the system."

> "Every module in the second decomposition is characterized by its knowledge of a design decision which it hides from all others. Its interface or definition was chosen to reveal as little as possible about its inner workings."

结论（Conclusion, p.1058）：

> "We propose instead that one begins with a list of difficult design decisions or design decisions which are likely to change. Each module is then designed to hide such a decision from the others."

### 关于"变更模块的成本"（第 "Comparison of the Two Modularizations — Changeability" 节，p.1055）

> "For the first decomposition the second change would result in changes in every module! The same is true of the third change. In the first decomposition the format of the line storage in core must be used by all of the programs."

> "In the second decomposition the story is entirely different. Knowledge of the exact way that the lines are stored is entirely hidden from all but module 1. Any change in the manner of storage can be confined to that module!"

关于"某个设计决策变得 costly"（第 "The Criteria" 节的 5 条具体建议之 3，p.1056）：

> "Because design evolution forces frequent changes on control block formats such a decision often proves extremely costly."

### 关于"被很多模块共享/使用"——**重要**

**任务里给的短语 `a module that is used by many others` 并不在 1972 原文中**（我对 CACM PDF 全文与上述 HTML 版都做了检索，`many others` 零命中）。原文里最接近的三处是：

1. 被广泛使用是**正面例子**（p.1056）：
   > "The line storage module, for example, is used in almost every action by the system."
2. 被广泛使用会让变更**昂贵**（p.1055，见上）：`the format of the line storage in core must be used by all of the programs.`
3. 真正该避免的是**共享内部结构**（p.1056，"The Criteria" 的具体建议之 1）：
   > "A data structure, its internal linkings, accessing procedures and modifying procedures are part of a single module. They are not shared by many modules as is conventionally done."

**解读：** Parnas 的判据是"隐藏会变化的决策"，不是"降低被依赖次数"。一个模块被很多模块使用是正常的、甚至是设计目标；病态在于**共享出去的是具体的数据结构/格式（设计决策）而不是抽象接口**。因此把 file-level fan-in 直接当作复杂度，**在 Parnas 这里找不到支持**；若要用他做依据，正确的说法是"fan-in 高的文件若把内部数据格式暴露给调用者，则变更是昂贵的"。

---

## (b) Foote, B. & Yoder, J. (1997), "Big Ball of Mud"

- 一手全文（作者自托管）：<https://www.laputan.org/mud/>
- 发表信息（文首）：Fourth Conference on Pattern Languages of Programs (PLoP '97 / EuroPLoP '97), Monticello, Illinois, September 1997；Technical Report #WUCS-97-34；另收录于 *Pattern Languages of Program Design 4*, Addison-Wesley, 2000, Chapter 29。
- 无 DOI（PLoP 技术报告）。

### 对病理的精确刻画（Abstract / Introduction）

> "A BIG BALL OF MUD is a casually, even haphazardly, structured system. Its organization, if one can call it that, is dictated more by expediency than design."

> "A BIG BALL OF MUD is haphazardly structured, sprawling, sloppy, duct-tape and bailing wire, spaghetti code jungle."

> "Information is shared promiscuously among distant elements of the system, often to the point where nearly all the important information becomes global or duplicated. The overall structure of the system may never have been well defined. If it was, it may have eroded beyond recognition."

### 对 erosion 的表述

> "Even systems with well-defined architectures are prone to structural erosion. The relentless onslaught of changing requirements that any successful system attracts can gradually undermine its structure."

> "Systems that were once tidy become overgrown as PIECEMEAL GROWTH gradually allows elements of the system to sprawl in an uncontrolled fashion."

### 它命名的机制（七个 pattern，文首目录）

`BIG BALL OF MUD`, `THROWAWAY CODE`, `PIECEMEAL GROWTH`, `KEEP IT WORKING`, `SHEARING LAYERS`, `SWEEPING IT UNDER THE RUG`, `RECONSTRUCTION`。

定义句（各 pattern 的开头）：

> "THROWAWAY CODE is quick-and-dirty code that was intended to be used only once and then discarded."

> "Systems and their constituent elements evolve at different rates. As they do, things that change quickly tend to become distinct from things that change more slowly. The SHEARING LAYERS that develop between them are like fault lines or facets that help foster the emergence of enduring abstractions."

> "A simple way to begin to control decline is to cordon off the blighted areas, and put an attractive façade around them. We call this strategy SWEEPING IT UNDER THE RUG."

### 直接回答任务的问题

- **它把"被广泛共享的模块"当作问题吗？** 不是。它谈的是 "information becomes global or duplicated"（共享**可变信息/状态**），以及结构 erode。没有 fan-in / "imported by many" 的机制。
- **它谈 cycles 吗？** 几乎没有。全文 `cycl` 的命中都是 "lifecycle" / "boom-bust cycle" / "positive feedback cycle"，**不是 dependency cycles**。
- **`grinding it to dust`？** **该短语不在本文中**（`grind` 零命中，`dust` 只有一处与城市街区有关）。如果报告里要用这个短语，请勿归到 Foote & Yoder 名下。

---

## (c) "Morning after syndrome" — 原始出处

**结论：这个术语是 Robert C. Martin 的。可核实的最早出处是 *Clean Architecture* (2017), Chapter 14 "Component Coupling"，Acyclic Dependencies Principle (ADP) 一节的正文。它所在的 ADP 章节内容更早见于 *Agile Software Development: Principles, Patterns, and Practices* (2002), Chapter 20 "Principles of Package Design"（ADP 从 p.256 起）；但我**未能取得 2002 年版正文**，因此无法确认该短语本身是否已出现在 2002 年版。**

### 一手出处（2017）

- Martin, R. C. (2017). *Clean Architecture: A Craftsman's Guide to Software Structure and Design*. Prentice Hall. Chapter 14, "Component Coupling", §"The Acyclic Dependencies Principle"。
- 出版商正文非公开可访问。以下英文原句取自该书 Ch.14 的公开**双语对照转载**（非出版商托管），逐句核对：<http://zone.ci/tech/program/468689.html>（另有内容一致的英文摘录：<https://www.letscodethemup.com/clean-architecture-chapter-14-component-coupling-the-acyclic-dependency-principle/>）
- 引用时请以纸质/电子书为准；本次**未能访问出版商版本**。

**术语的定义（verbatim）：**

> "Have you ever worked all day, gotten some stuff working, and then gone home, only to arrive the next morning to find that your stuff no longer works? Why doesn't it work? Because somebody stayed later than you and changed something you depend on! I call this 'the morning after syndrome.'"

> "The 'morning after syndrome' occurs in development environments where many developers are modifying the same source files."

**它到底在说什么（ADP 的处方）：**

> "Allow no cycles in the component dependency graph."

> "To make it work successfully, however, you must manage the dependency structure of the components. There can be no cycles. If there are cycles in the dependency structure, then the 'morning after syndrome' cannot be avoided."

**因此：病理 = 依赖环（cycles）。不是 fan-in。** 该术语描述的是"每天被依赖方改动打断"的团队/构建问题，而 Martin 给出的解法是打破环（DIP 反转 / 抽出公共组件），不是"降低被 import 次数"。

### 关于 2002 年版

- *Agile Software Development: Principles, Patterns, and Practices*（Prentice Hall, 2002）的出版商目录 PDF 证实：**Chapter 20 "Principles of Package Design"（p.253）** 含 **"The Acyclic-Dependencies Principle (ADP)"（p.256，"ALLOW NO CYCLES IN THE PACKAGE DEPENDENCY GRAPH."）** 与 **"The Effect of a Cycle in the Package Dependency Graph"（p.258）**——与 2017 Ch.14 的节标题和结构一一对应。
  - 目录来源（出版社/发行方 TOC PDF）：<https://wwwzb.fz-juelich.de/contentenrichment/inhaltsverzeichnisse/bis2009/ISBN-0-13-597444-5.pdf>
- **未能核实**：`morning after syndrome` 字样是否出现在 2002 年版 p.256-258。若必须给出"最早出处"，安全写法是：**"该术语出自 Martin 的 ADP 论述；可核实的最早出版文本为 2017 年 *Clean Architecture* 第 14 章，其内容对应 2002 年 *Agile Software Development* 第 20 章。"**
- 也**没有**找到早于 Martin 的使用（未系统排查 Lakos 1996 等，属未核实）。

### 同一章里对"fan-in"的官方定义（对本议题极其关键）

Martin 在 **Stable Dependencies Principle (SDP)** 一节里直接定义了 fan-in，并把它等同于**稳定性**：

> "Fan-in: Incoming dependencies. This metric identifies the number of classes outside this component that depend on classes within the component. … I: Instability: I = Fan-out / (Fan-in + Fan-out). This metric has the range [0, 1]. I = 0 indicates a maximally stable component."

> "In contrast, when the I metric is equal to 0, it means that the component is depended on by other components (Fan-in > 0), but does not itself depend on any other components (Fan-out = 0). Such a component is responsible and independent. It is as stable as it can get. Its dependents make it hard to change the component, and its has no dependencies that might force it to change."

> "The SDP says that the I metric of a component should be larger than the I metrics of the components that it depends on. That is, I metrics should decrease in the direction of dependency."

**解读（直接回答本议题）：** 在 Martin 的框架里，**高 fan-in 不是病态，而是"稳定"**。真正的风险区是他所称的 **Zone of Pain**（稳定但具体、且多变）：

> "Consider a component in the area of (0, 0). This is a highly stable and concrete component. Such a component is not desirable because it is rigid. It cannot be extended because it is not abstract, and it is very difficult to change because of its stability."

所以若要基于 Martin 来 flag 高 fan-in，正确的规则不是"被依赖多 = 复杂度高"，而是"**被依赖多、又是具体实现、又处在易变区域** = 痛苦区"；稳定且抽象的组件是被鼓励的。

---

## (d) 架构侵蚀 / 漂移的综述文献

### (d1) Perry, D. E. & Wolf, A. L. (1992), "Foundations for the Study of Software Architecture"

- 出处：*ACM SIGSOFT Software Engineering Notes* 17(4): 40–52, Oct 1992。
- DOI：<https://doi.org/10.1145/141874.141884>
- 取文本的 PDF（作者自托管，UT Austin）：<https://users.ece.utexas.edu/~perry/work/papers/swa-sen.pdf>

第 2.3 节 "Motivation for Architectural Specifications"（p.43）：

> "One frequently accompanying property of evolution is an increasing brittleness of the system—that is, an increasing resistance to change, or at least to changing gracefully [5]. This is due in part to two architectural problems: architectural erosion and architectural drift."

> "Architectural erosion is due to violations of the architecture. These violations often lead to an increase in problems in the system and contribute to the increasing brittleness of a system—for example, removing load-bearing walls often leads to disastrous results."

> "Architectural drift is due to insensitivity about the architecture. This insensitivity leads more to inadaptability than to disasters and results in a lack of coherence and clarity of form, which in turn makes it much easier to violate the architecture that has now become more obscured."

**强度：** 这是**概念/立场论文（position paper）**，给出了"erosion = 违规、drift = 失感"的**定义**，但没有测量、没有样本、没有数据。引用它只能证明"术语与因果叙事从 1992 年就存在"，不能作为实证证据。

### (d2) Li, R., Liang, P., Soliman, M. & Avgeriou, P. (2022), "Understanding software architecture erosion: A systematic mapping study"

- 出处：*Journal of Software: Evolution and Process* 34(3): e2423, 2022。
- DOI：<https://doi.org/10.1002/smr.2423>
- 开放获取 PDF（arXiv 版，全文可读）：<https://arxiv.org/pdf/2112.10934>
- 出版商 OA：<https://onlinelibrary.wiley.com/doi/pdfdirect/10.1002/smr.2423>

**样本：纳入 73 篇研究**（systematic mapping study）。

定义（Introduction）：

> "As the system evolves, the accumulation of such problems (e.g., architectural violations) can cause the implemented architecture to deviate away from the intended architecture. The phenomenon of divergence between the intended and implemented architectures is regarded as architecture erosion (AEr)."

后果分类（RQ4，§4.5）中的关键句：

> "Architectural defect denotes that an eroded architecture will make the architecture to have more defects (i.e., defect proneness), for example, anti-patterns [S9] and superfluous dependencies [S47], [S73], which in turn is likely to give rise to more erosion."

> "Increased cost denotes that rising costs (including time and labor cost) need to be invested into activities like maintenance and refactoring."

Findings：

> "Finding 5: AEr can lead to various consequences, such as damaging architectural structures and generating software defects."

**强度与重要限制（务必如实引用）：** 这是一篇 **mapping study**，它汇总的是**被纳入的 73 篇研究各自声称的**后果，而**不是**它自己独立测量的效应。它自己也承认证据薄弱：

> "there is limited empirical evidence regarding their effectiveness and productivity."

因此它是"文献一致认为 erosion 有害"的**权威汇总**，但不能当作"erosion → 缺陷"的**量化实证**。引用时应写成 "SMS 汇总的 73 篇研究中反复出现的后果主张"，而不是 "研究证明"。

### (d3) MacCormack, A., Rusnak, J. & Baldwin, C. Y. (2006), "Exploring the Structure of Complex Software Designs: An Empirical Study of Open Source and Proprietary Code"

- 出处：*Management Science* 52(7): 1015–1030, Jul 2006。
- DOI：<https://doi.org/10.1287/mnsc.1060.0552>
- 取文本的 PDF（HBS 作者自托管，标注 "Forthcoming: Management Science 2006"，39 页）：<https://www.hbs.edu/ris/Publication%20Files/05-016.pdf>
- 早期 working-paper 版（用词为 "Change Cost"）：<http://wayback.archive-it.org/all/20060612032250/http://opensource.mit.edu/papers/maccormackrusnakbaldwin.pdf>

**核心指标定义：**

> "We call the resulting metric 'Propagation Cost.' Intuitively, this measures the proportion of elements that could be affected, on average, when a change is made to one element in the system."

**主要结果（Linux vs Mozilla，两个规模相当的源码库）：**

> "The propagation cost for Mozilla is 17.35% versus 5.16% for Linux, a striking difference. This implies that the design of Linux is much more loosely-coupled than the first version of Mozilla. A change to a source file in Mozilla has the potential to impact three times as many source files, on average, as a similar change in Linux"

**Mozilla 重设计（纵向）：**

> "The re-design effort reduced propagation cost from a level varying between 15-18% to a level varying between 2-6%."

**关于"缺陷"——关键限制：** 该论文**没有测量缺陷**。全文 `defect`/`bug` 只出现在动机与未来工作里，且是明确的**研究议程**：

> "Finally, we have begun to assess whether the aspects of design structure that we measure can predict product performance. For example, ongoing work uses our cost metrics assembled at the cluster and source file level to predict the future occurrence of defects ('bugs') in that part of the design."

**强度：** 对 "coupling/propagation cost 可以被测量，且不同设计相差数倍" 是**强**证据（有对象、有指标、有数字）；对 "propagation cost → 缺陷/成本" 是**零证据（null / 未测）**。把它当作"fan-in/coupling 有害"的实证是**过度引申**。

### (d4) MacCormack, A. & Sturtevant, D. J. (2016), "Technical debt and system architecture: The impact of coupling on defect-related activity"

> 任务没有点名，但这是与"violations/coupling 的成本"最直接、最强的一手实证，必须包含。

- 出处：*Journal of Systems and Software* 120: 170–182, Oct 2016。
- DOI：<https://doi.org/10.1016/j.jss.2016.06.007>
- 取文本的 PDF（HBS 作者自托管）：<https://www.hbs.edu/ris/Publication%20Files/2016-JSS%20Technical%20Debt_d793c712-5160-4aa9-8761-781b444cc75f.pdf>

**样本 / 对象：** 两个规模相近的真实系统，**System H = 20,270 个文件**（Hierarchical，propagation cost 2.2%），**System C = 19,225 个文件**（Core-Periphery，propagation cost 22.2%）。观测期约 3 年。

**被测结果（outcome）：** "defect-related activity"（DRA）——以缺陷相关工作量/活动量作为**维护成本**的代理；外加 logit 模型的"是否发生缺陷"。

设计结构（§5.1）：

> "System H has very few cyclical dependencies and a small Core (2.9% of the system). It is a Hierarchical system. System C has a large number of cyclical dependencies and a very large Core (25.8% of the system). It is a Core-Periphery system."

主要数字（Table 3 & §5.2）：

> "Peripheral-M and Isolate files comprise 30% of the system, but experience only 32 of the 2909 pieces of defect related activity (i.e., 1.1%). ... components in the 'Central' category, which generate 2033 pieces of defect related activity (70% of all such activity) despite comprising only 31% of files."

> "In contrast, Core files account for 26% of system components, but 62% of defect-related activity. Over the period, 30% of Core files experience a defect, compared to 5.8% for peripheral files."

回归（Table 4/5，控制了 LOC 与最大圈复杂度）：

> "In models that add predictor variables to the controls, we find a strong association between files that possess high levels of coupling and both i) the likelihood of experiencing a defect, as well as ii) the overall amount of defect-related activity."

- logit：System C 的 `Core` 系数 **1.34561\*\*\***，Pseudo R² 0.106 → 0.140；System H 的 `Central` 系数 **0.69669\*\*\***，Pseudo R² 0.088 → 0.122。
- OLS（DRA 对数）：`Core` **0.24686\*\*\***。

财务口径（§5.4）：

> "For system H, we find each line of code in a Central file costs over 15 times as much to maintain as a line of code in a Peripheral-M file. For system C, we find each line of code in a Core file costs around three times as much as a Peripheral file."

**强度：强（但外部效度有限）。** 有明确的样本、outcome（缺陷相关活动量 / 是否出缺陷）、控制变量、显著性；方向为正（高耦合更贵）。**限制：只有 2 个系统，横截面，无法排除系统身份混杂（confounding）与反向因果。** 且注意其 "Core" 的定义是**最大的环状依赖簇（cyclical group）**，"Central" 是可见 fan-in/fan-out 都高的组——所以它支持"环 + 高耦合"是成本中心，**并没有**单独验证"纯 fan-in"。

### (d5) Zimmermann, T. & Nagappan, N. (2008), "Predicting defects using network analysis on dependency graphs"

> 任务没有点名，但它是对"file-level fan-in 是否预测缺陷"最直接的一手实证。

- 出处：*ICSE '08* (30th International Conference on Software Engineering), pp. 531–540。
- DOI：<https://doi.org/10.1145/1368088.1368161>
- 取文本的 PDF（作者自托管）：<https://thomas-zimmermann.com/publications/files/zimmermann-icse-2008.pdf>
- 官方摘要页（Microsoft Research）：<https://www.microsoft.com/en-us/research/publication/predicting-defects-using-network-analysis-on-dependency-graphs/>

**样本 / 对象：** Microsoft **Windows Server 2003** 的 binaries（binary 级依赖图）；outcome = **post-release defects** 以及开发者标记的 "escrow"（关键）binaries。

**结论摘要：**

> "In our evaluation on Windows Server 2003, we found that the recall for models built from network measures is by 10% points higher than for models built from complexity metrics. In addition, network measures could identify 60% of the binaries that the Windows developers considered as critical—twice as many as identified by complexity metrics."

关于 centrality（degree = 依赖数量）与缺陷：

> "Degree centrality. The degree measures the number of dependencies for a binary. The idea for dependency graphs is that binaries with many dependencies are more defect-prone than others."

Spearman 相关（与缺陷数），Table 4「Global Network」部分（binary 级依赖图）：`Degree` — Ingoing **.283\*\*** / Outgoing **.440\*\*** / Symmetric **.462\*\***（\*\* = 99% 显著）。这里 **Ingoing = fan-in（被别人依赖）**，**Outgoing = fan-out（依赖别人）**。

同一张表里还有一个与本议题直接相关的 OO 指标：`CyclicClassCoupling`（类之间的**环状耦合**）r = **.331\*\***（不同基线，无法与 Degree 直接比大小，但方向为正且显著）。

**对本议题最关键的一条（反直觉）：**

> "(3) Network measures have higher correlations for OUT and IN-OUT than for IN neighborhoods. In other words, outgoing dependencies are more related to defects than ingoing dependencies."

**另一条限制：**

> "(4) Most complexity metrics have slightly higher correlations than network measures. For non-OO metrics the correlations are above 0.500."

> 口径提醒：该文 Table 4 里另有一组 `FanIn .452(**) / FanOut .360(**)`，但按该文 Table 2 的定义，那是**函数级**的 `FanIn = # functions calling f()` / `FanOut = # functions called by f()` 聚合到 binary 的指标（与体量高度共线），**不是**文件之间的 import fan-in。判断 file-level fan-in 时应看上面的 `Degree Ingoing`，而不是这一行。

**强度：强（大工业系统、有显著性、有基线对比），但对"fan-in 应被 flag"是弱/反向支持。** 依赖图的 centrality 确实预测缺陷，但 (i) **fan-out（.440）比 fan-in（.283）更强**，(ii) **LOC/参数/圈复杂度等体量类指标的相关性还略高（>.50）**。所以"单看 file-level fan-in"的边际价值很低；它的价值在于**与体量指标组合**（组合模型 Spearman ≈0.60）。

### (d6) Baldwin, C., MacCormack, A. & Rusnak, J. (2014), "Hidden structure: Using network methods to map system architecture"

> 补充：它定义了后来工具链（如 SciTools Understand）所用的 "core / periphery" 与 propagation cost 口径，其中 **"Core" = 最大的环状依赖组**。

- 出处：*Research Policy* 43(8): 1381–1397, 2014。
- DOI：<https://doi.org/10.1016/j.respol.2014.05.004>
- 官方指标文档（SciTools Understand，说明其 metric 口径）：<https://docs.scitools.com/metrics/DirPropagationCost.html>

SciTools 文档原文（作为该指标在工业工具中被采用的一手文档）：

> "Propagation Cost the density of the matrix expressed as a percentage. This is a measure of how hard it is to modify the project."

> "The 'Core' is the largest cyclical group in the project."

> 注：本次**未能取得 Baldwin et al. 2014 全文**（该期刊未开放获取），上述仅为官方文档转述；若报告要引用其结论，需要另行获取全文核实。

---

### (d7) Cataldo, M., Mockus, A., Roberts, J. A. & Herbsleb, J. D. (2009), "Software Dependencies, Work Dependencies, and Their Impact on Failures"

> 补充：直接比较"语法依赖（syntactic，最接近 import / fan-in）"与其他依赖类型对缺陷的解释力，对本议题非常关键。

- 出处：*IEEE Transactions on Software Engineering* 35(6): 864–878, Nov/Dec 2009。
- DOI：<https://doi.org/10.1109/TSE.2009.42>
- **全文未取得**（IEEE 非开放获取）。以下为该文**官方摘要**（经 OpenAlex/出版商元数据获得，非全文）：<https://api.openalex.org/works/doi:10.1109/TSE.2009.42>

摘要（verbatim，短引）：

> "Our analysis is based on data collected from two projects from two independent companies. Combined, our data set encompasses eight years of development activity involving 154 developers."

> "While all dependencies increase the fault proneness, the logical dependencies explained most of the variance in fault proneness, while workflow dependencies had more impact than syntactic dependencies."

> "These results suggest that practices such as rearchitecting, guided by the network structure of logical dependencies, hold promise for reducing defects."

**强度：中（摘要级证据）。** 样本：2 家公司 2 个项目、8 年、154 名开发者；outcome = customer-reported defects / failure proneness。方向为正（所有依赖都提高 fault proneness），但**语法依赖（最接近 file-level import fan-in 的那种）解释力最弱**，语义/逻辑依赖最强。这进一步说明"数 import 条数"是弱代理。

### (d8) Sarkar, S., Kak, A. C. & Rama, G. M. (2008), "Metrics for Measuring the Quality of Modularization of Large-Scale Object-Oriented Software"

- 出处：*IEEE Transactions on Software Engineering* 34(5): 700–720, Sep/Oct 2008。
- DOI：<https://doi.org/10.1109/TSE.2008.43>
- **全文未取得**（IEEE 非开放获取）。以下为该文**官方摘要**（OpenAlex/出版商元数据）：<https://api.openalex.org/works/doi:10.1109/TSE.2008.43>

摘要（verbatim，短引）：

> "The goal of this paper is to provide a set of metrics that characterize large object-oriented software systems with regard to such dependencies. Our metrics characterize the quality of modularization with respect to the APIs of the modules, on the one hand, and, on the other, with respect to such object-oriented inter-module dependencies as caused by inheritance, associational relationships, state access violations, fragile base-class design, etc."

> "Using a two-pronged approach, we validate the metrics by applying them to popular open-source software systems."

**强度：对"耦合可度量"是中；对"耦合导致缺陷/成本"是 null（未测）。** 该文提出并**用于若干开源系统**度量"模块化质量"（其 API-based 与 OO 依赖类指标），但**摘要中没有任何缺陷、变更成本或维护工作量的结果变量**。因此**不能**用它来支持"高 fan-in → 更差的结果"；它只是"该怎么度量"的方法论文。若报告要引用它，必须写成"指标提案 + 在开源系统上的示例验证"，不能写成"实证发现耦合有害"。

### (d9) Melton, H. & Tempero, E. (2007), "An empirical study of cycles among classes in Java"

> 这是关于"Java 类的依赖环"最常被引用的一手实证研究。**结论对本议题很重要：它是描述性研究，没有测量缺陷或成本。**

- 出处：*Empirical Software Engineering* 12(4): 389–415, 2007（online 2007-07-13）。
- DOI：<https://doi.org/10.1007/s10664-006-9033-1>
- 取文本的 PDF（CiteseerX 上的**作者存档稿**，19 页，页脚标注 "Copyright 200X ACM"；标题/作者/摘要与正式版一致，但**不是**出版排版版，页码与 ESE 12(4):389–415 不同）：<https://citeseerx.ist.psu.edu/viewdoc/download?doi=10.1.1.141.5362&rep=rep1&type=pdf>

**样本 / 对象：** 78 个开源与闭源 Java 应用（多个版本共 100 个分析对象）；三种依赖关系：`USES`、`USES-IN-THE-INTERFACE`、`USES-IN-SIZE`；以 SCC（强连通分量）大小衡量环。

**主要结果（verbatim）：**

> "In this paper we present the first significant empirical study of cycles among the classes of 78 open- and closed-source Java applications."

> "of the applications comprising enough classes to support such a cycle, around 45% have a cycle involving at least 100 classes and around 10% have a cycle involving at least 1000 classes."

> "For the USES relation about 85% of the applications in the corpus have a SCC of size >10, about 40% have a SCC >100 and 3% of the applications have a SCC >1000. In fact, the largest SCC in this relation is 2145 classes."

**被测结果（outcome）——关键：** 该文测的是 **环的普遍程度（prevalence）** 与 **打破环所需移除的边数（Minimum Edge Feedback Set, mEFS）即"重构负担"**，**不是**缺陷、故障或变更成本。论文把"环是否真的伤害质量"明确写成**未来工作**：

> "Another direction is confirming the extent to which cycles effect quality (e.g., through a controlled experiment) so we can determine what proportion or number of classes involved in cycles we can tolerate in an application."

**易被误引的一句（务必注意其语境）：**

> "Another view is that we do not care about classes involved in cycles if they do not exhibit defects and are not frequently modified."

这是作者在 §7.3 列举的**一种替代观点/待研究问题**，**不是**该文的发现。

该文也把结果与 Big Ball of Mud 联系起来（§7.2），并用一个**轶事**说明两颗产品因代码失控被丢弃（公司 "B" 的 B5、B10）——轶事，不是统计证据。

**强度：对"环很常见、且往往非常大"是强；对"环导致缺陷/维护成本"是 null（未测，作者自认待验证）。** 若报告写"实证研究表明循环依赖导致缺陷"，**用这篇是错的**。可写的是："Melton & Tempero 证明了循环依赖在 Java 中普遍且规模巨大，但明确指出其质量后果仍待实验确认。"



## 证据强度总表（claim → source → strength）

| # | 主张（claim） | 来源 | 样本 / 被测结果 | 证据强度 |
|---|---|---|---|---|
| 1 | 模块化的判据应是"隐藏会变化的**设计决策**"，而非降低被使用次数 | Parnas 1972, §The Criteria / Conclusion | 概念论证（KWIC 案例） | 概念权威（无实证） |
| 2 | 把内部数据格式暴露给所有模块，会使一个变更扩散到**每个模块** | Parnas 1972, §Changeability | 案例推演 | 概念论证（无量化） |
| 3 | 被很多模块使用**不等于**病态；被广泛使用可以是好设计 | Parnas 1972（`used in almost every action by the system`） | — | 概念权威（**反 fan-in 论证**） |
| 4 | 架构会被"侵蚀到认不出来"；病因是 piecemeal growth、throwaway code、信息全局化/复制 | Foote & Yoder 1997 | 模式论文，无测量 | 概念权威（无实证） |
| 5 | Big Ball of Mud **没有**把 fan-in / 依赖环命名为机制 | Foote & Yoder 1997（全文检索） | — | 全文核实（**否定性发现**） |
| 6 | `grinding it to dust` **不在** Big Ball of Mud 中 | Foote & Yoder 1997（全文检索） | — | 全文核实（**否定性发现**） |
| 7 | "architecture erosion = 对架构的违规；drift = 架构失感" | Perry & Wolf 1992, §2.3 | 立场论文，无样本 | 定义性权威（无实证） |
| 8 | 73 篇研究的 SMS 反复声称 AEr 导致缺陷、维护成本上升、技术债 | Li, Liang, Soliman, Avgeriou 2022 (SMS) | 73 篇纳入研究（汇总其**主张**） | 中（汇总性；非独立测量；作者自认实证有限） |
| 9 | propagation cost 可测且不同设计差 3 倍；Mozilla 重设计后从 15–18% 降到 2–6% | MacCormack, Rusnak & Baldwin 2006, MS | 2 个系统（Linux / Mozilla）+ 纵向版本 | 强（对**变更传播成本**）；对缺陷 **null/未测** |
| 10 | 高耦合的 Central/Core 文件承担 62–70% 的 defect-related activity；每行维护成本 3–15× | MacCormack & Sturtevant 2016, JSS | 2 个系统，20,270 / 19,225 文件；outcome = DRA / 是否出缺陷；控制 LOC + 圈复杂度 | **强**（本议题最强），但 n=2、横截面、成本是**相关性** |
| 11 | 依赖图 centrality 与 post-release defects 显著正相关（symmetric degree r=.462） | Zimmermann & Nagappan 2008, ICSE | Windows Server 2003 binaries；outcome = post-release defects | 强（大工业系统） |
| 12 | **ingoing（fan-in）比 outgoing（fan-out）更弱**；体量/复杂度类的相关性还略高（>.50） | Zimmermann & Nagappan 2008, ICSE | 同上（Spearman 表） | **强（对本议题是弱/反向支持）** |
| 13 | 依赖结构组合模型比纯复杂度模型 recall 高 10 个百分点（关键 binary 60% vs 30%） | Zimmermann & Nagappan 2008, ICSE | 同上 | 强（但为**组合**模型，非 fan-in 单独） |
| 14 | "morning after syndrome" 描述的是**依赖环**导致的"每天被别人的改动打断"；ADP 的处方是打破环 | R.C. Martin, *Clean Architecture* (2017) Ch.14 "Component Coupling"（§(c)） | — | 英文原文已逐句核对（转载；出版商版未取得） |
| 18b | Martin 把 **fan-in 定义为"稳定性"**（I=0 = maximally stable）；风险区是"稳定+具体+多变"的 Zone of Pain | R.C. Martin, *Clean Architecture* (2017) Ch.14 SDP 一节（§(c)） | — | 定义性权威（**反 fan-in 论证**） |
| 15 | 循环依赖在 Java 中**非常普遍且规模巨大**（78 个应用中约 45% 有 ≥100 类的环，最大的一个 SCC 达 2145 类） | Melton & Tempero 2007, ESE | 78 个开源/闭源 Java 应用；outcome = 环的普遍程度 + mEFS 重构负担 | **强**（描述性） |
| 16 | 循环依赖**是否**导致缺陷/维护成本 | Melton & Tempero 2007 自述 | — | **null（未测；作者明确列为 future work）** |
| 17 | 所有依赖都提高 fault proneness，但**语法依赖(≈import)解释力最弱**，逻辑依赖最强 | Cataldo, Mockus, Roberts & Herbsleb 2009, TSE | 2 家公司 2 个项目、8 年、154 名开发者；outcome = customer-reported defects | 中（仅摘要级证据；全文未取得） |
| 18 | 提出了度量"模块化质量"的指标并在开源系统上示例验证 | Sarkar, Kak & Rama 2008, TSE | 若干流行开源系统；**无缺陷/成本结果变量** | 中（方法论文）；对"耦合有害" **null（未测）** |
| 19 | 依赖图的 centrality / degree 预测缺陷的能力可与复杂度指标相当，组合后更好 | Zimmermann & Nagappan 2008, ICSE | Windows Server 2003 binaries；outcome = post-release defects | 强（但为**组合**效应） |

---

## 直接回答"该不该把 file-level fan-in 当复杂度"

- **文献支持的是：** 依赖**结构**（尤其是**环**与**间接依赖/传播成本**）与维护成本、缺陷相关；把"结构"纳入质量报告是有依据的。
- **文献不支持的是：** 把"一个文件被 import 的次数"（fan-in）**单独**当作复杂度病态。Parnas 明确把"被广泛使用"当作正常/正面；BBM 的机制里没有 fan-in；ADP 针对环；Zimmermann & Nagappan 显示 fan-in 信号弱于 fan-out，也弱于 LOC/圈复杂度。
- **若一定要 flag fan-in，合理的定位是：** 作为"**变更传播风险 / 架构集中度**"的一个**线索指标**，而不是"复杂度"；并且
  1. 必须与 fan-out / 间接依赖（propagation cost）/ 环检测**一起**看；
  2. 报告口径应说明是直接 fan-in 还是传递闭包后的可见 fan-in；
  3. 阈值/告警需要避免与文件体量（LOC、函数数）重复计数——Zimmermann & Nagappan 的回归正说明体量类指标已经覆盖了大部分信号。

---

## 未能核实 / 缺口（诚实声明）

1. **任务给出的短语 `a module that is used by many others` 在 Parnas 1972 原文中不存在。** 我对 CACM PDF 全文与 PKU HTML 版都做了检索。可能是后人对其思想的转述，或来自其 1971 技术报告 / 后续论文；如需精确归属，需另查。
2. **`grinding it to dust` 不在 Big Ball of Mud 中**（全文检索）。请勿在该文名下引用此短语。
3. MacCormack, Rusnak & Baldwin (2006) 正文中**没有**缺陷/成本回归；相关内容明确是 "ongoing work"。
4. **Melton & Tempero (2007) 全文已取得**（CiteseerX 作者存档稿，见 §(d9)）；**Sarkar, Kak & Rama (2008) 与 Cataldo et al. (2009) 全文未取得**（IEEE 非 OA），只能用**官方摘要**，已在文中明确标注。
5. **Baldwin, MacCormack & Rusnak (2014, Research Policy)** 全文未取得，仅有官方工具文档的转述。
6. Perry & Wolf (1992) 的 ACM DL PDF 返回 403；本次使用的是作者自托管的同文 PDF（UT Austin），页码与 SIGSOFT SE Notes 17(4) 一致。
7. **Martin 2017 的出版商正文未取得**：§(c) 的英文引语来自公开双语对照转载（URL 已给出），并已与另一份独立英文摘要页交叉核对。**2002 年版正文未取得**，故只能确认 2002 年 Ch.20 的章节结构（由出版社目录 PDF 证实），无法确认该短语本身是否已出现在 2002 年版。
8. **未系统排查早于 Martin 的用法**（如 Lakos 1996 *Large-Scale C++ Software Design*）；"morning after syndrome" 是否 Martin 首创仍属"未发现更早来源"，而非"已证明 Martin 首创"。

---

## 参考文献（可直接粘进中文报告）

1. Parnas, D. L. (1972). On the Criteria To Be Used in Decomposing Systems into Modules. *Communications of the ACM*, 15(12), 1053–1058. https://doi.org/10.1145/361598.361623
2. Foote, B., & Yoder, J. (1997). Big Ball of Mud. *Fourth Conference on Pattern Languages of Programs (PLoP '97/EuroPLoP '97)*, Monticello, Illinois. https://www.laputan.org/mud/
3. Perry, D. E., & Wolf, A. L. (1992). Foundations for the Study of Software Architecture. *ACM SIGSOFT Software Engineering Notes*, 17(4), 40–52. https://doi.org/10.1145/141874.141884
4. Li, R., Liang, P., Soliman, M., & Avgeriou, P. (2022). Understanding software architecture erosion: A systematic mapping study. *Journal of Software: Evolution and Process*, 34(3), e2423. https://doi.org/10.1002/smr.2423
5. MacCormack, A., Rusnak, J., & Baldwin, C. Y. (2006). Exploring the Structure of Complex Software Designs: An Empirical Study of Open Source and Proprietary Code. *Management Science*, 52(7), 1015–1030. https://doi.org/10.1287/mnsc.1060.0552
6. MacCormack, A., & Sturtevant, D. J. (2016). Technical debt and system architecture: The impact of coupling on defect-related activity. *Journal of Systems and Software*, 120, 170–182. https://doi.org/10.1016/j.jss.2016.06.007
7. Zimmermann, T., & Nagappan, N. (2008). Predicting defects using network analysis on dependency graphs. *ICSE '08*, 531–540. https://doi.org/10.1145/1368088.1368161
8. Baldwin, C., MacCormack, A., & Rusnak, J. (2014). Hidden structure: Using network methods to map system architecture. *Research Policy*, 43(8), 1381–1397. https://doi.org/10.1016/j.respol.2014.05.004
9. Martin, R. C. (2017). *Clean Architecture: A Craftsman's Guide to Software Structure and Design*. Prentice Hall. Chapter 14, "Component Coupling"（ADP / SDP / SAP；"morning after syndrome"）。
10. Martin, R. C. (2002). *Agile Software Development: Principles, Patterns, and Practices*. Prentice Hall. Chapter 20, "Principles of Package Design"（pp.253–263；ADP 见 p.256）。
11. Melton, H., & Tempero, E. (2007). An empirical study of cycles among classes in Java. *Empirical Software Engineering*, 12(4), 389–415. https://doi.org/10.1007/s10664-006-9033-1
12. Cataldo, M., Mockus, A., Roberts, J. A., & Herbsleb, J. D. (2009). Software Dependencies, Work Dependencies, and Their Impact on Failures. *IEEE Transactions on Software Engineering*, 35(6), 864–878. https://doi.org/10.1109/TSE.2009.42
13. Sarkar, S., Kak, A. C., & Rama, G. M. (2008). Metrics for Measuring the Quality of Modularization of Large-Scale Object-Oriented Software. *IEEE Transactions on Software Engineering*, 34(5), 700–720. https://doi.org/10.1109/TSE.2008.43
