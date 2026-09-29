# Counting "sequences of binary logical operators" in Cognitive Complexity

Primary-source research for implementing the metric against tree-sitter parse trees.

**Research date:** 2026-09-29.
**Pinned revisions fetched from `master` via raw.githubusercontent.com / GitHub API:**

| Repo | HEAD SHA | Date |
|---|---|---|
| `SonarSource/sonar-java` | `89116880c1bab69b9db51a9984b687e5f050c612` | 2026-09-29 |
| `SonarSource/SonarJS` | `cd783750657846508f4612b0e0c5fb4a649838f5` | 2026-09-29 |

Cached copies of every file quoted below are in `./src/` next to this report.

### Sources actually read

| # | Source | URL |
|---|---|---|
| S1 | sonar-java `CognitiveComplexityVisitor.java` | https://raw.githubusercontent.com/SonarSource/sonar-java/master/java-frontend/src/main/java/org/sonar/java/ast/visitors/CognitiveComplexityVisitor.java |
| S2 | sonar-java `ExpressionUtils.java` (`skipParentheses`) | https://raw.githubusercontent.com/SonarSource/sonar-java/master/java-frontend/src/main/java/org/sonar/java/model/ExpressionUtils.java |
| S3 | sonar-java expected-value test file `CognitiveComplexityMethodCheckMax0.java` | https://raw.githubusercontent.com/SonarSource/sonar-java/master/java-checks/src/test/files/checks/CognitiveComplexityMethodCheckMax0.java |
| S4 | SonarJS `S3776/rule.ts` | https://raw.githubusercontent.com/SonarSource/SonarJS/master/packages/analysis/src/jsts/rules/S3776/rule.ts |
| S5 | SonarJS `S3776/unit.test.ts` | https://raw.githubusercontent.com/SonarSource/SonarJS/master/packages/analysis/src/jsts/rules/S3776/unit.test.ts |
| S6 | SonarJS `helpers/ast.ts` (`isLogicalExpression`) | https://raw.githubusercontent.com/SonarSource/SonarJS/master/packages/analysis/src/jsts/rules/helpers/ast.ts |
| S7 | SonarJS `helpers/jsx.ts` (`getJsxShortCircuitNodes`) | https://raw.githubusercontent.com/SonarSource/SonarJS/master/packages/analysis/src/jsts/rules/helpers/jsx.ts |
| S8 | SonarJS commit `e4f674ce0440f3fc5bb2b2cb7b8339f00721d3b7` (JS-272, 2024-10-08) | https://api.github.com/repos/SonarSource/SonarJS/commits/e4f674ce0440f3fc5bb2b2cb7b8339f00721d3b7 |
| S9 | White paper PDF, "Cognitive Complexity — a new way of measuring understandability", G. Ann Campbell, © SonarSource S.A. 2023 | https://www.sonarsource.com/docs/CognitiveComplexity.pdf |

> Note: the SonarJS paths in the task 404'd only for `unit.test.ts`'s suggested alternatives; the current path is
> `packages/analysis/src/jsts/rules/S3776/{rule.ts,unit.test.ts}` (the repo moved `packages/jsts/...` →
> `packages/analysis/src/jsts/...`). The sonar-java test for the check is
> `java-checks/src/test/files/checks/CognitiveComplexityMethodCheckMax0.java`, driven by
> `java-checks/src/test/java/org/sonar/java/checks/CognitiveComplexityMethodCheckTest.java` with `check.setMax(0)`.

---

## 1. The exact algorithm (both implementations)

Both implementations are the same algorithm with one difference (the SonarJS `||`/`??` exemption, §B).

```text
countLogicalIncrements(expressionRoot):        # one call per method/function body
    total   = 0
    visited = {}                                # Java: `ignored`;  JS: `consideredLogicalExpressions`

    # In-order list of the logical nodes of ONE component. A "component" is the maximal
    # set of logical nodes reachable by descending only through logical children and
    # parentheses.  ANY other node type is a hard boundary.
    flatten(n):
        n = skipParens(n)                       # transparent (Java: PARENTHESIZED_EXPRESSION;
                                                #   JS/ESTree: parens are not AST nodes at all)
        if n is logical(&& or ||):              # Java kind CONDITIONAL_AND / CONDITIONAL_OR
                                                # JS type  LogicalExpression (&&, ||, ??)
            visited.add(n)
            return flatten(n.left) + [n] + flatten(n.right)     # <-- IN-ORDER, node AFTER left
        return []                               # boundary: call, unary !, ternary, index, lambda, ...

    # pre-order walk of the REAL tree (Java: BaseTreeVisitor; JS: ESLint selectors)
    walk(n):
        if n is logical and n not in visited:
            prev = null
            for cur in flatten(n):              # source order left-to-right
                if JS and (cur.op == '||' or cur.op == '??'):
                    pass                        # SonarJS-only exemption (see B); counter NOT incremented
                elif prev == null or prev.op != cur.op:
                    total += 1                  # "+1 for each NEW sequence of like operators"
                prev = cur                      # NB: updated even on the exempt branch
        for child in allChildren(n):            # including call arguments, unary operand, ternary branches
            walk(child)

    walk(expressionRoot)
    return total
```

Java, quoted verbatim (S1, lines 280–306):

```java
  @Override
  public void visitBinaryExpression(BinaryExpressionTree tree) {
    if (tree.is(CONDITIONAL_AND, CONDITIONAL_OR) && !ignored.contains(tree)) {
      List<BinaryExpressionTree> flattenedLogicalExpressions = flattenLogicalExpression(tree).toList();

      BinaryExpressionTree previous = null;
      for (BinaryExpressionTree current : flattenedLogicalExpressions) {
        if (previous == null || !previous.is(current.kind())) {
          increaseComplexityByOne(current.operatorToken());
        }
        previous = current;
      }
    }
    super.visitBinaryExpression(tree);
  }

  private Stream<BinaryExpressionTree> flattenLogicalExpression(ExpressionTree expression) {
    if (expression.is(CONDITIONAL_AND, CONDITIONAL_OR)) {
      ignored.add(expression);

      BinaryExpressionTree binaryExpr = (BinaryExpressionTree) expression;
      ExpressionTree left = ExpressionUtils.skipParentheses(binaryExpr.leftOperand());
      ExpressionTree right = ExpressionUtils.skipParentheses(binaryExpr.rightOperand());

      return Stream.concat(Stream.concat(flattenLogicalExpression(left), Stream.of(binaryExpr)), flattenLogicalExpression(right));
    }
    return Stream.empty();
  }
```

SonarJS, quoted verbatim (S4, lines 355–395):

```ts
    function visitLogicalExpression(logicalExpression: TSESTree.LogicalExpression) {
      const jsxShortCircuitNodes = getJsxShortCircuitNodes(logicalExpression);
      if (jsxShortCircuitNodes != null) {
        for (const node of jsxShortCircuitNodes) {
          consideredLogicalExpressions.add(node);
        }
        return;
      }

      if (!consideredLogicalExpressions.has(logicalExpression)) {
        const flattenedLogicalExpressions = flattenLogicalExpression(logicalExpression);

        let previous: TSESTree.LogicalExpression | undefined;
        for (const current of flattenedLogicalExpressions) {
          if (
            current.operator !== '||' &&
            current.operator !== '??' &&
            previous?.operator !== current.operator
          ) {
            const operatorTokenLoc = getFirstTokenAfter(current.left, context as unknown as RuleContext)!.loc;
            addComplexity(operatorTokenLoc);
          }
          previous = current;
        }
      }
    }

    function flattenLogicalExpression(node: TSESTree.Node): TSESTree.LogicalExpression[] {
      if (isLogicalExpression(node)) {
        consideredLogicalExpressions.add(node);
        return [
          ...flattenLogicalExpression(node.left),
          node,
          ...flattenLogicalExpression(node.right),
        ];
      }
      return [];
    }
```

`isLogicalExpression` (S6, line 160) is `node?.type === 'LogicalExpression'` — i.e. SonarJS's flatten boundary test is purely structural, and `??` **is** a `LogicalExpression` in ESTree.

Java's `skipParentheses` (S2, lines 142–148):

```java
  public static ExpressionTree skipParentheses(ExpressionTree tree) {
    ExpressionTree result = tree;
    while (result.is(Tree.Kind.PARENTHESIZED_EXPRESSION)) {
      result = ((ParenthesizedTree) result).expression();
    }
    return result;
  }
```

An independent re-implementation of the above pseudocode (`.scratch/complexity/research/src/simulate.py`) reproduces **every** hard-coded expected value in both projects' test files (see §C.3), which is how the table in §C was derived.

---

## A. What triggers an increment; what the comparison is against; what resets it

**Trigger:** entering a binary logical node (`&&`/`||` in Java; `&&`/`||`/`??` in JS) that has not already been consumed as part of an enclosing logical component, then flattening its whole component and emitting `+1` for each element whose operator differs from the **previous element of the flattened list**.

**Comparison target — neither "last operator seen anywhere in the tree" nor "the immediate parent's operator".** It is the **immediately preceding logical operator in the in-order flatten of the current logical component**. The parent is irrelevant because `flatten` never consults a parent. Two consequences:

- `(a || b) && (c || d)` flattens to `[||, &&, ||]`: the `||` of `(a || b)` is compared to *nothing* (it is first), and the `&&` is compared to the `||` **inside its left operand**, not to a parent. Java gives **3**, not 2.
- `a || b && c || d` (parses as `(a || (b && c)) || d`) flattens to `[||, &&, ||]` → **3**. `flatten` recurses into `b && c` *before* appending the enclosing `||` (line 303: `concat(concat(flatten(left), Stream.of(binaryExpr)), flatten(right))`), so the emitted order is source order.

**What resets `previous` (i.e. starts a new component / a new sequence):**

| Construct | Resets? | Why / evidence |
|---|---|---|
| Parenthesized expressions `( ... )` | **No** | Java `skipParentheses` + recursion (S1 L300–301, S2 L142–148); ESTree has no paren node at all. Java test comment: `|| k)) // +1 - parentheses completely ignored` (S3 L97) and `if (a || (b || c)) {}` costs 1 logical (S3 L74–76). |
| Unary `!` | **Yes** | `!(b && c)` is `LOGICAL_COMPLEMENT`, not `CONDITIONAL_AND/OR`, so `flattenLogicalExpression` returns `Stream.empty()` (S1 L305); the inner `b && c` is then reached by `super.visitBinaryExpression` as a fresh component. Confirmed by `a && !(b && c)` → 2 in both test suites (S3 L262–264, S5 L221). |
| Leaving into another expression kind — method-call argument, array element, ternary branch, index expression, assignment RHS | **Yes** | Non-logical operands return `Stream.empty()`; the sub-expression is visited later as its own component. `return a && b || foo(b && c);` → 3 (S3 L15–17), `foo(1 && 2 || 3 && 4)` emits `+1` at both `&&` but **no** `||` in SonarJS (S5 L201–210), i.e. the call boundary splits the flatten. |
| Non-logical sibling | **Yes** | Same mechanism: `flatten` returns `[]` on that operand, the sibling is a separate subtree. |
| Entering a nested function/method | **Yes** | Java starts an entirely new `CognitiveComplexityVisitor` per method with `complexity = 0, nesting = 1` (S1 L94–102); a lambda body is traversed by the same visitor but is a separate flatten component and only bumps `nesting` (S1 L272–277). SonarJS attributes complexity per function via `functionOwnComplexity` (S4 L214–215, L422–426); `flatten` never crosses a function boundary. |

Additionally, Java's `ignored` set (S1 L281) / SonarJS's `consideredLogicalExpressions` (S4 L364) make the **outermost** logical node of a component the only one that flattens and counts; descendants are visited but produce no increment.

---

## B. The SonarJS deviation: are `||` and `??` still exempt?

**Yes — as of SonarJS `master` @ `cd7837506` the exemption is still present and is unconditional.** Exact condition (S4 L369–373):

```ts
current.operator !== '||' &&
current.operator !== '??' &&
previous?.operator !== current.operator
```

So an operator token increments **iff** its operator is neither `'||'` nor `'??'` **and** it differs from the previous operator in the flattened list. `||` and `??` elements are skipped for the increment but **still assigned to `previous`** (S4 L381), so they act as *sequence separators* for a following `&&`: `a && b || c && d` → `[&&, ||, &&]` → `&&`(+1), `||`(skip, but `previous = ||`), `&&`(+1) = **2**.

**Provenance (primary):** commit `e4f674ce0440f3fc5bb2b2cb7b8339f00721d3b7`, 2024-10-08, "JS-272 Improve S3776 (`cognitive-complexity`): Do not increase complexity on short-circuiting and null coalescing (#4862)" (S8). Its diff on `rule.ts` replaced

```diff
-          if (!previous || previous.operator !== current.operator) {
+          if (
+            current.operator !== '||' &&
+            current.operator !== '??' &&
+            (!previous || previous.operator !== current.operator)
+          ) {
```

and removed the older `isDefaultValuePattern(...)` exemption (which had only exempted `a || literal` / `a = a || literal` default-value patterns). The same commit changed the rule description in `S3776.html` to:

> "Cognitive complexity calculations exclude logical expressions using the `||` and `??` operators."

**White-paper basis — partial only.** The white paper *does* say null-coalescing operators are ignored (S9 lines 169–174: "Cognitive Complexity also ignores the null-coalescing operators found in many languages ... For that reason, Cognitive Complexity ignores null-coalescing operators"), but it **explicitly counts `||` sequences** (S9 lines 234–237: "it does increment for all sequences of binary boolean operators such as those in variable assignments, method invocations, and return statements"; and its annotated example at S9 lines 258–261 gives `|| d || e // +1`). So the `??` exemption agrees with the paper; **the `||` exemption contradicts it**. SonarJS is knowingly divergent here (JS-272 was a product decision, not a spec correction).

---

## C. Expression → increment table

### C.1 Assumed tree shape

All seven parse with C/JS precedence (`&&` binds tighter than `||`; both left-associative; `!` binds tighter than both; parentheses override), which is also Java's and ESTree's precedence. The **pure binary shape** is shown; parens are drawn only where written, but are semantic no-ops for this metric.

| Expression | Parse tree (`op(L,R)`) | Flatten order of operators |
|---|---|---|
| `a && b && c` | `&&(&&(a,b), c)` | `&&, &&` |
| `a || b || c || d` | `\|\|(\|\|(\|\|(a,b),c), d)` | `\|\|, \|\|, \|\|` |
| `a || b && c || d` | `\|\|(\|\|(a, &&(b,c)), d)` | `\|\|, &&, \|\|` |
| `a && b || c && d` | `\|\|(&&(a,b), &&(c,d))` | `&&, \|\|, &&` |
| `a && !(b && c)` | `&&(a, !(&&(b,c)))` | two components: `&&` \| `&&` |
| `(a \|\| b) && (c \|\| d)` | `&&(\|\|(a,b), \|\|(c,d))` | `\|\|, &&, \|\|` |
| `f(a && b) \|\| g(c \|\| d)` | `\|\|(call(f,[&&(a,b)]), call(g,[\|\|(c,d)]))` | three components: `\|\|` \| `&&` \| `\|\|` |

### C.2 The metric contribution from the logical operators alone

| Expression | sonar-java | current SonarJS `rule.ts` | White paper (spec) |
|---|---:|---:|---:|
| `a && b && c` | **1** | **1** | **1** (verified by the paper's `a && b` / `a && b && c && d` pair, S9 L221–222, plus S9 L234–237) |
| `a \|\| b \|\| c \|\| d` | **1** | **0** | **1** (verified by the `a \|\| b` / `a \|\| b \|\| c \|\| d` pair, S9 L223–224) |
| `a \|\| b && c \|\| d` | **3** | **1** | **3** (inferred: this is the paper's "marked difference" line, S9 L225–226; the paper prints no per-operator annotation, but the same run-length rule is applied explicitly to `&& b && c \|\| d \|\| e && f` → 3, S9 L258–261) |
| `a && b \|\| c && d` | **3** | **2** | **3** (inferred from the same run-length rule; not printed as a number) |
| `a && !(b && c)` | **2** | **2** | **2** (verified verbatim: `&& // +1` and `!(b && c)) // +1`, S9 L262–264) |
| `(a \|\| b) && (c \|\| d)` | **3** | **1** | **3** ⚠ inferred — paper is silent on parentheses; value assumes parens are transparent as in sonar-java |
| `f(a && b) \|\| g(c \|\| d)` | **3** | **1** | **3** ⚠ inferred — paper says method-invocation expressions count (S9 L234–237) but prints no example; value assumes each call argument is its own component |

`??` is Java-inapplicable and white-paper-"ignored"; SonarJS skips it. For example `a ?? b ?? c` → SonarJS **0**, Java N/A, paper **0**.

### C.3 Every value above is checked against the projects' own tests

`.scratch/complexity/research/src/simulate.py` implements the pseudocode and matches all of these primary-source expectations:

sonar-java `CognitiveComplexityMethodCheckMax0.java` (S3; numbers are whole-method totals = logical + `if`):
`return a && b || foo(b && c);` → 3 (L15–17) · `return a && (b || c) || d;` → 2 (L18–20) ·
`if (a && b || c || d)` → 3 (L21–23) · `if (a && b || c && d || e)` → 5 (L24–26) ·
`if (a || b && c || d && e)` → 5 (L27–29) · `if (a && b && c || d || e)` → 3 (L30–32) ·
`if (a && b && c && d && e)` → 2 (L36–38) · `if (a || b || c || d || e)` → 2 (L39–41) ·
`if (a && b && c || d || e && f)` → 4 (L42–44) · `if (a || (b || c))` → 2 (L74–76) ·
`extraConditions12` → 7 (L78–102) · `a || b || c` inside `while` → the `// 1 (for ||)` comment (L226).

SonarJS `unit.test.ts` (S5): `foo(1 && 2 && 3 && 4)` +1; `foo((1 && 2) && (3 && 4))` +1; `foo(((1 && 2) && 3) && 4)` +1; `foo(1 && (2 && (3 && 4)))` +1; `foo(1 || 2 || 3 || 4)` +0; `foo(1 && 2 || 3 || 4)` +1; `foo(1 && 2 || 3 && 4)` +2; `foo(1 && 2 && !(3 && 4))` +2 (L213–222); and the secondary-location assertions for `foo(1 && 2 || 3 && 4)` list only the two `&&` tokens (L201–210).

---

## D. Which nodes are visited, and in what order

**Counting order inside one component = in-order over the logical tree, which equals source order.** Java builds it with

```java
return Stream.concat(Stream.concat(flattenLogicalExpression(left), Stream.of(binaryExpr)), flattenLogicalExpression(right));
```
(S1 L303) — left subtree first, then the node itself, then the right subtree. SonarJS is identical:

```ts
return [ ...flattenLogicalExpression(node.left), node, ...flattenLogicalExpression(node.right) ];
```
(S4 L388–392).

**Traversal order over the whole tree = pre-order.** Java is driven by `methodTree.accept(visitor)` (S1 L97) on a `BaseTreeVisitor`, and after counting a component it continues with `super.visitBinaryExpression(tree)` (S1 L292), i.e. left operand then right operand. Because the outermost logical node of a component is visited first (pre-order) and its `flatten` adds every logical descendant to `ignored`, the descendants are still visited but skip their own count. SonarJS is an ESLint visitor: the `LogicalExpression` handler runs when the node is entered (S4 L186–188), and `consideredLogicalExpressions` (S4 L364) makes nested logicals no-ops.

**Node types involved:**
- sonar-java: `BinaryExpressionTree` with kind `CONDITIONAL_AND` / `CONDITIONAL_OR`; parentheses are `PARENTHESIZED_EXPRESSION` nodes, explicitly skipped (S2).
- SonarJS/ESTree: `LogicalExpression` — which covers `&&`, `||`, **and `??`**; parentheses have no node, so they are transparent by construction. `?.` (optional chaining) is *not* a `LogicalExpression` and never counts.

The counting order only matters because increments are run-length: the `k`-th operator in the flattened sequence is compared to operator `k-1` of the same component, never to the parse parent.

---

## E. `else if` and `switch` — things that surprise naive implementations

**`else if` = flat `+1`, never nesting-incremented.** Java (S1 L168–188):

```java
    boolean elseStatementNotIF = tree.elseStatement() != null && !tree.elseStatement().is(IF_STATEMENT);
    if (elseStatementNotIF) {
      increaseComplexityByOne(tree.elseKeyword());
      nesting++;
    } else if (tree.elseStatement() != null) {
      // else statement is an if, visiting it will increase complexity by nesting so by one only.
      ignoreNesting = true;
      complexity -= nesting - 1;
    }
    scan(tree.elseStatement());
```

The `complexity -= nesting - 1` + `ignoreNesting = true` pair cancels the nesting increment the nested `if` would otherwise receive, so an `else if` contributes exactly `+1` regardless of depth. SonarJS detects it structurally (S4 L263–271): `if (isIfStatement(parent) && parent.alternate === ifStatement) addComplexity(ifLoc); else addStructuralComplexity(ifLoc);` — `addComplexity` is always `1` (S4 L422–426), `addStructuralComplexity` is `nesting + 1` (S4 L397–398).

**`else` = flat `+1` but raises the nesting of its body.** Java `increaseComplexityByOne(tree.elseKeyword()); nesting++;` (S1 L177–178, decremented at L185–187). SonarJS `addComplexity(elseTokenLoc); nestingNodes.add(ifStatement.alternate);` (S4 L279–286). The white paper's Appendix B classifies `else`/`else if` as *hybrid* increments: they add `+1` but also raise nesting (S9 lines 182–185 and B2 at L520, while `else`/`else if` are absent from the B3 nesting-increment list at L527–533).

**`switch` counts exactly once for the whole statement, no matter how many `case` labels; `default` adds nothing.** Java (S1 L241–247):

```java
  @Override
  public void visitSwitchStatement(SwitchStatementTree tree) {
    increaseComplexityByNesting(tree.switchKeyword());
    nesting++;
    super.visitSwitchStatement(tree);
    nesting--;
  }
```

There is no per-`case` handler and no `default` handler at all. SonarJS (S4 L294–301) does one `addStructuralComplexity` and adds every `switchCase` to `nestingNodes`. Consequences a port must replicate:
- the switch's own `+1` receives the *current* nesting increment;
- everything inside every `case` (including the **first** case) is evaluated at `nesting + 1`, so the first `if` in a `case` yields `+2`;
- `default:` is just another `SwitchCase` node — no increment of its own.

Verified by S3 L47–72: `switch(foo)` is `//+1`, and inside its case an `if (...)` is `//+2 (nesting=1)` while a further nested `if (a && b && c || d)` is `//+5 (nesting=2)` (2 nesting + 3 logical), and the `else` is `//+1`. The white paper agrees: "A switch and all its cases combined incurs a single structural increment" (S9 L203–204). (Do **not** be misled by the paper's `+1`-per-`case` example at S9 L127–138: that example is the *Cyclomatic* Complexity illustration in the "An illustration of the problem" section, explicitly totalled as "Cyclomatic Complexity 4".)

**Other surprises for a tree-sitter port:**
- Java: methods of **anonymous** classes and of **local** classes are excluded from their own score; a local class's method complexity is folded into the enclosing method (S1 L133–144 `shouldAnalyzeMethod`, test `localClasses` at S3 L305–311). Lambdas do not get a score of their own but do raise `nesting` (S1 L272–277).
- SonarJS has a **JSX-only carve-out**: if a `LogicalExpression` sits directly in a `JSXExpressionContainer` and its whole logical subtree uses a single operator and contains no `ConditionalExpression`, `getJsxShortCircuitNodes` returns the node list and `visitLogicalExpression` returns before counting (S4 L356–362; S7 L22–49). Thus `{ obj.x && obj.y && obj.z && <strong>Welcome</strong> }` is **0**, not 1 (S5 valid cases L102–112). This rules out a naive "count every `&&` run" port for `.tsx`/`.jsx`.
- In SonarJS, `a || []`, `a = a || []`, `c ?? ''` are **0** (S5 L131–148) — but that is just a consequence of the general `||`/`??` exemption, not a separate default-value pattern (that separate pattern was deleted by JS-272, S8).

---

## Explicitly verified vs. uncertain

**Verified from primary source (code read, line-quoted above):** the Java algorithm (S1), `skipParentheses` (S2), the Java expected values (S3), the SonarJS algorithm including the `||`/`??` exemption (S4), the SonarJS expected values (S5), `isLogicalExpression` (S6), the JSX carve-out (S7), the JS-272 commit and its diff (S8), and the white-paper wording (S9, extracted directly from the official PDF).

**Uncertain / inferred (flagged in §C):** the white paper's numeric value for `(a || b) && (c || d)` and for `f(a && b) || g(c || d)` is not printed in the paper — the table's `3` is inferred from the stated run-length rule plus the sonar-java reference implementation; the paper never addresses parentheses explicitly. Likewise, the paper's exact treatment of a unary `!` as a component boundary is only inferable from its `!(b && c)` example, which does match both implementations.

**Not checked:** sonar-java `master` as of this date contains no `??` operator, so no Java comparison for `??` is possible; and no other language port (sonar-php, sonar-python, sonar-csharp, …) was inspected — they may differ.
