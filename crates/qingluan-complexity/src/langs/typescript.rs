//! TypeScript / JavaScript profile (TypeScript, TSX, JSX all share the shape).

use tree_sitter::Node;

use super::{LangProfile, enclosing_names, field_text, named_child_count, qualify};
use crate::kernel::Decision;

const FUNCTION_KINDS: &[&str] = &[
    "function_declaration",
    "generator_function_declaration",
    "function_expression",
    "generator_function",
    "arrow_function",
    "method_definition",
];

const COMMENT_KINDS: &[&str] = &["comment"];

pub(crate) fn profile() -> &'static LangProfile {
    static PROFILE: LangProfile = LangProfile {
        grammar: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        function_kinds: FUNCTION_KINDS,
        logical_kinds: &["binary_expression"],
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

pub(crate) fn tsx_profile() -> &'static LangProfile {
    static PROFILE: LangProfile = LangProfile {
        grammar: || tree_sitter_typescript::LANGUAGE_TSX.into(),
        function_kinds: FUNCTION_KINDS,
        logical_kinds: &["binary_expression"],
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

pub(crate) fn javascript_profile() -> &'static LangProfile {
    static PROFILE: LangProfile = LangProfile {
        grammar: || tree_sitter_javascript::LANGUAGE.into(),
        function_kinds: FUNCTION_KINDS,
        logical_kinds: &["binary_expression"],
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

fn classify(node: Node, _source: &[u8]) -> Option<Decision> {
    match node.kind() {
        "if_statement" => Some(Decision::If {
            else_if: node
                .parent()
                .is_some_and(|parent| parent.kind() == "else_clause"),
        }),
        // `else if` nests the inner `if_statement` in an `else_clause`; that
        // inner if scores, so this wrapper must not add a second point.
        "else_clause" => {
            let contains_if = node
                .named_child(0)
                .is_some_and(|child| child.kind() == "if_statement");
            (!contains_if).then_some(Decision::Else)
        }
        "ternary_expression" => Some(Decision::Ternary),
        "switch_statement" => Some(Decision::Switch),
        "switch_case" => Some(Decision::Case { default: false }),
        "switch_default" => Some(Decision::Case { default: true }),
        "for_statement" | "for_in_statement" | "while_statement" | "do_statement" => {
            Some(Decision::Loop)
        }
        "catch_clause" => Some(Decision::Catch),
        "break_statement" | "continue_statement" => node
            .child_by_field_name("label")
            .is_some()
            .then_some(Decision::Jump),
        "function_declaration"
        | "generator_function_declaration"
        | "function_expression"
        | "generator_function"
        | "arrow_function"
        | "method_definition" => Some(Decision::NestedFunction),
        _ => None,
    }
}

fn name_of(node: Node, source: &[u8]) -> (String, String) {
    let name = field_text(node, "name", source)
        .map(str::to_string)
        .or_else(|| assigned_name(node, source))
        .unwrap_or_else(|| "<anonymous>".into());
    let owners = enclosing_names(node, source, &["class_declaration", "class"], "name");
    let qualified = qualify(&owners, &name, ".");
    (name, qualified)
}

/// Arrow/function expressions have no name of their own; take the name of what
/// they are assigned to, which is what a reader would call them.
fn assigned_name(node: Node, source: &[u8]) -> Option<String> {
    let parent = node.parent()?;
    match parent.kind() {
        "variable_declarator" | "public_field_definition" | "field_definition" => {
            field_text(parent, "name", source).map(str::to_string)
        }
        "pair" => field_text(parent, "key", source).map(str::to_string),
        "assignment_expression" => parent
            .child_by_field_name("left")
            .and_then(|left| left.utf8_text(source).ok())
            .map(|text| text.trim().to_string()),
        _ => None,
    }
}

fn param_count(node: Node, _source: &[u8]) -> u32 {
    if let Some(parameters) = node.child_by_field_name("parameters") {
        return named_child_count(parameters);
    }
    // A parenthesised arrow has `parameters`; `x => x` has a bare `parameter`.
    u32::from(node.child_by_field_name("parameter").is_some())
}
