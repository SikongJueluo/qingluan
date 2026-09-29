# Should length contribute to the complexity number?

Primary-source research for the qingluan function-level complexity CLI, which currently emits a
per-function **vector** (`cc`, `cognitive`, `nloc`, `params`, `maxNesting`) and deliberately
refuses to invent a composite score (`docs/research/code-complexity-metrics.md` §3,
`.scratch/complexity/spec.md`). The question here: should function length (and file length) be
**folded into a score**, kept as a **separate axis**, or combined some **third way**?

**Research date:** 2026-09-30.
**Method:** every claim below traces to a primary artifact that was actually opened and read —
the paper/abstract via a publisher, author page, NASA/NTRS/Internet Archive copy, or Crossref /
OpenAlex / Semantic Scholar record; first-party tool docs/source; or an official standard. PDFs
were read as extracted text (`pdftotext`), not summaries. Quotes are verbatim and ≤ 40 words;
long passages are not copied into the repo. Verification status is marked per claim:

| Marker | Meaning |
|---|---|
| **[full]** | primary full text read in this pass |
| **[abstract]** | the artifact's abstract (publisher/OpenAlex/virascience mirror), full text not obtained |
| **[first-party doc]** | vendor/tool documentation or source code, read directly |
| **[unverified]** | not confirmed against a primary source in this pass — treat as a lead, not a finding |

A companion research note on the exact counting algorithm already exists at
`research/logical-sequences.md`; this report does not repeat it.

---

## TL;DR — the verdict

**Keep length as a separate axis. Do not fold it into a complexity score.** Optionally add a
**derived, separately-labelled diagnostic** (decision density `cc / nloc`, i.e. decisions per
line) so a human can tell "long but flat" from "short but branchy", and drive any
"is this bad?" signal from a **union of per-axis threshold breaches**, never from a weighted sum.

Three findings carry the verdict:

1. **Size is a genuine, independent axis** — but the *absolute* size→defect relation is weaker and
   more shape-dependent than folklore claims. McCabe's own NIST handbook states the two are
   independent ([NIST SP 500-235 §3.1](#31-mccabe--nist-sp-500-235)).
2. **Length and cyclomatic complexity are *not* strongly collinear at function level.** The largest
   study (17.6M Java methods, 6.3M C functions) measures SLOC–CC R² = 0.40 / 0.44, Spearman
   ρ = 0.80 / 0.83 (log-transformed R² 0.68 / 0.71), and concludes CC is *not* redundant with SLOC
   ([Landman et al. 2016](#6-correlation-between-length-and-complexity-in-practice)). The near-perfect
   collinearity appears only after **aggregating** CC to file/class level (R² rises to 0.64–0.73) —
   exactly what qingluan does not do. A 2026 pre-registered study adds that length predicts measured
   human cognitive load *better* than cyclomatic complexity does ([§6.2](#62-does-length-predict-human-difficulty-better-than-complexity-icer-2026)).
3. **Every existing composite's weights are subjective, norm-referenced, or proprietary** — none
   is an independently replicated regression of maintainability on its terms
   ([composite table](#4-composite-scores-that-exist-and-their-track-record)). Folding length in
   would import unvalidated weights into a tool that has so far been honest about scope.

The confounding critique (§2) is real but is a **class-level** result about *other* metrics
(RFC, WMC, LCOM, DIT, NOC), not a reason to delete function length. Its correct lesson for
qingluan is: never let size masquerade *as* complexity — which is precisely what a separate
`nloc` column plus a derived density already achieves.

---

## 1. Evidence that size predicts defects / maintainability

### 1.1 Basili & Perricone 1984 — the origin of the inverse fault-density curve

- **Citation:** V. R. Basili, B. T. Perricone, "Software errors and complexity: an empirical
  investigation", *Communications of the ACM* 27(1):42–52, Jan 1984.
  **DOI [10.1145/69605.2085](https://doi.org/10.1145/69605.2085)** *(note: the brief's
  `10.1145/69605.80124` does not resolve in Crossref; the correct DOI is `…69605.2085`.)*
  Full text read via the NASA technical report version, "Software Errors and Complexity: An
  Empirical Investigation", TR-1195, Aug 1982, [NTRS 19870015469](https://ntrs.nasa.gov/citations/19870015469)
  (obtained through the Internet Archive capture). **[full]**

- **What they measured:** 517 Fortran modules of one satellite-planning system; error counts per
  module normalised by executable lines; cyclomatic complexity per module.
- **What they found (Table 4 — errors/1000 executable lines by module size):** 50 → **16.0**,
  100 → 12.6, 150 → 12.4, 200 → 7.6, >200 → **6.4**.
  > "Table 4 implies that there is a higher error rate within smaller sized modules."
- **(Table 5 — average cyclomatic complexity by module size):** 50 → **6.0**, 100 → 17.9,
  150 → 28.1, 200 → 52.7, >200 → **60.0**.
  > "the larger modules were more complex than smaller modules."
- **Limitations, in their words:** they offer several *tentative* explanations and do not settle
  on one — "the majority of the modules examined were small… causing a biased result", larger
  modules may have been "coded with more care", and larger modules may still contain undetected
  errors because "all the 'paths' within the larger modules may not yet have been fully
  exercised." **[full]**
- **Why it matters here:** this is the source of the "small components have higher fault density"
  trope. Note the *confound inside their own data*: complexity rises steeply with size in the same
  five buckets, so "size" and "complexity" are not separated by this study.

**A critical methodological note on the famous R² = 0.94.** The Landman et al. literature survey
records that the high correlation usually attributed to this paper came from the five aggregated
buckets, not from the modules:

> "No correlation between module SLOC and module CC. Then the authors grouped modules into five
> buckets (by size) and calculated the average CC per bucket. Over these five data points, they
> reported the high correlation." — Landman et al. 2016, Table I comment on Basili & Perricone **[full]**

### 1.2 Hatton 1997 — the U-bend, and the author's later retraction

- **Citation:** L. Hatton, "Reexamining the Fault Density–Component Size Connection", *IEEE
  Software* 14(2):89–97, 1997. **DOI [10.1109/52.582978](https://doi.org/10.1109/52.582978)**.
  Full text read via the author's own page, <https://www.leshatton.org/Documents/Ubend_IS697.pdf>. **[full]**
- **What he measured:** a synthesis of ~9 prior case studies across Ada, C, C++, Fortran, Pascal
  and assembly, all reporting the same phenomenon; he fitted a logarithmic-then-quadratic fault
  model and a two-level human-memory explanation.
- **What he found:**
  > "larger software components are proportionately much more reliable than smaller software
  > components within the same system up to a certain size after which they rapidly deteriorate."
  The inflection is at "around 200 lines or so" (abstract); the fault curve is quadratic beyond it.
- **Limitations he states:** "5 studies do not make a water-tight case"; there is "no standard
  definition of fault, nor of line of code", with a possible 25:1 variation in severity and 2:1 in
  LOC definitions; and the model "needs to be subjected to further experiments."
- **Refutation — by the author himself. [full]** On the publication page
  <https://www.leshatton.org/IEEE_Soft_97b.html>, Hatton writes:
  > "September 2009. I no longer believe in the U-bend described in this paper. I used data
  > available at the time but meanwhile 10 years later, I have a lot more higher quality data and
  > a different and more convincing model… defect growth and component size appear inextricably
  > linked through the mechanism of information theory."
- **Net:** the U-shape is retracted by its own author; what survives is the direction — **size and
  defect growth are linked** — not the specific 200-line minimum.

### 1.3 Nagappan & Ball 2005 — absolute size is a *poor* predictor; relative churn is good

- **Citation:** N. Nagappan, T. Ball, "Use of relative code churn measures to predict system defect
  density", *ICSE 2005*, pp. 284–292. **DOI [10.1145/1062455.1062514](https://doi.org/10.1145/1062455.1062514)**.
  Full text read via Microsoft Research:
  <https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/icse05churn.pdf>. **[full]**
- **What they measured:** Windows Server 2003, churn from version history; compared *absolute*
  churn (raw line counts) against *relative* churn (churn normalised by component size, file count,
  temporal extent); DV = post-release defect density.
- **What they found:**
  > "while absolute measures of code churn are poor predictors of defect density, our set of
  > relative measures of code churn is highly predictive of defect density."
  and they cite the general result:
  > "Studies have shown that absolute measures like LOC are poor predictors of pre- and post
  > release faults [7] in industrial software systems."
- **Limitations:** single industrial system (Windows Server 2003), so external validity is narrow;
  and the finding is about *change*, not static length.
- **Why it matters here:** this is the strongest caution against treating *absolute* function
  length as a defect oracle. Size becomes informative when **normalised** — which is itself an
  argument for keeping `nloc` as a denominator/axis rather than adding it as a magnitude.

### 1.4 Later work and refutations — status

- The most direct later test of the size-confounder question is **Jiarpakdee, Tantithamthavorn,
  Ihara, Matsumoto 2014/2018, "An in-depth study of the potentially confounding effect of class
  size in fault prediction"**, *ACM TOSEM*. **DOI [10.1145/2556777](https://doi.org/10.1145/2556777)**.
  **[abstract]** via OpenAlex:
  > "the confounding effect of class size on the associations between object-oriented metrics and
  > fault-proneness in general exists"
  > "We should remove the confounding effect of class size when building fault prediction models."
  This is a **confirmation** of El Emam's direction on open-source systems, not a refutation.
- **Gap [unverified]:** a systematic search for further replications/refutations of Basili &
  Perricone and of El Emam was begun but not completed in this pass. Do not present §1 as an
  exhaustive survey.

---

## 2. The confounding critique — what El Emam et al. actually concluded

This is the canonical statement of the "size dominates" position, and it is routinely
mis-summarised. Its actual conclusion is narrower and more precise than "complexity metrics are
worthless".

- **Citation:** K. El Emam, S. Benlarbi, N. Goel, S. N. Rai, "The confounding effect of class size
  on the validity of object-oriented metrics", *IEEE TSE* 27(7):630–650, 2001.
  **DOI [10.1109/32.935855](https://doi.org/10.1109/32.935855)**. **[abstract]**, obtained from
  OpenAlex (partial) and a verbatim mirror of the published abstract
  (<https://www.virascience.com/document/06c2a244487ccf50aeda239756367e1938cf55d9/>). Full text not
  obtained (all open copies checked — KSU course mirror, W&lt;ayback, CiteSeerX, scholar.archive.org —
  were 403/404/rate-limited).

**What they did:** a large C++ telecommunications framework; independent variables = Chidamber &
Kemerer metrics plus a subset of Lorenz & Kidd metrics; dependent variable = incidence of a field
failure attributable to a fault (class fault-proneness); compared models with and without size.

**The actual conclusion, verbatim [abstract]:**

> "Our findings indicate that before controlling for size, the results are very similar to previous
> studies: the metrics that are expected to be validated are indeed associated with fault-proneness.
> **After controlling for size none of the metrics we studied were associated with fault-proneness
> anymore.** This demonstrates a strong size confounding effect, and casts doubt on the results of
> previous object-oriented metrics validation studies."

> "It is recommended that previous validation studies be re-examined to determine whether their
> conclusions would still hold after controlling for size, and that future validation studies
> should always control for size."

Also, from the opening of the abstract:

> "However, none of these studies control for the potentially confounding effect of class size. In
> this paper we show a strong size confounding effect, and question the results of previous
> object-oriented validation studies."

**Crucial scope limits — read these before generalising:**

1. **Level of analysis: class.** The unit is the *class*; size = class size. The finding is that at
   *class* granularity, the OO metrics (RFC, WMC, LCOM, DIT, NOC, …) lose their association with
   fault-proneness once class size is controlled. It is **not** a statement about function length
   vs. function-level cyclomatic or cognitive complexity.
2. **The metrics are *other* metrics.** "None of the metrics we studied" are the C&amp;K/Lorenz–Kidd
   suite — not nloc-vs-CC at function level.
3. **It was contested.** *IEEE TSE* published a comment: L. Briand et al. (2003), "Comments on
   'The confounding effect of class size on the validity of object-oriented metrics'",
   **DOI [10.1109/TSE.2003.1214331](https://doi.org/10.1109/TSE.2003.1214331)**. **[abstract]** via OpenAlex:
   > "We take issue with this perspective since the ability to measure size does not temporally
   > precede the ability to measure many of the object-oriented metrics that have been
   > proposed. Hence, the condition that a confounding variable must occur causally prior to
   > another explanatory variable is not met."
   So the strong reading ("size explains everything") is itself disputed on causal-grounding
   grounds, and later work ([TOSEM 2014](#14-later-work-and-refutations--status)) still found the
   confound to exist operationally.

**What this means for qingluan (the actual decision).** The confound warns against letting a
*class-level aggregate* of derived metrics be read as quality when it is mostly size. qingluan
computes per-function `cc` and `cognitive` on the function's own control flow. The correct
precaution is therefore **not** to fold `nloc` in (which would *increase* size's share of the
number) but to (a) keep `nloc` visible so a reader can discount a big-but-flat function, and
(b) never sum per-function `cc` into a file/class score and call that "complexity" — because
*that* aggregate is the thing Landman et al. showed is really a size measure.

---

## 3. What the metric authors themselves say

### 3.1 SonarSource Cognitive Complexity white paper

- **Artifact:** G. A. Campbell, "Cognitive Complexity — a new way of measuring understandability",
  SonarSource, **v1.7, 29 Aug 2023**. Read in full from the official PDF
  <https://www.sonarsource.com/docs/CognitiveComplexity.pdf> (via its Internet Archive capture;
  `web_fetch` cannot parse PDFs). **[full]**

**On length/size vs complexity (Introduction, p.4):**

> "Beyond the class level, it is widely acknowledged that the Cyclomatic Complexity scores of
> applications correlate to their lines of code totals. In other words, Cyclomatic Complexity is
> of little use above the method level."

> "it is impossible to know whether any given class with a high aggregate Cyclomatic Complexity is
> a large, easily maintained domain class, or a small class with a complex control flow."

This is a first-party admission that **above method level, CC is a size proxy** — and that knowing
*which* of the two you have is the actual problem. It supports keeping size visible as its own axis;
it does not support adding size into a complexity number.

**On nesting vs size (§ "Increment for nested flow-break structures", p.8):**

> "It seems intuitively obvious that a linear series of five if and for structures would be easier
> to understand than that same five structures successively nested, regardless of the number of
> execution paths through each series."

> "each time a structure that causes a structural or hybrid increment is nested inside another such
> structure, a nesting increment is added for each level of nesting."

Nesting is thus a *shape* property, explicitly justified by intuition about comprehension — not by
line count. It is a third axis distinct from both size and branch count.

**On what the metric deliberately does not measure (Abstract, §"Ignore shorthand", Conclusion):**

> "it accurately calculates the minimum number of test cases required to fully cover a method, it is
> not a satisfactory measure of understandability" *(said of CC)*

> "The processes of writing and maintaining code are human processes. Their outputs must adhere to
> mathematical models, but they do not fit into mathematical models themselves."

> "1. Ignore structures that allow multiple statements to be readably shorthanded into one"

**Important negative finding:** the white paper contains **no explicit instruction about combining
the metric with length or any other metric**, and no formula that includes LOC. It presents
Cognitive Complexity as a standalone metric. Any claim that "Sonar says don't combine" is *not*
supported by the white paper text. **[full]**

### 3.2 McCabe / NIST SP 500-235

- **Artifact:** A. H. Watson, T. J. McCabe, "Structured Testing: A Testing Methodology Using the
  Cyclomatic Complexity Metric", NIST Special Publication 500-235, Sept 1996. Read in full from
  <https://www.mccabe.com/pdf/mccabe-nist235r.pdf> (the `nvlpubs.nist.gov` mirror returns HTTP 406). **[full]**

**Section 3.1 is literally titled "Independence of complexity and size":**

> "There is a big difference between complexity and size. Consider the difference between the
> cyclomatic complexity measure and the number of lines of code, a common size measure."

> "Thus, although the number of lines of code is an important size measure, it is independent of
> complexity and should not be used for the same purposes."

> "Therefore, the common practice of attempting to limit complexity by controlling only how many
> lines a module will occupy is entirely inadequate. Limiting complexity directly is a better
> alternative."

**And the authors reject the size-normalised variant explicitly** (§2.5, on "modified" complexity):

> "the developer could take a module with complexity 90 and reduce it to 'modified' complexity 10
> simply by adding a ten-branch multiway decision statement to it that did nothing."

> "Although constructing 'modified' complexity measures is not recommended…"

This is the single strongest author-side statement for qingluan's position: **the metric's own
authors say size and complexity are independent, must not be used for the same purposes, and that
size-normalising (or size-blending) the number is both inadvisable and gameable.**

*Gap [unverified]:* McCabe's separate essay "Resolving the Complexity Dilemma" and the mccabe.com
FAQ could not be reached (404; no Wayback capture), so nothing is claimed from them.

### 3.3 radon

- **Artifact:** radon 6.0.1 docs, <https://radon.readthedocs.io/en/stable/intro.html> and
  <https://radon.readthedocs.io/en/stable/commandline.html>. **[first-party doc]**

radon reports **four separate commands**, never a merged number:

> "Radon currently has four commands: cc: compute Cyclomatic Complexity / raw: compute raw metrics /
> mi: compute Maintainability Index / hal: compute Halstead complexity metrics"

and on the one place it *does* combine axes (MI = Halstead volume + CC + SLOC + comments):

> "Maintainability Index is still a very experimental metric, and should not be taken into account
> as seriously as the other metrics."

radon's own further reading explicitly points at A. van Deursen, "Think Twice Before Using the
'Maintainability Index'" — i.e. the tool that ships MI tells you not to trust it. **[first-party doc]**

### 3.4 lizard

- **Artifact:** lizard README.rst, <https://raw.githubusercontent.com/terryyin/lizard/master/README.rst>. **[first-party doc]**

lizard prints length and complexity as **parallel columns**, each with its own threshold:

> "It counts - the nloc (lines of code without comments), - CCN (cyclomatic complexity number), -
> token count of functions. - parameter count of functions."

The output header is literally `NLOC  CCN  token  param  function@line@file`. Defaults are
independent: CCN warning at 15, function length warning at 1000. On nested constructs it says:
> "There is no definitive approach to account for the complexity of these nested constructs. One
> obvious way is to consider the nested complexity separately, as is currently done for Python."

And it is candid that it is a shape metric, not a size metric:
> "This tool actually calculates how complex the code 'looks' rather than how complex the code
> really 'is'."

### 3.5 SonarQube metric definitions (first-party)

- **Artifact:** <https://docs.sonarsource.com/sonarqube-server/user-guide/code-metrics/metrics-definition>. **[first-party doc]**

SonarQube's metric catalogue has **separate top-level sections "Size" and "Complexity"**, with
distinct metric keys and no combined size+complexity metric:

> "Lines of code | `ncloc` | The number of physical lines that contain at least one character which
> is neither a whitespace nor a tabulation nor part of a comment."

> "Cyclomatic complexity | `complexity` | A quantitative metric used to calculate the number of
> paths through the code."

> "Cognitive complexity | `cognitive_complexity` | A qualification of how hard it is to understand
> the code's control flow."

Function-level cyclomatic complexity is defined as `1 + number of conditional branches`. Note
also that "function-level complexity scores cannot be viewed directly in SonarQube, they are only
used to calculate the overall code's cyclomatic complexity" — an aggregation choice qingluan
should *not* copy (see §2/§6).

---

## 4. Composite scores that exist, and their track record

**Summary table — name → formula → how weights were set → validation status.**

| Model | Actual formula / aggregation | How the weights were set | Validation status |
|---|---|---|---|
| **Maintainability Index (original)** — Oman & Hagemeister 1992; Coleman et al. 1994; SEI handbook | `MI = 171 − 5.2·ln(V) − 0.23·G − 16.2·ln(L)` (+ `50·sin(√(2.4·C))` in SEI/radon; `V`=Halstead volume, `G`=CC, `L`=SLOC, `C`=comment %) | **Subjective-regression.** Coefficients from a polynomial regression fitted to **HP engineers' subjective quality ratings** (AFOTEC instrument), i.e. calibrated to opinion, not to defects/effort | Weak. `radon` labels it "still a very experimental metric"; radon's own reading list cites van Deursen's "Think Twice". No replicated outcome calibration found **[abstract/first-party doc]** |
| **MI (Microsoft Visual Studio, normalised)** | `MI = MAX(0, (171 − 5.2·ln V − 0.23·G − 16.2·ln L) · 100 / 171)`; bands 0–9 / 10–19 / 20–100 | **Ad hoc.** "we decided to be conservative with the thresholds… we decided to break down this 0-100 range 80-20 to keep the noise level low" — explicit noise knob, no calibration | Vendor-defined, no published validation cited on the page **[first-party doc]** |
| **SIG maintainability model** — Heitlager, Kuipers & Visser 2007; SIG/TÜViT | 8 metrics → property scores (1–5 ★) → mapped to ISO/IEC 25010 maintainability sub-characteristics → **single star rating** | **Expert-designed + norm-referenced.** Thresholds set so "about 5% of the software applications will be deemed highly maintainable and receive a 5-star rating"; re-calibrated annually to the population | Internal validation + correlation with programmer productivity (Bijlsma 2011) claimed by the model's co-author; **no independent replication found in this pass** **[abstract/first-party doc]** |
| **CodeScene Code Health** — Tornhill & Borg 2022 + CodeScene docs | Per-file score 10 (healthy) → 1 from **23–30 weighted smell deductions**; files aggregated by a **weighted average where the weight is each file's LoC** | **Expert/internal, not empirical.** "CodeScene reports that the cut-off points were decided by their internal team via a baseline library of hand-scored code examples." Docs claim rules are "calibrated against real-world codebases" but no dataset/method is published; **default per-rule weights are unpublished** | Code Red: r = −0.58 vs resolution time (LoC alone r = 0.13); ~15× defects, +124% time. But the vendor's own later ICSME 2024 paper reports Code Health AUC **0.95** vs a **naive LoC baseline 0.95** vs ML 0.97 — length alone nearly matches it. Cut-off differs between the vendor's own papers (8.0 vs 9) **[full/abstract/first-party doc]** |
| **SonarQube Maintainability Rating / technical debt** | `sqale_debt_ratio = technical debt / (cost to develop one line of code × ncloc)`; default cost 30 min/line; rating `A ≤5%`, `B <10%`, `C <20%`, `D <50%`, `E ≥50%` | **Expert estimates, no calibration claimed.** Rules are bucketed qualitatively (Trivial/Easy/Medium/Major/High/Complex) then mapped to a fixed per-language minute table (e.g. other languages 5/10/20/60/180/1 day). "The remediation cost of an issue is… taken over from the effort assigned to the rule." Bands are fixed constants | Mature, widely used operational model; **no published calibration** of per-rule minutes or the % bands. A CodeScene-authored 2024 paper claims SonarQube's default TD-ratio threshold "performs worse than random chance" for liability prediction (vendor-authored, so discount) **[first-party doc/abstract]** |

### 4.1 Maintainability Index — what the primary sources actually say

- **Coleman, Ash, Lowther & Oman 1994**, "Using metrics to evaluate software system
  maintainability", *IEEE Computer* 27(8):44–49. **DOI [10.1109/2.303623](https://doi.org/10.1109/2.303623)**.
  Read via the open postprint <http://www.ecs.csun.edu/~rlingard/comp589/ColemanPaper.pdf>. **[full]**

  The paper describes exactly how the constants were obtained:
  > "the models were again calibrated to HP engineers' subjective evaluation of the software as
  > measured by the abridged version of the AFOTEC software quality assessment instrument."

  > "That is, the independent variables used in our models were a host of 40 complexity metrics,
  > and the dependent variable was the (numeric) result of the [AFOTEC assessment]"

  and the resulting four-metric polynomial:
  > "Maintainability = 171 − 5.2 × ln(aveVol) − 0.23 × ave V(g') − 16.2 × ln(aveLOC)"

  Earlier in the same paper: "HPMAS was calibrated against HP engineers' subjective evaluation of
  16 software systems." **So the MI coefficients are a regression onto human opinion of 16 systems,
  not onto defects or maintenance effort.** This is the single most important fact about MI for
  the "are composite weights empirical?" question: they are *empirically fitted to subjectivity*.
- **radon's variant** (the one most developers meet) adds the SEI comment term and is labelled
  experimental (see §3.3). Formula:
  `MI = max[0, 100·(171 − 5.2·ln V − 0.23·G − 16.2·ln L + 50·sin(√(2.4·C)))/171]`. **[first-party doc]**
- **Microsoft's normalisation and bands** are documented as a deliberate noise-reduction choice,
  not a calibration (quoted in the table above). **[first-party doc]**
- **Criticism:** A. van Deursen, "Think Twice Before Using the 'Maintainability Index'", 2014,
  <https://avandeursen.com/2014/08/29/think-twice-before-using-the-maintainability-index/> —
  cited approvingly by radon. (Read the citation, not the post, in this pass.) **[unverified]**
- **Gap [unverified]:** the Oman & Hagemeister 1992 ICSM paper itself (DOI
  [10.1109/ICSM.1992.242525](https://doi.org/10.1109/ICSM.1992.242525)) was not obtained in full;
  the coefficient provenance above rests on Coleman et al.'s own account. No source was found that
  contradicts it.

### 4.2 SIG maintainability model

- **Citation:** I. Heitlager, T. Kuipers, J. Visser, "A Practical Model for Measuring
  Maintainability", *QUATIC 2007*. **DOI [10.1109/QUATIC.2007.8](https://doi.org/10.1109/QUATIC.2007.8)**
  *(Crossref confirms this DOI maps to this title; Semantic Scholar's mapping for the same DOI is
  wrong — trust Crossref).* **[abstract]** via OpenAlex:
  > "the maintainability index has been proposed to calculate a single number that expresses the
  > maintainability of a system. In this paper, we discuss several problems with the MI, and we
  > identify a number of requirements… We sketch a new maintainability model that alleviates most of
  > these problems."
- **First-party description of the model and its calibration** — J. Visser, "How does your software
  measure up?", inaugural lecture, 2012, Radboud Repository
  <https://repository.ubn.ru.nl/bitstream/handle/2066/104074/104074.pdf>. **[full]**
  > "The sig maintainability model – at least in its current form – uses 8 software metrics as input."

  > "These metrics are calculated from the source code… aggregated into scores on a scale of 1 to 5
  > stars… then mapped to maintainability sub-characteristics as defined by the iso/iec 25010… and
  > finally a single star rating for maintainability is obtained."

  > "The thresholds in the measurement model have been chosen such that about 5% of the software
  > applications will be deemed highly maintainable and receive a 5-star rating. The 5% least
  > maintainable systems will receive a single star only."

  > "Every year, the model is re-calibrated and sometimes refined such that the star ratings remain
  > a faithful representation of distribution of maintainability across the evolving population."

  > "Validation experiments have been conducted on the maintainability model to study its
  > statistical properties, to establish its correlation with desirable economic indicators such as
  > programmer productivity"
- **Weight provenance verdict:** thresholds are **norm-referenced** (percentile-of-population), and
  the sub-characteristic mapping is expert-designed against ISO/IEC 25010. That is a legitimate
  design, but it means the *numbers are not absolute* — a 5★ today is not a 5★ in five years.
  **[full]**

### 4.3 CodeScene Code Health

- **Citation:** A. Tornhill, M. Borg, "Code Red: The Business Impact of Code Quality — A
  Quantitative Study of 39 Proprietary Production Codebases", *TechDebt/ICSE 2022*, pp. 11–20.
  **DOI [10.1145/3524843.3528091](https://doi.org/10.1145/3524843.3528091)**
  *(correction: the brief's `10.1145/3524843.3525374` does not resolve in Crossref; verified here)*;
  preprint [arXiv:2203.04374](https://arxiv.org/abs/2203.04374). **[full]**
  - Result: > "low quality code contains 15 times more defects than high quality code."
    > "resolving issues in low quality code takes on average 124% more time in development."
  - Effect sizes: Pearson **r = −0.58** between Code Health and issue-resolution time, versus
    **r = 0.13** for raw LoC — i.e. the composite out-predicts plain length on this outcome.
  - Method: 30,737 files, 39 codebases, Code Health as the quality proxy, Jira defects and
    time-in-development as outcomes.
  - Limitations, in their words: > "All included codebases come from CodeScene users, which might be
    [a threat]" and the category sample sizes are unbalanced ("there is simply more high quality
    than low quality code in our data set"). They also decline to disclose selection criteria
    ("For confidentiality reasons, we cannot disclose neither the selection criteria nor the
    procedure for acquisition").
- **First-party model docs:** <https://docs.enterprise.codescene.io/versions/4.2.12-alpha2-2-0x5100/guides/technical/biomarkers.html>
  and <https://community.codescene.com/help/articles/9204152-how-is-code-health-calculated>. **[first-party doc]**
  > "Code Health is an aggregated metric based on 25+ factors scanned from the source code."
  > "CodeScene's default weighting is calibrated with data from hundreds of codebases, but you can
  > always override it."
  Score runs "from 10 (healthy code that relatively easy to understand and evolve) down to 1".
- **Most relevant design choice for qingluan:** CodeScene keeps the two axes as **separate factors
  inside** its aggregate —
  > "**Large Method:** Functions with many lines of code are harder to understand."
  > "**Complex Method:** Many conditional statements (e.g., if, for, while) reduce code health
  > (cyclomatic complexity)."
  i.e. even the most commercial composite does not multiply length by complexity; it scores each
  smell separately and then aggregates. And it deliberately avoids the word "quality":
  > "we wanted to avoid terms like 'quality' or 'maintainability' since they are easy to game and,
  > more serious, suggest an absolute truth."
- **Threshold/weight provenance — expert/internal, not empirical.** The Code Red paper itself
  states the cut-offs were set by hand:
  > "CodeScene reports that the cut-off points were decided by their internal team via a baseline
  > library of hand-scored code examples." *(arXiv §2.1)*
  The docs simultaneously claim calibration — "CodeScene's code health rules are calibrated against
  real-world codebases" — but no dataset or method is published, and the **default per-rule numeric
  weights are not public** (only relative overrides such as `{"name":"Brain Method","weight":0.5}`
  are documented). The healthy cut-off is even inconsistent between the vendor's own papers
  (8.0 in Code Red, 9 in the later ICSME 2024 paper) with no published rationale. So the composite
  is **not reproducible from first-party material**. [abstract/first-party doc]
- **The vendor's own later benchmark undercuts the composite's edge.** Borg, Ezzouhri & Tornhill,
  "Ghost Echoes Revealed", ICSME 2024 ([arXiv:2408.10754](https://arxiv.org/abs/2408.10754)), is
  CodeScene-authored (conflict of interest disclosed in the paper) and reports Code Health
  **AUC 0.95 vs a naive LoC baseline 0.95** vs state-of-the-art ML 0.97, concluding
  > "LoC counting is a simple yet effective way to identify files that are hard to maintain."
  For qingluan's question this is striking: a **bare length baseline matched the multi-factor
  composite** at file level. It is also a reminder that at *file* granularity length is a strong
  proxy (see §6.1), which is precisely what function-level analysis avoids. [abstract]

### 4.4 SonarQube Maintainability Rating / technical debt

See the table row above and §3.5. First-party definitions
<https://docs.sonarsource.com/sonarqube-server/user-guide/code-metrics/metrics-definition>. **[first-party doc]**

- The rating is **size-normalised**: > "The maintainability rating reflects the density of technical
  debt relative to the size of the code."
  This is the one mainstream score in which *length appears in the denominator*, because the score
  is an effort *density*, not a complexity magnitude. Worth noting as a third design: length used
  to normalise, never added.
- **SonarQube deliberately ships several independent ratings** — Security, Reliability,
  Maintainability, Security Review — rather than one composite (see §5).

**Composite-table bottom line.** Zero of the four families has a **publicly documented,
independently replicated** regression of maintainability/defects onto its terms. MI's weights are a
fit to expert opinion; SIG's are norm-referenced population percentiles; CodeScene's are
proprietary; SonarQube's remediation minutes are per-rule expert estimates with fixed bands.
Adding length to qingluan's number would be inventing weights no better justified than these.

---

## 5. Multi-axis presentation without a composite

**SonarQube** — the clearest first-party example of "present several axes, never sum them".
The metric catalogue separates **Size** and **Complexity** (§3.5), and the product exposes
**four independent ratings** with independent grids rather than a single quality score:
`security_rating`, `reliability_rating`, `sqale_rating` (maintainability),
`security_review_rating`. Ratings are driven by **issue counts/severity or debt density**, and the
UI shows **issues + remediation effort in minutes/days**, not a computed quality scalar. **[first-party doc]**
> "The rating related to the value of the technical debt ratio." — `sqale_rating`
> "The total number of issues impacting maintainability." — `software_quality_maintainability_issues`

**CodeScene** — presents a **factor list** (biomarkers) per file plus a "Code Health" score and a
"virtual code reviewer" that "aggregate[s] the most significant metrics for your chosen file" so a
human can see *which* smells fired, not just a number. The docs frame trends, not absolute truth:
> "we find that it's the trend that's most important: is the code evolving in the desired
> direction?" **[first-party doc]**

**ISO/IEC 25010** — the standards layer keeps maintainability as a set of distinct
sub-characteristics (adaptability, changeability, stability, testability, compliance) that
models map onto rather than collapse. This is visible first-party in Heitlager's abstract and in
Visser's description of the SIG mapping (§4.2). **[abstract/full]**

**clang-tidy** — **[first-party doc]** (re-verified via a research subagent against the LLVM docs).
It is a framework of independent per-check diagnostics: "Each check has a name and the checks to
run can be chosen using the `-checks=` option"; escalation is per check
(`--warnings-as-errors=<string>`), and **there is no score anywhere in the tool or docs**.
`readability-function-cognitive-complexity` is one check among ~600, with its own `Threshold`
(default 25) and per-statement sub-diagnostics. The absence of an aggregate is an observation from
the docs, not a quoted prohibition.

**Peer-reviewed academic arguments for separate axes:**

- **Kitchenham, Pfleeger & Fenton, "Towards a framework for software measurement validation",
  *IEEE TSE* 23(3), 1997** — **DOI [10.1109/32.489070](https://doi.org/10.1109/32.489070)**
  *(correction: the brief's `10.1109/32.473206` does not resolve)*. The 1995 companion abstract
  recommends "the use of measurement vectors rather than artificially contrived scalars"; the 1997
  paper adds: "unless we have some concept of system volume that allows us to combine the
  dimensions in a single measure. As yet we are not aware of any such concept." *(quotes reported
  by the research subagent after full-text extraction; not independently re-extracted here.)*
- **Zhang, Hassan, McIntosh & Zou, "The Use of Summation to Aggregate Software Metrics Hinders the
  Performance of Defect Prediction Models", *IEEE TSE* 43(5), 2017** —
  **DOI [10.1109/TSE.2016.2599161](https://doi.org/10.1109/TSE.2016.2599161)** *(verified in
  Crossref)*: "aggregation schemes can significantly alter correlations among metrics, as well as
  the correlations between metrics and the defect count." This is the direct empirical warning
  against the kind of summing qingluan already refuses to do.
- **ISO/IEC 25010** decomposes maintainability into separately-defined sub-characteristics
  (modularity, reusability, analysability, modifiability, testability) and specifies **no
  aggregation rule**; **ISO/IEC 5055** likewise *reports* four separate structural measures. Neither
  forbids a composite — they simply standardise separate axes. **[abstract]** (ISO text read via the
  iso25000 portal; the paid ISO text was not read.)

**The general argument for separate axes**, stated by the sources above:
1. NIST SP 500-235: size and complexity are independent and "should not be used for the same
   purposes"; blending is gameable. **[full]**
2. Sonar white paper: above method level a high CC number is ambiguous between "large, easily
   maintained domain class" and "small class with a complex control flow" — the reader needs both
   axes to disambiguate. **[full]**
3. Landman et al. 2016: CC and SLOC *diverge* in exactly the large-function tail that matters
   most, so collapsing them destroys the signal you most want. **[full]**
4. Kitchenham et al. 1997 and Zhang et al. 2017: there is no established way to combine the
   dimensions into one volume measure, and summation measurably degrades defect prediction.
   **[abstract, as reported]**

---

## 6. Correlation between length and complexity in practice

This is the empirical crux, and there *is* a trustworthy large-scale number.

- **Citation:** D. Landman, A. Serebrenik, E. Bouwers, J. J. Vinju, "Empirical analysis of the
  relationship between CC and SLOC in a large corpus of Java methods and C functions",
  *Journal of Software: Evolution and Process* 28(7):589–618, 2016.
  **DOI [10.1002/smr.1760](https://doi.org/10.1002/smr.1760)**. Full text read from the CWI open
  copy <https://ir.cwi.nl/pub/23938/JOEP8339.pdf>; data/scripts at
  <https://zenodo.org/records/293795>. **[full]**

**Corpus:** 17,633,256 Java methods and 6,259,031 C functions (open source).

**Headline numbers (this study's own measurement, method/function level):**

| Statistic | Java | C |
|---|---|---|
| R² (SLOC vs CC), all methods | **0.40** | **0.44** |
| Pearson r (= √R²) | ≈ 0.63 | ≈ 0.66 |
| Spearman ρ (rank) | **0.80** | **0.83** |
| R² after log transform | **0.68** | **0.71** |
| Power-law fit | `CC = 10^0.28 · SLOC^0.65` | `CC = 10^0.41 · SLOC^0.79` |
| R² after **file-level** aggregation | **0.64** (0.87 log) | 0.39 (0.84 log) |
| R² after summing methods/functions | **0.73** (0.90 log) | 0.70 (0.90 log) |

*(Spearman values and the two aggregation rows are from the paper's Tables IV–V, read by a research
subagent against the same CWI PDF; the headline R²/power-law figures were independently confirmed
here. Directionally: rank-correlation is high in the bulk, linear R² is moderate, and aggregation
manufactures the high linear correlation.)*

**Their conclusion, verbatim:**
> "linear correlation between SLOC and CC is only moderate as a result of increasingly high variance"
> "not strong enough to conclude that CC is redundant with SLOC"

**The tails matter — this is the key result for the "long but flat" question:**

- Correlation *falls* as methods get longer (top rows of their Table IV/V): R² drops from 0.40 → 0.30
  (top 10% by SLOC) → 0.21 (top 1%) → 0.08 (top 0.01%) for Java; the same downward pattern holds
  for C (0.44 → 0.36 → 0.28 → 0.12).
  > "The variance of CC over SLOC increases with higher SLOC."
- Spearman (rank) is high in the bulk but collapses in the tail:
  > "showing reasonably high ρ values, but decreasing rapidly when we move out of the lower ranges
  > that the distribution skews towards."
  > "for the bulk of the data, it is indeed true that a new conditional leads to a new line of code"
  > "the decline of the Spearman correlation for higher SLOC… reflects the fact that many different
  > combinations of SLOC and CC are being exercised in the larger methods of the corpus."

  **Translation:** for small/typical functions, length and CC are near-collinear (so adding length
  buys little there); for **large** functions they come apart — long-but-flat and short-but-branchy
  both exist, and that is exactly the population a complexity report is trying to triage. This is
  the empirical basis for keeping both axes.
- **Aggregation is what creates the illusion of redundancy:**
  > "CC summed over larger code units measures an aspect of system size rather than internal
  > complexity of subroutines. This largely explains the often reported strong correlation between
  > CC and SLOC in literature."
  qingluan reports per-function and does not sum CC into a file score — so it sits on the favourable
  side of this result.
- **Independent supporting detail — length can miss branchiness:**
  > "the CC of 8% Java methods and 23% C functions that do use Boolean operators are influenced…
  > These subroutines would be missed when counting only SLOC or when ignoring the operators for CC."
  A function can be short and still branchy (long lines with many `&&`/`||`), so length is not a
  proxy for `cc` in either direction.
- **Stated limitations, in their words:** open-source corpora only, so "our results should not be
  immediately generalized to proprietary software"; heteroscedasticity ("All the linear models
  suffered from heteroscedasticity") complicates the linear reading; and they explicitly checked
  that corpus size is not driving the result (1000 random half-corpora reproduced the R²).

### 6.1 The counter-claim (file level, low-reputation venue)

The strongest *opposite* claim — that CC has no independent explanatory power — exists, and it is
worth naming so the verdict is not built on a single paper.

- **Citation:** G. Jay, J. E. Hale, R. K. Smith, D. P. Hale, N. A. Kraft, C. Ward, "Cyclomatic
  Complexity and Lines of Code: Empirical Evidence of a Stable Linear Relationship",
  *Journal of Software Engineering and Applications* 2(3):137–143, 2009.
  **DOI [10.4236/jsea.2009.23020](https://doi.org/10.4236/jsea.2009.23020)** — **SCIRP is a
  low-reputation publisher**; treat this as a weak source. **[abstract]** via Crossref/OpenAlex:
  > "CC can be said to have absolutely no explanatory power of its own… LOC and CC have a stable
  > practically perfect linear relationship that holds across programmers, languages, code
  > paradigms… and software processes."
- **Why it does not overturn §6:** the study is at **file** level with log–log regression on ~1.2M
  SourceForge files. The Landman numbers above show precisely why that inflates the correlation —
  file-level aggregation raises Java R² from 0.40 to 0.64 and sum-of-methods to 0.73 (log: 0.87 /
  0.90). qingluan reports **per function**, so the pre-aggregation regime is the relevant one, and
  there the answer is "moderate, not redundant".
- **Honest framing:** at file/module level the two *are* near-collinear; at function level they are
  not. This is itself an argument for keeping both axes at function level rather than collapsing.

### 6.2 Does length predict *human* difficulty better than complexity? (ICER 2026)

A new, directly on-point controlled study — surfaced by the correlation subagent and confirmed
here by DOI/title/abstract — asks which measure actually tracks measured cognitive load.

- **Citation:** B. Thorgeirsson, J. Vahrenhold, "How (and How Not) Do Code Complexity Measures
  Predict Cognitive Load?", *ICER 2026* (ACM Conference on International Computing Education
  Research), Vol. 1. **DOI [10.1145/3765964.3811665](https://doi.org/10.1145/3765964.3811665)**.
  **[abstract]** verified here; the specific statistics below are **as reported by the research
  subagent** and were not independently re-derived. Pre-registered, N = 551 students, 24 snippets.
- **Reported result:** zero-order correlation with cognitive load was higher for **SLOC**
  (r ≈ 0.41 / 0.39 on Paas / NASA-TLX) than for **cyclomatic complexity** (r ≈ 0.26 / 0.27). In the
  multivariable mixed model SLOC remained the dominant predictor (β ≈ 0.32 / 0.33) while CC
  **inverted to a small negative coefficient** (β ≈ −0.16 / −0.11) — a suppression effect once the
  shared variance is partialled out. Dominance analysis reportedly found SLOC "completely dominated
  the other measures at every subset size".
- **Why it matters:** it is evidence that **length carries unique, non-redundant signal about
  comprehension**, and that folding length into a complexity number (or dropping it) would discard
  the stronger of the two predictors. It also shows the measures are collinear enough to produce
  suppression, which is a further reason not to sum them.
- **Caveat:** the study is about *student* cognitive load on small snippets, not defects or
  maintenance effort; and the coefficient values are second-hand here. Treat the direction as
  supporting, not decisive.

### 6.3 The strongest counter-evidence: at file/class level, size subsumes the structural signals

Two peer-reviewed studies push against over-valuing *complexity* — and they must be stated, because
they cut against a naive "complexity first" reading even though they support keeping length as a
first-class axis.

- **Sjøberg, Yamashita, Anda, Mockus & Dybå, "Quantifying the Effect of Code Smells on Maintenance
  Effort", *IEEE TSE* 39(3), 2013 — DOI [10.1109/TSE.2012.89](https://doi.org/10.1109/TSE.2012.89).**
  Level: file/class; outcome: IDE-measured maintenance effort; 4 Java systems, 298 modified files.
  > "None of the 12 investigated smells was significantly associated with increased effort after we
  > adjusted for file size and the number of changes"
  > "This result indicates that a single predictor of file size achieves a better fit than all of
  > the smell predictors."
  In their model, file size is significant (β = .58) while **God Method is not** (β = −.32, p = .18).
  *Interpretation for qingluan:* this does **not** argue for folding length into a complexity score.
  It argues the opposite — that **length is the primary axis and structural wrappers add nothing on
  top of it at file level**. It is a reason to keep `nloc` prominent and separate, and a reason not
  to treat "long method" as automatic proof of control-flow complexity. **[abstract, as reported]**
- **Tahir, Bennin, MacDonell & Marsland, "Revisiting the size effect in software fault prediction
  models", ESEM 2018 — DOI [10.1145/3239235.3239243](https://doi.org/10.1145/3239235.3239243)**
  *(title confirmed in Crossref)*. At class level, size "fully mediates" the fault relationship for
  RFC, CBO, LCOM, Fan-in/Fan-out — **but WMC is the exception**, retaining a direct effect. So the
  size-confounder story is metric-specific: coupling/cohesion metrics are size in disguise;
  complexity-family metrics are not entirely. **[abstract, as reported]**

**The other half of the picture — why "long but flat" is the majority case.** Landman et al.'s
Table III reports that **65% of Java methods (11.6M) and 33% of C functions have CC = 1** — no
branches at all — while SLOC in that group spans four orders of magnitude (Java median 3,
max 33,850). A score that adds or multiplies in length re-ranks that 65% using a signal that
branching metrics literally cannot contain. Function length is also **heavy-tailed**: Hatton &
Warr (*Entropy* 27(6):561, 2025, DOI [10.3390/e27060561](https://doi.org/10.3390/e27060561))
fit a power law to C function lengths with β = −1.52 (adjusted R² = 0.99), and Herraiz et al.
(2011) find file sizes follow a double Pareto that **underestimates** the tail. Consequence: any
length term — reported or folded — needs explicit percentile/outlier handling, or a handful of
giant/generated functions dominate. **[full/abstract]**

**Gap [unverified]:** I did not complete a survey of *other* length–complexity correlation studies
(e.g. later replications, non-Java/C languages, or module-level work). The brief asked for exactly
this number, and `10.1002/smr.1760` is the best-documented trustworthy source found; the
literature table in that paper (Table I, ~30 prior studies, R² 0.08–0.96) is itself the best
available meta-survey.

---

## 7. Verdict for qingluan

**Recommendation: keep length as a separate axis. Do not fold it into a complexity score. Adopt a
middle path built from a derived density diagnostic plus union-of-threshold breaches.**

### 7.1 Why not fold

1. **The metric's own authors forbid it.** NIST SP 500-235 §3.1: LOC "is independent of complexity
   and should not be used for the same purposes", and the size-normalised/modified variant is
   explicitly rejected as gameable (90 → 10 by adding a dead switch). **[full]**
2. **Function-level collinearity is only moderate and collapses in the tail** (R² 0.40/0.44;
   0.08 in the largest Java methods). Folding length in would systematically re-rank the large
   functions by a factor that is *least* correlated with branching exactly where users care most.
   **[full]**
3. **No composite has defensible public weights.** MI = regression onto the subjective ratings of
   16 systems; SIG = population percentiles re-calibrated yearly; CodeScene = hand-scored internal
   baseline with **unpublished default weights** (and its own later benchmark shows a naive LoC
   baseline matching it); SonarQube = per-rule expert minutes from a fixed table. Inventing a
   qingluan weight would be strictly less defensible than any of these. **[full/abstract/first-party doc]**
   Independently, Zhang et al. (TSE 2017) show that **summation aggregation measurably degrades
   defect prediction**, and Kitchenham et al. (TSE 1995/97) state there is no accepted concept that
   "allows us to combine the dimensions in a single measure." **[abstract, as reported]**
4. **Folding contradicts qingluan's stated identity** (`spec.md`: "输出指标向量，**不发明复合分数**")
   and the earlier rejection of Halstead/MI on validity grounds. The evidence gathered here
   *supports* that earlier decision rather than overturning it.

### 7.2 Why not ignore length either

- Absolute size is linked to defect growth (Hatton's surviving direction, even after his U-bend
  retraction; Basili's inverse fault-density curve; CodeScene's Large Method smell), and **large
  functions are a distinct failure mode** that branch-counting alone cannot see (Landman's tail
  variance; McCabe's own §3.1 example of "complexity 1 and 282 lines of code").
- More strongly: **65% of Java methods have CC = 1** (Landman Table III), so for the majority of
  the population length is the *only* varying axis. And at file/class level, Sjøberg et al. (TSE
  2013) found file size alone out-predicts all 12 code smells combined (§6.3) — length is not the
  secondary axis.
- So `nloc` should be **reported, and thresholded**, but not summed; give it percentile/outlier
  handling because function length is power-law distributed (Hatton & Warr 2025, §6.3).

### 7.3 The recommended middle path (all presentation-only, no new score)

1. **Keep `nloc` as its own column and its own threshold.** Defaults worth borrowing: lizard's
   function-length warning at 1000 (the outlier end); SonarQube's "Large Method" smell is a
   first-party precedent for treating length itself as the finding. Consider a *lower*,
   shape-informed default (see next item) rather than 1000, since 1000 is an outlier net.
2. **Add one derived diagnostic: decision density `cc / nloc`** ("decisions per line"), clearly
   labelled as *derived*, with no threshold of its own presented as a defect gate. This is the
   third way: it uses both axes to distinguish the two failure modes —
   - low density + high `nloc` → **long but flat** (the 282-line, CC=1 case);
   - high density + low `nloc` → **short but branchy** (the long-Boolean-operator case Landman
     identifies).
   This matches radon's own *separate* `cc` and `raw` outputs and lizard's parallel columns. There
   is a direct published precedent: **Gill & Kemerer, "Cyclomatic complexity density and software
   maintenance productivity", *IEEE TSE* 17(12), 1991,
   DOI [10.1109/32.106988](https://doi.org/10.1109/32.106988)** define **cyclomatic density =
   CC / NCSLOC** — *"The intent is to factor out the size component of complexity"* — and report it
   as a significant single-value predictor of maintenance productivity. NIST SP 500-235 Appendix A.11
   records the same "cyclomatic density" idea. **Keep it a labelled diagnostic, not a score:** a
   density discards the length information that §6.2/§6.3 show is independently valuable, which is
   why it accompanies `nloc` rather than replacing it.
   Because function length is heavy-tailed (§6.3), surface density alongside raw `nloc`
   and never as a bare ranking key without outlier visibility.
3. **Any "is this function bad?" signal = union of per-axis breaches**, e.g. `cc > 10` OR
   `cognitive > 15` OR `nloc > L` OR `params > P` OR `maxNesting > D`. The output is the *set* of
   breached axes plus their values, never a sum. This is how SonarQube (four independent ratings +
   per-rule issues) and clang-tidy (one diagnostic per check) already behave. **[first-party doc]**
4. **If a single ordering is ever required** (e.g. top-K), use per-axis ranks combined by
   max/union-of-ranks or Pareto dominance, and keep `--sort cognitive` / `--sort cc` / `--sort nloc`
   as the honest, explicit orderings already in the spec. Do not normalise-and-add: normalisation
   is where arbitrary weights re-enter.
5. **Do not sum per-function `cc` into a file/class "complexity".** Landman et al. show that
   aggregate is a size measure. If a file-level number is wanted, present it as *size*
   (nloc/SLOC) and keep complexity as the distribution of its functions.

### 7.4 What would change the verdict

Fold length in **only if** a calibration study appears that (a) uses function-level length and
function-level CC/cognitive as separate predictors, (b) against an outcome qingluan cares about
(defect proneness or change effort, not expert opinion), and (c) reports stable, replicated
weights. Nothing found in this pass meets that bar. The size-confounder literature is a *class-level*
caution about other metrics, not such a calibration.

---

## Appendix — sources actually read, and what was not

**Read in full (primary):** Basili & Perricone TR-1195 (NTRS/Internet Archive); Hatton 1997 PDF and
his 2009 retraction page; Nagappan & Ball ICSE 2005 PDF; El Emam 2001 abstract (two independent
mirrors); Landman et al. 2016 CWI PDF; SonarSource Cognitive Complexity white paper v1.7 (Internet
Archive capture of the official PDF, cross-checked by a second reader); NIST SP 500-235 (mccabe.com
copy, 124 pp); Coleman et al. 1994 postprint; Visser 2012 inaugural lecture; Tornhill & Borg 2022
Code Red (arXiv 2203.04374); radon intro/commandline; lizard README.rst; SonarQube metric
definitions; CodeScene enterprise biomarkers docs + CodeScene community "How is Code Health
Calculated?"; Microsoft Learn MI page.

**Abstract only / as-reported:** El Emam et al. 2001 (full text closed on **every** route tried —
see below); Heitlager, Kuipers & Visser 2007 (QUATIC); Jiarpakdee et al. TOSEM 2014
(`10.1145/2556777`); Briand et al. 2003 comment (`10.1109/TSE.2003.1214331`); Hatton 1997 formal
abstract (OpenAlex) — full PDF also read; Jay et al. 2009 (`10.4236/jsea.2009.23020`,
low-reputation SCIRP venue); Thorgeirsson & Vahrenhold ICER 2026 (`10.1145/3765964.3811665`);
Sjøberg et al. 2013 (`10.1109/TSE.2012.89`); Tahir et al. 2018 (`10.1145/3239235.3239243`);
Yamashita & Moonen 2012 (`10.1109/ICSM.2012.6405287`); Tufano et al. 2015 (`10.1109/ICSE.2015.59`);
Gill & Kemerer 1991 (`10.1109/32.106988`, green-OA MIT working-paper version read);
Hatton & Warr 2025 (`10.3390/e27060561`).

**Verification catches worth recording:**
- The brief's Basili & Perricone DOI `10.1145/69605.80124` **does not resolve**; the correct DOI is
  **`10.1145/69605.2085`** (Crossref).
- For `10.1109/QUATIC.2007.8`, Semantic Scholar returns the wrong paper ("A Probabilistic Approach
  to Web Portal's Data Quality Evaluation"); **Crossref** confirms the DOI does map to Heitlager et
  al., "A Practical Model for Measuring Maintainability". Use Crossref for this DOI.
- The brief's Code Red DOI `10.1145/3524843.3525374` **does not resolve**; the correct DOI is
  **`10.1145/3524843.3528091`** (Crossref; pp. 11–20).
- The brief's Kitchenham et al. DOI `10.1109/32.473206` **does not resolve**; the correct one is
  **`10.1109/32.489070`** (Crossref).
- A research subagent supplied `10.1109/32.588521` for Gill & Kemerer 1991; that DOI actually
  resolves to the SPIN model-checker paper. The correct DOI is **`10.1109/32.106988`** (Crossref).
  All new DOIs added in §6.3 were Crossref-verified individually; the specific statistics there are
  as-reported by the subagent.
- `nvlpubs.nist.gov`'s copy of SP 500-235 returns HTTP 406; mccabe.com's copy works.
- `web_fetch` cannot parse PDFs; `pdftotext` (Nix poppler-utils) was used. `/tmp` is not persistent
  across shell invocations in this environment, so downloads must be re-fetched per call.

**Not obtained / explicitly unverified:**
- El Emam et al. 2001 **full text** and the **full text** of the 2003 comment. Their conclusions
  here rest on the published abstracts (two independent mirrors for El Emam). A dedicated search
  confirmed the paper is closed on every route tried — Unpaywall `is_oa:false`, OpenAlex
  `oa_status:"closed"` / `any_repository_has_fulltext:false`, Semantic Scholar `CLOSED`, KSU course
  mirror HTTP 403, Wayback CDX empty, NRC record NRCC 44110 metadata-only. **No El Emam correlation
  value (r/ρ/R²) is reported here, because none was seen in the paper itself.**
- Other sources located but not text-verified: Shepperd 1988, "A critique of cyclomatic complexity
  as a software metric" (`10.1049/sej.1988.0003`); Fenton & Neil 1999 (`10.1109/32.815326`);
  Fenton & Bieman, *Software Metrics* 3rd ed. (2014) — no accessible text, so no quote is offered;
  Mamun, Berger & Hansson 2019 (`10.1007/s10664-019-09714-9`).
- Oman & Hagemeister 1992 ICSM full text (coefficient provenance taken from Coleman et al.'s own
  account).
- McCabe's "Resolving the Complexity Dilemma" and mccabe.com FAQ (404, no Wayback capture).
- A completed survey of later replications/refutations beyond TOSEM 2014, and of other
  length–complexity correlation studies beyond `10.1002/smr.1760`. Two further studies were located
  and are cited in §6.1–6.2 (Jay et al. 2009; Thorgeirsson & Vahrenhold ICER 2026), but the survey
  is not exhaustive and the ICER 2026 statistics are second-hand.
- clang-tidy's presentation model was **re-verified first-party via a research subagent** (§5);
  the "no score" statement is an observation from the LLVM docs, not a quoted prohibition.
- CodeScene's actual default per-rule weights and the numeric alert cut-off are **not public**, so
  the composite cannot be reproduced; the threshold also differs between the vendor's own papers.
- Kitchenham et al. 1997 and Zhang et al. 2017 full-text quotes were extracted by a research
  subagent; only their DOIs/titles were independently verified here (Crossref). The Fenton & Bieman
  book quote was **not** obtained.
- A fully exhaustive survey of every composite model and every replication was not attempted;
  coverage is the set of models named in the brief plus the primary critiques found along the way.
