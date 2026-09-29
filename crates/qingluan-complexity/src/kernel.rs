//! Language-independent counter kernel.
//!
//! Every arithmetic rule lives here; a language module only answers "what kind
//! of decision point is this node?" ([`Decision`]) and "is this a logical
//! operator node?" ([`LangProfile::logical_kinds`]). Keeping the rules in one
//! place is what makes cross-language numbers comparable.
//!
//! The scoring follows sonar-java's `CognitiveComplexityVisitor`, which agrees
//! with the white paper on every published example. One difference is
//! deliberate: sonar-java walks an `if`'s condition *before* deepening the
//! nesting level, while this kernel deepens the whole `if` subtree at once
//! (that is what makes `else`/`else if` land at the right depth without the
//! visitor's nesting-offset fiddling). A ternary inside an `if` condition
//! therefore scores one nesting level deeper than sonar-java would score it;
//! everything else matches.

use tree_sitter::Node;

use crate::Metrics;
use crate::langs::{LangProfile, LogicalOp};

/// What a node contributes to the metric vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    /// `if` / `else if`. Both add one to CC; cognitive adds `1 + nesting`,
    /// except for an `else if`, which is flat `+1` so a long chain stays cheap.
    If { else_if: bool },
    /// `else` block: flat `+1`, and its body keeps the nesting the enclosing
    /// `if` already applied (so it matches the `then` branch).
    Else,
    /// `?:` and friends: structural, so `+1 + nesting`.
    Ternary,
    /// `switch` / `match` statement or expression: one structural increment
    /// **in total**, however many cases it has. The cases themselves score
    /// nothing on the cognitive side.
    Switch,
    /// One `case` / `match` arm: `+1` CC unless it is the default/wildcard.
    Case { default: bool },
    /// `for` / `while` / `do` / `foreach`: structural.
    Loop,
    /// `catch`: structural. `try` and `finally` are free.
    Catch,
    /// `goto`, labelled `break`/`continue`: flat `+1`, no nesting.
    Jump,
    /// A nested function or closure: scores nothing itself, but raises the
    /// nesting level of its body.
    NestedFunction,
}

/// Count everything for one function subtree.
///
/// `root` is the function node itself (or the file root for the module
/// fallback). The root is never classified: the enclosing function must not
/// score as its own nested function.
pub(crate) fn count(root: Node, source: &[u8], profile: &LangProfile) -> Metrics {
    let mut counter = Counter {
        source,
        profile,
        cc: 1,
        cognitive: 0,
        nesting: 0,
        max_nesting: 0,
    };
    counter.walk(root);

    Metrics {
        cc: counter.cc,
        cognitive: counter.cognitive,
        nloc: nloc(root, source, profile),
        params: (profile.param_count)(root, source),
        max_nesting: counter.max_nesting,
    }
}

struct Counter<'a> {
    source: &'a [u8],
    profile: &'a LangProfile,
    cc: u32,
    cognitive: u32,
    nesting: u32,
    max_nesting: u32,
}

/// Explicit work stack: trees can be arbitrarily deep and a recursive walk
/// would let a pathological input blow the native stack.
enum Task<'tree> {
    Enter {
        node: Node<'tree>,
        in_logical_sequence: bool,
        is_root: bool,
    },
    Leave {
        nesting: u32,
    },
}

impl Counter<'_> {
    fn walk(&mut self, root: Node) {
        let mut stack = vec![Task::Enter {
            node: root,
            in_logical_sequence: false,
            is_root: true,
        }];
        while let Some(task) = stack.pop() {
            match task {
                Task::Enter {
                    node,
                    in_logical_sequence,
                    is_root,
                } => {
                    let nesting = self.nesting;
                    let child_sequence = if is_root {
                        false
                    } else {
                        self.enter(node, in_logical_sequence)
                    };
                    stack.push(Task::Leave { nesting });
                    let mut cursor = node.walk();
                    let children: Vec<Node> = node.children(&mut cursor).collect();
                    for child in children.into_iter().rev() {
                        stack.push(Task::Enter {
                            node: child,
                            in_logical_sequence: child_sequence,
                            is_root: false,
                        });
                    }
                }
                Task::Leave { nesting } => self.nesting = nesting,
            }
        }
    }

    /// Score one node. Returns whether its children are still inside the same
    /// logical-operator sequence (true only through operands and parentheses).
    fn enter(&mut self, node: Node, in_logical_sequence: bool) -> bool {
        if logical_operator(self.profile, node, self.source).is_some() {
            self.cc += 1;
            if !in_logical_sequence {
                // Only the outermost node of a logical component counts; the
                // component is flattened and walked in one go here.
                self.cognitive += logical_runs(self.profile, self.source, node);
            }
            return true;
        }

        match (self.profile.classify)(node, self.source) {
            Some(Decision::If { else_if }) => {
                self.cc += 1;
                if else_if {
                    self.cognitive += 1;
                } else {
                    self.cognitive += 1 + self.nesting;
                    self.nest();
                }
            }
            Some(Decision::Else) => self.cognitive += 1,
            Some(Decision::Ternary) => {
                self.cc += 1;
                self.cognitive += 1 + self.nesting;
                self.nest();
            }
            Some(Decision::Switch) => {
                self.cognitive += 1 + self.nesting;
                self.nest();
            }
            Some(Decision::Case { default }) => {
                if !default {
                    self.cc += 1;
                }
            }
            Some(Decision::Loop) => {
                self.cc += 1;
                self.cognitive += 1 + self.nesting;
                self.nest();
            }
            Some(Decision::Catch) => {
                self.cc += 1;
                self.cognitive += 1 + self.nesting;
                self.nest();
            }
            Some(Decision::Jump) => self.cognitive += 1,
            Some(Decision::NestedFunction) => self.nest(),
            None => {}
        }

        node.kind() == self.profile.paren_kind && in_logical_sequence
    }

    fn nest(&mut self) {
        self.nesting += 1;
        self.max_nesting = self.max_nesting.max(self.nesting);
    }
}

/// The operator of a logical node, if it is one.
///
/// The operator field is read first and anonymous children are scanned as a
/// fallback, because grammars disagree on whether `and`/`or` is a field.
pub(crate) fn logical_operator(
    profile: &LangProfile,
    node: Node,
    source: &[u8],
) -> Option<LogicalOp> {
    if !profile.logical_kinds.contains(&node.kind()) {
        return None;
    }
    if let Some(operator) = node.child_by_field_name(profile.operator_field)
        && let Ok(text) = operator.utf8_text(source)
        && let Some(op) = LogicalOp::from_text(text)
    {
        return Some(op);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Ok(text) = child.utf8_text(source)
            && let Some(op) = LogicalOp::from_text(text)
        {
            return Some(op);
        }
    }
    None
}

/// Number of "new sequences of like operators" in one logical component.
///
/// Mirrors sonar-java's `flattenLogicalExpression`: flatten the component
/// in-order, descending through operands and parentheses only, then charge one
/// per operator that differs from its predecessor in that list. That is what
/// makes `a && b && c` = 1, `a || b && c || d` = 3, and `a && !(b && c)` = 2
/// (the `!` is a flatten boundary, so the inner `&&` starts a new component).
fn logical_runs(profile: &LangProfile, source: &[u8], root: Node) -> u32 {
    let mut operators = Vec::new();
    flatten_logical(profile, source, root, &mut operators);

    let mut runs = 0;
    let mut previous: Option<LogicalOp> = None;
    for operator in operators {
        if previous != Some(operator) {
            runs += 1;
        }
        previous = Some(operator);
    }
    runs
}

fn flatten_logical(profile: &LangProfile, source: &[u8], node: Node, out: &mut Vec<LogicalOp>) {
    let Some(operator) = logical_operator(profile, node, source) else {
        return;
    };
    if let Some(left) = node.child_by_field_name(profile.left_field) {
        flatten_logical(profile, source, skip_parens(left, profile.paren_kind), out);
    }
    out.push(operator);
    if let Some(right) = node.child_by_field_name(profile.right_field) {
        flatten_logical(profile, source, skip_parens(right, profile.paren_kind), out);
    }
}

fn skip_parens<'tree>(mut node: Node<'tree>, paren_kind: &str) -> Node<'tree> {
    while node.kind() == paren_kind {
        match node.named_child(0) {
            Some(inner) => node = inner,
            None => break,
        }
    }
    node
}

/// Non-blank, non-comment lines inside the node's span.
fn nloc(root: Node, source: &[u8], profile: &LangProfile) -> u32 {
    let start = root.start_byte();
    let end = root.end_byte().min(source.len());
    if start >= end {
        return 0;
    }
    let mut buffer = source[start..end].to_vec();

    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if profile.comment_kinds.contains(&node.kind()) {
            let comment_start = node.start_byte().saturating_sub(start);
            let comment_end = (node.end_byte() - start).min(buffer.len());
            for byte in &mut buffer[comment_start..comment_end] {
                *byte = b' ';
            }
            continue;
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        stack.extend(children);
    }

    buffer
        .split(|&byte| byte == b'\n')
        .filter(|line| line.iter().any(|&byte| !byte.is_ascii_whitespace()))
        .count() as u32
}
