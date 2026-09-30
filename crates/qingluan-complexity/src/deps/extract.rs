//! Import-specifier extraction: one parsed buffer → the import specifiers it
//! declares, in document order.
//!
//! Extraction is deliberately *syntax-only* — it never probes the filesystem.
//! What a specifier means (a repo file, an external package, or a target we
//! cannot know) is [`super::resolve`]'s job; keeping the two apart is what
//! makes the per-language resolution rates honest.
//!
//! Language traps encoded here are verified against the pinned grammars in
//! `.scratch/complexity/research/coupling-static-analysis.md` (§1.2, §2.2,
//! §3.2, §4.2, §5.2, §6.2): the optional `source` field of a TypeScript
//! `import_statement` (`import x = require(...)` keeps it on
//! `import_require_clause`), the field-less Java `import_declaration`, the
//! `relative_import` node of Python, and the `import_spec` list nesting of Go.

use tree_sitter::{Node, Tree};

use crate::Language;

/// What one file's imports look like before resolution.
#[derive(Default, Clone)]
pub(crate) struct FileExtract {
    /// Every import specifier, in document order.
    pub specs: Vec<ImportSpec>,
    /// Rust `mod foo;` declarations (outlined only), for the module tree.
    pub rust_mods: Vec<RustModDecl>,
    /// Java `package a.b;` of the file, if any.
    pub java_package: Option<String>,
    /// Java top-level type names declared in the file.
    pub java_types: Vec<String>,
}

/// One import specifier, still unresolved.
#[derive(Clone)]
pub(crate) enum ImportSpec {
    /// Rust `use` path: `crate::a::b`, `super::x`, bare `foo::bar`. A
    /// trailing `::*` keeps the same module target, so no glob flag exists.
    RustPath { segments: Vec<String> },
    /// Python `import a.b` / `from .x import y`.
    /// `level` counts leading dots (0 = absolute); `module` is the dotted
    /// module (`a.b`); `from_name` is the imported leaf (`y`, which may be a
    /// submodule or an attribute — the resolver probes the filesystem).
    PyModule {
        module: Vec<String>,
        level: usize,
        from_name: Option<Vec<String>>,
    },
    /// TS/JS module specifier exactly as written (`"./a"`, `"pkg/x"`).
    TsSpecifier { specifier: String },
    /// Go import path literal.
    GoPath { path: String },
    /// Java import: the fully-qualified name segments; `wildcard` is the
    /// trailing `.*`; `is_static` marks `import static` (last segment is a
    /// *member*, so the container is what resolves).
    JavaType {
        fqn: Vec<String>,
        wildcard: bool,
        is_static: bool,
    },
}

/// One Rust outlined `mod name;` declaration.
#[derive(Clone)]
pub(crate) struct RustModDecl {
    pub name: String,
    /// `#[path = "..."]` on the declaration, if any.
    pub path_attr: Option<String>,
    /// Names of inline `mod` blocks enclosing the declaration, outermost
    /// first. These shift the directory the target file is looked up in.
    pub inline: Vec<String>,
}

/// Extract every import-relevant fact from one parsed buffer.
pub(crate) fn extract(language: Language, tree: &Tree, source: &[u8]) -> FileExtract {
    let mut out = FileExtract::default();
    match language {
        Language::Rust => walk_rust(tree.root_node(), source, &mut out),
        Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx => {
            walk_ts(tree.root_node(), source, &mut out)
        }
        Language::Python => walk_python(tree.root_node(), source, &mut out),
        Language::Go => walk_go(tree.root_node(), source, &mut out),
        Language::Java => walk_java(tree.root_node(), source, &mut out),
    }
    out
}

fn text<'a>(node: Node, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or_default()
}

fn unquote(literal: &str) -> &str {
    let bytes = literal.as_bytes();
    match (bytes.first(), bytes.last()) {
        (Some(b'"'), Some(b'"')) | (Some(b'\''), Some(b'\'')) | (Some(b'`'), Some(b'`')) => {
            &literal[1..literal.len() - 1]
        }
        _ => literal,
    }
}

// ---------------------------------------------------------------- Rust ----

/// Walk a Rust CST. `use` declarations count anywhere (inside a function body
/// they are still module dependencies); outlined `mod` declarations only count
/// at module scope — an outlined `mod` inside a function is not legal Rust,
/// and a macro-generated one is invisible to the CST by construction
/// (research §1.2, finding 3).
fn walk_rust(root: Node, source: &[u8], out: &mut FileExtract) {
    let mut inline = Vec::new();
    walk_rust_node(root, source, &mut inline, out);
}

fn walk_rust_node(node: Node, source: &[u8], inline: &mut Vec<String>, out: &mut FileExtract) {
    match node.kind() {
        "use_declaration" => {
            if let Some(argument) = node.child_by_field_name("argument") {
                let mut segments = Vec::new();
                flatten_use(argument, source, &mut segments, &mut out.specs);
            }
        }
        "mod_item" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| text(n, source).to_string());
            match (node.child_by_field_name("body"), name) {
                // Inline module: shifts the directory context for nested
                // outlined mods, but declares no file of its own.
                (Some(body), Some(name)) => {
                    inline.push(name);
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        walk_rust_node(child, source, inline, out);
                    }
                    inline.pop();
                }
                // Outlined module: record with its #[path] attribute, if the
                // declaration sits at module scope (parent is the file, a
                // module body, or the item list of a module body).
                (None, Some(name)) if is_at_module_scope(node) => {
                    out.rust_mods.push(RustModDecl {
                        name,
                        path_attr: path_attribute(node, source),
                        inline: inline.clone(),
                    });
                }
                _ => {}
            }
            return;
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_rust_node(child, source, inline, out);
    }
}

/// Whether an outlined `mod` sits directly in module scope, not inside a
/// function, closure or impl block.
fn is_at_module_scope(node: Node) -> bool {
    let mut parent = node.parent();
    while let Some(p) = parent {
        match p.kind() {
            "source_file" | "declaration_list" | "mod_item" => {}
            _ => return false,
        }
        parent = p.parent();
    }
    true
}

/// `#[path = "..."]` attached to the item, by looking at the attribute items
/// that precede it. `#[cfg_attr(..., path = ...)]` is deliberately *not*
/// honoured: the conditional target cannot be evaluated, and inventing one
/// would fabricate an edge (research §1.1, "Conditional path" row).
fn path_attribute(node: Node, source: &[u8]) -> Option<String> {
    let mut sibling = node.prev_sibling();
    while let Some(attr) = sibling {
        if attr.kind() != "attribute_item" {
            break;
        }
        let raw = text(attr, source)
            .trim_start_matches("#[")
            .trim_end_matches(']')
            .trim();
        if let Some((key, value)) = raw.split_once('=')
            && key.trim() == "path"
        {
            return Some(unquote(value.trim()).to_string());
        }
        sibling = attr.prev_sibling();
    }
    None
}

/// Flatten a `use` tree into leaf paths. `use a::{b, c::d}` yields
/// `[a, b]` and `[a, c, d]`; each leaf keeps the shared prefix.
fn flatten_use(node: Node, source: &[u8], segments: &mut Vec<String>, specs: &mut Vec<ImportSpec>) {
    match node.kind() {
        "scoped_identifier" => {
            // `path::name`: the path side accumulates silently, the whole
            // path emits once — emitting the prefix alone would invent an
            // edge the declaration does not create.
            if let Some(path) = node.child_by_field_name("path") {
                push_path_segments(path, source, segments);
            }
            if let Some(name) = node.child_by_field_name("name") {
                segments.push(text(name, source).to_string());
            }
            emit_rust_path(segments, specs);
        }
        "crate" | "super" | "self" | "identifier" => {
            segments.push(text(node, source).to_string());
            emit_rust_path(segments, specs);
        }
        "use_as_clause" => {
            if let Some(path) = node.child_by_field_name("path") {
                flatten_use(path, source, segments, specs);
            }
        }
        "scoped_use_list" => {
            // `prefix::{items}`: resolve the prefix once, then each item with
            // the prefix retained.
            if let Some(path) = node.child_by_field_name("path") {
                push_path_segments(path, source, segments);
            }
            if let Some(list) = node.child_by_field_name("list") {
                flatten_use(list, source, segments, specs);
            }
        }
        "use_list" => {
            let mut cursor = node.walk();
            let children: Vec<Node> = node.children(&mut cursor).collect();
            // Every leaf restores the shared prefix before the next one.
            let base = segments.len();
            for child in children {
                flatten_use(child, source, segments, specs);
                segments.truncate(base);
            }
        }
        "use_wildcard" => {
            // `prefix::*`: the module target of the prefix is a real edge
            // (the glob changes which *names* are bound, not the target).
            emit_rust_path(segments, specs);
        }
        // `use <Foo as Bar>::Baz;` and macro metavariables: the first segment
        // is a type or a macro artifact, not a module name. Emitting anything
        // here would be a confident wrong answer (research §1.2, finding 1).
        _ => {}
    }
}

/// Append the segments of a `path` field node (identifier, keyword or nested
/// scoped_identifier) without emitting anything.
fn push_path_segments(node: Node, source: &[u8], segments: &mut Vec<String>) {
    match node.kind() {
        "scoped_identifier" => {
            if let Some(path) = node.child_by_field_name("path") {
                push_path_segments(path, source, segments);
            }
            if let Some(name) = node.child_by_field_name("name") {
                segments.push(text(name, source).to_string());
            }
        }
        "crate" | "super" | "self" | "identifier" => {
            segments.push(text(node, source).to_string());
        }
        _ => {}
    }
}

fn emit_rust_path(segments: &mut Vec<String>, specs: &mut Vec<ImportSpec>) {
    if !segments.is_empty() {
        specs.push(ImportSpec::RustPath {
            segments: segments.clone(),
        });
    }
    segments.clear();
}

// ------------------------------------------------------------ TypeScript ----

/// TS and JS share the walk: `import_statement.source` (optional in TS — the
/// `import x = require(...)` form keeps it on `import_require_clause`),
/// re-export sources, and string-literal arguments of `require(...)` and
/// `import(...)`.
fn walk_ts(root: Node, source: &[u8], out: &mut FileExtract) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "import_statement" => {
                let source_field = node.child_by_field_name("source").or_else(|| {
                    // `import x = require("./y")`: the specifier is on the
                    // clause, not on the statement (verified, research §2.2).
                    find_child_of_kind(node, "import_require_clause")
                        .and_then(|clause| clause.child_by_field_name("source"))
                });
                push_ts_specifier(source_field, source, out);
            }
            "export_statement" => {
                // Only `export ... from "..."` carries a source; a local
                // `export const x` does not.
                push_ts_specifier(node.child_by_field_name("source"), source, out);
            }
            "call_expression" => {
                let callee = node.child_by_field_name("function");
                let literal = node.child_by_field_name("arguments").and_then(|args| {
                    let mut cursor = args.walk();
                    args.children(&mut cursor)
                        .find(|child| child.kind() == "string")
                });
                let is_require = callee
                    .is_some_and(|c| c.kind() == "identifier" && text(c, source) == "require");
                let is_dynamic_import = callee.is_some_and(|c| c.kind() == "import");
                if (is_require || is_dynamic_import)
                    && let Some(literal) = literal
                {
                    push_ts_specifier(Some(literal), source, out);
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
}

fn push_ts_specifier(node: Option<Node>, source: &[u8], out: &mut FileExtract) {
    if let Some(node) = node
        && node.kind() == "string"
    {
        let specifier = unquote(text(node, source));
        if !specifier.is_empty() {
            out.specs.push(ImportSpec::TsSpecifier {
                specifier: specifier.to_string(),
            });
        }
    }
}

fn find_child_of_kind<'t>(node: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .find(|child| child.kind() == kind)
}

// --------------------------------------------------------------- Python ----

fn walk_python(root: Node, source: &[u8], out: &mut FileExtract) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "import_statement" => {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    let dotted = match child.kind() {
                        "dotted_name" => dotted_segments(child, source),
                        "aliased_import" => child
                            .child_by_field_name("name")
                            .map(|name| dotted_segments(name, source))
                            .unwrap_or_default(),
                        _ => continue,
                    };
                    if !dotted.is_empty() {
                        out.specs.push(ImportSpec::PyModule {
                            module: dotted,
                            level: 0,
                            from_name: None,
                        });
                    }
                }
            }
            "import_from_statement" => {
                // Positional walk rather than the `module_name` field: the
                // field exists, but matching by position also survives
                // grammar revisions where `relative_import` sits outside it.
                // The first dotted/relative child is the module; every
                // later dotted name is an imported leaf (`from x import a, b`
                // emits one specifier per leaf).
                let mut module: Option<(Vec<String>, usize)> = None;
                let mut names: Vec<Vec<String>> = Vec::new();
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    match child.kind() {
                        "dotted_name" | "relative_import" if module.is_none() => {
                            module = Some(match child.kind() {
                                "dotted_name" => (dotted_segments(child, source), 0),
                                _ => {
                                    // The prefix holds the dots; the dotted
                                    // part is optional (`from . import x`,
                                    // research §4.2). `import_prefix` is a
                                    // plain child, not a field.
                                    let dots = find_child_of_kind(child, "import_prefix")
                                        .map(|p| text(p, source).len())
                                        .unwrap_or(0);
                                    let dotted = find_child_of_kind(child, "dotted_name")
                                        .map(|d| dotted_segments(d, source))
                                        .unwrap_or_default();
                                    (dotted, dots)
                                }
                            });
                        }
                        "dotted_name" => {
                            names.push(dotted_segments(child, source));
                        }
                        "aliased_import" => {
                            if let Some(name) = child.child_by_field_name("name") {
                                names.push(dotted_segments(name, source));
                            }
                        }
                        _ => {}
                    }
                }
                // `from x import *` has no name child at all — the module
                // edge alone is what we keep. `__future__` is its own node
                // kind (`future_import_statement`) and never reaches here.
                let (module, level) = module.unwrap_or_default();
                if names.is_empty() {
                    out.specs.push(ImportSpec::PyModule {
                        module,
                        level,
                        from_name: None,
                    });
                } else {
                    for name in names {
                        out.specs.push(ImportSpec::PyModule {
                            module: module.clone(),
                            level,
                            from_name: Some(name),
                        });
                    }
                }
            }
            _ => {}
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
}

fn dotted_segments(node: Node, source: &[u8]) -> Vec<String> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(|child| child.is_named())
        .map(|child| text(child, source).to_string())
        .collect()
}

// ------------------------------------------------------------------- Go ----

fn walk_go(root: Node, source: &[u8], out: &mut FileExtract) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "import_spec"
            && let Some(path) = node.child_by_field_name("path")
        {
            let path = unquote(text(path, source));
            if !path.is_empty() {
                out.specs.push(ImportSpec::GoPath {
                    path: path.to_string(),
                });
            }
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
}

// ------------------------------------------------------------------ Java ----

fn walk_java(root: Node, source: &[u8], out: &mut FileExtract) {
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        match child.kind() {
            "package_declaration" => {
                // Field-less node: the name is the first scoped_identifier
                // child (annotations may precede it).
                let mut cursor = child.walk();
                let name = child
                    .children(&mut cursor)
                    .find(|part| matches!(part.kind(), "scoped_identifier" | "identifier"));
                if let Some(name) = name {
                    out.java_package = Some(scoped_name(name, source).join("."));
                }
            }
            "import_declaration" => {
                // Field-less node: the name is a scoped_identifier child, an
                // `asterisk` child means on-demand, an anonymous `static`
                // child marks a static import (verified, research §6.2).
                let mut segments = Vec::new();
                let mut wildcard = false;
                let mut is_static = false;
                let mut cursor = child.walk();
                for part in child.children(&mut cursor) {
                    match part.kind() {
                        "scoped_identifier" | "identifier" => {
                            let mut path = Vec::new();
                            push_scoped_name(part, source, &mut path);
                            segments = path;
                        }
                        "asterisk" => wildcard = true,
                        "static" => is_static = true,
                        _ => {}
                    }
                }
                if !segments.is_empty() {
                    out.specs.push(ImportSpec::JavaType {
                        fqn: segments,
                        wildcard,
                        is_static,
                    });
                }
            }
            "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "annotation_type_declaration" => {
                if let Some(name) = child.child_by_field_name("name") {
                    out.java_types.push(text(name, source).to_string());
                }
            }
            _ => {}
        }
    }
}

fn scoped_name(node: Node, source: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    push_scoped_name(node, source, &mut out);
    out
}

/// Flatten `scoped_identifier` nesting: the `scope` side recurses, the `name`
/// side is the last segment.
fn push_scoped_name(node: Node, source: &[u8], out: &mut Vec<String>) {
    match node.kind() {
        "scoped_identifier" => {
            if let Some(scope) = node.child_by_field_name("scope") {
                push_scoped_name(scope, source, out);
            }
            if let Some(name) = node.child_by_field_name("name") {
                out.push(text(name, source).to_string());
            }
        }
        "identifier" => out.push(text(node, source).to_string()),
        _ => {}
    }
}
