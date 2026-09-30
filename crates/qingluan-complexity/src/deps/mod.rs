//! The dependency (coupling) report: a whole-repo import graph.
//!
//! This is a *different kind of report* from the function-level scan. The
//! complexity engine is "one file in, metrics out"; coupling needs every
//! file's imports resolved against every other file, so this module walks
//! the same scan set, builds per-language indexes (a Rust module tree, a
//! Java type index, Go module paths), and reports:
//!
//! * **cycles** — strongly connected components, the one coupling judgement
//!   the literature and every tool agree is a defect (ADP; research §4);
//! * **fan-in / fan-out / instability `I = Ce/(Ca+Ce)`** per file — display
//!   only, never a gate: high fan-in is the *definition* of a stable
//!   abstraction in the primary sources (research §2), and our fan-in is a
//!   lower bound besides;
//! * **resolution stats** per language — resolved / external / unresolved,
//!   because every downstream number inherits the resolution recall
//!   (research §6), and a silent undercount is worse than a visible one.
//!
//! Churn is deliberately *not* read here: this engine stays a pure function
//! over the scan set, and history arrives as an injected map
//! ([`ChurnEntry`]) so golden tests need no VCS.

pub(crate) mod extract;
pub(crate) mod graph;
pub(crate) mod resolve;

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;

use rayon::prelude::*;

pub use graph::Cycle;
use resolve::{GoIndex, JavaIndex, Resolution, RustResolver};

use crate::scan::{self, ScanOptions};
use crate::{Language, scan as scan_files};

/// One file's coupling vector.
#[derive(Debug, Clone, PartialEq)]
pub struct FileDeps {
    pub path: PathBuf,
    pub language: Language,
    /// Files importing this one. A **lower bound**: whichever import
    /// constructs the language hides (Java same-package, TS aliases, …)
    /// remove exactly these edges.
    pub fan_in: u32,
    /// Files this one imports. Cheap and comparatively honest — it only
    /// needs this file's own specifiers resolved.
    pub fan_out: u32,
    /// Martin's instability `I = Ce/(Ca+Ce)`: 0 = maximally responsible and
    /// independent, 1 = maximally dependent. 0 when the file has no edges.
    pub instability: f64,
}

/// Resolution accounting for one language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LanguageDeps {
    pub language: Language,
    pub files: u32,
    /// Specifiers that became repo edges.
    pub resolved: u32,
    /// Specifiers that name something outside the repo (packages, stdlib,
    /// other crates). Not a miss.
    pub external: u32,
    /// Specifiers that should have landed in the repo but did not. The
    /// visible-miss budget; the invisible one (constructs that emit no
    /// specifier at all, like Java same-package references) is documented in
    /// the research, not countable here.
    pub unresolved: u32,
}

impl LanguageDeps {
    pub fn specifiers(&self) -> u32 {
        self.resolved + self.external + self.unresolved
    }

    /// Share of specifiers whose target we could account for, either way.
    pub fn resolution_rate(&self) -> f64 {
        let total = self.specifiers();
        if total == 0 {
            1.0
        } else {
            f64::from(self.resolved + self.external) / f64::from(total)
        }
    }
}

/// Result of one dependency analysis.
#[derive(Debug, Clone, PartialEq)]
pub struct DepsReport {
    /// Absolute root the `files` paths are relative to (same convention as
    /// the complexity scan).
    pub root: PathBuf,
    /// Every scanned file, sorted by path.
    pub files: Vec<FileDeps>,
    /// Per-language resolution accounting, sorted by language name.
    pub languages: Vec<LanguageDeps>,
    /// Dependency cycles, cross-directory first (see [`graph::sort_cycles`]).
    pub cycles: Vec<Cycle>,
}

/// Analyze the import graph of everything the options select.
///
/// The walk and the skip rules are the complexity scan's own — the two
/// reports must agree on what "the repo" is, or their numbers cannot be
/// compared.
pub fn analyze_deps(options: &ScanOptions) -> io::Result<DepsReport> {
    let report = scan_files(options)?;
    let root = report.root.clone();

    // Node identity: canonicalized paths. Every probe in `resolve` builds
    // candidate paths from these, so plain set lookups stay exact. `nodes`,
    // `languages` and the scan report's files stay index-aligned.
    let nodes: Vec<PathBuf> = report
        .files
        .iter()
        .map(|file| std::fs::canonicalize(&file.path).unwrap_or_else(|_| file.path.clone()))
        .collect();
    let languages: Vec<Language> = report.files.iter().map(|file| file.language).collect();
    let node_set: HashSet<PathBuf> = nodes.iter().cloned().collect();
    let node_index: HashMap<PathBuf, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, path)| (path.clone(), i))
        .collect();

    // Parse and extract in parallel; the trees are walked once and discarded.
    let extracts: Vec<(PathBuf, extract::FileExtract)> = nodes
        .par_iter()
        .zip(&languages)
        .filter_map(|(path, language)| {
            let source = std::fs::read(path).ok()?;
            let tree = crate::langs::parse(*language, &source)?;
            Some((path.clone(), extract::extract(*language, &tree, &source)))
        })
        .collect();
    let extract_map: HashMap<PathBuf, extract::FileExtract> = extracts.into_iter().collect();

    // Manifests for the language indexes: same walk, no include/exclude
    // filter — a Cargo.toml or go.mod outside the filtered set can only ever
    // resolve files that are in it.
    let mut walked = Vec::new();
    for path in &options.paths {
        scan::collect_files(path, &mut walked);
    }
    let cargo_manifests: Vec<PathBuf> = walked
        .iter()
        .filter(|p| p.file_name().is_some_and(|n| n == "Cargo.toml"))
        .filter_map(|p| std::fs::canonicalize(p).ok())
        .collect();
    let go_manifests: Vec<PathBuf> = walked
        .iter()
        .filter(|p| p.file_name().is_some_and(|n| n == "go.mod"))
        .filter_map(|p| std::fs::canonicalize(p).ok())
        .collect();
    let go_files: Vec<PathBuf> = nodes
        .iter()
        .zip(&languages)
        .filter(|(_, language)| **language == Language::Go)
        .map(|(path, _)| path.clone())
        .collect();

    let rust = RustResolver::build(&cargo_manifests, &extract_map, &node_set);
    let java = JavaIndex::build(
        extract_map
            .iter()
            .filter(|(path, _)| Language::from_path(path) == Some(Language::Java)),
    );
    let go = GoIndex::build(&go_manifests, &go_files);

    // Resolve every specifier of every file.
    let mut language_stats: HashMap<Language, LanguageDeps> = HashMap::new();
    let mut raw_edges: Vec<(usize, usize)> = Vec::new();
    for (i, path) in nodes.iter().enumerate() {
        let Some(extract) = extract_map.get(path) else {
            continue;
        };
        let entry = language_stats.entry(languages[i]).or_insert(LanguageDeps {
            language: languages[i],
            files: 0,
            resolved: 0,
            external: 0,
            unresolved: 0,
        });
        entry.files += 1;
        for spec in &extract.specs {
            let resolution = match spec {
                extract::ImportSpec::RustPath { segments } => rust.resolve_use(path, segments),
                extract::ImportSpec::TsSpecifier { specifier } => {
                    resolve::resolve_ts(specifier, path, &node_set)
                }
                extract::ImportSpec::PyModule {
                    module,
                    level,
                    from_name,
                } => resolve::resolve_python(module, *level, from_name, path, &node_set),
                extract::ImportSpec::GoPath { path: import } => go.resolve(import, path),
                extract::ImportSpec::JavaType {
                    fqn,
                    wildcard,
                    is_static,
                } => java.resolve(fqn, *wildcard, *is_static),
            };
            match resolution {
                Resolution::Resolved(targets) => {
                    entry.resolved += 1;
                    for target in targets {
                        if let Some(&to) = node_index.get(&target) {
                            raw_edges.push((i, to));
                        }
                    }
                }
                Resolution::External => entry.external += 1,
                Resolution::Unresolved => entry.unresolved += 1,
            }
        }
    }

    // Graph, fans, cycles.
    let file_graph = graph::FileGraph::new(nodes.len(), raw_edges);
    let fan_ins = file_graph.fan_ins();
    let files: Vec<FileDeps> = nodes
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let fan_in = fan_ins[i];
            let fan_out = file_graph.fan_out(i);
            FileDeps {
                path: path.clone(),
                language: languages[i],
                fan_in,
                fan_out,
                instability: if fan_in + fan_out == 0 {
                    0.0
                } else {
                    f64::from(fan_out) / f64::from(fan_in + fan_out)
                },
            }
        })
        .collect();

    let mut cycles: Vec<Cycle> = file_graph
        .cycles()
        .into_iter()
        .map(|members| Cycle::new(members.into_iter().map(|i| nodes[i].clone()).collect()))
        .collect();
    graph::sort_cycles(&mut cycles);

    let mut language_rows: Vec<LanguageDeps> = language_stats.into_values().collect();
    language_rows.sort_by_key(|stats| stats.language.name());

    Ok(DepsReport {
        root,
        files,
        languages: language_rows,
        cycles,
    })
}

/// One file's change history, as injected by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChurnEntry {
    /// Commits that touched the file.
    pub commits: u32,
    /// Unix seconds of the file's first appearance.
    pub first_seen_unix: i64,
}

/// A file where high fan-in meets high change rate — the only coupling
/// combination with measured support (research §7: 18/1014 locally).
#[derive(Debug, Clone, PartialEq)]
pub struct Hotspot {
    pub path: PathBuf,
    pub fan_in: u32,
    pub commits: u32,
    /// Commits per month since first appearance (age floor 0.5 months, the
    /// same definition as the local measurements).
    pub rate_per_month: f64,
}

/// Join fan-in with churn: files with `fan_in >= min_fan_in` whose change
/// rate is in the top decile of all files that have churn data, ordered by
/// `fan_in × rate`.
pub fn hotspots(
    report: &DepsReport,
    churn: &HashMap<PathBuf, ChurnEntry>,
    now_unix: i64,
    min_fan_in: u32,
) -> Vec<Hotspot> {
    const MONTH: f64 = 30.44 * 86400.0;
    let mut rated: Vec<(f64, &FileDeps, ChurnEntry)> = Vec::new();
    for file in &report.files {
        if let Some(entry) = churn.get(&file.path) {
            let months = ((now_unix - entry.first_seen_unix) as f64 / MONTH).max(0.5);
            rated.push((f64::from(entry.commits) / months, file, *entry));
        }
    }
    if rated.is_empty() {
        return Vec::new();
    }
    // Nearest-rank p90 over every file with history, not just the filtered
    // view — "is this rate unusual" needs the population.
    let mut rates: Vec<f64> = rated.iter().map(|(rate, _, _)| *rate).collect();
    rates.sort_by(|a, b| a.total_cmp(b));
    let rank = (90 * rates.len()).div_ceil(100).clamp(1, rates.len());
    let p90 = rates[rank - 1];

    let mut out: Vec<Hotspot> = rated
        .into_iter()
        .filter(|(_, file, _)| file.fan_in >= min_fan_in)
        .filter(|(rate, _, _)| *rate >= p90)
        .map(|(rate, file, entry)| Hotspot {
            path: file.path.clone(),
            fan_in: file.fan_in,
            commits: entry.commits,
            rate_per_month: rate,
        })
        .collect();
    out.sort_by(|a, b| {
        let key = |hot: &Hotspot| f64::from(hot.fan_in) * hot.rate_per_month;
        key(b).total_cmp(&key(a)).then_with(|| a.path.cmp(&b.path))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_entry(path: &str, fan_in: u32) -> FileDeps {
        FileDeps {
            path: PathBuf::from(path),
            language: Language::Rust,
            fan_in,
            fan_out: 0,
            instability: 0.0,
        }
    }

    fn report(files: &[FileDeps]) -> DepsReport {
        DepsReport {
            root: PathBuf::from("/repo"),
            files: files.to_vec(),
            languages: Vec::new(),
            cycles: Vec::new(),
        }
    }

    fn churn(commits: u32, first_seen_unix: i64) -> ChurnEntry {
        ChurnEntry {
            commits,
            first_seen_unix,
        }
    }

    const NOW: i64 = 1_787_000_000;

    #[test]
    fn hotspots_need_both_fan_in_and_top_decile_rate() {
        // Ten files with strictly rising rates; the nearest-rank p90 is the
        // top one, so only a fan_in-qualifying file at that rate survives.
        let files: Vec<FileDeps> = (1..=10)
            .map(|i| report_entry(&format!("f{i:02}.rs"), if i == 10 { 9 } else { 1 }))
            .collect();
        let mut history = HashMap::new();
        for (i, file) in files.iter().enumerate() {
            history.insert(
                file.path.clone(),
                churn(u32::try_from(i + 1).unwrap(), NOW - 10 * 30 * 86400),
            );
        }
        let hot = hotspots(&report(&files), &history, NOW, 3);
        assert_eq!(hot.len(), 1);
        assert_eq!(hot[0].path, PathBuf::from("f10.rs"));
        assert_eq!(hot[0].fan_in, 9);
    }

    #[test]
    fn hotspot_rate_uses_the_half_month_floor() {
        // First seen "now": age floors at 0.5 months, so rate = commits/0.5.
        let files = vec![report_entry("a.rs", 5)];
        let mut history = HashMap::new();
        history.insert(PathBuf::from("a.rs"), churn(3, NOW));
        let hot = hotspots(&report(&files), &history, NOW, 3);
        assert!((hot[0].rate_per_month - 6.0).abs() < 1e-9);
    }

    #[test]
    fn files_without_history_do_not_qualify() {
        let files = vec![report_entry("a.rs", 9)];
        let hot = hotspots(&report(&files), &HashMap::new(), NOW, 3);
        assert!(hot.is_empty());
    }

    #[test]
    fn resolution_rate_counts_external_as_accounted() {
        let stats = LanguageDeps {
            language: Language::Rust,
            files: 2,
            resolved: 6,
            external: 2,
            unresolved: 2,
        };
        assert!((stats.resolution_rate() - 0.8).abs() < 1e-9);
        assert_eq!(stats.specifiers(), 10);
        assert_eq!(
            LanguageDeps {
                language: Language::Go,
                files: 0,
                resolved: 0,
                external: 0,
                unresolved: 0
            }
            .resolution_rate(),
            1.0
        );
    }
}
