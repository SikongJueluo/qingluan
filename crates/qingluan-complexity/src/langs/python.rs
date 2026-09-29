//! Python profile.

use tree_sitter::Node;

use super::{LangProfile, enclosing_names, field_text, named_child_count, qualify};
use crate::kernel::Decision;

pub(crate) fn profile() -> &'static LangProfile {
    static PROFILE: LangProfile = LangProfile {
        grammar: || tree_sitter_python::LANGUAGE.into(),
        function_kinds: &["function_definition"],
        logical_kinds: &["boolean_operator"],
        operator_field: "operator",
        left_field: "left",
        right_field: "right",
        paren_kind: "parenthesized_expression",
        comment_kinds: &["comment"],
        name_of,
        param_count,
        classify,
    };
    &PROFILE
}

fn classify(node: Node, source: &[u8]) -> Option<Decision> {
    match node.kind() {
        "if_statement" => Some(Decision::If { else_if: false }),
        // An `elif` is a hybrid increment: flat, no nesting penalty.
        "elif_clause" => Some(Decision::If { else_if: true }),
        // `for ... else` / `while ... else` / `try ... else` are not branches
        // of an if and score nothing.
        "else_clause" => node
            .parent()
            .is_some_and(|parent| parent.kind() == "if_statement")
            .then_some(Decision::Else),
        "conditional_expression" => Some(Decision::Ternary),
        "for_statement" | "while_statement" => Some(Decision::Loop),
        "except_clause" => Some(Decision::Catch),
        "match_statement" => Some(Decision::Switch),
        "case_clause" => Some(Decision::Case {
            default: is_wildcard_case(node, source),
        }),
        "lambda" | "function_definition" => Some(Decision::NestedFunction),
        _ => None,
    }
}

/// `case _:` is the catch-all; it is not a decision point.
fn is_wildcard_case(node: Node, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(|child| child.utf8_text(source).is_ok_and(|text| text.trim() == "_"))
}

fn name_of(node: Node, source: &[u8]) -> (String, String) {
    let name = field_text(node, "name", source)
        .unwrap_or("<anonymous>")
        .to_string();
    let owners = enclosing_names(node, source, &["class_definition"], "name");
    let qualified = qualify(&owners, &name, ".");
    (name, qualified)
}

fn param_count(node: Node, _source: &[u8]) -> u32 {
    node.child_by_field_name("parameters")
        .map(named_child_count)
        .unwrap_or(0)
}
