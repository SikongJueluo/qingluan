# Coupling, fan-in and change blast radius: the metric lineage

Primary-source research for a proposed new axis in `qingluan complexity`: *"a file that many other
files import has a large change blast radius, and that is itself a kind of complexity."*

**Research date:** 2026-09-30.
**Scope:** what the established metric lineage for "how many things depend on this module" actually
is, what each metric counts, at what granularity, what the original authors claimed it predicts, and
how real tools compute and present it today. Complements `docs/research/code-complexity-metrics.md`
(§3.3 fan-in/fan-out) and `docs/research/code-length-metrics.md` (axis-separation conventions).

**Method.** Six parallel research streams, each restricted to primary sources — original papers,
the authors' own articles/books, official tool documentation, official tool source code. Blog posts
and Wikipedia were used only to *locate* primary sources. Every claim below carries the URL it was
read from. Load-bearing quotes (SDP on high fan-in, the Henry–Kafura formula, the ADP statement,
`Ca`/`Ce`/`I`, CBO/RFC) were re-verified by the lead author directly against the cited file. Only
short passages are quoted; no upstream file, paper or PDF is copied into the repo. Downloaded PDFs
were deleted (see Appendix A).

**Two corrections to the brief, established during the research:**

1. **Chidamber & Kemerer did not derive CBO from Henry & Kafura.** Their stated theoretical base is
   Bunge's ontology (via Wand), plus Pressman's classic "degree of interdependence between modules";
   the CK94 bibliography contains no Henry & Kafura and no "information flow" (§5.4). The
   information-flow lineage of CBO is a later retrospective attribution by survey authors.
2. **"Size fully mediates Fan-in/Fan-out" is an over-generalisation of Tahir et al. 2018.** That
   paper's own abstract says the mediation "is not consistent in all examined systems" and that the
   authors "are unable to confirm if class size has a significant mediation or moderation effect"
   (§5.5). `docs/research/code-length-metrics.md` §3.4 currently states the strong version; it should
   be softened.

---

## TL;DR

1. **The lineage is 50 years old and has three independent roots.** (a) *Information-flow complexity*
   — Henry & Kafura 1981, procedure-level `length × (fan-in × fan-out)²`, validated against change
   counts in UNIX v6. (b) *Change impact / ripple effect* — Haney 1972's change-propagation probability
   matrix, Yau & Collofello's ripple-effect and design-stability measures: "given that module M changes,
   how much else must change?". (c) *Package coupling* — Martin 1994's `Ca`/`Ce`/`I`/`A`/`D` and the
   SDP/SAP/ADP principles, canonicalised in the 2002 book. CK 1994's CBO/RFC is a fourth, separate line
   that *merges* fan-in and fan-out into one per-class number.
2. **No primary source says "high fan-in is bad".** High fan-in is the *definition of stability* in
   Martin's framework, and he calls such units "Responsible". It becomes a problem in exactly two
   stated situations: high fan-in **plus low abstractness** (the "Zone of Pain"), and fan-in landing
   on a package that was **intended to be volatile** — and in the second case the fault is the
   *depender*, not the depended-upon (§4.3). Henry & Kafura also read high fan-in as "stress point /
   inadequate refinement", not as a defect class of its own.
3. **What every tool treats as a defect is a *cycle*, not a high fan-in.** ADP is about cycles;
   dependency-cruiser ships `no-circular` at `error` severity; madge exits 1 on cycles; jdepend calls
   cycles a "deadly embrace"; ESLint calls them "always a dangerous anti-pattern"; ArchUnit's
   headline slice rule is `beFreeOfCycles()`. Nobody ships a fan-in threshold.
4. **Fan-in/coupling is presented as a neutral number almost everywhere.** jdepend prints `Ca`/`Ce`/`A`/`I`/`D`
   with no thresholds; dependency-cruiser computes `afferentCouplings`/`efferentCouplings`/`instability`
   but `--metrics` is **off by default**. Where a threshold does exist it is a *rule*, not a metric:
   SonarQube `S1200` caps class coupling at **20** and `S6539` caps a class's imports at **20**;
   NDepend publishes `TypeCe > 50` and `NormDistFromMainSeq > 0.7` as guidance; ESLint
   `import/max-dependencies` caps fan-out at **10**. Note that every one of these caps is fan-**out** or
   symmetric class coupling — none is a "too many people depend on you" threshold.
5. **What is computable from import statements alone (no call graph):** per-file fan-in/fan-out and
   `I = Ce/(Ca+Ce)`, file-level propagation cost, and cycles (strongly connected components). **Not**
   computable: Henry & Kafura fan-in/fan-out (needs data flow through parameters and globals), CK's
   CBO (needs method/instance-variable use), RFC (needs one call level), and Martin's *class*-count
   `Ca`/`Ce` (imports give you *module* counts; the counted unit is classes) (§8).
6. **The repo's own local measurement agrees with the sources** and is worth reading alongside this
   file: `.scratch/complexity/research/coupling/local-measurements.md` finds fan-in **anti-correlated**
   with churn (Spearman −0.23 to −0.31) and uncorrelated with complexity, i.e. "raw fan-in ranks the
   *least* risky code at the top. That is exactly what the Stable Dependencies Principle wants."

---

## 1. The lineage at a glance

| Metric | Definition (as the source defines it) | Granularity | Primary source |
|---|---|---|---|
| **Information-flow complexity** (Henry & Kafura 1981) | `length × (fan-in × fan-out)²`; fan-in = local flows in + data structures read; fan-out = local flows out + data structures updated; length = source lines | procedure (then summed to module) | [IEEE TSE 7(5):510–518, DOI 10.1109/TSE.1981.231113](https://doi.org/10.1109/TSE.1981.231113) |
| **Change-propagation model** (Haney 1972) | `T = A(I − P)⁻¹`, where `Pij` = "Probability that a change in module i necessitates a change in module j"; `(I − P)⁻¹` is the "ripple factor". *Not* a ratio measure | module pair → system | Haney, *Module Connection Analysis*, AFIPS FJCC 1972, DOI [10.1145/1479992.1480016](https://doi.org/10.1145/1479992.1480016) (see §3.1) |
| **Ripple effect / stability** (Yau & Collofello) | `LRE_k = Σ_i P(ki)·LCM_ki`, a probability- and complexity-weighted count of the affected-module set `RIPPLEM`; `LS_k = 1/LRE_k`; program `LSP = 1/LREP`; design `DS_x = 1/DLRE_x` | module → program | Yau & Collofello, IEEE TSE SE-6(6):545–552, 1980, DOI [10.1109/TSE.1980.234503](https://doi.org/10.1109/TSE.1980.234503) (see §3.2) |
| **Afferent coupling `Ca`** (Martin) | "The number of classes outside this package that depend upon classes within this package." | package, counting **classes** | [Martin, *OO Design Quality Metrics*, 1994](http://www.objectmentor.com/resources/articles/oodmetrc.pdf) |
| **Efferent coupling `Ce`** | "The number of classes inside this package that depend upon classes outside this package." | package, counting classes | same |
| **Instability `I`** | `Ce / (Ca + Ce)`, range `[0,1]`; `I=0` maximally stable, `I=1` maximally instable | package | same |
| **Abstractness `A`** | `# abstract classes / total # classes`, range `[0,1]` | package | same |
| **Distance `D`** | `\|A + I − 1\| / √2`, range `[0, ≈0.707]`; normalised `D' = \|A+I−1\|` | package | [Martin, *Stability*, C++ Report](http://www.objectmentor.com/resources/articles/stability.pdf) |
| **CBO** (Chidamber & Kemerer 1994) | "CBO for a class is a count of the number of other classes to which it is coupled." (coupled = methods of one use methods or instance variables of the other, either direction; inheritance included) | class; **fan-in and fan-out merged** | [CK, *A Metrics Suite for OOD* (Dec 1993 preprint of IEEE TSE 20(6))](https://www.eso.org/~tcsmgr/oowg-forum/TechMeetings/Articles/OOMetrics.pdf) |
| **RFC** (CK 1994) | `RFC = \|RS\|`, response set = methods of the class ∪ methods they call (one level) | class | same |
| **Propagation cost** (MacCormack et al. / Lattix) | average **Fan-In/Fan-Out Visibility**: "the proportion of elements that could be affected, on average, when a change is made to one element in the system", computed from the transitive closure of the dependency matrix | source file → system | MacCormack, Rusnak & Baldwin, *Management Science* 52(7):1015–1030, 2006, DOI [10.1287/mnsc.1060.0552](https://doi.org/10.1287/mnsc.1060.0552) |
| **Dependency cycle** (Martin, ADP) | "THE DEPENDENCY STRUCTURE BETWEEN PACKAGES MUST BE A DIRECTED ACYCLIC GRAPH (DAG). THAT IS, THERE MUST BE NO CYCLES IN THE DEPENDENCY STRUCTURE." | package graph | [Martin, *Granularity*, C++ Report](http://www.objectmentor.com/resources/articles/granularity.pdf) |

---

## 2. Henry & Kafura 1981 — information-flow complexity

**Paper:** S. M. Henry and D. G. Kafura, "Software Structure Metrics Based on Information Flow",
*IEEE Transactions on Software Engineering*, SE-7(5):510–518, Sept. 1981.
DOI [10.1109/TSE.1981.231113](https://doi.org/10.1109/TSE.1981.231113).
A full-text mirror of the paper was read at
<https://masters.donntu.ru/2020/fknt/mazalov/library/article04/index.htm>; page range and abstract
were cross-checked against the mirror's masthead. The mirror reproduces the paper's own text;
all quotes below are from it.

### 2.1 What fan-in / fan-out mean there

They are **not import counts, and not call counts.** They are *information flows*, derived from data
flow analysis. The paper defines four flow kinds (Definitions 1–4), including flows that exist with
no call at all: a global data structure read/write creates a flow, and a return value that the caller
actually *uses* creates a flow. Then:

> **Definition 5:** The fan-in of procedure A is the number of local flows into procedure A plus the
> number of data structures from which procedure A retrieves information.

> **Definition 6:** The fan-out of procedure A is the number of local flows from procedure A plus the
> number of data structures which procedure A updates.

And the formula, verbatim:

> The formula defining the complexity value of a procedure is `length * (fan-in * fan-out) ** 2`.

> The term `fan-in * fan-out` represents the total possible number of combinations of an input source
> to an output destination. The weighting of the fan-in and fan-out component is based on the belief
> that the complexity is more than linear in terms of the connections which a procedure has to its
> environment. The power of two used in this weighting is the same as Brook's law of programmer
> interaction and Belady's formula for system partitioning.

`length` is deliberately crude:

> The length of a procedure was defined as the number of lines of text in the source code for the
> procedure. This measure includes imbedded comments but does not include comments preceding the
> procedure statement.

**Granularity.** Procedure is the unit; the *module* is defined operationally as the set of
procedures that read from or write to a given data structure (Definition 7, following Parnas), and
"the complexity of a module is defined to be the sum of the complexities of the procedures within the
module."

### 2.2 What they claimed it predicts

**Change-proneness** — and, carefully, *not* "maintainability". The exact claims are:

> In this paper we will validate the complexity metric by demonstrating that it is highly correlated
> with the occurrence of system changes.

> Because of this relationship between program changes and errors we will use these two concepts
> interchangeably in this section.

The only maintenance sentence in the paper is hedged ("possible areas where redesign or reimplementation
is needed, and where maintenance of the system might be difficult"). The "predicts maintainability"
framing found in later literature belongs to a *different, later* paper — Li & Henry 1993,
"Object-oriented metrics that predict maintainability", JSS 23(2):111–122, DOI
[10.1016/0164-1212(93)90077-B](https://doi.org/10.1016/0164-1212(93)90077-B). Keep the two apart.

The paper also claims the numbers are *interpretable*, not just predictive — three diagnostic readings
are stated: a high fan-in **and** fan-out suggests the procedure "may perform more than one function"; a
high complexity marks "stress points in a system (i.e., a procedure with high information traffic through
it)"; and "a large fan-in or fan-out" may indicate a **missing level of abstraction** or "inadequate
refinement" (an implementation problem if the procedure is also long). All three readings treat high
fan-in as *structure to interpret*, never as a defect class on its own.

### 2.3 The empirical evidence (numeric)

System: **UNIX version 6**. Sample: **165 procedures, of which 80 had changes** (their Figure 10 sums to
165/80; the complexity histogram tallies the same 165). Exclusions: "Procedures written in assembly
language and certain memoryless procedures were eliminated from the information flow analysis"; the `U`
data structure was dropped as an outlier ("the U data structure has 3303 global flows with 84
procedures... Accordingly, the U structure will not be given further consideration"). Change data came
from "the UNIX users group" — reference [15] is "M. Ferentz, Rockefeller Univ., private
correspondence, 1979".

| Factor correlated with % of procedures changed (their Figure 15) | Spearman r | significance |
|---|---|---|
| procedure complexity — the full `length × (fan-in × fan-out)²`, by order of magnitude | **0.94** | 0.021 |
| `(fan-in × fan-out)²` alone | **0.98** | 0.028 |
| `(fan-in × fan-out)` alone | 0.83 | 0.042 |
| `length²` | 0.60 | 0.078 |
| `length` | — | not computable ("Due to the density of this distribution of length it was not possible to obtain a meaningful correlation coefficient") |

Change rate by complexity order 10⁰→10⁷: 12%, 32%, 46%, 70%, 58%, 92%, 67%, 0%
(17/38/41/27/26/12/3/1 procedures). Also: 11 of the 12 procedures with complexity ≥ 10⁵, and 2 of 3
with complexity ≥ 10⁶, required changes; of procedures violating the "one module only" property, 38 of
53 were in the change list. Complexity values ranged from 4 to 27,432,000. The paper's own summary:

> The connections of a procedure to its environment, namely `(fan-in fan-out) ** 2`, is an extremely
> good indicator of complexity.

> we find that length actually detracts from the predictive accuracy of the complexity formula
> indicating that at least as far as UNIX is concerned length is not a reliable indicator of procedure
> complexity.

One internal inconsistency worth knowing: the text says "significance levels for the correlation
coefficients reported in the rest of this section are similar to this 2 percent level", but Figure 15
prints .028, .042 and .078.

**Limitations the authors state themselves:** the interface/coupling measurements (their §V) are
explicitly unvalidated — "we have found no satisfactory way to thoroughly validate these measurements
using the UNIX data we currently possess."

### 2.4 Later replication, criticism, and treatment in surveys

- **Kitchenham 1988, "An evaluation of software structure metrics"** (COMPSAC, DOI
  [10.1109/CMPSAC.1988.17200](https://doi.org/10.1109/CMPSAC.1988.17200)) is the direct evaluation on
  an industrial system (a communications system), and it is negative. Abstract: "It was found that the
  design metrics were not as good at identifying change-prone, fault-prone, and complex programs as
  simple code metrics (i.e. lines of code and number of branches). It was also observed that the
  compound metrics, built up from several different basic counts, can obscure underlying effects, and
  thus make it more difficult to use metrics constructively." (Quoted from the OpenAlex record; the
  paper body is paywalled.)
- **Kitchenham, Pfleeger & Fenton 1995**, "Towards a framework for software measurement validation",
  IEEE TSE 21(12):929–944, DOI [10.1109/32.489070](https://doi.org/10.1109/32.489070) — the
  representational-theory critique. Attributed to it: fan-in and fan-out are "treated as homogenous
  attributes by virtue of the scalar multiplication", and "The authors have not indicated whether this
  is based on any observations or empirically validated scientific models." *Verification note: the TSE
  paper is paywalled and this wording was read second-hand from Yang (2010), University of Auckland PhD
  thesis, "Measuring Indirect Coupling" —
  <https://citeseerx.ist.psu.edu/document?repid=rep1&type=pdf&doi=145cb89e3f7a52b1df75d02f8b830c31b2fea8f0>.
  Quote the thesis, not the TSE paper, if this line is reused.*
- **Shepperd & Ince 1994, "A critique of three metrics"**, JSS 26(3):197–210, DOI
  [10.1016/0164-1212(94)90011-6](https://doi.org/10.1016/0164-1212(94)90011-6) — the three are
  Halstead's, cyclomatic complexity, and **information flow**. Body paywalled; the composition of the
  three is confirmed by a later open-access paper
  ([arXiv:2012.12324](https://arxiv.org/pdf/2012.12324v1)).
- **Counterweight — Shepperd 1991**, *Journal of Software Maintenance* 3(4), DOI
  [10.1002/smr.4360030404](https://doi.org/10.1002/smr.4360030404) reports that information-flow
  based coupling *does* predict errors: "a 600% greater probability of a residual error as a
  consequence of a change in a module with a high level of information flow-based coupling".
- **MacDonell 1991**, "Rigour in software complexity measurement experimentation", JSS 16:141–150 —
  attacks the validation design itself, on two grounds: attribute substitution ("Henry and Kafura use
  program changes as an equivalent substitute for development errors in the empirical validation of
  their information flow metric... the validity of the assumption could be questionable") and sample
  size ("the length of 53% of the modules examined was less than twenty lines, and the largest was just
  180 lines long. If larger procedures had been analysed, a different metric may have been developed").
  Text at
  <https://openrepository.aut.ac.nz/bitstreams/6b7a0b4f-45f7-4ad8-9b1b-f9621a5903a7/download>.
- **The size question, on the paper's own numbers.** H&K's factor analysis reports `(fan-in × fan-out)²`
  correlating with change at **0.98**, while the length-inclusive composite correlates at **0.94** — a
  pure structural term beats the composite — and no independent length-vs-change correlation was ever
  computed ("Due to the density of this distribution of length it was not possible to obtain a
  meaningful correlation coefficient"). Kitchenham 1988 then found LOC and branch counts *better* than
  the information-flow design metrics. So the strongest reading of the evidence is that H&K's
  information-flow term behaves like a size/complexity proxy, and the multiplicative `length ×` factor
  adds nothing measurable.
- **Authors' own follow-ups:** Henry, Kafura & Harris 1981, "On the relationships among three software
  metrics", *SIGMETRICS PER* 10(1):81–88, DOI
  [10.1145/1010627.807911](https://doi.org/10.1145/1010627.807911) — the information-flow metric "appears
  to be an independent measure of complexity" relative to Halstead effort and cyclomatic complexity;
  Henry & Kafura 1984, *Software: Practice and Experience* 14(6):561–573, DOI
  [10.1002/spe.4380140606](https://doi.org/10.1002/spe.4380140606) re-validates on the UNIX kernel.
- **A correction on Briand, Daly & Wüst 1999.** The full text of that paper (IEEE TSE 25(1):91–121)
  contains **zero** occurrences of "fan-in"/"fan-out". The common claim that it formalises H&K
  fan-in/fan-out as coupling dimensions is not supported by the paper; it formalises *object-oriented*
  coupling (CBO, RFC, and their own export/import coupling framework).

---

## 3. Change impact: Haney 1972 and Yau & Collofello

This is the branch of the lineage that actually asks *"if this module changes, what else must change?"*
— the brief's "blast radius" — and it is older than Henry & Kafura.

### 3.1 Haney 1972 — change-propagation probabilities, not a ratio

**Paper:** R. J. Haney, "Module Connection Analysis: A Tool for Scheduling Software Debugging
Activities", AFIPS Fall Joint Computer Conference 1972, Vol. 41 Part 1, pp. 173–179, DOI
[10.1145/1479992.1480016](https://doi.org/10.1145/1479992.1480016). Read from the AFIPS proceedings
scan at <http://www.bitsavers.org/pdf/afips/1972-12_%252341_Part_1.pdf>.

**Correction to a common gloss.** Haney does **not** define a stability ratio; there is no
"intra-module / total connections" formula anywhere in the paper, and "stability" appears only
qualitatively ("stability is never achieved"). What he defines is a change-propagation model:

> The technique described here, called Module Connection Analysis, is based on the idea that every
> module pair ... of a system has a finite (possibly 0) probability that a change in one module will
> necessitate a change in any other module.

> The fundamental axiom of module connection analysis is that intermodule connections are the essential
> culprit in elongated schedules. That a change in one module creates the necessity for changes in other
> modules, and these changes create others, and so on.

> `Pij = Probability that a change in module i necessitates a change in module j.`

As printed, `T = A(I − P)⁻¹` (A = row vector of initial changes per module, T = total changes by
module); internal release *k* gets `AP^k`; he calls `(I − P)⁻¹` the **"ripple factor"**. `P` is meant to
be measured empirically: `Pij = (number of changes in j caused by i) / (total changes made to i)`.
Worked example: the Xerox Universal Timesharing System, 18 subsystems, 296 initial changes →
**2963.85** total required changes (critical path ≈ 15 months), with a sensitivity curve showing the
system "precariously close to 'critical mass'" at an average connection probability of ≈.04. Claimed
purpose: "quantitative estimates of the effects of module interconnections", for scheduling debugging
and staging internal releases.

So in Haney the blast radius is encoded in the **probability matrix**, not in a fan-in count.

### 3.2 Yau & Collofello — ripple effect and stability measures

**Papers:** Yau, Collofello & MacGregor, "Ripple effect analysis of software maintenance", COMPSAC 1978,
pp. 60–65, DOI [10.1109/CMPSAC.1978.810308](https://doi.org/10.1109/CMPSAC.1978.810308); Yau &
Collofello, "Some Stability Measures for Software Maintenance", *IEEE TSE* SE-6(6):545–552, 1980, DOI
[10.1109/TSE.1980.234503](https://doi.org/10.1109/TSE.1980.234503); Yau & Collofello, "Design Stability
Measures for Software Maintenance", *IEEE TSE* SE-11(9), 1985, DOI
[10.1109/TSE.1985.232544](https://doi.org/10.1109/TSE.1985.232544). The 1980 paper's full text is
reproduced verbatim in the appendix of Yau's own technical report RADC-TR-83-262 (NTIS AD-A143763),
which is the copy read here: <https://archive.org/details/DTIC_ADA143763>.

Definitions, verbatim:

> One of the most important quality attributes of software maintainability is the stability of a
> program, which indicates the resistance to the potential ripple effect that the program would have
> when it is modified.

> The logical stability of a module is a measure of the resistance to the expected impact of a
> modification to the module on other modules in the program in terms of logical considerations.

Formulas (RADC-TR-83-262 §7.1): `LRE_k = Σ_i P(ki)·LCM_ki` with `LCM_ki = Σ_{t∈W_ki} C_t`
(`C_t` = a module complexity measure, `W_ki` = "the modules involved in the intermodule change
propagation"); module stability **`LS_k = 1 / LRE_k`**; program level `LSP = 1 / LREP`; design level
`DS_x = 1 / DLRE_x`. The set that is the literal ancestor of "blast radius" is named:

> RIPPLEM the set of modules in a program which are affected by the logical ripple effect.

Note what the unit is: a **probability-weighted, complexity-weighted count of affected modules**, not a
raw module count and not a ratio. Empirical support is weak on the authors' own wording — "An indirect
validation of these stability measures is also given" (TSE 1980); "A limited validation experiment for
our logical stability measure has also been conducted" (RADC-TR-83-262). **Neither paper defines a
fan-in metric**; fan-in is implicit in the propagation sets. Yau & Collofello later classify Haney
among the *probabilistic* precursors (with Soong and Myers) and criticise it for "an assumption that
all modifications to a module have the same ripple effect, a symmetry assumption".

### 3.3 The modern continuation: impact sets

Arnold & Bohner 1993 (ICSM, DOI [10.1109/ICSM.1993.366933](https://doi.org/10.1109/ICSM.1993.366933),
green-OA copy at <https://www.cs.purdue.edu/homes/xyzhang/spring07/Papers/00366933.pdf>) rename and
systematise this branch:

> Impact analysis (IA) is the activity of identifying what to modify to accomplish a change, or of
> identifying the potential consequences of a change.

and give the impact-set vocabulary that is the direct descendant of Yau's ripple set: the **starting
impact set (SIS)**, the **estimated impact set (EIS)**, and the **actual impact set (AIS)** — "the set of
objects ... actually modified as the result of performing the change". Bohner & Arnold's 1996 IEEE CS
Press collection *Software Change Impact Analysis* reprints Arnold & Bohner 1993 but **not** Haney or
Yau & Collofello.

---

## 4. Robert C. Martin: package metrics and the SDP

**Primary texts, in order of appearance** (all read directly; the live Object Mentor PDFs still serve):

1. *OO Design Quality Metrics: An Analysis of Dependencies*, © 1994 Robert C. Martin —
   <http://www.objectmentor.com/resources/articles/oodmetrc.pdf>. This is the origin of `Ca`, `Ce`,
   `I`, `A`, `D`. At this point the counted granule is Booch's "Class Category"; from the 1996 C++ Report
   columns onward it is the **package**.
2. C++ Report "Engineering Notebook" columns: 5th, *Granularity* (REP/CRP/CCP + **ADP**) —
   <http://www.objectmentor.com/resources/articles/granularity.pdf>; 6th, *Stability* (**SDP**, **SAP**,
   all the metrics, main sequence, zones) —
   <http://www.objectmentor.com/resources/articles/stability.pdf>.
3. *Design Principles and Design Patterns* (2000 white paper) — condensed book chapter; text mirrored at
   <https://www.seeleycoder.com/wp-content/uploads/2019/04/design_principles.pdf>.
4. *Agile Software Development: Principles, Patterns, and Practices*, ch. 20 "Principles of Package
   Design" (pp. 253–268) and ch. 22 (metric summary) — **the canonical primary text**. Original 2002
   Prentice Hall, ISBN 0-13-597444-5. (Quotes below marked "book ch.20" come from the Pearson reprint
   ISBN 978-1-292-02594-0.)

### 4.1 Exact definitions

> **Ca : Afferent Couplings :** The number of classes outside this category that depend upon classes
> within this category.
>
> **Ce : Efferent Couplings :** The number of classes inside this category that depend upon classes
> outside this categories. [sic]
>
> **I : Instability :** `(Ce ÷ (Ca+Ce))` : This metric has the range [0,1]. I=0 indicates a maximally
> stable category. I=1 indicates a maximally instable category.

> **A : Abstractness :** `(# abstract classes in category ÷ total # of classes in category)`. This
> metric range is [0,1]. 0 means concrete and 1 means completely abstract.

> **D : Distance :** `|(A+I-1)÷√2|` : The perpendicular distance of a category from the main sequence.
> This metric ranges from [0,~0.707]. (One can normalize this metric to range between [0,1] by using
> the simpler form `|(A+I-1)|`. I call this metric Dn.)

The counted unit is **classes**, never packages:

> The Ca and Ce metrics are calculated by counting the number of classes outside the package in
> question that have dependencies on the classes inside the package in question.

Wording drift across editions is real but immaterial: the 2000 paper says Ce is "The number of classes
outside the package that classes inside the package depend upon"; book ch.22 says "the number of classes
in other packages that the classes in the subject package depend on". The 1994 paper prints `D` with
`÷2`; the 1996 column, the 2000 paper and the book all print `÷√2`.

### 4.2 What SDP actually says

Column form (verbatim, all caps in the original):

> THE DEPENDENCIES BETWEEN PACKAGES IN A DESIGN SHOULD BE IN THE DIRECTION OF THE STABILITY OF THE
> PACKAGES. A PACKAGE SHOULD ONLY DEPEND UPON PACKAGES THAT ARE MORE STABLE THAT IT IS. [sic]

Book ch.20 form: **"Depend in the direction of stability."** Operationally:

> The SDP says that the I metric of a package should be larger than the I metrics of the packages that
> it depends upon. i.e. I metrics should decrease in the direction of dependency.

### 4.3 **High fan-in: good, bad, or neutral? — the sources' answer**

**High `Ca` is not a defect in this framework. It is the definition of stability, and Martin frames it
as responsibility.**

> I call classes that are heavily depended upon, "Responsible". Responsible classes tend to be stable
> because any change has a large impact.
>
> The most stable classes of all, are classes that are both Independent and Responsible.

> **I=0** means that the package is depended upon by other packages, but does not itself depend upon any
> other packages. It is *responsible and independent*. Such a package is as stable as it can get.

> **I=1** means that no other package depends upon this package; and this package does depend upon other
> packages. This is as instable as a package can get; it is *irresponsible and dependent*.

Book ch.20 adds the explicit reason — and note that this is a *cost*, i.e. the thing the brief calls
"blast radius", but Martin treats it as a property to be *chosen deliberately*, not a smell:

> A package with lots of incoming dependencies is very stable because it requires a great deal of work
> to reconcile any changes with all the dependent packages.

It turns bad in exactly **two** stated situations:

1. **High fan-in + low abstractness** — the "Zone of Pain":
   > the lower left point of the AI graph represents packages that are concrete and have lots of
   > incoming dependencies. This point represents the worst case for a package.
2. **Fan-in landing on a package that was meant to be volatile** — and here the fault is the depender:
   > Any package that we expect to be volatile should not be depended on by a package that is difficult
   > to change!

The only place Martin actively *reduces* `Ca` is a main-sequence correction, not a general indictment:

> We may want to hide certain classes within a package to prevent afferent couplings... In order to
> keep this package on the main sequence, we want to limit its afferent couplings, so we hide the
> classes that other packages don't need to know about. (book ch.22)

Martin also refuses to treat even the `(0,0)` corner as universally bad, because pain depends on
volatility:

> a concrete utility library ... may in fact be nonvolatile ... Such packages are harmless in the
> (0,0) zone since they are not likely to be changed. (book ch.20)

And the conclusion of the 1996 column is a blanket disclaimer about the whole construct:

> a metric is not a god; it is merely a measurement against an arbitrary standard.

**Summary for the brief:** in the primary sources, high fan-in is **neutral-to-good and explicitly a
responsibility**; it is a hazard only in combination (`Ca` high with `A` low) or misplaced (volatile
target). "This file has many importers" is, in Martin's vocabulary, a statement that the file *is
stable* — which is what you normally want from a shared interface. The signal that something is wrong
is the *absence* of abstraction on top of that stability, or a dependent that should not have pointed
there.

### 4.4 SAP

Column form:

> PACKAGES THAT ARE MAXIMALLY STABLE SHOULD BE MAXIMALLY ABSTRACT. INSTABLE PACKAGES SHOULD BE CONCRETE.
> THE ABSTRACTION OF A PACKAGE SHOULD BE IN PROPORTION TO ITS STABILITY.

Book ch.20 form: **"A package should be as abstract as it is stable."** Purpose:

> It says that a stable package should also be abstract so that its stability does not prevent it from
> being extended.

> The SAP and the SDP combined amount to the Dependency Inversion Principle for Packages... the SDP says
> that dependencies should run in the direction of stability, and the SAP says that stability implies
> abstraction. Thus, dependencies run in the direction of abstraction.

### 4.5 ADP (the cycle principle)

> THE DEPENDENCY STRUCTURE BETWEEN PACKAGES MUST BE A DIRECTED ACYCLIC GRAPH (DAG). THAT IS, THERE MUST
> BE NO CYCLES IN THE DEPENDENCY STRUCTURE.

(2000 paper: "The dependencies betwen packages must not form cycles" [sic]; book ch.20: "Allow no cycles
in the package-dependency graph.") The stated consequences are exactly change/release/test coupling:

> MyTasks now depends upon every other package in the system. This makes MyTasks very difficult to
> release.

> the cycle has had the effect that MyApplication, MyTasks, and MyDialogs must always be released at
> the same time. They have, in effect, become one large package.

> we must link in every other package in the system... just to run a simple unit test ... compile times
> grow geometrically with the number of modules.

Exactly two remedies are given: apply the Dependency Inversion Principle, or "create a new package that
both ... depend upon [and] move the class(es) that they both depend upon into that new package."

### 4.6 Main sequence, zones

A on the vertical axis, I on the horizontal; the good poles are `(0,1)` (stable and abstract) and
`(1,0)` (instable and concrete). The main sequence is the line joining them, i.e. `A + I = 1`; `D` is
perpendicular distance from it. Zone of Pain is around `(0,0)` (concrete, heavily depended upon); Zone
of Uselessness is around `(1,1)` (abstract with no dependents). Only ~half of packages can sit at the
endpoints; the rest are judged by distance to the line.

---

## 5. Chidamber & Kemerer 1994 — CBO and RFC

**Paper:** S. R. Chidamber and C. F. Kemerer, "A Metrics Suite for Object Oriented Design", *IEEE TSE*
20(6):476–493, 1994, DOI [10.1109/32.295895](https://doi.org/10.1109/32.295895).
**Provenance caveat:** the openly reachable TSE PDF is an image-only scan with no text layer. The quotes
below are from the authors' own MIT working paper of the same title, revised December 1993 — the
preprint of the TSE paper — read at
<https://www.eso.org/~tcsmgr/oowg-forum/TechMeetings/Articles/OOMetrics.pdf>. Briand, Daly & Wüst 1999
quote the published TSE wording verbatim, which independently confirms the definitions.

### 5.1 CBO

> **Definition.** CBO for a class is a count of the number of other classes to which it is coupled.

> Since objects of the same class have the same properties, two classes are coupled when methods
> declared in one class use methods or instance variables of the other class. [footnote 5:] Note that
> this will include coupling due to inheritance.

Coupling is **symmetric and direction-blind**:

> any action by {MX} on {MY} or {IY} constitutes coupling, as does any action by {MY} on {MX} or {IX}.

Briand et al.'s formalisation: `CBO(c) = |{ d | uses(c,d) ∨ uses(d,c) }|`, and they state plainly that
"CBO makes no distinction between import and export coupling". It is a **binary** count of *class pairs*
— multiple method calls to the same class count once, and a method call and a field access are weighted
identically.

**Stated purpose:**

> Excessive coupling between object classes is detrimental to modular design and prevents reuse. ...
> the higher the sensitivity to changes in other parts of the design, and therefore maintenance is more
> difficult. ... The higher the inter-object class coupling, the more rigorous the testing needs to be.

Briand's summary: CBO was proposed "as an indicator for maintainability, testability and reusability
of a class."

### 5.2 RFC

> **Definition.** `RFC = | RS |` where RS is the response set for the class.
>
> `RS = { M } ∪ all i { Ri }` where `{ Ri }` = set of methods called by method i and `{ M }` = set of
> all methods in the class.

Footnote 26 settles the depth question, which the formula alone leaves ambiguous:

> membership to the response set is defined only up to the first level of nesting of method calls due
> to the practical considerations involved in collection of the metric.

**Stated purpose:** "the testing and debugging of the class becomes more complicated"; "A worst case
value for possible responses will assist in appropriate allocation of testing time"; and RFC "is also a
measure of the potential communication between the class and other classes."

### 5.3 Empirical validation

Two sites: Site A, a software vendor, C++, two GUI class libraries, **634 classes**; Site B, a
semiconductor manufacturer, Smalltalk CAM/VLSI system, **1459 classes**, >30 engineers. Reported
distributions include CBO median 0 / max 84 (Site A) and median 9 / max 234 (Site B); RFC median 6 /
max 120 and median 29 / max 422. The authors explicitly do not establish predictive validity:

> The most obvious extension of this research is to analyze the degree to which these metrics correlate
> with managerial performance indicators, such as design, test and maintenance effort, quality and
> system performance.

and they decline to generalise across the two languages: "no claims are offered as to any systematic
differences between the C++ and Smalltalk environments."

### 5.4 The information-flow lineage — a correction

CK's stated theoretical base is **Bunge's ontology**, not information flow:

> Following Wand and Weber, the theoretical base chosen for the metrics was the ontology of Bunge.

Coupling is defined ontologically via Wand: "two objects are coupled if and only if at least one of them
acts upon the other, X is said to act upon Y if the history of Y is affected by X." The 1991 OOPSLA
predecessor instead cites traditional module coupling: "This is consistent with traditional definitions
of coupling as 'measure of the degree of interdependence between modules' [Pressman, 1987]." Fan-in/fan-out
appears in CK's work only as a **planned comparison metric** in future work, never as CBO's basis. The
CK94 preprint bibliography contains no Henry & Kafura and no "information flow" entry (only Li & Henry,
later work).

### 5.5 Later formalisation and critique

- **Briand, Daly & Wüst 1999** (<http://www.sdml.cs.kent.edu/library/Briand%2799.pdf>) classify CBO as
  conflating direction (import/export), types (invocation/attribute), inheritance, and strength
  (binary, ignoring frequency): "a class c which is loosely coupled to five other classes may be easier
  to maintain than a class d which is strongly coupled to only two."
- **El Emam et al. 2001**, TSE 27(7):630–650, DOI [10.1109/32.935855](https://doi.org/10.1109/32.935855):
  paywalled with no open copy reachable during this research; only the bibliographic record was verified.
  Its "size confounding" finding is widely cited but was **not** re-verified here.
- **Tahir et al., ESEM 2018** (open copy <https://arxiv.org/pdf/2104.12349v1>): the abstract says size
  mediation "is not consistent in all examined systems" and "We are unable to confirm if class size has
  a significant mediation or moderation effect." The strong "size fully mediates Fan-in/Fan-out" claim
  should not be attributed to this paper.

---

## 6. Tool implementations

*(Streams: Henry & Kafura 1981; Haney 1972 / Yau & Collofello / Arnold & Bohner; Martin's package
principles; Chidamber & Kemerer 1994; SonarQube / NDepend / Structure101 / Lattix; dependency-cruiser /
madge / ESLint `import/no-cycle` / jdepend / ArchUnit. Every row was checked against the tool's own
documentation or source.)*

### 6.1 Consolidated table

| Tool | Counts what | Granularity | How presented | Thresholds / defaults | Source |
|---|---|---|---|---|---|
| **dependency-cruiser** | resolved per-module dependency edges from parsed imports/requires; each edge carries `circular`, `cycle`, `dependencyTypes`, `dynamic` | file/module (folder for `metrics` reporter / `--collapse`) | `err` lines, dot/ddot/archi SVG, `metrics` numeric table, json/html/teamcity; non-zero exit on `error` rules | preset `recommended`: `no-orphans` **warn**, `no-circular` **error**, `no-deprecated-core` error, `no-duplicate-dependency-types` warn, `no-non-package-json` error, `not-to-deprecated` warn, `not-to-unresolvable` error. `--metrics` **off by default**; `maxDepth` unlimited; `instability = Ce/(Ce+Ca) \|\| 0` | <https://github.com/sverweij/dependency-cruiser/blob/main/configs/recommended.cjs> · <https://github.com/sverweij/dependency-cruiser/blob/main/src/analyze/derive/module-utl.mjs> |
| **madge** | resolved module graph via `dependency-tree` + `filing-cabinet` | file/module | text list, `--json`, `--dot`, `--image svg`, `--summary` fan-out counts, `--circular` path list; **exit 1** on cycles | no numeric thresholds; `fileExtensions:['js']`, `includeNpm:false`; cycle check opt-in via `--circular`; type/dynamic imports count unless `skipTypeImports`/`skipAsyncImports` set | <https://github.com/pahen/madge/blob/master/README.md> · <https://github.com/pahen/madge/blob/master/lib/cyclic.js> |
| **ESLint `import/no-cycle`** | static `import` declarations; SCC pre-filter then BFS; `require()` inside imported modules may be missed | file/module | ESLint diagnostic at configured severity; **no autofix** | `maxDepth` default `Infinity`; `ignoreExternal:false`; `allowUnsafeDynamicCyclicDependency:false`; self-imports and type-only imports ignored. Companion `import/max-dependencies` (fan-**out**): default **10** | <https://github.com/import-js/eslint-plugin-import/blob/main/src/rules/no-cycle.js> · <https://github.com/import-js/eslint-plugin-import/blob/main/docs/rules/max-dependencies.md> |
| **jdepend** | Java **bytecode** package dependencies (constant-pool class refs); `Ca`/`Ce` count other *packages* | package; "components" = package + sub-packages via `-components` | text report (`Ca`, `Ce`, `A`, `I`, `D`, `Depends Upon`, `Used By`, cycle tree, CSV summary), XML UI, Swing GUI, Ant task, JUnit `DependencyConstraint` | **no thresholds and no `-X`**; only `-file` / `-components`; `ignore.*` filters (none by default); volatility default 1; `D = \|A+I−1\| × V` | <https://github.com/clarkware/jdepend/blob/master/docs/JDepend.html> · <https://github.com/clarkware/jdepend/blob/master/src/jdepend/framework/JavaPackage.java> |
| **ArchUnit** | Java bytecode class dependencies (method/constructor calls, field access, inheritance, annotations) | user-defined **slices** (package infix `(*)` / `(**)` or `SliceAssignment`); layers; classes | JUnit test failure (`AssertionError`) via `@ArchTest` / `rule.check(...)` | `cycles.maxNumberToDetect` default **100**; `cycles.maxNumberOfDependenciesPerEdge` default **20**; core `CycleDetector.detectCycles(nodes, edges)`; Lakos CCD/ACD/RACD/NCCD available | <https://www.archunit.org/userguide/html/000_Index.html> |
| **SonarQube** | **No coupling *metric*, but coupling *rules*.** The metric definitions contain no `afferent`/`efferent`/CBO/RFC/LCOM keys. Instead: **S1200** "Classes should not be coupled to too many other classes" counts the classes a class references (fields, parameters, return types, calls; nested-class dependencies excluded) and needs a resolved type model; **S6539** counts a class's **imports** (fan-out) and is explicitly labelled experimental; **S7197** reports "circular dependencies between source files, including indirect cycles spanning multiple files". Architecture view: "A tangle is a set of classes or files that depend on each other in a cycle." | class (S1200/S6539); file (S7197); architecture "containers" | issues participating in the quality gate; Architecture current-vs-intended view (tangles = flaws) | S1200 `max` default **20** (`DEFAULT_MAX = 20` in `ClassCouplingCheck.java`), Major / CODE_SMELL / 2h; S6539 `couplingThreshold` default **20**, "experimental value"; S7197 no numeric threshold. Architecture is **commercial-only** (Developer/Enterprise/Data Center, not Community Build); legacy cycle detection is deprecated, removal January 2026 | <https://docs.sonarsource.com/sonarqube-server/latest/user-guide/code-metrics/metrics-definition/> · <https://github.com/SonarSource/sonar-java/blob/master/java-checks/src/main/java/org/sonar/java/checks/design/ClassCouplingCheck.java> · <https://github.com/SonarSource/sonar-java/blob/master/java-checks/src/main/java/org/sonar/java/checks/design/ClassImportCouplingCheck.java> · <https://docs.sonarsource.com/sonarqube-server/architecture-analysis> |
| **NDepend** | .NET dependency metrics at five levels: Assembly `Ca`/`Ce`, `NamespaceCa`/`NamespaceCe` ("The Afferent Coupling for a particular namespace is the number of namespaces that depends directly on it"), `TypeCa`/`TypeCe`, `MethodCa`/`MethodCe`, `FieldCa`; plus `I`, `A`, `D`, Relational Cohesion `H = (R+1)/N`, `TypeRank` (PageRank over the type-dependency graph, normalised so the average is 1), and `Level` | assembly / namespace / type / method / field | metric dashboard with baseline diff, treemap, CQLinq rules → issues/technical debt, and a DSM where "If a structure contains a cycle, the cycle is displayed by a red square" | published guidance: `TypeCe > 50` "depends on too many other types"; `NormDistFromMainSeq > 0.7`; RelationalCohesion good 1.5–4.0; LCOM > 0.8. Rule `ND1410 AvoidExcessiveClassCoupling` (CC ≥ 10 method / 25 type, types used ≥ 40/90, namespaces used ≥ 5/10); `ND1407` on `NormDistFromMainSeq > 0.7`; `ND1402 warnif count > 10`. Cycles via `Level == null` (ND1400/ND1401 namespaces, ND1409 types, ND1213 type-initialisation cycles) | <https://www.ndepend.com/docs/code-metrics> · <https://www.ndepend.com/default-rules/NDepend-Rules-Explorer.html> · <https://www.ndepend.com/docs/dependency-structure-matrix-dsm> |
| **Structure101** | "Fat" and "Tangles": "Fat is too much stuff at any point of the source code composition... Tangles occur when code containers are cyclically dependent." Measures: *folder feedback dependencies* ("the sum total of all code-level dependencies that are in the 'minimum feedback set' of a folder tangle"), *tangled folder*, *fat folders*, *fat files*, *fat functions/methods* ("Functions with too many possible execution paths" — i.e. cyclomatic complexity), *biggest file tangle* ("cyclic dependencies between files... measured as the number of files it includes"). Above function level, Fat = "the number of dependencies in the dependency graph of sub-items" | function/method, class, file, leaf package, design package, jar/component; architecture diagrams | a single **XS ("excess")** metric = Fat + Tangles, shown as a distribution chart and complexity perspective; DSM/graph; Structure101 Build enforces specs | `Fat = Max(Value − Threshold, 0) / Value`; "Design-scope dependency graphs should be acyclic, so the threshold for Tangled (Design) is usually set to 0"; tangle degree = MFS dependencies / total dependencies (worked example 2.8%) | <https://web.archive.org/web/20180101000000id_/http://www.structure101.com/static-content/pages/resources/documents/XS-MeasurementFramework.pdf> · <https://www.sonarsource.com/structure101/docs/cpa/studio/Content/reference/key-measures.html> · <https://www.sonarsource.com/structure101/docs/java/studio/content/restructure101/tangles> |
| **Lattix LDM** | Reverse-engineered Dependency Structure Matrix over calls/references; distinguishes **Actual Dependencies**, **Permitted References** and **Disallowed References** (design rules); dependency strength = "the number of calls between source files"; productises the MacCormack et al. visibility measures (Fan-In/Fan-Out visibility, i.e. transitive reachability). Lattix's own blog names the system-level reading **"system stability"**: "System stability measures how sensitive the system is to change. When a change is made to the software, system stability will tell you how much of the rest of the software will be affected." | package/namespace hierarchy and nested clusters; source-file DSM | DSM matrix; partitioning exposes cyclic blocks; layered architecture model; design rules | design rules only; no published numeric default found | <https://web.archive.org/web/20060208040538id_/http://www.lattix.com/download/dl/DSM_for_Software_Architecture.pdf> · <https://blog.lattix.com/measure-your-software-architectural-health> · see §6.4 |

### 6.2 What "counts what" means in practice

Two independent axes of variation matter for qingluan:

1. **Source imports vs. resolved/compiled references.** dependency-cruiser, madge and ESLint
   `import/no-cycle` parse source imports; jdepend and ArchUnit read **bytecode**, so they see class
   references that never appear as imports, and they see *classes*, not modules. Martin's own `Ca`/`Ce`
   are defined over classes, and his 1996 column even notes that `I` "is easiest to calculate when you
   have organized your source code such that there is one class in each source file" — i.e. the
   practical bridge from class-level to file-level counting is a one-class-per-file convention.
2. **Cycles vs. magnitude.** No tool surveyed ships a *fan-in* threshold. The magnitude numbers
   (`Ca`, `Ce`, `I`, instability) are reported for a human or a custom rule to interpret. Magnitude
   thresholds do exist, but as advisory rules or fan-out caps: SonarQube S1200/S6539 (20), NDepend
   ND1410/`TypeCe > 50`, ESLint `import/max-dependencies` (10). The only rules that fail a build by
   default are the cycle rules.

### 6.3 jdepend versus Martin

jdepend is the closest thing to a reference implementation of Martin's package metrics, and it is
faithful but adds two things Martin did not: it counts *packages* (not classes) on each side of the
edge, and it multiplies `D` by a per-package **volatility** factor (`jdepend.properties`, default 1).
It reports cycles separately under `Package Dependency Cycles`, with the memorable line:

> Packages participating in a package dependency cycle are in a deadly embrace with respect to
> reusability and their release cycle.

### 6.4 Lattix and propagation cost — the one "many things depend on me" metric with a transitive closure

The most literal formalisation of the brief's intuition is **propagation cost**, from MacCormack, Rusnak
& Baldwin (HBS Working Paper 05-016; published *Management Science* 52(7):1015–1030, 2006, DOI
[10.1287/mnsc.1060.0552](https://doi.org/10.1287/mnsc.1060.0552); text read from
<https://www.hbs.edu/ris/Publication%20Files/05-016.pdf>). Verbatim:

> We use the technique of matrix multiplication to identify the "visibility" of any given element for
> any given path length. Specifically, by raising the dependency matrix to successive powers of n, the
> results show the direct and indirect dependencies that exist for successive path lengths. By summing
> these matrices together we derive the visibility matrix V, showing the dependencies that exist for all
> possible path lengths up to n.

> The first, called "Fan-Out Visibility," is obtained by summing along the rows of the visibility
> matrix, and dividing by the total number of elements. An element with high Fan-Out visibility depends
> upon (or calls functions within) many other elements. The second, called "Fan-In Visibility," is
> obtained by summing down the columns of the visibility matrix, and dividing by the total number of
> elements. An element with high Fan-In visibility has many other elements that depend upon it (or call
> functions within it).

> We call the resulting metric "Propagation Cost." Intuitively, this measures the proportion of elements
> that could be affected, on average, when a change is made to one element in the system.

Three properties matter for qingluan:

1. This is the **transitive** version of fan-in — not "how many files import me", but "how many elements
   can reach me through a chain of any length". A direct-import fan-in of 3 can have a fan-in visibility
   of 300.
2. The unit of analysis in the paper is explicitly **the source file** ("The Unit of Analysis: The Source
   File"), and the dependency is a file-to-file use/call relationship.
3. The visibility matrix is **binarised** ("we limit values in the visibility matrix to be binary,
   capturing only the fact there exists a dependency, and not the number of possible paths"), which is
   also how it stays well-defined in the presence of cycles.

In their data, Mozilla's propagation cost fell from 15–18% to 2–6% over the studied period.

**A naming trap worth recording.** The metric's name is *not* stable across versions. The HBS working
paper read for this report (05-016) uses **"Propagation Cost"** and **"Clustered Cost"** — both quoted
above verbatim. A second research stream reports that the published *Management Science* version uses
**"Change Cost"** and **"coordination cost"** instead (0 occurrences of "propagation cost" in the copy
it read), and that the Lattix-side term for the system-level reading is "system stability". The INFORMS
text is paywalled and the free mirrors were unreachable from this environment, so this report does not
resolve which term the 2006 published version uses. The *definition* is identical either way; the term
that stuck in the literature, and in tool marketing, is "propagation cost".

**Lattix LDM** is the commercial tool that productises these measures; its own documentation had to be
read through a 2006 archived white paper because lattix.com returns HTTP 403 to this environment. So
the Lattix-side presentation above is thinner than for the other tools — a genuine gap, flagged in
Appendix B.

---

## 7. Cycles are the defect; fan-in is not

Every surveyed tool encodes the same judgement, and the primary source behind it is Martin's ADP
(§4.5):

| Tool | What the docs treat as the defect | Quote / evidence |
|---|---|---|
| dependency-cruiser | cycles | `no-circular` ships at `severity: "error"`; comment: "This dependency is part of a circular relationship. You might want to revise your solution (i.e. use dependency inversion, make sure the modules have a single responsibility)." Its fan-in rule is described as "not sure it's like super useful or anything, it's just a side-effect of the previous use." |
| madge | cycles | `--circular` exits 1 when cycles exist. |
| ESLint `import/no-cycle` | cycles | "Cyclic dependency are **always** a dangerous anti-pattern..."; the high-fan-**out** rule `max-dependencies` (default 10) is catalogued separately as a code smell. |
| jdepend | cycles | "deadly embrace with respect to reusability and their release cycle"; `Ca`/`Ce`/`I`/`D` carry no thresholds. |
| ArchUnit | cycles / forbidden dependencies | headline slice rule `slices().matching(...).should().beFreeOfCycles()`; `notDependOnEachOther()`. |
| SonarQube | cycles ("tangles"), and separately excessive class coupling | "A tangle is a set of classes or files that depend on each other in a cycle. ... Tangles make code more complex, and harder to understand and maintain." Cycle rule S7197: "This rule reports circular dependencies between source files, including indirect cycles spanning multiple files"; S1200 treats coupling as a smell rather than a defect class: "Classes which rely on many other classes tend to aggregate too many responsibilities and should be split into several smaller ones." |
| NDepend | cycles and mutual dependency | Namespaces/types in cycles are selected by `Level == null` ("if the namespace is involved in a dependency cycle"); rules ND1400/ND1401 (namespaces mutually dependent / dependency cycles), ND1409 (mutually dependent types), ND1213 (type-initialisation cycles). Coupling magnitude has *guidance*, not build failure: `TypeCe > 50`. |
| Structure101 | tangles (cycles) are "objectively problematic"; Fat is over-complexity | "Tangles occur when code containers are cyclically dependent"; "Spec dependency violations"; "Architecture diagram violations". |

Only one family of rules surveyed turns a coupling *magnitude* into a violation, and it is not fan-in:
SonarQube S1200 (classes a class depends on, max 20) and S6539 (a class's imports, max 20), and NDepend
ND1410 (types/namespaces used). Every high-fan-in number remains a report or a graph edge.

Note also that none of the five JS/Java tools' own docs cite Martin or name the ADP (a `grep -i acyclic`
over dependency-cruiser's `doc/`, jdepend's `JDepend.html`, madge's README and the `no-cycle.md` docs
returned no matches) — the ADP citation is ours, from Martin's primary text, not the tools'.

---

## 8. What is actually computable from import statements alone

The brief asks specifically which of this lineage is reachable *without a call graph*. Answer, with the
reason from each definition:

| Thing | Import-graph computable? | Why |
|---|---|---|
| **File/module fan-in** (number of modules that import this one) | **Yes** for languages with explicit imports (Java, TS/JS, Python; Rust only with real module-path resolution) | it is literally the in-degree of the import graph |
| **File/module fan-out**, and `I = Ce/(Ca+Ce)` at file granularity | **Yes** | dependency-cruiser computes exactly this (`afferentCouplings` = `dependents.length`, `efferentCouplings` = `dependencies.length`) |
| **Cycles / SCCs** | **Yes** — this is the one property that is *only* a graph property | madge, dependency-cruiser, ESLint all implement it directly on the import graph |
| **Propagation cost, reachability, Lakos CCD/ACD/NCCD** | **Yes** at file/component level | all are reachability/edge counts over the dependency matrix |
| **Martin `Ca`/`Ce` as published** | **Partially / not literally** | they count **classes**, not files. Imports give module counts; getting class counts needs resolving which symbols are used, and same-package Java references produce *no import at all*, so a file-level approximation systematically undercounts |
| **Henry & Kafura fan-in/fan-out** | **No** | they are *information flows*: a value returned and then used, a global data structure read/written, a value passed through a third procedure. Imports see none of that |
| **CK CBO** | **No** | needs "methods declared in one class use methods or instance variables of the other class" |
| **CK RFC** | **No** | needs one level of call resolution inside the class plus its callees |

Note that real tools *do* ship the import-based approximation, at class rather than file granularity:
SonarQube **S6539** counts a class's imports with a default threshold of 20, and its own property
description calls the value "an experimental value". That is the closest published precedent for the
kind of number the brief proposes — and it caps fan-**out**, not fan-in.

Two practical caveats, both measured in this repo's sibling work
(`.scratch/complexity/research/coupling/local-measurements.md`, §"Resolution quality"):
import resolution is language-dependent and a **lower bound** — Java ~53.5% of files with a resolvable
importer (same-package refs invisible), Python ~41.3% (dynamic imports invisible), TS/JS ~23.3% (path
aliases and bare specifiers skipped), Rust ~2.7% with naive `crate::` resolution. And type-only imports
change the counts depending on the tool: ESLint `import/no-cycle` ignores `import type`; madge and
dependency-cruiser count them unless configured not to.

---

## 9. Implications for the proposed axis (inference, not source)

Marked clearly as our reading, not as a finding:

1. **The axis is real but its sign is the opposite of the naive framing.** All three lineages agree
   that "many things depend on this module" is a *stability/responsibility* fact. Calling it
   "complexity" without a companion variable would put the most-depended-upon, least-changed interface
   files at the top of a risk list — which is what the sibling local measurement found happens when you
   rank by raw fan-in.
2. **The defensible framings are the sources' own:** (a) fan-in as **stability** (`Ca`, `I`), which is a
   neutral property to display; (b) fan-in **× churn/change-rate** as blast radius; (c) fan-in **with
   abstractness** (Martin's Zone of Pain) as the actual hazard; (d) **cycles** as the actual defect.
   Option (d) is the only one every tool is willing to fail a build on.
3. **Granularity honesty is required.** From imports we can honestly report *file/module* fan-in,
   `I = Ce/(Ca+Ce)` at file granularity, and cycles. We cannot honestly call it Martin's `Ca` (class
   counts) or Henry & Kafura's fan-in (data flow) without labelling the downgrade.
4. **Do not re-derive a composite.** The sources give a *vector* (`Ca`, `Ce`, `I`, `A`, `D`, cycles),
   and Martin himself calls the whole thing "a measurement against an arbitrary standard". This is
   consistent with the repo's existing no-composite-score decision.

---

## Appendix A — source hygiene

- All PDFs were downloaded to workspace scratch directories (`.scratch/coupling-lineage-tmp*/`) and
  **removed before finishing**. This environment wipes `/tmp` between bash calls, so nothing persisted
  there either.
- Only short quotations are included. No paper, book chapter, upstream documentation page or source
  file was copied into the repo. The only file written is this report.
- Tool source claims were verified against raw GitHub at `main`/`master` on 2026-09-30.
- This workspace is shared with other agents working the same feature; an unrelated research stream
  removed a different set of scratch files mid-run. That stream's artifacts (`.scratch/complexity/
  research/coupling/`, `.scratch/complexity/research/coupling-*.md`, root-level `J*.pdf`) are **not**
  this report's and were left untouched.

## Appendix B — verification status and unresolved items

**Verified directly by the lead author** (text extracted and read; quotes above come from these):
Henry & Kafura 1981 body text (university full-text mirror); Martin's *OO Design Quality Metrics*,
*Stability* and *Granularity* (live Object Mentor PDFs); the ADP all-caps statement in `granularity.pdf`;
the MacCormack et al. working paper (HBS 05-016); SonarQube's metric-definitions page (no coupling keys)
and `ClassCouplingCheck.java` (`DEFAULT_MAX = 20`); dependency-cruiser `recommended.cjs`,
`no-circular.cjs` and `calculateInstability`; jdepend's `Ca`/`Ce`/`A`/`I`/`D` definitions and "deadly
embrace"; madge's exit-on-cycle; ESLint's `maxDepth` and `max-dependencies` default 10.

**Read second-hand or via mirrors — flagged in place:**
- H&K 1981: IEEE Xplore is bot-walled (HTTP 202). Body text came from a university mirror,
  cross-checked against Crossref/OpenAlex metadata and three independent restatements of the formula.
- CK94: the TSE PDF is an image-only scan with no text layer. Quotes are from the authors' Dec-1993 MIT
  preprint of the same paper, cross-confirmed by Briand et al.'s verbatim quotations of the TSE text.
- Kitchenham, Pfleeger & Fenton 1995: paywalled; the "homogeneous attributes" critique is quoted from a
  PhD thesis, not from the TSE paper.
- SonarQube S7197's rule text: read from Sonar's own community forum; the rule pages did not serve.
- Haney 1972 and Yau & Collofello: read from non-publisher scans (bitsavers AFIPS proceedings; the
  RADC-TR-83-262 technical report that reproduces the TSE 1980 paper), not from ACM/IEEE.

**Not verified / genuinely open:**
- **MacCormack et al. terminology** — working paper says "Propagation Cost"/"Clustered Cost" (verified);
  a stream reports the published version says "Change Cost"/"coordination cost". INFORMS is paywalled
  and the free mirrors were unreachable, so the discrepancy is left open.
- **Lattix first-party documentation** — lattix.com returns HTTP 403 and the Wayback Machine has no
  relevant snapshots; the Lattix row rests on one archived 2006 white paper plus a current first-party
  blog post.
- **Structure101 default Fat thresholds** — the XS white paper's example-thresholds figure is an image
  and did not extract.
- **El Emam et al. 2001** (DOI 10.1109/32.935855) and the TSE 2003 comments exchange
  (DOI 10.1109/TSE.2003.1214331) — paywalled with no open copy reachable; the size-confound result is
  cited by reputation only and was not re-checked.
- **Fenton & Pfleeger** *Software Metrics* and **Zuse** *Software Complexity* — no accessible copy; their
  treatment of H&K is asserted only via thesis paraphrase and is therefore not quoted here.
- **Shepperd & Ince 1994** body text — paywalled; only the bibliographic record and the identity of the
  three metrics could be confirmed.
