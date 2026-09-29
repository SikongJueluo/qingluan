//! Function-level complexity metrics.
//!
//! Two metrics per function, both defined in
//! `docs/research/code-complexity-metrics.md`:
//!
//! * [`Metrics::cc`] — McCabe cyclomatic complexity, CC1 flavour: `1 + number
//!   of decisions`, where every short-circuiting boolean operator counts
//!   (`&&`, `||`, `??`, Python `and`/`or`) and every `switch`/`match` arm
//!   counts. A `default`/wildcard arm counts for nothing.
//! * [`Metrics::cognitive`] — SonarSource cognitive complexity (white paper
//!   Appendix B). We follow the *sonar-java* implementation, which agrees with
//!   the white paper on every published example; the SonarJS `||`/`??`
//!   exemption of 2024-10 is deliberately **not** followed, so every language
//!   scores alike.
//!
//! Three size metrics come free on the same walk: [`Metrics::nloc`],
//! [`Metrics::params`], [`Metrics::max_nesting`]. No composite score is
//! derived from these — deciding that a 500-line CC 20 function is worse than
//! a 10-line CC 20 function is the reader's job.
//!
//! Function boundaries come from tree-sitter, so a file that does not compile
//! still yields a walkable tree (that is the point: we review diffs written by
//! agents). Only the **outermost** function of a nested cluster is reported —
//! a nested function or closure hands its decision points to the enclosing
//! function instead of becoming a row of its own, and raises its own body one
//! nesting level (`.scratch/complexity/spec.md`). A file with no function at
//! all gets one synthetic `<module>` entry so script-style sources are not
//! silently lost.

use std::path::Path;

use serde::Serialize;

mod kernel;
mod langs;

pub mod scan;

pub use scan::{FileComplexity, ScanOptions, ScanReport, SkipStats, scan};

/// Source languages with a bundled grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Language {
    Rust,
    TypeScript,
    /// TypeScript with JSX (`.tsx`).
    Tsx,
    JavaScript,
    /// JavaScript with JSX (`.jsx`).
    Jsx,
    Python,
    Go,
    Java,
}

impl Language {
    /// Language for a path, from its extension.
    pub fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        Self::from_extension(&extension)
    }

    /// Language for a bare extension (no dot), case-insensitively.
    pub fn from_extension(extension: &str) -> Option<Self> {
        Some(match extension.to_ascii_lowercase().as_str() {
            "rs" => Self::Rust,
            "ts" | "mts" | "cts" => Self::TypeScript,
            "tsx" => Self::Tsx,
            "js" | "mjs" | "cjs" => Self::JavaScript,
            "jsx" => Self::Jsx,
            "py" | "pyi" => Self::Python,
            "go" => Self::Go,
            "java" => Self::Java,
            _ => return None,
        })
    }

    /// Lower-case language name, as printed in JSON and skip reasons.
    pub fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::JavaScript => "javascript",
            Self::Jsx => "jsx",
            Self::Python => "python",
            Self::Go => "go",
            Self::Java => "java",
        }
    }

    pub(crate) fn profile(self) -> &'static langs::LangProfile {
        match self {
            Self::Rust => langs::rust::profile(),
            Self::TypeScript => langs::typescript::profile(),
            Self::Tsx => langs::typescript::tsx_profile(),
            Self::JavaScript | Self::Jsx => langs::typescript::javascript_profile(),
            Self::Python => langs::python::profile(),
            Self::Go => langs::go::profile(),
            Self::Java => langs::java::profile(),
        }
    }
}

/// The metric vector reported for one function.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
    /// McCabe cyclomatic complexity, CC1 flavour (always at least 1).
    pub cc: u32,
    /// SonarSource cognitive complexity (white paper Appendix B).
    pub cognitive: u32,
    /// Non-blank, non-comment lines inside the function.
    pub nloc: u32,
    /// Declared parameters.
    pub params: u32,
    /// Deepest control-structure nesting reached inside the function.
    pub max_nesting: u32,
}

/// One function (or module fallback) with its spans and metrics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionMetrics {
    /// Bare function name, or `<anonymous>` / `<module>`.
    pub name: String,
    /// Name qualified by its enclosing type when there is one.
    pub qualified_name: String,
    /// 1-based first line.
    pub start_line: usize,
    /// 1-based last line.
    pub end_line: usize,
    /// 1-based first column.
    pub start_col: usize,
    /// Byte offset of the function start (for editor/UI use).
    pub start_byte: usize,
    /// Byte offset just past the function end.
    pub end_byte: usize,
    #[serde(flatten)]
    pub metrics: Metrics,
}

/// Analyze one source buffer. Returns functions in document order.
///
/// Never fails: unparsable input still produces whatever tree-sitter could
/// recover, and a file with no recoverable function yields the `<module>`
/// fallback (or nothing at all, for a blank file).
pub fn analyze_source(language: Language, source: &[u8]) -> Vec<FunctionMetrics> {
    let profile = language.profile();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&(profile.grammar)())
        .expect("bundled grammar is ABI-compatible with the pinned tree-sitter core");
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    let root = tree.root_node();

    let mut outer = Vec::new();
    collect_outermost_functions(root, profile.function_kinds, &mut outer);
    let mut functions: Vec<FunctionMetrics> = outer
        .into_iter()
        .map(|node| function_metrics(node, source, profile))
        .collect();

    if functions.is_empty() && root.named_child_count() > 0 {
        functions.push(module_metrics(root, source, profile));
    }

    functions.sort_by_key(|function| (function.start_line, function.start_col));
    functions
}

/// Read and analyze one file. `Ok(None)` means the extension has no grammar.
pub fn analyze_path(path: &Path) -> std::io::Result<Option<FileComplexity>> {
    let Some(language) = Language::from_path(path) else {
        return Ok(None);
    };
    let source = std::fs::read(path)?;
    let functions = analyze_source(language, &source);
    Ok(Some(FileComplexity {
        path: path.to_path_buf(),
        language,
        functions,
    }))
}

/// Distribution of one metric over all functions of a scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Distribution {
    pub p50: u32,
    pub p90: u32,
    pub p99: u32,
    pub max: u32,
    /// Functions strictly above the configured threshold.
    pub over_threshold: u32,
}

/// Nearest-rank percentiles over `values` (unsorted input is fine).
///
/// Nearest-rank rather than interpolation: integer output, and the same input
/// always gives the same percentile, which is what makes repeated runs
/// diffable.
pub fn distribution(values: &[u32], threshold: u32) -> Distribution {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    Distribution {
        p50: percentile(&sorted, 50),
        p90: percentile(&sorted, 90),
        p99: percentile(&sorted, 99),
        max: sorted.last().copied().unwrap_or(0),
        over_threshold: sorted.iter().filter(|&&value| value > threshold).count() as u32,
    }
}

fn percentile(sorted: &[u32], percentile: usize) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (percentile * sorted.len()).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn function_metrics(
    node: tree_sitter::Node,
    source: &[u8],
    profile: &langs::LangProfile,
) -> FunctionMetrics {
    let (name, qualified_name) = (profile.name_of)(node, source);
    let start = node.start_position();
    let end = node.end_position();
    FunctionMetrics {
        name,
        qualified_name,
        start_line: start.row + 1,
        end_line: end.row + 1,
        start_col: start.column + 1,
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        metrics: kernel::count(node, source, profile),
    }
}

fn module_metrics(
    root: tree_sitter::Node,
    source: &[u8],
    profile: &langs::LangProfile,
) -> FunctionMetrics {
    let start = root.start_position();
    let end = root.end_position();
    FunctionMetrics {
        name: "<module>".into(),
        qualified_name: "<module>".into(),
        start_line: start.row + 1,
        end_line: end.row + 1,
        start_col: start.column + 1,
        start_byte: root.start_byte(),
        end_byte: root.end_byte(),
        metrics: kernel::count(root, source, profile),
    }
}

/// Collect the outermost function nodes: a function nested inside another
/// function (or closure) is part of the enclosing one, never its own row.
fn collect_outermost_functions<'tree>(
    root: tree_sitter::Node<'tree>,
    function_kinds: &[&str],
    out: &mut Vec<tree_sitter::Node<'tree>>,
) {
    let mut stack = vec![(root, false)];
    while let Some((node, inside_function)) = stack.pop() {
        let is_function = function_kinds.contains(&node.kind());
        if is_function && !inside_function {
            out.push(node);
        }
        let children_inside = inside_function || is_function;
        let mut cursor = node.walk();
        let children: Vec<tree_sitter::Node> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push((child, children_inside));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distribution_is_nearest_rank_and_counts_above_threshold() {
        let dist = distribution(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 5);
        assert_eq!(dist.p50, 5);
        assert_eq!(dist.p90, 9);
        assert_eq!(dist.p99, 10);
        assert_eq!(dist.max, 10);
        assert_eq!(dist.over_threshold, 5);
        assert_eq!(distribution(&[], 5).max, 0);
    }

    #[test]
    fn languages_map_from_extensions_case_insensitively() {
        assert_eq!(
            Language::from_path(Path::new("a/b.rs")),
            Some(Language::Rust)
        );
        assert_eq!(Language::from_path(Path::new("A.TSX")), Some(Language::Tsx));
        assert_eq!(Language::from_path(Path::new("a/b.md")), None);
    }
}
