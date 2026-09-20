//! Dependency-direction guard for the S1 terminal seam.

use std::collections::BTreeSet;
use std::path::PathBuf;

const FORBIDDEN_IN_CORE: &[&str] = &[
    "tonic",
    "tonic-prost",
    "prost",
    "prost-build",
    "sqlx",
    "libc",
    "nix",
    "pty-process",
    "vte",
    "tokio",
];

const FORBIDDEN_IN_TERMINAL: &[&str] = &[
    "tonic",
    "tonic-prost",
    "prost",
    "prost-build",
    "qingluan-daemon",
    "qingluan-protocol",
    "qingluan-cli",
    "qingluan-sandbox",
];

fn read_manifest(crate_name: &str) -> String {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "..",
        "crates",
        crate_name,
        "Cargo.toml",
    ]
    .iter()
    .collect();
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()))
}

fn dependency_names(manifest: &str) -> BTreeSet<String> {
    let mut in_dependency_table = false;
    let mut names = BTreeSet::new();

    for raw in manifest.lines() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        if line.starts_with('[') {
            in_dependency_table = matches!(
                line,
                "[dependencies]" | "[dev-dependencies]" | "[build-dependencies]"
            );
        } else if in_dependency_table && let Some((key, _)) = line.split_once('=') {
            names.insert(key.trim().split('.').next().unwrap_or_default().to_owned());
        }
    }

    names
}

fn assert_absent(crate_name: &str, dependencies: &BTreeSet<String>, forbidden: &[&str]) {
    let offenders: Vec<_> = forbidden
        .iter()
        .copied()
        .filter(|name| dependencies.contains(*name))
        .collect();
    assert!(
        offenders.is_empty(),
        "{crate_name} has forbidden dependencies: {offenders:?}"
    );
}

#[test]
fn core_stays_a_pure_domain_layer() {
    let dependencies = dependency_names(&read_manifest("qingluan-core"));
    assert_absent("qingluan-core", &dependencies, FORBIDDEN_IN_CORE);
    assert_absent(
        "qingluan-core",
        &dependencies,
        &[
            "qingluan-terminal",
            "qingluan-storage",
            "qingluan-protocol",
            "qingluan-daemon",
        ],
    );
}

#[test]
fn terminal_depends_only_on_core_in_s1() {
    let dependencies = dependency_names(&read_manifest("qingluan-terminal"));
    assert_absent("qingluan-terminal", &dependencies, FORBIDDEN_IN_TERMINAL);
    assert_eq!(dependencies, BTreeSet::from(["qingluan-core".to_owned()]));
}

#[test]
fn storage_does_not_depend_back_on_terminal() {
    let dependencies = dependency_names(&read_manifest("qingluan-storage"));
    assert_absent("qingluan-storage", &dependencies, &["qingluan-terminal"]);
}
