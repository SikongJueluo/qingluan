//! Golden tests.
//!
//! Every expected number here is hand-checked against a primary source: the
//! ESLint/McCabe example in `docs/research/code-complexity-metrics.md` §1.4,
//! the SonarSource worked example in §2.4, and the white paper's logical
//! operator examples (verified against sonar-java's
//! `CognitiveComplexityVisitor`).

use qingluan_complexity::{Language, analyze_source};

fn functions(language: Language, source: &str) -> Vec<qingluan_complexity::FunctionMetrics> {
    analyze_source(language, source.as_bytes()).functions
}

/// §1.4: six decision points, so CC1 = 7.
const ESLINT_CLASSIFY: &str = r#"
function classify(a, b) {
  if (a > 0 && b > 0) return 1;
  if (a < 0) return 2;
  for (let i = 0; i < 3; i++) {
    if (b === i) return 3;
  }
  return a > b ? 4 : 5;
}
"#;

/// §2.4: sonarjs reports 15; the same function has CC 8.
const SONAR_CLASSIFY: &str = r#"
function classify(user, items) {
  let result = "";
  if (user.isActive && user.verified) {
    for (const it of items) {
      if (it.price > 100 && it.stock > 0) {
        result += "A";
      } else {
        result += it.banned ? "B" : "C";
      }
    }
  } else if (user.isAdmin) {
    result = "admin";
  } else {
    result = "guest";
  }
  return result;
}
"#;

#[test]
fn javascript_cyclomatic_matches_the_eslint_example() {
    let found = functions(Language::JavaScript, ESLINT_CLASSIFY);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "classify");
    assert_eq!(found[0].metrics.cc, 7);
    assert_eq!(found[0].metrics.params, 2);
}

#[test]
fn javascript_cognitive_matches_the_sonar_worked_example() {
    let found = functions(Language::JavaScript, SONAR_CLASSIFY);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].metrics.cognitive, 15);
    assert_eq!(found[0].metrics.cc, 8);
    assert_eq!(found[0].metrics.max_nesting, 4);
}

#[test]
fn logical_operator_sequences_follow_the_white_paper() {
    // sonar-java's flatten-then-compare-runs rule, which agrees with every
    // published white paper example.
    let source = r#"
function f(a, b, c, d) {
  let one = a && b && c;
  let two = a || b && c || d;
  let three = a && !(b && c);
  let four = a || b || c || d;
  return one + two + three + four;
}
"#;
    let found = functions(Language::JavaScript, source);
    // 1 + 3 + 2 + 1
    assert_eq!(found[0].metrics.cognitive, 7);
    // every && and || counts once for CC1: 2 + 3 + 2 + 3
    assert_eq!(found[0].metrics.cc, 1 + 10);
}

#[test]
fn rust_function_boundaries_names_and_decisions() {
    let source = r#"
fn plain(a: u32, b: u32) -> u32 {
    if a > 0 && b > 0 { 1 } else if a < 0 { 2 } else { 3 }
}

struct Service;

impl Service {
    fn method(&self, x: u32) -> u32 {
        match x {
            0 => 0,
            1 => 1,
            _ => 2,
        }
    }
}
"#;
    let found = functions(Language::Rust, source);
    assert_eq!(found.len(), 2);

    let plain = &found[0];
    assert_eq!(plain.name, "plain");
    assert_eq!(plain.qualified_name, "plain");
    assert_eq!(plain.metrics.params, 2);
    // if + `&&` + else-if
    assert_eq!(plain.metrics.cc, 4);
    // if(+1) + &&(+1) + else-if(+1) + else(+1)
    assert_eq!(plain.metrics.cognitive, 4);

    let method = &found[1];
    assert_eq!(method.name, "method");
    assert_eq!(method.qualified_name, "Service::method");
    // `self` is not a parameter
    assert_eq!(method.metrics.params, 1);
    // two non-wildcard arms
    assert_eq!(method.metrics.cc, 3);
    // a match is one structural increment in total
    assert_eq!(method.metrics.cognitive, 1);
    assert_eq!(method.start_line, 9);
}

#[test]
fn nested_functions_count_into_the_enclosing_function() {
    let source = r#"
fn outer(items: &[u32]) -> usize {
    let positives = items.iter().filter(|item| **item > 0).count();
    if positives > 0 { positives } else { 0 }
}
"#;
    let found = functions(Language::Rust, source);
    // the closure is not a row of its own
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "outer");
    assert_eq!(found[0].metrics.cc, 2);
    assert_eq!(found[0].metrics.cognitive, 2);
}

#[test]
fn python_lambda_is_part_of_the_enclosing_function() {
    let source = r#"
def outer(items):
    positives = [x for x in items if x > 0]
    check = lambda x: x > 0
    if positives and check(1):
        return len(positives)
    return 0
"#;
    let found = functions(Language::Python, source);
    assert_eq!(found.len(), 1);
    // if + `and`
    assert_eq!(found[0].metrics.cc, 3);
    // if(+1) + and(+1)
    assert_eq!(found[0].metrics.cognitive, 2);
}

#[test]
fn python_else_is_only_a_branch_of_an_if() {
    let source = r#"
def loop(items):
    for item in items:
        if item:
            return item
    else:
        return None
"#;
    let found = functions(Language::Python, source);
    // for + if, the for-else is not a branch
    assert_eq!(found[0].metrics.cc, 3);
    // for(+1) + if(+2, nested)
    assert_eq!(found[0].metrics.cognitive, 3);
}

#[test]
fn typescript_else_if_chains_stay_flat() {
    let source = r#"
export function grade(score: number): string {
  if (score > 90) {
    return "A";
  } else if (score > 80) {
    return "B";
  } else if (score > 70) {
    return "C";
  } else {
    return "F";
  }
}
"#;
    let found = functions(Language::TypeScript, source);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].metrics.cc, 4);
    // one if + two else-ifs + one else, none of them nested
    assert_eq!(found[0].metrics.cognitive, 4);
    assert_eq!(found[0].metrics.max_nesting, 1);
}

#[test]
fn switch_counts_once_for_cognitive_but_every_case_for_cc() {
    let source = r#"
function pick(value) {
  switch (value) {
    case 1:
      return "one";
    case 2:
      return "two";
    case 3:
      return "three";
    default:
      return "many";
  }
}
"#;
    let found = functions(Language::JavaScript, source);
    // three cases, the default is free
    assert_eq!(found[0].metrics.cc, 4);
    assert_eq!(found[0].metrics.cognitive, 1);
}

#[test]
fn nloc_ignores_blank_lines_and_comments() {
    let source = "fn f() -> u32 {\n    // a comment\n\n    let x = 1;\n    x\n}\n";
    let found = functions(Language::Rust, source);
    // code lines: fn, let, x, }
    assert_eq!(found[0].metrics.nloc, 4);
}

#[test]
fn a_file_without_functions_gets_a_module_entry() {
    let source = "print(\"hello\")\nif True:\n    print(\"world\")\n";
    let found = functions(Language::Python, source);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "<module>");
    assert_eq!(found[0].metrics.cc, 2);
}

#[test]
fn blank_input_yields_nothing() {
    assert!(functions(Language::Rust, "").is_empty());
    assert!(functions(Language::Python, "\n\n").is_empty());
}

#[test]
fn broken_syntax_still_recovers_and_never_panics() {
    let source = "fn broken(a: u32) -> u32 {\n    if a > 0 {\n        let x = a &&;\n";
    let found = functions(Language::Rust, source);
    assert!(!found.is_empty());
    assert!(found[0].metrics.cc >= 1);
    assert!(found[0].metrics.nloc >= 1);
}

#[test]
fn file_nloc_uses_the_same_rules_over_the_whole_buffer() {
    let source = "// header\n\nfn a() { let x = 1; }\n\nstruct S;\n/* spans\n   lines */\nfn b() { let y = 2; }\n";
    let analysis = analyze_source(Language::Rust, source.as_bytes());
    // The two function lines plus one code line outside any function; the
    // header comment, block comment and blanks never count.
    assert_eq!(analysis.nloc, 3);
    assert_eq!(analysis.functions.len(), 2);
}
