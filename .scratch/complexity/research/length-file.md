# File length as a second size axis: defensible defaults and their provenance

Primary-source research for adding a **file-level length metric** to `qingluan complexity`.

**Research date:** 2026-09-30.
**Scope:** what default threshold (if any) for file length is defensible, and where each candidate
number actually comes from. Complements `docs/research/code-complexity-metrics.md` (function-level
metrics) and `.scratch/complexity/spec.md` (CLI contract). Only short passages are quoted; no upstream
file is copied into the repo.

**Revisions fetched** (raw.githubusercontent.com from `master`/`main` unless noted; SHAs via `git ls-remote`):

| Repo | HEAD SHA | Date |
|---|---|---|
| `SonarSource/sonar-java` | `b7d5579025a93cef62d9a28dd28f9289e3bea466` | 2026-09-30 |
| `SonarSource/SonarJS` | `3f352bb32881589eb34fa00308e19f3c77576f49` | 2026-09-30 |
| `SonarSource/sonar-python` | `fc9ddc99724022c1858a2b8b116097d2474d543c` | 2026-09-30 |
| `SonarSource/sonar-php` | `12c1ff117b0b1642aca7a084b3ebe7d79087eeaf` | 2026-09-30 |
| `SonarSource/sonar-go` | `5451f51dfb864c46a7b7e099eb452d25ee79ea06` | 2026-09-30 |
| `SonarSource/sonar-dotnet` | `d3974c1c45a42302500fd4fff09765a3e830deea` | 2026-09-30 |
| `eslint/eslint` | `d166567901e09940c1de4ad0e95d572eeab5c927` | 2026-09-30 |
| `checkstyle/checkstyle` | `0573dedd580e357c16e51b46a036547fc6caf305` | 2026-09-30 |
| `pylint-dev/pylint` | `1cafd4d2cc42a0cd9569862be6e7c193fc9f5bb7` | 2026-09-30 |
| `realm/SwiftLint` | `ec4691d9e813a1d3358e82917326b56b0a71d3c9` | 2026-09-30 |
| `mgechev/revive` | `7ff27d13643f674bc27ef4343a76a3730994144c` | 2026-09-30 |
| `terryyin/lizard` | `dc11c278bf44500464703fd0b5313fc6eb292983` | 2026-09-30 |
| `torvalds/linux` | `551c722f40809618230001baccf219193e22fc5a` | 2026-09-30 |
| `google/styleguide` | `403f0581bd93732111cc8f84f92b07ce1ed50d82` | 2026-09-30 |
| `python/peps` | `50803e5f0aa404f34093c16d13a9bd7d21d904d1` | 2026-09-30 |

## TL;DR — verdict

1. **There is no evidence-backed file-length threshold.** Every number in circulation (300, 400, 750,
   1000, 2000) is a **tool convention**, not a result. Worse, the peer-reviewed literature does not
   even agree on the *direction* of the file-size → defect-density relationship: monotone-decreasing
   (Koru 2009), U-shaped (Hatton 1997 — **retracted by its author in 2009**), inverted-U (Syer et al.
   2015), and no relationship at all (Fenton & Ohlsson 2000, a strong null). The only number with any
   empirical origin is Hatton's 200–400 LOC, and it is disowned. Tools, meanwhile, concede the point:
   ESLint's docs say there is "not an objective maximum", and PMD deprecated its LoC length rule saying
   such limits "could even be called a bad practice".
2. **The evidence that does exist is directional, not numeric.** Longer files cost more to maintain —
   Sjøberg et al. (2013) measured maintenance effort in seconds and found file LOC correlates with it
   (ρ = 0.37–0.61) and explains most of the modelled variation. That argues for **surfacing** file
   length as a metric, not for gating on a constant. Patch/change size has the *better* evidence base
   (more review latency, lower comment usefulness, more missed bugs) but the effects are
   small-to-moderate with real nulls, and it is a ***different axis*** from file size ("100 lines is
   usually a reasonable size for a CL"); evidence does not transfer between them. The famous
   "review under 200/400 lines" rule is **not peer-reviewed** — it is a 2006 vendor whitepaper that
   contradicts itself. And no clean peer-reviewed study of *file length → review difficulty* was found
   at all: "files over N lines are harder to review" is not established.
3. **"Sonar = 750" is wrong as a blanket statement.** Via analyzer source + the first-party SonarCloud
   rules API: **Java and Go default to 750**, while **JS/TS, Python, PHP, Kotlin, Swift, Scala, C, C++,
   C#, VB.NET default to 1000**. Sonar's own per-language counting semantics also disagree.
4. **Recommendation:** add file `nloc` (non-blank, non-comment lines) to the per-file output as an
   orthogonal axis, and surface it as **distribution + "longest files" top-K ranked list** — the same
   shape the function-level CLI already uses. Do **not** default to a hard threshold in a one-shot
   repo health check. If `--threshold` is offered, default it to **1000 nloc** and label it explicitly
   as a convention (the modal value across modern tools), not as evidence; never 300 for a repo-wide
   scan.

---

## 1. SonarSource S104

### 1.1 Rule identity and description

Current RSPEC title, identical in every analyzer's packaged rule metadata:
**"Files should not have too many lines of code"** (note: *of code*, not the commonly quoted
"Files should not have too many lines").
Sources: `sonar-java` `S104.json`, `sonar-python` `S104.json`, `sonar-php` `S104.json`,
`sonar-dotnet` `rspec/cs/S104.json`.

The `S104.html` bodies shipped by `sonar-java`, `sonar-python`, `sonar-php`, `sonar-go`,
`sonar-dotnet` are byte-identical:

> When a source file grows too much, it can accumulate numerous responsibilities and become
> challenging to understand and maintain.
>
> Above a specific threshold, refactor the file into smaller files whose code focuses on
> well-defined tasks. Those smaller files will be easier to understand and test.

The rule source carries the canonical RSPEC URL:
`https://sonarsource.github.io/rspec/#/rspec/S104/java` (and `.../javascript`).

**Access note (unverified why):** `https://github.com/SonarSource/rspec` returns HTTP 404 and
`https://sonarsource.github.io/rspec/` 301-redirects to a GitHub Pages login page, so the RSPEC SPA
itself could not be read in this environment. The quoted text and defaults below come from the
analyzer repositories' packaged rule resources and from the first-party SonarCloud rules API
(`/api/rules/show`), which are the sources the RSPEC page is generated from.

### 1.2 Defaults and counting semantics per language (verified)

| Language | Implementation | Parameter key | Default | What it counts |
|---|---|---|---|---|
| Java | `TooManyLinesOfCodeInFileCheck` | `Max` | **750** | distinct lines holding a non-EOF syntax token = ncloc-like (blanks + comment-only lines excluded) |
| Go | `TooManyLinesOfCodeFileCheck` | `Max` | **750** | `tree.metaData().linesOfCode().size()` = LOC set |
| JS/TS | `S104/rule.ts` + `config.ts` | `maximum` | **1000** | lines that are neither blank nor full-line comments (ncloc-like) |
| Python | `TooManyLinesInFileCheck` | `maximum` | **1000** | **line number of the file's last token** — i.e. physical lines, except blank/comment lines *after* the last token |
| PHP | `TooManyLinesInFileCheck` | `max` | **1000** | `LineVisitor.linesOfCode(tree)` = LOC |
| C# / VB.NET | `FileLinesBase` | `maximumFileLocThreshold` | **1000** | distinct token line numbers = LOC |
| Kotlin | (via rules API) | `max` | **1000** | not checked at source level |
| Swift | (via rules API) | `Max` | **1000** | not checked at source level |
| Scala | (via rules API) | `Max` | **1000** | not checked at source level |
| C / C++ | (via rules API) | `maximumFileLocThreshold` | **1000** | not checked at source level |

Evidence for the Java default and semantics:

```java
private static final int DEFAULT_MAXIMUM = 750;
@RuleProperty(key = "Max", description = "Maximum authorized lines in a file.",
              defaultValue = "" + DEFAULT_MAXIMUM)
...
int lines = metricsComputer.getLinesOfCode(tree);
if (lines > maximum) { addIssueOnFile(...); }
```

`LinesOfCodeVisitor` visits `TOKEN` nodes and adds `Position.startOf(syntaxToken).line()` to a
`HashSet` for every non-EOF token, so blank and comment-only lines never enter the count.

Evidence for the JS default: `config.ts` declares
`{ field: 'maximum', description: 'Maximum authorized lines in a file', default: 1000 }`, and the
rule (`DEFAULT = 1000`, `eslintId = 'max-lines'`) computes the count with `getLocsNumber`, which
skips `/^\s*$/` lines and full-line comments.

Evidence for Python (different semantics — worth reading twice):

```java
private static final int DEFAULT = 1000;
@RuleProperty(key = "maximum", defaultValue = "" + DEFAULT)
...
int line = ctx.syntaxNode().lastToken().line();
if (line > maximum) { ctx.addFileIssue(...); }
```

This is a **physical-line** measure (the last token's line number), so a 1200-line file with 400
comment lines still counts ~1200, unlike Java/JS.

URLs:
- https://raw.githubusercontent.com/SonarSource/sonar-java/master/java-checks/src/main/java/org/sonar/java/checks/TooManyLinesOfCodeInFileCheck.java
- https://raw.githubusercontent.com/SonarSource/sonar-java/master/java-frontend/src/main/java/org/sonar/java/ast/visitors/LinesOfCodeVisitor.java
- https://raw.githubusercontent.com/SonarSource/SonarJS/master/packages/analysis/src/jsts/rules/S104/rule.ts
- https://raw.githubusercontent.com/SonarSource/SonarJS/master/packages/analysis/src/jsts/rules/S104/config.ts
- https://raw.githubusercontent.com/SonarSource/SonarJS/master/packages/analysis/src/jsts/rules/S138/rule.ts (`getLocsNumber`)
- https://raw.githubusercontent.com/SonarSource/sonar-python/master/python-checks/src/main/java/org/sonar/python/checks/TooManyLinesInFileCheck.java
- https://raw.githubusercontent.com/SonarSource/sonar-php/master/php-checks/src/main/java/org/sonar/php/checks/TooManyLinesInFileCheck.java
- https://raw.githubusercontent.com/SonarSource/sonar-go/master/sonar-go-checks/src/main/java/org/sonar/go/checks/TooManyLinesOfCodeFileCheck.java
- https://raw.githubusercontent.com/SonarSource/sonar-dotnet/master/analyzers/src/SonarAnalyzer.Core/Rules/FileLinesBase.cs
- First-party rules API (cross-check, per-language params): `https://sonarcloud.io/api/rules/show?key=<lang>:S104&organization=sonarsource`

### 1.3 SonarQube metric definitions: `lines` vs `ncloc` vs `comment_lines`

From the official Metric Definitions page (quoted in full, they are one sentence each):

| Metric | Key | Definition |
|---|---|---|
| Lines | `lines` | "The number of physical lines (number of carriage returns)." |
| Lines of code | `ncloc` | "The number of physical lines that contain at least one character which is neither a whitespace nor a tabulation nor part of a comment." |
| Comment lines | `comment_lines` | "The number of lines containing either comment or commented-out code." |

Source: https://docs.sonarsource.com/sonarqube-server/latest/user-guide/code-metrics/metrics-definition/

So "file length" is ambiguous even inside one vendor: `lines` (physical) and `ncloc` (code) differ by
the blank+comment population, and S104 itself follows different ones in different languages.

---

## 2. ESLint `max-lines`

- **Default: 300 lines** — `"max" (default 300) enforces a maximum number of lines in a file.`
- Options: `skipBlankLines` and `skipComments` (both **off** by default; source: `defaultOptions: [300]`
  and `option && option.skipComments` / `skipBlankLines` fall through to falsy).
- The rule is **not** in any shared config (`recommended: false`) — 300 is only the value you get if
  you enable the rule and pass nothing.
- It counts `sourceCode.lines`, minus one trailing empty line if the file ends with a line break
  ("This rule does not count that extra line."). `skipComments` ignores lines containing *just*
  comments; a trailing comment on a code line is still counted.
- Reported as a single error per file, located from the first over-limit line to EOF.

The docs themselves refuse to bless a number:

> While there is not an objective maximum number of lines considered acceptable in a file, most people
> would agree it should not be in the thousands. Recommendations usually range from 100 to 500 lines.

Source: https://eslint.org/docs/latest/rules/max-lines ·
source code: https://github.com/eslint/eslint/blob/main/lib/rules/max-lines.js

For contrast, the sibling `max-lines-per-function` defaults to **50** (`defaultOptions: [50]`).

---

## 3. Checkstyle `FileLength`

- **Default `max`: 2000.** Only other property is `fileExtensions` (default: all files).
- **There is no `countEmpty` property on `FileLength`** — not in the current source, not in the 8.0
  source, not in the 6.18 xdocs. `countEmpty` (default **true**, max 150) belongs to
  **`MethodLength`**, a different check. The premise "does FileLength's `countEmpty` default to true?"
  is therefore false: the option does not exist; MethodLength is the check that counts empty lines and
  comments by default.
- It counts **all physical lines including blanks and comments**: the implementation is
  `if (fileText.size() > max)`.
- Rationale (verbatim): "Rationale: If a source file becomes very long it is hard to understand.
  Therefore, long classes should usually be refactored into several individual classes that focus on a
  specific task."

Sources:
- https://checkstyle.org/checks/sizes/filelength.html
- https://raw.githubusercontent.com/checkstyle/checkstyle/master/src/main/java/com/puppycrawl/tools/checkstyle/checks/sizes/FileLengthCheck.java
- https://raw.githubusercontent.com/checkstyle/checkstyle/checkstyle-8.0/src/main/java/com/puppycrawl/tools/checkstyle/checks/sizes/FileLengthCheck.java
- https://raw.githubusercontent.com/checkstyle/checkstyle/checkstyle-6.18/src/xdocs/config_sizes.xml
- https://checkstyle.org/checks/sizes/methodlength.html

---

## 4. PMD

- **PMD has no file-length rule.** It only ever had *class*/*method* length rules, and the class-length
  one is gone.
- PMD 6 `ExcessiveClassLength`: default `minimum` = **1000.0** lines of code, operating on classes, not
  files. Deprecated since **6.53.0** and removed with **PMD 7.0.0**. The deprecation note is the single
  most on-point primary-source statement found in this whole investigation:

  > The rule is based on the simple metric lines of code (LoC). The reasons for deprecation are:
  > - LoC is a noisy metric, NCSS (non-commenting source statements) is a more solid metric
  > - LoC is easily circumvented by bad code style (e.g. stuffing several assignments into one,
  >   concatenating code lines)
  > - Enforcing length limits with LoC is not very meaningful, could be called a bad practice
  >
  > In order to find "big" classes, the rule NcssCount can be used instead.

- PMD 7's replacement is `NcssCount`, still scoped to methods and classes
  (`methodReportLevel` = 60 NCSS, `classReportLevel` = 1500 NCSS), **not files**.
- PMD's `CommentSize` limits comment blocks (not files).

Sources:
- https://docs.pmd-code.org/pmd-doc-6.55.0/pmd_rules_java_design.html (`ExcessiveClassLength`)
- https://docs.pmd-code.org/latest/pmd_rules_java_design.html (`NcssCount`, and the absence of any
  `*FileLength*` / `ExcessiveFileLength` rule in the Java design category)

*Unverified:* whether any non-Java PMD language ships a file-length rule. The Java rule set — by far
the largest — does not.

---

## 5. Style guides: what do they actually say?

Finding up front: **no major language style guide sets a file-length limit.** They regulate *function*
length (as advice) and *line* width (as a limit), and are silent on file size.

### Google C++ Style Guide

> Prefer small and focused functions.
>
> We recognize that long functions are sometimes appropriate, so no hard limit is placed on functions
> length. If a function exceeds about 40 lines, think about whether it can be broken up without harming
> the structure of the program.

Also: "Only define a function at its public declaration if it is short, say, 10 lines or fewer."
No file-length rule anywhere in the guide.
URL: https://google.github.io/styleguide/cppguide.html

### Google Python Style Guide — "3.18 Function length"

> We recognize that long functions are sometimes appropriate, so no hard limit is placed on function
> length. If a function exceeds about 40 lines, think about whether it can be broken up without harming
> the structure of the program.

No file-length rule. URL: https://google.github.io/styleguide/pyguide.html

### Google Java Style Guide

No file-length or function-length limit. The nearest structural rule is §3.4.1
"Exactly one top-level class declaration" — "Each top-level class resides in a source file of its own."
URL: https://google.github.io/styleguide/javaguide.html

### Linux kernel `coding-style.rst` — chapter 6 "Functions"

> Functions should be short and sweet, and do just one thing. They should fit on one or two screenfuls
> of text (the ISO/ANSI screen size is 80x24, as we all know), and do one thing and do that well.
>
> The maximum length of a function is inversely proportional to the complexity and indentation level of
> that function. So, if you have a conceptually simple function that is just one long (but simple)
> case-statement, [...] it's OK to have a longer function.

No file-length statement. URL:
https://raw.githubusercontent.com/torvalds/linux/master/Documentation/process/coding-style.rst

### PEP 8

No file-length statement. Its only size rule is line width:

> Limit all lines to a maximum of 79 characters.

with docstrings/comments at 72 and an allowance up to 99 for teams that agree.
URL: https://peps.python.org/pep-0008/#maximum-line-length

### Google engineering practices — **change size**, not file size

> There are no hard and fast rules about how large is "too large." 100 lines is usually a reasonable
> size for a CL, and 1000 lines is usually too large, but it's up to the judgment of your reviewer. The
> number of files that a change is spread across also affects its "size." A 200-line change in one file
> might be okay, but spread across 50 files it would usually be too large.

This is the origin of the widely repeated "100 / 1000" numbers, and it is explicitly about **CLs
(patches)**, not files. URL:
https://google.github.io/eng-practices/review/developer/small-cls.html

---

## 6. Cross-tool default table — is there a de facto standard?

| Tool / rule | Default | What it counts | Config notes | URL |
|---|---|---|---|---|
| ESLint `max-lines` | **300** | file lines, minus a trailing empty line | `skipBlankLines`/`skipComments` default false; rule off by default | https://eslint.org/docs/latest/rules/max-lines |
| SonarQube/SonarCloud S104 — Java | **750** | token-bearing lines (ncloc-like) | param `Max` | see §1 |
| SonarQube/SonarCloud S104 — Go | **750** | LOC set | param `Max` | see §1 |
| SonarQube/SonarCloud S104 — JS/TS | **1000** | non-blank, non-comment-only lines | param `maximum` | see §1 |
| SonarQube/SonarCloud S104 — Python | **1000** | physical line number of last token | param `maximum` | see §1 |
| SonarQube/SonarCloud S104 — PHP | **1000** | `linesOfCode` (ncloc) | param `max` | see §1 |
| SonarQube/SonarCloud S104 — C#/VB.NET | **1000** | distinct token lines | param `maximumFileLocThreshold` | see §1 |
| SonarQube/SonarCloud S104 — Kotlin/Swift/Scala/C/C++ | **1000** | not checked at source level | params `max` / `Max` / `maximumFileLocThreshold` | SonarCloud rules API |
| Checkstyle `FileLength` | **2000** | **all physical lines** (blanks + comments included) | no `countEmpty` option exists | https://checkstyle.org/checks/sizes/filelength.html |
| pylint `too-many-lines` (C0302) | **1000** | physical lines from `tokenize` (`line_num -= 1  # to be ok with wc -l`) | option `max-module-lines` | https://github.com/pylint-dev/pylint/blob/main/pylint/checkers/format.py |
| SwiftLint `file_length` | **warning 400 / error 1000** | lines; `ignore_comment_only_lines` default false | `@ConfigurationElement(... warning: 400, error: 1000)` | https://github.com/realm/SwiftLint/blob/main/Source/SwiftLintBuiltInRules/Rules/RuleConfigurations/FileLengthConfiguration.swift |
| revive `file-length-limit` | **0 = disabled** | lines | `skip-comments`/`skip-blank-lines` default false | https://github.com/mgechev/revive/blob/master/RULES_DESCRIPTIONS.md |
| PMD | **none** | — | `ExcessiveClassLength` (1000 LoC) deprecated 6.53.0, removed in PMD 7 | https://docs.pmd-code.org/pmd-doc-6.55.0/pmd_rules_java_design.html |
| lizard | **no file-length threshold** | — | `-L`/`--length` default 1000 is **function** length; per-file table is diagnostic only | https://github.com/terryyin/lizard/blob/master/README.rst |
| Checkstyle `MethodLength` (contrast) | 150 | lines, `countEmpty` default **true** | not a file rule | https://checkstyle.org/checks/sizes/methodlength.html |

**Verdict on "de facto standard": there isn't one.** The values cluster but do not converge:

- **300** — one influential linter's unconfigured default, and that linter's own docs say the real
  range people use is 100–500. Repo-wide, 300 would flag a very large fraction of any mature codebase.
- **750** — Sonar's JVM (Java) and Go analyzers only.
- **1000** — the mode: most Sonar languages, pylint, SwiftLint's *error* tier, PMD's old class-length
  threshold, lizard's function-length threshold. This is the closest thing to a convention.
- **2000** — Checkstyle, and it counts comment and blank lines, making it much weaker per unit number.
- **400** — SwiftLint's warning tier (paired with 1000 error).
- **disabled** — revive's default (0), i.e. the tool refuses to choose.

No source found states *why* any of these specific numbers was chosen (ESLint 300, Checkstyle 2000,
Sonar 750/1000 all lack a published rationale). They are round numbers, not measurements.

---

## 7. Empirical evidence: file size vs change size

### 7.1 The distinction, stated explicitly

**File/module size** = lines of code in a source file, class, or module. Outcomes studied: defect
count, defect **density** (defects per KLOC), fault-proneness, maintenance effort.

**Change/patch/CL size** = lines added + removed in a diff, commit, changeset, or CL. Outcomes
studied: review defect-detection, review usefulness, review latency.

These are different units measured over different timescales and are **not interchangeable**. A large
file usually yields small patches spread over time; a small file can receive a huge patch. Evidence
about one does **not** transfer to the other. This matters because the widely repeated "reviewers
can't handle more than ~200–400 lines" rule is a *patch*-size claim, while "this file is 4000 lines"
is a *file*-size claim.

### 7.2 File/module size — what the peer-reviewed literature actually says

| Study | Venue | Finding relevant here | Verification |
|---|---|---|---|
| Basili & Perricone, "Software errors and complexity" | CACM 27(1), 1984, DOI `10.1145/69605.2085` | Larger modules had **lower** fault density; the origin of the debate | **SECONDARY** (via Hatton's citation) |
| Hatton, "Re-examining the fault density–component size connection" | IEEE Software 14(2), 1997, DOI `10.1109/52.582978` | U-shaped fault density vs component size, minimum at **200–400 lines** | full text; **retracted by the author in 2009** |
| Fenton & Ohlsson, "Quantitative Analysis of Faults and Failures in a Complex Software System" | IEEE TSE 26(8), 2000, DOI `10.1109/32.879815` | **Strong null** on "big modules hold most faults because they are most of the code"; LOC predicts fault *counts* and *ranks* modules, but not *density* | full text (quotes via the literature sub-agent; no PDF tooling in this sandbox) |
| Koru, Zhang, El Emam, Liu, "An Investigation into the Functional Form of the Size-Defect Relationship for Software Modules" | IEEE TSE 35(2), 2009, DOI `10.1109/TSE.2008.90` | Power-law, **monotone**: larger modules proportionally *less* defect-prone | abstract verbatim; full text paywalled |
| Syer, Nagappan, Adams, Hassan, "Replicating and Re-Evaluating the Theory of Relative Defect-Proneness" | IEEE TSE 41(2), 2015, DOI `10.1109/TSE.2014.2361131` | **Replication fails to generalize**; own curve is an **inverted U** | full text |
| Sjøberg, Yamashita, Anda, Mockus, Dybå, "Quantifying the Effect of Code Smells on Maintenance Effort" | IEEE TSE 39(8), 2013, DOI `10.1109/TSE.2012.89` | File LOC correlates with **measured** maintenance effort (ρ = 0.37–0.61, p < 0.01); size + number of changes explain almost all variation | full text |

Quotes and numbers behind the table:

- **Hatton 1997** (full text): "logarithmic behaviour up to the capacity of the short-term memory (in
  the region of 200-400 lines and apparently independent of language)" and "quadratic behaviour for
  component sizes of larger than this cut-off value"; "The most reliable systems may result from
  systems with component sizes grouped around the 200-400 line mark." Worked example: "it is far
  better to implement it as 5 x 200 line components [...] rather than 50 x 20 line components".
  **Retraction (verified verbatim on the author's own page, September 2009):** "I no longer believe in
  the U-bend described in this paper. I used data available at the time but meanwhile 10 years later, I
  have a lot more higher quality data and a different and more convincing model to explain the nature
  of the defect curve." The single most-cited empirical justification for a 200–400-line default is
  disowned by its author. URL: https://www.leshatton.org/IEEE_Soft_97b.html
- **Fenton & Ohlsson 2000** (full text): "Hypotheses 1a and 2a are strongly supported, while 1b and 2b
  are strongly rejected" (1b/2b are the size-explains-and-the-large-modules-are-just-big hypotheses);
  "It is not the case that size explains in any significant way the number of faults." The paper's own
  asymmetry is the honest summary: size is "a reasonable predictor of number of faults (although not of
  fault density)" and "LOC is quite good at ranking the most fault-prone modules." They also warn that
  earlier work "analysed the relation by grouping modules according to size. As illustrated above this
  can be very misleading" — size-bin grouping is exactly what manufactures a U-shape.
- **Koru et al. 2009** (verbatim abstract; full text paywalled): "Our results consistently revealed a
  significant effect of size on defect proneness; however, contrary to common intuition, the
  size-defect relationship took a logarithmic form, indicating that smaller classes were proportionally
  more problematic than larger classes." Their policy number: "an inspection strategy investing 80% of
  available resources on 100-LOC classes and the rest on 1,000-LOC classes would be more than twice as
  cost effective as the opposite strategy" (Mozilla, Cn3d, JBoss, Eclipse). Via Syer et al. for the
  Mozilla hazard estimate: "a one unit increase in the natural logarithm of size led to a 44% increase
  in the rate of defect fixes." Zhang (ICSM 2009, DOI `10.1109/ICSM.2009.5306304`) separately confirms
  and models LOC's *ranking* ability with Weibull functions (Eclipse + NASA).
- **Syer et al. 2015** (full text): "In general, our results do not indicate a well supported,
  consistent relationship between size and defects. We find that the conclusions of Koru et al. are
  not generalizable." Their curve is an inverted U: "defect density increases in smaller files, peaks
  in the largest small-sized files/smallest medium-sized files, then decreases in medium and larger
  files." They also record the methodological objection that "defect density is artificially high in
  smaller modules because the denominator (i.e., size) is small", making the density framing itself
  suspect.
- **Sjøberg et al. 2013** (full text, 298 Java files, effort instrumented in seconds): "None of the 12
  investigated smells was significantly associated with increased effort after we adjusted for file
  size and the number of changes; [...] File size and the number of changes explained almost all of the
  modeled variation in effort." Correlation of file LOC with effort: System A ρ = 0.37, B = 0.61,
  C = 0.58, D = 0.48 (all p < 0.01). Caveat: this is **effort**, not defects — effort ≠ correctness risk.

### 7.3 Can file size be separated from internal complexity?

No study found cleanly isolates the two, and the literature explains why: they are collinear.
Fenton & Ohlsson note "there is a good linear correlation between cyclomatic complexity and LOC."
The defensible reading is therefore:

- Raw file LOC predicts defect **counts** and maintenance **effort** well.
- Whether it predicts defect **density**, and in which direction, is **genuinely unresolved** across
  four mutually incompatible findings: monotone-decreasing (Koru), U-shaped (Hatton, retracted),
  inverted-U (Syer), and no relationship at all (Fenton & Ohlsson).
- Adding complexity metrics on top of size buys little (Fenton & Ohlsson; Sjøberg et al.).

### 7.4 Is file size "merely a proxy" for complexity?

This is the literature's live dispute, and it cuts both ways.

- **El Emam, Benlarbi, Goel, Rai, "The Confounding Effect of Class Size on the Validity of
  Object-Oriented Metrics"** (IEEE TSE 27(7), 2001, DOI `10.1109/32.935855`): "none of these studies
  allow for the potentially confounding effect of class size. We demonstrate a strong size confounding
  effect and question the results of previous object-oriented metrics validation studies." *Secondary:*
  the often-repeated "associations disappear after controlling for size" could not be verified (the
  paper is paywalled; only the abstract was read).
- **Evanco, "Comments on 'The confounding effect of class size...'"** (IEEE TSE 29(5), 2003,
  DOI `10.1109/TSE.2003.1214331`) pushes back: "the ability to measure size does not temporally
  precede the ability to measure many of the object-oriented metrics [...] the condition that a
  confounding variable must occur causally prior to another explanatory variable is not met."
- **Zhou, Leung, Xu** (IEEE TSE 35(5), 2009, DOI `10.1109/TSE.2009.32`) and **Zhou, Xu, Leung, Chen**
  (ACM TOSEM 23(1), 2014, DOI `10.1145/2556777`): size confounding overestimates OO-metric
  associations with change-proneness and fault-proneness, and "after removing the confounding effect,
  the prediction performance of fault prediction models [...] can in general be significantly
  improved."
- **Graves, Karr, Marron, Siy** (IEEE TSE 26(7), 2000, DOI `10.1109/32.859533`) go further: "process
  measures based on the change history are more useful in predicting fault rates than product metrics
  of the code: For instance, the number of times code has been changed is a better indication of how
  many faults it will contain than is its length."
- **Nagappan & Ball** (ICSE 2005, DOI `10.1145/1062455.1062514`) confirm the predictor is *relative*
  churn, not raw size: "while absolute measures of code churn are poor predictors of defect density,
  our set of relative measures of code churn is highly predictive of defect density." File size enters
  only as a normalizer.
- **Bird et al.** (ESEC/FSE 2011, DOI `10.1145/2025113.2025119`) show ownership/organizational factors
  add substantial variance on top of size + complexity + churn.
- **Yang et al.** (IEEE TSE 41(4), 2015, DOI `10.1109/TSE.2014.2370048`) find slice-based cohesion
  metrics "in general do not outperform the baseline metrics" where the baseline includes size,
  structural complexity, Halstead, and churn — i.e. size is a strong baseline.

**Reading:** size and internal complexity are collinear, so "size independent of complexity" is largely
unanswerable as posed; the defensible claims are that raw LOC is a reasonable predictor of fault
*counts* and a good *ranking* signal (Fenton & Ohlsson), and that raw file LOC is a strong predictor of
maintenance *effort* (Sjøberg et al.) — while change history and ownership often beat it.

### 7.5 Change/patch size → review outcomes

Much better supported than file size — the direction is consistent, but the effects are
small-to-moderate and metric-dependent, and real null results exist.

| Study | Venue | Finding | Verification |
|---|---|---|---|
| Rigby & Bird, "Convergent Contemporary Software Peer Review Practices" | ESEC/FSE 2013, DOI `10.1145/2491411.2491444` | Median change size 11–32 lines (OSS), 44 (Android/AMD), 263 (Lucent); median 2 reviewers; latency ~hours | full text |
| Sadowski et al., "Modern Code Review: A Case Study at Google" | ICSE-SEIP 2018, DOI `10.1145/3183519.3183525` | Median 24 lines modified; >10% single-line; >35% single-file, ~90% <10 files; feedback <1 h small vs ~5 h very large | full text |
| Bosu, Greiler, Bird, "Characteristics of Useful Code Reviews" | MSR 2015, DOI `10.1109/MSR.2015.21` | More **files** in a change → lower proportion of valuable comments | full text (size metric = file count, not lines) |
| Kononenko et al., "Investigating Code Review Quality" | ICSME 2015, DOI `10.1109/ICSM.2015.7332457` | "the larger the code changes, the easier it is for reviewers to miss bugs" (coef. ~0.10***; adjusted R² only ≈0.12–0.17) | full text |
| Kononenko et al., "Code Review Quality: How Developers See It" | ICSE 2016, DOI `10.1145/2884781.2884840` | Developers rank size as the #1 factor for review time; "long patches are hard to review — attention wanes" | full text |
| Baysal et al., "The Influence of Non-technical Factors on Code Review" | WCRE 2013 / EMSE 2015, DOI `10.1007/s10664-015-9366-8` | **Weak/null:** patch size ↔ review time r = 0.09 (accepted) / 0.05 (rejected); positivity effect not significant | full text |
| di Biase et al., "The effects of change decomposition on code review" | PeerJ CS 2018, DOI `10.7717/peerj-cs.193` | **Null:** decomposing changes did not increase the number of found defects in a controlled experiment | full text (preprint arXiv:1805.10978) |
| Doğan & Tüzün, "Towards a taxonomy of code review smells" | IST 2022, DOI `10.1016/j.infsof.2021.106737` | Empirically elicited threshold: ">500 changed LOC" (28/32 respondents); ping-pong rises XS→L then drops at XL (non-monotonic) | full text |
| Kemerer & Paulk, "The Impact of Design and Code Reviews on Software Quality" | IEEE TSE 35(4), 2009, DOI `10.1109/TSE.2009.27` | "The recommended review rate of 200 LOC/hour or less was found to be an effective rate" — a **rate**, not a size | abstract |

Key non-monotonic and confounding notes: Google's comment count peaks at ~1250 lines and falls for
larger changes (auto-generated code/deletions); Thongtanunam & Hassan (IEEE TSE 48(1), 2021,
DOI `10.1109/TSE.2020.2964660`) find patch characteristics are among the *confounders* of
review-outcome models "not as strong as the confounding factors (i.e., patch characteristics and
overall reviewing activities)".

### 7.6 Provenance of the "review under 200/400 lines" rule

**It is not peer-reviewed.** It comes from Cohen, Brown, DuRette, Teleki, *Best Kept Secrets of Peer
Code Review*, Smart Bear, Inc., 2006 — a vendor book plus vendor case study by the maker of the review
tool used in the study (observational; 2,500 reviews, 3.2M LOC, 50 developers, 10 months; no DOI, no
peer review, no peer-reviewed replication). Verbatim: "LOC under review should be under 200, not to
exceed 400. Anything larger overwhelms reviewers and defects are not uncovered."

It also contradicts itself on the adjacent metric: it reports that "review size does not affect the
defect rate [...] 94% of all reviews had a defect rate under 20 defects per hour regardless of review
size" (its Figure 22 is captioned "Defect rate is not influenced by the size of the review"), the
distinction being defect *density* (per kLOC, falls with size) vs defect *rate* (per hour, flat). The
peer-reviewed but **different** norm is ≤200 LOC/**hour** (Kemerer & Paulk, above). Treat the
"200/400" number as folklore with a vendor origin.

### 7.7 Bottom line on the evidence

- **File/module size:** no study establishes that file size *itself*, independent of internal
  complexity, predicts defects. The direction of the file-size ↔ defect-density relation is
  unresolved across four mutually incompatible findings, and the only threshold-like number (Hatton's
  200–400 LOC) was retracted by its author. The one genuinely strong file-size effect is on
  **maintenance effort** (Sjøberg et al., measured in seconds: ρ = 0.37–0.61, and size + number of
  changes "explained almost all of the modeled variation in effort").
- **Patch/change size:** consistently signed but small-to-moderate, with real nulls (Baysal; di Biase)
  and a well-known folklore rule that is not peer-reviewed.
- **Do not transfer between axes.** Patch size has the better evidence base and the more defensible
  mechanism (reviewer capacity per review event); file size has contradictory defect evidence and
  strong evidence only for effort. A patch-size finding cannot justify a file-size threshold, and a
  navigation argument for file size cannot borrow patch-size numbers.
- **We could find no clean peer-reviewed study of file length → review/navigation difficulty.** Any
  claim of the form "files over N lines are harder to review" is, as of this report, **not
  established**.

### 7.8 What this means for a threshold

**None of this yields a file-length threshold.** The only number with an empirical origin is Hatton's
200–400 LOC, and (a) its author retracted it, (b) later work contradicts its direction, and (c) it was
about *component* size under a short-term-memory model, not about review navigability. Meanwhile the
"LoC is noisy" critique (PMD, §4) and ESLint's own "no objective maximum" (§2) point the other way. A
file-length default therefore remains **convention**, not evidence.

The genuine evidence-backed size statement available to qingluan is the weak form: *longer files cost
more to maintain* (Sjøberg et al.), which supports **surfacing** file length as a metric — not gating
on a specific number.

---

## 8. Threshold vs ranked list: what existing tools actually surface

| Tool | Surface | Shape |
|---|---|---|
| SonarQube / SonarCloud S104 | one **file-level issue** (`addIssueOnFile` / `addFileIssue`) per offending file | pass/fail per file; plus size metrics and treemaps for browsing |
| ESLint `max-lines` | one **error at `Program:exit`**, located from the first over-limit line to EOF | pass/fail per file |
| Checkstyle `FileLength` | violation message `maxLen.file` | pass/fail per file |
| pylint `too-many-lines` | one C0302 message per module | pass/fail per module |
| SwiftLint `file_length` | warning and/or error per file | two-tier pass/fail |
| lizard | per-function table plus a **per-file summary table** (`LOC Avg.NLOC AvgCCN Avg.ttoken function_cnt file`); warnings only for function thresholds; `--sort` sorts warnings by fields such as nloc | **raw table**, no file threshold |
| cloc `--by-file`, tokei `--files --sort lines` | **ranked raw tables** (`tokei --sort` sorts by `blanks, code, comments, lines`) | ranked list |

Observation: tools whose job is *gating a change* (ESLint in CI, Sonar quality gate, Checkstyle)
surface a threshold. Tools whose job is *describing a codebase* (cloc, tokei, lizard's file table)
surface a ranked/aggregate table and no file-length threshold. qingluan's v1 is explicitly the latter
("全仓体检", no CI gate, spec §已定范围), which points at the ranked-table shape.

### Why a threshold is a poor fit for a once-per-repo scan

1. It collapses a continuous quantity to one bit per file, discarding exactly the distribution
   context the spec already insists on for functions ("有 p50/p90/max，『87』才知道是离群值还是常态").
2. It forces a single constant across languages and file types where the natural scales differ by an
   order of magnitude (generated tables, vendored data, HTML templates vs hand-written modules).
3. There is no defensible constant to pick (this entire report).
4. Its only unique value — failing a build — is a capability qingluan v1 deliberately does not have.

### Why a ranked list plus distribution is the better surface

- It matches the existing function-level contract: `scanned … / distribution p50 p90 p99 max / worst K`.
- It is self-calibrating: on a clean repo it shows the tail is small; on a legacy repo it shows where
  the tail is. A threshold cannot express either fact.
- It composes with the no-composite-score rule: file `nloc` is just another column a human reads
  alongside the per-function vector.

---

## 9. Recommendation for qingluan

1. **Add the axis, not a gate.** Emit per-file `nloc` (non-blank, non-comment) and optionally `lines`
   (physical) as an orthogonal size vector. Do not fold it into any score — consistent with the
   existing "output a metric vector, never a composite score" decision.
2. **Count `nloc`, not `lines`, for the headline number.** It matches the function-level `nloc` field
   and avoids blaming comment-heavy files. Optionally show `lines` too, since that is what humans and
   editors display.
3. **Surface it as distribution + "longest files" top-K**, mirroring the function table:
   `scanned N files / file nloc p50 p90 p99 max / longest files (nloc, lines, functions, cognitive max)`.
   No default threshold on the repo-wide path.
4. **If `--threshold` is offered** (opt-in only), default the file-level threshold to **1000 nloc** and
   document it as a convention inherited from the tool ecosystem (majority of Sonar languages, pylint,
   SwiftLint's error tier), **not** as evidence. Do not use 300 repo-wide — that is a per-file linter
   default intended for code you are authoring, not for auditing an existing tree.
5. Keep it configurable per language if it ever gates, because the natural scale is language-dependent.
6. Skip generated/vendored/minified files before ranking (spec already requires this for functions);
   otherwise the longest-files list degenerates into a lockfile leaderboard.
7. If the phase-2 `--from/--to` diff mode ever grows a **patch-size** signal, note in the spec that
   patch size is a different axis with a better (though still modest, contested) evidence base, and
   that the "200/400 lines" folklore is a non-peer-reviewed vendor number (≤200 LOC/**hour** is the
   peer-reviewed rate norm). Prefer reporting the patch-size distribution over gating on it, for the
   same reasons as file length.

**Bottom line on the original question:** the defensible default for *file length* is "no hard default;
rank and show the tail", and if a number must exist, **1000 nloc** is the most defensible round number
purely because it is the modal ecosystem convention — with the explicit caveat that it is **convention,
not evidence**. The evidence-backed size signal lives on the *change size* axis, not the file axis.

---

## Appendix — primary sources read in this session

| # | Source | URL |
|---|---|---|
| 1 | SonarQube Metric Definitions (`lines`, `ncloc`, `comment_lines`) | https://docs.sonarsource.com/sonarqube-server/latest/user-guide/code-metrics/metrics-definition/ |
| 2 | sonar-java `TooManyLinesOfCodeInFileCheck.java` | https://raw.githubusercontent.com/SonarSource/sonar-java/master/java-checks/src/main/java/org/sonar/java/checks/TooManyLinesOfCodeInFileCheck.java |
| 3 | sonar-java `LinesOfCodeVisitor.java` | https://raw.githubusercontent.com/SonarSource/sonar-java/master/java-frontend/src/main/java/org/sonar/java/ast/visitors/LinesOfCodeVisitor.java |
| 4 | SonarJS `S104/rule.ts`, `config.ts`, `S138/rule.ts` | https://raw.githubusercontent.com/SonarSource/SonarJS/master/packages/analysis/src/jsts/rules/S104/rule.ts |
| 5 | sonar-python / sonar-php / sonar-go / sonar-dotnet S104 implementations and `S104.html`/`S104.json` | see §1 links |
| 6 | SonarCloud rules API (first-party, per-language defaults) | https://sonarcloud.io/api/rules/show?key=java:S104&organization=sonarsource |
| 7 | ESLint `max-lines` docs + source | https://eslint.org/docs/latest/rules/max-lines · https://github.com/eslint/eslint/blob/main/lib/rules/max-lines.js |
| 8 | Checkstyle `FileLength` docs + source (current, 8.0, 6.18) | https://checkstyle.org/checks/sizes/filelength.html |
| 9 | Checkstyle `MethodLength` docs + source (`countEmpty`) | https://checkstyle.org/checks/sizes/methodlength.html |
| 10 | PMD 6 `ExcessiveClassLength` / PMD 7 design rules | https://docs.pmd-code.org/pmd-doc-6.55.0/pmd_rules_java_design.html · https://docs.pmd-code.org/latest/pmd_rules_java_design.html |
| 11 | Google C++ / Java / Python style guides | https://google.github.io/styleguide/ |
| 12 | Linux kernel `coding-style.rst` | https://raw.githubusercontent.com/torvalds/linux/master/Documentation/process/coding-style.rst |
| 13 | PEP 8 | https://peps.python.org/pep-0008/ |
| 14 | Google eng-practices, "Small CLs" | https://google.github.io/eng-practices/review/developer/small-cls.html |
| 15 | pylint `format.py` (`max-module-lines`) | https://github.com/pylint-dev/pylint/blob/main/pylint/checkers/format.py |
| 16 | SwiftLint `FileLengthConfiguration.swift` | https://github.com/realm/SwiftLint/blob/main/Source/SwiftLintBuiltInRules/Rules/RuleConfigurations/FileLengthConfiguration.swift |
| 17 | revive `RULES_DESCRIPTIONS.md` | https://github.com/mgechev/revive/blob/master/RULES_DESCRIPTIONS.md |
| 18 | lizard README | https://github.com/terryyin/lizard/blob/master/README.rst |
| 19 | cloc README (`--by-file`) / tokei README (`--files`, `--sort`) | https://github.com/AlDanial/cloc · https://github.com/XAMPPRocky/tokei |
| 20 | Sjøberg et al., "Quantifying the Effect of Code Smells on Maintenance Effort", IEEE TSE 2013, DOI `10.1109/TSE.2012.89` | https://www.mn.uio.no/ifi/personer/vit/dagsj/sjoberg_etal_code-smells.pdf |
| 21 | Syer et al., "Replicating and Re-Evaluating the Theory of Relative Defect-Proneness", IEEE TSE 2015, DOI `10.1109/TSE.2014.2361131` | https://www.swag.uwaterloo.ca/www/assets/other/papers/syer-tse-2014.pdf |
| 22 | Fenton & Ohlsson, "Quantitative Analysis of Faults and Failures in a Complex Software System", IEEE TSE 2000, DOI `10.1109/32.879815` | http://www.inf.fu-berlin.de/inst/ag-se/teaching/S-ERROR-2004/FenOhl98.pdf |
| 23 | Hatton, "Re-examining the fault density–component size connection", IEEE Software 1997 + author's 2009 retraction | https://www.leshatton.org/Documents/Ubend_IS697.pdf · https://www.leshatton.org/IEEE_Soft_97b.html |
| 24 | Koru et al., "An Investigation into the Functional Form of the Size-Defect Relationship for Software Modules", IEEE TSE 2009, DOI `10.1109/TSE.2008.90` (findings read via ref. 21) | https://ieeexplore.ieee.org/document/4693715 |
| 25 | Basili & Perricone, "Software errors and complexity", CACM 1984, DOI `10.1145/69605.2085` (findings via ref. 23) | https://dl.acm.org/doi/10.1145/69605.2085 |
| 26 | Rigby & Bird, "Convergent Contemporary Software Peer Review Practices", ESEC/FSE 2013, DOI `10.1145/2491411.2491444` | https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/rigby2013convergent.pdf |
| 27 | Sadowski et al., "Modern Code Review: A Case Study at Google", ICSE-SEIP 2018, DOI `10.1145/3183519.3183525` | https://sback.it/publications/icse2018seip.pdf |
| 28 | Bosu, Greiler, Bird, "Characteristics of Useful Code Reviews", MSR 2015, DOI `10.1109/MSR.2015.21` | https://www.microsoft.com/en-us/research/publication/characteristics-of-useful-code-reviews-an-empirical-study-at-microsoft/ |
| 29 | Kononenko et al., ICSME 2015 (`10.1109/ICSM.2015.7332457`) and ICSE 2016 (`10.1145/2884781.2884840`) | DOI links |
| 30 | Baysal et al., WCRE 2013 / EMSE 2015, DOI `10.1007/s10664-015-9366-8` | https://link.springer.com/article/10.1007/s10664-015-9366-8 |
| 31 | di Biase et al., PeerJ CS 2018, DOI `10.7717/peerj-cs.193` | https://arxiv.org/abs/1805.10978 |
| 32 | Doğan & Tüzün, IST 2022, DOI `10.1016/j.infsof.2021.106737`; Kemerer & Paulk, IEEE TSE 2009, DOI `10.1109/TSE.2009.27` | DOI links |
| 33 | El Emam et al. TSE 2001 (`10.1109/32.935855`); Evanco TSE 2003 (`10.1109/TSE.2003.1214331`); Zhou et al. TSE 2009 / TOSEM 2014 (`10.1109/TSE.2009.32`, `10.1145/2556777`); Graves et al. TSE 2000 (`10.1109/32.859533`); Nagappan & Ball ICSE 2005 (`10.1145/1062455.1062514`); Bird et al. FSE 2011 (`10.1145/2025113.2025119`); Yang et al. TSE 2015 (`10.1109/TSE.2014.2370048`); Thongtanunam & Hassan TSE 2021 (`10.1109/TSE.2020.2964660`) | DOI links |
| 34 | Cohen et al., *Best Kept Secrets of Peer Code Review*, Smart Bear 2006 (non-peer-reviewed; "200/400" folklore origin) | https://archive.org/details/bestkeptsecretso00jaso |

**Explicitly unverified in this report:** the Fenton & Ohlsson quotes (extracted from the full text by
the literature sub-agent — this sandbox has no PDF text-extraction tooling, so they could not be
re-extracted here); the Koru and Basili & Perricone findings (read through Syer et al. 2015 / Hatton
1997 respectively, not from the original papers); the El Emam "associations disappear" claim (abstract
only; full text paywalled); the RSPEC SPA text (site unreachable — analyzer-packaged resources used
instead); per-language S104 counting semantics for Kotlin/Swift/Scala/C/C++ (defaults verified via the
rules API, implementations not read); non-Java PMD rule sets; Checkstyle versions older than 6.18.
