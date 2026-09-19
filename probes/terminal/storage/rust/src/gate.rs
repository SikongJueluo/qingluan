//! Throwaway Gate C harness for probe C. NOT production code.
//!
//! Drives the crash matrix and the recovery scenarios across REAL child
//! processes of this binary:
//!
//!   * `writer` children die with `libc::_exit(70)` at exact statement
//!     boundaries (process-crash evidence);
//!   * power-loss evidence adds an explicit physical truncation of the
//!     segment file to its fsynced checkpoint after the `_exit`;
//!   * `recover` always runs in a NEW process, twice in a row for
//!     idempotence;
//!   * the parent only reads state (snapshots, reads, acks) — it never
//!     performs recovery itself.
//!
//! Every result is classified exactly (complete / explicit gap / explicit
//! interruption); fabricated success fails the gate.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use uuid::Uuid;

use crate::crash::CRASH_EXIT;
use crate::db::Store;
use crate::reader::{ReadCursor, ReadOutcome, earliest_position, read_from};
use crate::recovery::{RecoveryReport, SegmentClass};
use crate::{DB_FILE, TERMINAL_ID, line_content};

/// Hard internal budget; run.sh wraps the whole probe in `timeout` and the
/// outer just invocation carries its own bound.
const GATE_BUDGET: std::time::Duration = std::time::Duration::from_secs(90);

// ────────────────────────────── summary types ──────────────────────────────

#[derive(Debug, Serialize)]
pub struct PointResult {
    pub point: String,
    pub power_loss: bool,
    pub fsynced_checkpoint: Option<u64>,
    pub classes: Vec<String>,
    pub actions: usize,
    pub watermark_after: u64,
    pub next_line: u64,
    pub gaps: usize,
    pub stdout_published_line: Option<u64>,
    pub events_after: u64,
    pub ok: bool,
    pub checks: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ScenarioResult {
    pub name: String,
    pub ok: bool,
    pub checks: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct GateSummary {
    pub stage: &'static str,
    pub started_unix_ms: u64,
    pub duration_ms: u128,
    pub points: Vec<PointResult>,
    pub scenarios: Vec<ScenarioResult>,
    pub crashed_children: u32,
    pub recover_children: u32,
    pub total_checks: usize,
    pub ok: bool,
}

// ────────────────────────────── child plumbing ─────────────────────────────

struct ChildOut {
    code: Option<i32>,
    success: bool,
    stdout: String,
    stderr: String,
}

fn run_child(args: &[String]) -> Result<ChildOut> {
    let exe = std::env::current_exe().context("current exe")?;
    let out = Command::new(&exe)
        .args(args)
        .output()
        .with_context(|| format!("spawn {}", exe.display()))?;
    Ok(ChildOut {
        code: out.status.code(),
        success: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

fn writer_args(workdir: &Path, migrations: &Path) -> Vec<String> {
    vec![
        "writer".into(),
        "--workdir".into(),
        workdir.display().to_string(),
        "--migrations".into(),
        migrations.display().to_string(),
        "--terminal".into(),
        TERMINAL_ID.into(),
    ]
}

fn arg(mut args: Vec<String>, k: &str, v: impl ToString) -> Vec<String> {
    args.push(k.into());
    args.push(v.to_string());
    args
}

fn parse_records(stdout: &str) -> Vec<serde_json::Value> {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn published_records(recs: &[serde_json::Value]) -> Vec<(u64, usize)> {
    recs.iter()
        .filter_map(|r| {
            r.get("published").map(|p| {
                (
                    p["line"].as_u64().unwrap_or(0),
                    p["bytes"].as_u64().unwrap_or(0) as usize,
                )
            })
        })
        .collect()
}

fn writer_done_epoch(recs: &[serde_json::Value]) -> Option<String> {
    recs.iter().find_map(|r| {
        r.get("writer_done")
            .and_then(|d| d["epoch"].as_str().map(String::from))
    })
}

fn recover_child_crash(
    migrations: &Path,
    workdir: &Path,
    crash: &str,
    counter: &mut u32,
) -> Result<()> {
    *counter += 1;
    let out = run_child(&[
        "recover".to_string(),
        "--workdir".into(),
        workdir.display().to_string(),
        "--migrations".into(),
        migrations.display().to_string(),
        "--terminal".into(),
        TERMINAL_ID.into(),
        "--crash".into(),
        crash.to_string(),
    ])?;
    if out.code != Some(crate::crash::CRASH_EXIT) {
        bail!(
            "recover child with crash {crash} exited {:?} (want {}); stderr: {}",
            out.code,
            crate::crash::CRASH_EXIT,
            out.stderr
        );
    }
    Ok(())
}

fn recover_child(migrations: &Path, workdir: &Path, counter: &mut u32) -> Result<RecoveryReport> {
    *counter += 1;
    let out = run_child(&[
        "recover".to_string(),
        "--workdir".into(),
        workdir.display().to_string(),
        "--migrations".into(),
        migrations.display().to_string(),
        "--terminal".into(),
        TERMINAL_ID.into(),
    ])?;
    if !out.success {
        bail!("recover child failed: {}", out.stderr);
    }
    let line = out
        .stdout
        .lines()
        .find(|l| l.starts_with("RECOVER "))
        .context("recover child printed no report")?;
    Ok(serde_json::from_str(&line["RECOVER ".len()..])?)
}

/// Physical power-loss truncation: cut the file to its fsynced checkpoint.
fn truncate_to(path: &Path, keep: u64) -> Result<()> {
    let f = std::fs::OpenOptions::new().write(true).open(path)?;
    f.set_len(keep)
        .with_context(|| format!("truncate {} to {keep}", path.display()))?;
    f.sync_data()?;
    Ok(())
}

fn seg_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with("seg-") && n.ends_with(".log") {
            out.push(e.path());
        }
    }
    out.sort();
    Ok(out)
}

fn trace_steps(path: &Path) -> Vec<String> {
    let s = std::fs::read_to_string(path).unwrap_or_default();
    s.lines()
        .map(|l| l.split('\t').last().unwrap_or("").to_string())
        .collect()
}

// ────────────────────────────── state snapshots ────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize)]
struct StateSnap {
    segments: Vec<(i64, String, u64, u64, u64, String)>,
    gaps: Vec<(u64, u64, String)>,
    watermark: u64,
    degraded: bool,
    refuse_new_start: bool,
    process_status: String,
    output_status: String,
    epoch: String,
    latest_revision: Option<(u64, u64, u64, i64)>,
    files: Vec<(String, u64)>,
}

async fn snapshot(store: &Store, root: &Path) -> Result<StateSnap> {
    let term = store.get_terminal(TERMINAL_ID).await?;
    let segs = store.segments(TERMINAL_ID).await?;
    let gaps = store
        .list_gaps(TERMINAL_ID)
        .await?
        .into_iter()
        .map(|g| (g.first_line, g.last_line, g.reason))
        .collect();
    let mut files = Vec::new();
    for p in seg_files(root)? {
        files.push((
            p.file_name().unwrap().to_string_lossy().into_owned(),
            std::fs::metadata(&p).map(|m| m.len()).unwrap_or(u64::MAX),
        ));
    }
    Ok(StateSnap {
        segments: segs
            .iter()
            .map(|s| {
                (
                    s.segment_id,
                    s.file_name.clone(),
                    s.committed_bytes,
                    s.fsynced_bytes,
                    s.last_line,
                    s.state.clone(),
                )
            })
            .collect(),
        gaps,
        watermark: term.line_watermark,
        degraded: term.degraded,
        refuse_new_start: term.refuse_new_start,
        process_status: term.process_status,
        output_status: term.output_status,
        epoch: Uuid::from_bytes(term.log_epoch).to_string(),
        latest_revision: store
            .latest_tail_revision(TERMINAL_ID)
            .await?
            .map(|r| (r.revision, r.line, r.byte_offset, r.segment_id)),
        files,
    })
}

async fn open_store(workdir: &Path, migrations: &Path) -> Result<Store> {
    Store::open(&workdir.join(DB_FILE), migrations).await
}

// ────────────────────────────── read assertions ────────────────────────────

async fn expect_bytes(
    store: &Store,
    root: &Path,
    cursor: ReadCursor,
    max: usize,
) -> Result<std::result::Result<Vec<u8>, crate::db::CursorExpired>> {
    Ok(
        match read_from(store, root, TERMINAL_ID, cursor, max).await? {
            ReadOutcome::Data(d) => Ok(d.bytes),
            ReadOutcome::Expired(e) => Err(e),
        },
    )
}

async fn check_all_lines_readable(
    store: &Store,
    root: &Path,
    watermark: u64,
    line_bytes: usize,
    checks: &mut Vec<String>,
) -> Result<()> {
    for line in 1..=watermark {
        let got = expect_bytes(
            store,
            root,
            ReadCursor {
                line,
                byte_offset: 0,
            },
            line_bytes,
        )
        .await?;
        let want = line_content(line, line_bytes).into_bytes();
        match got {
            Ok(b) if b == want => {}
            other => bail!("line {line} unreadable or wrong: {:?}", other.map(|_| ())),
        }
    }
    checks.push(format!("lines 1..={watermark} byte-exact after recovery"));
    Ok(())
}

// ────────────────────────────── crash matrix ───────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum PowerLoss {
    None,
    Zero,
    CommittedBoundary,
}

#[derive(Clone)]
struct Spec {
    point: &'static str,
    /// Run a base writer (2 committed lines) before the crashing writer.
    seed: bool,
    /// The crashing writer carries an event and the terminal exit.
    event: bool,
    power: PowerLoss,
    /// Dominant expected recovery class for the touched segment (Fresh
    /// expectations: "" = no segments at all).
    class: SegmentClass,
    watermark_after: u64,
    next_line: u64,
    /// Line 3 appears on the crashed writer's stdout (published before death).
    stdout_line3: bool,
    events_after: u64,
}

fn seeded(
    point: &'static str,
    class: SegmentClass,
    watermark_after: u64,
    next_line: u64,
    stdout_line3: bool,
    events_after: u64,
) -> Spec {
    Spec {
        point,
        seed: true,
        event: false,
        power: PowerLoss::None,
        class,
        watermark_after,
        next_line,
        stdout_line3,
        events_after,
    }
}

fn matrix_specs() -> Vec<Spec> {
    let mut v = vec![
        // frame append boundaries
        seeded("frame_before_write", SegmentClass::Clean, 2, 3, false, 0),
        seeded("frame_mid_write", SegmentClass::Partial, 2, 3, false, 0),
        seeded("frame_after_write", SegmentClass::Partial, 2, 3, false, 0),
        seeded("frame_after_sync", SegmentClass::Partial, 2, 3, false, 0),
        // SQLite transaction boundaries (real rollback via process death)
        seeded("txn_begin", SegmentClass::Partial, 2, 3, false, 0),
        seeded("txn_update", SegmentClass::Partial, 2, 3, false, 0),
        seeded("txn_before_commit", SegmentClass::Partial, 2, 3, false, 0),
        seeded("txn_after_commit", SegmentClass::Clean, 3, 4, false, 0),
        // publication boundaries
        seeded("publish_before", SegmentClass::Clean, 3, 4, false, 0),
        seeded("publish_after", SegmentClass::Clean, 3, 4, true, 0),
        // event boundaries (exit + event commit in the same transaction)
        Spec {
            event: true,
            ..seeded("event_insert", SegmentClass::Partial, 2, 3, false, 0)
        },
        Spec {
            event: true,
            ..seeded("event_commit", SegmentClass::Clean, 3, 4, false, 1)
        },
        Spec {
            event: true,
            ..seeded("event_publish", SegmentClass::Clean, 3, 4, true, 1)
        },
        // segment creation boundaries (fresh state: first-ever segment)
        Spec {
            seed: false,
            event: false,
            power: PowerLoss::None,
            class: SegmentClass::Missing, // marker: expect NO segments at all
            watermark_after: 0,
            next_line: 1,
            stdout_line3: false,
            events_after: 0,
            point: "seg_before",
        },
        Spec {
            seed: false,
            event: false,
            power: PowerLoss::None,
            class: SegmentClass::Pending,
            watermark_after: 0,
            next_line: 1,
            stdout_line3: false,
            events_after: 0,
            point: "seg_header_written",
        },
        Spec {
            seed: false,
            event: false,
            power: PowerLoss::None,
            class: SegmentClass::Pending,
            watermark_after: 0,
            next_line: 1,
            stdout_line3: false,
            events_after: 0,
            point: "seg_header_synced",
        },
        // durable segment file that no DB row owns: discover + quarantine
        Spec {
            seed: false,
            event: false,
            power: PowerLoss::None,
            class: SegmentClass::Orphan,
            watermark_after: 0,
            next_line: 1,
            stdout_line3: false,
            events_after: 0,
            point: "seg_before_db_row",
        },
    ];
    // Power-loss variants: after `_exit(70)`, physically truncate the file to
    // its fsynced checkpoint so the unsynced bytes vanish.
    for (point, power) in [
        ("seg_header_written", PowerLoss::Zero),
        ("frame_mid_write", PowerLoss::CommittedBoundary),
        ("frame_after_write", PowerLoss::CommittedBoundary),
    ] {
        let base = v.iter().find(|s| s.point == point).unwrap().clone();
        v.push(Spec { power, ..base });
    }
    v
}

async fn run_matrix_point(
    base: &Path,
    migrations: &Path,
    spec: &Spec,
    crashed: &mut u32,
    recovers: &mut u32,
) -> Result<PointResult> {
    let name = if spec.power == PowerLoss::None {
        spec.point.to_string()
    } else {
        format!("{}+power-loss", spec.point)
    };
    let dir = base.join(&name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    let mut checks = Vec::new();
    let line_bytes = 120usize;

    // Base state: 2 committed lines in one segment.
    let seed_epoch = if spec.seed {
        let out = run_child(&arg(
            arg(writer_args(&dir, migrations), "--append", 2),
            "--line-bytes",
            line_bytes,
        ))?;
        if !out.success {
            bail!("seed writer failed: {}", out.stderr);
        }
        let recs = parse_records(&out.stdout);
        let w = writer_done_epoch(&recs).context("seed writer_done epoch")?;
        checks.push("seed: 2 lines committed".into());
        w
    } else {
        String::new()
    };

    // Crash writer: dies at the exact boundary.
    *crashed += 1;
    let mut cargs = arg(
        arg(writer_args(&dir, migrations), "--append", 1),
        "--line-bytes",
        line_bytes,
    );
    cargs = arg(cargs, "--crash", spec.point);
    cargs = arg(
        cargs,
        "--trace",
        dir.join("trace.log").display().to_string(),
    );
    if spec.event {
        cargs.push("--event-per-line".into());
        cargs = arg(cargs, "--with-exit", 0);
    }
    let out = run_child(&cargs)?;
    if out.code != Some(CRASH_EXIT) {
        bail!(
            "crash writer for {} exited {:?} (want {CRASH_EXIT}); stderr: {}",
            spec.point,
            out.code,
            out.stderr
        );
    }
    let recs = parse_records(&out.stdout);
    let published = published_records(&recs);
    let saw_line3 = published.iter().any(|(l, _)| *l == 3);
    if saw_line3 != spec.stdout_line3 {
        bail!(
            "{}: line 3 on stdout = {saw_line3}, want {}",
            spec.point,
            spec.stdout_line3
        );
    }
    checks.push(format!(
        "writer died with _exit({CRASH_EXIT}) at {}; stdout line3={}",
        spec.point, saw_line3
    ));

    // Trace ordering evidence.
    let steps = trace_steps(&dir.join("trace.log"));
    if steps.last().map(String::as_str) != Some(spec.point) {
        let want_point = spec.point;
        bail!(
            "trace for {} ended at {:?}, want {want_point}",
            spec.point,
            steps.last()
        );
    }
    if matches!(spec.point, "seg_header_synced" | "seg_before_db_row") {
        // The directory fsync after segment creation is mandatory: it must
        // have run (and be traceable) before these durable boundaries
        // (seg_header_written deliberately dies BEFORE the syncs).
        if !steps.contains(&"seg_dir_fsynced".to_string()) {
            bail!(
                "{}: directory fsync after segment creation missing",
                spec.point
            );
        }
        checks.push("directory fsynced after segment creation".into());
    }
    if spec.stdout_line3 {
        let c = steps.iter().position(|s| s == "txn_after_commit");
        let p = steps.iter().position(|s| s == spec.point);
        match (c, p) {
            (Some(c), Some(p)) if c < p => {
                checks.push("commit recorded before publication".into());
            }
            _ => bail!(
                "{}: publication ordering not provable from trace",
                spec.point
            ),
        }
    }

    // Power-loss variant: unsynced bytes (and, before the header sync,
    // the directory entry itself) vanish.
    let mut checkpoint = None;
    if spec.power != PowerLoss::None {
        let ckpt = match spec.power {
            PowerLoss::Zero => 0u64,
            PowerLoss::CommittedBoundary => {
                let store = open_store(&dir, migrations).await?;
                let term = store.get_terminal(TERMINAL_ID).await?;
                let seg_id = term.active_segment.expect("active segment row");
                store.get_segment(seg_id).await?.committed_bytes
            }
            PowerLoss::None => unreachable!(),
        };
        for f in seg_files(&dir)? {
            if spec.power == PowerLoss::Zero {
                // Nothing was durable yet, not even the directory entry.
                std::fs::remove_file(&f)?;
            } else {
                let len = std::fs::metadata(&f)?.len();
                if len > ckpt {
                    truncate_to(&f, ckpt)?;
                }
            }
        }
        crate::fsync_dir(&dir)?;
        checkpoint = Some(ckpt);
        checks.push(format!(
            "power loss: file cut to its fsynced checkpoint {ckpt} (entry removed at 0)"
        ));
    }

    // Recovery in a NEW process — twice; classification + idempotence.
    let report = recover_child(migrations, &dir, recovers)?;
    if report.line_watermark != spec.watermark_after {
        bail!(
            "{}: watermark {} want {}",
            name,
            report.line_watermark,
            spec.watermark_after
        );
    }
    if !report.gaps.is_empty() {
        bail!("{}: fabricated gaps {:?}", name, report.gaps);
    }
    if report.degraded {
        bail!("{}: matrix point left the terminal degraded", name);
    }
    if spec.seed && Uuid::from_bytes(report.epoch).to_string() != seed_epoch {
        bail!("{}: epoch rotated on a normal crash point", name);
    }
    if !spec.seed && report.epoch_rotated && spec.point != "seg_before_db_row" {
        // fresh terminals always rotate (create) the epoch once; that is fine
        bail!("{}: unexpected epoch rotation", name);
    }
    let classes: Vec<String> = report
        .outcomes
        .iter()
        .map(|o| o.class.as_str().to_string())
        .collect();
    let expect = match spec.class {
        SegmentClass::Clean => "clean",
        SegmentClass::Partial => "partial",
        SegmentClass::Corrupt => "corrupt",
        SegmentClass::Orphan => "orphan",
        SegmentClass::Missing => "missing",
        SegmentClass::Truncated => "truncated",
        SegmentClass::EpochMismatch => "epoch-mismatch",
        SegmentClass::Pending => "pending",
    };
    // Power-loss variants truncated the file to the fsynced checkpoint before
    // recovery, so the uncommitted tail is already gone: the classification
    // is then "clean" while every final-state assertion stays identical.
    let expect = if spec.power == PowerLoss::CommittedBoundary && spec.seed {
        "clean"
    } else {
        expect
    };
    if spec.seed {
        if classes.len() != 1 || classes[0] != expect {
            bail!("{}: outcome classes {:?} want [{expect}]", name, classes);
        }
    } else if spec.point == "seg_before" {
        if !classes.is_empty() {
            bail!("{}: outcomes {classes:?} want none", name);
        }
    } else {
        let want = match spec.class {
            SegmentClass::Pending => "pending",
            SegmentClass::Orphan => "orphan",
            other => bail!("unmapped fresh expectation {other:?}"),
        };
        if !classes.contains(&want.to_string()) {
            bail!("{}: outcomes {classes:?} want [{want}]", name);
        }
    }
    let actions = report.actions.len();
    checks.push(format!(
        "recover#1: class {classes:?}, watermark {}, epoch {}",
        report.line_watermark,
        Uuid::from_bytes(report.epoch).simple()
    ));

    let store = open_store(&dir, migrations).await?;
    let snap1 = snapshot(&store, &dir).await?;
    let report2 = recover_child(migrations, &dir, recovers)?;
    if !report2.actions.is_empty() {
        bail!(
            "{}: second recovery kept repairing: {:?}",
            name,
            report2.actions
        );
    }
    let snap2 = snapshot(&store, &dir).await?;
    if snap1 != snap2 {
        bail!(
            "{}: consecutive recovery is not idempotent:\n{snap1:#?}\n{snap2:#?}",
            name
        );
    }
    checks.push("consecutive recover idempotent (no actions, identical state)".into());
    drop(store);

    // Continuation: numbering resumes exactly at watermark+1 (no reuse, no skip).
    let out = run_child(&arg(
        arg(writer_args(&dir, migrations), "--append", 1),
        "--line-bytes",
        line_bytes,
    ))?;
    if !out.success {
        bail!("continuation writer failed: {}", out.stderr);
    }
    let recs = parse_records(&out.stdout);
    let published = published_records(&recs);
    if published.first().map(|(l, _)| *l) != Some(spec.next_line) {
        bail!(
            "{}: continuation line {:?} want {}",
            name,
            published.first(),
            spec.next_line
        );
    }
    checks.push(format!(
        "numbering continues at {} (committed lines kept, unpublished numbers reusable)",
        spec.next_line
    ));

    let store = open_store(&dir, migrations).await?;
    let term = store.get_terminal(TERMINAL_ID).await?;
    if term.line_watermark != spec.next_line {
        bail!(
            "{}: final watermark {} want {}",
            name,
            term.line_watermark,
            spec.next_line
        );
    }
    let events = store.event_count(TERMINAL_ID).await?;
    if events != spec.events_after {
        bail!("{}: events {events} want {}", name, spec.events_after);
    }
    // Active segment invariant: file length equals the committed boundary.
    for (_, file, committed, fsynced, _, _) in &snap2.segments {
        if fsynced > committed {
            bail!("{}: fsynced > committed for {file}", name);
        }
    }
    check_all_lines_readable(&store, &dir, term.line_watermark, line_bytes, &mut checks).await?;

    Ok(PointResult {
        point: name,
        power_loss: spec.power != PowerLoss::None,
        fsynced_checkpoint: checkpoint,
        classes,
        actions,
        watermark_after: spec.watermark_after,
        next_line: spec.next_line,
        gaps: report.gaps.len(),
        stdout_published_line: if spec.stdout_line3 { Some(3) } else { None },
        events_after: spec.events_after,
        ok: true,
        checks,
    })
}

// ────────────────────────────── scenarios ──────────────────────────────────

async fn scenario_utf8_frames(
    base: &Path,
    migrations: &Path,
    recovers: &mut u32,
) -> Result<ScenarioResult> {
    let dir = fresh_dir(base, "utf8_frames")?;
    let mut checks = Vec::new();
    // line 1 short, line 2 = 30000 multibyte chars (90000 bytes, 2 frames),
    // line 3 short.
    let out = run_child(&arg(
        arg(
            arg(
                arg(writer_args(&dir, migrations), "--prelude", 1),
                "--long-utf8",
                30000,
            ),
            "--append",
            1,
        ),
        "--line-bytes",
        100,
    ))?;
    ensure_writer_ok(&out, "utf8 writer")?;
    let long: Vec<u8> = "界".repeat(30000).into_bytes();
    let store = open_store(&dir, migrations).await?;

    let got = expect_bytes(
        &store,
        &dir,
        ReadCursor {
            line: 2,
            byte_offset: 0,
        },
        long.len(),
    )
    .await?;
    if got? != long {
        bail!("long UTF-8 line mismatch across frames");
    }
    checks.push("long UTF-8 line reassembles byte-exact across 2 frames".into());

    // Read continuation at (line, byte_offset): 60003 is a char boundary.
    let a = expect_bytes(
        &store,
        &dir,
        ReadCursor {
            line: 2,
            byte_offset: 60003,
        },
        5000,
    )
    .await?
    .context("cursor (2,60003) expired")?;
    if a != long[60003..65003] {
        bail!("continuation at byte 60003 mismatch");
    }
    let rd = read_from(
        &store,
        &dir,
        TERMINAL_ID,
        ReadCursor {
            line: 2,
            byte_offset: 60003,
        },
        5000,
    )
    .await?;
    let ReadOutcome::Data(d) = rd else {
        bail!("no data")
    };
    let b = expect_bytes(&store, &dir, d.next, 10000)
        .await?
        .context("second read expired")?;
    if a.iter().chain(b.iter()).copied().collect::<Vec<_>>() != long[60003..75003] {
        bail!("byte_offset continuation is not continuous");
    }
    checks.push("read continuation at line+byte_offset stays byte-continuous".into());

    // Cross-line continuation near the frame/line end.
    let c = expect_bytes(
        &store,
        &dir,
        ReadCursor {
            line: 2,
            byte_offset: 89997,
        },
        100,
    )
    .await?
    .context("cross-line read expired")?;
    let mut want = long[89997..].to_vec();
    want.extend_from_slice(&line_content(3, 100).as_bytes()[..97]);
    if c != want {
        bail!("cross-line continuation mismatch");
    }
    checks.push("read crosses the frame and line boundary exactly".into());

    // Fixed read end: pinned cursor at the current end follows new appends.
    let pin = ReadCursor {
        line: 3,
        byte_offset: 100,
    };
    let rd = read_from(&store, &dir, TERMINAL_ID, pin, 10).await?;
    match rd {
        ReadOutcome::Data(d) if d.bytes.is_empty() && d.at_end => {}
        other => bail!("fixed read end not at end: {other:?}"),
    }
    drop(store);
    let out = run_child(&arg(writer_args(&dir, migrations), "--append", 1))?;
    ensure_writer_ok(&out, "utf8 append 4")?;
    let store = open_store(&dir, migrations).await?;
    let got = expect_bytes(&store, &dir, pin, 50)
        .await?
        .context("pinned cursor expired")?;
    if got != line_content(4, 120).as_bytes()[..50] {
        bail!("fixed read end did not follow the append");
    }
    checks.push("fixed read end follows new appends without losing position".into());
    let _ = recovers;
    Ok(ScenarioResult {
        name: "utf8_frames".into(),
        ok: true,
        checks,
    })
}

async fn scenario_rotations(base: &Path, migrations: &Path) -> Result<ScenarioResult> {
    let dir = fresh_dir(base, "rotations")?;
    let mut checks = Vec::new();
    let mut args = arg(
        arg(writer_args(&dir, migrations), "--append", 10),
        "--line-bytes",
        1500,
    );
    args = arg(args, "--max-segment-bytes", 4096);
    let out = run_child(&args)?;
    ensure_writer_ok(&out, "rotation writer")?;
    let store = open_store(&dir, migrations).await?;
    let segs = store.segments(TERMINAL_ID).await?;
    if segs.len() < 4 {
        bail!("expected >=4 segments (>=3 rotations), got {}", segs.len());
    }
    checks.push(format!(
        "{} segments after rotation threshold 4096",
        segs.len()
    ));

    let want_all: Vec<u8> = (1..=10)
        .flat_map(|i| line_content(i, 1500).into_bytes())
        .collect();
    let got = expect_bytes(
        &store,
        &dir,
        ReadCursor {
            line: 1,
            byte_offset: 0,
        },
        want_all.len(),
    )
    .await?
    .context("full read expired")?;
    if got != want_all {
        bail!("full read across segments mismatch");
    }
    checks.push("read from line 1 spans every rotation byte-exact".into());

    let want_tail: Vec<u8> = (3..=10)
        .flat_map(|i| line_content(i, 1500).into_bytes())
        .skip(700)
        .collect();
    let got = expect_bytes(
        &store,
        &dir,
        ReadCursor {
            line: 3,
            byte_offset: 700,
        },
        want_tail.len(),
    )
    .await?
    .context("mid rotation read expired")?;
    if got != want_tail {
        bail!("continuation across rotations mismatch");
    }
    checks.push("continuation at (line,byte_offset) spans >=3 rotations exactly".into());

    let pin = ReadCursor {
        line: 10,
        byte_offset: 1500,
    };
    drop(store);
    let out = run_child(&arg(
        arg(writer_args(&dir, migrations), "--append", 2),
        "--line-bytes",
        1500,
    ))?;
    ensure_writer_ok(&out, "rotation append")?;
    let store = open_store(&dir, migrations).await?;
    let got = expect_bytes(&store, &dir, pin, 3000)
        .await?
        .context("pin expired")?;
    let want: Vec<u8> = (11..=12)
        .flat_map(|i| line_content(i, 1500).into_bytes())
        .collect();
    if got != want {
        bail!("fixed read end across rotation mismatch");
    }
    checks.push("fixed read end continues across a new rotation".into());
    Ok(ScenarioResult {
        name: "rotations".into(),
        ok: true,
        checks,
    })
}

async fn scenario_cursor_cleanup(
    base: &Path,
    migrations: &Path,
    recovers: &mut u32,
) -> Result<ScenarioResult> {
    let dir = fresh_dir(base, "cursor_cleanup")?;
    let mut checks = Vec::new();
    let mut args = arg(
        arg(writer_args(&dir, migrations), "--append", 12),
        "--line-bytes",
        1500,
    );
    args = arg(args, "--max-segment-bytes", 4096);
    let out = run_child(&args)?;
    ensure_writer_ok(&out, "cleanup writer 12")?;
    let store = open_store(&dir, migrations).await?;
    // A naturally persisted revision: no fixture and no out-of-band copy —
    // the mapping is read back from the committed transaction itself.
    let rev = store
        .tail_revision(TERMINAL_ID, 12)
        .await?
        .context("tail revision 12 persisted")?;
    if rev.line != 12 || rev.byte_offset != 1500 {
        bail!(
            "revision 12 fixed cursor ({}, {}) want (12, 1500)",
            rev.line,
            rev.byte_offset
        );
    }
    let holder = store.get_segment(rev.segment_id).await?;
    if !(holder.first_line <= 12 && 12 < holder.last_line) {
        bail!(
            "revision 12 points at segment [{}..{}) which does not cover line 12",
            holder.first_line,
            holder.last_line
        );
    }
    checks.push(format!(
        "naturally persisted revision 12 fixes cursor (line 12, byte {}, seg {})",
        rev.byte_offset, rev.segment_id
    ));

    // Rotate past it; the revision becomes a history position that still
    // resolves through the persisted mapping.
    drop(store);
    let mut args = writer_args(&dir, migrations);
    args = arg(args, "--append", 2);
    args = arg(args, "--line-bytes", 1500);
    args = arg(args, "--max-segment-bytes", 4096);
    let out = run_child(&args)?;
    ensure_writer_ok(&out, "cleanup writer 2")?;
    let store = open_store(&dir, migrations).await?;
    if store.get_segment(rev.segment_id).await?.state != "sealed" {
        bail!("rotation did not seal the revision's holding segment");
    }
    let cursor = crate::reader::resolve_tail_revision(&store, TERMINAL_ID, 12)
        .await?
        .context("revision 12 unresolvable after rotation")?;
    let got = expect_bytes(&store, &dir, cursor, 3000)
        .await?
        .context("resolved history revision expired")?;
    let want: Vec<u8> = (13..=14)
        .flat_map(|i| line_content(i, 1500).into_bytes())
        .collect();
    if got != want {
        bail!("resolved revision read is not byte-exact from the fixed cursor");
    }
    checks.push("revision resolved after rotation reads byte-exact from its fixed cursor".into());

    // Resource recovery deletes the sealed history segments; the revisions
    // pointing into them expire in the same transaction.
    let latest = store
        .latest_tail_revision(TERMINAL_ID)
        .await?
        .context("latest revision")?;
    let keep = latest.segment_id;
    let deleted = store.cleanup_old_segments(TERMINAL_ID, keep).await?;
    if deleted.is_empty() || deleted.iter().any(|d| d.segment_id >= keep) {
        bail!(
            "cleanup deleted the wrong segments: {:?}",
            deleted.iter().map(|d| d.segment_id).collect::<Vec<_>>()
        );
    }
    if !deleted.iter().any(|d| d.segment_id == rev.segment_id) {
        bail!("cleanup kept the revision's holding segment; expiry not exercised");
    }
    for d in &deleted {
        if dir.join(&d.file_name).exists() {
            let _ = std::fs::remove_file(dir.join(&d.file_name));
        }
    }
    crate::fsync_dir(&dir)?;
    if store.tail_revision(TERMINAL_ID, 12).await?.is_some() {
        bail!("revision 12 survived the cleanup transaction");
    }
    checks.push(format!(
        "cleanup deleted {} sealed segments and expired revision 12 transactionally",
        deleted.len()
    ));

    // The expired revision resolves to nothing and reading its position
    // fails explicitly with the earliest available position.
    if crate::reader::resolve_tail_revision(&store, TERMINAL_ID, 12)
        .await?
        .is_some()
    {
        bail!("expired revision still resolves to a cursor");
    }
    match read_from(
        &store,
        &dir,
        TERMINAL_ID,
        ReadCursor {
            line: rev.line,
            byte_offset: 0,
        },
        100,
    )
    .await?
    {
        ReadOutcome::Expired(e) => {
            let earliest = earliest_position(&store, TERMINAL_ID).await?;
            if e.earliest_line != earliest.line {
                bail!(
                    "expired cursor earliest {} want {}",
                    e.earliest_line,
                    earliest.line
                );
            }
            checks.push(format!(
                "expired revision cursor fails explicitly with earliest position (line {}, byte {})",
                e.earliest_line, e.earliest_byte_offset
            ));
        }
        ReadOutcome::Data(_) => bail!("cursor into deleted segment returned data"),
    }
    // The surviving latest revision still resolves after cleanup, and a
    // later append is readable through it.
    let cursor = crate::reader::resolve_tail_revision(&store, TERMINAL_ID, latest.revision)
        .await?
        .context("surviving revision expired by cleanup")?;
    checks.push("surviving revision still resolves after cleanup".into());
    drop(store);
    let mut args = writer_args(&dir, migrations);
    args = arg(args, "--append", 2);
    args = arg(args, "--line-bytes", 1500);
    let out = run_child(&args)?;
    ensure_writer_ok(&out, "post-cleanup writer")?;
    let store = open_store(&dir, migrations).await?;
    let got = expect_bytes(&store, &dir, cursor, 3000)
        .await?
        .context("surviving revision read expired")?;
    let want: Vec<u8> = (15..=16)
        .flat_map(|i| line_content(i, 1500).into_bytes())
        .collect();
    if got != want {
        bail!("surviving revision read mismatch");
    }
    checks.push("appends after cleanup are readable through the surviving revision".into());
    let _ = recovers;
    Ok(ScenarioResult {
        name: "cursor_cleanup".into(),
        ok: true,
        checks,
    })
}

async fn scenario_tail_revision_multiframe(
    base: &Path,
    migrations: &Path,
) -> Result<ScenarioResult> {
    let dir = fresh_dir(base, "tail_revision_multiframe")?;
    let mut checks = Vec::new();
    // Line 2 is a 90000-byte multibyte line spanning two frames: its exact
    // end (90000) differs from the last chunk's start offset, so the stored
    // byte position is genuinely validated by the resolved read.
    let out = run_child(&arg(
        arg(
            arg(
                arg(writer_args(&dir, migrations), "--prelude", 1),
                "--long-utf8",
                30000,
            ),
            "--append",
            1,
        ),
        "--line-bytes",
        100,
    ))?;
    ensure_writer_ok(&out, "multiframe revision writer")?;
    let store = open_store(&dir, migrations).await?;
    let rev = store
        .tail_revision(TERMINAL_ID, 2)
        .await?
        .context("revision 2 persisted")?;
    if rev.byte_offset != 90000 {
        bail!(
            "multiframe revision byte_offset {} want the exact line end 90000",
            rev.byte_offset
        );
    }
    let cursor = crate::reader::resolve_tail_revision(&store, TERMINAL_ID, 2)
        .await?
        .context("revision 2 unresolvable")?;
    let got = expect_bytes(&store, &dir, cursor, 100)
        .await?
        .context("multiframe revision read expired")?;
    if got != line_content(3, 100).into_bytes() {
        bail!("multiframe revision did not resolve to the exact line end");
    }
    checks.push(
        "multiframe revision stores the exact end-of-line byte (90000, not a chunk start)".into(),
    );
    Ok(ScenarioResult {
        name: "tail_revision_multiframe".into(),
        ok: true,
        checks,
    })
}

/// Wait until the writer's trace shows `step` (bounded poll).
fn wait_for_trace_step(trace: &Path, step: &str) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        if trace_steps(trace).iter().any(|s| s == step) {
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            bail!("trace never recorded {step}");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Commit-before-query visibility: while the writer is parked between its
/// fsync and its SQLite transaction (and after crashes at those exact
/// boundaries), a concurrent reader must see only DB-committed lines — the
/// appended-but-uncommitted frame must be invisible even when reading from
/// an earlier line.
async fn scenario_commit_before_query(base: &Path, migrations: &Path) -> Result<ScenarioResult> {
    let dir = fresh_dir(base, "commit_before_query")?;
    let mut checks = Vec::new();
    let line_bytes = 120usize;
    let out = run_child(&arg(
        arg(writer_args(&dir, migrations), "--append", 2),
        "--line-bytes",
        line_bytes,
    ))?;
    ensure_writer_ok(&out, "cbq seed writer")?;

    // Live concurrency: a writer parked between fsync and the commit
    // transaction while the parent reads the same store.
    let gate = dir.join("gate");
    let trace = dir.join("trace.log");
    let mut args = arg(
        arg(writer_args(&dir, migrations), "--append", 1),
        "--line-bytes",
        line_bytes,
    );
    args = arg(args, "--pre-commit-gate", gate.display().to_string());
    args = arg(args, "--trace", trace.display().to_string());
    let exe = std::env::current_exe()?;
    let errfile = std::fs::File::create(dir.join("child.stderr"))?;
    let mut child = Command::new(&exe)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::from(errfile))
        .spawn()?;
    let mut stdout = child.stdout.take().context("child stdout")?;
    wait_for_trace_step(&trace, "pre_commit_gate_wait_begin")?;

    {
        let store = open_store(&dir, migrations).await?;
        let term = store.get_terminal(TERMINAL_ID).await?;
        if term.line_watermark != 2 {
            bail!(
                "parked writer advanced the watermark to {}",
                term.line_watermark
            );
        }
        // Reading from an earlier line must stop at the committed boundary.
        let got = expect_bytes(
            &store,
            &dir,
            ReadCursor {
                line: 1,
                byte_offset: 0,
            },
            300,
        )
        .await?
        .context("concurrent read expired")?;
        let want: Vec<u8> = (1..=2)
            .flat_map(|i| line_content(i, line_bytes).into_bytes())
            .collect();
        if got != want {
            bail!("concurrent read leaked uncommitted bytes");
        }
        // Pinned at the committed end: empty, at end.
        let rd = read_from(
            &store,
            &dir,
            TERMINAL_ID,
            ReadCursor {
                line: 2,
                byte_offset: line_bytes as u64,
            },
            50,
        )
        .await?;
        match rd {
            ReadOutcome::Data(d) => {
                if !d.bytes.is_empty() || !d.at_end {
                    bail!("pinned cursor returned data before the commit");
                }
            }
            _ => bail!("pinned cursor expired before the commit"),
        }
        // The appended bytes ARE on disk beyond the committed boundary.
        let seg_id = term.active_segment.context("active segment")?;
        let seg = store.get_segment(seg_id).await?;
        let len = std::fs::metadata(dir.join(&seg.file_name))?.len();
        if len <= seg.committed_bytes {
            bail!(
                "parked writer has no uncommitted bytes on disk ({len} <= {})",
                seg.committed_bytes
            );
        }
        checks.push(format!(
            "concurrent read while parked between fsync and commit sees only committed lines \
             (file {len} > committed {})",
            seg.committed_bytes
        ));
    }

    // Release the gate; the writer commits and only then publishes.
    std::fs::write(&gate, b"1")?;
    let mut reader = BufReader::new(&mut stdout);
    let mut buf = String::new();
    let mut published_line3 = false;
    loop {
        buf.clear();
        let n = reader.read_line(&mut buf)?;
        if n == 0 {
            break;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(buf.trim()) {
            if v.get("published").and_then(|p| p["line"].as_u64()) == Some(3) {
                published_line3 = true;
            }
        }
    }
    let status = child.wait()?;
    if !status.success() || !published_line3 {
        bail!("gated writer failed or never published line 3: {status}");
    }
    {
        let store = open_store(&dir, migrations).await?;
        let got = expect_bytes(
            &store,
            &dir,
            ReadCursor {
                line: 2,
                byte_offset: line_bytes as u64,
            },
            50,
        )
        .await?
        .context("post-commit read expired")?;
        if got != line_content(3, line_bytes).as_bytes()[..50] {
            bail!("line 3 wrong after the commit");
        }
        checks.push("line 3 becomes visible only after the commit transaction".into());
    }

    // Crash-boundary variants: read BEFORE any recovery runs.
    for point in ["frame_after_sync", "txn_before_commit"] {
        let d = fresh_dir(base, &format!("cbq_{point}"))?;
        let out = run_child(&arg(
            arg(writer_args(&d, migrations), "--append", 2),
            "--line-bytes",
            line_bytes,
        ))?;
        ensure_writer_ok(&out, "cbq crash seed")?;
        let mut cargs = arg(
            arg(writer_args(&d, migrations), "--append", 1),
            "--line-bytes",
            line_bytes,
        );
        cargs = arg(cargs, "--crash", point);
        let out = run_child(&cargs)?;
        if out.code != Some(crate::crash::CRASH_EXIT) {
            bail!("cbq crash writer for {point} exited {:?}", out.code);
        }
        let store = open_store(&d, migrations).await?;
        let term = store.get_terminal(TERMINAL_ID).await?;
        if term.line_watermark != 2 {
            bail!("{point}: watermark {} before recovery", term.line_watermark);
        }
        let got = expect_bytes(
            &store,
            &d,
            ReadCursor {
                line: 1,
                byte_offset: 0,
            },
            300,
        )
        .await?
        .context("pre-recovery read expired")?;
        let want: Vec<u8> = (1..=2)
            .flat_map(|i| line_content(i, line_bytes).into_bytes())
            .collect();
        if got != want {
            bail!("{point}: pre-recovery read exposed the uncommitted frame");
        }
        let rd = read_from(
            &store,
            &d,
            TERMINAL_ID,
            ReadCursor {
                line: 2,
                byte_offset: line_bytes as u64,
            },
            50,
        )
        .await?;
        match rd {
            ReadOutcome::Data(d) if d.bytes.is_empty() && d.at_end => {}
            other => bail!("{point}: pinned cursor returned {other:?} before recovery"),
        }
        drop(store);
        checks.push(format!(
            "{point}: read before recovery exposes no uncommitted frame"
        ));
    }
    Ok(ScenarioResult {
        name: "commit_before_query".into(),
        ok: true,
        checks,
    })
}

async fn scenario_exit_event(base: &Path, migrations: &Path) -> Result<ScenarioResult> {
    let dir = fresh_dir(base, "exit_event")?;
    let mut checks = Vec::new();
    let mut args = arg(
        arg(writer_args(&dir, migrations), "--append", 3),
        "--line-bytes",
        120,
    );
    args.push("--event-per-line".into());
    args = arg(args, "--with-exit", 0);
    args = arg(args, "--trace", dir.join("trace.log").display().to_string());
    let out = run_child(&args)?;
    ensure_writer_ok(&out, "exit_event writer")?;
    let recs = parse_records(&out.stdout);

    // Publication order on stdout: every published line precedes its event,
    // and the exit event is last.
    let mut order = Vec::new();
    for r in &recs {
        if r.get("published").is_some() {
            order.push(format!("line:{}", r["published"]["line"].as_u64().unwrap()));
        }
        if r.get("event_published").is_some() {
            order.push(format!(
                "event:{}",
                r["event_published"]["seq"].as_u64().unwrap()
            ));
        }
    }
    if order.last().map(String::as_str) != Some("event:3") {
        bail!("exit event not the last published record: {order:?}");
    }
    let store = open_store(&dir, migrations).await?;
    let term = store.get_terminal(TERMINAL_ID).await?;
    if term.process_status != "exited" || term.output_status != "closed" {
        bail!(
            "exit state not committed: {}/{}",
            term.process_status,
            term.output_status
        );
    }
    if term.exit_code != Some(0) || term.exit_kind.as_deref() != Some("clean") {
        bail!(
            "exit code/kind wrong: {:?}/{:?}",
            term.exit_code,
            term.exit_kind
        );
    }
    let events = store.session_events(TERMINAL_ID).await?;
    if events.len() != 3 || events[2].1 != "exit" {
        bail!("per-session event sequence wrong: {events:?}");
    }
    let (pruned, acked, last) = store.session_state(TERMINAL_ID).await?.unwrap();
    if (pruned, acked, last) != (0, 0, 3) {
        bail!("session_state {pruned}/{acked}/{last} want 0/0/3");
    }
    checks.push("terminal exit state + per-session event seq committed in ONE transaction".into());
    // Trace: publication strictly after commit for every line.
    let steps = trace_steps(&dir.join("trace.log"));
    let mut last_commit = 0usize;
    for (i, s) in steps.iter().enumerate() {
        if s == "txn_after_commit" {
            last_commit = i;
        }
        if s == "publish_after" && i < last_commit {
            bail!("publication recorded before commit");
        }
    }
    checks.push("publish only after commit (trace ordering)".into());
    Ok(ScenarioResult {
        name: "exit_event".into(),
        ok: true,
        checks,
    })
}

async fn scenario_ack_prune(
    base: &Path,
    migrations: &Path,
    recovers: &mut u32,
) -> Result<ScenarioResult> {
    let dir = fresh_dir(base, "ack_prune")?;
    let mut checks = Vec::new();
    let mut args = arg(
        arg(writer_args(&dir, migrations), "--append", 5),
        "--line-bytes",
        120,
    );
    args.push("--event-per-line".into());
    let out = run_child(&args)?;
    ensure_writer_ok(&out, "ack writer")?;
    let store = open_store(&dir, migrations).await?;

    store.ack_events(TERMINAL_ID, 2).await?;
    store.ack_events(TERMINAL_ID, 1).await?; // older ack must not move it back
    let acked = store.session_state(TERMINAL_ID).await?.unwrap().1;
    if acked != 2 {
        bail!("ack not monotonic: {acked}");
    }
    if store.ack_events(TERMINAL_ID, 9).await.is_ok() {
        bail!("ack above the committed bound was accepted");
    }
    checks.push("ack monotonic; over-bound ack rejected (no upper overflow)".into());

    let pruned_to = store.prune_events(TERMINAL_ID).await?;
    if pruned_to != 2 || store.event_count(TERMINAL_ID).await? != 3 {
        bail!("prune removed more than the acked prefix");
    }
    checks.push("prune removed only the acked contiguous prefix".into());

    // Persistence across a recovery in a new process.
    drop(store);
    let report = recover_child(migrations, &dir, recovers)?;
    if !report.actions.is_empty() {
        bail!("clean ack scenario needed repairs: {:?}", report.actions);
    }
    let store = open_store(&dir, migrations).await?;
    let (pruned, acked, last) = store.session_state(TERMINAL_ID).await?.unwrap();
    if (pruned, acked, last) != (2, 2, 5) {
        bail!("ack/prune state lost across recovery: {pruned}/{acked}/{last}");
    }
    if store.event_count(TERMINAL_ID).await? != 3 {
        bail!("unpruned events lost across recovery");
    }
    checks.push("unpruned events + ack state survive a restart".into());

    store.ack_events(TERMINAL_ID, 5).await?;
    store.prune_events(TERMINAL_ID).await?;
    if store.event_count(TERMINAL_ID).await? != 0 {
        bail!("final prune left events");
    }
    let (pruned, acked, last) = store.session_state(TERMINAL_ID).await?.unwrap();
    if (pruned, acked, last) != (5, 5, 5) {
        bail!("final ack/prune state {pruned}/{acked}/{last}");
    }
    checks.push("full ack + prune transactional and consistent".into());

    // The event table is now EMPTY: the next appended event must still
    // continue the published sequence from session_state (a MAX(event_seq)
    // allocator would restart at 1 and reuse a published sequence).
    drop(store);
    let mut args = arg(writer_args(&dir, migrations), "--append", 1);
    args = arg(args, "--line-bytes", 120);
    args.push("--event-per-line".into());
    let out = run_child(&args)?;
    ensure_writer_ok(&out, "post-prune event writer")?;
    let recs = parse_records(&out.stdout);
    let published_seq = recs
        .iter()
        .find_map(|r| r.get("event_published").and_then(|e| e["seq"].as_u64()))
        .context("post-prune event not published")?;
    if published_seq != 6 {
        bail!("post-prune event seq {published_seq} want 6 (continues last_appended_seq)");
    }
    let store = open_store(&dir, migrations).await?;
    let events = store.session_events(TERMINAL_ID).await?;
    if events != vec![(6u64, "output".to_string())] {
        bail!("post-prune events {events:?} want [(6, output)]");
    }
    let (pruned, acked, last) = store.session_state(TERMINAL_ID).await?.unwrap();
    if (pruned, acked, last) != (5, 5, 6) {
        bail!("post-prune session state {pruned}/{acked}/{last} want 5/5/6");
    }
    checks.push("event appended after a full prune continues the sequence at 6".into());
    Ok(ScenarioResult {
        name: "ack_prune".into(),
        ok: true,
        checks,
    })
}

async fn scenario_drain(
    base: &Path,
    migrations: &Path,
    step: &str,
    recovers: &mut u32,
    crashed: &mut u32,
) -> Result<ScenarioResult> {
    let dir = fresh_dir(base, &format!("drain_{step}"))?;
    let mut checks = Vec::new();
    let mut args = writer_args(&dir, migrations);
    args = arg(args, "--drain-step", step);
    args = arg(args, "--drain-fail", 12);
    args = arg(args, "--producer-lines", 20);
    args = arg(args, "--line-bytes", 100);
    let out = run_child(&args)?;
    ensure_writer_ok(&out, "drain writer")?;
    let recs = parse_records(&out.stdout);
    let drain = recs
        .iter()
        .find_map(|r| r.get("drain").cloned())
        .context("no drain record")?;
    let flushed = drain["flushed"].as_u64().unwrap();
    let dropped = drain["dropped"].as_u64().unwrap();
    if (flushed, dropped) != (15, 5) {
        bail!("drain {step}: flushed {flushed} dropped {dropped} want 15/5");
    }
    let gap = drain["gap"].as_array().cloned().unwrap_or_default();
    let gap: Vec<u64> = gap.iter().filter_map(|v| v.as_u64()).collect();
    if gap != vec![9, 14] {
        bail!("drain {step}: gap {gap:?} want [9, 14)");
    }
    let published = published_records(&recs);
    let lines: Vec<u64> = published.iter().map(|(l, _)| *l).collect();
    let want_lines: Vec<u64> = [1u64..=8, 14u64..=20].iter().cloned().flatten().collect();
    if lines != want_lines {
        bail!("drain {step}: published lines {lines:?} want {want_lines:?}");
    }
    checks.push(format!(
        "producer continued across {step} failures; 8-frame bound; 5 drops; gap [9,14)"
    ));

    let report = recover_child(migrations, &dir, recovers)?;
    if report.line_watermark != 20 || !report.degraded {
        bail!(
            "drain {step}: watermark {} degraded {}",
            report.line_watermark,
            report.degraded
        );
    }
    let pending = report
        .outcomes
        .iter()
        .filter(|o| o.class == SegmentClass::Pending)
        .count();
    if pending != 12 {
        bail!(
            "drain {step}: {pending} abandoned pending segments want 12 ({:?})",
            report.outcomes
        );
    }
    checks.push(format!(
        "{pending} abandoned segments quarantined whole, never adopted"
    ));
    let report2 = recover_child(migrations, &dir, recovers)?;
    if !report2.actions.is_empty() {
        bail!("drain {step}: recovery not idempotent");
    }

    let store = open_store(&dir, migrations).await?;
    let term = store.get_terminal(TERMINAL_ID).await?;
    if !term.degraded || !term.refuse_new_start {
        bail!("drain {step}: degraded/refuse flags not latched");
    }
    let gaps = store.list_gaps(TERMINAL_ID).await?;
    if gaps.len() != 1 || (gaps[0].first_line, gaps[0].last_line) != (9, 14) {
        bail!("drain {step}: gaps {gaps:?}");
    }
    // Exact gap behaviour: 1..8 readable, hole expired, 14..20 readable.
    let got = expect_bytes(
        &store,
        &dir,
        ReadCursor {
            line: 1,
            byte_offset: 0,
        },
        960,
    )
    .await?
    .context("line 1 expired")?;
    if got
        != (1u64..=8)
            .flat_map(|i| line_content(i, 100).into_bytes())
            .collect::<Vec<u8>>()
    {
        bail!("drain {step}: pre-gap lines wrong");
    }
    if expect_bytes(
        &store,
        &dir,
        ReadCursor {
            line: 9,
            byte_offset: 0,
        },
        10,
    )
    .await?
    .is_ok()
    {
        bail!("drain {step}: gap line 9 readable");
    }
    let got = expect_bytes(
        &store,
        &dir,
        ReadCursor {
            line: 14,
            byte_offset: 0,
        },
        700,
    )
    .await?
    .context("line 14 expired")?;
    if got
        != (14u64..=20)
            .flat_map(|i| line_content(i, 100).into_bytes())
            .collect::<Vec<u8>>()
    {
        bail!("drain {step}: post-gap lines wrong");
    }
    checks.push("exact gap: pre-gap lines readable, gap expired, post-gap lines readable".into());
    drop(store);

    // Refuse-new-start: a new writer start is rejected with a typed failure.
    *crashed += 1;
    let out = run_child(&arg(writer_args(&dir, migrations), "--append", 1))?;
    if out.code != Some(crate::crash::REFUSE_EXIT) {
        bail!("drain {step}: new start exit {:?} want REFUSE", out.code);
    }
    if !out.stdout.contains("refuse_new_start") {
        bail!("drain {step}: refusal did not identify itself");
    }
    checks.push("new writer start refused after overflow latch".into());
    Ok(ScenarioResult {
        name: format!("drain_{step}"),
        ok: true,
        checks,
    })
}

/// Crash/power-loss checkpoints inside recovery directory mutations: the
/// recover child dies at the exact boundary, the harness models the loss of
/// anything not yet durable, and reruns recovery to prove durable
/// idempotence (no adoption, no resurrection, artifacts survive).
async fn scenario_recovery_crash(
    base: &Path,
    migrations: &Path,
    recovers: &mut u32,
) -> Result<ScenarioResult> {
    let dir = fresh_dir(base, "recovery_crash")?;
    let mut checks = Vec::new();

    let quarantine_entries = |d: &Path| -> Result<Vec<String>> {
        let mut names = Vec::new();
        let qdir = d.join(crate::recovery::QUARANTINE_DIR);
        if !qdir.exists() {
            return Ok(names);
        }
        for e in std::fs::read_dir(&qdir)? {
            names.push(e?.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        Ok(names)
    };

    // Fixture A: synced-but-uncommitted tail (Partial class -> artifact +
    // truncate mutations). Seed + damage + crash at each tail checkpoint.
    for point in ["rec_artifact_written", "rec_tail_truncated"] {
        let d = fresh_dir(&dir, &format!("tail_{point}"))?;
        let out = run_child(&arg(
            arg(writer_args(&d, migrations), "--append", 3),
            "--line-bytes",
            120,
        ))?;
        ensure_writer_ok(&out, "recovery crash seed")?;
        let seg_path = d.join("seg-000001.log");
        let mut data = std::fs::read(&seg_path)?;
        let committed = data.len() as u64;
        data.extend_from_slice(b"\x00uncommitted-tail");
        std::fs::write(&seg_path, &data)?;

        recover_child_crash(migrations, &d, point, recovers)?;

        if point == "rec_artifact_written" {
            // Durability ordering proof: the artifact is synced BEFORE the
            // live file is truncated, so it exists while the live file still
            // carries the tail.
            let artifacts = quarantine_entries(&d)?;
            if artifacts.is_empty() {
                bail!("{point}: no quarantine artifact after the crash");
            }
            let live = std::fs::metadata(&seg_path)?.len();
            if live <= committed {
                bail!(
                    "{point}: artifact durable but live file already truncated ({live} <= {committed})"
                );
            }
            checks.push(format!(
                "{point}: artifact durable before the live file loses the tail"
            ));
        } else {
            // rec_tail_truncated: the live file is already cut back to the
            // committed boundary and the artifact is durable.
            let live = std::fs::metadata(&seg_path)?.len();
            if live != committed {
                bail!("{point}: live file {live} != committed {committed}");
            }
            if quarantine_entries(&d)?.is_empty() {
                bail!("{point}: artifact lost");
            }
            checks.push(format!(
                "{point}: live file truncated only after the artifact was durable"
            ));
        }

        // Rerun recovery in new processes (twice): convergence + idempotence.
        let report = recover_child(migrations, &d, recovers)?;
        if report.line_watermark != 3 || report.degraded || !report.gaps.is_empty() {
            bail!("{point}: rerun outcome {report:?}");
        }
        let live = std::fs::metadata(&seg_path)?.len();
        if live != committed {
            bail!("{point}: rerun left the file at {live} want {committed}");
        }
        let report2 = recover_child(migrations, &d, recovers)?;
        if !report2.actions.is_empty() {
            bail!("{point}: recovery not idempotent after the crash");
        }
        checks.push(format!("{point}: rerun converges and stays idempotent"));
    }

    // Fixture B: whole-file corruption (quarantine rename mutation) plus a
    // power loss that destroys the renamed artifact: the source name must
    // not resurrect, the gap must be explicit and the rerun idempotent.
    {
        let d = fresh_dir(&dir, "quarantine")?;
        let out = run_child(&arg(
            arg(writer_args(&d, migrations), "--append", 3),
            "--line-bytes",
            120,
        ))?;
        ensure_writer_ok(&out, "recovery crash seed b")?;
        let seg_path = d.join("seg-000001.log");
        let mut data = std::fs::read(&seg_path)?;
        data[crate::frame::SEGMENT_HEADER_LEN + 44] ^= 0xFF; // first frame payload
        std::fs::write(&seg_path, &data)?;

        recover_child_crash(migrations, &d, "rec_file_quarantined", recovers)?;
        if seg_path.exists() {
            bail!("quarantine crash left the source name in place");
        }
        let artifacts = quarantine_entries(&d)?;
        if artifacts.is_empty() {
            bail!("quarantine crash lost the artifact before the DB repair");
        }
        // Power loss: the rename's durability is destroyed (artifact gone,
        // source never resurrects).
        for a in &artifacts {
            std::fs::remove_file(d.join(crate::recovery::QUARANTINE_DIR).join(a))?;
        }
        crate::fsync_dir(&d.join(crate::recovery::QUARANTINE_DIR))?;

        let report = recover_child(migrations, &d, recovers)?;
        if report.line_watermark != 3 {
            bail!(
                "quarantine rerun watermark {} want 3",
                report.line_watermark
            );
        }
        if !report.degraded || report.gaps.len() != 1 {
            bail!("quarantine rerun must record an explicit gap + degraded: {report:?}");
        }
        if seg_path.exists() {
            bail!("source name resurrected across the power loss");
        }
        let report2 = recover_child(migrations, &d, recovers)?;
        if !report2.actions.is_empty() {
            bail!("quarantine rerun not idempotent");
        }
        checks.push(
            "power loss during quarantine rename: no resurrection, explicit gap, idempotent rerun"
                .into(),
        );
    }
    Ok(ScenarioResult {
        name: "recovery_crash".into(),
        ok: true,
        checks,
    })
}

/// Bounded drain overflow with REAL payloads: multi-frame lines exhaust the
/// frame bound and a maximum-sized record exhausts the byte bound; both
/// must drop explicitly (gap + degraded + refuse-new-start) instead of
/// buffering unbounded producer memory.
async fn scenario_drain_overflow(
    base: &Path,
    migrations: &Path,
    recovers: &mut u32,
    crashed: &mut u32,
) -> Result<ScenarioResult> {
    let dir = fresh_dir(base, "drain_overflow")?;
    let mut checks = Vec::new();

    // Multi-frame lines: 200000-byte lines (4 frames each) against an
    // 8-frame bound -> 2 queued, 4 dropped.
    {
        let d = fresh_dir(&dir, "multiframe")?;
        let mut args = writer_args(&d, migrations);
        args = arg(args, "--drain-step", "sync");
        args = arg(args, "--drain-fail", 5);
        args = arg(args, "--producer-lines", 6);
        args = arg(args, "--line-bytes", 200000);
        let out = run_child(&args)?;
        ensure_writer_ok(&out, "drain multiframe writer")?;
        let recs = parse_records(&out.stdout);
        let drain = recs
            .iter()
            .find_map(|r| r.get("drain").cloned())
            .context("no drain record")?;
        let (flushed, dropped) = (
            drain["flushed"].as_u64().unwrap(),
            drain["dropped"].as_u64().unwrap(),
        );
        if (flushed, dropped) != (2, 4) {
            bail!("multiframe overflow: flushed {flushed} dropped {dropped} want 2/4");
        }
        let peak_frames = drain["peak_queued_frames"].as_u64().unwrap();
        let peak_bytes = drain["peak_queued_bytes"].as_u64().unwrap();
        if peak_frames > crate::DRAIN_QUEUE_FRAMES as u64
            || peak_bytes > crate::DRAIN_QUEUE_BYTES as u64
        {
            bail!("queue exceeded its bounds: {peak_frames} frames / {peak_bytes} bytes");
        }
        let gap = drain["gap"].as_array().cloned().unwrap_or_default();
        let gap: Vec<u64> = gap.iter().filter_map(|v| v.as_u64()).collect();
        if gap != vec![3, 7] {
            bail!("multiframe overflow gap {gap:?} want [3, 7)");
        }
        let published = published_records(&recs);
        let lines: Vec<u64> = published.iter().map(|(l, _)| *l).collect();
        if lines != vec![1, 2] {
            bail!("multiframe overflow published {lines:?} want [1, 2]");
        }
        if published.iter().any(|(_, b)| *b != 200000) {
            bail!("multiframe overflow flushed regenerated/wrong payloads");
        }
        checks.push(format!(
            "multi-frame lines: 4-frame lines bound the queue (peak {peak_frames} frames / \
             {peak_bytes} bytes of REAL payloads); gap [3,7)"
        ));
        let report = recover_child(migrations, &d, recovers)?;
        if !report.degraded || report.line_watermark != 2 {
            bail!("multiframe overflow recovery {report:?}");
        }
        let store = open_store(&d, migrations).await?;
        let term = store.get_terminal(TERMINAL_ID).await?;
        if !term.degraded || !term.refuse_new_start {
            bail!("multiframe overflow did not latch degraded/refuse");
        }
        let got = expect_bytes(
            &store,
            &d,
            ReadCursor {
                line: 1,
                byte_offset: 0,
            },
            400000,
        )
        .await?
        .context("line 1 expired")?;
        if got.len() != 400000 {
            bail!(
                "multiframe overflow flushed content wrong ({} bytes)",
                got.len()
            );
        }
        // No fabricated continuity into the dropped range: a read pinned
        // near the end of line 2 stops exactly at the line end (the explicit
        // gap starts at line 3).
        let rd = read_from(
            &store,
            &d,
            TERMINAL_ID,
            ReadCursor {
                line: 2,
                byte_offset: 199900,
            },
            1000,
        )
        .await?;
        match rd {
            ReadOutcome::Data(dd) => {
                if dd.bytes != line_content(2, 200000).as_bytes()[199900..] {
                    bail!("multiframe overflow tail bytes wrong");
                }
                if !dd.at_end || dd.bytes.len() != 100 {
                    bail!("read continued into the dropped range");
                }
            }
            _ => bail!("pinned cursor at the gap boundary expired"),
        }
        let gaps = store.list_gaps(TERMINAL_ID).await?;
        if gaps.len() != 1 || (gaps[0].first_line, gaps[0].last_line) != (3, 7) {
            bail!("multiframe overflow gaps {gaps:?}");
        }
        drop(store);
        *crashed += 1;
        let out = run_child(&arg(writer_args(&d, migrations), "--append", 1))?;
        if out.code != Some(crate::crash::REFUSE_EXIT) {
            bail!("multiframe overflow new start exit {:?}", out.code);
        }
        checks.push("multi-frame overflow: degraded + gap + refuse-new-start retained".into());
    }

    // Maximum-sized record: a single line larger than the whole byte bound
    // is dropped even on an empty queue.
    {
        let d = fresh_dir(&dir, "max_record")?;
        let mut args = writer_args(&d, migrations);
        args = arg(args, "--drain-step", "commit");
        args = arg(args, "--drain-fail", 0);
        args = arg(args, "--producer-lines", 3);
        args = arg(args, "--line-bytes", 600000);
        let out = run_child(&args)?;
        ensure_writer_ok(&out, "drain max-record writer")?;
        let recs = parse_records(&out.stdout);
        let drain = recs
            .iter()
            .find_map(|r| r.get("drain").cloned())
            .context("no drain record")?;
        if (
            drain["flushed"].as_u64().unwrap(),
            drain["dropped"].as_u64().unwrap(),
        ) != (0, 3)
        {
            bail!(
                "max-record overflow: flushed {} dropped {}",
                drain["flushed"],
                drain["dropped"]
            );
        }
        let gap = drain["gap"].as_array().cloned().unwrap_or_default();
        let gap: Vec<u64> = gap.iter().filter_map(|v| v.as_u64()).collect();
        if gap != vec![1, 4] {
            bail!("max-record overflow gap {gap:?} want [1, 4)");
        }
        let report = recover_child(migrations, &d, recovers)?;
        if !report.degraded || report.line_watermark != 0 {
            bail!("max-record overflow recovery {report:?}");
        }
        let store = open_store(&d, migrations).await?;
        if !store.get_terminal(TERMINAL_ID).await?.refuse_new_start {
            bail!("max-record overflow did not latch refuse-new-start");
        }
        if store
            .segments(TERMINAL_ID)
            .await?
            .iter()
            .any(|s| s.state == "active")
        {
            // nothing was ever flushed, so nothing may be adopted
            bail!("max-record overflow adopted a segment");
        }
        drop(store);
        *crashed += 1;
        let out = run_child(&arg(writer_args(&d, migrations), "--append", 1))?;
        if out.code != Some(crate::crash::REFUSE_EXIT) {
            bail!("max-record overflow new start exit {:?}", out.code);
        }
        checks.push(
            "maximum-sized record (600000 B > 524288 B bound) refused on an empty queue; \
             gap [1,4) + degraded + refuse-new-start"
                .into(),
        );
    }
    Ok(ScenarioResult {
        name: "drain_overflow".into(),
        ok: true,
        checks,
    })
}

async fn scenario_clear_logs(
    base: &Path,
    migrations: &Path,
    recovers: &mut u32,
) -> Result<ScenarioResult> {
    let dir = fresh_dir(base, "clear_logs")?;
    let mut checks = Vec::new();
    let gate = dir.join("gate");
    let trace = dir.join("trace.log");
    let mut args = arg(
        arg(writer_args(&dir, migrations), "--append", 10),
        "--line-bytes",
        120,
    );
    args.push("--event-per-line".into());
    args = arg(args, "--gate-after-line", 3);
    args = arg(args, "--gate-file", gate.display().to_string());
    args = arg(args, "--trace", trace.display().to_string());

    let exe = std::env::current_exe()?;
    let errfile = std::fs::File::create(dir.join("child.stderr"))?;
    let mut child = Command::new(&exe)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::from(errfile))
        .spawn()?;
    let mut stdout = child.stdout.take().context("child stdout")?;
    let mut reader = BufReader::new(&mut stdout);
    let mut buf = String::new();
    let mut seen_after_deletion = 0u32;
    let mut deleted = false;
    loop {
        buf.clear();
        let n = reader.read_line(&mut buf)?;
        if n == 0 {
            break;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(buf.trim()) else {
            continue;
        };
        if let Some(p) = v.get("published") {
            let line = p["line"].as_u64().unwrap();
            if line == 3 && !deleted {
                for f in seg_files(&dir)? {
                    std::fs::remove_file(&f).with_context(|| format!("remove {}", f.display()))?;
                }
                std::fs::write(&gate, b"1")?;
                deleted = true;
                checks.push("logs cleared while the writer process was running".into());
            }
            if deleted && line > 3 {
                seen_after_deletion += 1;
            }
        }
    }
    let status = child.wait()?;
    if !status.success() {
        bail!("writer died after log cleanup: {status}");
    }
    if seen_after_deletion != 7 {
        bail!("writer published {seen_after_deletion} lines after deletion, want 7");
    }
    let steps = trace_steps(&trace);
    if !steps.contains(&"active_segment_vanished".to_string()) {
        bail!("writer did not notice the vanished segment");
    }
    checks.push("substitute writer survived, noticed the vanished segment, kept numbering".into());

    let store = open_store(&dir, migrations).await?;
    let before = snapshot(&store, &dir).await?;
    drop(store);
    let report = recover_child(migrations, &dir, recovers)?;
    if report.line_watermark != 10 {
        bail!("clear_logs: watermark {} want 10", report.line_watermark);
    }
    if !report.degraded {
        bail!("clear_logs: deleted history must degrade");
    }
    let gaps = report.gaps.clone();
    if gaps.len() != 1
        || (
            gaps[0].first_line,
            gaps[0].last_line,
            gaps[0].reason.as_str(),
        ) != (1, 4, "missing")
    {
        bail!("clear_logs: gaps {gaps:?}");
    }
    let store = open_store(&dir, migrations).await?;
    let after = snapshot(&store, &dir).await?;
    if after.epoch != before.epoch || after.watermark != before.watermark {
        bail!("clear_logs: terminal identity/watermark changed");
    }
    if after.process_status != "running" {
        bail!(
            "clear_logs: process marker reset to {}",
            after.process_status
        );
    }
    let events = store.event_count(TERMINAL_ID).await?;
    if events != 10 {
        bail!("clear_logs: unpruned events {events} want 10");
    }
    checks.push(
        "process marker, line_watermark, terminal record and unpruned events preserved".into(),
    );
    let got = expect_bytes(
        &store,
        &dir,
        ReadCursor {
            line: 4,
            byte_offset: 0,
        },
        120,
    )
    .await?
    .context("post-deletion lines expired")?;
    if got != line_content(4, 120).as_bytes() {
        bail!("clear_logs: post-deletion content wrong");
    }
    if expect_bytes(
        &store,
        &dir,
        ReadCursor {
            line: 1,
            byte_offset: 0,
        },
        10,
    )
    .await?
    .is_ok()
    {
        bail!("clear_logs: deleted line still readable");
    }
    checks.push("explicit gap for the deleted range; later lines readable".into());
    Ok(ScenarioResult {
        name: "clear_logs".into(),
        ok: true,
        checks,
    })
}

async fn scenario_destructive(
    base: &Path,
    migrations: &Path,
    recovers: &mut u32,
) -> Result<ScenarioResult> {
    let dir = fresh_dir(base, "destructive")?;
    let mut checks = Vec::new();
    let out = run_child(&arg(
        arg(writer_args(&dir, migrations), "--append", 2),
        "--line-bytes",
        120,
    ))?;
    ensure_writer_ok(&out, "destructive writer")?;
    let old_epoch = writer_done_epoch(&parse_records(&out.stdout)).context("old epoch")?;
    let old_epoch_bytes: [u8; 16] = Uuid::parse_str(&old_epoch)?.into_bytes();

    // Deleting the DB (+WAL+SHM) is the destructive, unexplainable rebuild.
    std::fs::remove_file(dir.join(DB_FILE))?;
    std::fs::remove_file(dir.join(format!("{DB_FILE}-wal"))).ok();
    std::fs::remove_file(dir.join(format!("{DB_FILE}-shm"))).ok();
    let report = recover_child(migrations, &dir, recovers)?;
    if !report.created_terminal || !report.destructive_rebuild {
        bail!(
            "destructive: created={} rebuild={}",
            report.created_terminal,
            report.destructive_rebuild
        );
    }
    let new_epoch = Uuid::from_bytes(report.epoch).to_string();
    if new_epoch == old_epoch {
        bail!("destructive rebuild kept the epoch");
    }
    if dir.join("seg-000001.log").exists() {
        bail!("destructive rebuild left the old segment in place");
    }
    checks.push(
        "DB+WAL+SHM deletion => destructive rebuild, new epoch, old files quarantined".into(),
    );

    let store = open_store(&dir, migrations).await?;
    let term = store.get_terminal(TERMINAL_ID).await?;
    let earliest = earliest_position(&store, TERMINAL_ID).await?;
    match term.validate_cursor(&old_epoch_bytes, 1, (earliest.line, earliest.byte_offset)) {
        Err(e) => {
            checks.push(format!(
                "old-epoch cursor rejected with earliest position (line {}, byte {})",
                e.earliest_line, e.earliest_byte_offset
            ));
        }
        Ok(()) => bail!("old cursor accepted after destructive rebuild"),
    }
    drop(store);
    let out = run_child(&arg(
        arg(writer_args(&dir, migrations), "--append", 1),
        "--line-bytes",
        120,
    ))?;
    ensure_writer_ok(&out, "post-destructive writer")?;
    let recs = parse_records(&out.stdout);
    let epoch = writer_done_epoch(&recs).unwrap();
    if epoch != new_epoch {
        bail!("post-destructive writer used epoch {epoch} want {new_epoch}");
    }
    checks.push("writer continues under the new epoch (numbering restart is the documented destructive cost)".into());
    Ok(ScenarioResult {
        name: "destructive".into(),
        ok: true,
        checks,
    })
}

async fn scenario_migration_rollback(base: &Path, migrations: &Path) -> Result<ScenarioResult> {
    let dir = fresh_dir(base, "migration_rollback")?;
    let mut checks = Vec::new();
    // Fixture migrations: the real 0001 plus a 0009 whose second statement
    // fails; sqlx runs each migration inside a transaction, so nothing of
    // 0009 may survive.
    let bad = dir.join("bad-migrations");
    std::fs::create_dir_all(&bad)?;
    std::fs::copy(migrations.join("0001_init.sql"), bad.join("0001_init.sql"))?;
    std::fs::write(
        bad.join("0009_rollback_probe.sql"),
        "CREATE TABLE migration_probe_should_vanish (id INTEGER PRIMARY KEY);\n\
         INSERT INTO log_gap (terminal_id, first_line, last_line, reason, created_ms)\n\
         VALUES ('rollback-probe', 5, 3, 'missing', 0);\n",
    )?;
    let db = dir.join(DB_FILE);
    let failed = Store::open(&db, &bad).await;
    if failed.is_ok() {
        bail!("broken migration unexpectedly succeeded");
    }
    checks.push("failing migration aborts Store::open".into());
    drop(failed);

    // Nothing of the failed migration survived (transactional rollback).
    let only1 = dir.join("only1-migrations");
    std::fs::create_dir_all(&only1)?;
    std::fs::copy(
        migrations.join("0001_init.sql"),
        only1.join("0001_init.sql"),
    )?;
    let store = Store::open(&db, &only1).await?;
    if store.table_exists("migration_probe_should_vanish").await? {
        bail!("partial table survived the migration rollback");
    }
    let versions = store.applied_migration_versions().await?;
    if versions != vec![1] {
        bail!("migration versions after rollback {versions:?} want [1]");
    }
    checks.push("transactional rollback: no partial DDL, version not recorded".into());
    drop(store);

    // The real migration set upgrades the same DB 0001 -> 0002 -> 0003
    // cleanly.
    let store = Store::open(&db, migrations).await?;
    let versions = store.applied_migration_versions().await?;
    if versions != vec![1, 2, 3] {
        bail!("upgrade path versions {versions:?} want [1, 2, 3]");
    }
    checks.push("existing DB upgrades 0001 -> 0002 -> 0003 cleanly after the rollback".into());
    Ok(ScenarioResult {
        name: "migration_rollback".into(),
        ok: true,
        checks,
    })
}

async fn scenario_indexed_damage(
    base: &Path,
    migrations: &Path,
    kind: &str,
    recovers: &mut u32,
) -> Result<ScenarioResult> {
    let dir = fresh_dir(base, &format!("damage_{kind}"))?;
    let mut checks = Vec::new();
    let out = run_child(&arg(
        arg(writer_args(&dir, migrations), "--append", 3),
        "--line-bytes",
        120,
    ))?;
    ensure_writer_ok(&out, "damage writer")?;
    let seg_path = dir.join("seg-000001.log");
    let data = std::fs::read(&seg_path)?;
    let scan = crate::frame::scan_frames(&data, crate::frame::SEGMENT_HEADER_LEN);
    if scan.outcome != crate::frame::ScanOutcome::Clean {
        bail!("damage setup: segment not clean");
    }

    let want_gap: (u64, u64, &str) = match kind {
        "missing" => {
            std::fs::remove_file(&seg_path)?;
            (1, 4, "missing")
        }
        "truncated" => {
            // Physical truncation mid frame-2 header (power-loss style).
            let cut = scan.frames[1].start as u64 + 10;
            truncate_to(&seg_path, cut)?;
            (2, 4, "truncated")
        }
        "corrupt" => {
            let mut d = data.clone();
            d[scan.frames[1].payload_at] ^= 0xFF;
            std::fs::write(&seg_path, &d)?;
            (2, 4, "corrupt")
        }
        "bad_tail" => {
            let mut d = data.clone();
            d.extend_from_slice(b"\x00not-a-frame-at-all");
            std::fs::write(&seg_path, &d)?;
            (0, 0, "")
        }
        other => bail!("unknown damage kind {other}"),
    };

    let report = recover_child(migrations, &dir, recovers)?;
    if report.line_watermark != 3 {
        bail!(
            "damage {kind}: watermark {} want 3 (never lowered)",
            report.line_watermark
        );
    }
    let gaps = report.gaps.clone();
    if want_gap.2.is_empty() {
        if !gaps.is_empty() || report.degraded {
            bail!("damage {kind}: uncommitted tail fabricated gaps/degradation");
        }
        let len = std::fs::metadata(&seg_path)?.len();
        if len != scan.frames[2].end as u64 {
            bail!("damage {kind}: tail not truncated to committed boundary ({len})");
        }
        checks.push("bad tail truncated to committed boundary, never adopted, no gap".into());
    } else {
        if !report.degraded {
            bail!("damage {kind}: degraded not set");
        }
        if gaps.len() != 1
            || (
                gaps[0].first_line,
                gaps[0].last_line,
                gaps[0].reason.as_str(),
            ) != (want_gap.0, want_gap.1, want_gap.2)
        {
            bail!("damage {kind}: gaps {gaps:?} want {want_gap:?}");
        }
        if kind == "truncated" || kind == "corrupt" {
            let len = std::fs::metadata(&seg_path)?.len();
            let expect = if kind == "truncated" {
                scan.frames[0].end
            } else {
                scan.frames[0].end
            };
            if len != expect as u64 {
                bail!("damage {kind}: file len {len} want {expect}");
            }
        }
        checks.push(format!(
            "indexed {kind} data => explicit log_gap [{}, {}) + degraded; watermark kept",
            want_gap.0, want_gap.1
        ));
    }

    let store = open_store(&dir, migrations).await?;
    let snap1 = snapshot(&store, &dir).await?;
    let report2 = recover_child(migrations, &dir, recovers)?;
    if !report2.actions.is_empty() || snapshot(&store, &dir).await? != snap1 {
        bail!("damage {kind}: recovery not idempotent");
    }
    drop(store);

    // Numbering never reuses the lost lines' successors incorrectly.
    let out = run_child(&arg(
        arg(writer_args(&dir, migrations), "--append", 1),
        "--line-bytes",
        120,
    ))?;
    ensure_writer_ok(&out, "damage continuation")?;
    let published = published_records(&parse_records(&out.stdout));
    if published.first().map(|(l, _)| *l) != Some(4) {
        bail!(
            "damage {kind}: continuation {:?} want line 4",
            published.first()
        );
    }
    checks.push("numbering continues at 4; no line reuse".into());
    Ok(ScenarioResult {
        name: format!("damage_{kind}"),
        ok: true,
        checks,
    })
}

// ────────────────────────────── entry point ────────────────────────────────

fn fresh_dir(base: &Path, name: &str) -> Result<PathBuf> {
    let dir = base.join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn ensure_writer_ok(out: &ChildOut, what: &str) -> Result<()> {
    if !out.success {
        bail!("{what} failed ({}): {}", out.code.unwrap_or(-1), out.stderr);
    }
    Ok(())
}

pub async fn gate_c(workdir: &Path, migrations: &Path) -> Result<GateSummary> {
    let started = Instant::now();
    std::fs::create_dir_all(workdir)?;
    let matrix_dir = workdir.join("matrix");
    let scenarios_dir = workdir.join("scenarios");
    std::fs::create_dir_all(&matrix_dir)?;
    std::fs::create_dir_all(&scenarios_dir)?;

    let mut crashed = 0u32;
    let mut recovers = 0u32;
    let mut points = Vec::new();
    let mut scenarios = Vec::new();

    for spec in matrix_specs() {
        let deadline_check = || -> Result<()> {
            if started.elapsed() > GATE_BUDGET {
                bail!("gate exceeded its internal budget");
            }
            Ok(())
        };
        deadline_check()?;
        points.push(
            run_matrix_point(&matrix_dir, migrations, &spec, &mut crashed, &mut recovers)
                .await
                .with_context(|| format!("matrix point {}", spec.point))?,
        );
    }

    scenarios.push(scenario_utf8_frames(&scenarios_dir, migrations, &mut recovers).await?);
    scenarios.push(scenario_rotations(&scenarios_dir, migrations).await?);
    scenarios.push(scenario_cursor_cleanup(&scenarios_dir, migrations, &mut recovers).await?);
    scenarios.push(scenario_tail_revision_multiframe(&scenarios_dir, migrations).await?);
    scenarios.push(scenario_commit_before_query(&scenarios_dir, migrations).await?);
    scenarios.push(scenario_exit_event(&scenarios_dir, migrations).await?);
    scenarios.push(scenario_ack_prune(&scenarios_dir, migrations, &mut recovers).await?);
    for step in ["append", "sync", "commit"] {
        scenarios.push(
            scenario_drain(
                &scenarios_dir,
                migrations,
                step,
                &mut recovers,
                &mut crashed,
            )
            .await
            .with_context(|| format!("drain {step}"))?,
        );
    }
    scenarios.push(
        scenario_drain_overflow(&scenarios_dir, migrations, &mut recovers, &mut crashed).await?,
    );
    scenarios.push(scenario_recovery_crash(&scenarios_dir, migrations, &mut recovers).await?);
    scenarios.push(scenario_clear_logs(&scenarios_dir, migrations, &mut recovers).await?);
    scenarios.push(scenario_destructive(&scenarios_dir, migrations, &mut recovers).await?);
    scenarios.push(scenario_migration_rollback(&scenarios_dir, migrations).await?);
    for kind in ["missing", "truncated", "corrupt", "bad_tail"] {
        scenarios.push(
            scenario_indexed_damage(&scenarios_dir, migrations, kind, &mut recovers)
                .await
                .with_context(|| format!("damage {kind}"))?,
        );
    }

    let total_checks = points.iter().map(|p| p.checks.len()).sum::<usize>()
        + scenarios.iter().map(|s| s.checks.len()).sum::<usize>();
    let ok = points.iter().all(|p| p.ok) && scenarios.iter().all(|s| s.ok);
    Ok(GateSummary {
        stage: "gate-c",
        started_unix_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        duration_ms: started.elapsed().as_millis(),
        points,
        scenarios,
        crashed_children: crashed,
        recover_children: recovers,
        total_checks,
        ok,
    })
}
