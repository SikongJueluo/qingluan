//! Per-language profiles.
//!
//! A profile is pure mapping: which nodes are functions, which are decisions,
//! how a function is named and how many parameters it declares. All arithmetic
//! lives in [`crate::kernel`].

pub(crate) mod go;
pub(crate) mod java;
pub(crate) mod python;
pub(crate) mod rust;
pub(crate) mod typescript;

use tree_sitter::Node;

use crate::kernel::Decision;

/// Binary operators whose runs cognitive complexity charges for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogicalOp {
    And,
    Or,
    /// `??`. The white paper does not mention it; we charge it like any other
    /// short-circuiting operator rather than following SonarJS's exemption.
    Nullish,
}

impl LogicalOp {
    pub(crate) fn from_text(text: &str) -> Option<Self> {
        Some(match text {
            "&&" | "and" => Self::And,
            "||" | "or" => Self::Or,
            "??" => Self::Nullish,
            _ => return None,
        })
    }
}

/// Everything the kernel needs to know about one language.
pub(crate) struct LangProfile {
    pub grammar: fn() -> tree_sitter::Language,
    /// Node kinds that define a function. Only the outermost ones are reported.
    pub function_kinds: &'static [&'static str],
    /// Node kinds that are binary logical operators (`&&`, `||`, ...).
    pub logical_kinds: &'static [&'static str],
    pub operator_field: &'static str,
    pub left_field: &'static str,
    pub right_field: &'static str,
    pub paren_kind: &'static str,
    pub comment_kinds: &'static [&'static str],
    /// `(name, qualified_name)` of a function node.
    pub name_of: fn(Node, &[u8]) -> (String, String),
    pub param_count: fn(Node, &[u8]) -> u32,
    pub classify: fn(Node, &[u8]) -> Option<Decision>,
}

/// Text of a named child field.
pub(crate) fn field_text<'a>(node: Node, field: &str, source: &'a [u8]) -> Option<&'a str> {
    node.child_by_field_name(field)?.utf8_text(source).ok()
}

/// How many named children a node has (used for parameter lists).
pub(crate) fn named_child_count(node: Node) -> u32 {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).count() as u32
}

/// Names of the enclosing types/classes, outermost first.
pub(crate) fn enclosing_names(
    node: Node,
    source: &[u8],
    kinds: &[&str],
    name_field: &str,
) -> Vec<String> {
    let mut names = Vec::new();
    let mut current = node.parent();
    while let Some(parent) = current {
        if kinds.contains(&parent.kind())
            && let Some(name) = field_text(parent, name_field, source)
        {
            names.push(name.to_string());
        }
        current = parent.parent();
    }
    names.reverse();
    names
}

/// Join an owner chain and a name into a qualified name.
pub(crate) fn qualify(owners: &[String], name: &str, separator: &str) -> String {
    if owners.is_empty() {
        name.to_string()
    } else {
        format!("{}{separator}{name}", owners.join(separator))
    }
}
