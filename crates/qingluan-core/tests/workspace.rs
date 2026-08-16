//! Vertical-slice tests for `qingluan_core::workspace`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use qingluan_core::workspace::{
    self, WorkspaceError, build_catalog, discover, parse_workspace_list_output, scan_sessions_root,
    summarize_session_lines,
};

fn lines_of(chunks: &[&str]) -> Vec<Result<String, std::io::Error>> {
    chunks.iter().map(|c| Ok((*c).to_owned())).collect()
}

fn summarize(chunks: &[&str]) -> Option<workspace::ScannedSession> {
    summarize_session_lines(
        lines_of(chunks).into_iter(),
        Path::new("/tmp/fake/session.jsonl"),
        111_111,
    )
}

fn write_session(root: &Path, dir: &str, file: &str, header: &str, rest: &[&str]) -> PathBuf {
    let dir = root.join(dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(file);
    let mut f = fs::File::create(&path).unwrap();
    writeln!(f, "{header}").unwrap();
    for line in rest {
        writeln!(f, "{line}").unwrap();
    }
    path
}

// ── Pi 0.84.2 display semantics ───────────────────────────────────────────

#[test]
fn title_prefers_latest_nonblank_session_info_name() {
    let s = summarize(&[
        r#"{"type":"session","id":"s1","cwd":"/w","timestamp":"2026-08-16T10:00:00.000Z"}"#,
        r#"{"type":"message","id":"m1","timestamp":"2026-08-16T10:01:00.000Z","message":{"role":"user","content":[{"type":"text","text":"first user text"}]}}"#,
        r#"{"type":"session_info","id":"i1","timestamp":"2026-08-16T10:02:00.000Z","name":"named session"}"#,
    ])
    .unwrap();
    assert_eq!(s.title, "named session");

    // A later blank session_info clears the name → falls back to first user text.
    let s = summarize(&[
        r#"{"type":"session","id":"s1","cwd":"/w","timestamp":"2026-08-16T10:00:00.000Z"}"#,
        r#"{"type":"message","id":"m1","timestamp":"2026-08-16T10:01:00.000Z","message":{"role":"user","content":[{"type":"text","text":"first user text"}]}}"#,
        r#"{"type":"session_info","id":"i1","timestamp":"2026-08-16T10:02:00.000Z","name":"named session"}"#,
        r#"{"type":"session_info","id":"i2","timestamp":"2026-08-16T10:03:00.000Z","name":"   "}"#,
    ])
    .unwrap();
    assert_eq!(s.title, "first user text");
}

#[test]
fn title_from_first_nonempty_user_text_string_or_blocks() {
    // Assistant text first (string content), then user blocks: user text wins.
    let s = summarize(&[
        r#"{"type":"session","id":"s1","cwd":"/w","timestamp":"2026-08-16T10:00:00.000Z"}"#,
        r#"{"type":"message","id":"m1","timestamp":"2026-08-16T10:01:00.000Z","message":{"role":"assistant","content":"assistant says hi","timestamp":1786876860000}}"#,
        r#"{"type":"message","id":"m2","timestamp":"2026-08-16T10:02:00.000Z","message":{"role":"user","content":[{"type":"thinking","thinking":"x"},{"type":"text","text":"hello "},{"type":"text","text":"blocks"}],"timestamp":1786876920000}}"#,
    ])
    .unwrap();
    assert_eq!(s.title, "hello  blocks");

    // No user text at all → (no messages).
    let s = summarize(&[
        r#"{"type":"session","id":"s1","cwd":"/w","timestamp":"2026-08-16T10:00:00.000Z"}"#,
        r#"{"type":"message","id":"m1","timestamp":"2026-08-16T10:01:00.000Z","message":{"role":"assistant","content":[],"timestamp":1786876860000}}"#,
    ])
    .unwrap();
    assert_eq!(s.title, "(no messages)");
    // Header-only file.
    let s = summarize(&[
        r#"{"type":"session","id":"s1","cwd":"/w","timestamp":"2026-08-16T10:00:00.000Z"}"#,
    ])
    .unwrap();
    assert_eq!(s.title, "(no messages)");
}

#[test]
fn message_count_counts_all_message_entries() {
    let s = summarize(&[
        r#"{"type":"session","id":"s1","cwd":"/w","timestamp":"2026-08-16T10:00:00.000Z"}"#,
        // legacy entry without id
        r#"{"type":"message","timestamp":"2026-08-16T10:01:00.000Z","message":{"role":"user","content":"hi","timestamp":1786876860000}}"#,
        // toolResult with content: counted, but no activity/title contribution
        r#"{"type":"message","id":"m2","timestamp":"2026-08-16T10:05:00.000Z","message":{"role":"toolResult","content":[{"type":"text","text":"out"}],"timestamp":1786877100000}}"#,
        // message without content field: counted only
        r#"{"type":"message","id":"m3","timestamp":"2026-08-16T10:06:00.000Z","message":{"role":"user"}}"#,
        "",
        "not json at all",
        r#"{"type":"model_change","id":"x","timestamp":"2026-08-16T10:07:00.000Z"}"#,
    ])
    .unwrap();
    assert_eq!(s.message_count, 3);
    assert_eq!(s.title, "hi");
}

#[test]
fn modified_prefers_message_ms_then_entry_iso_then_header_then_mtime() {
    // a) numeric message.timestamp ms wins over later entry ISO fallback.
    let s = summarize(&[
        r#"{"type":"session","id":"s1","cwd":"/w","timestamp":"2026-08-16T10:00:00.000Z"}"#,
        r#"{"type":"message","id":"m1","timestamp":"2026-08-16T23:59:59.000Z","message":{"role":"user","content":"a","timestamp":1000}}"#,
        r#"{"type":"message","id":"m2","timestamp":"2026-08-16T20:00:00.000Z","message":{"role":"assistant","content":"b"}}"#,
    ])
    .unwrap();
    assert_eq!(s.modified_ms(), 1_786_910_400_000); // later entry ISO wins over small ms value

    // b) max over numeric ms.
    let s = summarize(&[
        r#"{"type":"session","id":"s1","cwd":"/w","timestamp":"2026-08-16T10:00:00.000Z"}"#,
        r#"{"type":"message","id":"m1","timestamp":"2026-08-16T10:01:00.000Z","message":{"role":"user","content":"a","timestamp":1786876860000}}"#,
        r#"{"type":"message","id":"m2","timestamp":"2026-08-16T10:02:00.000Z","message":{"role":"assistant","content":"b","timestamp":1786876920000}}"#,
    ])
    .unwrap();
    assert_eq!(s.modified_ms(), 1_786_876_920_000);

    // c) header fallback when no qualifying messages.
    let s = summarize(&[
        r#"{"type":"session","id":"s1","cwd":"/w","timestamp":"2026-08-16T10:00:00.000Z"}"#,
        r#"{"type":"message","id":"m1","timestamp":"2026-08-16T10:01:00.000Z","message":{"role":"toolResult","content":"out"}}"#,
    ])
    .unwrap();
    assert_eq!(s.modified_ms(), 1_786_874_400_000);

    // d) mtime fallback when header timestamp is unparseable.
    let s = summarize(&[r#"{"type":"session","id":"s1","cwd":"/w","timestamp":"bogus"}"#]).unwrap();
    assert_eq!(s.modified_ms(), 111_111);
}

#[test]
fn invalid_when_first_valid_entry_is_not_a_session_header() {
    assert!(
        summarize(&[
            r#"{"type":"message","id":"m1","message":{"role":"user","content":"hi"}}"#,
            r#"{"type":"session","id":"s1","cwd":"/w","timestamp":"2026-08-16T10:00:00.000Z"}"#,
        ])
        .is_none()
    );
    // Blank/malformed leading lines are tolerated; header must still appear.
    assert!(
        summarize(&[
            "",
            "garbage",
            r#"{"type":"session","id":"s1","cwd":"/w","timestamp":"2026-08-16T10:00:00.000Z"}"#,
        ])
        .is_some()
    );
    // Empty file is not a session.
    assert!(summarize(&[]).is_none());
}

#[test]
fn falsey_leading_json_values_are_skipped_before_the_header() {
    // Pi's reader treats parsed falsey values (null/false/0/"") like blank
    // lines: skipped, not "a non-session first entry".
    let s = summarize(&[
        "null",
        "false",
        "0",
        "0.0",
        "\"\"",
        r#"{"type":"session","id":"s1","cwd":"/w","timestamp":"2026-08-16T10:00:00.000Z"}"#,
        r#"{"type":"message","id":"m","timestamp":"2026-08-16T10:01:00.000Z","message":{"role":"user","content":"hi","timestamp":1786876860000}}"#,
    ])
    .unwrap();
    assert_eq!(s.title, "hi");
    assert_eq!(s.session_id.as_deref(), Some("s1"));
}

#[test]
fn header_requires_string_id_and_absolute_cwd() {
    let ok = |header: &str| summarize(&[header]).is_some();
    let ts = "\"timestamp\":\"2026-08-16T10:00:00.000Z\"";
    // Valid header: string id + absolute cwd.
    assert!(ok(&format!(
        r#"{{"type":"session","id":"s1","cwd":"/w",{ts}}}"#
    )));
    // Pi's header validator rejects missing/non-string id.
    assert!(!ok(&format!(r#"{{"type":"session","cwd":"/w",{ts}}}"#)));
    assert!(!ok(&format!(
        r#"{{"type":"session","id":42,"cwd":"/w",{ts}}}"#
    )));
    // Safe association: missing/non-string/empty/non-absolute cwd rejects the
    // whole file (never lexically resolves against the process cwd).
    assert!(!ok(&format!(r#"{{"type":"session","id":"s1",{ts}}}"#)));
    assert!(!ok(&format!(
        r#"{{"type":"session","id":"s1","cwd":3,{ts}}}"#
    )));
    assert!(!ok(&format!(
        r#"{{"type":"session","id":"s1","cwd":"",{ts}}}"#
    )));
    assert!(!ok(&format!(
        r#"{{"type":"session","id":"s1","cwd":"relative/ws",{ts}}}"#
    )));
    assert!(!ok(&format!(
        r#"{{"type":"session","id":"s1","cwd":".",{ts}}}"#
    )));
}

// ── Sessions-root scanning ────────────────────────────────────────────────

#[test]
fn scan_root_reads_only_one_level_jsonl_files() {
    let tmp = tempdir();
    let root = tmp.path().join("sessions");
    write_session(
        &root,
        "--w--",
        "a.jsonl",
        r#"{"type":"session","id":"a","cwd":"/w","timestamp":"2026-08-16T10:00:00.000Z"}"#,
        &[
            r#"{"type":"message","id":"m","timestamp":"2026-08-16T10:01:00.000Z","message":{"role":"user","content":"in root scan"}}"#,
        ],
    );
    // Nested subagent artifacts must never be scanned.
    write_session(
        &root.join("--w--").join("nested-run"),
        "deeper",
        "b.jsonl",
        r#"{"type":"session","id":"b","cwd":"/w","timestamp":"2026-08-16T10:00:00.000Z"}"#,
        &[],
    );
    // Non-jsonl files ignored.
    fs::write(root.join("--w--").join("notes.txt"), "x").unwrap();
    fs::create_dir_all(root.join("--other--")).unwrap();

    let scanned = scan_sessions_root(&root);
    assert_eq!(scanned.len(), 1);
    assert_eq!(scanned[0].session_id.as_deref(), Some("a"));
    assert_eq!(scanned[0].title, "in root scan");

    // Missing root: no sessions, no error.
    assert!(scan_sessions_root(&root.join("does-not-exist")).is_empty());
}

#[test]
fn scan_root_follows_top_level_symlink_dirs_and_jsonl_symlinks() {
    // Pi's listAll takes top-level directories *and* symlinks, and accepts
    // any `*.jsonl` name (symlinks included); mirror that locally.
    #[cfg(unix)]
    {
        let tmp = tempdir();
        let root = tmp.path().join("sessions");
        // Session file lives directly in the real dir; the scan reaches it
        // through the top-level symlink (still exactly one level).
        write_session(
            tmp.path(),
            "real-dir",
            "s.jsonl",
            r#"{"type":"session","id":"via-symlink-dir","cwd":"/w","timestamp":"2026-08-16T10:00:00.000Z"}"#,
            &[],
        );
        let real_dir = tmp.path().join("real-dir");
        fs::create_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(&real_dir, root.join("alias-dir")).unwrap();

        // A `.jsonl` symlink in a level-1 dir is accepted (target lives at
        // top level, which the one-level scan never reads).
        let target = root.join("target.jsonl");
        let header = r#"{"type":"session","id":"via-symlink-file","cwd":"/w","timestamp":"2026-08-16T10:00:00.000Z"}"#;
        {
            let mut f = fs::File::create(&target).unwrap();
            writeln!(f, "{header}").unwrap();
        }
        let level1 = root.join("--w--");
        fs::create_dir_all(&level1).unwrap();
        std::os::unix::fs::symlink(&target, level1.join("link.jsonl")).unwrap();

        let mut scanned = scan_sessions_root(&root);
        scanned.sort_by(|a, b| a.file.cmp(&b.file));
        assert_eq!(scanned.len(), 2);
        assert_eq!(scanned[0].session_id.as_deref(), Some("via-symlink-file"));
        assert_eq!(scanned[1].session_id.as_deref(), Some("via-symlink-dir"));
    }
}

// ── Association / catalog ─────────────────────────────────────────────────

fn header_for(cwd: &str) -> String {
    format!(r#"{{"type":"session","id":"x","cwd":"{cwd}","timestamp":"2026-08-16T10:00:00.000Z"}}"#)
}

#[test]
fn catalog_associates_by_normalized_path_and_sorts_sessions_desc() {
    let tmp = tempdir();
    let root = tmp.path().join("sessions");
    let ws_real = tmp.path().join("real-ws");
    fs::create_dir_all(&ws_real).unwrap();
    // Symlink alias for the session cwd: must still match after canonicalization.
    #[cfg(unix)]
    std::os::unix::fs::symlink(&ws_real, tmp.path().join("alias-ws")).unwrap();

    write_session(
        &root,
        "d1",
        "old.jsonl",
        &header_for(ws_real.to_str().unwrap()),
        &[
            r#"{"type":"message","id":"m","timestamp":"2026-08-16T10:01:00.000Z","message":{"role":"user","content":"old","timestamp":1000000}}"#,
        ],
    );
    #[cfg(unix)]
    write_session(
        &root,
        "d1",
        "new.jsonl",
        &header_for(tmp.path().join("alias-ws").to_str().unwrap()),
        &[
            r#"{"type":"message","id":"m","timestamp":"2026-08-16T10:01:00.000Z","message":{"role":"user","content":"new","timestamp":2000000}}"#,
        ],
    );
    // Foreign session: belongs to no registered workspace → omitted.
    write_session(
        &root,
        "d2",
        "foreign.jsonl",
        &header_for("/somewhere/else"),
        &[],
    );

    let registered = vec![workspace::RegisteredWorkspace {
        name: "main".into(),
        root: ws_real.to_string_lossy().into_owned(),
    }];
    let catalog = build_catalog(&registered, scan_sessions_root(&root));

    assert_eq!(catalog.schema_version, 1);
    assert_eq!(catalog.workspaces.len(), 1);
    let ws = &catalog.workspaces[0];
    assert!(ws.available);
    assert_eq!(ws.unavailable_reason, None);
    assert_eq!(ws.sessions.len(), 2);
    // Sorted by modified desc.
    assert_eq!(ws.sessions[0].title, "new");
    assert_eq!(ws.sessions[1].title, "old");
    assert_eq!(ws.sessions[0].modified, "1970-01-01T00:33:20.000Z");
    let json = serde_json::to_value(&catalog).unwrap();
    assert_eq!(json["schemaVersion"], 1);
    assert_eq!(json["workspaces"][0]["sessions"][0]["messageCount"], 1);
}

#[test]
fn catalog_keeps_unavailable_workspace_representable() {
    let tmp = tempdir();
    let root = tmp.path().join("sessions");
    let missing_root = tmp.path().join("gone-ws");
    write_session(
        &root,
        "d",
        "s.jsonl",
        &header_for(missing_root.to_str().unwrap()),
        &[],
    );

    let registered = vec![workspace::RegisteredWorkspace {
        name: "gone".into(),
        root: missing_root.to_string_lossy().into_owned(),
    }];
    let catalog = build_catalog(&registered, scan_sessions_root(&root));

    assert_eq!(catalog.workspaces.len(), 1);
    let ws = &catalog.workspaces[0];
    assert!(!ws.available);
    assert!(ws.unavailable_reason.is_some());
    // Its matching session stays associated (represented as unavailable).
    assert_eq!(ws.sessions.len(), 1);
}

#[test]
fn sessions_with_missing_or_empty_cwd_never_associate_to_the_process_cwd() {
    // Regression: an empty/missing header cwd used to lexically normalize to
    // the process cwd and silently land in the current workspace. Such files
    // are now rejected during summary validation.
    let tmp = tempdir();
    let root = tmp.path().join("sessions");
    let ts = "\"timestamp\":\"2026-08-16T10:00:00.000Z\"";
    write_session(
        &root,
        "d",
        "no-cwd.jsonl",
        &format!(r#"{{"type":"session","id":"s1",{ts}}}"#),
        &[],
    );
    write_session(
        &root,
        "d",
        "empty-cwd.jsonl",
        &format!(r#"{{"type":"session","id":"s2","cwd":"",{ts}}}"#),
        &[],
    );
    write_session(
        &root,
        "d",
        "dot-cwd.jsonl",
        &format!(r#"{{"type":"session","id":"s3","cwd":".",{ts}}}"#),
        &[],
    );

    // The vulnerable shape: registered workspace root == process cwd.
    let process_cwd = std::env::current_dir().unwrap();
    let registered = vec![workspace::RegisteredWorkspace {
        name: "process-cwd-ws".into(),
        root: process_cwd.to_string_lossy().into_owned(),
    }];

    // Rejected at the source: the scanner never yields them.
    assert!(scan_sessions_root(&root).is_empty());
    let catalog = build_catalog(&registered, scan_sessions_root(&root));
    assert_eq!(catalog.workspaces.len(), 1);
    assert_eq!(catalog.workspaces[0].sessions.len(), 0);
}

// ── jj template parsing (pure) ────────────────────────────────────────────

#[test]
fn parses_jj_workspace_list_template_output() {
    let output = concat!(
        "\"default\"\t\"/home/u/proj\"\n",
        "\"装饰 ws\"\t\"/home/u/пр\\u001bоект\"\n",
    );
    let ws = parse_workspace_list_output(output);
    assert_eq!(
        ws,
        vec![
            workspace::RegisteredWorkspace {
                name: "default".into(),
                root: "/home/u/proj".into(),
            },
            workspace::RegisteredWorkspace {
                name: "装饰 ws".into(),
                root: "/home/u/пр\u{1b}оект".into(),
            },
        ]
    );
    // Tolerant of junk.
    assert!(parse_workspace_list_output("").is_empty());
}

// ── jj integration (real jj binary; skipped when unavailable) ─────────────

/// `list_jj_workspaces` runs in the process cwd, so the two tests that chdir
/// must not race each other.
static CWD_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn jj_available() -> bool {
    Command::new("jj")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn jj(args: &[&str], cwd: &Path) {
    let status = Command::new("jj")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("jj spawn");
    assert!(status.success(), "jj {args:?} failed in {}", cwd.display());
}

#[test]
fn discover_end_to_end_with_real_jj() {
    if !jj_available() {
        eprintln!("skipping: jj not in PATH");
        return;
    }
    let tmp = tempdir();
    let repo = tmp.path().join("repo");
    let _ws2 = tmp.path().join("ws2");
    fs::create_dir_all(&repo).unwrap();
    jj(&["git", "init", "repo"], tmp.path());
    jj(&["workspace", "add", "../ws2", "--name", "extra"], &repo);

    let sessions_root = tmp.path().join("sessions");
    write_session(
        &sessions_root,
        "a",
        "s.jsonl",
        &header_for(repo.to_str().unwrap()),
        &[
            r#"{"type":"message","id":"m","timestamp":"2026-08-16T10:01:00.000Z","message":{"role":"user","content":"repo session","timestamp":1786876860000}}"#,
        ],
    );

    let _guard = CWD_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
    let saved = std::env::current_dir().unwrap();
    std::env::set_current_dir(&repo).unwrap();
    let catalog = discover(Some(&sessions_root)).unwrap();
    std::env::set_current_dir(saved).unwrap();

    assert_eq!(catalog.schema_version, 1);
    assert_eq!(catalog.workspaces.len(), 2);
    assert_eq!(catalog.workspaces[0].name, "default");
    assert!(catalog.workspaces.iter().any(|w| w.name == "extra"));
    let default = &catalog.workspaces[0];
    assert_eq!(default.sessions.len(), 1);
    assert_eq!(default.sessions[0].title, "repo session");
}

#[test]
fn discover_outside_jj_is_typed_not_in_jj_repository() {
    if !jj_available() {
        eprintln!("skipping: jj not in PATH");
        return;
    }
    let tmp = tempdir();
    let _guard = CWD_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
    let saved = std::env::current_dir().unwrap();
    std::env::set_current_dir(tmp.path()).unwrap();
    let result = discover(Some(&tmp.path().join("sessions")));
    std::env::set_current_dir(saved).unwrap();

    match result {
        Err(WorkspaceError::NotInJjRepository { .. }) => {}
        other => panic!("expected NotInJjRepository, got {other:?}"),
    }
}

// ── helpers ───────────────────────────────────────────────────────────────

fn tempdir() -> TempDirGuard {
    let dir = std::env::temp_dir().join(format!(
        "qingluan-ws-test-{}-{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    TempDirGuard { dir }
}

struct TempDirGuard {
    dir: PathBuf,
}

impl TempDirGuard {
    fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
