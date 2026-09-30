# Presentation of coupling in architecture tools — number, ranked list, graph, or violation list?

Topic 5 of the complexity-metrics report: **how existing architecture tools surface coupling to a human, and whether any of them ships a first-party numeric THRESHOLD on fan-in (afferent coupling) or instability.**

**Research date:** 2026-09-30.
**Method:** official docs / source code only (vendor docs, project source, first-party REST APIs). Blog posts were not used as evidence. All quotes are short verbatim excerpts; every claim carries the URL it came from. Full-text extraction was done on downloaded HTML/JS; nothing was written into the repo and `/tmp` scratch was discarded.

---

## TL;DR

1. **No first-party tool was found that ships a default numeric threshold on fan-in (afferent coupling, Ca).** The closest things are all *efferent* or *composite* metrics: SonarQube **S1200 "Classes should not be coupled to too many other classes" — default `max = 20`** (fan-out / CBO: "Maximum number of classes a single class is allowed to depend upon"), NDepend's `TypeCe > 50` recommendation and its rule **ND1410** (≥ 40 types used per method / ≥ 90 per type), and NDepend's **`NormDistFromMainSeq > 0.7`** — the only first-party *numeric* default that is a function of instability (D′ = |A + I − 1|, normalized).
2. **Structure101 does ship built-in default thresholds**, but on structural *size/cycles*, not coupling: **Fat = 15** (method, cyclomatic complexity), **Fat = 120** (edges at class/package/design level), **design-tangle threshold = 0 %**, wrapped into a single metric **XS (Excessive Structural Complexity)**. There is **no "XLM" and no "NCCD"** in Structure101's first-party docs (could not verify).
3. **JDepend (the original Martin-metrics tool) applies no thresholds at all.** `Ca`, `Ce`, `A`, `I`, `D` are computed and printed per package; cycles are listed. The only tolerance constants in the repo are in an *example JUnit test* the user is meant to copy.
4. **dependency-cruiser computes Ca/Ce/I** (the `metrics` reporter prints a table sorted by instability descending) **and enforces the Stable Dependencies Principle as a user-authored rule** — `to: { moreUnstable: true }` is a *relative* comparison, not a number. Built-in rules (`no-circular`, `no-orphans`, …) are boolean/violation rules with no thresholds.
5. **Dominant presentation form: a dependency GRAPH, plus a DSM matrix in the "architecture" tools, plus a tangle/SCC violation report for cycles.** Per-item *numbers* exist (JDepend text report, dependency-cruiser `metrics`, NDepend metrics, Sonargraph metrics view) but normally as a secondary metrics view. A *ranked list* is the second most common form (NDepend's "Most used types (#TypesUsingMe)" top-100 query, CodeScene's "Sum of Couplings" table).
6. Where thresholds exist at all, they are **either shipped as a default rule** (Structure101 XS thresholds, NDepend default rules, SonarQube S1200 = 20) **or left entirely to the user** (ArchUnit test rules, dependency-cruiser forbidden rules, Sonargraph metric-threshold configuration, Lattix design rules / heatmap thresholds).

---

## 1. Structure101 (Headway Software, acquired by SonarSource)

**Source situation.** `headwaysoftware.com` and `structure101.com` now redirect to SonarSource; the first-party help (© Headway Software Technologies Ltd) is served under `https://www.sonarsource.com/structure101/docs/`. All URLs below are live and return HTTP 200.

### Metric definitions

**XS — Excessive Structural Complexity** (page title of `xs/xs.html`, verified):

> "Excessive Structural Complexity (XS)"
> https://www.sonarsource.com/structure101/docs/java/studio6/Content/xs/xs.html

The framework steps:

> "First measure the complexity of every item in the code-base hierarchy (i.e. every method, type, file, folder). **Thresholds define when an item is considered excessively-complex.**"
> https://www.sonarsource.com/structure101/docs/java/studio6/Content/xs/measuring-xs.html

**"Fat" is defined against a threshold** (this is the tool's structural-size metric):

> "Structure101 calculates the number of execution paths through a function (known as McCabe's metric, or Cyclomatic Complexity - CC) and **considers it Fat when this value exceeds a defined threshold.**"
> https://www.sonarsource.com/structure101/docs/java/studio6/Content/restructure101/fat.html

> "Mostly the number of **edges in the dependency graph** at the breakout point is used. For functions/methods, the number of possible execution paths (also known as Cyclomatic Complexity (CC)) is used."
> https://www.sonarsource.com/structure101/docs/java/studio6/Content/xs/fat.html

**"Tangle" = cyclic dependencies** (SCC-like, plus minimum feedback set):

> "A tangle is a portion of a dependency graph within which all the items are directly or indirectly dependent on all the other nodes in the tangle."
> "The **design tangle metric** is calculated as the total number of code-level references in the mfs divided by the total number of code-level references on the parent high-level packages … 6 / (14 + 6 + 51) = 0.09 = 9%"
> https://www.sonarsource.com/structure101/docs/cpa/studio/Content/xs/tangle.html

**"Structural over-complexity"** is the official umbrella term:

> "Structural over-complexity — These measures look at code that is not in your Structure Spec. … They comprise 2 simple concepts. **Fat** is too much stuff at any point of the source code composition. … **Tangles** occur when code containers are cyclically dependent."
> https://www.sonarsource.com/structure101/docs/java/studio6/Content/reference/key-measures.html

### Exact thresholds (YES — built-in defaults, user-configurable)

> "The industry norm for the maximum number of paths through a function is usually 10-15. **By default, Structure101 Studio takes the value of 15 as the threshold for Fat at method level.** … Fat at the other points of breakout are measured as the number of edges in the corresponding dependency graph. There is no industry norm for this value, but we have found that **120 is a good start-point for most projects**. Structure101 Studio lets you change these defaults at any or all scopes. **The threshold for design tangles is 0%** — package-level dependency tangles are always considered undesirable."
> https://www.sonarsource.com/structure101/docs/java/studio6/Content/xs/excess-complexity.html

> "A configuration defines a set of metrics and thresholds - enter edit mode to change these. **The default is the Structural Integrity configuration** - this is what we reckon is a good place to start."
> https://www.sonarsource.com/structure101/docs/java/studio6/Content/perspectives/xsconfig.html

**Formula** (the "excess" amount, normalized against the threshold):

> "we calculate a percentage from the value and the threshold as follows: **Max ( Value - Threshold , 0 ) / Value** … E.g. for value = 10: max( (10 - 15)/10, 0 ) = max( -0.5, 0 ) = 0"
> https://www.sonarsource.com/structure101/docs/cpa/studio/Content/xs/quantifying-xs.html

**Fan-in?** No. `fan-in`, `afferent`, `fan-out` do not occur in the official help tree. Structure101's thresholds are on **Fat (composition size)** and **tangles (cycles)**; architecture violations are checked against a **user-declared Structure Spec / Architecture Diagram**, not against a coupling number.

### Presentation form

- **Graph** + tangle isolation: "Structure101 Studio's auto-partitioning isolates tangles in a red box." (`xs/tangle.html`)
- **DSM matrix**: "The dependency matrix is an alternative to the diagram for visualizing dependency graphs. … If there are any dependencies above the diagonal, then the graph contains at least one tangle." — https://www.sonarsource.com/structure101/docs/java/studio/content/graphs/matrix
- **Ranked/offender list + aggregate chart**: over-complexity chart of "% of the code-base that lies within a Fat item" vs "% … within a Tangled region", with lists of contributing items (`restructure101/complexity-chart.html`, `perspectives/offenders_distribution.html`).
- **CLI**: `check-xs` (offenders XML, `fail-on-offenders` default `true`), `check-key-measures` / `checkarch` (CSV, `violations.xml`), `report-key-measures` (XML). See `Content/cli/headless-checkxs.html`, `headless-check-key-measures.html`, `headless-checkarch.html`.

**Not verifiable:** **"XLM" / "Excess Level Metric"** — no occurrence in the live help tree, the XS whitepaper, or the archived CLI help. The official metric is **XS**. **"NCCD"** — not present in Structure101 docs.

---

## 2. NDepend (ndepend.com)

### Metric definitions — https://www.ndepend.com/docs/code-metrics

- **Afferent coupling (Ca):** "The number of types outside this assembly that depend on types within this assembly. High afferent coupling indicates that the concerned assemblies have many responsibilities." — **no recommended threshold is given for Ca.**
- **Efferent coupling (Ce):** "The number of types outside this assembly used by child types of this assembly. High efferent coupling indicates that the concerned assembly is dependant."
- **Relational Cohesion (H):** "H = (R + 1)/ N" — with a recommendation: "**A good range for RelationalCohesion is 1.5 to 4.0.**"
- **Instability (I):** "The ratio of efferent coupling (Ce) to total coupling. **I = Ce / (Ce + Ca).** … range … 0 to 1, with I=0 indicating a completely stable assembly and I=1 indicating a completely instable assembly."
- **Distance from main sequence (D):** "The perpendicular normalized distance of an assembly from the idealized line A + I = 1 … range 0 to 1" — with a recommendation: "**Assemblies where NormDistFromMainSeq is higher than 0.7 might be problematic.** However, in the real world it is very hard to avoid such assemblies."
- **Type-level efferent coupling:** "**Types where TypeCe > 50 are types that depends on too many other types.** They are complex and have more than one responsibility."

All on https://www.ndepend.com/docs/code-metrics (sections "Metrics on Assemblies" / "Metrics on Types").

### Exact thresholds in the shipped default rule set

The default rule set is browsable at https://www.ndepend.com/default-rules/NDepend-Rules-Explorer.html (211 rules with full CQLinq source; `?ruleid=ND1407` etc. are stable permalinks).

| Rule | CQLinq condition (verbatim) | Meaning |
|---|---|---|
| **ND1407** AssembliesThatDontSatisfyTheAbstractnessInstabilityPrinciple | `warnif count > 0 from a in Application.Assemblies where a.NormDistFromMainSeq > 0.7` | **D′ (from A and I) > 0.7** is a violation |
| **ND1410** AvoidExcessiveClassCoupling | `where typesUsed.Length >= (x.IsMethod ? 40 : 90)` (+ `namespacesUsed.Length >= (x.IsMethod ? 5 : 10)`, + min cyclomatic complexity 10/25) | efferent coupling threshold 40 (method) / 90 (type) |
| **ND1405** AssembliesWithPoorRelationalCohesion | `where types.LongCount() > 20` … `where relationalCohesion < 0.8` | H < 0.8 |
| **ND1406** NamespacesWithPoorRelationalCohesion | same, namespace level | H < 0.8 |
| **ND1400 / ND1401** AvoidNamespacesMutuallyDependent / AvoidNamespacesDependencyCycles | `warnif count > 0` over SCC/cycle detection (`ContainsNamespaceDependencyCycle`, null `Level`) | **cycles, no numeric threshold** |
| **ND1213** AvoidTypesInitializationCycles | `warnif count > 0` | cycles, no numeric threshold |

ND1407's own description text:

> "This rule warns about assemblies with a *normalized distance* greater than than **0.7**."
> https://www.ndepend.com/default-rules/NDepend-Rules-Explorer.html (rule ND1407)

### Fan-in presentation — a RANKED LIST, not a gate

The shipped statistics query `Most used types (#TypesUsingMe )` is exactly a ranked fan-in table:

```csharp
// <Name>Most used types (#TypesUsingMe )</Name>
(from t in Types orderby t.NbTypesUsingMe descending
 where !t.IsGeneratedByCompiler
 select new { t, t.TypesUsingMe }).Take(100)
```

> "This code query lists the 100 application and third-party types, with the higher number of types users."
> https://www.ndepend.com/default-rules/NDepend-Rules-Explorer.html

The only place fan-in appears in a *rule* condition is the dead-code rule `t.NbTypesUsingMe == 0` (ND1700 family), i.e. a boolean "is it used at all", not a "too many" threshold.

### Presentation form

- **Dependency graph** and **Dependency Structure Matrix**: https://www.ndepend.com/docs/dependency-structure-matrix-dsm — the rules themselves tell you to use them: "To browse a cycle on the dependency graph or the dependency matrix, right click a cycle cell … export the matched namespaces to the dependency graph or matrix." (ND1401 HowToFix).
- **Violation/issue list**: rules produce issues with Debt and Severity; Quality Gates turn groups of issues into PASS/WARN/FAIL: "A Quality Gate outputs a status (Pass, Warn, Fail)." — https://www.ndepend.com/docs/quality-gates
- **Per-metric numeric views**: the metrics are also surfaced through code queries and the metrics views described at https://www.ndepend.com/docs/code-metrics.

---

## 3. JDepend (Mike Clark, clarkware.com / github.com/clarkware/jdepend)

### Metric definitions — source + docs

`JavaPackage.java`:

```java
/** @return The afferent coupling (Ca) of this package. */
public int afferentCoupling() { return afferents.size(); }
/** @return The efferent coupling (Ce) of this package. */
public int efferentCoupling() { return efferents.size(); }
/** @return Instability (0-1). */
public float instability() {
    float totalCoupling = (float) efferentCoupling() + (float) afferentCoupling();
    if (totalCoupling > 0) { return efferentCoupling()/totalCoupling; }
    return 0;
}
/** @return The package's abstractness (0-1). */
public float abstractness() { ... getAbstractClassCount() / getClassCount() ... }
/** @return The package's distance from the main sequence (D). */
public float distance() { float d = Math.abs(abstractness() + instability() - 1); return d * volatility; }
```

https://github.com/clarkware/jdepend/blob/master/src/jdepend/framework/JavaPackage.java

The historical docs repeat the definitions: "The ratio of efferent coupling (Ce) to total coupling (Ce + Ca) such that I = Ce / (Ce + Ca)." and "D=0 indicating a package that is coincident with the main sequence" — https://github.com/clarkware/jdepend/blob/master/docs/JDepend.html (sections "Instability (I)", "Distance from the Main Sequence (D)", "Package Dependency Cycles").

### Thresholds — **none**

Full-text search of the repository (`src/`, `docs/`, `README.md`, config files) finds **no default threshold, no "ideal range", no 0.3/0.7 constants, and no D cut-off**. The metric ranges (0–1) are *definitional* ranges, and "ideal packages are either completely abstract and stable (x=0, y=1) or completely concrete and instable (x=1, y=0)" is a definition of D = 0, not a gate.

The only numeric tolerances in the project are in the **sample test file users are meant to copy**:

```java
double ideal = 0.0;
double tolerance = 0.8;   // testOnePackageDistance
...
double tolerance = 1.0;   // testAllPackagesDistance
assertEquals("Distance exceeded: " + p.getName(), ideal, p.distance(), tolerance);
```

https://github.com/clarkware/jdepend/blob/master/test/jdepend/framework/ExampleTest.java

So the "JDepend ideal-range check" is a **user-authored assertion pattern**, not a shipped default, and the repo's own example uses a very loose tolerance (0.8 / 1.0).

### Presentation form

- **Per-package number block** in the text report (`docs/jdepend-text.out`): `Ca:`, `Ce:`, `A:`, `I:`, `D:`, plus `Depends Upon:` / `Used By:` lists.
- **Cycle list**: the text UI prints a "Package Dependency Cycles" section (`src/jdepend/textui/JDepend.java`, `printCycles`), driven by `JavaPackage.containsCycle()/collectCycle()`; the Swing UI shows a `Cyclic` flag in the package node.
- **Graph-ish trees**: the Swing UI has an "efferent couplings" tree and an "afferent couplings" tree (package hierarchy exploration), documented in `docs/JDepend.html` ("The top tree displays the efferent couplings … The bottom tree displays the afferent couplings").
- **DOT export** via `contrib/jdepend2dot.sh` / `jdepend2dot.xsl`.
- **Assertion API**: `DependencyConstraint` for "these packages may only depend on these" (`assertEquals("Dependency mismatch", true, constraint.match(...))`) — a user-authored, exact-graph comparison, not a metric threshold.

---

## 4. dependency-cruiser (sverweij)

> Note: `dependency-cruiser.js.org` is a js.org placeholder page (it only loads `https://js.org/302?dependency-cruiser.js`), **not** the documentation. The docs are the `doc/` folder of the GitHub repo; GitHub blob URLs are used below.

### Metrics — it DOES compute Ca / Ce / I

```js
afferentCouplings: dependents.length,
efferentCouplings: dependencies.length,
instability,
```

`src/report/metrics.mjs` — https://github.com/sverweij/dependency-cruiser/blob/main/src/report/metrics.mjs

The `metrics` reporter prints a table with columns `type, name, N, Ca, Ce, I (%), size, #tls`, **sorted by instability descending by default**:

> "By default the metrics reporter emits instability metrics for all modules and folders, ordered by instability (descending)."
> https://github.com/sverweij/dependency-cruiser/blob/main/doc/options-reference.md

CLI switch:

> "`--metrics` — Makes dependency-cruiser calculate stability metrics (number of dependents, number of dependencies and 'instability' (`# dependencies/ (# dependencies + # dependents)`)) for all folders. These metrics are adapted from *Agile software development: principles, patterns, and practices* by Robert C Martin."
> https://github.com/sverweij/dependency-cruiser/blob/main/doc/cli.md

Reporter column definitions (official):

| metric | abbreviation | description |
|---|---|---|
| Afferent couplings | Ca | "The number of modules outside this folder that depend on this folder ("coming in")" |
| Efferent couplings | Ce | "The number of modules this folder depends on *outside* the current folder ("going out")" |
| Instability | I | "Ce / (Ca + Ce) a number between 0 and 1 … 0: wholy stable; and 1 wholy unstable" |

https://github.com/sverweij/dependency-cruiser/blob/main/doc/cli.md (section "metrics - generate a report with stability metrics for each folder")

### Thresholds — **no numeric threshold; the SDP rule is relative**

The Stable Dependencies Principle is expressed as a *relationship* between two modules, not a cut-off:

> "`moreUnstable` — When set to true moreUnstable matches for any dependency that has a higher Instability than the module that depends on it. … This attribute is useful when you want to check against Robert C. Martin's stable dependencies principle: 'depend in the direction of stability'."
> https://github.com/sverweij/dependency-cruiser/blob/main/doc/rules-reference.md

```javascript
{ name: "SDP", from: {}, to: { moreUnstable: true } }
```

The shipped rule presets (`configs/recommended.cjs` → `configs/rules/*.cjs`) contain **no numeric thresholds**: `no-orphans` (`from: { orphan: true }`, severity warn), `no-circular` (`to: { circular: true }`, severity error), `no-deprecated-core`, `no-duplicate-dependency-types`, `no-non-package-json`, `not-to-deprecated`, `not-to-unresolvable`. `orphan` and `reachable` are booleans:

> "`orphans` — A Boolean indicating whether or not to match modules that have no incoming and no outgoing dependencies."
> "`reachable` — a Boolean indicating whether or not modules matching the `to` part of the rule are *reachable* … from modules matching the `from` part."
> https://github.com/sverweij/dependency-cruiser/blob/main/doc/rules-reference.md

(`moreUnstable` is also limited to `module`/`folder` scope: "at this time only the `moreUnstable`, `circular` and `path`/ `pathNot` attributes … work, so it is possible to check the 'stable dependencies principle' on folder level.")

### Presentation form

Many reporters, all first-party: `dot`, `ddot`, `archi`, `flat`, `mermaid`, `d2`, `json`, `text`, `csv`, `markdown`, `metrics`, `html`, `dot-webpage`, `teamcity`, `azure-devops`, `baseline`, `anon`, `error*`, `null` (`src/report/`). So: **graph first-class, plus a numeric metrics table**, plus **rule-violation lists** for `forbidden` rules.

---

## 5. SonarQube (SonarSource)

### Is there an official coupling *metric*?

**No — not today.** Full-text extraction of the current and recent official metric-definition pages finds **zero occurrences of `coupl`, `afferent`, `efferent`, `instability`, `abstractness`, or `main sequence`**:

- https://docs.sonarsource.com/sonarqube/latest/user-guide/metric-definitions/
- https://docs.sonarsource.com/sonarqube-server/9.9/user-guide/metric-definitions/
- https://docs.sonarsource.com/sonarqube-server/8.9/user-guide/metric-definitions/

The live metric registry also contains no such metric (`GET /api/metrics/search?ps=500` on SonarSource's own instance: 271 metrics, none named coupling/instability/afferent). Coupling in SonarQube is therefore a **rule**, not a metric, and it is **fan-out/CBO**, not fan-in.

### S1200 — the one real first-party coupling threshold

`GET https://next.sonarqube.com/sonarqube/api/rules/show?key=java:S1200` (SonarSource's own SonarQube instance):

```json
{"key":"java:S1200","name":"Classes should not be coupled to too many other classes",
 "params":[{"key":"max",
            "htmlDesc":"Maximum number of classes a single class is allowed to depend upon",
            "defaultValue":"20","type":"INTEGER"}],
 "type":"CODE_SMELL","severity":"MAJOR","scope":"MAIN"}
```

- **Default: `max = 20`.**
- Canonical human URL: https://rules.sonarsource.com/java/RSPEC-1200/ (this host did not respond from this network — the API above is the primary evidence used). The rule also exists for `php:S1200` and `csharpsquid:S1200`.
- Semantics: "classes a single class is allowed to **depend upon**" = **efferent** coupling (fan-out). There is no fan-in counterpart.

### Cycles / tangles

SonarQube's newer "Architecture" feature has a first-party definition of *tangle* and a directive-driven rule:

> "**Tangles** — A tangle is a set of classes or files that depend on each other in a cycle. There is a path from every item to every other item in the tangle's dependency graph. Tangles make code more complex, and harder to understand and maintain."
> https://docs.sonarsource.com/sonarqube-server/architecture-analysis.md

> "**Flaws** are structural problems that exist in the codebase regardless of the intended architecture. Flaws include tangles and oversized components. … For each type of SonarQube issue, you get a list of issues, ordered by priority."
> https://docs.sonarsource.com/sonarqube-server/architecture-analysis/project-architecture.md

Rule `javaarchitecture:S8134`:

> "Forbidden relationships should be removed to solve tangles as directed by the project's architect" — severity MAJOR, **`params: []` (no threshold)**
> https://next.sonarqube.com/sonarqube/api/rules/show?key=javaarchitecture:S8134

So even the tangle rule has no number; the number comes from the *user's declared intended architecture* ("any non-defined dependency between siblings will be considered as forbidden" — project-architecture.md).

### Presentation form

Interactive **architecture map** (levelized graph) + intended-architecture editor + **ranked problem list** ("ordered by priority") + **per-tangle visual representation** + issues surfaced in quality gates. Note the map is the tool's default view, not a per-file coupling number.

---

## 6. ArchUnit (Java)

**Rules are project-authored test-code assertions — there is no shipped graded rule set and no quality threshold.**

> "ArchUnit's main focus is to automatically test architecture and coding rules, using any plain Java unit testing framework."
> "To express architectural rules … ArchUnit offers an abstract DSL-like fluent API … To specify a rule, use the class `ArchRuleDefinition` as entry point"
> https://www.archunit.org/userguide/html/000_Index.html

Layer rule (verbatim from the guide):

```java
layeredArchitecture().consideringAllDependencies()
    .layer("Controller").definedBy("..controller..")
    .layer("Service").definedBy("..service..")
    .layer("Persistence").definedBy("..persistence..")
    .whereLayer("Controller").mayNotBeAccessedByAnyLayer()
    .whereLayer("Service").mayOnlyBeAccessedByLayers("Controller")
    .whereLayer("Persistence").mayOnlyBeAccessedByLayers("Service")
```
https://raw.githubusercontent.com/TNG/ArchUnit/main/docs/userguide/004_What_to_Check.adoc

Cycle check: `slices().matching("com.myapp.(*)..").should().beFreeOfCycles()` — same URL (§4.7 Cycle Checks); `ModuleRuleDefinition.modules().definedByPackages(...).should().beFreeOfCycles()` in
https://raw.githubusercontent.com/TNG/ArchUnit/main/docs/userguide/008_The_Library_API.adoc

**Metrics exist but are compute-only.** §8.7 "Software Architecture Metrics" documents Lakos CCD/ACD/RACD/NCCD (§8.7.1) and Martin Ce/Ca/I/A/D (§8.7.2) — the guide only prints the values; `ArchitectureMetrics` exposes no assertion API. There is no built-in threshold.
https://www.archunit.org/userguide/html/000_Index.html (§8.7)

The only numeric settings in ArchUnit are **cycle-report/performance caps**, not quality gates:

> "# This will limit the maximum number of cycles to detect and thus required CPU and heap. # default is 100" (`cycles.maxNumberToDetect`)
> "# This will limit the maximum number of dependencies to report per cycle edge. … # default is 20" (`cycles.maxNumberOfDependenciesPerEdge`)
> https://www.archunit.org/userguide/html/000_Index.html (§8.2.1 Configurations)

Output form: a **failing test / violation list** naming the offending dependencies or cycles.

---

## 7. JDeps (OpenJDK official tool)

> "The jdeps command shows the package-level or class-level dependencies of Java class files. … By default, the jdeps command writes the dependencies to the system output."
> https://docs.oracle.com/en/java/javase/21/docs/specs/man/jdeps.html

- Options: `-s`/`-summary` ("Prints a dependency summary only."), `-verbose:package`, `-verbose:class`, `--dot-output` ("generates one .dot file for each analyzed archive … and also a summary file named summary.dot").
- Output is a **per-package text dependency list**:
  ```
  Notepad.jar -> java.base
  Notepad.jar -> java.desktop
  <unnamed> (Notepad.jar)
   -> java.awt
   -> java.awt.event
  ```
- **No metrics and no numeric thresholds of any kind.**
- **`jdeps -cycles` does not exist.** A full-text search of the JDK 8/17/21/25 jdeps man pages and of `jdeps --help` on OpenJDK/Temurin 25.0.4.1 finds no `cycle`/`cyclic` option; jdeps performs no cycle detection.

---

## 8. Other tools (first-party docs only)

### Sourcetrail (archived, github.com/CoatiSoftware/Sourcetrail)

- Presentation: **interactive dependency graph only**. "**Graph:** The graph displays the structure of your source code. It focuses on the currently selected symbol and directly shows all incoming and outgoing dependencies to other symbols." (README.md)
- "The graph view visualizes the currently selected symbol and all its relationships to other symbols as an interactive graph visualization." (`DOCUMENTATION.md`)
- **No coupling metric, no threshold, and no cycle/SCC view** (no `cycle`/`circular`/`tarjan`/`strongly` matches in README/DOCUMENTATION/CHANGELOG; no `Cycle*`/`Tarjan` file under `src/lib/data/graph/`).

### CodeScene (docs.enterprise.codescene.io, v6.8.8)

- Code Health: "an aggregated metric based on 25+ factors scanned from the source code"; score from 10 down to 1. (guides/technical/code-health.html, terminology/codescene-terminology.html)
- Change Coupling (temporal/logical): "Change Coupling means that two (or more) modules change together over time." Ranking output: "The **Sum of Couplings** view gives you **a table of files sorted by this metric**." (guides/technical/change-coupling.html)
- **Verified numeric defaults:** `coupling_threshold_percent` — "Specifies minimal temporal coupling for the 'Absence of Expected Change' warning. **Default is 80 (%).**" (guides/delta/automated-delta-analyses.html); hotspot-map "coupling threshold is **fixed at 20%**" (guides/technical/hotspots.html); full scan reverts to hotspot scan above **50 files**; Knowledge Island = **≥ 95 %** single-author.
- These are thresholds on *temporal/change* coupling and on Code Health — **not** on structural fan-in/instability.
- Presentation: hotspot map, hierarchical change-coupling graph, sorted tables, and **CI pass/fail quality gates** (violation-style).

### Lattix (docs.lattix.com)

- Presentation: **DSM matrix is primary**. "The Dependency Structure Matrix (DSM) is one of the primary ways to visualize a project within Lattix Architect. … It is highly scalable and the problematic dependencies are easy to identify." (userGuide/Working_with_the_Dependency_Structure_Matrix_DSM.html); plus a Conceptual Architecture Diagram.
- Enforcement is **rules → violations**: "Design Rule violations are flagged"; `Cannot Use` / `Can Use` / `Must Use` rules (userGuide/Monitoring_and_Enforcing_Architecture.html).
- Metrics with formulas but **no default thresholds**: Coupling "= 100 * n / ( V*(V-1)/2 )", System Cyclicality, Intercomponent Cyclicality, Normalized Cumulative Dependency (NCCD). (userGuide/Metrics.html).
  - Lattix quotes Lakos' book for NCCD reference values: "'Typical values for the NCCD of a high-quality package architecture implementing an application specific tool range from about 0.85 to about 1.10.'" — this is a *book* recommendation quoted in the docs, not a Lattix gate.
- The only verified built-in numeric defaults are heatmap colors: "system stability by default has a **warning threshold of 80% and a fail threshold of 50%**" (userGuide/Working_with_the_Heatmap.html). That is Lattix's own "System Stability" metric, not Martin's I.

### Sonargraph (hello2morrow, eclipse.hello2morrow.com/doc/standalone)

- Metrics documented as one-liners: "**Structural Debt Index (Components)** — Description: Cumulative structural debt index of component cycles."; "**Cyclicity (Components)** — Cumulated cyclicity of component cycles."; "**ACD** — Average component dependency according to John Lakos."; "**CCD** — Cumulative component dependency according to John Lakos."; "**Component Dependencies to Remove (Components)** — Number of component dependencies to remove to break up all component cycles." (content/core_metrics.html)
- **No official derivation or default value for the Structural Debt Index** could be verified; **no built-in default metric threshold value** is published.
- Thresholds are **user-configured**: "The metric thresholds configuration allows to define threshold values for those predefined metrics…"; "The pie chart is only available for metrics with a defined threshold." (content/examining_metrics_results.html)
- Quality gates are **user-authored conditions**: e.g. "Number of architecture violations must be reduced by at least 10%.", "An increase in the Average Component Dependency (ACD) must be lower than 5%." (content/defining_qualitygates.html). CI output shows threshold violations: "Condition '<= 0 threshold violations for metric 'Core:Type:SourceElementCount' …'" (content/qualitygate_sgbuild_integration.html).
- Architecture DSL = **artifact/dependency-direction rules** ("it would be marked as an architecture violation if a class from the UI layer would create a new instance of an object from the model layer"), not numeric asserts.
- Presentation: graph views, cycle view, per-element **metrics view** (table/histogram/pie), and **architecture violation lists**.

---

## Summary table

| Tool | Coupling metric(s) | Built-in numeric threshold? | Output / presentation form |
|---|---|---|---|
| **Structure101** | Fat (edges / CC), design tangle = mfs-references ÷ total references, XS = normalized excess × LOC. No fan-in metric. | **YES (shipped defaults)** — Fat = **15** (method CC), Fat = **120** (edges, class/package/design), design tangle = **0 %**; user-editable configs. | Graph + **DSM matrix** + tangle isolation (red box/mfs edges) + %Fat/%Tangled chart + offender lists + CLI XML/CSV (`check-xs`, `fail-on-offenders` default true) |
| **NDepend** | Ca, Ce, I = Ce/(Ce+Ca), A, D/D′ (NormDistFromMainSeq), H | **YES (shipped default rules)** — ND1407 `NormDistFromMainSeq > 0.7`; ND1410 types-used ≥ **40** (method) / **90** (type); ND1405/06 H < **0.8**; doc: `TypeCe > 50`, H good range **1.5–4.0**. **No Ca threshold.** | **DSM** + dependency graph + issue/violation list (rule → issues, Debt/Severity) + Quality Gate PASS/WARN/FAIL + ranked "Most used types" top-100 query |
| **JDepend** | Ca, Ce, A, I = Ce/(Ce+Ca), D = \|A+I−1\|·V, plus cycle flag | **NO** — no constants anywhere; the `0.8`/`1.0` tolerance lives only in the example JUnit test the user copies | Per-package **number block** (Ca/Ce/A/I/D, Depends Upon/Used By) + **cycle list** + Swing efferent/afferent trees + DOT export + `DependencyConstraint` assertion API |
| **dependency-cruiser** | Ca, Ce, I = Ce/(Ca+Ce) (`--metrics`) | **NO** — `moreUnstable: true` is *relative*; `orphan`/`circular`/`reachable` are booleans; no numbers in the preset rules | **Graph reporters** (dot/ddot/archi/flat/mermaid/d2/html) + **metrics table sorted by I desc** + JSON/CSV/text/markdown + forbidden-rule **violation list** |
| **SonarQube** | **No coupling metric** in the current registry. S1200 = CBO/fan-out rule. | **YES (shipped rule default)** — S1200 `max = 20` (classes depended upon = efferent). Tangle rule `S8134` has **no** param. | Architecture **map** (levelized graph) + intended-architecture editor + prioritized **problem list** + per-tangle visualization + issues in quality gates |
| **ArchUnit** | Computes Lakos CCD/ACD/RACD/NCCD and Martin Ce/Ca/I/A/D (compute-only) | **NO quality threshold.** Only cycle report caps: `cycles.maxNumberToDetect=100`, `cycles.maxNumberOfDependenciesPerEdge=20` | Project-authored rules in test code → **failing test / violation list** |
| **JDeps** | none | **NO** | Per-package/class **text dependency list**, `-summary`, optional **DOT graph**. No cycle detection |
| **Sourcetrail** | none | **NO** | Interactive **dependency graph** only |
| **CodeScene** | Code Health (10→1), change/temporal coupling, "Sum of Couplings" | **YES, but on temporal coupling / Code Health**: `coupling_threshold_percent` = **80 %**, hotspot coupling **20 %**, Knowledge Island **95 %**. Not structural fan-in/instability | Hotspot map + coupling **graph** + **ranked tables** + CI pass/fail gates |
| **Lattix** | Coupling = 100·n/(V(V−1)/2), System/Intercomponent Cyclicality, NCCD (formulas only) | Rules: **none numeric**. Heatmap only: System Stability warn **80 %** / fail **50 %** (Lattix's own metric, not Martin's I) | **DSM matrix** (primary) + CAD diagram + heatmap + design-rule **violation** reports |
| **Sonargraph** | SDI (component cycles), Cyclicity, ACD/CCD (Lakos), Component Rank | **NO published default values**; thresholds are user-configured and surfaced as violations | Graph/cycle views + metrics view (table/histogram/pie for metrics with a threshold) + architecture-violation lists + quality gates |

---

## Direct answers

### (a) Is there ANY first-party precedent for a numeric threshold on fan-in (afferent coupling) specifically, or on instability (I = Ce/(Ca+Ce))?

**Fan-in (Ca): no. No tool in this survey ships a numeric default threshold on afferent coupling. Could not verify any.**

- SonarQube's only coupling threshold — **S1200 "Classes should not be coupled to too many other classes", default `max = 20`** — is on *how many classes a class depends upon*, i.e. **efferent**/fan-out. URL: https://next.sonarqube.com/sonarqube/api/rules/show?key=java:S1200 (canonical page https://rules.sonarsource.com/java/RSPEC-1200/).
- NDepend gives a *recommendation* for efferent coupling ("Types where **TypeCe > 50** are types that depends on too many other types", https://www.ndepend.com/docs/code-metrics) and a rule threshold on types-consumed (**≥ 40 / ≥ 90**, ND1410), but **gives no recommendation and no rule threshold for Ca**. The only fan-in-related condition is `t.NbTypesUsingMe == 0` (dead code), not "too many".
- Structure101 has thresholds only on **Fat** (edges/CC) and **tangles** (cycles), and its docs never mention afferent coupling.
- JDepend, dependency-cruiser, ArchUnit, JDeps, Sourcetrail: no fan-in threshold at all.

**Instability (I): no direct threshold on I itself, but yes for a metric derived from I.**

- **NDepend rule ND1407 "Assemblies that don't satisfy the Abstractness/Instability principle"** is the one concrete first-party default: `where a.NormDistFromMainSeq > 0.7`, documented as "This rule warns about assemblies with a normalized distance greater than **0.7**". `NormDistFromMainSeq` is the normalized distance from Martin's main sequence, i.e. a function of **A and I**. This is the closest thing to a first-party numeric instability threshold. URLs: https://www.ndepend.com/default-rules/NDepend-Rules-Explorer.html (rule ND1407) and https://www.ndepend.com/docs/code-metrics (recommendation "higher than 0.7 might be problematic").
- No tool ships a threshold of the form "I < 0.3" or "0.3 ≤ I ≤ 0.7". The classic-looking "I between 0.3 and 0.7" that is often attributed to JDepend **could not be verified in the JDepend source or docs** — JDepend has no such constant.
- dependency-cruiser deliberately expresses SDP as a *relative* rule (`moreUnstable: true`, no number), and its own docs note instability is descriptive: "Instability has a bit of an unusual connotation here - it's not 'bad' to be a 100% Instable module - it's only the nature of the module." (https://github.com/sverweij/dependency-cruiser/blob/main/doc/rules-reference.md)

**Other numeric thresholds that do exist first-party (for context):** Structure101 Fat 15/120 and tangle 0 %; NDepend H 1.5–4.0 and H < 0.8; SonarQube S1200 = 20; CodeScene `coupling_threshold_percent` = 80 % (temporal coupling); Lattix System Stability 80 %/50 % (heatmap).

### (b) Or do architecture tools universally present a graph, a ranked list, or a violation list, leaving the threshold to the user?

**Mostly the latter, but not universally — there are three clear "shipped default" exceptions.**

- **Threshold left to the user (user-authored rules):** ArchUnit (test-code rules; §8.7 metrics are compute-only), dependency-cruiser (`forbidden` rules; `moreUnstable` is relative), Sonargraph (metric-threshold configuration + user-authored quality-gate conditions), Lattix (design rules + user-editable heatmap thresholds), JDepend (no thresholds at all — the tolerance only appears in the example test the user copies), JDeps (nothing).
- **Threshold shipped as a default rule, still overridable:** Structure101 (Fat 15/120, tangle 0 % — "lets you change these defaults at any or all scopes"), NDepend (default rule set ND1407/ND1410/ND1405-06), SonarQube (S1200 `max = 20`).
- **Dominant *output* contract regardless of thresholds:** a **dependency graph** (universal), a **DSM matrix** in the architecture-oriented tools, and a **violation list** for rules/cycles. A **ranked list** is the standard way coupling-adjacent *numbers* are surfaced (NDepend's "Most used types (#TypesUsingMe)" top-100, CodeScene's "Sum of Couplings" table, dependency-cruiser's instability-sorted metrics table, Structure101's offender lists).

### (c) What is the canonical UI: graph, DSM matrix, tangle/SCC report, or per-file number?

All four exist, with a clear hierarchy:

1. **Dependency graph — the universal baseline.** Every UI tool surveyed (Structure101, NDepend, JDepend's Swing trees + dot export, dependency-cruiser, SonarQube Architecture map, Sourcetrail, CodeScene, Sonargraph, Lattix's CAD) offers one; for Sourcetrail it is the *only* form.
2. **DSM matrix — the canonical *architecture* view** in the tools that target architects: Structure101 ("The dependency matrix is an alternative to the diagram for visualizing dependency graphs"), NDepend (dedicated doc page, cycles shown as red squares/black cells), Lattix ("one of the primary ways to visualize a project"). Note that in a DSM, cycles *are* the above-diagonal cells — Structure101: "If there are any dependencies above the diagonal, then the graph contains at least one tangle."
3. **Tangle / SCC report — the canonical *cycle* violation form** (Structure101 tangles, SonarQube "tangles", NDepend namespace-cycle rules, ArchUnit `beFreeOfCycles()`, dependency-cruiser `no-circular`, JDepend's cycle list). Cycle presentation is a **violation list**, and tangle "size" is expressed either as a count of items or as a feedback-set ratio (Structure101's design tangle metric).
4. **Per-file/per-package number — a secondary *metrics* view, never the primary UI.** JDepend's text report (Ca/Ce/A/I/D per package) and dependency-cruiser's `metrics` reporter (Ca/Ce/I table) are the closest to a pure "per-file number" contract; NDepend and Sonargraph expose numbers through metric views/code queries; SonarQube currently exposes no coupling number at all. **No tool we found presents coupling as a bare per-file number without a graph/list/report around it.**

---

## Could not verify

- **Structure101 "XLM" / "Excess Level Metric".** No occurrence in the live help tree, the XS whitepaper, or archived CLI help; the official name is **XS = Excessive Structural Complexity**. Treat "XLM" as unverified.
- **Structure101 "NCCD".** Not present in Structure101 docs (it is a Lakos metric; it appears in ArchUnit and Lattix docs instead).
- **A numeric threshold on afferent coupling (Ca) in any tool.** None found.
- **A numeric threshold on Martin's instability I itself** (e.g. "0.3–0.7"). None found; only NDepend's D′ > 0.7, which is a function of I and A.
- **JDepend "ideal range" defaults.** No such constants exist in the source or docs; the only tolerances are in the example JUnit test (`0.8`, `1.0`).
- **`jdeps -cycles`.** Not a real JDeps option (absent from JDK 8/17/21/25 man pages and `jdeps --help`).
- **SonarQube historical Ca/Ce/I metrics.** Current (latest), 9.9 and 8.9 official metric-definition pages contain no such metrics, and `CoreMetrics` in SonarQube 5.6 / 6.7.7 / 7.9.6 contains no afferent/efferent/instability constants. A first-party page documenting a *removed* Java "design metrics" set could not be located, so a historical existence is **not** asserted.
- **`rules.sonarsource.com` rule page for S1200** was unreachable from this network; the rule default was taken from SonarSource's own public API instead (`next.sonarqube.com`).
- **Sonargraph Structural Debt Index derivation and any built-in default threshold values**; **Sonargraph `NCCD`** (not present under that name).
- **Lattix default thresholds for its Coupling / Cyclicality / Connectedness metrics** (only System Stability 80 %/50 % is documented).
