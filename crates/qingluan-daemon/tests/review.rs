//! Integration tests for review diff extraction against a real `jj`
//! subprocess. Skipped (with a message) when `jj` is not on PATH, so CI
//! environments without jj stay green.

use std::path::{Path, PathBuf};
use std::process::Command;

use qingluan_daemon::review::{
    ChangeStatus, ReviewError, create_session, parse_git_diff, run_jj_diff,
};

/// Run jj in `dir` with a test identity; panic on failure.
fn jj(dir: &Path, args: &[&str]) {
    let output = Command::new("jj")
        .current_dir(dir)
        .args(["--no-pager", "--color=never"])
        // The user's global config signs commits with GPG; test identities
        // have no secret key, so drop signing for these scratch repos.
        .arg("--config=signing.behavior=drop")
        .arg("--config=user.name=Test")
        .arg("--config=user.email=test@example.com")
        .args(args)
        .output()
        .expect("spawn jj");
    assert!(
        output.status.success(),
        "jj {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Whether the `jj` binary is available.
fn have_jj() -> bool {
    Command::new("jj")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Unique temp dir for one test.
fn temp_repo(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("qingluan-review-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

#[test]
fn extract_snapshots_committed_and_working_copy_changes() {
    if !have_jj() {
        eprintln!("skipping: jj not on PATH");
        return;
    }
    let dir = temp_repo("extract");
    jj(&dir, &["git", "init", "."]);

    // Base state: two files committed as `base`; `main` points at that
    // commit (@- after `jj commit` leaves @ on a fresh empty child).
    // Direct file writes need a real snapshot, which `bookmark create`
    // alone does not trigger.
    write(&dir, "keep.txt", "kept\n");
    write(&dir, "modify.txt", "line one\nline two\n");
    jj(&dir, &["commit", "-m", "base"]);
    jj(&dir, &["bookmark", "create", "main", "-r", "@-"]);

    // Reviewed change: modify (committed), add + delete (working copy).
    write(
        &dir,
        "modify.txt",
        "line one changed\nline two\nline three\n",
    );
    write(&dir, "added.txt", "fresh\n");
    std::fs::remove_file(dir.join("keep.txt")).unwrap();
    jj(&dir, &["commit", "-m", "half the change"]);

    let session = create_session(&dir, "main", "@").expect("session");
    let by_path = |p: &str| {
        session
            .files
            .iter()
            .find(|f| f.path == p)
            .unwrap_or_else(|| panic!("missing {p} in {:?}", session.files))
            .clone()
    };

    assert_eq!(session.from, "main");
    assert_eq!(session.to, "@");

    let modified = by_path("modify.txt");
    assert_eq!(modified.status, ChangeStatus::Modified);
    assert_eq!(modified.additions, 2);
    assert_eq!(modified.deletions, 1);

    let added = by_path("added.txt");
    assert_eq!(added.status, ChangeStatus::Added);
    assert_eq!(added.additions, 1);

    let deleted = by_path("keep.txt");
    assert_eq!(deleted.status, ChangeStatus::Deleted);
    assert_eq!(deleted.deletions, 1);

    // Full contents (max-context reconstruction): verify against the store
    // shape via parse of the raw diff — create_session already consumed it,
    // so re-run for content assertions.
    let raw = run_jj_diff(&dir, "main", "@").unwrap();
    let parsed = parse_git_diff(&raw);
    let (_, contents) = parsed
        .iter()
        .find(|(m, _)| m.path == "modify.txt")
        .expect("modify.txt in diff");
    assert_eq!(contents.old, "line one\nline two\n");
    assert_eq!(contents.new, "line one changed\nline two\nline three\n");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn plain_directory_without_jj_repo_is_rejected() {
    if !have_jj() {
        eprintln!("skipping: jj not on PATH");
        return;
    }
    let dir = temp_repo("norepo");
    // Empty directory: not inside any jj repository.
    match create_session(&dir, "main", "@") {
        Err(ReviewError::NotARepo { dir: d }) => assert!(d.contains("qingluan-review-test")),
        other => panic!("expected NotARepo, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bad_revset_surfaces_jj_error() {
    if !have_jj() {
        eprintln!("skipping: jj not on PATH");
        return;
    }
    let dir = temp_repo("badrev");
    jj(&dir, &["git", "init", "."]);
    write(&dir, "a.txt", "a\n");
    match create_session(&dir, "no-such-bookmark", "@") {
        Err(ReviewError::JjFailed { .. }) => {}
        other => panic!("expected JjFailed, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}
