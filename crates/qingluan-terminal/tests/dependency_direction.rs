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
    "sqlx",
    "qingluan-daemon",
    "qingluan-protocol",
    "qingluan-cli",
    "qingluan-sandbox",
];

/// Dependencies the terminal seam must have (plan §2 / design baseline):
/// the core domain types, PTY allocation/spawn (`pty-process` 0.5.3), the
/// cgroup/term/signal helpers (`nix` 0.31.3), raw pidfd/pipe syscalls
/// (`libc`), and the async runtime for the self-managed `AsyncFd` write
/// path.
const REQUIRED_IN_TERMINAL: &[&str] = &["qingluan-core", "libc", "nix", "pty-process", "tokio"];

/// The full approved dependency set, including the narrow S2 storage seam
/// (`qingluan-storage`, per the plan's `daemon → terminal → storage → core`
/// direction), the UUID minter for canonical `TerminalId`/`LogEpoch` text,
/// and the crate's own `test-hooks` self dev-dependency. Any other
/// dependency is an unapproved widening.
const APPROVED_IN_TERMINAL: &[&str] = &[
    "qingluan-core",
    "qingluan-storage",
    "qingluan-terminal",
    "libc",
    "nix",
    "pty-process",
    "tokio",
    "uuid",
];

const FORBIDDEN_IN_STORAGE: &[&str] = &[
    "qingluan-terminal",
    "qingluan-daemon",
    "qingluan-protocol",
    "qingluan-cli",
    "qingluan-sandbox",
    "tonic",
    "tonic-prost",
    "prost",
    "prost-build",
    "pty-process",
    "vte",
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
fn terminal_depends_only_on_core_and_the_approved_pinned_stack() {
    let dependencies = dependency_names(&read_manifest("qingluan-terminal"));
    assert_absent("qingluan-terminal", &dependencies, FORBIDDEN_IN_TERMINAL);
    for name in REQUIRED_IN_TERMINAL {
        assert!(
            dependencies.contains(*name),
            "qingluan-terminal must depend on {name}"
        );
    }
    let mut unexpected: Vec<&str> = dependencies
        .iter()
        .map(String::as_str)
        .filter(|name| !APPROVED_IN_TERMINAL.contains(name))
        .collect();
    unexpected.sort_unstable();
    assert!(
        unexpected.is_empty(),
        "qingluan-terminal gained unapproved dependencies: {unexpected:?}"
    );
}

#[test]
fn storage_does_not_depend_back_on_terminal() {
    let dependencies = dependency_names(&read_manifest("qingluan-storage"));
    assert_absent("qingluan-storage", &dependencies, &["qingluan-terminal"]);
}

#[test]
fn storage_stays_below_terminal_and_gains_no_pty_or_wire_deps() {
    let dependencies = dependency_names(&read_manifest("qingluan-storage"));
    assert_absent("qingluan-storage", &dependencies, FORBIDDEN_IN_STORAGE);
}
