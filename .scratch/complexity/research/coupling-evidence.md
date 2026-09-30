# Research: does file-level fan-in (afferent coupling) predict defects, change cost, or maintenance effort?

**Research date:** 2026-09-30.
**Question.** qingluan has a function-level complexity CLI (per-function CC1 / Sonar cognitive / nloc /
parameters / nesting) that deliberately refuses composite scores. It is considering a *new axis*:
**file-level fan-in** — how many other files import this one — as a proxy for *change blast radius*.
This report asks the literature (peer-reviewed primary sources only) whether fan-in / afferent
coupling actually predicts defects, change cost, or maintenance effort, and whether the right signal
is raw fan-in or **fan-in × change frequency**.

**Complements:** `docs/research/code-length-metrics.md` (evidence-vs-convention discipline; §3.4 on
the El Emam size confounder; §4 on why composites are rejected) and
`docs/research/code-complexity-metrics.md` (§fan-in/fan-out = Henry & Kafura, needs a call graph).

**Method.** Six parallel sub-agents, each on one topic cluster, plus direct re-verification by this
agent of every quote that carries a verdict. Full text was read wherever obtainable; where only the
publisher abstract was reachable that is stated explicitly. Only short passages are quoted. No
upstream PDF was left in the repo.

**Reading conventions used throughout**

| Label | Meaning |
| --- | --- |
| **FULL TEXT** | Quote taken from the paper body. |
| **ABSTRACT ONLY** | Quote taken from the publisher/aggregator abstract; body not read. |
| **UNVERIFIED** | Could not independently confirm; treat as secondary. |

---

## TL;DR — the three verdicts

**(a) Does raw fan-in predict anything?**
**Weakly, and not on its own.** There is **no primary source in which fan-in (Ca) and fan-out (Ce)
carry opposite signs** for defects (sub-agent search, §2). Both directions are usually *positively*
associated with faults, but **fan-out is the stronger and more consistent predictor; fan-in is often
weak or non-significant** — Tahir et al. 2021 EMSE: *"The Fan-in metric has most of the insignificant
correlation values"* (arXiv:2106.04687, FULL TEXT) — and there are genuine **negative/null** results:
Nagappan, Ball & Zeller 2006 (5 Microsoft systems) found *"not a single metric that would correlate
with post-release defects in all five projects"*, with ClassCoupling −0.303 in one project and
FanIn/FanOut Max ≈ −0.2 in another; Subramanyam & Krishnan 2003 found CBO **negative for Java**; and
Child, Rosner & Counsell 2019 found **half of ~10 CBO variants** *"have no practical application to
the prediction of defects"* (§2.2). Fan-in is also *less* size-confounded than CBO /
fan-out (so it is not pure size in disguise), but its total effect is small. The famous "controlling
for size kills coupling" result (El Emam et al. 2001) is itself **contested**, not settled:
Subramanyam & Krishnan 2003 found CK metrics still significant *after* controlling for size, and
Tahir et al. 2018/2021 could not confirm consistent size mediation (§2.3). Martin's Ca/Ce and the
Stable Dependencies Principle — the usual justification for "high fan-in is important" — is
**design convention with no dataset, no statistics and no validation** (verified from the primary
text, §2.1).

**(b) Does fan-in × churn do better?**
**Churn alone does the heavy lifting; nobody has validated the product.** The change-history
literature is consistent that *how often a file changes* is a strong fault signal
(Nagappan & Ball 2005: relative churn R² = 0.811, 89.0% discrimination; Hassan 2009: 13–42% error
reduction over prior-modification models; Rahman & Devanbu 2013: *"code metrics … are generally less
useful than process metrics"*; §3). Two caveats, both verified: (i) **no study runs a head-to-head of
churn against coupling/fan-in** — Hassan 2009 never runs a static baseline; (ii) the winning churn
measures are **LOC-normalised**, so "process beats static" is partly "process framed in size units
beats raw size". The only widely-used "hotspot" implementation does **not** multiply fan-in by churn:
CodeScene's own docs define the hotspot axis as *development activity*, keep code health as a
**separate severity axis** (*"CodeScene starts by identifying files with high change frequency and low
Code Health"*), rank with an unpublished model, and use **observed change coupling** — not structural
fan-in — for the coupling dimension (§4). So "fan-in × churn as a validated scalar" has **no
peer-reviewed validation**; the defensible design is a **two-axis union/rank with churn as the
frequency axis**, the same pattern qingluan already uses for thresholds.

**(c) Is structural fan-in a good proxy for observed co-change / blast radius?**
**No — this is the weakest link.** Oliva & Gerosa (ISSRE 2015) and the Oliva 2016 PhD thesis (45 Apache
Java projects, 77,286 snapshots) found: when A depends on B and B changes, A co-changes only **~32% of
the time on average (σ = 13.6%)**; classifiers on structural dependencies had AUC **0.52–0.76**, i.e.
*"it is not possible to accurately predict co-changes solely using dependencies"*; and *"the majority
of co-changes do not correlate with structural dependencies"* (FULL TEXT, §5). Ajienka & Capiluppi
(JSS 2017, 79 projects) independently found the structural↔co-change correlation significant in
**fewer than 10 of 79 projects**, with only **1–44%** of co-changing pairs structurally coupled. The
impact-analysis literature agrees from the other side: *"Static analysis can produce safe but
overly-conservative impact sets"* (Cai, IEEE TSE 2018). Import fan-in therefore **over-approximates**
some propagation and **misses** most of it.

**Bottom line for qingluan.** Fan-in is a *structural* signal about dependency direction and
stability — useful as a **descriptive axis** ("this file is depended on by N files; a change here
reaches N call sites") and as a **navigation aid**. It is **not** a validated blast-radius or defect
metric, and folding it into a composite with churn would repeat exactly the unvalidated-composite
mistake §4 of `code-length-metrics.md` rejects. If a churn axis is added at all, keep it **separate,
observed, and labelled** ("changes in last N days"), and do **not** present fan-in as its validated
multiplier.

---

## 1. Henry & Kafura 1981 — the origin, and what it actually says

**Citation.** S. Henry & D. Kafura, "Software Structure Metrics Based on Information Flow",
*IEEE TSE*, **SE-7(5):510–518**, Sept. 1981. DOI
[10.1109/TSE.1981.231113](https://doi.org/10.1109/TSE.1981.231113). Crossref confirms title, volume
SE-7, issue 5, pages 510–518, 511 citing works.

**The metric.** Verbatim (FULL TEXT via an unofficial HTML mirror of the paper; the IEEE PDF is
paywalled and no OA copy exists — **flag: quote source is a mirror, not the publisher PDF**):

> "The formula defining the complexity value of a procedure is length * (fan-in* fan-out) ** 2."

*fan-in* = local flows into the procedure + data structures from which it reads; *fan-out* = local
flows out + data structures it updates; *length* = lines of text including embedded comments. The
`(fan-in × fan-out)²` weighting is the authors' own construction, **not** a fitted result.

**What they measured, on what system.** **UNIX V6** (C; assembly and "memoryless" procedures
excluded). The dependent variable is **80 changes** that the authors collected from the UNIX users
group (private correspondence, Ferentz 1979) — *not* a defect database. Procedure count is **not
stated** in the paper (a secondary source claims 10 modules / 220 procedures). Reported correlations
(FULL TEXT):

| Quantity | r |
| --- | --- |
| composite complexity vs % procedures containing an error | **0.94** (p = 0.0214) |
| (fan-in × fan-out) | 0.83 |
| (fan-in × fan-out)² | 0.98 |
| (length)² | 0.60 |

**The methodological catch that the "r = 0.94" folk claim always omits.** The correlations are
computed over **aggregated complexity intervals, not individual procedures**: *"correlate the order
of complexity with the percentage of procedures containing an error"*; *"Fig. 11 displays a
distribution of the eight intervals used."* Effective **n ≈ 8**, and two levels were dropped —
*"because of this small sample size, these levels will be eliminated"*. A rank correlation over ~8
buckets on one OS is not evidence that fan-in predicts defects per file.

**The authors' own stated limitations (verbatim, FULL TEXT):**

> "we have found no satisfactory way to thoroughly validate these measurements using the UNIX data we currently possess"

> "This paper represents only the beginning of the work that should be undertaken"

> "Due to the density of this distribution of length it was not possible to obtain a meaningful correlation coefficient"

> "the module is so complex that is distorts other measurements"

**Replications and failures — the key result for qingluan:**

- **Kitchenham 1988** (COMPSAC, DOI [10.1109/CMPSAC.1988.17200](https://doi.org/10.1109/CMPSAC.1988.17200))
  and **Kitchenham, Pickard & Linkman 1990** (DOI
  [10.1049/sej.1990.0007](https://doi.org/10.1049/sej.1990.0007)) are the strongest independent tests,
  and both are **negative** against the metric family (ABSTRACT ONLY):
  > "were not as good at identifying change-prone, fault-prone, and complex programs as simple code metrics (i.e. lines of code and number of branches)"

  The 1990 abstract adds that only **informational fan-out** was predictive — **fan-in was not**.
- **Kafura & Reddy 1987** (DOI [10.1109/TSE.1987.233164](https://doi.org/10.1109/TSE.1987.233164)) is
  **not an independent replication** — Kafura is a co-author; it is a subjective expert-judgement
  study on a 16k-LOC Fortran system with no objective maintenance records, self-described as *"a very
  limited study"*.
- **Card, Church & Agresti 1986** (correct DOI
  [10.1109/TSE.1986.6312942](https://doi.org/10.1109/TSE.1986.6312942), FULL TEXT via a NASA scan) is
  **commonly miscited as a Henry–Kafura test**: it never tested the HK formula; its null result is for
  parameter-vs-COMMON coupling, and it found a *positive* effect for descendant span (*"Modules with
  many descendants are more fault prone"*).
- **Fenton & Neil, "Software Metrics: Roadmap" (2000)** contains **zero mentions** of Henry or Kafura
  — citing it as an HK critique is unsupported.

**Verdict on §1:** the 1981 paper is a **self-validated, aggregated, single-system** result on the
authors' **composite** formula; the one strong independent test of the family found **LOC / branch
counts better**, and fan-in specifically was not predictive. **UNVERIFIED:** the exact procedure count;
Shepperd & Ince 1994, Ince & Shepperd 1989, Troy & Zweben 1981, Pickard & Carter 1995 and the
Radjenović 2013 SLR (all paywalled, no accessible abstract); and whether any peer-reviewed source
literally says the formula "was never independently replicated at scale" (no such source was found —
that phrasing exists only in a non-peer-reviewed industry document).

---

## 2. Coupling and defects: does fan-in predict fault-proneness?

This section reports the direct tests. The headline: **fan-in and fan-out do not have opposite
signs**; fan-out is the stronger predictor; fan-in is usually weak; and the size question has a
subtle answer (fan-in is *less* size-mediated, not more).

### 2.1 Martin's Ca/Ce and the Stable Dependencies Principle are convention, not evidence

Robert C. Martin, *Design Principles and Design Patterns* (2000), primary text read in full from the
Internet Archive copy of the objectmentor.com PDF. It contains **no dataset, no experiment, no
statistics**. Verbatim (FULL TEXT):

> "Ca Afferent Coupling. The number of classes outside the package that depend upon classes inside the package."

> "Ce Efferent Coupling. The number of classes outside the package that classes inside the package depend upon."

> "The Stable Dependencies Principle (SDP): Depend in the direction of stability."

> "One sure way to make a software package difficult to change, is to make lots of other software packages depend upon it."

Note the internal tension: high Ca is *prescribed* as "stable", yet the rationale is that many
dependents make a package **difficult to change**. There is no defect/fault validation. **Do not cite
SDP/SAP/Ca/Ce as empirical evidence.**

### 2.2 The classic class-level results

| Study | Fan-in (Ca / export) | Fan-out (Ce / import) | Quote | Status |
| --- | --- | --- | --- | --- |
| Basili, Briand & Melo 1996, IEEE TSE 22(10):751–761, [10.1109/32.544352](https://doi.org/10.1109/32.544352) | NOC (inheritance in-degree, a fan-in *proxy*) **protective**: −2.01 / −3.3848 | CBO **positive** +0.142 univariate / +0.13 multivariate (p=.0072); RFC positive | "The larger the NOC, the lower the probability of defect detection." | FULL TEXT (published **TSE** PDF; the UMD tech-report copy prints signs **globally inverted** — cite the TSE values) |
| Glasberg, El Emam, Melo & Madhavji 2000, NRC Canada ERB-1080 | Export coupling = fan-in: **positive but weak** (+0.32, p=0.037) | Import coupling = fan-out: **positive, stronger** (+0.47, p=0.0042) | "Export Coupling metrics were found positively associated with fault-proneness…" | FULL TEXT; controls for size (NM), DIT, DIT² |
| Tahir, Bennin, Xiao & MacDonell 2021, EMSE, [10.1007/s10664-021-09991-3](https://doi.org/10.1007/s10664-021-09991-3) (arXiv:2106.04687) | Fan-in **positive but weak**, least consistent | Fan-out **positive, significant total/direct/indirect effects** | "The Fan-in metric has most of the insignificant correlation values whereas LOC, Fan-out, RFC and WMC metrics had more significant correlation values" | FULL TEXT (arXiv version) |
| Zimmermann, Nagappan, Herzig, Premraj & Wang 2011, ICST, [10.1109/ICST.2011.39](https://doi.org/10.1109/ICST.2011.39) | Direction of dependency matters but **project-dependent** | — | "components that have outgoing dependencies to components with higher object-oriented complexity tend to have fewer field failures for VISTA, but the opposite relation holds for ECLIPSE" | ABSTRACT ONLY |
| **Nagappan, Ball & Zeller 2006, ICSE, [10.1145/1134285.1134349](https://doi.org/10.1145/1134285.1134349)** | Project A FanIn Max **−0.197** | Project A FanOut Max **−0.200**; ClassCoupling Max **−0.303** in project D | "It turns out that there is not a single metric that would correlate with post-release defects in all five projects." | **FULL TEXT** — resolves the earlier "metric catalogue unverified" flag: coupling *was* measured. Signs verified, significance not (bold lost in text layer) |
| **Child, Rosner & Counsell 2019, JSS 151:120–132, [10.1016/j.jss.2019.02.020](https://doi.org/10.1016/j.jss.2019.02.020)** | — | CBO **split by variant**: 6 of ~10 variants fail, the rest are "sound" | "these metrics have no practical application to the prediction of defects" (about the failing variants) | FULL TEXT |
| Olague, Etzkorn, Gholston & Quattlebaum 2007, IEEE TSE, [10.1109/TSE.2007.1015](https://doi.org/10.1109/TSE.2007.1015) | — | **Not a blanket null**: MOOD's coupling component fails, CK's CBO does not | "the class components in the MOOD metrics suite are not good class fault-proneness predictors" | ABSTRACT ONLY |
| Gyimóthy, Ferenc & Siket 2005, IEEE TSE 31(10), [10.1109/TSE.2005.112](https://doi.org/10.1109/TSE.2005.112) | CBO **best** (R²=0.349) but **LOC nearly equal** (R²=0.342), no size adjustment; NOC/LCOM null | — | "we found that CBO (coupling) and LOC (size) metrics came out top while WMC (complexity) got a lower score" | TSE full text **unobtainable**; quotes are same-author restatements (Siket 2010; Siket thesis). Undirected CBO — cannot speak to fan-in vs fan-out |
| D'Ambros, Lanza & Robbes 2010, MSR, [10.1109/MSR.2010.5463279](https://doi.org/10.1109/MSR.2010.5463279) | FanIn is in the metric catalogue but **no per-metric result is reported** | FanOut likewise | "Using the CK and the OO metric sets together is preferable to using them in isolation…" | FULL TEXT per one verifier (institutional copy); a second verifier could obtain **no copy** — treat as partially verified |

**Negative / null results found (now substantial).** (i) No study was found where Ca and Ce have
strictly opposite signs. (ii) Fan-in is frequently **insignificant** (Tahir 2021; NRC 2000 for NOC
after controlling for descendant export coupling). (iii) **Nagappan, Ball & Zeller 2006** is a genuine
multi-project null: no metric generalises, and there are **negative** coupling coefficients
(ClassCoupling −0.303 in project D; FanIn/FanOut Max −0.197/−0.200 in project A). (iv) **Subramanyam
& Krishnan 2003** find CBO **negative for Java**: *"C++ classes with higher CBOs are associated with
higher defects, whereas they are associated with fewer defects for the Java sample"* (∂/∂CBO +0.173
vs **−0.011**). (v) **Child, Rosner & Counsell 2019** show the answer depends on **which CBO
definition the tool implements** — half the variants fail outright. (vi) **Olague et al. 2007**: the
MOOD coupling component is a poor predictor while CK's is not. (vii) Gyimóthy et al. 2005 report CBO
as best but **LOC essentially tied with it**, with no size adjustment.

### 2.3 The size confounder: "size kills coupling" is contested, not settled

This is where the repo's existing note needs care in **both** directions. `code-length-metrics.md`
§3.4 quotes El Emam et al. 2001 as the canonical size-confounder result, then cites a Tahir et al.
ESEM 2018 paper as showing size *fully mediates* coupling metrics. Sub-agent verification found the
El Emam quote is right but **the Tahir attribution is misread**, and the "size kills coupling" claim
is contested by later work.

**The canonical null (El Emam, Benlarbi, Goel & Rai 2001**, IEEE TSE 27(7):630–650,
DOI [10.1109/32.935855](https://doi.org/10.1109/32.935855) — **ABSTRACT ONLY**; IEEE/ACM return 403,
Unpaywall/OpenAlex report no OA copy). The famous sentence **is verified verbatim** in the full IEEE
abstract (reproduced at
[neverworkintheory.org](https://neverworkintheory.org/2011/07/07/the-confounding-effect-of-class-size-on-the-validity-of-object-oriented-metrics.html);
earlier sentences match OpenAlex independently):

> "After controlling for size, none of the metrics we studied were associated with fault-proneness any more."

Scope, from the same abstract (verbatim): study on **"a large C++ telecommunications framework"**;
**"the Chidamber and Kemerer metrics and a subset of the Lorenz and Kidd metrics"**; DV **"the
incidence of a fault attributable to a field failure (fault-proneness of a class)"**. Since the CK
suite includes **CBO**, a blanket "none of the metrics" covers coupling. **Still UNVERIFIED:** the
exact metric table (which L&K subset; whether any explicit Ca/Ce measure was included), system name,
class/KLOC counts, and the pre/post-control odds ratios.

**The rebuttals and the re-analysis:**

| Position | Source | Exact quote | Label |
| --- | --- | --- | --- |
| **It survives size control** | Subramanyam & Krishnan 2003, IEEE TSE 29(4):297–310, [10.1109/TSE.2003.1191795](https://doi.org/10.1109/TSE.2003.1191795) | "even after controlling for the size of the software, these metrics are significantly associated with defects" | FULL TEXT (Wayback author PDF) |
| **The confounder premise is wrong** | **Evanco** 2003, IEEE TSE 29(7), [10.1109/TSE.2003.1214331](https://doi.org/10.1109/TSE.2003.1214331) — **not** Briand et al. | "the ability to measure size does not temporally precede the ability to measure many of the object-oriented metrics" | ABSTRACT ONLY |
| **Confound exists, but de-confounded models predict better** | **Zhou, Xu, Leung & Chen** 2014, ACM TOSEM 23(1), [10.1145/2556777](https://doi.org/10.1145/2556777) | "after removing the confounding effect, the prediction performance of fault prediction models … can in general be significantly improved" | ABSTRACT ONLY |
| **No consistent mediation/moderation** | Tahir, Bennin, MacDonell & Marsland 2018, ESEM, [10.1145/3239235.3239243](https://doi.org/10.1145/3239235.3239243) | "We are unable to confirm if class size has a significant mediation or moderation effect on the relationships between OO metrics and the number of faults." | FULL TEXT (arXiv:2104.12349) |
| **Size control eliminates predictive capability** | Gil & Lalouche 2017, EMSE, [10.1007/s10664-017-9513-5](https://doi.org/10.1007/s10664-017-9513-5) | "As it turns out, metrics controlled for size, tend to eliminate their predictive capabilities." Also: "the validity of a metric can be accurately (with R-squared values being at times as high as 0.97) predicted from its correlation with size"; "Overall, our results suggest code size is the only 'unique' valid metric." | ABSTRACT ONLY (EMSE negative-results special section) |
| **Coupling specifically is size-mediated** | Tahir, Bennin, Xiao & MacDonell 2021, EMSE, [10.1007/s10664-021-09991-3](https://doi.org/10.1007/s10664-021-09991-3) | "size consistently has significant mediation impact only on the relationship between Coupling Between Objects (CBO) and defects/defect-proneness"; "size fully mediates the relationship between CBO and Fan-out, and the number of defects" (Mylyn) | FULL TEXT |

**Warning about a circulating secondary mis-citation.** A related-work sentence in Subramanyam &
Krishnan 2003 claims that in El Emam et al. the residual effects of most CK metrics *except coupling
and inheritance* become non-significant after size control. **That contradicts El Emam's own
abstract** (*"none of the metrics we studied were associated with fault-proneness any more"*). It is a
mis-citation in a secondary source; do not repeat it as El Emam's finding (El Emam's primary text is
paywalled and could not settle it).

Three concrete corrections to record (they belong to `code-length-metrics.md` if it is ever revised,
but this report does not edit that file):

1. **Tahir et al. ESEM 2018 does not claim size fully mediates coupling.** That sentence exists in the
   paper, but scoped to **one system**: *"it is concluded that class size fully mediates the
   relationship between the number of faults and the following metrics: RFC, CBO, LCOM, Fan-in and
   Fan-out **in this system**"* — and the surrounding text identifies "this system" as **Apache Lucene
   2.4 (DAMB)**. The paper's overall conclusion is the opposite of a clean confound story: *"We
   contend that class size does not fully explain the relationships between OO metrics and the number
   of faults."* The authors are Tahir, Bennin, MacDonell & Marsland (**not** Rasool/Gencel). Its own
   sub-result: *"size appears to have a more significant mediation effect on CBO and Fan-out than
   other metrics"* → **fan-in is among the *least* size-mediated**; 2021 EMSE follow-up: *"Overall,
   WMC, LCOM and Fan-in are the metrics with the least evidence of a mediation effect of size."*
2. **The 2003 TSE critique is by W. Evanco**, not Briand/Melo/Wüst.
3. **DOI 10.1145/2556777 is Zhou, Xu, Leung & Chen 2014**, "An in-depth study of the potentially
   confounding effect of class size in fault prediction" — not an unnamed "TOSEM 2014 study".

**Also relevant:** Subramanyam & Krishnan found CBO's **sign flips by language** — *"the net effect of
CBO on defects … is positive for the C++ sample. A similar analysis of the Java sample indicates that
the net effect of CBO on defects … is negative."* That is the closest thing in the literature to a
genuinely **negative** coupling result, and it is a class-level CBO count, not file fan-in.

**Assessment.** The honest reading is: (i) coupling is strongly correlated with size, so an
**unadjusted** coupling coefficient proves nothing; (ii) the blanket "size kills coupling" claim rests
on **one** binary-DV study on **one** system with no open full text, and is contradicted by a count-DV
study on two language samples that did control for size; (iii) Tahir et al.'s bootstrap
mediation/moderation on 17 systems could not confirm consistent mediation. For a **file-level import
count**, this cuts both ways: import count is *not* pure size in disguise (fan-in is among the least
size-mediated), but neither is it a validated independent risk factor.

### 2.4 What this means for a file-level import count

Every result above is **class-level**. None of them measures "number of files that import this file".
Moving to the file level changes three things, all of which weaken the inference: (1) imports are
**syntactic** and over-approximate real use; (2) a file has far fewer incoming edges than a class
(the graph is coarser, so in-degree is a low-resolution integer); (3) in-degree is confounded with
**age, popularity and LOC** (§6). The class-level evidence is therefore an **upper bound** on how much
a file-level import count can be expected to carry.

---

## 3. Change history beats static structure

The change-history literature is the strongest body of evidence in this report. The consistent
finding: **process/history metrics (churn, change frequency, ownership) predict faults at least as
well as — usually better than — static structural metrics.**

- **Nagappan & Ball 2005**, "Use of Relative Code Churn Measures to Predict System Defect Density",
  ICSE 2005, DOI [10.1145/1062455.1062514](https://doi.org/10.1145/1062455.1062514). **FULL TEXT.**
  Abstract:
  > "Using statistical regression models, we show that while absolute measures of code churn are poor predictors of defect density, our set of relative measures of code churn is highly predictive of defect density."
  Relative model R² = **0.811** (Table 3); stepwise ladder .592 → .811; discrimination
  **2195/2465 = 89.0%** (PCA), 2188/2465 = 88.8%. Single system: Windows Server 2003 → SP1, **2465
  binaries / 96,189 files**. The relative measures are LOC-normalised ratios (churned LOC / LOC,
  deleted LOC / LOC, files churned / file count, …). **Takeaway: size/LOC is a good denominator and a
  poor magnitude.**
- **Hassan 2009**, "Predicting faults using the complexity of code changes", ICSE 2009,
  DOI [10.1109/ICSE.2009.5070510](https://doi.org/10.1109/ICSE.2009.5070510). **FULL TEXT.** Systems
  are **NetBSD, FreeBSD, OpenBSD, Postgres, KDE, KOffice** — **not** Eclipse:
  > "our change complexity metrics are better predictors of fault potential in comparison to other well-known historical predictors of faults, i.e., prior modifications and prior faults."
  Error deltas vs a prior-modifications model: FreeBSD −47.4 (−22%), NetBSD −39.8 (−14%), OpenBSD
  −40.4 (−18%), Postgres −52.7 (−37%), KDE −52.1 (−13%), KOffice +3.3 (+1%, n.s.); decay model up to
  −42%. **Critical caveat (verified):** the paper **never runs a CK/McCabe/static-complexity
  baseline**; it justifies comparing only against history predictors because prior work showed those
  beat complexity. So it is *not* a head-to-head against static structure. Also: *"no single model
  statistically outperforms all other models for all systems."*
  **Citation correction:** there is **no 2011 TSE paper** with this title — Crossref/DBLP return only
  the 2009 ICSE paper; the "(6 OSS + Eclipse)" memory conflates it with Shihab et al., ESEM 2010.
- **Rahman & Devanbu 2013**, "How, and Why, Process Metrics Are Better", ICSE 2013,
  DOI [10.1109/ICSE.2013.6606589](https://doi.org/10.1109/ICSE.2013.6606589). **FULL TEXT** (authors'
  draft). 85 releases / 12 projects:
  > "code metrics, despite widespread use in the defect prediction literature, are generally less useful than process metrics for prediction"
  and code metrics show *"high stasis"* (they barely vary across releases). **Contrary evidence
  quoted inside the same paper:** *"Menzies et al. report that code metrics are useful for defect
  prediction"*, and Arisholm et al. found process and code metrics *"perform similarly in terms of
  AUC"*.
- **Kamei et al. 2013**, "A Large-Scale Empirical Study of Just-in-Time Quality Assurance", IEEE TSE
  39(6), DOI [10.1109/TSE.2012.70](https://doi.org/10.1109/TSE.2012.70). ABSTRACT ONLY: 6 OSS + 5
  commercial projects; average accuracy **68%**, recall **64%**; inspecting 20% of the effort
  identifies **35% of defect-inducing changes**.
- **Bird, Nagappan, Murphy, Gall & Devanbu 2011**, "Don't Touch My Code! Examining the Effects of
  Ownership on Software Quality", ESEC/FSE 2011,
  DOI [10.1145/2025113.2025119](https://doi.org/10.1145/2025113.2025119). ABSTRACT ONLY:
  > "measures of ownership such as the number of low-expertise developers, and the proportion of ownership for the top owner have a relationship with both pre-release faults and post-release failures"
  Two Microsoft systems (Vista, Windows 7); *"the removal of low-expertise contributions dramatically
  decreases the performance of contribution based defect prediction."* **Do not over-read:** the paper
  does **not** claim ownership beats static structure head-to-head.
- **Graves, Karr, Marron & Siy 2000**, "Predicting Fault Incidence Using Software Change History",
  IEEE TSE 26(7):653–661, DOI [10.1109/32.859533](https://doi.org/10.1109/32.859533). **GAP —
  abstract and body NOT VERIFIED** (IEEE closed, no OA, all aggregators 403/JS). **SECONDARY ONLY**
  (UMD CMSC838M lecture slides, 2002): a 1.5M-LOC telephone-switching legacy system, ~2 years of
  delta/IMR data, Poisson GLM deviance — deltas + age **697.4**, + LOC **696.3** (≈ no gain from
  adding LOC), LOC only **1271.4**, past-faults 757.4, org 2587.7, null 3108.8; weighted
  time-damped churn best at **631.0** with a ~50%/yr halving of contribution; developers and "module
  connectivity to other modules" listed as **non-predictors**. Treat all numbers as secondary until
  the publisher version is read.
- **D'Ambros, Lanza & Robbes 2010** (MSR), DOI
  [10.1109/MSR.2010.5463279](https://doi.org/10.1109/MSR.2010.5463279) — **conflicting verification.**
  One verifier read a full-text copy (institutional repository) and confirms the metric catalogue
  includes FanIn/FanOut but reports only **metric-set** scores (CK 0/4 explanative/predictive, OO 6/6,
  CK+OO 8/8; §2.2); a second verifier could reach **no copy at all**. Either way, the widespread claim
  that "relative churn/entropy ranked best among 15 approaches" is **UNVERIFIED** here.

**Two reading cautions for §3:**

1. **"Process beats static" partly means "process framed in size units beats raw size."** The winning
   churn measures are **LOC-normalised** (churned LOC / total LOC). So a fair summary is not
   "history demolishes structure"; it is "history *plus a size normaliser* beats raw size and static
   aggregates". This is consistent with `code-length-metrics.md` §3.5's reading of Nagappan & Ball.
2. **No head-to-head against coupling/fan-in exists.** Hassan 2009 has no static baseline; Nagappan &
   Ball have no coupling metric; Rahman & Devanbu compare process vs "code metrics" broadly. The
   strongest *indirect* statement is Oliva's thesis literature summary (§5.2), not a controlled trial.

> **Reading of §3.** If the goal is *predicting where defects and change effort concentrate*,
> **change frequency is the primary axis**, and static structure is a secondary, weaker explanatory
> axis. For a *blast-radius proxy in particular*, the thing being proxied (a change propagating) is
> literally a history phenomenon, and the history signal is directly observable.

---

## 4. Hotspot analysis (change frequency × complexity): definition and validation

**Headline: the hotspot is not a product.** Every first-party source describes **two orthogonal axes
that are intersected (overlap), then ranked by an unpublished algorithm**. No equation is published in
any CodeScene doc version; the widely-circulated `score = complexity × revisions` formula is
**third-party, uncited, and contradicted by all first-party sources** — do not attribute it to
Tornhill/CodeScene.

### 4.1 First-party definitions (verbatim)

CodeScene 7.5.1 docs, <https://docs.enterprise.codescene.io/versions/7.5.1/guides/technical/hotspots.html>
(FULL TEXT of docs):

> "CodeScene starts by identifying files with high change frequency and low Code Health."

> "**Hotspots**: This is the relevance of the findings – the priority. The metric is calculated from the development activity in the code."

> "**Low Code Health**: This is the severity of any hotspot."

CodeScene Terminology v7.5.1 (FULL TEXT of docs) makes the orthogonality explicit:

> "Hotspots identify the code with the highest development activity."

> "hotspots don't imply a quality problem on their own … need to be combined with the code health measure"

The **legacy v4.1.23** docs state the intended combination directly (FULL TEXT):

> "We use the lines of code in each file as a proxy for complexity" … "change frequency … as a proxy for the effort" … "You want to look for an overlap between the two metrics."

Tornhill's own 2013 article: *"The overlap between complex code and high activity are the combined
factors to guide our refactoring efforts."* The 2nd edition of *Your Code as a Crime Scene*
(publisher TOC) names its hotspot chapter sections *"Explore the Complexity Dimension"*, *"Intersect
Complexity and Effort"*, *"Drive Refactoring via a Probability Surface"*, *"Be Aware That Hotspots
Reflect Probabilities"*. **FLAG:** the book's definitional prose itself is paywalled (Google Books
quota exhausted, O'Reilly 403) — the TOC/Chapter titles are verified, the definition sentence is not.

So the combination is **intersection / quadrant / union with ranking**; the complexity axis was
**LoC** in legacy versions and **Code Health** now; and the "probability surface"/priority ranking is
**not published**.

### 4.2 The coupling dimension CodeScene actually uses is observed, not structural

Hotspot prioritisation factors (docs, FULL TEXT): *"The hotspot has to be changed together with
several other modules"* — i.e. **change coupling from co-commits**, plus developer spread and
coordination bottleneck. **No structural import fan-in appears anywhere in the hotspot definition.**
CodeScene does expose an observed in-degree of its own:

> "By summing how many times a module has been coupled to another one in a commit we get a measurement called **sum of couplings**. This is a way to find modules that are architecturally significant."

That is the *observed* analogue of fan-in, used for "architectural significance" — **not** as the
blast-radius predictor. This is itself a signal: the vendor of hotspots chose observed co-change over
structural fan-in for the coupling dimension.

### 4.3 Validation status — vendor-internal, with a tied LoC baseline

| Study | What measured | Result | Limitations |
| --- | --- | --- | --- |
| Tornhill & Borg 2022, *Code Red*, DOI [10.1145/3524843.3528091](https://doi.org/10.1145/3524843.3528091), arXiv:2203.04374 | 39 proprietary codebases, Code Health vs Jira defects / cycle time | "15 times more defects", "124% more time", "9 times longer maximum cycle times"; LoC r=0.13 vs Code Health r=−0.58 | **vendor-coauthored**; explicit threat: "All included codebases come from CodeScene users, which might be a sample … not representative"; thresholds set internally; precise workshop venue string unverified |
| Tornhill & Borg, *Ghost Echoes*, ICSME 2024, DOI [10.1109/ICSME58944.2024.00072](https://doi.org/10.1109/ICSME58944.2024.00072), arXiv:2408.10754 | maintainability of 304 Java files (MainData) | AUC: SotA ML 0.97, **Code Health 0.95, naive LoC baseline 0.95 (tied)**, MS-MI 0.89, SonarQube 0.60–0.86, human 0.83 | 2/3 authors CodeScene employees, no COI statement; Java only; no significance testing; studies **maintainability, not hotspots** |
| CodeScene docs (vendor) | hotspot dashboard example | "There's a strong correlation between Hotspots and software defects"; 1.2% of code / 12.5% of effort / 45% of bugs | **unreferenced marketing copy**, not evidence |

**Independent peer-reviewed evaluation of CodeScene's hotspot construction: NONE FOUND** — this is a
negative finding and should be stated as such. The closest independent work: Willenbring & Walia,
ISSREW 2022, DOI
[10.1109/ISSREW55968.2022.00036](https://doi.org/10.1109/ISSREW55968.2022.00036) (content not
obtained; OSTI says "Abstract not provided"); Faragó et al., SCAM 2015, DOI
[10.1109/SCAM.2015.7335410](https://doi.org/10.1109/SCAM.2015.7335410) (+ the CRAN `hotspot`
package), whose hotspot is **ownership × cumulative churn** with no complexity axis; and an
independent TSE-preprint result reported as **Code Churn F = 44.2 (p < 0.0001) vs Cyclomatic
Complexity F = 1.0 (p = 0.2498)** — i.e. churn dominates raw complexity. **UNVERIFIED:** the exact
bibliographic identity of that last preprint.

**Churn alone vs churn × complexity: no head-to-head study found.** CodeScene's own legacy docs say
*"change alone is the single most important metric"*; Nagappan & Ball validate **relative churn**
(normalised by size and time), **not** churn × complexity.

**Term collision (verified).** Mo, Cai, Kazman & Xiao, WICSA 2015, DOI
[10.1109/WICSA.2015.12](https://doi.org/10.1109/WICSA.2015.12), use "hotspot patterns" for
**architecture smells** — "recurring architecture problems that occur in most complex systems",
"detected by the combination of history and architecture information". A completely different
construct; do not conflate.

> **Reading of §4.** "Hotspot" as shipped is a **priority view mapping an activity axis (change
> frequency) against a health axis**, intersected visually and ranked by an unpublished model. The
> two-axis presentation is defensible; the multiplication is not what the vendor does, has no
> peer-reviewed validation, and the closest thing to an independent benchmark (the vendor's own later
> paper) shows the composite **tied a naive LoC baseline**.

---

## 5. The crux: does structural fan-in match observed co-change?

This is the question that decides whether import fan-in can be sold as "change blast radius".

### 5.1 Oliva & Gerosa 2015 (ISSRE) — structural dependencies vs change propagation

**Citation.** G. A. Oliva & M. A. Gerosa, "Experience report: How do structural dependencies influence
change propagation? An empirical study", ISSRE 2015, pp. 250–260,
DOI [10.1109/ISSRE.2015.7381818](https://doi.org/10.1109/ISSRE.2015.7381818).
Abstract verified verbatim from a bibliographic record
([manuscript.isc.ac 3730232](https://manuscript.isc.ac/Inventory/49/3730232.htm), ABSTRACT ONLY for
the ISSRE paper):

> "Our results indicated that, in general, it is more likely that two artifacts will not co-change just because one depends on the other."

> "However, the rate with which an artifact co-changes with another is higher when the former structurally depends on the latter."

> "Finally, we also found several cases where software changes could not be justified using structural dependencies, meaning that co-changes might be induced by other subtler kinds of relationships."

4 open-source Java projects, thousands of snapshots. Note the tempering: dependence *raises* the
co-change rate but the modal case is still **no co-change**.

### 5.2 Oliva 2016 PhD thesis — the numbers (the strongest single source in this report)

**Citation.** G. A. Oliva, *On the Link between Structural Dependencies and Software Changes*, PhD
thesis, IME–University of São Paulo, 2016. Full text read
([teses.usp.br PDF](https://teses.usp.br/teses/disponiveis/45/45134/tde-20230727-113255/publico/OlivaGustavoAnsaldi.pdf),
FULL TEXT). Chapter 5 = 45 Apache Java projects, **77,286 code snapshots**. Verbatim:

> "when A depends on B and B changes, the chances of A changing together with B is around 32% in average, with a standard deviation of 13.6%"

> "the likelihood that A will change together with B is 32% in average, being around 20% higher in average than the likelihood found in the case where A does not depend on B"

> "Classifiers had a poor accuracy in general, with Area Under the Curve (AUC) ranging from 0.52 to 0.76, thus implying that it is not possible to accurately predict co-changes solely using dependencies."

> "the majority of co-changes do not correlate with structural dependencies, meaning that structural dependencies might be responsible for a small portion of all software changes"

> "Changes in methods rarely propagate via call dependencies."

> "even though it is more likely that Ji and Ji will not co-change just because Ji depends on h (i.e., dependencies do not instantly make two files change together), the rate with which Ji co-changes with h is higher when Ji structurally depends on h" — warm-up study rate *"ranging from 13.5% to 20%"*

The thesis also states the history-beats-structure position directly:

> "Starting in 2004, several researchers published studies indicating that co-changes could be much more accurately predicted using historical information (e.g., evolutionary coupling) instead of structural information (Hassan and Holt, 2004; Ying et al., 2004; Zimmermann et al., 2005)."

**Limitations the author states:** no causal claim (association only); rename/file-move not handled by
the path-based tool; method removals not handled in one RQ; commits in distributed VCS are split
differently (cited Brindescu et al.). **Status flag:** the ISSRE chapter is peer-reviewed; the
large-scale chapter was, at thesis time, *"to be submitted to the Empirical Software Engineering
journal"* — treat the 32%/AUC numbers as **examined-thesis evidence, not yet journal-refereed**.

### 5.3 Gall, Hajek & Jazayeri 1998 — logical coupling, and a citation trap

**Citation.** ICSM 1998, DOI [10.1109/ICSM.1998.738508](https://doi.org/10.1109/ICSM.1998.738508).
Full text read via the Wayback copy of the TU Wien paper archive. 20 releases of a ~10-MLOC
telecommunications switching system; CAESAR/CSA+CRA detect modules with identical change subsequences
across releases. Verbatim (FULL TEXT):

> "Such measures do not reveal all dependencies (e.g. dynamic relations)."

> "We may say that such code-based measures reveal syntactic dependencies and what we are really interested in is logical dependencies among modules."

> "Our results indicate that such retrospective analysis is a valuable complement to code-based and predictive analyses that are commonly practiced today."

**Trap flag (verified):** Gall et al. report **no percentage** of logically-coupled modules lacking a
structural relation, and **never measure structural coupling at all**. The comparison is qualitative.
Any figure of the form "X% of change couplings have no structural counterpart" is **not** in Gall et
al. 1998 and must not be attributed to it.

### 5.4 Independent confirmations of the structural ↔ co-change gap

- **Ajienka & Capiluppi, JSS 2017**, DOI
  [10.1016/j.jss.2017.08.042](https://doi.org/10.1016/j.jss.2017.08.042) (FULL TEXT, 79 projects,
  all revisions). The Spearman correlation between number of structural references and co-change
  confidence was significant in **fewer than 10 of 79 projects**; *"the majority of projects show an
  insignificant positive correlation coefficient"*; *"A stronger structural coupling does not imply a
  higher co-change likelihood."* The relation is asymmetric — about 80% of structural dependencies
  include logical dependencies *"but not vice versa"* — and only **1%–44%** of co-changing pairs are
  structurally coupled (in 63/79 projects ≤ 20%).
- **Stana & Şora, ENASE 2019**, DOI
  [10.5220/0007758104860493](https://doi.org/10.5220/0007758104860493) (FULL TEXT): logical
  dependencies outnumber structural by roughly an order of magnitude, and *"in at least 91% of the
  cases, logical dependencies involve files that are not structurally related"*. **Flag: the 91%
  figure is a secondary citation of Oliva & Gerosa SBES 2011, whose primary text was not obtained.**
- **Kirbas et al., JSEP 2017**, DOI [10.1002/smr.1842](https://doi.org/10.1002/smr.1842) (FULL TEXT,
  CC-BY): measures **only** evolutionary coupling → defects, **not** structural coupling (zero
  occurrences of "structural coupling"). Verbatim: *"there is generally a positive correlation
  between EC and defects, but the correlation strength varies."* **Do not cite it as a
  structural-vs-co-change comparison**, despite it being the standard "evolutionary coupling and
  defects" reference.

### 5.5 Change impact analysis — structural reachability over-approximates

- **Cai, "Hybrid Program Dependence Approximation for Effective Dynamic Impact Prediction", IEEE TSE
  44(4):334–364 (2018), DOI [10.1109/TSE.2017.2692783](https://doi.org/10.1109/TSE.2017.2692783)**
  (FULL TEXT). Verbatim:
  > "Static analysis can produce safe but overly-conservative impact sets"

  > "Our results confirm the imprecision of PI/EAS, whose impact sets often contain hundreds of methods."

  > "the impact sets of DIVER are 35-50 percent the size of the impact sets of PI/EAS while remaining as safe"

  Ten Java subjects; reported precision improvement 100–186% over the prior dynamic baseline. The
  point for qingluan: a static import edge means the dependent **can** be affected, not that it
  **will** be — static reachability is the conservative upper bound.
- **Law & Rothermel, ICSE 2003**, DOI
  [10.1109/ICSE.2003.1201210](https://doi.org/10.1109/ICSE.2003.1201210) (FULL TEXT): dynamic
  PathImpact sets were *"about one half (51%) of the size of the FS [static slicing] impact sets
  using single executions and 87% … using test suites"*. **Nuance to carry:** changed methods were
  often "deep" in the call graph, so transitive closure was sometimes *smaller* than PathImpact — do
  **not** claim static reachability always explodes.
- **Orso, Apiwattanapong, Law, Rothermel & Harrold, ICSE 2004**, DOI
  [10.1109/ICSE.2004.1317471](https://doi.org/10.1109/ICSE.2004.1317471) (FULL TEXT): static slicing
  *"can identify much larger impact sets"* than dynamic techniques and *"can lead to unnecessary
  expense"*; dynamic-vs-dynamic precision gaps averaged 2.7 (single executions) / 3.8 (test suites).
- Also in this thread: Ren et al., Chianti, OOPSLA 2004; Breech, Tegtmeyer & Pollock, ICSM 2006, DOI
  [10.1109/ICSM.2006.33](https://doi.org/10.1109/ICSM.2006.33) (influence mechanisms for *increased*
  precision); Arnold & Bohner's change-impact survey (**not retrieved — UNVERIFIED**).

**DOI corrections for the record:** Zimmermann et al., ICSE 2004 is
[10.1109/ICSE.2004.1317478](https://doi.org/10.1109/ICSE.2004.1317478), pp. 563–572 (the often-quoted
`10.1145/999243.1000359` is **not** that paper); the TSE 2005 journal version is
[10.1109/TSE.2005.72](https://doi.org/10.1109/TSE.2005.72), 31(6):429–445. Robbes, Pollet & Lanza is
"Logical Coupling Based on Fine-Grained Change Information",
[10.1109/WCRE.2008.47](https://doi.org/10.1109/WCRE.2008.47). **UNVERIFIED:** Zimmermann 2004/2005
full text (association-rule support/confidence thresholds, ROSE precision/recall) and D'Ambros,
Lanza & Robbes, WCRE 2009, DOI
[10.1109/WCRE.2009.19](https://doi.org/10.1109/WCRE.2009.19) (abstract unobtainable here) — no number
is asserted for either.

---

## 6. File-level "import count" / in-degree specifically

### 6.1 The one paper that reports in-degree for a large industrial dependency graph

**Zimmermann & Nagappan 2008**, ICSE 2008, DOI
[10.1145/1368088.1368161](https://doi.org/10.1145/1368088.1368161) — **FULL TEXT** read (Windows Server
2003 binaries). This is the standard "network analysis works" citation, and its internal ordering is
actually **unfavourable to in-degree**. Table 4 (Spearman correlation with **number of defects**):

| Measure | Ingoing | Outgoing | Symmetric |
| --- | --- | --- | --- |
| Degree | **.283** | **.440** | **.462** |

and in the same table, complexity/size metrics: `Lines Total .516`, `Parameters Total .521`,
`FanIn Total .502`, `FanOut Total .493`. So on the paper's own data **in-degree (.283) loses to
out-degree (.440), to fan-in totals, and to plain lines of code (.516)**. Verbatim (FULL TEXT):

> "Degree centrality. The degree measures the number of dependencies for a binary."

Their headline claim (verified verbatim from the MSR abstract) is about the *whole family* of network
measures, not in-degree alone:

> "we found that the recall for models built from network measures is by 10% points higher than for models built from complexity metrics"

For the developer-curated "escrow" binaries, `GlobalInDegree` recall = **0.50** vs `TotalFanIn` /
`TotalFanOut` / `TotalLines` = 0.30; the winners were closeness/`dwReach` variants at **0.60**
(Table 3). **In-degree alone is not the reported winner.** Unit is binaries, not source files.

### 6.2 The direct negative results

| Study | Unit | Result | Quote | Status |
| --- | --- | --- | --- | --- |
| Kitchenham, Pickard & Linkman 1990, *Software Engineering Journal* 5(1):50–58, [10.1049/sej.1990.0007](https://doi.org/10.1049/sej.1990.0007) | program (module), Henry & Kafura info-flow metrics | **only fan-out predictive; fan-in not; LOC/branches better than both** | "Although one of the design metrics (informational fan-out) was able to identify change-prone, fault-prone and complex programs, code metrics (i.e. lines of code and number of branches) were better." | ABSTRACT ONLY |
| Bhattacharya, Iliofotou, Neamtiu & Faloutsos, ICSE 2012, [10.1109/ICSE.2012.6227173](https://doi.org/10.1109/ICSE.2012.6227173) | **functions and modules (files)** of 11 OSS systems over ~a decade | in- and out-degree both poor bug-severity predictors; a PageRank-like NodeRank worked | "The node degree is not a good predictor of bug severity… note how in- and out-degrees are poor bug severity predictors." | FULL TEXT |
| Mens 2016, arXiv:1608.01533 (Wiley-IEEE book chapter; not peer-reviewed journal) | survey | fan-out related to quality, **fan-in not**; and the size confound runs *negative* | "Kitchenham found fan-out to be related to the aforementioned quality characteristics, while fan-in was not… modules with a large fan-in tend to be relatively small" | FULL TEXT (preprint) |
| Bird, Nagappan, Gall, Murphy & Devanbu, ISSRE 2009, [10.1109/ISSRE.2009.17](https://doi.org/10.1109/ISSRE.2009.17) | Windows Vista binaries | dependency-only network (which contains in-degree) is not superior to combined/socio-technical models | "neither the dependency network model nor the contribution network model were superior to either the combined or socio-technical models" | FULL TEXT |
| Nguyen, Adams & Hassan, ICSM 2010, [10.1109/ICSM.2010.5609560](https://doi.org/10.1109/ICSM.2010.5609560) | Eclipse modules | only "a small subset" of network measures matter — **which ones unverified** | — | ABSTRACT ONLY |

**Correction to a common miscitation (verified in this session):** Card, Church & Agresti 1986 is
**not** the "fan-in/fan-out unrelated to defect rate" result. The correct citation is
D. N. Card, V. E. Church, W. W. Agresti, "An empirical study of software design practices", IEEE TSE
SE-12(2):264–271, DOI [10.1109/TSE.1986.6312942](https://doi.org/10.1109/TSE.1986.6312942) — in it,
fan-in/fan-out appear only as a *module classifier* (nonterminal/terminal/utility), not as a tested
quality predictor; the null result concerns data/common coupling. (The DOI sometimes quoted,
10.1109/TSE.1986.6312935, is a different paper in the same issue.) The correct primary null is
**Kitchenham et al. 1990** above.

### 6.3 The one pro result — and it is about *change-proneness*, at class level

**Vasa, Schneider & Nierstrasz, ICSM 2007**, "The Inevitable Stability of Software Change",
DOI [10.1109/ICSM.2007.4362613](https://doi.org/10.1109/ICSM.2007.4362613). Abstract verbatim
(ABSTRACT ONLY):

> "Classes that tend to be modified, however, are also the more popular ones, that is, those with greater Fan-In."

> "Common wisdom, for example, states that in a well-designed object-oriented system, the more popular a class is, the less likely it is to change from one version to the next"

This explicitly **contradicts the "popular ⇒ stable" intuition** and is the best available pro citation
— but it is **class-level** and about **change-proneness**, not defect count, and its unit is type
dependencies, not file imports.

### 6.4 Why file in-degree is confounded (and can be *negatively* related to size)

- **Preferential attachment.** Louridas, Spinellis & Vlachos, "Power laws in software", ACM TOSEM
  18(1), DOI [10.1145/1391984.1391986](https://doi.org/10.1145/1391984.1391986); and Wang et al.,
  ICSM 2009, DOI [10.1109/ICSM.2009.5306348](https://doi.org/10.1109/ICSM.2009.5306348) (Linux call
  graphs over 223 versions: *"very strong preferential attachment tendency"*). In-degree accumulates
  over **time/popularity**, so a fan-in axis is partly an **age** axis.
- **The size confound may run the *other* way at module level.** Mens 2016: *"modules with a large
  fan-in tend to be relatively small"* — the opposite of the class-level positive fan-in↔size
  correlation. qingluan should test this on its own corpus rather than assume either direction.
- **Not found:** any study that regresses file in-degree on file **age** explicitly, or that tests
  source-file import in-degree as an isolated predictor with size/churn controls. Ecosystem
  package-level work (Decan, Mens & Constantinou MSR 2018, DOI
  [10.1145/3196398.3196401](https://doi.org/10.1145/3196398.3196401); Bogart et al. FSE 2016; Kula et
  al.) shows high in-degree is a **risk-exposure / blast-radius** notion — many dependents can break
  — but establishes no defect or effort prediction, and the ecosystems "differ substantially in their
  practices and expectations toward change".

---

## 7. Master findings table

| # | Study | Unit / what measured | Result | Limitations | Link |
| --- | --- | --- | --- | --- | --- |
| 1 | Henry & Kafura 1981, IEEE TSE SE-7(5) | procedure/module info-flow `length·(fan-in×fan-out)²` vs maintenance | origin of fan-in; **exact correlation & limitations UNVERIFIED** | single legacy OS; composite formula never standardised | [doi](https://doi.org/10.1109/TSE.1981.231113) |
| 2 | Martin 2000 (objectmentor PDF) | Ca/Ce/I, SDP/SAP | **convention only** — no data, no statistics | not empirical | [archive](https://web.archive.org/web/20150906155800id_/http://www.objectmentor.com/resources/articles/Principles_and_Patterns.pdf) |
| 3 | Basili, Briand & Melo 1996, TSE 22(10) | 180 C++ classes, logistic regression | NOC (fan-in proxy) protective in prose; CBO/RFC positive | tables contradict prose; class-level | [doi](https://doi.org/10.1109/32.544352) |
| 4 | Glasberg, El Emam, Melo & Madhavji 2000, NRC | commercial Java, export vs import coupling | both positive; **fan-out stronger** (+0.47 vs +0.32) | class-level, single system | [pdf](https://www.ehealthinformation.ca/web/default/files/wp-files/2000-Validating-Object-oriented-Design-Metrics.pdf) |
| 5 | Tahir, Bennin, Xiao & MacDonell 2021, EMSE | 23 systems, mediation/moderation | **fan-in weakest**; fan-out strongest; fan-in least size-mediated | class-level | [doi](https://doi.org/10.1007/s10664-021-09991-3) / arXiv:2106.04687 |
| 6 | Tahir, Bennin, MacDonell & Marsland 2018, ESEM | 17 systems, mediation/moderation | **no strong evidence size mediates** OO-metric effects | **repo §3.4 misreads this paper** | [doi](https://doi.org/10.1145/3239235.3239243) / arXiv:2104.12349 |
| 7 | El Emam, Benlarbi, Goel & Rai 2001, TSE 27(7) | large C++ telecom framework, CK+L&K, fault-proneness (binary) | "After controlling for size, none of the metrics we studied were associated with fault-proneness any more." | class-level, single system, no OA full text | [doi](https://doi.org/10.1109/32.935855) |
| 7b | Subramanyam & Krishnan 2003, TSE 29(4) | C++ and Java industry classes, WMC/CBO/DIT + SIZE, defect **count** | "even after controlling for the size of the software, these metrics are significantly associated with defects"; **CBO negative for Java, positive for C++** | industry-only; CBO sign unstable | [doi](https://doi.org/10.1109/TSE.2003.1191795) |
| 7c | Evanco 2003, TSE 29(7) | comment on El Emam | size is not a valid confounder: "the ability to measure size does not temporally precede the ability to measure many of the object-oriented metrics" | comment, no new data | [doi](https://doi.org/10.1109/TSE.2003.1214331) |
| 7d | Zhou, Xu, Leung & Chen 2014, TOSEM 23(1) | OSS systems, size confound in fault prediction | confound "in general exists"; de-confounded models predict **better** | abstract only | [doi](https://doi.org/10.1145/2556777) |
| 7e | Gil & Lalouche 2017, EMSE (negative-results section) | 26 metrics incl. CK | "metrics controlled for size, tend to eliminate their predictive capabilities"; "code size is the only 'unique' valid metric" | abstract only; per-metric R² unverified | [doi](https://doi.org/10.1007/s10664-017-9513-5) |
| 7f | Nagappan, Ball & Zeller 2006, ICSE | 5 Microsoft systems >1M LOC, coupling incl. FanIn/FanOut | **null/negative**: "not a single metric that would correlate … in all five projects"; ClassCoupling −0.303 (proj D); FanIn Max −0.197 (proj A) | signs verified, significance not | [doi](https://doi.org/10.1145/1134285.1134349) |
| 7g | Child, Rosner & Counsell 2019, JSS 151 | ~10 CBO variants, large Java system | **split**: half the variants "have no practical application to the prediction of defects", the rest "sound" | answer depends on CBO definition | [doi](https://doi.org/10.1016/j.jss.2019.02.020) |
| 7h | Olague et al. 2007, IEEE TSE | MOOD vs CK suites, 6 Rhino versions | MOOD's coupling component is a poor predictor; CK's CBO is not | abstract only | [doi](https://doi.org/10.1109/TSE.2007.1015) |
| 8 | Nagappan & Ball 2005, ICSE | Windows Server 2003, 2465 binaries, relative churn | relative churn R²=**0.811**, discrimination **89.0%**; absolute churn poor | single system; churn ≠ fan-in; relative measures are LOC-normalised | [doi](https://doi.org/10.1145/1062455.1062514) |
| 9 | Graves, Karr, Marron & Siy 2000, TSE 26(7) | 1.5M-LOC legacy telecom system, ~2yr deltas | deltas+age deviance 697.4 vs LOC-only 1271.4; time-damped churn best 631.0; developers & module connectivity non-predictors | **SECONDARY (lecture slides) — abstract/body not verified** | [doi](https://doi.org/10.1109/32.859533) |
| 10 | Hassan 2009, ICSE | change entropy, NetBSD/FreeBSD/OpenBSD/Postgres/KDE/KOffice | change entropy beats prior-modifications/prior-faults by 13–42% (5/6 systems, p<.05; KOffice n.s.) | **no static-complexity baseline run**; not Eclipse | [doi](https://doi.org/10.1109/ICSE.2009.5070510) |
| 10b | Rahman & Devanbu 2013, ICSE | 85 releases / 12 projects | "code metrics … are generally less useful than process metrics for prediction"; code metrics have "high stasis" | their own paper notes contrary results (Menzies; Arisholm) | [doi](https://doi.org/10.1109/ICSE.2013.6606589) |
| 10c | Kamei et al. 2013, TSE 39(6) | 6 OSS + 5 commercial, just-in-time | avg accuracy 68%, recall 64%; 20% effort → 35% of defect-inducing changes | abstract only | [doi](https://doi.org/10.1109/TSE.2012.70) |
| 11 | Bird et al. 2011, ESEC/FSE | Vista + Win7, ownership | ownership measures relate to pre/post-release failures | Microsoft-only; **no head-to-head vs structure claimed** | [doi](https://doi.org/10.1145/2025113.2025119) |
| 12 | Oliva & Gerosa 2015, ISSRE | 4 Java projects, deps vs co-change | dependence raises co-change rate but **modal case is no co-change** | open-source Java | [doi](https://doi.org/10.1109/ISSRE.2015.7381818) |
| 13 | Oliva 2016 PhD thesis | 45 ASF projects, 77,286 snapshots | **32% co-change when dependent (σ 13.6%); AUC 0.52–0.76; majority of co-changes unexplained by structure** | large-scale chapter not journal-refereed | [pdf](https://teses.usp.br/teses/disponiveis/45/45134/tde-20230727-113255/publico/OlivaGustavoAnsaldi.pdf) |
| 13b | Ajienka & Capiluppi 2017, JSS | 79 projects, #references vs co-change confidence | significant in **<10 of 79**; "A stronger structural coupling does not imply a higher co-change likelihood"; 1–44% of co-change pairs are structurally coupled | OSS; correlation-based | [doi](https://doi.org/10.1016/j.jss.2017.08.042) |
| 14 | Gall, Hajek & Jazayeri 1998, ICSM | 20 releases, 10 MLOC telecom system | logical coupling = identical change subsequences; reveals hidden deps **qualitatively** | **no structural measurement, no percentage** | [doi](https://doi.org/10.1109/ICSM.1998.738508) |
| 15 | Zimmermann & Nagappan 2008, ICSE | Windows Server 2003 **binaries**, dependency graph | network *family* +10pts recall vs complexity; but **in-degree .283 < out-degree .440 < LOC .516** | single proprietary OS; binary-level; in-degree not the winner | [doi](https://doi.org/10.1145/1368088.1368161) |
| 15b | Kitchenham, Pickard & Linkman 1990, Softw. Eng. J. 5(1) | module-level info-flow metrics | **fan-out predictive; fan-in not; LOC/branches better** | abstract only | [doi](https://doi.org/10.1049/sej.1990.0007) |
| 15c | Vasa, Schneider & Nierstrasz 2007, ICSM | 12 Java projects, type-dep graph | **greater Fan-In ⇒ more likely to change** (contradicts "popular ⇒ stable") | class-level; change-proneness not defects | [doi](https://doi.org/10.1109/ICSM.2007.4362613) |
| 15d | Bhattacharya et al. 2012, ICSE | functions + modules, 11 OSS systems | "in- and out-degrees are poor bug severity predictors"; NodeRank (PageRank-like) wins | model-based benchmark | [doi](https://doi.org/10.1109/ICSE.2012.6227173) |
| 16 | CodeScene docs 7.5.1 + Code Red + Ghost Echoes | hotspot = dev activity; health = separate axis | hotspot axis is **change frequency**; coupling = **observed**; Ghost Echoes composite AUC 0.95 **tied** naive LoC 0.95 | vendor docs/papers; no independent hotspot evaluation found | [docs](https://docs.enterprise.codescene.io/versions/7.5.1/guides/technical/hotspots.html) · [doi](https://doi.org/10.1109/ICSME58944.2024.00072) |
| 17 | D'Ambros, Lanza & Robbes 2010, MSR | 15 bug-prediction approaches | paper includes FanIn/FanOut but reports only metric-set scores | **full text NOT obtainable — the "entropy ranked best" claim is UNVERIFIED** | [doi](https://doi.org/10.1109/MSR.2010.5463279) |
| 17b | Cai 2018, IEEE TSE 44(4) | dynamic impact analysis, 10 Java subjects | "Static analysis can produce safe but overly-conservative impact sets"; DIVER sets 35–50% the size | dynamic only, per-execution | [doi](https://doi.org/10.1109/TSE.2017.2692783) |

---

## 8. Implications for qingluan

1. **Do not add a composite `fan-in × churn` score.** No primary source validates such a product;
   CodeScene, the only shipping implementation, keeps the axes separate and ranks with a model it
   does not publish. This is the same conclusion §4 of `code-length-metrics.md` reached for MI /
   SIG / CodeScene / Sonar — the pattern here is identical.
2. **Raw import fan-in is a *descriptive* structural axis, not a risk score.** Defensible one-line
   claim: *"N other files import this file; a signature change here reaches N call sites."* That is
   arithmetic on the graph, not a defect prediction. The class-level literature does not support a
   stronger claim, and the file-level co-change evidence actively undercuts "blast radius".
3. **If a churn axis is added, it is the *frequency* axis and must stay separate and observed.**
   "Changes touching this file in the last 90 days" is directly computable from git/jj history,
   needs no AST, and is the axis the fault literature actually backs (Graves/Nagappan/Hassan). Present
   it as a **ranked list / threshold union**, exactly like the existing length thresholds.
4. **Never use fan-in alone to claim co-change.** Oliva's 32% and AUC ≤ 0.76 are the numbers to cite
   if anyone asks why not. If qingluan ever mines its own history, it can compute **observed coupling**
   (co-change degree — CodeScene's "sum of couplings") and show structural fan-in *next to* it; the
   gap between the two is itself the honest finding.
5. **Report fan-in with nloc and age beside it.** Otherwise file in-degree mostly re-ranks by size and
   age, the exact confound the repo already guards against at function level. Note the confound's
   *sign* at module level is disputed: Mens 2016 reports *"modules with a large fan-in tend to be
   relatively small"*, while class-level studies find fan-in positively correlated with size — a
   discrepancy qingluan can settle on its own corpus.
6. **If the repo's `code-length-metrics.md` is ever revised, apply the corrections in §10** — the
   existing §3.4 misreads Tahir et al. 2018 and misattributes the 2003 TSE comment.

---

## 9. Not verified / still open

| Item | Status |
| --- | --- |
| Henry & Kafura 1981 exact procedure count; publisher-version text | **UNVERIFIED** — full text read from an unofficial HTML mirror; PDF paywalled |
| Shepperd & Ince 1994, Ince & Shepperd 1989, Troy & Zweben 1981 (correct DOI 10.1016/0164-1212(81)90031-5), Pickard & Carter 1995, Radjenović 2013 SLR | **UNVERIFIED** — paywalled, no accessible abstract or OA copy |
| Any peer-reviewed statement that the H&K formula was "never independently replicated at scale" | **NOT FOUND** — phrasing exists only in a non-peer-reviewed industry doc |
| Graves et al. 2000 exact abstract/body numbers | **UNVERIFIED** — IEEE closed, no OA, all aggregators blocked; only lecture slides (secondary) available |
| D'Ambros, Lanza & Robbes 2010 MSR full text | **CONFLICTING** — one verifier read an institutional copy (metric catalogue only), another could reach no copy; the "relative churn/entropy ranked best among 15 approaches" claim is **UNVERIFIED** |
| El Emam et al. 2001 exact metric table, system name, pre/post odds ratios | **UNVERIFIED** — abstract sentence itself is now verified; full text paywalled with no OA copy |
| Subramanyam & Krishnan 2003 exact CBO coefficients | **UNVERIFIED** — only the narrative direction extracted (CBO + for C++, − for Java) |
| Nagappan, Ball & Zeller 2006 metric catalogue | **RESOLVED** — FULL TEXT obtained; coupling (ClassCoupling/FanIn/FanOut) was measured. Remaining gap: statistical significance of the negative coefficients (bold lost in PDF text layer) |
| Briand, Wüst, Daly & Porter 2000, JSS 51(3), 10.1016/S0164-1212(99)00102-8 | **NOTHING VERIFIED** — no abstract deposited in Crossref/OpenAlex/S2; ScienceDirect 403. **Do not cite it** for a fan-in/fan-out or size-control claim |
| Gyimóthy, Ferenc & Siket 2005 TSE full text; Gil & Lalouche 2017 full text; Zhou et al. 2014 full text; Olague et al. 2007 full text; Child et al. 2019 (full text obtained) | Gyimóthy/Zhou/Gil/Olague **UNVERIFIED** (abstract or same-author restatement only) |
| El Emam et al. 2001 primary text vs the S&K secondary claim that coupling survived size control | The secondary claim **contradicts** El Emam's abstract and is unresolved (primary paywalled) — do not repeat it |
| A published reply by El Emam et al. to Evanco 2003 | **UNVERIFIED** — not located |
| Any **independent** (non-CodeScene) peer-reviewed evaluation of CodeScene's hotspot construction | **NOT FOUND** — Code Red is vendor-coauthored; Ghost Echoes is vendor-coauthored and studies maintainability, not hotspots |
| CodeScene Code Red / Ghost Echoes exact venues and significance tests | Partly **UNVERIFIED** (venue string; no significance testing in Ghost Echoes) |
| File-level import in-degree as an isolated predictor with size/churn control | **NOT FOUND** in the literature |
| Troy & Zweben 1981 primary text; Geipel & Schweitzer TSE 2012 full text; Arnold & Bohner survey; Oliva & Gerosa SBES 2011 primary (the 91% figure) | **UNVERIFIED** — cited only secondarily |
| Hong et al. / Kula et al. / Decan TSE 2019 ecosystem in-degree | **UNVERIFIED** — abstracts not resolved; ecosystem evidence is qualitative only |
| Le Goues & Weimer churn-vs-cyclomatic-complexity F values | **UNVERIFIED** — exact bibliographic identity of the preprint not pinned down |

## 10. Corrections this report makes to existing repo notes

These concern `docs/research/code-length-metrics.md` (not edited here — recorded so the owner can act):

1. **§3.4 misreads Tahir et al. ESEM 2018.** The "class size fully mediates RFC/CBO/LCOM/Fan-in/Fan-out"
   sentence is scoped to **Apache Lucene 2.4 alone**; the paper's conclusion is *"We are unable to
   confirm if class size has a significant mediation or moderation effect…"*. Authors are Tahir,
   Bennin, MacDonell & Marsland, **not Rasool/Gencel**.
2. **§3.4 attributes the 2003 TSE critique to Briand et al.** The actual comment is by **W. Evanco**
   (DOI 10.1109/TSE.2003.1214331).
3. **§3.4's "subsequent TOSEM 2014 study" is Zhou, Xu, Leung & Chen**, "An in-depth study of the
   potentially confounding effect of class size in fault prediction", TOSEM 23(1), DOI
   10.1145/2556777 — and its finding is *weaker* than "none of the metrics survive": it says
   de-confounded models predict **better**.
4. **§3.2/§3.4 implication for Tahir is inverted:** fan-in is among the metrics with the **least**
   evidence of size mediation (2021 EMSE), while **fan-out/CBO** are the more size-mediated ones.
5. **§2's framing of Nagappan & Ball survives** but should add: the winning churn measures are
   **LOC-normalised ratios**, so "relative churn" embeds size as its denominator — which is exactly
   the "size is a good denominator" reading in §3.5.
6. **Numerical sourcing:** cite Basili et al. 1996 numbers from the published **TSE** PDF, not the UMD
   technical report — the tech-report copy prints the signs **inverted** relative to the paper's own
   prose.
7. **Do not repeat the circulating secondary claim** that El Emam et al. 2001 found coupling and
   inheritance *survived* size control. It appears in Subramanyam & Krishnan's related work and
   **contradicts El Emam's own abstract**.

*This file is the deliverable. Sub-agent raw notes were working files and are not part of it.*
