//! Binary-level tests for `qingluan workspace` commands.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_qingluan"))
}

fn tempdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "qingluan-cli-ws-test-{}-{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn workspace_list_json_outside_jj_fails_with_machine_readable_stderr() {
    let dir = tempdir();
    let out = Command::new(binary())
        .args(["workspace", "list", "--json"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);

    assert!(!out.status.success(), "expected nonzero exit");
    assert!(
        out.stdout.is_empty(),
        "stdout must stay machine-clean, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let value: serde_json::Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"], "not_in_jj_repository");
    assert!(value["message"].is_string());
}

#[test]
fn workspace_list_json_in_jj_repo_emits_catalog() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap();
    if !repo_root.join(".jj").exists() || !jj_available() {
        eprintln!("skipping: workspace root is not a jj repo or jj missing");
        return;
    }
    let out = Command::new(binary())
        .args(["workspace", "list", "--json"])
        .current_dir(repo_root)
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).expect("catalog JSON");
    assert_eq!(value["schemaVersion"], 1);
    assert!(!value["workspaces"].as_array().unwrap().is_empty());
    // Semantic camelCase fields are present on each workspace.
    for ws in value["workspaces"].as_array().unwrap() {
        assert!(ws["name"].is_string());
        assert!(ws["root"].is_string());
        assert!(ws["available"].is_boolean());
        assert!(ws["unavailableReason"].is_null() || ws["unavailableReason"].is_string());
    }
}

fn jj_available() -> bool {
    Command::new("jj")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}
