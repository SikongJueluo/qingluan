# Research: 函数长度与文件长度作为复杂度轴

> 调研日期：2026-09-30 · 目标：决定是否把「函数长度」「文件长度」纳入 `qingluan complexity`，
> 阈值取多少，以及长度该不该折进复杂度分数。
> 方法：三个并行子代理各自核对一手来源（规则源码/规则文档/原始论文），加上一次本地实测——
> 用 qingluan 自己的引擎扫本机 28 个仓库（Rust/Java/Python/TS 混合，18573 个一阶函数、2073 个一阶文件）。
> 子代理的完整报告（含逐条 URL 与引文）在
> `.scratch/complexity/research/length-function.md`、`length-file.md`、`length-contribution.md`；
> 本文是综述，只保留结论与关键引文。本地实测方法与数据见附录 A。

## TL;DR

1. **纳入，但只作为独立轴，绝不折进 cc/cognitive。** 本地实测 `nloc` 与 cognitive 的 Spearman
   只有 **0.567**；最大规模的一手测量（Landman et al. 2016，1763 万个 Java 方法 + 626 万个 C 函数）
   给出 R² = 0.40/0.44，而且**在最长的函数里掉到 0.08**——长而平与短而绕同时存在，正是要分类处理的尾部。
   更硬的一条：McCabe 自己的手册 NIST SP 500-235 §3.1 标题就叫「复杂度与尺寸的独立性」，
   原文「it is independent of complexity and should not be used for the same purposes」。
2. **阈值：函数 `nloc > 100`、文件 `nloc > 1000`。两个都是约定，不是实证发现。**
   - 100 的依据是**同口径共识**：Clippy `too_many_lines` 默认 100，而 Clippy 数的就是
     「非空非注释行」——与我们的 `nloc` 完全同口径；Sonar S138 有 6 种语言的默认也是 100
     （口径近似）；PMD 6 的 `ExcessiveMethodLength` 也是 100（该规则已在 7.0 删除）。
   - 1000 的依据：Sonar S104 在 8/10 语言里的默认（Java/Go 例外，是 750）、pylint `too-many-lines` 1000。
   - 本地命中率：函数 160/18573 = **0.86%**；文件 25/2073 = **1.21%**，与现有 cc>10（486）、
     cognitive>15（412）同一量级。
3. **两处常见的错误说法，本文初稿都误用过，已纠正**：
   - **「Sonar S138 默认 80」是传说**：80 不在任何一套规则集里（§1.1）。
   - **不能笼统说「公开默认都是物理行，除以 1.336 换算即可」**：口径按工具不同——Clippy 与
     Sonar 的 Java/JS 本来就数代码行，ESLint/Checkstyle/lizard 才数物理行（§1.1）。
4. **长度不进分数，进「违规集合」。** `--threshold` = 三条规则的**并集**：函数命中数 561 → 613
   （+9.3%；只算生产代码 477 → 499，+4.6%），多出来的全是长度独有（52 个，其中生产代码 22 个）。
5. **文件长度单独一张表**（分布 + 最长文件 top-K）。它是**模块组织**信号而不是复杂度信号：
   >1000 的 25 个文件里 80% 已被函数指标命中，剩下 5 个函数全都正常（最长的一个 7587 行、
   **331 个函数**）。
6. **不做复合分数**：查遍 MI / SIG / CodeScene / Sonar 四家，**没有一家的权重是对可复现结果回归出来的**
   （§4）。`cc × nloc` 只会比它们更没依据。

## 1. 函数长度：一手来源的阈值对照

### 1.1 单位不一致是这里最大的坑

| 工具 | 默认 | 口径（关键差异） |
| --- | --- | --- |
| **Clippy `too_many_lines`** | **100** | **代码行：非空、非注释——与我们的 `nloc` 完全同口径**（`clippy_config/src/conf.rs`） |
| Sonar S138 | Java **75**；JS/TS **200**；Python/Kotlin/Swift/Scala/C/C++ **100**；Go 120；PHP 150 | 「ncloc 类」：跳过空行与整行注释。Java 取自 `MethodTooBigCheck` 的 `DEFAULT_MAX=75`；JS 还额外跳过 IIFE 与 React 组件；Python 数 token 行再去掉 docstring |
| ESLint `max-lines-per-function` | **50** | 物理行，`skipBlankLines`/`skipComments` 默认 `false`（含空行注释） |
| Checkstyle `MethodLength` | **150** | 物理行，`countEmpty=true` 时从 `{` 到 `}` 全算 |
| PMD `ExcessiveMethodLength` | 100 | LoC。**6.53 弃用、7.0 删除**，官方理由：「Enforcing length limits with LoC is not very meaningful, could even be called a bad practice」；替代品 `NcssCount` 的 methodReportLevel = **60**（数语句不数行） |
| RuboCop `Metrics/MethodLength` | **10** | 代码行；其 `default.yml` 自述「被三分之一项目直接弃用，保留者对其上限意见不一」 |
| funlen（Go） | 60 行 / 40 语句 | 理由明写「fit a function within one screen」 |
| lizard `-L` | 1000 | 物理跨度 `end_line-start_line+1`（README 未给理由） |
| Power of 10 Rule 4（Holzmann, IEEE Computer 2006） | 60 | 「no more than about 60 lines of code per function」，少数带理由的硬规则 |

**80 不在这个表里，因为它不在任何一套规则集里。** 两处实查可佐证：Sonar 的分语言默认见上表
（SonarQube 公共 API `/api/rules/show?key=<lang>:S138` 逐个语言查得），Clippy 的 100 在
`clippy_config` 源码里。凡是写「Sonar 建议 80 行」的文章，都找不到一手出处。

口径差异直接决定选哪个数：100 之所以可辩护，不是因为「大家都在用 100」，
而是因为 **Clippy 的 100 和我们数的是同一种行**（顺带 Sonar 的 100 组、PMD6 的 100 下限口径也接近）。
ESLint 的 50 与 Checkstyle 的 150 数的是物理行，**不能直接搬**。

### 1.2 风格指南都是软建议（一手引文）

| 来源 | 说了什么 |
| --- | --- |
| Google C++ Style / Google Python Style §3.18 | 逐字相同：「no hard limit is placed on … length. If a function exceeds about 40 lines, think about whether it can be broken up…」——软建议，且数的是物理行 |
| Google Java Style | 只字未提函数长度 |
| PEP 8 | 不提函数长度（79 是行宽，不是函数长度） |
| Linux `coding-style.rst` §6 | 「one or two screenfuls of text (the ISO/ANSI screen size is 80x24)」并说「maximum length … is inversely proportional to the complexity and indentation level」；`checkpatch.pl` **没有**函数长度检查 |
| C++ Core Guidelines F.3 | 只说「Keep functions short and simple」，不给数字 |

结论：**没有任何实证研究给出最优函数长度**；唯一的硬数字来自工具与工程约定。
除 Power of 10 的 60 行（理由是「一屏可读」）以外，所有数字都没写理由。

### 1.3 ESLint 官方给「长度必须独立成轴」的一手论证

ESLint `max-lines-per-function` 文档里有一节 *"Why not use `max-statements` or other complexity
measurement rules instead?"*：一段 16 行的嵌套调用链（`m("div", [...])` 套 `m("table"...)`），
`max-statements` 报 1、`complexity` 报 1、`max-nested-callbacks` 报 1、`max-depth` 报 0，
而 `max-lines-per-function` 报 16 行。<https://eslint.org/docs/latest/rules/max-lines-per-function>

这是复杂度规则维护者自己写下的「行数能看见、复杂度看不见」，与下面 §3.3 的统计结论（长函数尾部
两者分离）是同一个事实的两种表述。

## 2. 文件长度：一手来源的阈值对照

| 工具 | 默认 | 口径 |
| --- | --- | --- |
| Sonar S104 | Java **750**、Go **750**；JS/TS、Python、PHP、Kotlin、Swift、Scala、C/C++、C#/VB.NET **1000** | 多数语言数「ncloc 类」行；**Python 例外**，数最后一个 token 的行号（物理行） |
| pylint `too-many-lines` | 1000 | 物理行 |
| SwiftLint `file_length` | 400 警告 / 1000 错误 | 物理行（`ignore_comment_only_lines` 默认 false） |
| ESLint `max-lines` | 300 | 物理行含空行注释；**该规则默认关闭**，300 只是「启用时」的默认 |
| Checkstyle `FileLength` | 2000 | 物理行，**没有** `countEmpty` 选项（`countEmpty` 是 `MethodLength` 的） |
| revive `file-length-limit` | 0（关闭） | — |
| PMD | 无（`ExcessiveClassLength` 已于 6.x 删除） | — |

两条必须一起记住的警告：

1. **Google 的「100–1000 行」说的是 CL / 变更大小，不是文件大小。** 变更大小与评审难度的
   证据链是另一个变量，不能拿来给文件长度定阈值。
2. **PMD 删除长度规则时的官方理由**同样是这里最强的反方证据：用 LoC 强制长度
   「not very meaningful, could even be called a bad practice」。

所以文件长度这一轴更弱：**没有一家给出理由**，而且主流工具的态度是「要么关闭（ESLint 默认关、
revive 默认 0），要么只排名不拦截（cloc/tokei/lizard 输出表格）」。

文献对「文件大小 → 缺陷」的**方向**也没有共识（子代理逐篇核对了六篇，方向互相矛盾）：

| 研究 | 方向 |
| --- | --- |
| Koru et al., IEEE TSE 2009 | 单调递减（越大密度越低） |
| Hatton, IEEE Software 1997 | U 形，拐点 200–400 **LOC**（已被作者本人撤回） |
| Syer et al., IEEE TSE 2015 | 倒 U 形（Koru 的复现**没能泛化**） |
| Fenton & Ohlsson, IEEE TSE 2000 | **强零结果**：「It is not the case that size explains in any significant way the number of faults」 |
| Sjøberg et al., IEEE TSE 2013 | 文件 LOC 与**维护工作量**相关（ρ = 0.37–0.61, p<0.01），与变更次数一起「explained almost all of the modeled variation in effort」 |

唯一一致的两点是：**长度和内部复杂度在文件级高度共线**（Fenton & Ohlsson：CC 与 LOC
「a good linear correlation」——又一次印证 §3.3 的聚合效应），以及**测得出来的正相关是
「工作量」而不是「缺陷」**。这两点合起来只支持一件事：**把文件长度展示出来**，而不是给它定一个闸门。

### 2.1 「一次评审不超过 200–400 行」这条流行规则不能引用

它是 **SmartBear 2006 年的一份厂商白皮书**（Cisco Systems 的评审数据），**非同行评审**，
而且自相矛盾：同一份材料里「缺陷密度随尺寸下降」与「缺陷率基本持平」并存。
同行评审的补丁大小研究结论普遍**弱或零**：Baysal et al. EMSE 2015 的 r = 0.09/0.05（近零）、
di Biase et al. PeerJ CS 2018 的对照实验在「发现缺陷数」上是**零结果**、
Kononenko et al. ICSME 2015 的调整后 R² 只有 0.12–0.17、
Kemerer & Paulk TSE 2009 说的是**评审速率**（≤200 LOC/**小时**），不是补丁大小上限。
唯一稳健的「文件数」效应是 Bosu et al. MSR 2015（是**文件个数**，不是行数）。

**明确记录一个空白**：没有干净的同行评审研究直接测「文件长度 → 评审/导航难度」。
所以「超过 N 行的文件更难评审」这句话**不能当成已确立的事实**来写进帮助文本——
文件长度在我们这里只是「值得看一眼」的排序信号。

## 3. 长度和复杂度是不是一件事（这一节决定要不要单独成轴）

### 3.1 本地实测：高度相关，但远非同一件事

ρ(nloc, cognitive) = 0.567、ρ(nloc, cc) = 0.564（§A.3）；「长而平」97 个 vs「短而绕」193 个；
按两种指标排的前 100 只重合 48 个；长度独有的命中 52 个（阈值 100，其中生产代码 22 个）。
如果长度只是复杂度的影子，这些数应该接近 1.0 / 接近全重合——它们不是。

### 3.2 一手口径：McCabe 自己的手册说「尺寸与复杂度相互独立」

NIST SP 500-235（Watson & McCabe 1996）§3.1 的标题就叫 **"Independence of complexity and size"**：

> "Thus, although the number of lines of code is an important size measure, it is independent of
> complexity and should not be used for the same purposes."
>
> "Therefore, the common practice of attempting to limit complexity by controlling only how many
> lines a module will occupy is entirely inadequate."

同一手册还明确否掉了「用尺寸归一化」的变体：可以把一个 complexity 90 的模块「加上一个什么都不做的
十分支 switch」变成 modified complexity 10，因此构造 modified 度量 *"is not recommended"*。
<https://www.mccabe.com/pdf/mccabe-nist235r.pdf>

**这是「向量而非复合分」最硬的一手依据**，而且来自 McCabe 自己的手册，不是我们的发明：
尺寸与复杂度**独立**、**不应用于同一目的**；混合/归一化的变体**可被游戏**。

### 3.3 大规模实测：函数级相关只有中等，且在最长的函数里崩掉

Landman, Serebrenik, Bouwers & Vinju 2016, *J. Softw. Evol. Process* 28(7):589–618,
DOI [10.1002/smr.1760](https://doi.org/10.1002/smr.1760)：**1763 万个 Java 方法 + 626 万个 C 函数**。

| 统计量（函数级） | Java | C |
| --- | --- | --- |
| R²(SLOC, CC) 全体 | 0.40 | 0.44 |
| Spearman ρ | 0.80 | 0.83 |
| R²（对数变换后） | 0.68 | 0.71 |
| R² 在 SLOC 最大的 10% / 1% / 0.01% | 0.30 / 0.21 / **0.08** | 0.36 / 0.28 / 0.12 |
| R² 按**文件**聚合后 | 0.64 | 0.39 |
| R² 按方法求和后 | 0.73 | 0.70 |

> "not strong enough to conclude that CC is redundant with SLOC"
>
> "CC summed over larger code units measures an aspect of system size rather than internal
> complexity of subroutines. This largely explains the often reported strong correlation between
> CC and SLOC in literature."

四条对我们直接有用的结论：

1. **「长度 ≈ 复杂度」的印象主要来自聚合**：按文件/类求和后 R² 从 0.40 跳到 0.64–0.73。
   我们只报函数级、且**不把 cc 求和成文件分**，正好站在有利的一侧。
2. **在最长的函数里两者分开**（R² 掉到 0.08）：长而平与短而绕同时存在，而这正是复杂度报告
   最想分类处理的那批代码。
3. **小/典型函数里两者接近共线**，所以增量集中在尾部——这也是本地「生产代码只多 22 个命中」的原因。
4. **绝大多数函数根本没有分支，而它们的长度差异极大**：Landman Table III —— Java **65% 的方法
   （1160 万个）CC = 1**，其 SLOC 却跨四个数量级（中位 3 行，最大 33850 行）。把这批函数按长度
   重排用的是**分支指标在结构上不可能包含**的信息；反过来也说明「长」本身不等于「绕」。

（反例：Jay et al. 2009 声称 CC「absolutely no explanatory power of its own」，
DOI [10.4236/jsea.2009.23020](https://doi.org/10.4236/jsea.2009.23020)，但那是**文件级**测量且发表在
低声誉的 SCIRP 期刊；文件级聚合恰是上表里把 R² 抬到 0.64+ 的那个操作。列为弱证据。
另有一份用**我们同款 nloc 口径**的新研究：Chin & Holmes, ICPC 2026，60.4 万个方法，
结论是 Length 是理解类度量的混淆变量——支持「长度 ≠ 复杂度」。）

### 3.4 类级别「尺寸混淆」的著名结论，不能直接搬到函数级

El Emam, Benlarbi, Goel & Rai 2001, IEEE TSE 27(7):630–650,
DOI [10.1109/32.935855](https://doi.org/10.1109/32.935855)。摘要原文：

> "After controlling for size none of the metrics we studied were associated with fault-proneness
> anymore. This demonstrates a strong size confounding effect…"

必须连同三条边界一起引用，否则就是过度推论：

1. 单位是**类**（class size），不是函数；
2. 被检验的是 Chidamber & Kemerer / Lorenz & Kidd 那批**别的**度量，不是「函数 nloc vs 函数 cc」；
3. 它**被公开质疑过**：Briand et al. 2003, IEEE TSE, DOI [10.1109/TSE.2003.1214331](https://doi.org/10.1109/TSE.2003.1214331)
   （「尺寸可测并不先于 OO 度量可测」，因果前提不成立）；而后续 TOSEM 2014 研究
   （DOI [10.1145/2556777](https://doi.org/10.1145/2556777)）在开源系统上仍复现出该混淆存在。

对 qingluan 的正确教训是：**不要让聚合后的尺寸冒充复杂度**——而不是「函数长度没意义」。

**但这套「尺寸混淆」不是对所有度量都成立。** Tahir et al., ESEM 2018,
DOI [10.1145/3239235.3239243](https://doi.org/10.1145/3239235.3239243)（标题已核实）发现尺寸
对 RFC / CBO / LCOM / Fan-in / Fan-out 是「fully mediates」，**而 WMC 是例外——它保留了直接效应**。
WMC（加权方法复杂度）与 cc 属同一族。所以：**复杂度族度量不是纯尺寸代理**，这也与 §3.3
（函数级 R² 只有 0.40）互相印证。

### 3.5 尺寸与缺陷：方向成立，具体形状不成立

| 论文 | 一手来源说了什么 | 状态 |
| --- | --- | --- |
| Basili & Perricone 1984, CACM 27(1):42–52, DOI [10.1145/69605.2085](https://doi.org/10.1145/69605.2085) | 517 个 Fortran 模块：单位行错误率随尺寸**下降**（50 行 → 16.0 errors/1000 行；>200 行 → 6.4）。同一篇里模块平均 CC 随尺寸陡增（50 行 6.0 → >200 行 60.0）——**尺寸与复杂度在他们自己的数据里就没分开** | 成立但常被误读：流传的 R²=0.94 来自「按尺寸分五桶后的聚合点」，不是模块本身 |
| Hatton 1997, IEEE Software 14(2):89–97, DOI [10.1109/52.582978](https://doi.org/10.1109/52.582978) | 缺陷密度随尺寸呈 U 形。注意：他讲的是 **component（约 200–400 行）**，不是函数 | **作者 2009 年在自己论文页面上撤回**：「I no longer believe in the U-bend described in this paper」<https://www.leshatton.org/IEEE_Soft_97b.html> |
| Nagappan & Ball 2005, ICSE, DOI [10.1145/1062455.1062514](https://doi.org/10.1145/1062455.1062514) | 「absolute measures of code churn are poor predictors of defect density, our set of relative measures… is highly predictive」 | 成立（单一工业系统，外部效度窄）。**含义：尺寸适合当分母/轴，不适合当量级** |

**「尺寸重要」有证据，「最优 N 行」没有。** 阈值只能是约定——ESLint 官方文档在 `max-lines` 里
直接承认「there is not an objective maximum number of lines considered acceptable in a file」。
这一点与 `code-complexity-metrics.md` 对 CC 阈值 10 的处理一致（那里也标注了 McCabe 的原话
「a reasonable, but not magical, upper limit」）。

### 3.6 白皮书里其实**没有**「size」这个词

流行的转述是「Sonar 说 cognitive complexity 不是 size metric」。**白皮书 v1.7 全文里
"size" 零命中，"not a size metric" 不是原话**（子代理逐页核对，本文不再引用这句话）。
真正相关的是三段：

- p4 Introduction：批评 CC 在方法级以上失效——「the Cyclomatic Complexity scores of applications
  correlate to their lines of code totals. In other words, Cyclomatic Complexity is of little use
  above the method level.」
- p6 Ignore shorthand：「Cognitive Complexity does not increment for methods … they allow
  short-handing multiple lines of code into one.」
- Appendix B（规范正文）通篇没有任何长度项。

所以「cognitive complexity 忽略长度」是**设计属性**（它忽略直线代码与方法抽取），
**不是一条「不要设长度阈值」的论证**——Sonar 自己就用 S138 单独管长度。
把它当成反长度限制的依据是误读；本文之前的草稿也这么误读过，已改。

### 3.7 直接测「读起来有多难」的最新证据

Thorgeirsson & Vahrenhold, ICER 2026, DOI [10.1145/3765964.3811665](https://doi.org/10.1145/3765964.3811665)
（预注册，N=551）：零阶相关里 **SLOC 预测实测认知负荷比 CC 更好**（r ≈ 0.41/0.39 vs 0.26/0.27）；
多变量模型里 SLOC 仍是主预测子，CC 翻转成小的负系数（抑制效应）。
（系数由子代理转述，未逐条复算；方向可用，数值当参考。）

### 3.8 反方证据：文件级上「代码坏味道」加不出尺寸之外的信息

这一条是反着来的，必须放在这里，否则本文会显得只挑了支持自己的证据。

Sjøberg, Yamashita, Anda, Mockus & Dybå, *Quantifying the Effect of Code Smells on Maintenance
Effort*, IEEE TSE 39(3), 2013, DOI [10.1109/TSE.2012.89](https://doi.org/10.1109/TSE.2012.89)：

> "None of the 12 investigated smells was significantly associated with increased effort after we
> adjusted for file size and the number of changes."
>
> "a single predictor of file size achieves a better fit than all of the smell predictors."

其中 God Method（长方法坏味道）不显著（β = −.32, p = .18），文件大小显著（β = .58）；
粒度是**文件/类**，结局是**维护工作量**，不是函数级缺陷。

怎么读它对我们有利也有不利：

- **不利于**「长函数 = 坏」的简单叙事：在控制文件大小后，长方法本身没有独立的工作量效应。
  所以长度阈值只能是**导航/审查辅助**，不能声称「超过 100 行就会更贵」。
- **有利于**「长度要显眼且单列」：在他们这份数据里，**文件大小是唯一稳的预测子**，
  结构性的包装指标在它之上加不出信息。也就是说长度不是要被折叠掉的噪声，而是最基础的那一维。
- 配套的相反证据见 Tahir et al.（§3.4）：WMC 是尺寸混淆的例外，所以复杂度族度量也没被证伪。

净结论：**两条轴都留着，但都不要过度声称**——这正是「并集 + 标出命中轴」而不是
「一个总分 + 一个闸门」的又一个理由。

### 3.9 长度是重尾分布，阈值天然是离群点判定

Hatton & Warr, *Entropy* 27(6):561, 2025, DOI [10.3390/e27060561](https://doi.org/10.3390/e27060561)
（标题与 DOI 已核实）：函数长度服从幂律（β = −1.52，调整后 R² = 0.99）。
本地数据同向：函数 nloc 中位 7 行、p99 94 行、最大 1132 行。
所以长度阈值**不可能**是「最优值」，只能是「离群点」——这也解释了为什么所有工具的默认值
都差得那么远（10 到 1000），以及为什么我们坚持把 p50/p90/p99 分布跟阈值一起打印。

## 4. 为什么不做复合分数（这次把每个复合分的权重来源查清了）

| 模型 | 公式 / 聚合方式 | 权重怎么定的 | 验证状态 |
| --- | --- | --- | --- |
| **MI**（Oman & Hagemeister 1992 / Coleman et al. 1994 / SEI） | `171 − 5.2·lnV − 0.23·G − 16.2·lnL`（SEI/radon 版另加 `50·sin√(2.4C)`） | **对 HP 工程师的主观评分做多项式回归**（AFOTEC 量表，16 个系统）：Coleman 原文「the models were again calibrated to HP engineers' subjective evaluation」 | radon 自称 "still a very experimental metric"，并推荐 van Deursen 的 *Think Twice Before Using the Maintainability Index* |
| **MI（微软归一化）** | `MAX(0, MI·100/171)`，0–9 / 10–19 / 20–100 | 明说是为了降噪的保守选择：「we decided to be conservative with the thresholds… to keep the noise level low」 | 厂商自定，无公开验证 |
| **SIG maintainability**（Heitlager, Kuipers & Visser 2007, DOI [10.1109/QUATIC.2007.8](https://doi.org/10.1109/QUATIC.2007.8)） | 8 个度量 → 1–5 星 → ISO/IEC 25010 子特性 → 单一星级 | **人群百分位定标**：「chosen such that about 5% of the software applications… receive a 5-star rating」，**每年重标定** | 作者方内部验证，未见独立复现 |
| **CodeScene Code Health**（Tornhill & Borg 2022, DOI [10.1145/3524843.3528091](https://doi.org/10.1145/3524843.3528091)） | 25+ biomarker → 10…1 分（文件级按 LoC 加权平均聚合） | **不公开，且来源是内部手工评分**：Code Red §2.1 说各因子的 cutoff「were decided by their internal team via a baseline library of hand-scored code examples」；厂商自己的两篇论文里「健康」分界线还不一致（Code Red 8.0 vs ICSME 2024 的 9） | Code Red：低健康度代码缺陷多 15 倍、开发时间 +124%；但 39 个代码库全部来自 CodeScene 用户（作者自承外部效度威胁）。更要命的是厂商自己后来的 benchmark（Ghost Echoes, ICSME 2024）：Code Health 的文件级 AUC **0.95**，而**朴素的 LoC 基线也是 0.95**（ML 0.97）——25+ 因子的复合分没能跑赢「只数行数」 |
| **SonarQube 可维护性评级 / 技术债** | `债比 = 技术债 / (每行成本 × ncloc)`，默认 30 分钟/行；评级带 A ≤5% … E ≥50% | **每条规则的修复分钟数是专家设定**，评级带是固定常数 | 工程上广泛使用，未见对分钟数/带宽的公开标定 |

六条结论：

1. **没有一个复合分的权重是「对可复现结果做回归」得到的**：一条是对 16 个系统的主观评分做回归，
   一条是人群百分位，一条不公开，一条是专家分钟数。我们要发明一个权重，只会比这些更没依据。
2. **单位不可通约，而且可被游戏。** §1.3 的 ESLint 例子里同一段代码在四个指标下读数是
   1 / 1 / 1 / 0 / 16 行；§3.2 的 NIST 例子更直接——加一个什么都不做的十分支 switch 就能把
   「modified complexity 90」变成 10。
3. **连最商业化的复合分也是「先分开算、再聚合」**：CodeScene 把 **Large Method**（行数）和
   **Complex Method**（分支）当成两个独立 biomarker，从不把行数乘进复杂度。
4. **SonarQube 给的是四个独立评级**（security / reliability / maintainability / security review），
   不是一个大分；它唯一让长度参与复合的地方是**分母**（债密度 = 债 / nloc）——长度用来
   **归一化**，从不被**加进去**。它的修复分钟数也不是标定出来的：是 Trivial/Easy/Medium/Major/High/Complex
   六个定性档位映射到「每语言一张固定分钟表」（其余语言 5/10/20/60/180/1 天）。
5. **软件度量学界的主流建议就是「向量」而不是「标量」。** Kitchenham, Pfleeger & Fenton,
   IEEE TSE 1997, DOI [10.1109/32.489070](https://doi.org/10.1109/32.489070)：
   > "unless we have some concept of system volume that allows us to combine the dimensions in a
   > single measure. As yet we are not aware of any such concept."

   同一作者组 1995 年的配套文章给出的建议是「measurement vectors rather than artificially contrived
   scalars」。这是教科书作者对「不要人造标量」的直接表态（引文由子代理提取，DOI 已核对）。
6. **把度量求和聚合本身就会破坏信号。** Zhang, Hassan, McIntosh & Zou, IEEE TSE 2017,
   DOI [10.1109/TSE.2016.2599161](https://doi.org/10.1109/TSE.2016.2599161)：求和的聚合方式
   「significantly alter[s] correlations among metrics, as well as the correlations between metrics
   and the defect count」。这正好印证 §3.3 里 Landman 的结论——**所以我们不把函数 cc 求和成文件分**。

顺带一条「不发明分数」的同业观察：`clang-tidy` 的 `readability-function-cognitive-complexity`
是**每条检查一个诊断**、各自有 `--warnings-as-errors`，全套工具里没有任何聚合分
（默认阈值 25，已一手核实）。

补一条「本仓已经拒过一次」：`code-complexity-metrics.md` §3 已记录对 Halstead / MI 的效度质疑
（Curtis et al. 1979、Lassez et al. 1983、Shepperd 1988）。本次调研没有推翻它，只给出了补一条轴的理由。

## 5. 建议落地方案

### 5.1 阈值

| 轴 | 默认 | 依据 | 本地命中率 |
| --- | --- | --- | --- |
| 函数长度 `nloc` | **> 100** | Clippy `too_many_lines` 同口径默认；Sonar S138 的 100 组（Python/Kotlin/Swift/Scala/C/C++）；PMD6 下限 | 160 / 18573 = **0.86%** |
| 文件长度 `nloc` | **> 1000** | Sonar S104 在 8/10 语言的默认；pylint `too-many-lines` | 25 / 2073 = **1.21%** |

为什么不是别的数：

- **函数 60（Power of 10 Rule 4）**是唯一带理由的硬数字（「一屏」），但那是航天嵌入式安全关键
  代码的规范；换算到本地会点名更多函数，对通用仓库偏严。可作为将来的 `strict` 预设。
- **函数 50（ESLint）**数物理行，且是**编辑器里即时提示**的默认——本地同口径（代码行）会到 5%+，
  对 review 队列太吵。
- **函数 75（Sonar Java）/ 200（Sonar JS/TS）**：75 对非 Java 语言偏严，200 只报极端情况
  （本地 0.22%）。取 100 落在中间，且是**同口径**的众数。
- **文件 750**：那是 Sonar 给 Java/Go 的（且是 ncloc 类口径），不是众数；本地 2.32%。
  **文件 1000** 才是 Sonar 8/10 语言的默认、pylint 的默认，本地 1.21%。
- 文件这一轴本来就没有依据（§2），所以它**不参与默认的 `--threshold` 判定**：默认只报分布与
  「最长文件」榜，阈值可配、且标注为约定（见 §5.2）。

语言偏差（函数 nloc > 100 的比例，本地）：Java 0.41%、Python 1.12%、Rust 1.42%、TypeScript 2.11%。
统一阈值会让 Java 侧几乎无声——这与 Sonar 给 Java 更严默认（75）是同一个观察的反面。
**仍建议统一**：跨语言可比、少一个配置维度，用 `[complexity]` 按仓库覆盖即可；
不做 per-language 默认表（Sonar 做是因为它每语言一套独立实现，我们是一套内核）。

### 5.2 输出契约怎么改

- 函数分布行从两行变三行：`cognitive` / `cc` / `nloc`（各自带阈值计数）。
- **函数** `--threshold` 取三条规则的**并集**（cc>10 ∪ cognitive>15 ∪ nloc>100）；
  表格加一列标出命中了哪些轴（如 `cc,len`），让「为什么它上榜」可读，而不是一个黑箱分数。
- **文件**单独一段（`--files`）：`file nloc / functions / worst cog / worst cc`，
  默认按 nloc 排名（top-K），不做阈值拦截——这是 cloc/tokei/lizard 的做法，
  也符合「没有依据就不要装成闸门」。
- JSON：`distribution` 增 `nloc`，新增顶层 `files` 数组；`schemaVersion` 升到 2。
- 可选：`--sort flags`（命中轴数降序）——**序数**（0–3 个独立违规），不是加权和。

### 5.3 明确不做的

- 不做 `cc × nloc`、`cc + nloc/50` 这类复合；不引入 MI / SIG / Code Health 式加权总分（§4）。
- **不做派生比率（如 `cc / nloc` 决策密度）作为默认输出或闸门。** 纠正一句：这个比率**不是**我们
  发明的——Gill & Kemerer 1991（IEEE TSE, DOI [10.1109/32.106988](https://doi.org/10.1109/32.106988)）
  就叫它 *cyclomatic density*（CC / NCSLOC），明说「The intent is to factor out the size component
  of complexity」，并报告它与维护生产率显著相关；McCabe 手册附录也讨论过同类诊断。但它**丢弃长度信息**
  （400 行的平函数与 40 行的平函数密度一样低，问题却完全不同），而那正是我们要保留的轴。
  结论：可以做成**可选的展示列**（`--density`，明确标注为派生值），不作为判定依据、不进并集。
- 不把文件长度混进函数 top-K。
- **不把函数 cc 求和成文件/类复杂度**：Landman 的结论正是「求和后它就变成尺寸度量」（§3.3）。

### 5.4 行为变化与遗留

- 默认 `--threshold` 的输出会变（函数命中 561 → 613），属于可见的行为变化，要写进 spec 并在
  issue 里注明；`--json` 加字段是向后兼容的。
- 测试代码不单独豁免：函数长度分布上测试与生产几乎一致（p90 31 vs 29，§A.5），
  长度独有的命中在测试里占比更高是符合预期的（表驱动测试本来就长）。
- 顺带修一个默认排除的漏洞：`EXCLUDED_DIRS` 只有 `third_party`，本机 `MG-Nav/third-party/`
  被完整扫进来（habitat-lab 的 vendored 代码）。补 `third-party`。
- 未纳入的候选：注释密度、最大缩进宽度、单行长度（那是格式问题，不是复杂度）。
- **第二阶段（diff 增量守门）若引入补丁大小信号**：那是**另一条轴**（变更大小，不是文件长度），
  证据基础比文件长度好一点但仍属「弱到中等」，所以同样只报分布、不拿 200/400 当闸门
  （§2.1：那个数字来自厂商白皮书，且其同行评审复现多为零结果）。

## 附录 A：本地实测

### A.1 方法

用本仓刚实现（未修改）的引擎扫本机 28 个仓库：Rust / Java / Python / TypeScript 混合，
排除 `third-party` / `third_party` / `node_modules` / `site-packages` / `vendor` 后得
**18573 个函数、2073 个文件**。`nloc` = 非空行、非注释行，与引擎定义一致。
数据、脚本与完整输出：`.scratch/complexity/research/data/`（`analyze.py` + `summary.txt`）。

这是便利样本（一个开发者机器上的仓库），不是语料库。它的用途只有两个：
检验公开默认值在本机代码上是否是可用量级，以及回答「长度和复杂度到底重不重叠」。
它不替代公开默认值——阈值仍以工具/文献的默认为准。

### A.2 分布

函数 nloc（n=18573）：

| 分位 | p50 | p75 | p90 | p95 | p99 | p99.9 | max | mean |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| nloc | 7 | 15 | 29 | 42 | 94 | 314 | 1132 | 13.3 |

| 阈值 | 40 | 50 | 60 | 80 | **100** | 150 | 200 | 300 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 占比 | 5.46% | 3.49% | 2.35% | 1.34% | **0.86%** | 0.37% | 0.22% | 0.11% |

文件 nloc（n=2073）：

| 分位 | p50 | p75 | p90 | p95 | p99 | p99.9 | max | mean |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| nloc | 61 | 161 | 335 | 513 | 1042 | 2278 | 7587 | 141.3 |

| 阈值 | 200 | 300 | 500 | 750 | **1000** | 2000 |
| --- | --- | --- | --- | --- | --- | --- |
| 占比 | 19.97% | 11.63% | 5.31% | 2.32% | **1.21%** | 0.19% |

### A.3 长度与复杂度重不重叠

| 相关系数（Spearman） | 值 |
| --- | --- |
| nloc ↔ cognitive | 0.567 |
| nloc ↔ cc | 0.564 |
| cc ↔ cognitive | **0.994** |
| 文件 nloc ↔ 文件内最差函数的 cognitive | 0.689 |

三条阈值各自的命中数与边际贡献（函数阈值取 100）：

| 分组 | n | `cc>10` | `cognitive>15` | `nloc>100` | 并集 | 长度独有 |
| --- | --- | --- | --- | --- | --- | --- |
| 全部 | 18573 | 486 | 412 | 160 | **613** | **52** |
| 生产代码 | 12299 | 422 | 348 | 117 | 499 | **22** |
| 测试代码 | 6274 | 64 | 64 | 43 | 114 | **30** |

诚实地读这张表：在生产代码里长度轴只多 22 个命中（+4.6%），在测试代码里多 30 个（+36%）。
它**便宜**，不是**颠覆性**的——真正的理由不是数量，而是这 22 个是复杂度指标**结构上**看不见的一类，
且 §3.3 的最大规模测量显示两者恰恰在最长的函数里分离。

- 「长而平」（`nloc>100 && cc<=10`）：53 个；「短而绕」（`nloc<=40 && cc>10`）：193 个。
- 按 cognitive 的前 100 与按 nloc 的前 100，**重合 48 个**。
- 长度独有的 52 个里，语言分布是 TS 19 / Python 14 / Java 12 / Rust 7；按仓库分散在
  qingluan 10、BaseUI 9、Mini-Nav 9、pi-extensions 9 等，不是某一个仓库的怪癖。

顺带一个值得记录的观察：本仓数据里 cc 与 cognitive 的 Spearman 高达 **0.994**——
说明在真实代码上两个指标排序几乎一致，cognitive 主要贡献的是**同分段的排序细化**
（switch 密集处才明显分歧），而不是另一种视野。这不改变默认用 cognitive 的决定
（spec 的理由是 switch 场景），但值得知道。

### A.4 文件长度能看见什么函数指标看不见的

| 阈值 | 超长文件 | 其中已被函数指标命中 | 函数全都正常 |
| --- | --- | --- | --- |
| > 750 | 48 | 37（77.1%） | 11 |
| **> 1000** | **25** | 20（80.0%） | **5** |

典型例子：`FPGA_WebLab/src/APIClient.ts` 7587 nloc、**331 个函数**；`qingluan/crates/qingluan-terminal/tests/runtime.rs`
1523 nloc、最差函数 cognitive 9。这两个文件在任何函数级指标下都很不起眼。

但要看清超长文件**长在哪里**，否则会把「文件长度」误当成复杂度信号：

| 分组 | 每文件函数数（中位） | 平均函数长度 |
| --- | --- | --- |
| 文件 > 750 nloc | **45** | 52.1 行 |
| 文件 <= 750 nloc | 8 | 15.6 行 |

> 750 的 48 个文件里，函数数从 1 到 331（中位 45）；其中「函数全都正常」的 11 个，
> 多数是**几十个小函数堆在一起**（77 个 / 53 个 / 88 个 / 78 个函数的文件，平均函数只有 9–29 行），
> 只有 2–3 个是真正「几个又大又平」的函数。也就是说：**函数长度是复杂度信号，
> 文件长度主要是模块组织信号。** 所以文件那张表必须同时给函数数。

### A.5 测试代码

| 分组 | n | p90 nloc | p99 nloc | >100 占比 |
| --- | --- | --- | --- | --- |
| 函数，非测试 | 12299 | 29 | 99 | 0.95% |
| 函数，测试 | 6274 | 31 | 87 | 0.69% |

函数长度上测试与生产几乎一样；文件长度上测试偏长（p50 121 vs 50）。所以长度阈值不需要对测试
单独放宽，但「最长文件」榜里测试会占相当比例，属于预期。

### A.6 口径注意（不是缺陷，但定阈值前要想清楚）

1. 引擎的 `nloc` 按函数语法区间算，**含签名行与收尾大括号**，也**含嵌套函数/闭包的行**
   （因为我们把嵌套函数算进外层，见 spec）。所以「一个 40 行的函数里塞了 150 行的回调」
   会记成 ~200 行。这跟本工具「嵌套归外层」的整体口径一致，也跟 Clippy 的 `too_many_lines`
   一致（子代理逐条核对了 Clippy 源码 `clippy_config/src/conf.rs` 的口径），但意味着 JS/TS
   的回调密集代码更容易触发长度阈值。
   如果实测下来 TS 侧噪声偏大（本机 TS 的 >100 比例 2.11% 已是 Java 的 5 倍），
   第一个该试的旋钮是给 `[complexity]` 按语言覆盖阈值，而不是改口径。
2. 注释与空行不计数；Rust 的 `///` 文档注释算注释（不计数），但 `#[attribute]` 算代码行。
3. 生成物仍按默认排除规则处理，长度阈值不额外做「疑似生成」判断——
   本机最长文件 `APIClient.ts`（7587 行、331 个函数）就是漏网的生成物，值得单独开一条。
