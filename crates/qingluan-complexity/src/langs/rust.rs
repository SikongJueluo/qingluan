//! Rust profile.

use tree_sitter::Node;

use super::{LangProfile, field_text, qualify};
use crate::kernel::Decision;

pub(crate) fn profile() -> &'static LangProfile {
    static PROFILE: LangProfile = LangProfile {
        grammar: || tree_sitter_rust::LANGUAGE.into(),
        function_kinds: &["function_item"],
        logical_kinds: &["binary_expression"],
        operator_field: "operator",
        left_field: "left",
        right_field: "right",
        paren_kind: "parenthesized_expression",
        comment_kinds: &["line_comment", "block_comment"],
        name_of,
        param_count,
        classify,
    };
    &PROFILE
}

fn classify(node: Node, source: &[u8]) -> Option<Decision> {
    match node.kind() {
        "if_expression" => Some(Decision::If {
            else_if: node
                .parent()
                .is_some_and(|parent| parent.kind() == "else_clause"),
        }),
        // `else if` is an if inside the else clause: the inner if scores, this
        // wrapper must not add a second point.
        "else_clause" => {
            let contains_if = node
                .named_child(0)
                .is_some_and(|child| child.kind() == "if_expression");
            (!contains_if).then_some(Decision::Else)
        }
        "match_expression" => Some(Decision::Switch),
        // A bare `_` parses as an empty `match_pattern`: the catch-all arm is
        // free, like a `default` label.
        "match_arm" => Some(Decision::Case {
            default: node.child_by_field_name("pattern").is_some_and(|pattern| {
                pattern
                    .utf8_text(source)
                    .is_ok_and(|text| text.trim() == "_")
            }),
        }),
        "loop_expression" | "while_expression" | "for_expression" => Some(Decision::Loop),
        // A closure body is part of the enclosing function, one level deeper.
        "closure_expression" | "function_item" => Some(Decision::NestedFunction),
        _ => None,
    }
}

fn name_of(node: Node, source: &[u8]) -> (String, String) {
    let name = field_text(node, "name", source)
        .unwrap_or("<anonymous>")
        .to_string();

    let mut owners = Vec::new();
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "impl_item" => {
                if let Some(target) = parent.child_by_field_name("type") {
                    // `Counter<'_>` and `Foo<T>` qualify a method as `Counter`
                    // and `Foo`; the generic arguments are noise.
                    let owner = innermost_type_identifier(target)
                        .and_then(|name| name.utf8_text(source).ok())
                        .or_else(|| target.utf8_text(source).ok())
                        .map(|text| text.trim().to_string());
                    if let Some(owner) = owner {
                        owners.push(owner);
                    }
                }
            }
            "trait_item" => {
                if let Some(trait_name) = field_text(parent, "name", source) {
                    owners.push(trait_name.to_string());
                }
            }
            _ => {}
        }
        current = parent.parent();
    }
    owners.reverse();

    let qualified = qualify(&owners, &name, "::");
    (name, qualified)
}

/// The innermost `type_identifier` of a type node, if it has one.
fn innermost_type_identifier(node: Node) -> Option<Node> {
    if node.kind() == "type_identifier" {
        return Some(node);
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    children.into_iter().find_map(innermost_type_identifier)
}

fn param_count(node: Node, _source: &[u8]) -> u32 {
    let Some(parameters) = node.child_by_field_name("parameters") else {
        return 0;
    };
    let mut cursor = parameters.walk();
    parameters
        .named_children(&mut cursor)
        .filter(|child| child.kind() != "self_parameter")
        .count() as u32
}
