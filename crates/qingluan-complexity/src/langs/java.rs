//! Java profile.
//!
//! tree-sitter-java has no `else_clause` node: a plain `else` is simply the
//! `alternative` child of its `if_statement`, and an `else if` is an
//! `if_statement` sitting in that same `alternative` slot. The kind alone
//! therefore cannot tell the three apart, so both `if_statement` and the
//! catch-all arm ask [`is_alternative_of_if`].

use tree_sitter::Node;

use super::{LangProfile, enclosing_names, field_text, named_child_count, qualify};
use crate::kernel::Decision;

const FUNCTION_KINDS: &[&str] = &["method_declaration", "constructor_declaration"];
const LOGICAL_KINDS: &[&str] = &["binary_expression"];
const COMMENT_KINDS: &[&str] = &["line_comment", "block_comment"];

pub(crate) fn profile() -> &'static LangProfile {
    static PROFILE: LangProfile = LangProfile {
        grammar: || tree_sitter_java::LANGUAGE.into(),
        function_kinds: FUNCTION_KINDS,
        logical_kinds: LOGICAL_KINDS,
        operator_field: "operator",
        left_field: "left",
        right_field: "right",
        paren_kind: "parenthesized_expression",
        comment_kinds: COMMENT_KINDS,
        name_of,
        param_count,
        classify,
    };
    &PROFILE
}

fn classify(node: Node, source: &[u8]) -> Option<Decision> {
    match node.kind() {
        "if_statement" => Some(Decision::If {
            else_if: is_alternative_of_if(node),
        }),
        "ternary_expression" => Some(Decision::Ternary),
        "switch_expression" => Some(Decision::Switch),
        // `case A, B:` is a single label node, so it costs one point; the
        // empty `default` label (in both `:` and `->` form) is free.
        "switch_label" => Some(Decision::Case {
            default: is_default_label(node, source),
        }),
        "for_statement" | "enhanced_for_statement" | "while_statement" | "do_statement" => {
            Some(Decision::Loop)
        }
        "catch_clause" => Some(Decision::Catch),
        // Only a labelled `break`/`continue` jumps; the grammar exposes the
        // label as an unnamed-field named child.
        "break_statement" | "continue_statement" => {
            node.named_child(0).is_some().then_some(Decision::Jump)
        }
        // A lambda hands its decisions to the enclosing method; so does a
        // method of a nested or anonymous class.
        "lambda_expression" | "method_declaration" | "constructor_declaration" => {
            Some(Decision::NestedFunction)
        }
        // Any other node in the `alternative` slot is the plain `else` branch.
        // Checking it last keeps a naked `else switch (...)` or `else while
        // (...)` classified as what it is.
        _ => is_alternative_of_if(node).then_some(Decision::Else),
    }
}

/// Whether `node` is the `alternative` child of an enclosing `if_statement`.
///
/// True for the inner `if_statement` of an `else if` (which stays flat) and for
/// the body of a plain `else` (which scores as [`Decision::Else`]).
fn is_alternative_of_if(node: Node) -> bool {
    node.parent().is_some_and(|parent| {
        parent.kind() == "if_statement"
            && parent
                .child_by_field_name("alternative")
                .is_some_and(|alternative| alternative.id() == node.id())
    })
}

/// `default:` and `default ->` both parse as a `switch_label` holding no
/// pattern of its own.
fn is_default_label(node: Node, source: &[u8]) -> bool {
    node.utf8_text(source)
        .is_ok_and(|text| text.trim_start().starts_with("default"))
}

fn name_of(node: Node, source: &[u8]) -> (String, String) {
    let name = field_text(node, "name", source)
        .unwrap_or("<anonymous>")
        .to_string();
    let owners = enclosing_names(
        node,
        source,
        &[
            "class_declaration",
            "record_declaration",
            "interface_declaration",
            "enum_declaration",
        ],
        "name",
    );
    let qualified = qualify(&owners, &name, ".");
    (name, qualified)
}

fn param_count(node: Node, _source: &[u8]) -> u32 {
    node.child_by_field_name("parameters")
        .map(named_child_count)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use crate::{Language, analyze_source};

    fn java(source: &str) -> Vec<crate::FunctionMetrics> {
        analyze_source(Language::Java, source.as_bytes()).functions
    }

    #[test]
    fn method_inside_a_class_is_named_and_qualified() {
        let source = r#"
class Calc {
    int add(int a, int b) {
        return a + b;
    }
}
"#;
        let found = java(source);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "add");
        assert_eq!(found[0].qualified_name, "Calc.add");
        assert_eq!(found[0].metrics.params, 2);
        // only the implicit path: `+` is not a logical operator
        assert_eq!(found[0].metrics.cc, 1);
        assert_eq!(found[0].metrics.cognitive, 0);
    }

    #[test]
    fn else_if_chains_stay_flat() {
        let source = r#"
class Grade {
    String grade(int score) {
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
}
"#;
        let found = java(source);
        assert_eq!(found.len(), 1);
        // one if + two else-ifs + the else, the else is free for CC
        assert_eq!(found[0].metrics.cc, 4);
        // if(+1) + else-if(+1) + else-if(+1) + else(+1), none nested
        assert_eq!(found[0].metrics.cognitive, 4);
        assert_eq!(found[0].metrics.max_nesting, 1);
    }

    #[test]
    fn switch_counts_once_for_cognitive_but_every_case_for_cc() {
        let source = r#"
class Picker {
    String pick(int value) {
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
}
"#;
        let found = java(source);
        // three cases, the default is free
        assert_eq!(found[0].metrics.cc, 4);
        // a switch is one structural increment however many cases it has
        assert_eq!(found[0].metrics.cognitive, 1);
    }

    #[test]
    fn every_catch_is_a_cc_point_and_a_structural_increment() {
        let source = r#"
class Guard {
    void run() {
        try {
            work();
        } catch (IllegalStateException e) {
            recover();
        } catch (RuntimeException e) {
            recover();
        }
    }
}
"#;
        let found = java(source);
        // two catches + the implicit path; `try`/`finally` are free
        assert_eq!(found[0].metrics.cc, 3);
        // each catch is entered at the same depth: +1 and +1
        assert_eq!(found[0].metrics.cognitive, 2);
        assert_eq!(found[0].metrics.max_nesting, 1);
    }

    #[test]
    fn a_lambda_is_part_of_the_enclosing_method() {
        let source = r#"
import java.util.function.IntPredicate;

class Lambdas {
    IntPredicate make(boolean flag) {
        return value -> {
            if (flag) {
                return value > 0;
            }
            return false;
        };
    }
}
"#;
        let found = java(source);
        // the lambda is not a row of its own
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "make");
        // the lambda's `if` still counts, one level deeper for nesting
        assert_eq!(found[0].metrics.cc, 2);
        assert_eq!(found[0].metrics.cognitive, 2);
        assert_eq!(found[0].metrics.max_nesting, 2);
    }

    #[test]
    fn logical_operator_sequences_follow_the_white_paper() {
        let source = r#"
class Logic {
    boolean check(boolean a, boolean b, boolean c, boolean d) {
        boolean one = a && b && c;
        boolean two = a || b && c || d;
        return one;
    }
}
"#;
        let found = java(source);
        // `a && b && c` = 1 run, `a || b && c || d` = 3 runs
        assert_eq!(found[0].metrics.cognitive, 4);
        // every `&&`/`||` node is one CC point: 2 + 3, plus the implicit path
        assert_eq!(found[0].metrics.cc, 6);
    }

    #[test]
    fn a_constructor_is_named_after_its_class() {
        let source = r#"
class Widget {
    private final int size;

    Widget(int size) {
        this.size = size;
    }
}
"#;
        let found = java(source);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Widget");
        // the generic owner rule applies: class name joined to the member name
        assert_eq!(found[0].qualified_name, "Widget.Widget");
        assert_eq!(found[0].metrics.params, 1);
        assert_eq!(found[0].metrics.cc, 1);
    }
}
