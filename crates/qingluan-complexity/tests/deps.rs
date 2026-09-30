//! Dependency-graph fixtures: one small tree per language, asserting what
//! resolves, what stays external, and — the load-bearing property — that an
//! unresolvable specifier never fabricates an edge or a cycle.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use qingluan_complexity::{
    ChurnEntry, Cycle, DepsReport, FileDeps, LanguageDeps, ScanOptions, analyze_deps, hotspots,
};
use tempfile::TempDir;

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("file has a parent")).unwrap();
    fs::write(path, contents).unwrap();
}

fn analyze(root: &Path) -> DepsReport {
    analyze_deps(&ScanOptions::new(root)).expect("fixture scans cleanly")
}

fn rel<'a>(report: &'a DepsReport, root: &Path, relative: &str) -> &'a FileDeps {
    let want = root.join(relative);
    report
        .files
        .iter()
        .find(|file| file.path == want)
        .unwrap_or_else(|| {
            panic!(
                "{relative} not in report; have: {:?}",
                report.files.iter().map(|f| &f.path).collect::<Vec<_>>()
            )
        })
}

fn lang(report: &DepsReport, name: &str) -> LanguageDeps {
    *report
        .languages
        .iter()
        .find(|stats| stats.language.name() == name)
        .unwrap_or_else(|| panic!("no stats row for {name}"))
}

fn cycle_members(report: &DepsReport, root: &Path) -> Vec<Vec<String>> {
    report
        .cycles
        .iter()
        .map(|cycle: &Cycle| {
            cycle
                .files
                .iter()
                .map(|path| {
                    path.strip_prefix(root)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .to_string()
                })
                .collect()
        })
        .collect()
}

// ---------------------------------------------------------------- Rust ----

fn rust_fixture(root: &Path) {
    write(
        root,
        "Cargo.toml",
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    );
    // A cross-module cycle: a.rs uses b, b.rs uses a.
    write(root, "src/lib.rs", "mod a;\nmod b;\nuse demo::a::A;\n");
    write(
        root,
        "src/a.rs",
        "use crate::b::helper;\nuse std::fs;\nfn f() { helper(); let _ = fs::read; }\n",
    );
    write(
        root,
        "src/b.rs",
        "use crate::a::A;\npub fn helper() -> A { A }\n",
    );
    write(root, "src/nested/mod.rs", "mod child;\nmod other;\n");
    // `super::` from a plain child file, and `crate::` deeper down.
    write(
        root,
        "src/nested/child.rs",
        "use super::other::O;\nuse crate::a::A;\nfn g(_: O, _: A) {}\n",
    );
    write(root, "src/nested/other.rs", "pub struct O;\n");
    // `#[path]` at the top level of lib.rs and inside a non-root file.
    write(
        root,
        "src/lib.rs",
        "#[path = \"direct.rs\"]\nmod direct;\nmod holder;\nmod a;\nmod b;\nmod nested;\nuse demo::a::A;\n",
    );
    write(root, "src/direct.rs", "pub fn d() {}\n");
    write(
        root,
        "src/holder.rs",
        "#[path = \"p.rs\"]\npub mod inner;\n",
    );
    write(
        root,
        "src/p.rs",
        "use crate::direct;\npub fn h() { direct::d() }\n",
    );
    // `use serde::…` stays external without any Cargo dependency entry.
    write(
        root,
        "src/holder.rs",
        "#[path = \"p.rs\"]\npub mod inner;\nuse serde::Serialize;\n",
    );
    write(
        root,
        "src/p.rs",
        "use crate::direct;\nuse crate::nested::child;\npub fn h() { direct::d(); let _ = child::g; }\n",
    );
}

#[test]
fn rust_resolves_module_paths_and_reports_the_cycle() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    rust_fixture(root);
    let report = analyze(root);

    // crate::b::helper from a.rs: `helper` is an item, the module file b.rs
    // is the edge target.
    let a = rel(&report, root, "src/a.rs");
    assert_eq!(a.fan_out, 1);
    // super::other::O from nested/child.rs lands on nested/other.rs; the
    // same file's crate::a::A is a second out-edge.
    let child = rel(&report, root, "src/nested/child.rs");
    assert_eq!(child.fan_out, 2);
    // lib.rs's own `use demo::a::A` resolves within the crate: root-module
    // child `a`, item `A` — edge to a.rs. (The crate-name shortcut only
    // applies across crates.)
    let lib = rel(&report, root, "src/lib.rs");
    assert_eq!(lib.fan_out, 1);

    // a.rs ↔ b.rs is the only cycle; same directory, so not cross-directory.
    assert_eq!(
        cycle_members(&report, root),
        vec![vec!["src/a.rs", "src/b.rs"]]
    );
    assert!(!report.cycles[0].cross_directory);

    let rust = lang(&report, "rust");
    // std::fs and serde::Serialize are external; every local path above
    // resolves (lib 1, a 1, b 1, child 2, p 2 = 7 specifiers).
    assert_eq!(rust.external, 2);
    assert_eq!(rust.unresolved, 0, "no misses in this fixture");
    assert_eq!(rust.resolved, 7);
    assert_eq!(rust.files, 9);
}

#[test]
fn rust_path_attribute_follows_the_verified_directory_rules() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    rust_fixture(root);
    let report = analyze(root);

    // #[path = "p.rs"] inside src/holder.rs resolves relative to src/, so
    // p.rs exists in the module tree and its `use crate::direct` resolves.
    let p = rel(&report, root, "src/p.rs");
    assert_eq!(p.fan_out, 2, "crate::direct + crate::nested::child");
    // Module *containment* is not an import edge: nothing `use`s p.rs, so
    // its fan-in is 0 — the module tree made it resolvable, not depended on.
    assert_eq!(p.fan_in, 0);
    // direct.rs is depended on only through the #[path]-reached module.
    let direct = rel(&report, root, "src/direct.rs");
    assert_eq!(direct.fan_in, 1);
}

#[test]
fn rust_workspace_crates_link_by_name() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(
        root,
        "lib-one/Cargo.toml",
        "[package]\nname = \"lib-one\"\nversion = \"0.1.0\"\n",
    );
    write(root, "lib-one/src/lib.rs", "pub fn one() {}\n");
    write(
        root,
        "app-two/Cargo.toml",
        "[package]\nname = \"app-two\"\nversion = \"0.1.0\"\n",
    );
    write(
        root,
        "app-two/src/main.rs",
        "use lib_one::one;\nfn main() { one() }\n",
    );

    let report = analyze(root);
    // The hyphen becomes an underscore in the use path; the edge lands on
    // the dependency crate's lib root.
    let main = rel(&report, root, "app-two/src/main.rs");
    assert_eq!(main.fan_out, 1);
    let lib = rel(&report, root, "lib-one/src/lib.rs");
    assert_eq!(lib.fan_in, 1);
    assert_eq!(lang(&report, "rust").unresolved, 0);
}

// ------------------------------------------------------------- Python ----

#[test]
fn python_resolves_relative_absolute_and_from_import_leaves() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(root, "pkg/__init__.py", "");
    write(
        root,
        "pkg/mod.py",
        "from . import other\nfrom .sub import thing\nimport json\n",
    );
    write(root, "pkg/other.py", "x = 1\n");
    write(root, "pkg/sub.py", "thing = 1\n");

    let report = analyze(root);
    let module = rel(&report, root, "pkg/mod.py");
    // `.` → pkg package file; `.sub` → pkg/sub.py (submodule probe wins over
    // an attribute in __init__.py); `import json` → external.
    assert_eq!(module.fan_out, 2);
    // Named-module attribution: `from . import other` depends on other.py;
    // the package's own __init__.py is a side effect, not the edge.
    assert_eq!(rel(&report, root, "pkg/__init__.py").fan_in, 0);
    assert_eq!(rel(&report, root, "pkg/sub.py").fan_in, 1);
    let python = lang(&report, "python");
    assert_eq!(python.resolved, 2);
    assert_eq!(python.external, 1);
    assert_eq!(python.unresolved, 0);
    assert!(report.cycles.is_empty());
}

#[test]
fn python_relative_miss_is_unresolved_but_absolute_miss_is_external() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(root, "solo.py", "from . import nothing\nimport os\n");

    let report = analyze(root);
    let python = lang(&report, "python");
    // `from . import` in a top-level script has no package: a relative miss.
    assert_eq!(python.unresolved, 1);
    // `import os` lands nowhere in the repo: external, not a miss.
    assert_eq!(python.external, 1);
}

// ---------------------------------------------------------- TypeScript ----

#[test]
fn typescript_resolves_relative_specifiers_only() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(
        root,
        "app/a.ts",
        "import { x } from \"./b\";\nimport { y } from \"./c.js\";\nimport { z } from \"./dir\";\nconst r = require(\"./b\");\n",
    );
    write(
        root,
        "app/b.ts",
        "import { a } from \"./a\";\nexport const x = 1;\n",
    );
    write(root, "app/c.ts", "export const y = 1;\n");
    write(root, "app/dir/index.ts", "export const z = 1;\n");
    write(
        root,
        "app/alias.ts",
        "import { q } from \"@/missing\";\nimport { p } from \"react\";\n",
    );

    let report = analyze(root);
    let a = rel(&report, root, "app/a.ts");
    // ./b (extensionless), ./c.js → c.ts (substitution), ./dir → index.ts,
    // and require("./b") deduplicates against the first edge.
    assert_eq!(a.fan_out, 3);
    let ts = lang(&report, "typescript");
    assert_eq!(
        ts.resolved, 5,
        "three imports + require on a.ts, one on b.ts"
    );
    assert_eq!(ts.external, 1, "react");
    assert_eq!(ts.unresolved, 1, "@/missing is a visible alias miss");
    // a ↔ b is a real cycle.
    assert_eq!(
        cycle_members(&report, root),
        vec![vec!["app/a.ts", "app/b.ts"]]
    );
}

#[test]
fn an_unresolvable_edge_never_fabricates_a_cycle() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    // x → y resolves; y's only way back is an alias we cannot resolve. If
    // resolution guessed here, this fixture would report a phantom cycle.
    write(root, "x.ts", "import \"./y\";\n");
    write(root, "y.ts", "import \"@/x\";\n");

    let report = analyze(root);
    assert!(report.cycles.is_empty());
    assert_eq!(rel(&report, root, "x.ts").fan_out, 1);
    assert_eq!(rel(&report, root, "y.ts").fan_out, 0);
    assert_eq!(lang(&report, "typescript").unresolved, 1);
}

// ------------------------------------------------------------------ Go ----

#[test]
fn go_edges_follow_the_module_path_to_package_directories() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(root, "go.mod", "module example.com/m\n\ngo 1.22\n");
    write(
        root,
        "main.go",
        "package main\nimport (\n\t\"fmt\"\n\t\"example.com/m/util\"\n)\nfunc main() { fmt.Println(util.U()); util.V() }\n",
    );
    write(
        root,
        "util/x.go",
        "package util\nfunc U() int { return 1 }\n",
    );
    write(
        root,
        "util/y.go",
        "package util\nfunc V() int { return 2 }\n",
    );

    let report = analyze(root);
    let main = rel(&report, root, "main.go");
    // The import fans out to every file of the util package.
    assert_eq!(main.fan_out, 2);
    assert_eq!(rel(&report, root, "util/x.go").fan_in, 1);
    assert_eq!(rel(&report, root, "util/y.go").fan_in, 1);
    let go = lang(&report, "go");
    assert_eq!(go.resolved, 1);
    assert_eq!(go.external, 1, "fmt");
    assert_eq!(go.unresolved, 0);
}

#[test]
fn go_without_go_mod_treats_every_import_as_external() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(
        root,
        "main.go",
        "package main\nimport \"fmt\"\nfunc main() {}\n",
    );
    let report = analyze(root);
    assert_eq!(lang(&report, "go").external, 1);
    assert_eq!(lang(&report, "go").resolved, 0);
}

// ------------------------------------------------------------------ Java ----

#[test]
fn java_resolves_types_containers_and_reports_wildcards_as_misses() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(
        root,
        "src/com/example/A.java",
        "package com.example;\nimport com.example.helper.B;\nimport com.example.helper.*;\nimport java.util.List;\nimport static com.example.C.CONST;\nclass A { B b; List<String> l; static final int K = CONST; }\n",
    );
    write(
        root,
        "src/com/example/helper/B.java",
        "package com.example.helper;\npublic class B {}\n",
    );
    write(
        root,
        "src/com/example/C.java",
        "package com.example;\npublic class C { public static final int CONST = 1; }\n",
    );

    let report = analyze(root);
    let a = rel(&report, root, "src/com/example/A.java");
    // B (type index) and C (static container); the wildcard adds nothing.
    assert_eq!(a.fan_out, 2);
    let java = lang(&report, "java");
    assert_eq!(java.resolved, 2);
    assert_eq!(java.external, 1, "java.util.List: unknown package");
    assert_eq!(
        java.unresolved, 1,
        "com.example.helper.*: known package, no type"
    );
    assert!(report.cycles.is_empty());
}

#[test]
fn java_same_package_references_are_invisible_but_do_not_fabricate() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    // Same-package use writes no import statement at all: the edge is
    // invisible, and nothing may be invented in its place.
    write(root, "one/P.java", "package one;\nclass P { two.Q q; }\n");
    write(root, "two/Q.java", "package two;\nclass Q { one.P p; }\n");

    let report = analyze(root);
    assert!(report.cycles.is_empty());
    assert_eq!(rel(&report, root, "one/P.java").fan_out, 0);
    // Not even a specifier exists, so the miss is invisible by construction
    // (documented ceiling, not an `unresolved` count).
    assert_eq!(lang(&report, "java").specifiers(), 0);
}

// ------------------------------------------------------------ degenerate ----

#[test]
fn a_single_file_repo_reports_no_cycles_and_zero_edges() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(root, "main.py", "import os\nprint(os)\n");
    let report = analyze(root);
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].fan_in, 0);
    assert_eq!(report.files[0].fan_out, 0);
    assert_eq!(report.files[0].instability, 0.0);
    assert!(report.cycles.is_empty());
}

#[test]
fn an_empty_directory_yields_an_empty_report() {
    let dir = TempDir::new().unwrap();
    let report = analyze(dir.path());
    assert!(report.files.is_empty());
    assert!(report.languages.is_empty());
    assert!(report.cycles.is_empty());
}

#[test]
fn instability_spans_zero_for_pure_targets_and_one_for_pure_importers() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(root, "leaf.ts", "export const x = 1;\n");
    write(
        root,
        "uses.ts",
        "import { x } from \"./leaf\";\nconsole.log(x);\n",
    );

    let report = analyze(root);
    let leaf = rel(&report, root, "leaf.ts");
    let uses = rel(&report, root, "uses.ts");
    // I = Ce/(Ca+Ce): the leaf is maximally responsible (0), the importer
    // maximally dependent (1).
    assert_eq!(leaf.instability, 0.0);
    assert_eq!(uses.instability, 1.0);
}

// ---------------------------------------------------------------- churn ----

#[test]
fn injected_churn_selects_hotspots_without_touching_a_vcs() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(root, "steady.ts", "export const a = 1;\n");
    write(
        root,
        "hot.ts",
        "import { a } from \"./steady\";\nexport const b = [a, a, a];\n",
    );
    let report = analyze(root);
    let steady = rel(&report, root, "steady.ts").path.clone();

    let now = 1_787_000_000i64;
    let mut history = HashMap::new();
    history.insert(
        steady.clone(),
        ChurnEntry {
            commits: 40,
            first_seen_unix: now - 10 * 30 * 86400,
        },
    );
    let hot = hotspots(&report, &history, now, 3);
    // fan_in of steady.ts is 1 — below the bar, churn alone must not qualify.
    assert!(hot.is_empty());
}
