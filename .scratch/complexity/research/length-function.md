# Function-length thresholds: primary-source research

**Question.** What is the defensible default threshold for *function length* (lines per function) for a
review tool whose unit is **non-blank, non-comment lines (`nloc`)**, and where does each candidate
number come from?

**Research date:** 2026-09-30.
**Method:** every number below is read from the rule's own documentation page or its own source code
(checks/rule implementations, `default.yml`, README), a published paper, or an official style guide.
Secondary pages were used only to locate primaries. Anything not personally verified is marked
**unverified** rather than guessed.

**Companion docs:** `docs/research/code-complexity-metrics.md` (metric selection),
`.scratch/complexity/spec.md` (metric vector, thresholds CC 10 / cognitive 15, no composite score).

---

## 0. Verdict up front

1. **No empirical study establishes an optimal function length.** Nobody has published a defensible
   "the number is N". Every shipped number is a convention — chosen for readability, for "fits on a
   screen/page", or inherited from an earlier tool. `80` in particular appears as a default in **none**
   of the rule sets surveyed here; it is folklore.
2. **The honest framing for the CLI:** `nloc` is a *descriptive* metric with a *configurable* flag
   line. The threshold is a policy knob, not a finding. Never fold it into a score.
3. **Recommended default for qingluan: flag `nloc > 100`, with a documented strict preset at 60.**
   Rationale:
   - 100 is the only value that is the default of *more than one* tool that counts **exactly
     qingluan's unit** (Clippy `too_many_lines` = 100 non-blank non-comment lines; SonarSource S138 =
     100 for the Python/Kotlin/Swift/Scala/C/C++ group), and it is the low bound of PMD's (now removed)
     LoC rule.
   - 60 is the only number anchored in a **first-party engineering standard with a stated rationale**
     (Power of 10 Rule 4: one printed page; funlen: "fit a function within one screen"), and it is the
     right preset for safety-critical or strict repos.
   - Both are conventions. If the team wants a single conservative number instead, 60 is the better
     *provenance*; 100 is the better *ecosystem fit*.
4. **Do not copy SonarJava's 75 blindly.** Sonar's own per-language defaults range 75–200 for the same
   RSPEC rule (Java 75; JS/TS 200), which is itself evidence that the number is policy, not science.

---

## 1. Tool → default → exact semantics → primary URL

`nloc` below means "lines containing at least one character that is neither whitespace nor part of a
comment" (SonarQube's `ncloc` definition, quoted in §4).

| # | Tool / rule | Default | Exact semantics (what is counted) | Primary URL |
|---|---|---|---|---|
| 1 | **SonarSource S138 — Java** (`MethodTooBigCheck`) | **75** | `metricsComputer.getLinesOfCode(block)` = Sonar LOC of the method body → the `ncloc` notion, i.e. code lines; blank lines and comment-only lines excluded; lines with code + trailing comment count once. `@RuleProperty(defaultValue = "75")`. Historical: 3.14/4.0 shipped `DEFAULT_MAX = 100`, 5.14 already 75. | [MethodTooBigCheck.java](https://github.com/SonarSource/sonar-java/blob/master/java-checks/src/main/java/org/sonar/java/checks/MethodTooBigCheck.java) · [RSPEC S138 (SPA)](https://sonarsource.github.io/rspec/#/rspec/S138/javascript) |
| 2 | **SonarSource S138 — JS/TS** | **200** | `const DEFAULT = 200`; `getLocsNumber()` walks `loc.start.line-1 … loc.end.line` and **skips blank lines and full-line comments only** (`isFullLineComment` requires the comment to be the sole token on the line) → code lines, trailing comments do not add a line; counts the signature line and the closing-brace line. Skips IIFEs; skips React function components (capitalised name + returns JSX). Parameter name `maximum`, display name `max`, RSPEC default 200. | [rule.ts](https://github.com/SonarSource/SonarJS/blob/master/packages/analysis/src/jsts/rules/S138/rule.ts) · [config.ts](https://github.com/SonarSource/SonarJS/blob/master/packages/analysis/src/jsts/rules/S138/config.ts) · [meta.ts](https://github.com/SonarSource/SonarJS/blob/master/packages/analysis/src/jsts/rules/S138/meta.ts) |
| 3 | **SonarSource S138 — Python** (`TooManyLinesInFunctionCheck`) | **100** | `DEFAULT = 100`; counts `FunctionLineVisitor.linesOfCode` (distinct token line numbers = lines with code), then removes docstring lines → code lines only; blank lines and comment-only lines excluded. Property key `max`. | [TooManyLinesInFunctionCheck.java](https://github.com/SonarSource/sonar-python/blob/master/python-checks/src/main/java/org/sonar/python/checks/TooManyLinesInFunctionCheck.java) |
| 4 | **SonarSource S138 — Go** (`TooLongFunctionCheck`) | **120** | Rule class exists; default not read by me (see parent-agent note). **Verified by the delegating agent via SonarQube's first-party `/api/rules/show`.** | [TooLongFunctionCheck.java](https://github.com/SonarSource/sonar-go/blob/master/sonar-go-checks/src/main/java/org/sonar/go/checks/TooLongFunctionCheck.java) |
| 5 | **SonarSource S138 — other languages** | Kotlin/Swift/Scala/C/C++ **100**, PHP **150** | Same RSPEC-138; per-language defaults. **Verified by the delegating agent via `/api/rules/show`; not independently read here.** | RSPEC-138 (`/api/rules/show?key=<lang>:S138`) |
| 6 | **ESLint `max-lines-per-function`** | **50** | `defaultOptions: [50]`; counts *every physical line* from `node.loc.start.line` to `node.loc.end.line` inclusive — **blank lines and comments count by default**. `skipBlankLines` (default `false`) skips whitespace-only lines; `skipComments` (default `false`) skips lines whose only token is a comment; `IIFEs` (default `false`) means IIFE bodies are **not** counted (set `true` to include). Message: `"{{name}} has too many lines ({{lineCount}}). Maximum allowed is {{maxLines}}."` | [docs](https://eslint.org/docs/latest/rules/max-lines-per-function) · [source](https://github.com/eslint/eslint/blob/main/lib/rules/max-lines-per-function.js) |
| 7 | **Clippy `too_many_lines`** (Rust, `pedantic`) | **100** | `too_many_lines_threshold(...): u64 = 100` ("The maximum number of lines a function or method can have"). Counts **code lines only**: strips the enclosing braces, trims leading/trailing blank lines, and tracks `//` and `/* */` state so blank and comment-only lines are not counted. Closures are not checked separately (the parent body is). | [conf.rs](https://github.com/rust-lang/rust-clippy/blob/master/clippy_config/src/conf.rs) · [too_many_lines.rs](https://github.com/rust-lang/rust-clippy/blob/master/clippy_lints/src/functions/too_many_lines.rs) · [lint docs](https://rust-lang.github.io/rust-clippy/master/index.html#too_many_lines) |
| 8 | **Checkstyle `MethodLength`** | **150** | `DEFAULT_MAX_LINES = 150`. `countEmpty` default **`true`**: length = `closingBrace.line − openingBrace.line + 1` → the whole block from the line with `{` through the line with `}` inclusive, **including blank lines and comments**. `countEmpty=false`: counts distinct lines that carry at least one non-comment token (text blocks expanded); brace lines included. Tokens: `METHOD_DEF`, `CTOR_DEF`, `COMPACT_CTOR_DEF`. | [docs](https://checkstyle.org/checks/sizes/methodlength.html) · [source](https://github.com/checkstyle/checkstyle/blob/master/src/main/java/com/puppycrawl/tools/checkstyle/checks/sizes/MethodLengthCheck.java) |
| 9 | **Checkstyle `ExecutableStatementCount`** | **30** | `DEFAULT_MAX = 30`; counts *executable statements*, not lines. Tokens: `CTOR_DEF`, `METHOD_DEF`, `INSTANCE_INIT`, `STATIC_INIT`, `COMPACT_CTOR_DEF`, `LAMBDA`. Included only for contrast: it is the "statements" alternative, not a length rule. | [docs](https://checkstyle.org/checks/sizes/executablestatementcount.html) · [source](https://github.com/checkstyle/checkstyle/blob/master/src/main/java/com/puppycrawl/tools/checkstyle/checks/sizes/ExecutableStatementCountCheck.java) |
| 10 | **PMD `ExcessiveMethodLength`** (Java) — **removed** | `minimum` **100.0** | Deprecated 6.53.0, removed in 7.0.0. Score = `node.getEndLine() - node.getBeginLine()` (physical span, no `+1`), i.e. blank lines and comments count. PMD's own deprecation text is the most useful primary evidence in this whole survey — see §5. | [PMD 6.55 docs](https://docs.pmd-code.org/pmd-doc-6.55.0/pmd_rules_java_design.html) · [ExcessiveMethodLengthRule.java](https://github.com/pmd/pmd/blob/pmd_releases/6.55.0/pmd-java/src/main/java/net/sourceforge/pmd/lang/java/rule/design/ExcessiveMethodLengthRule.java) · [ExcessiveLengthRule.java](https://github.com/pmd/pmd/blob/pmd_releases/6.55.0/pmd-java/src/main/java/net/sourceforge/pmd/lang/java/rule/design/ExcessiveLengthRule.java) |
| 11 | **PMD 7 `NcssCount`** (the replacement) | method **60**, class **1500** | `methodReportLevel` default 60. NCSS = "Non-Commenting Source Statements": ignores comments and blank lines and counts **statements**, not lines. This is the modern PMD answer to "big method". | [design.xml](https://github.com/pmd/pmd/blob/master/pmd-java/src/main/resources/category/java/design.xml) · [NcssCountRule.java](https://github.com/pmd/pmd/blob/master/pmd-java/src/main/java/net/sourceforge/pmd/lang/java/rule/design/NcssCountRule.java) |
| 12 | **PMD `NPathComplexity`** (Java) | **200** | Not a length rule. NPath = number of acyclic execution paths (grows multiplicatively/exponentially); docs: "A threshold of 200 is generally considered the point where measures should be taken to reduce complexity and increase readability." Answers the "is PMD's Java rule NPath or lines based?" question: the *length* rule was LoC-based (`ExcessiveMethodLength`), the *replacement* is NCSS-based; NPath is a separate path-count rule. | [design.xml](https://github.com/pmd/pmd/blob/master/pmd-java/src/main/resources/category/java/design.xml) |
| 13 | **lizard `-L`** | **1000** | `DEFAULT_MAX_FUNC_LENGTH = 1000`; `FunctionInfo.length = end_line - start_line + 1` → **physical** line span, blank lines and comments included. Deliberately lenient (≈7–10× every other tool here); the README gives no rationale for the value. lizard also exposes a separate `nloc` metric and wants you to use `-Tnloc=N` for code lines. | [README.rst](https://github.com/terryyin/lizard/blob/master/README.rst) · [lizard.py](https://github.com/terryyin/lizard/blob/master/lizard.py) |
| 14 | **RuboCop `Metrics/MethodLength`** | **10** | `Max: 10`, `CountComments: false` (comment lines not counted) → code lines; `CountAsOne` can fold arrays/hashes/heredocs/method calls to one line. Also carries its own honest warning in `default.yml`. | [config/default.yml](https://github.com/rubocop/rubocop/blob/master/config/default.yml) · [method_length.rb](https://github.com/rubocop/rubocop/blob/master/lib/rubocop/cop/metrics/method_length.rb) |
| 15 | **funlen** (Go linter, in golangci-lint) | **60 lines / 40 statements** | README: "The default limits are 60 lines and 40 statements." `ignore-comments` default false. Rationale given: "The intent for the funlen linter is to fit a function within one screen." | [README.md](https://github.com/ultraware/funlen/blob/master/README.md) |
| 16 | **Checkstyle's bundled configs** | — | `sun_checks.xml` enables `<module name="MethodLength"/>` (so 150); **`google_checks.xml` does not include `MethodLength` at all** — consistent with Google's own "no hard limit" guidance (§3). | [sun_checks.xml](https://github.com/checkstyle/checkstyle/blob/master/src/main/resources/sun_checks.xml) · [google_checks.xml](https://github.com/checkstyle/checkstyle/blob/master/src/main/resources/google_checks.xml) |

**Reading the table as a distribution.** For a unit of *code lines* (closest to qingluan `nloc`):
10 (RuboCop) · 60 (funlen) · 75 (Sonar Java) · 100 (Clippy, Sonar Python/Kotlin/Swift/Scala/C/C++,
PMD 6 LoC minimum) · 120 (Sonar Go) · 200 (Sonar JS/TS). For a unit of *physical lines* (blank +
comments included): 50 (ESLint) · 150 (Checkstyle, lizard's default is 1000). Because qingluan counts
code lines, the **physical-line** thresholds are not directly comparable; a physical-line count is
typically ~15–30 % higher than the code-line count for the same function.

---

## 2. The white paper's position on length

Primary source: G. Ann Campbell, *Cognitive Complexity — a new way of measuring understandability*,
SonarSource, **v1.7, 29 August 2023**, <https://www.sonarsource.com/docs/CognitiveComplexity.pdf>
(22 pages; text extracted from the PDF).

**Finding: the white paper never says "not a size metric", and the word "size" does not occur anywhere
in v1.7.** Full-text search over all 22 extracted pages for `size`, `code size`, `SLOC`, `volume`,
`proportional`, `independent`, `longer` (as "N lines longer") etc. produced exactly one relevant hit —
the LOC-correlation criticism of Cyclomatic Complexity below. The popular paraphrase is therefore
**unverified against this document**; do not attribute it to the white paper without a different
citation.

What the white paper *does* say, and which is the real basis for "cognitive complexity deliberately
does not grow with lines":

- **§Introduction, p. 4** — the only sentence that ties metric quality to lines of code, as a
  *criticism of CC*:

  > "Beyond the class level, it is widely acknowledged that the Cyclomatic Complexity scores of
  > applications correlate to their lines of code totals. In other words, Cyclomatic Complexity is of
  > little use above the method level."

- **§"Ignore shorthand", p. 6** — why method extraction and one-line shorthands are free:

  > "The method structure itself is a prime example. Breaking code into methods allows you to condense
  > multiple statements into a single, evocatively named call, i.e. to 'shorthand' it. Thus, Cognitive
  > Complexity does not increment for methods. Cognitive Complexity also ignores the null-coalescing
  > operators found in many languages, again because they allow short-handing multiple lines of code
  > into one."

- **§"Metrics that are valuable above the method level", p. 10**:

  > "Further, because Cognitive Complexity does not increment for the method structure, aggregate
  > numbers become useful."

- **Appendix B: Specification, p. 16** — the normative enumeration contains **no length/size term at
  all**. Increments are: `if`/`else if`/`else`/ternary, `switch`, `for`/`foreach`, `while`/`do while`,
  `catch`, `goto LABEL`/labelled `break`/`continue`, sequences of binary logical operators, each method
  in a recursion cycle; nesting level is raised by the control-flow structures plus nested
  methods/lambdas; nesting increments apply to `if`/ternary, `switch`, loops, `catch`. Length is not
  mentioned once.

**Correct reading.** Cognitive complexity is defined as a *control-flow* metric by construction: it is
invariant to adding straight-line statements (unless you add a method and thus an increment-free
boundary). That is a *design property* of the metric, not a published argument against length limits.
SonarSource enforces length separately, as RSPEC-138 (§1 rows 1–5) — which is precisely the same
"metric vector, never one composite score" split qingluan already decided on.

---

## 3. Official style guides: exact sentences

| Guide | Number? | Quote | URL |
|---|---|---|---|
| **Google C++ Style Guide**, "Write Short Functions" | Soft, **~40 lines**, explicitly *no hard limit* | "Prefer small and focused functions. / We recognize that long functions are sometimes appropriate, so no hard limit is placed on functions length. If a function exceeds about 40 lines, think about whether it can be broken up without harming the structure of the program." | <https://google.github.io/styleguide/cppguide.html#Write_Short_Functions> |
| **Google Python Style Guide**, §3.18 "Function length" | Soft, **~40 lines**, no hard limit | "Prefer small and focused functions. We recognize that long functions are sometimes appropriate, so no hard limit is placed on function length. If a function exceeds about 40 lines, think about whether it can be broken up without harming the structure of the program." (The rest of the section repeats the C++ rationale verbatim.) | <https://google.github.io/styleguide/pyguide.html#318-function-length> |
| **Google Java Style Guide** | **Nothing** | No function/method-length rule exists in the guide. Its only size rule is formatting: §4.4 Column limit 100 characters. Checkstyle's `google_checks.xml` correspondingly omits `MethodLength` (see §1 row 16). | <https://google.github.io/styleguide/javaguide.html> |
| **Linux kernel `Documentation/process/coding-style.rst`**, §6 "Functions" | Soft: **one or two 80×24 screenfuls** (≈24–48 lines), scaled by complexity | "Functions should be short and sweet, and do just one thing.  They should fit on one or two screenfuls of text (the ISO/ANSI screen size is 80x24, as we all know), and do one thing and do that well." / "The maximum length of a function is inversely proportional to the complexity and indentation level of that function.  So, if you have a conceptually simple function that is just one long (but simple) case-statement, where you have to do lots of small things for a lot of different cases, it's OK to have a longer function." | <https://github.com/torvalds/linux/blob/master/Documentation/process/coding-style.rst> |
| **PEP 8** | **Nothing about function length** | Verified against the source: the only "length" guidance is "Maximum Line Length" (79 characters default, up to 99 for teams that agree). No sentence limits the number of lines per function. | <https://peps.python.org/pep-0008/#maximum-line-length> |
| **C++ Core Guidelines** F.3 | **No number** | "F.3: Keep functions short and simple — Reason: Large functions are hard to read, more likely to contain complex code, and more likely to have variables in larger than minimal scopes. Functions with complex control structures are more likely to be long and more likely to hide logical errors." | <https://isocpp.github.io/CppCoreGuidelines/CppCoreGuidelines#Rf-short> |
| **Power of 10** (Holzmann, IEEE *Computer* 39(6), 2006), Rule 4 | **Hard rule, ~60 lines** — the only first-party standard here with an explicit rationale | "Rule: No function should be longer than what can be printed on a single sheet of paper in a standard reference format with one line per statement and one line per declaration. Typically, this means no more than about 60 lines of code per function. **Rationale:** Each function should be a logical unit in the code that is understandable and verifiable as a unit. It is much harder to understand a logical unit that spans multiple screens on a computer display or multiple pages when printed. Excessively long functions are often a sign of poorly structured code." | Paper: <https://doi.org/10.1109/MC.2006.212>; PDF consulted via the archived copy <https://web.archive.org/web/20180101000000id_/http://spinroot.com/gerard/pdf/P10.pdf> (the live `spinroot.com` URL now returns 403) |

**Hard-enforced vs soft-advice.** The distinction the question asks for:

- **Hard limits enforced by a checker**: ESLint 50 (opt-in rule), Checkstyle 150 (opt-in module), Clippy
  100 (opt-in `pedantic` lint), Sonar S138 (built into the analyzers; per-language default), RuboCop 10,
  funlen 60, Power of 10 Rule 4 (mandatory for JPL flight software, checked in their process).
- **Soft advice with a number**: Google C++/Python "~40 lines, no hard limit"; Linux "one or two
  screenfuls" + inversely proportional to complexity.
- **Silence**: Google Java Style Guide, PEP 8. Neither states any number. The Linux kernel's
  `scripts/checkpatch.pl` contains **no function-length check** either (verified by reading the script),
  so even the kernel's advice is not machine-enforced.

---

## 4. What `nloc` means, in the exact words of the tool vendors

Because qingluan's unit is non-blank non-comment lines, the two most useful definitions are:

- SonarQube metric definitions, "Lines of code (`ncloc`)": *"The number of physical lines that contain
  at least one character which is neither a whitespace nor a tabulation nor part of a comment."*
  <https://docs.sonarsource.com/sonarqube-server/latest/user-guide/code-metrics/metrics-definition/>
- Clippy `too_many_lines` counts code lines with the same effect (blank + comment-only lines dropped),
  and Sonar's own S138 implementations for Java/JS/Python all count code lines, not physical lines.

So the comparison set for qingluan's `nloc` is **10 / 60 / 75 / 100 / 120 / 200** from §1, not the
physical-line set (50 / 150 / 1000). This is the single most important calibration fact in this report.

---

## 5. What counts as *evidence* vs *convention*

### 5.1 Evidence-backed (empirical, peer-reviewed or first-party measurement)

| Source | What it actually establishes | Why it does **not** give a function-length threshold |
|---|---|---|
| Les Hatton, "Reexamining the fault density–component size connection", *IEEE Software* 14(2), 1997 (preprint: "Why is the defect density curve U-shaped with component size?"). PDF: <https://www.leshatton.org/Documents/Ubend_IS697.pdf> | Real fault data (Ada, assembler, Fortran; multiple systems): **fault density vs component size is U-shaped**, with the transition from logarithmic to quadratic fault growth at roughly **200–400 lines** per *component*; very small components are proportionately less reliable too. | Unit is the **component/module**, not the function. Nothing in it licenses a per-function line limit. It does show that "bigger is always worse" is false and that "smaller is always better" is also false. |
| Kyle D. Chin & Reid Holmes, "Put The 'Code' Back In 'Code Comprehension Study'", **ICPC '26** (peer-reviewed). PDF: <https://www.cs.ubc.ca/~rtholmes/papers/icpc_2026_chin.pdf> | 604k real methods across 12+ production projects. Uses exactly qingluan's unit: *"Length: The number of lines of code in a method, excluding blank lines and lines that are entirely comments."* Finds that previously-studied comprehension metrics are **highly correlated with Length** and that Length "may be a confounder of prior results"; also that comprehension findings from small snippets did not generalise. | The paper's groupings — Small ≤15 LOC, Medium 16–38 LOC, Large 39+ LOC — are **descriptive quantiles of their own dataset** ("These limits were chosen to capture equal thirds of the total LOC in our dataset"), not normative thresholds. The paper proposes no limit number. |
| SonarSource's choice of per-language S138 defaults (Java 75, JS 200, Python etc. 100, PHP 150, Go 120) | Shows the vendor's *own* numbers vary by language by nearly 3× for the identical rule and metric. | This is a **policy** observation, not an empirical optimum; it is in fact evidence *against* treating any single number as scientifically fixed. |
| PMD's deprecation analysis of `ExcessiveMethodLength` (PMD 6.53–7.0) | First-party post-mortem: *"The rule is based on the simple metric lines of code (LoC). The reasons for deprecation are: LoC is a noisy metric, NCSS (non-commenting source statements) is a more solid metric (results are code-style independent, comment-insensitive); LoC is easily circumvented by bad code style (e.g. stuffing several assignments into one, concatenating code lines); Enforcing length limits with LoC is not very meaningful, could even be called a bad practice. In order to find 'big' methods, the rule NcssCount can be used instead."* | Directly relevant to qingluan: a major tool vendor concluded that **LoC-based length limits are a weak rule** and replaced them with a statement-count rule (NCSS). Suppresses hype for any specific line threshold. |

### 5.2 Convention (a number exists, but its provenance is choice, not measurement)

| Number | Origin / provenance | Status |
|---|---|---|
| **~40** | Google C++ and Google Python style guides ("no hard limit", "think about whether it can be broken up"). | Soft advice; explicitly not enforced. |
| **60** | Power of 10 Rule 4 (a printed page / one line per statement) and funlen ("fit a function within one screen"). The rationale is human-factors intuition about screens and paper, not a study. | Hard rule in a safety-critical context (JPL); strong provenance, no empirical basis. |
| **75 / 100 / 120 / 150 / 200** | SonarSource per-language S138 defaults. The 100 group is the largest; Java's 75 was lowered from 100 historically; JS/TS' 200 accommodates functional/declarative code. | Pure policy. |
| **100** | Clippy `too_many_lines` default; Sonar's 100 group; PMD 6 `ExcessiveMethodLength` minimum. Clippy's doc gives only the generic "harder to understand" reason. | Convention, but the *modal* one among code-line counters. |
| **10** | RuboCop `Metrics/MethodLength`, and its own configuration file says so: *"Expected to be disabled by default in the next major release. Rejected outright by a third of projects; the ones that keep it disagree on the limit."* | Convention, self-described as contentious. Excellent evidence that thresholds are taste, not fact. |
| **150** | Checkstyle `MethodLength` (physical lines, blank+comments included by default). | Convention. |
| **1000** | lizard `-L` (physical span). The README does not justify it. | Deliberately lenient default; **rationale unverified** (no explanation found in the README or source). |
| **80** | — | **No primary source found.** It is not a default of any rule set surveyed (Sonar per-language, ESLint, Clippy, Checkstyle, PMD, lizard, RuboCop, funlen). Treat "80" as folklore unless someone produces a citation. |

### 5.3 Not verified / open items (do not cite as fact)

- **"Not a size metric" as a white-paper quote** — not present in v1.7; see §2. Marked unverified.
- **SIG / Baggen et al., "Standardized code quality benchmarking for improving software
  maintainability"** (Software Quality Journal, 2012) — a frequently-cited candidate for an
  empirically-derived "15 lines per unit" threshold. Fetch attempts timed out; **unverified**. Do not
  quote a 15-line number from it.
- **lizard's rationale for `-L 1000`** — not documented in the README; unverified.
- **Sonar S138 defaults for Go 120, PHP 150, Kotlin/Swift/Scala/C/C++ 100** — supplied by the
  delegating agent from SonarQube's first-party `/api/rules/show` API; not independently re-read here.
- **SonarJS's `S138.html`/`generated-meta.js`** — the RSPEC description text lives in
  `sonar-java-plugin`/`python-checks` resource trees (read for Java and Python); the JS/TS RSPEC page
  itself is a client-side app (<https://sonarsource.github.io/rspec/#/rspec/S138/javascript>) and was
  read via the analyzer sources instead.

---

## 6. Recommendation for qingluan

Given the spec's "metric vector, no composite score" and configurable thresholds
(`cc_threshold`, `cognitive_threshold`):

1. **Report `nloc` with the same definition everything else uses** — SonarQube `ncloc` / Clippy
   `too_many_lines`: count a line iff it contains at least one token that is not whitespace and not
   part of a comment. Lines with code + trailing comment count once. Comment-only and blank lines
   never count. (This is already the spec's `nloc`.)
2. **Default threshold: flag `nloc > 100`.** Same unit as Clippy's 100 and Sonar's 100 group; matches
   the low bound of PMD's former LoC rule. It will flag genuinely large functions without crying wolf
   on the long-but-flat switch/table functions that qingluan already prefers to sort away from the top
   of the list.
3. **Offer a `strict` preset at 60** — the only hard-rule-with-rationale number in the field
   (Power of 10 Rule 4; funlen's screen-fit). Useful for safety-critical or greenfield repos.
4. **Never sort by `nloc` alone and never combine it with CC/cognitive.** PMD's deprecation note is the
   primary-source argument: LoC-based limits are noisy and gameable, and the interesting signal is the
   *pair* (long + complex). The existing default sort (`cognitive`) plus showing `nloc` in the same row
   already implements the defensible version of this.
5. **Document the choice as a convention.** A one-line note in the CLI help and in the spec — "length
   threshold is policy, not a measurement; default 100 follows Clippy/Sonar ncloc-counting rules" —
   prevents the number from being read as a finding.
