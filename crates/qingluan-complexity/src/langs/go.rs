//! Go profile.

use tree_sitter::Node;

use super::{LangProfile, field_text, qualify};
use crate::kernel::Decision;

pub(crate) fn profile() -> &'static LangProfile {
    static PROFILE: LangProfile = LangProfile {
        grammar: || tree_sitter_go::LANGUAGE.into(),
        function_kinds: &["function_declaration", "method_declaration"],
        logical_kinds: &["binary_expression"],
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

fn classify(node: Node, _source: &[u8]) -> Option<Decision> {
    match node.kind() {
        "if_statement" => Some(Decision::If {
            else_if: is_else_if(node),
        }),
        // An `else` branch. When it only leads into an `else if` the inner if
        // scores and this wrapper must not add a second point.
        "block" => {
            if !is_else_branch(node) {
                return None;
            }
            match leading_statement(node) {
                Some(statement) if statement.kind() == "if_statement" => None,
                _ => Some(Decision::Else),
            }
        }
        "for_statement" => Some(Decision::Loop),
        "expression_switch_statement" | "type_switch_statement" | "select_statement" => {
            Some(Decision::Switch)
        }
        "expression_case" | "type_case" | "communication_case" => {
            Some(Decision::Case { default: false })
        }
        // Go gives `default:` its own kind (checked with the dump example), so
        // the empty-`expression_case` fallback the other grammars need is not
        // required here.
        "default_case" => Some(Decision::Case { default: true }),
        // Only a labelled `break`/`continue` is a jump; an unlabelled one is
        // the structured flow its loop already scores for.
        "break_statement" | "continue_statement" => node
            .named_child(0)
            .is_some_and(|child| child.kind() == "label_name")
            .then_some(Decision::Jump),
        "goto_statement" => Some(Decision::Jump),
        // A closure belongs to the enclosing function, one level deeper.
        "func_literal" => Some(Decision::NestedFunction),
        _ => None,
    }
}

/// Whether `node` is the `alternative` (the `else` branch) of an `if_statement`.
///
/// `alternative` is the only field an `if_statement` gives a `block`; its
/// `consequence` is a block too, so the field check is what separates an
/// `else` from a `then`.
fn is_else_branch(node: Node) -> bool {
    node.parent().is_some_and(|parent| {
        parent.kind() == "if_statement"
            && parent
                .child_by_field_name("alternative")
                .is_some_and(|alternative| alternative == node)
    })
}

/// Whether this `if_statement` is the `if` of an `else if` chain.
///
/// Go has no `else if` keyword, so the chain arrives in one of two shapes and
/// both must score flat `+1`:
///
/// * `} else if cond {` — this if *is* the outer if's `alternative`;
/// * `} else { if cond { ... } ... }` — this if leads the alternative block.
fn is_else_if(node: Node) -> bool {
    if is_else_branch(node) {
        return true;
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.kind() == "statement_list"
        && leading_statement(parent) == Some(node)
        && parent
            .parent()
            .is_some_and(|block| block.kind() == "block" && is_else_branch(block))
}

/// The first statement of a `block` or `statement_list`, if any.
fn leading_statement(node: Node) -> Option<Node> {
    match node.kind() {
        "block" => node.named_child(0).and_then(leading_statement),
        "statement_list" => node.named_child(0),
        _ => None,
    }
}

fn name_of(node: Node, source: &[u8]) -> (String, String) {
    let name = field_text(node, "name", source)
        .unwrap_or("<anonymous>")
        .to_string();
    let owners: Vec<String> = receiver_type(node, source).into_iter().collect();
    let qualified = qualify(&owners, &name, ".");
    (name, qualified)
}

/// The receiver type of a method: `Service` for `(s *Service)` and for
/// `(g *Gen[T])` alike, since the type name is what qualifies the method.
fn receiver_type(node: Node, source: &[u8]) -> Option<String> {
    let receiver = node.child_by_field_name("receiver")?;
    let declaration = receiver.named_child(0)?;
    let receiver_type = declaration.child_by_field_name("type")?;
    first_type_identifier(receiver_type)
        .and_then(|name| name.utf8_text(source).ok())
        .map(str::to_string)
}

/// The innermost `type_identifier` of a type node, descending through
/// `pointer_type`, `generic_type` and friends.
fn first_type_identifier(node: Node) -> Option<Node> {
    if node.kind() == "type_identifier" {
        return Some(node);
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    children.into_iter().find_map(first_type_identifier)
}

fn param_count(node: Node, _source: &[u8]) -> u32 {
    let Some(parameters) = node.child_by_field_name("parameters") else {
        return 0;
    };
    let mut cursor = parameters.walk();
    parameters
        .named_children(&mut cursor)
        .map(|parameter| declared_names(parameter).max(1))
        .sum()
}

/// How many names a parameter node declares.
///
/// `a, b int` is a single `parameter_declaration` with two `name` fields, so
/// counting declarations would undercount it by one per extra name.
fn declared_names(node: Node) -> u32 {
    let mut count = 0;
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            if cursor.field_name() == Some("name") {
                count += 1;
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use crate::{Language, analyze_source};

    fn functions(source: &str) -> Vec<crate::FunctionMetrics> {
        analyze_source(Language::Go, source.as_bytes())
    }

    #[test]
    fn go_functions_methods_and_receivers_are_named() {
        let source = r#"
package main

type Service struct{}

func plain(a, b int, c string) int {
	if a > 0 {
		return 1
	}
	return 0
}

func (s *Service) Method(x int) int {
	return x
}
"#;
        let found = functions(source);
        assert_eq!(found.len(), 2);

        let plain = &found[0];
        assert_eq!(plain.name, "plain");
        assert_eq!(plain.qualified_name, "plain");
        assert_eq!(plain.metrics.params, 3);
        // if
        assert_eq!(plain.metrics.cc, 2);
        assert_eq!(plain.metrics.cognitive, 1);

        let method = &found[1];
        assert_eq!(method.name, "Method");
        // the receiver's pointer is not part of the name
        assert_eq!(method.qualified_name, "Service.Method");
        assert_eq!(method.metrics.params, 1);
        assert_eq!(method.metrics.cc, 1);
        assert_eq!(method.metrics.cognitive, 0);
    }

    #[test]
    fn go_else_if_chains_stay_flat() {
        let source = r#"
package main

func grade(score int) string {
	if score > 90 {
		return "A"
	} else if score > 80 {
		return "B"
	} else if score > 70 {
		return "C"
	} else {
		return "F"
	}
}
"#;
        let found = functions(source);
        assert_eq!(found.len(), 1);
        // if + two else-ifs; the else is free for CC
        assert_eq!(found[0].metrics.cc, 4);
        // if(+1) + two else-ifs(+1 each) + else(+1), none of them nested
        assert_eq!(found[0].metrics.cognitive, 4);
        assert_eq!(found[0].metrics.max_nesting, 1);
    }

    #[test]
    fn go_braced_else_if_is_the_same_flat_chain() {
        let source = r#"
package main

func grade(score int) string {
	if score > 90 {
		return "A"
	} else {
		if score > 80 {
			return "B"
		}
		return "C"
	}
}

func nested(a bool) {
	if a {
		if a {
		}
	}
}
"#;
        let found = functions(source);
        assert_eq!(found.len(), 2);

        // `} else { if ... }` is Go's other spelling of `else if`: the wrapper
        // block is transparent, so there is no else point and the inner if is
        // flat rather than nested.
        let braced = &found[0];
        assert_eq!(braced.metrics.cc, 3);
        assert_eq!(braced.metrics.cognitive, 2);
        assert_eq!(braced.metrics.max_nesting, 1);

        // An if in the *then* branch is a real nesting, not an else-if.
        let nested = &found[1];
        assert_eq!(nested.metrics.cc, 3);
        // outer if(+1) + inner if(+1 + 1 nesting)
        assert_eq!(nested.metrics.cognitive, 3);
        assert_eq!(nested.metrics.max_nesting, 2);
    }

    #[test]
    fn go_switch_scores_once_for_cognitive_but_every_case_for_cc() {
        let source = r#"
package main

func pick(value int) string {
	switch value {
	case 1:
		return "one"
	case 2:
		return "two"
	case 3:
		return "three"
	default:
		return "many"
	}
}
"#;
        let found = functions(source);
        // three cases, the default is free
        assert_eq!(found[0].metrics.cc, 4);
        assert_eq!(found[0].metrics.cognitive, 1);
    }

    #[test]
    fn go_type_switch_select_and_labelled_jumps() {
        let source = r#"
package main

func dispatch(value any, ch chan int) {
	switch value.(type) {
	case int:
	case string:
	default:
	}
	select {
	case <-ch:
	default:
	}
Outer:
	for {
		for {
			break Outer
		}
	}
	goto End
End:
	_ = value
}
"#;
        let found = functions(source);
        assert_eq!(found.len(), 1);
        // two type cases + one communication case + two for loops
        assert_eq!(found[0].metrics.cc, 6);
        // Each statement starts at the function's own depth: type switch(+1)
        // + select(+1) + outer for(+1) + inner for(+1 + 1 nesting)
        // + `break Outer`(+1, flat) + goto(+1, flat)
        assert_eq!(found[0].metrics.cognitive, 7);
        assert_eq!(found[0].metrics.max_nesting, 2);
    }

    #[test]
    fn go_logical_operator_sequences_follow_the_white_paper() {
        let source = r#"
package main

func logic(a, b, c, d bool) {
	one := a && b && c
	two := a || b && c || d
	_, _ = one, two
}
"#;
        let found = functions(source);
        // `a && b && c` is one run; `a || b && c || d` is three
        assert_eq!(found[0].metrics.cognitive, 4);
        // every && and || counts once for CC1: 1 + 2 + 3
        assert_eq!(found[0].metrics.cc, 6);
    }

    #[test]
    fn go_closures_count_into_the_enclosing_function() {
        let source = r#"
package main

func outer(x int) int {
	check := func(value int) int {
		if value > 0 {
			return 1
		}
		return 0
	}
	return check(x)
}
"#;
        let found = functions(source);
        // the func_literal is not a row of its own
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "outer");
        // if inside the closure
        assert_eq!(found[0].metrics.cc, 2);
        // closure(+1 nesting) + if(+1 + 1)
        assert_eq!(found[0].metrics.cognitive, 2);
        assert_eq!(found[0].metrics.max_nesting, 2);
    }

    #[test]
    fn go_grouped_parameter_names_each_count() {
        let source = r#"
package main

func f(a, b int, c string) {}

func g(xs ...int) {}

func h(int, string) {}
"#;
        let found = functions(source);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].metrics.params, 3);
        assert_eq!(found[1].metrics.params, 1);
        assert_eq!(found[2].metrics.params, 2);
    }
}
