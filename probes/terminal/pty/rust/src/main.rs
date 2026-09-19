// Throwaway PTY lifecycle / Stop-barrier probe driver (probe B), Gate B
// restructure: per-terminal cgroup v2 + pty-process PTY/spawn/read/resize +
// self-managed non-blocking writes. NOT production code.
//
// Phases:
//   --phase crash <workdir>  spawn a live fixture, write registry.json, exit
//                            abruptly (leaves the fixture running on purpose)
//   --phase main  <workdir>  run all scenarios incl. registry recovery
//   --cleanup <workdir>      kill + remove the probe cgroup root (identity by
//                            construction: the probe itself is never in it)

mod cgroup;
mod proc;
mod pty;
mod quota;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use pty::{
    AbortReason, EnvSpec, Event, FaultStep, OutputEnd, SendOutcome, SpawnError, SpawnParams,
    StopParams, StopStats, Terminal, Terminals, WRITE_CHUNK,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const TERM_GRACE: Duration = Duration::from_millis(600);
const KILL_GRACE: Duration = Duration::from_secs(3);
const BLOCK_TOTAL: usize = 256 * 1024;
/// Probe-local quota limits for the runtime scenario (design defaults are
/// 8/32; smaller values exercise the same rules with fewer PTYs). The
/// global limit must also cover the four slots deliberately left occupied
/// by the failed-cleanup scenarios (stop-cancellation fault + three
/// rollback cleanup failures), which run last.
const SESSION_LIMIT: usize = 2;
const GLOBAL_LIMIT: usize = 4;

macro_rules! check {
    ($cond:expr, $($arg:tt)*) => {
        if !$cond {
            anyhow::bail!("assertion failed: {}", format!($($arg)*));
        }
    };
}

#[derive(Serialize, Deserialize)]
struct RegistryFile {
    active: Vec<RegistryEntry>,
}

#[derive(Serialize, Deserialize)]
struct RegistryEntry {
    terminal_id: String,
    pid: i32,
    starttime: u64,
}

#[derive(Serialize)]
struct InterruptedRecord {
    terminal_id: String,
    pid: i32,
    starttime: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signal: Option<i32>,
}

/// Injectable signal backend for the registry recovery proof: recovery must
/// NEVER signal a recorded pid (its identity cannot be trusted after a crash).
trait SignalBackend {
    // Intentionally call-free in recovery: the method exists so the recording
    // backend can PROVE (calls == 0) that recovery never signals a pid whose
    // identity may have been reused after a crash.
    #[allow(dead_code)]
    fn send_signal(&self, pid: i32, sig: i32);
}

#[derive(Default)]
struct RecordingBackend {
    calls: Mutex<Vec<(i32, i32)>>,
}

impl SignalBackend for RecordingBackend {
    fn send_signal(&self, pid: i32, sig: i32) {
        self.calls.lock().unwrap().push((pid, sig));
    }
}

/// Real registry recovery: mark every active entry Interrupted. Sends no
/// signals and invents no exit result — the backend parameter exists to
/// prove (via the recording implementation) that no signal path is taken.
fn recover_registry<B: SignalBackend>(
    registry: &RegistryFile,
    _backend: &B,
) -> Vec<InterruptedRecord> {
    registry
        .active
        .iter()
        .map(|entry| InterruptedRecord {
            terminal_id: entry.terminal_id.clone(),
            pid: entry.pid,
            starttime: entry.starttime,
            exit_code: None,
            signal: None,
        })
        .collect()
}

struct Ctx {
    workdir: PathBuf,
    fixture: PathBuf,
    terminals: Terminals,
    measurements: Mutex<BTreeMap<String, Value>>,
}

impl Ctx {
    fn record(&self, key: &str, value: Value) {
        println!("  measure {key}={value}");
        self.measurements
            .lock()
            .unwrap()
            .insert(key.to_string(), value);
    }

    async fn spawn_fixture(
        &self,
        session: &str,
        mode: &str,
        extra: &[&str],
        env: EnvSpec,
        raw: bool,
    ) -> Result<Arc<Terminal>> {
        self.spawn_fixture_fault(session, mode, extra, env, raw, None)
            .await
    }

    async fn spawn_fixture_raw(
        &self,
        session: &str,
        mode: &str,
        extra: &[&str],
        env: EnvSpec,
        fault: Option<FaultStep>,
    ) -> Result<Terminal, SpawnError> {
        let mut args = vec![mode.to_string()];
        args.extend(extra.iter().map(|s| s.to_string()));
        let mut params = SpawnParams::new(
            &self.fixture.to_string_lossy(),
            &args.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            &self.workdir,
            env,
            session,
        );
        params.fault = fault;
        self.terminals.spawn(params).await
    }

    async fn spawn_fixture_fault(
        &self,
        session: &str,
        mode: &str,
        extra: &[&str],
        env: EnvSpec,
        raw: bool,
        fault: Option<FaultStep>,
    ) -> Result<Arc<Terminal>> {
        let terminal = self
            .spawn_fixture_raw(session, mode, extra, env, fault)
            .await
            .map_err(|e| anyhow!("spawn fixture {mode}: {e:?}"))?;
        if raw {
            terminal
                .set_raw()
                .map_err(|e| anyhow!("set raw mode: {e}"))?;
        }
        Ok(Arc::new(terminal))
    }

    async fn spawn_program(
        &self,
        session: &str,
        program: &str,
        args: &[&str],
    ) -> Result<Arc<Terminal>> {
        Ok(Arc::new(
            self.terminals
                .spawn(SpawnParams::new(
                    program,
                    args,
                    &self.workdir,
                    EnvSpec::Snapshot(path_env()),
                    session,
                ))
                .await
                .map_err(|e| anyhow!("spawn {program}: {e:?}"))?,
        ))
    }

    fn dump_events(&self, name: &str, terminal: &Terminal) -> Result<()> {
        let path = self.workdir.join("events.jsonl");
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .context("open events.jsonl")?;
        for (t, event) in terminal.events() {
            let line = json!({
                "scenario": name,
                "t_ms": (t * 1000.0).round() / 1000.0,
                "event": event.kind(),
                "detail": format!("{event:?}"),
            });
            writeln!(file, "{line}").context("write events.jsonl")?;
        }
        Ok(())
    }
}

fn path_env() -> Vec<(String, String)> {
    vec![(
        "PATH".to_string(),
        std::env::var("PATH").unwrap_or_default(),
    )]
}

fn expect_spawn_err<T>(result: Result<T, SpawnError>, what: &str) -> Result<SpawnError> {
    match result {
        Ok(_) => bail!("{what} should have been rejected"),
        Err(e) => Ok(e),
    }
}

fn fixture_path() -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var("QINGLUAN_PTY_FIXTURE_BIN")
            .map_err(|_| anyhow!("QINGLUAN_PTY_FIXTURE_BIN not set"))?,
    ))
}

fn stop_stats_value(stats: &StopStats) -> Value {
    json!({
        "forced": stats.forced,
        "term_phase_ms": stats.term_phase_ms,
        "kill_phase_ms": stats.kill_phase_ms,
        "total_ms": stats.total_ms,
        "term_rounds": stats.term_rounds,
        "signalled": stats.signalled,
        "output_forced_close": stats.output_forced_close,
        "existing_state": stats.existing_state,
    })
}

fn expect_complete(outcome: SendOutcome) -> Result<usize> {
    match outcome {
        SendOutcome::Complete { written } => Ok(written),
        other => bail!("expected complete write, got {other:?}"),
    }
}

fn default_params() -> StopParams {
    StopParams {
        term_grace: TERM_GRACE,
        empty_wait: KILL_GRACE,
        output_wait: Duration::from_secs(1),
    }
}

/// The shared failure message of a stop result, for same-failure checks.
fn failure_message(result: Result<StopStats>) -> Option<String> {
    result.err().map(|e| format!("{e:#}"))
}

async fn ask_size(terminal: &Terminal, generation: u64) -> Result<()> {
    expect_complete(terminal.send(b"size\n".to_vec(), generation).await)?;
    Ok(())
}

/// Wait for the natural lifecycle end: root exited on its own, output closed
/// on its own, then finalize (quota released via the non-stop path). Also
/// asserts the lifecycle event cardinality.
async fn wait_finalized(ctx: &Ctx, terminal: &Terminal, name: &str) -> Result<()> {
    terminal
        .wait_event(
            |e| matches!(e, Event::ProcessExited { .. }),
            Duration::from_secs(10),
        )
        .await
        .context("root never exited")?;
    terminal
        .wait_event(
            |e| matches!(e, Event::OutputClosed { .. }),
            Duration::from_secs(10),
        )
        .await
        .context("output never closed")?;
    check!(
        terminal.is_exited() && terminal.is_output_closed(),
        "state flags disagree with events"
    );
    check!(
        terminal.count_events(|e| matches!(e, Event::ProcessExited { .. })) == 1,
        "exactly one ProcessExited expected"
    );
    check!(
        terminal.count_events(|e| matches!(e, Event::OutputClosed { .. })) == 1,
        "exactly one OutputClosed expected"
    );
    terminal.finalize().await?;
    check!(
        terminal.quota_state() == quota::SlotState::Released,
        "quota must be released after finalize"
    );
    check!(
        terminal.count_events(|e| matches!(e, Event::QuotaReleased)) == 1,
        "exactly one QuotaReleased expected"
    );
    ctx.dump_events(name, terminal)
}

/// Consider the write blocked once the byte count is partial and stops moving
/// across several polls (the tty input queue is full).
async fn wait_blocked(progress: &std::sync::atomic::AtomicUsize, total: usize) -> Result<usize> {
    let mut last = 0usize;
    let mut stable = 0u32;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let current = progress.load(Ordering::SeqCst);
        if current == last {
            stable += 1;
        } else {
            stable = 0;
        }
        last = current;
        if current > 0 && current < total && stable >= 6 {
            return Ok(current);
        }
        check!(
            Instant::now() < deadline,
            "write never blocked (progress={current} total={total})"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_gone(pid: i32, starttime: u64, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while proc::alive_with_starttime(pid, starttime) {
        check!(
            Instant::now() < deadline,
            "pid {pid} still alive after {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Ok(())
}

// --- Scenarios -----------------------------------------------------------------

async fn scenario_identity(ctx: &Ctx) -> Result<()> {
    let terminal = ctx
        .spawn_fixture(
            "identity",
            "park",
            &[],
            EnvSpec::Snapshot(path_env()),
            false,
        )
        .await?;
    let stat = proc::read_stat(terminal.root_pid).context("read root stat")?;
    check!(
        stat.pid == stat.pgrp && stat.pgrp == stat.session,
        "root must be its own session and group leader, got {stat:?}"
    );
    check!(stat.session == terminal.sid, "sid mismatch");
    let fg = terminal.tcgetpgrp_now().context("tcgetpgrp")?;
    check!(
        fg == terminal.root_pid,
        "initial foreground pgrp {fg} must be the root {}",
        terminal.root_pid
    );
    // Gate B: the root must live in its own terminal cgroup.
    let cg_path = terminal.cgroup_path().context("no Spawned cgroup event")?;
    let root_cg = cgroup::proc_cgroup_path(terminal.root_pid).context("root cgroup path")?;
    check!(
        root_cg.starts_with(&cg_path),
        "root must join its terminal cgroup: {:?} !~ {:?}",
        root_cg,
        cg_path
    );
    check!(
        terminal.cgroup_populated(),
        "terminal cgroup must be populated"
    );
    terminal
        .wait_output_contains("PARKING", Duration::from_secs(5))
        .await
        .context("fixture never announced PARKING")?;
    let stats = terminal.stop(default_params()).await?;
    check!(
        !stats.forced,
        "TERM-responsive fixture must stop gracefully"
    );
    check!(
        matches!(terminal.exit_info(), Some((_, Some(15)))),
        "expected SIGTERM death, got {:?}",
        terminal.exit_info()
    );
    check!(
        terminal.quota_state() == quota::SlotState::Released,
        "quota must be released after stop"
    );
    check_lifecycle_cardinality(&terminal, true)?;
    ctx.record("identity", stop_stats_value(&stats));
    ctx.dump_events("identity", &terminal)
}

fn check_lifecycle_cardinality(terminal: &Terminal, stopped: bool) -> Result<()> {
    check!(
        terminal.count_events(|e| matches!(e, Event::ProcessExited { .. })) == 1,
        "exactly one ProcessExited expected"
    );
    check!(
        terminal.count_events(|e| matches!(e, Event::OutputClosed { .. })) == 1,
        "exactly one OutputClosed expected"
    );
    check!(
        terminal.count_events(|e| matches!(e, Event::QuotaReleased)) == 1,
        "exactly one QuotaReleased expected"
    );
    if stopped {
        check!(
            terminal.count_events(|e| matches!(e, Event::StopIntentCommitted)) == 1,
            "exactly one StopIntentCommitted expected"
        );
        check!(
            terminal.count_events(|e| matches!(e, Event::StopCompleted { .. })) == 1,
            "exactly one StopCompleted expected"
        );
    }
    Ok(())
}

async fn scenario_env(ctx: &Ctx) -> Result<()> {
    let mut full = path_env();
    full.push((
        "QINGLUAN_PROBE_MARKER".into(),
        "snapshot-marker-value".into(),
    ));
    let terminal = ctx
        .spawn_fixture(
            "env-full",
            "env-report",
            &[],
            EnvSpec::Snapshot(full),
            false,
        )
        .await?;
    let output = terminal
        .wait_output_contains("ENV-REPORT", Duration::from_secs(5))
        .await
        .context("env-report (full) never answered")?;
    let text = String::from_utf8_lossy(&output).into_owned();
    check!(
        text.contains("marker=snapshot-marker-value"),
        "full snapshot marker missing: {text}"
    );
    check!(
        text.contains("path=true"),
        "full snapshot PATH missing: {text}"
    );
    check!(
        text.contains("probeonly=false"),
        "probe env leaked into full snapshot: {text}"
    );
    check!(
        text.contains(&format!("cwd={}", ctx.workdir.display())),
        "explicit cwd not used"
    );
    wait_finalized(ctx, &terminal, "env-full").await?;

    let terminal = ctx
        .spawn_fixture("env-empty", "env-report", &[], EnvSpec::Empty, false)
        .await?;
    let output = terminal
        .wait_output_contains("ENV-REPORT", Duration::from_secs(5))
        .await
        .context("env-report (empty) never answered")?;
    let text = String::from_utf8_lossy(&output).into_owned();
    check!(
        text.contains("marker=absent"),
        "empty env must not inherit marker: {text}"
    );
    check!(
        text.contains("path=false"),
        "empty env must not inherit PATH: {text}"
    );
    check!(
        text.contains("probeonly=false"),
        "probe env leaked into empty env: {text}"
    );
    wait_finalized(ctx, &terminal, "env-empty").await?;

    // Missing snapshot: rejected before any PTY, process, cgroup, or quota.
    let occupied_before = ctx.terminals.quota().occupying();
    let names_before = ctx.terminals.terminal_cgroup_names();
    let err = expect_spawn_err(
        ctx.spawn_fixture_raw("env-missing", "park", &[], EnvSpec::Missing, None)
            .await,
        "missing env",
    )?;
    check!(
        matches!(err, SpawnError::EnvMissing),
        "expected EnvMissing, got {err:?}"
    );
    check!(
        ctx.terminals.quota().occupying() == occupied_before,
        "missing env must not consume quota"
    );
    check!(
        ctx.terminals.terminal_cgroup_names() == names_before,
        "missing env must not create any cgroup"
    );
    ctx.record(
        "env",
        json!({"full": "marker+path", "empty": "no marker, no PATH", "missing": "rejected", "probe_env_leak": false}),
    );
    Ok(())
}

async fn scenario_write_bytes(ctx: &Ctx) -> Result<()> {
    // The caller Vec carries 8 MiB of spare capacity with a tiny len: the
    // enqueue-time normalization must make the accepted queue back exactly
    // the 300 payload bytes (see the pty unit test); the fixture checksum
    // proves exactly those bytes were delivered.
    let mut payload: Vec<u8> = Vec::with_capacity(8 * 1024 * 1024);
    payload.extend_from_slice(b"line1\n");
    payload.extend_from_slice("中文多行\nsecond-line\n".as_bytes());
    payload.extend_from_slice(&[0x03, 0x1b, b'[', b'2', b'K']);
    payload.extend_from_slice(b"tail-no-newline");
    while payload.len() < 300 {
        payload.push(b'A');
    }
    payload.truncate(300);
    check!(
        payload.capacity() >= 8 * 1024 * 1024,
        "caller Vec must carry its spare capacity into send"
    );
    let expected_sum: u64 = payload
        .iter()
        .fold(0u64, |acc, b| acc.wrapping_add(*b as u64));
    let n = payload.len();

    // raw+noecho so control bytes reach the fixture untouched.
    let terminal = ctx
        .spawn_fixture(
            "write-bytes",
            "bytecount",
            &[&n.to_string()],
            EnvSpec::Snapshot(path_env()),
            true,
        )
        .await?;
    let generation = terminal.generation_now();
    let written = expect_complete(terminal.send(payload, generation).await)?;
    check!(written == n, "written {written} must equal {n}");
    let confirmation = format!("READ {n} CHECKSUM {expected_sum}");
    terminal
        .wait_output_contains(&confirmation, Duration::from_secs(10))
        .await
        .context("bytecount fixture never confirmed delivery")?;
    wait_finalized(ctx, &terminal, "write-bytes").await?;
    ctx.record(
        "write_bytes",
        json!({"payload_bytes": n, "written": n, "checksum": expected_sum.to_string(), "payload_spare_capacity": "8MiB", "accepted_backing": "exact-length"}),
    );
    Ok(())
}

async fn scenario_resize(ctx: &Ctx) -> Result<()> {
    let terminal = ctx
        .spawn_fixture(
            "resize",
            "size-report",
            &[],
            EnvSpec::Snapshot(path_env()),
            false,
        )
        .await?;
    terminal
        .wait_output_contains("SIZE-READY", Duration::from_secs(5))
        .await
        .context("size-report never ready")?;
    let generation = terminal.generation_now();

    ask_size(&terminal, generation).await?;
    terminal
        .wait_output_contains("SIZE 120 30", Duration::from_secs(5))
        .await
        .context("initial size must be 120x30")?;

    terminal
        .resize(40, 100)
        .await
        .map_err(|e| anyhow!("resize 40x100: {e}"))?;
    ask_size(&terminal, generation).await?;
    terminal
        .wait_output_contains("SIZE 100 40", Duration::from_secs(5))
        .await
        .context("after resize(40,100) fixture must see 100x40")?;

    terminal
        .resize(30, 80)
        .await
        .map_err(|e| anyhow!("resize 30x80: {e}"))?;
    ask_size(&terminal, generation).await?;
    terminal
        .wait_output_contains("SIZE 80 30", Duration::from_secs(5))
        .await
        .context("after resize(30,80) fixture must see 80x30")?;

    expect_complete(terminal.send(b"exit\n".to_vec(), generation).await)?;
    let exited = terminal
        .wait_event(
            |e| matches!(e, Event::ProcessExited { .. }),
            Duration::from_secs(5),
        )
        .await
        .context("size-report never exited")?;
    check!(
        matches!(
            exited,
            Event::ProcessExited {
                code: Some(0),
                signal: None
            }
        ),
        "expected clean exit, got {exited:?}"
    );
    wait_finalized(ctx, &terminal, "resize").await?;
    ctx.record(
        "resize",
        json!({"initial": "120x30", "after_first": "100x40", "after_second": "80x30"}),
    );
    Ok(())
}

async fn scenario_trailing(ctx: &Ctx) -> Result<()> {
    let terminal = ctx
        .spawn_fixture(
            "trailing",
            "trailing",
            &[],
            EnvSpec::Snapshot(path_env()),
            false,
        )
        .await?;
    let exited = terminal
        .wait_event(
            |e| matches!(e, Event::ProcessExited { .. }),
            Duration::from_secs(5),
        )
        .await
        .context("root never exited")?;
    check!(
        matches!(
            exited,
            Event::ProcessExited {
                code: Some(0),
                signal: None
            }
        ),
        "expected clean exit 0, got {exited:?}"
    );
    let closed = terminal
        .wait_event(
            |e| matches!(e, Event::OutputClosed { .. }),
            Duration::from_secs(5),
        )
        .await
        .context("output never closed")?;
    check!(
        matches!(
            closed,
            Event::OutputClosed {
                end: OutputEnd::Eof
            }
        ),
        "natural close must be Eof, got {closed:?}"
    );
    let tail = terminal.output_tail(64);
    check!(
        tail.ends_with("TRAILING-ABC-无换行".as_bytes()),
        "no-newline tail must be preserved, got {:?}",
        String::from_utf8_lossy(&tail)
    );
    terminal.finalize().await?;
    ctx.record(
        "trailing",
        json!({"tail_preserved": true, "end": "eof", "exit_code": 0}),
    );
    ctx.dump_events("trailing", &terminal)
}

async fn scenario_child_holds(ctx: &Ctx) -> Result<()> {
    let terminal = ctx
        .spawn_fixture(
            "child-holds",
            "child-holds",
            &[],
            EnvSpec::Snapshot(path_env()),
            false,
        )
        .await?;
    terminal
        .wait_output_contains("PARKING", Duration::from_secs(5))
        .await
        .context("grandchild never parked")?;
    let exited = terminal
        .wait_event(
            |e| matches!(e, Event::ProcessExited { .. }),
            Duration::from_secs(5),
        )
        .await
        .context("root never exited")?;
    check!(
        matches!(
            exited,
            Event::ProcessExited {
                code: Some(0),
                signal: None
            }
        ),
        "expected clean exit, got {exited:?}"
    );
    check!(
        !terminal.is_output_closed(),
        "output must stay open while a child still holds the pts"
    );
    check!(
        terminal.quota_state() == quota::SlotState::Active,
        "quota must stay occupied while root exited but output open"
    );
    check!(
        terminal.cgroup_populated(),
        "holder must still live in the terminal cgroup"
    );

    let stats = terminal.stop(default_params()).await?;
    check!(!stats.forced, "TERM-responsive holder must stop gracefully");
    check!(
        terminal.quota_state() == quota::SlotState::Released,
        "quota must be released only after output closed and stop completed"
    );
    check_lifecycle_cardinality(&terminal, true)?;
    ctx.record(
        "child_holds",
        json!({"stop": stop_stats_value(&stats), "end": "forced"}),
    );
    ctx.dump_events("child-holds", &terminal)
}

async fn scenario_shell_jobs(ctx: &Ctx) -> Result<()> {
    let terminal = ctx
        .spawn_program(
            "shell-jobs",
            "/bin/sh",
            &["-m", "-c", "sleep 30 & sleep 30"],
        )
        .await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let members = proc::session_members(terminal.sid);
    check!(
        members.len() >= 3,
        "expected root + two sleeps, got {} members: {members:?}",
        members.len()
    );
    check!(
        members.iter().all(|m| m.session == terminal.sid),
        "all members must share the terminal session"
    );
    // Gate B: every member, in every job pgrp, stays inside the terminal cgroup.
    let cg_path = terminal.cgroup_path().context("no cgroup event")?;
    for member in &members {
        let member_cg = cgroup::proc_cgroup_path(member.pid)
            .with_context(|| format!("cgroup path of member {}", member.pid))?;
        check!(
            member_cg.starts_with(&cg_path),
            "member {} must stay in the terminal cgroup: {:?} !~ {:?}",
            member.pid,
            member_cg,
            cg_path
        );
    }
    let mut pgrps: Vec<i32> = members.iter().map(|m| m.pgrp).collect();
    pgrps.sort_unstable();
    pgrps.dedup();
    check!(
        pgrps.len() >= 2,
        "job control must give jobs their own process groups, got {pgrps:?}"
    );
    let fg = terminal.tcgetpgrp_now().context("tcgetpgrp")?;
    check!(fg > 0, "foreground pgrp must be positive, got {fg}");
    check!(
        pgrps.contains(&fg),
        "foreground pgrp {fg} must be one of {pgrps:?}"
    );

    let stats = terminal.stop(default_params()).await?;
    check!(!stats.forced, "sleep jobs must die on SIGTERM");
    check!(
        stats.signalled >= 3,
        "every job member must have been signalled, got {}",
        stats.signalled
    );
    let info = terminal.exit_info().context("exit info missing")?;
    check!(
        info.1 == Some(15),
        "root expected to die of SIGTERM, got {info:?}"
    );
    check_lifecycle_cardinality(&terminal, true)?;
    ctx.record(
        "shell_jobs",
        json!({"members": members.len(), "pgrps": pgrps, "fg_pgrp": fg, "stop": stop_stats_value(&stats)}),
    );
    ctx.dump_events("shell-jobs", &terminal)
}

async fn scenario_kill_escalation(ctx: &Ctx) -> Result<()> {
    let terminal = ctx
        .spawn_fixture(
            "kill-escalation",
            "park-noterm",
            &[],
            EnvSpec::Snapshot(path_env()),
            false,
        )
        .await?;
    terminal
        .wait_output_contains("PARKING", Duration::from_secs(5))
        .await
        .context("fixture never announced PARKING")?;
    let stats = terminal.stop(default_params()).await?;
    check!(
        stats.forced,
        "TERM-ignoring fixture must require cgroup.kill"
    );
    check!(
        stats.term_phase_ms >= 450,
        "bounded gentle wait must actually elapse (term_phase_ms={})",
        stats.term_phase_ms
    );
    check!(
        stats.total_ms < 5000,
        "stop must complete in bounded time, took {}ms",
        stats.total_ms
    );
    let info = terminal.exit_info().context("exit info missing")?;
    check!(
        info.1 == Some(9),
        "expected SIGKILL death via cgroup.kill, got {info:?}"
    );
    check_lifecycle_cardinality(&terminal, true)?;
    ctx.record("kill_escalation", stop_stats_value(&stats));
    ctx.dump_events("kill-escalation", &terminal)
}

async fn scenario_blocked_write_stop(ctx: &Ctx) -> Result<()> {
    let terminal = ctx
        .spawn_fixture(
            "blocked-write",
            "park-noterm",
            &[],
            EnvSpec::Snapshot(path_env()),
            true,
        )
        .await?;
    terminal
        .wait_output_contains("PARKING", Duration::from_secs(5))
        .await
        .context("fixture never announced PARKING")?;

    // Small sanity write completes fully first (normal path).
    let generation = terminal.generation_now();
    let small = expect_complete(terminal.send(b"ping".to_vec(), generation).await)?;
    check!(small == 4, "small write must complete, got {small}");

    // Blocked write: the fixture never reads, the tty input queue fills.
    let (progress, outcome_rx) = terminal.send_tracked(vec![b'A'; BLOCK_TOTAL], generation);
    let blocked_at = wait_blocked(&progress, BLOCK_TOTAL).await?;

    let t_stop = Instant::now();
    let abort_latency = Arc::new(Mutex::new(None::<Duration>));
    let abort_slot = abort_latency.clone();
    let outcome_fut = async move {
        let outcome = outcome_rx.await.unwrap_or(SendOutcome::Aborted {
            written: 0,
            reason: AbortReason::WriteFailed("outcome channel dropped".into()),
        });
        *abort_slot.lock().unwrap() = Some(t_stop.elapsed());
        outcome
    };
    // Stop runs concurrently with the blocked write: the barrier must abort
    // it without waiting for the write to finish.
    let (stats, outcome) = tokio::join!(terminal.stop(default_params()), outcome_fut);
    let stats = stats?;
    let abort_latency = abort_latency.lock().unwrap().unwrap_or_default();

    let written = match &outcome {
        SendOutcome::Aborted { written, reason } => {
            check!(
                matches!(reason, AbortReason::StopIntent),
                "abort reason must be StopIntent, got {reason:?}"
            );
            check!(
                *written >= blocked_at && *written < BLOCK_TOTAL,
                "partial write must satisfy blocked_at <= written < total, got written={written} blocked_at={blocked_at}"
            );
            *written
        }
        other => bail!("expected aborted write, got {other:?}"),
    };
    check!(
        abort_latency < Duration::from_millis(1500),
        "stop must abort the in-flight write promptly, took {abort_latency:?}"
    );
    check!(stats.forced, "park-noterm root requires the KILL phase");
    let rejected = terminal.send(b"after-stop".to_vec(), generation).await;
    check!(
        matches!(rejected, SendOutcome::RejectedAfterStop),
        "send after stop intent must be rejected, got {rejected:?}"
    );
    check_lifecycle_cardinality(&terminal, true)?;
    ctx.record(
        "blocked_write_stop",
        json!({
            "total": BLOCK_TOTAL,
            "written": written,
            "blocked_at": blocked_at,
            "abort_latency_ms": abort_latency.as_millis() as u64,
            "stop": stop_stats_value(&stats),
        }),
    );
    ctx.dump_events("blocked-write", &terminal)
}

async fn scenario_generation(ctx: &Ctx) -> Result<()> {
    let terminal = ctx
        .spawn_fixture(
            "generation",
            "park-noterm",
            &[],
            EnvSpec::Snapshot(path_env()),
            true,
        )
        .await?;
    terminal
        .wait_output_contains("PARKING", Duration::from_secs(5))
        .await
        .context("fixture never announced PARKING")?;

    let generation = terminal.generation_now();
    expect_complete(terminal.send(b"warm".to_vec(), generation).await)?;

    let (progress, outcome_rx) = terminal.send_tracked(vec![b'B'; BLOCK_TOTAL], generation);
    let blocked_at = wait_blocked(&progress, BLOCK_TOTAL).await?;

    let t0 = Instant::now();
    let new_generation = terminal.invalidate_generation();
    check!(
        new_generation == generation + 1,
        "generation must increment by one"
    );
    let outcome = outcome_rx.await.unwrap_or(SendOutcome::Aborted {
        written: 0,
        reason: AbortReason::WriteFailed("outcome channel dropped".into()),
    });
    let latency = t0.elapsed();
    let written = match &outcome {
        SendOutcome::Aborted { written, reason } => {
            check!(
                matches!(reason, AbortReason::ControlLost),
                "abort reason must be ControlLost, got {reason:?}"
            );
            check!(
                *written >= blocked_at && *written < BLOCK_TOTAL,
                "written must satisfy blocked_at <= written < total, got {written}"
            );
            *written
        }
        other => bail!("expected aborted write, got {other:?}"),
    };
    check!(
        !terminal.is_exited(),
        "generation invalidation must not kill the terminal"
    );
    check!(
        latency < Duration::from_millis(1500),
        "invalidation must abort the write promptly, took {latency:?}"
    );
    let stale = terminal.send(b"stale".to_vec(), generation).await;
    check!(
        matches!(stale, SendOutcome::RejectedStaleGeneration),
        "stale-generation send must be rejected, got {stale:?}"
    );
    // Ordered-log evidence: the partial old-generation bytes committed
    // BEFORE the SwitchCommit are legal; after the switch, no WriteCommit
    // below the current generation may exist (the abort above committed
    // none, and the reject never enters the log).
    let scan = terminal.commit_scan();
    check!(
        scan.stale_writes == 0,
        "commit log shows {} WriteCommits below the current generation after the SwitchCommit",
        scan.stale_writes
    );
    check!(
        scan.switches >= 1,
        "the invalidation must be a logged SwitchCommit"
    );
    check!(
        scan.total_bytes > 0,
        "the aborted write's partial commits must be logged WriteCommits"
    );
    let stats = terminal.stop(default_params()).await?;
    check_lifecycle_cardinality(&terminal, true)?;
    ctx.record(
        "generation",
        json!({
            "written": written,
            "blocked_at": blocked_at,
            "abort_latency_ms": latency.as_millis() as u64,
            "log_switches": scan.switches,
            "log_stale_writes": scan.stale_writes,
            "stop": stop_stats_value(&stats),
        }),
    );
    ctx.dump_events("generation", &terminal)
}

/// Continuous reader + high-frequency generation switching: every send is
/// accounted by its outcome; the fixture's tally file must equal the
/// accounted byte stream EXACTLY, and the coordinator's ordered commit log
/// — SwitchCommit(new_generation, seq) and WriteCommit(generation, seq,
/// bytes) under the same lock — must scan clean: after every switch, no
/// WriteCommit below the current generation. The scenario also produces a
/// DETERMINISTIC old-generation reject (a send claimed on the previous
/// generation once the final one is fixed) and finishes with a successful
/// write at the final generation, proving a post-switch generation still
/// commits. (A generation change observed BEFORE the commit is a legal
/// rejection and never enters the log.)
async fn scenario_generation_race(ctx: &Ctx) -> Result<()> {
    let total: usize = 1024 * 1024;
    let tally_path = ctx.workdir.join("gen-race-tally.bin");
    let _ = fs::remove_file(&tally_path);
    let terminal = ctx
        .spawn_fixture(
            "gen-race",
            "tally",
            &[&total.to_string(), &tally_path.to_string_lossy()],
            EnvSpec::Snapshot(path_env()),
            true,
        )
        .await?;

    // Bumper: 100 bumps every 5ms, concurrent with in-flight writes. Every
    // bump commits under the same coordinator lock as every write syscall.
    let handle = terminal.generation_handle();
    let bumper = tokio::spawn(async move {
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(5)).await;
            handle.bump();
        }
    });

    // Leave the last WRITE_CHUNK for the deterministic final-generation
    // write below (it must commit AFTER the last switch).
    const FINAL_BYTES: usize = WRITE_CHUNK;
    let loop_target = total - FINAL_BYTES;

    let mut accounted: usize = 0;
    let mut expected: Vec<u8> = Vec::with_capacity(total);
    let mut complete = 0usize;
    let mut aborted = 0usize;
    let mut rejected = 0usize;
    let mut send_idx: u8 = 1;
    while accounted < loop_target {
        let generation = terminal.generation_now();
        let remaining = loop_target - accounted;
        let size = remaining.min(WRITE_CHUNK);
        let payload = vec![send_idx; size];
        send_idx = (send_idx % 250) + 1;
        let (_progress, rx) = terminal.send_tracked(payload.clone(), generation);
        let outcome = rx.await.unwrap_or(SendOutcome::Aborted {
            written: 0,
            reason: AbortReason::WriteFailed("outcome channel dropped".into()),
        });
        match outcome {
            SendOutcome::Complete { written } => {
                check!(
                    written == payload.len(),
                    "complete write wrote {written} of {}",
                    payload.len()
                );
                expected.extend_from_slice(&payload);
                accounted += written;
                complete += 1;
            }
            SendOutcome::Aborted { written, reason } => {
                check!(
                    matches!(reason, AbortReason::ControlLost),
                    "race abort reason must be ControlLost, got {reason:?}"
                );
                expected.extend_from_slice(&payload[..written]);
                accounted += written;
                aborted += 1;
            }
            SendOutcome::RejectedStaleGeneration => {
                rejected += 1;
            }
            other => bail!("unexpected outcome in race: {other:?}"),
        }
    }
    bumper.await?;

    // The bumper is joined: the generation is final and stable now.
    let final_generation = terminal.generation_now();
    check!(
        final_generation >= 2,
        "the bumper must have committed at least one switch (final generation {final_generation})"
    );

    // DETERMINISTIC old-generation reject: with the final generation fixed,
    // a send claimed on the previous generation is refused at its commit
    // point and commits no byte.
    let stale = terminal.send(vec![b'S'], final_generation - 1).await;
    check!(
        matches!(stale, SendOutcome::RejectedStaleGeneration),
        "an old-generation send must be rejected, got {stale:?}"
    );

    // DETERMINISTIC final write: the final generation still commits after
    // the last switch.
    let final_payload = vec![251u8; FINAL_BYTES];
    let final_written =
        expect_complete(terminal.send(final_payload.clone(), final_generation).await)?;
    check!(
        final_written == FINAL_BYTES,
        "the final-generation write must fully commit, got {final_written}"
    );
    expected.extend_from_slice(&final_payload);

    let expected_sum: u64 = expected
        .iter()
        .fold(0u64, |acc, b| acc.wrapping_add(*b as u64));
    let confirmation = format!("READ {total} CHECKSUM {expected_sum}");
    let output = terminal
        .wait_output_contains(&confirmation, Duration::from_secs(15))
        .await
        .context("tally fixture never confirmed the accounted stream")?;
    check!(
        String::from_utf8_lossy(&output).contains(&confirmation),
        "fixture checksum mismatch: expected {confirmation}"
    );
    let file_bytes = fs::read(&tally_path).context("read tally file")?;
    check!(
        file_bytes == expected,
        "tally stream differs from the accounted byte stream ({} vs {} bytes): bytes were committed outside the accounted order",
        file_bytes.len(),
        expected.len()
    );
    // Ordered-log evidence (never just the last write's generation): scan
    // the coordinator's SwitchCommit/WriteCommit log in commit order — after
    // every switch no WriteCommit below the current generation may exist,
    // and the final generation must have committed after the last switch.
    let scan = terminal.commit_scan();
    check!(
        scan.stale_writes == 0,
        "commit log shows {} WriteCommits below the current generation after a SwitchCommit",
        scan.stale_writes
    );
    check!(
        scan.switches >= 1,
        "the bumper must have logged SwitchCommits"
    );
    check!(
        scan.total_bytes as usize == total,
        "total committed {} must equal {}",
        scan.total_bytes,
        total
    );
    check!(
        scan.last_write_after_last_switch == Some(final_generation),
        "the last post-switch WriteCommit must carry the final generation {final_generation}, got {:?}",
        scan.last_write_after_last_switch
    );
    check!(
        scan.last_write_generation == Some(final_generation),
        "the newest WriteCommit must be at the final generation, got {:?}",
        scan.last_write_generation
    );
    wait_finalized(ctx, &terminal, "gen-race").await?;
    ctx.record(
        "generation_race",
        json!({
            "total_bytes": total,
            "sends_complete": complete,
            "sends_aborted_control_lost": aborted,
            "sends_rejected_stale": rejected,
            "deterministic_stale_reject": true,
            "bumps": 100,
            "log_switches": scan.switches,
            "log_writes": scan.writes,
            "log_stale_writes": scan.stale_writes,
            "final_write_generation": final_generation,
            "last_write_after_last_switch": scan.last_write_after_last_switch,
            "stream_matches_accounting": true,
        }),
    );
    ctx.dump_events("gen-race", &terminal)
}

/// A blocked write against a non-reading terminal must abort at its own
/// independent deadline with the partial byte count reported.
async fn scenario_write_deadline(ctx: &Ctx) -> Result<()> {
    let terminal = ctx
        .spawn_fixture(
            "write-deadline",
            "park-noterm",
            &[],
            EnvSpec::Snapshot(path_env()),
            true,
        )
        .await?;
    terminal
        .wait_output_contains("PARKING", Duration::from_secs(5))
        .await
        .context("fixture never announced PARKING")?;
    let generation = terminal.generation_now();
    let (_progress, rx) = terminal.send_tracked_deadline(
        vec![b'D'; BLOCK_TOTAL],
        generation,
        Duration::from_millis(300),
    );
    let t0 = Instant::now();
    let outcome = rx.await.unwrap_or(SendOutcome::Aborted {
        written: 0,
        reason: AbortReason::WriteFailed("outcome channel dropped".into()),
    });
    let latency = t0.elapsed();
    let written = match &outcome {
        SendOutcome::Aborted { written, reason } => {
            check!(
                matches!(reason, AbortReason::WriteDeadline),
                "abort reason must be WriteDeadline, got {reason:?}"
            );
            check!(
                *written > 0 && *written < BLOCK_TOTAL,
                "deadline must report a partial write, got {written}"
            );
            *written
        }
        other => bail!("expected deadline abort, got {other:?}"),
    };
    check!(
        latency >= Duration::from_millis(280) && latency < Duration::from_millis(2000),
        "deadline must fire at ~300ms, took {latency:?}"
    );
    let stats = terminal.stop(default_params()).await?;
    check_lifecycle_cardinality(&terminal, true)?;
    ctx.record(
        "write_deadline",
        json!({
            "written": written,
            "total": BLOCK_TOTAL,
            "deadline_ms": 300,
            "latency_ms": latency.as_millis() as u64,
            "stop": stop_stats_value(&stats),
        }),
    );
    ctx.dump_events("write-deadline", &terminal)
}

/// Service shutdown cancels a blocked in-flight write; stop still works after.
async fn scenario_service_shutdown(ctx: &Ctx) -> Result<()> {
    let terminal = ctx
        .spawn_fixture(
            "svc-shutdown",
            "park-noterm",
            &[],
            EnvSpec::Snapshot(path_env()),
            true,
        )
        .await?;
    terminal
        .wait_output_contains("PARKING", Duration::from_secs(5))
        .await
        .context("fixture never announced PARKING")?;
    let generation = terminal.generation_now();
    let (progress, rx) = terminal.send_tracked(vec![b'S'; BLOCK_TOTAL], generation);
    let blocked_at = wait_blocked(&progress, BLOCK_TOTAL).await?;

    let t0 = Instant::now();
    ctx.terminals.signal_shutdown();
    let outcome = rx.await.unwrap_or(SendOutcome::Aborted {
        written: 0,
        reason: AbortReason::WriteFailed("outcome channel dropped".into()),
    });
    let latency = t0.elapsed();
    let written = match &outcome {
        SendOutcome::Aborted { written, reason } => {
            check!(
                matches!(reason, AbortReason::ServiceShutdown),
                "abort reason must be ServiceShutdown, got {reason:?}"
            );
            check!(
                *written >= blocked_at && *written < BLOCK_TOTAL,
                "partial write must satisfy blocked_at <= written < total, got written={written} blocked_at={blocked_at}"
            );
            *written
        }
        other => bail!("expected service-shutdown abort, got {other:?}"),
    };
    check!(
        latency < Duration::from_millis(1500),
        "shutdown must abort the write promptly, took {latency:?}"
    );
    // Stop must still complete the lifecycle after a service shutdown.
    let stats = terminal.stop(default_params()).await?;
    check_lifecycle_cardinality(&terminal, true)?;
    ctx.record(
        "service_shutdown",
        json!({
            "written": written,
            "abort_latency_ms": latency.as_millis() as u64,
            "stop": stop_stats_value(&stats),
        }),
    );
    ctx.dump_events("svc-shutdown", &terminal)
}

/// Bounded input messages: a send above MAX_SEND_BYTES (candidate 256 KiB)
/// is rejected with a typed outcome BEFORE enqueueing (no byte committed),
/// while the exact boundary is still accepted and blocks as usual; the
/// accepted queued+inflight bytes stay bounded by
/// (WRITE_QUEUE_CAPACITY + 1) * MAX_SEND_BYTES = 768 KiB.
async fn scenario_oversize(ctx: &Ctx) -> Result<()> {
    let terminal = ctx
        .spawn_fixture(
            "oversize",
            "park-noterm",
            &[],
            EnvSpec::Snapshot(path_env()),
            true,
        )
        .await?;
    terminal
        .wait_output_contains("PARKING", Duration::from_secs(5))
        .await
        .context("fixture never announced PARKING")?;
    let generation = terminal.generation_now();
    let committed_before = terminal.total_committed();

    // 256 KiB + 1: typed rejection before enqueueing.
    let (progress, rx) = terminal.send_tracked(vec![b'O'; pty::MAX_SEND_BYTES + 1], generation);
    let outcome = rx.await.unwrap_or(SendOutcome::Aborted {
        written: 0,
        reason: AbortReason::WriteFailed("outcome channel dropped".into()),
    });
    check!(
        matches!(outcome, SendOutcome::RejectedOversize),
        "a {}-byte send must be rejected as oversize, got {outcome:?}",
        pty::MAX_SEND_BYTES + 1
    );
    check!(
        progress.load(Ordering::SeqCst) == 0,
        "an oversize send must not commit any byte"
    );
    check!(
        terminal.total_committed() == committed_before,
        "an oversize send must not enqueue or commit any byte"
    );
    check!(
        terminal.count_events(|e| matches!(e, Event::SendRejectedOversize)) == 1,
        "exactly one SendRejectedOversize expected"
    );

    // The exact boundary stays legal: the 256 KiB send blocks in-flight.
    let (p1, r1) = terminal.send_tracked(vec![b'O'; pty::MAX_SEND_BYTES], generation);
    let blocked_at = wait_blocked(&p1, pty::MAX_SEND_BYTES).await?;
    check!(
        blocked_at > 0 && blocked_at < pty::MAX_SEND_BYTES,
        "the boundary send must block partially, got {blocked_at}"
    );

    // Queue accounting is unchanged: two queued + one in-flight fill it; the
    // next send is RejectedQueueFull, and an oversize send is still rejected
    // as oversize (size validation precedes queue accounting).
    let (_p2, r2) = terminal.send_tracked(vec![b'P'; pty::MAX_SEND_BYTES], generation);
    let (_p3, r3) = terminal.send_tracked(vec![b'P'; pty::MAX_SEND_BYTES], generation);
    let (_p4, r4) = terminal.send_tracked(vec![b'P'; 1], generation);
    let full = r4.await.unwrap_or(SendOutcome::RejectedQueueFull);
    check!(
        matches!(full, SendOutcome::RejectedQueueFull),
        "a third queued send must be rejected as queue-full, got {full:?}"
    );
    let (_p5, r5) = terminal.send_tracked(vec![b'X'; pty::MAX_SEND_BYTES + 1], generation);
    let oversize = r5.await.unwrap_or(SendOutcome::RejectedOversize);
    check!(
        matches!(oversize, SendOutcome::RejectedOversize),
        "an oversize send with a full queue must still be rejected as oversize, got {oversize:?}"
    );

    let stats = terminal.stop(default_params()).await?;
    let outcome1 = r1.await.unwrap_or(SendOutcome::Aborted {
        written: 0,
        reason: AbortReason::WriteFailed("r1 dropped".into()),
    });
    check!(
        matches!(outcome1, SendOutcome::Aborted { ref reason, .. } if matches!(reason, AbortReason::StopIntent)),
        "in-flight boundary send must abort at the stop barrier, got {outcome1:?}"
    );
    for (label, rx) in [("queued-2", r2), ("queued-3", r3)] {
        let outcome = rx.await.unwrap_or(SendOutcome::RejectedAfterStop);
        check!(
            matches!(outcome, SendOutcome::RejectedAfterStop),
            "{label} must be rejected after stop, got {outcome:?}"
        );
    }
    check_lifecycle_cardinality(&terminal, true)?;
    ctx.record(
        "oversize",
        json!({
            "max_send_bytes": pty::MAX_SEND_BYTES,
            "queue_capacity": pty::WRITE_QUEUE_CAPACITY,
            "accepted_queued_inflight_bound_bytes": (pty::WRITE_QUEUE_CAPACITY + 1) * pty::MAX_SEND_BYTES,
            "rejected_bytes": pty::MAX_SEND_BYTES + 1,
            "boundary_send": "accepted, blocked as usual",
            "stop": stop_stats_value(&stats),
        }),
    );
    ctx.dump_events("oversize", &terminal)
}

/// The send queue is bounded: with the writer blocked, the queued commands
/// fill the capacity and further sends are rejected without side effects.
async fn scenario_queue_full(ctx: &Ctx) -> Result<()> {
    let terminal = ctx
        .spawn_fixture(
            "queue-full",
            "park-noterm",
            &[],
            EnvSpec::Snapshot(path_env()),
            true,
        )
        .await?;
    terminal
        .wait_output_contains("PARKING", Duration::from_secs(5))
        .await
        .context("fixture never announced PARKING")?;
    let generation = terminal.generation_now();

    let (_p1, r1) = terminal.send_tracked(vec![b'Q'; BLOCK_TOTAL], generation); // in-flight, blocked
    let (_p2, r2) = terminal.send_tracked(vec![b'Q'; BLOCK_TOTAL], generation); // queued
    let (_p3, r3) = terminal.send_tracked(vec![b'Q'; BLOCK_TOTAL], generation); // queued
    let (_p4, r4) = terminal.send_tracked(vec![b'Q'; BLOCK_TOTAL], generation); // queue full
    let rejected = r4.await.unwrap_or(SendOutcome::RejectedQueueFull);
    check!(
        matches!(rejected, SendOutcome::RejectedQueueFull),
        "fourth send must be rejected for a full queue, got {rejected:?}"
    );

    let stats = terminal.stop(default_params()).await?;
    let outcome1 = r1.await.unwrap_or(SendOutcome::Aborted {
        written: 0,
        reason: AbortReason::WriteFailed("r1 dropped".into()),
    });
    check!(
        matches!(outcome1, SendOutcome::Aborted { ref reason, .. } if matches!(reason, AbortReason::StopIntent)),
        "in-flight send must abort at the stop barrier, got {outcome1:?}"
    );
    for (label, rx) in [("queued-2", r2), ("queued-3", r3)] {
        let outcome = rx.await.unwrap_or(SendOutcome::RejectedAfterStop);
        check!(
            matches!(outcome, SendOutcome::RejectedAfterStop),
            "{label} must be rejected after stop, got {outcome:?}"
        );
    }
    check_lifecycle_cardinality(&terminal, true)?;
    ctx.record(
        "queue_full",
        json!({
            "queue_capacity": pty::WRITE_QUEUE_CAPACITY,
            "rejected": 1,
            "stop": stop_stats_value(&stats),
        }),
    );
    ctx.dump_events("queue-full", &terminal)
}

/// Concurrent Stop x2 share one intent, one completion, and one quota
/// release; a stop after completion returns the shared result unchanged.
async fn scenario_stop_idempotent(ctx: &Ctx) -> Result<()> {
    let terminal = ctx
        .spawn_fixture(
            "stop-idem",
            "park",
            &[],
            EnvSpec::Snapshot(path_env()),
            false,
        )
        .await?;
    terminal
        .wait_output_contains("PARKING", Duration::from_secs(5))
        .await
        .context("fixture never announced PARKING")?;

    let (s1, s2) = tokio::join!(
        terminal.stop(default_params()),
        terminal.stop(default_params())
    );
    let s1 = s1?;
    let s2 = s2?;
    check!(
        s1 == s2,
        "concurrent stops must share one result: {s1:?} vs {s2:?}"
    );
    check_lifecycle_cardinality(&terminal, true)?;

    let counts_before = (
        terminal.count_events(|e| matches!(e, Event::StopIntentCommitted)),
        terminal.count_events(|e| matches!(e, Event::StopCompleted { .. })),
    );
    let s3 = terminal.stop(default_params()).await?;
    check!(
        s3 == s1,
        "post-completion stop must return the shared result"
    );
    check!(
        (
            terminal.count_events(|e| matches!(e, Event::StopIntentCommitted)),
            terminal.count_events(|e| matches!(e, Event::StopCompleted { .. }))
        ) == counts_before,
        "post-completion stop must not emit new stop events"
    );
    ctx.record(
        "stop_idempotent",
        json!({"shared_result": true, "intent_count": 1, "completion_count": 1, "stop": stop_stats_value(&s1)}),
    );
    ctx.dump_events("stop-idem", &terminal)
}

/// Caller cancellation of Stop: the first stop caller is aborted right
/// after the unique StopIntentCommitted is observed; the DETACHED stop
/// completion still runs to the end, and later callers — each under an
/// explicit timeout — receive the SAME shared result. Variant (a) covers
/// the success path (including the concurrent-stop fault coverage of the
/// old stop-fault scenario: shared bounded failure, one intent, slot never
/// released) and variant (b) the controlled failure path with a cancelled
/// leader.
async fn scenario_stop_cancellation(ctx: &Ctx) -> Result<()> {
    // (a) Success: leader aborted mid-stop, detached stop completes anyway.
    let terminal = ctx
        .spawn_fixture(
            "stop-cancel",
            "park-noterm",
            &[],
            EnvSpec::Snapshot(path_env()),
            false,
        )
        .await?;
    terminal
        .wait_output_contains("PARKING", Duration::from_secs(5))
        .await
        .context("fixture never announced PARKING")?;
    let leader_terminal = terminal.clone();
    let leader = tokio::spawn(async move { leader_terminal.stop(default_params()).await });
    terminal
        .wait_event(
            |e| matches!(e, Event::StopIntentCommitted),
            Duration::from_secs(5),
        )
        .await
        .context("stop intent never committed")?;
    check!(
        terminal.count_events(|e| matches!(e, Event::StopIntentCommitted)) == 1,
        "exactly one StopIntentCommitted expected"
    );
    // Cancel the leader caller: the committed stop sequence is detached.
    leader.abort();
    let join = leader.await;
    check!(
        matches!(&join, Err(e) if e.is_cancelled()),
        "the first stop caller must have been aborted, got {join:?}"
    );
    // Second and third callers, each under an explicit timeout: the detached
    // completion publishes the success, so they return the SAME result —
    // cancellation of the leader can neither undo nor hang the stop.
    let (s2, s3) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(
            terminal.stop(default_params()),
            terminal.stop(default_params())
        )
    })
    .await
    .map_err(|_| anyhow!("post-cancellation stops never finished"))?;
    let s2 = s2?;
    let s3 = s3?;
    check!(
        s2 == s3 && !s2.existing_state,
        "post-cancellation stops must share the committed result: {s2:?} vs {s3:?}"
    );
    check!(s2.forced, "park-noterm requires the KILL phase");
    check_lifecycle_cardinality(&terminal, true)?;
    check!(
        terminal.quota_state() == quota::SlotState::Released,
        "the detached stop must release the quota"
    );
    ctx.record(
        "stop_cancellation",
        json!({
            "leader": "aborted after StopIntentCommitted",
            "detached_completion": true,
            "shared_result": true,
            "stop": stop_stats_value(&s2),
        }),
    );
    ctx.dump_events("stop-cancel", &terminal)?;

    // (b) Controlled failure: same cancellation, but the detached stop
    // fails (empty_wait = 0 against a TERM-immune root); the later callers
    // get the SAME shared bounded failure, never a hang, and the slot is
    // never Released.
    let terminal = ctx
        .spawn_fixture(
            "stop-cancel-fault",
            "park-noterm",
            &[],
            EnvSpec::Snapshot(path_env()),
            false,
        )
        .await?;
    terminal
        .wait_output_contains("PARKING", Duration::from_secs(5))
        .await
        .context("fixture never announced PARKING")?;
    let faulty = StopParams {
        empty_wait: Duration::ZERO,
        ..default_params()
    };
    let leader_terminal = terminal.clone();
    let leader = tokio::spawn(async move { leader_terminal.stop(faulty).await });
    terminal
        .wait_event(
            |e| matches!(e, Event::StopIntentCommitted),
            Duration::from_secs(5),
        )
        .await
        .context("stop intent never committed")?;
    leader.abort();
    let join = leader.await;
    check!(
        matches!(&join, Err(e) if e.is_cancelled()),
        "the first faulty stop caller must have been aborted, got {join:?}"
    );
    let t0 = Instant::now();
    let (m2, m3) = tokio::time::timeout(Duration::from_secs(30), async {
        let a = failure_message(terminal.stop(faulty).await);
        let b = failure_message(terminal.stop(faulty).await);
        (a, b)
    })
    .await
    .map_err(|_| anyhow!("post-cancellation faulty stops never finished (waiter hang)"))?;
    let bounded = t0.elapsed();
    check!(
        m2.is_some() && m2 == m3,
        "post-cancellation stops must share the same bounded failure: {m2:?} vs {m3:?}"
    );
    check!(
        bounded < Duration::from_secs(10),
        "failed stops must return bounded, took {bounded:?}"
    );
    check!(
        terminal.count_events(|e| matches!(e, Event::StopIntentCommitted)) == 1,
        "exactly one StopIntentCommitted expected"
    );
    check!(
        terminal.count_events(|e| matches!(e, Event::StopCompleted { .. })) == 0,
        "a failed stop must not emit StopCompleted"
    );
    check!(
        terminal.quota_state() == quota::SlotState::Active,
        "a failed stop must never release the slot, got {:?}",
        terminal.quota_state()
    );
    // The kill phase already ran: the root is reaped by the detached monitor
    // and the cgroup empties (no live-process leak), but it is never removed
    // (cleanup did not complete) — the probe-root sweep reclaims it at end.
    let cg_path = terminal.cgroup_path().context("no cgroup event")?;
    let cg_dir = PathBuf::from(&cg_path);
    let empty_deadline = Instant::now() + Duration::from_secs(5);
    while cgroup::cg_populated(&cg_dir) {
        check!(
            Instant::now() < empty_deadline,
            "killed root must leave the terminal cgroup empty (no process leak)"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    check!(
        cg_dir.is_dir(),
        "the failed stop must leave the terminal cgroup for the probe-root sweep"
    );
    ctx.record(
        "stop_cancellation_fault",
        json!({
            "leader": "aborted after StopIntentCommitted",
            "shared_failure": true,
            "failure": m2,
            "bounded_ms": bounded.as_millis() as u64,
            "intent_count": 1,
            "completion_count": 0,
            "slot": "Active",
            "cgroup": "kept for probe-root sweep",
        }),
    );
    ctx.dump_events("stop-cancel-fault", &terminal)
}

/// EOF vs force-close race: whichever side commits the single OutputClosed,
/// there is exactly one event. (a) natural close before stop → Eof;
/// (b) close after stop with a tiny output wait → exactly one event.
async fn scenario_close_race(ctx: &Ctx) -> Result<()> {
    // (a) Natural EOF wins: the single-process fixture exits, the output
    // closes on its own, and the later stop is an existing-state call.
    {
        let terminal = ctx
            .spawn_fixture(
                "close-race-a",
                "trailing",
                &[],
                EnvSpec::Snapshot(path_env()),
                false,
            )
            .await?;
        terminal
            .wait_event(
                |e| matches!(e, Event::ProcessExited { .. }),
                Duration::from_secs(5),
            )
            .await
            .context("root never exited")?;
        terminal
            .wait_event(
                |e| matches!(e, Event::OutputClosed { .. }),
                Duration::from_secs(5),
            )
            .await
            .context("output never closed")?;
        let closed = terminal
            .events()
            .iter()
            .rev()
            .find(|(_, e)| matches!(e, Event::OutputClosed { .. }))
            .map(|(_, e)| e.clone())
            .context("no OutputClosed")?;
        check!(
            matches!(
                closed,
                Event::OutputClosed {
                    end: OutputEnd::Eof
                }
            ),
            "pre-stop close must be Eof, got {closed:?}"
        );
        let stats = terminal.stop(default_params()).await?;
        check!(
            stats.existing_state,
            "stop after a natural completion must be an existing-state call"
        );
        check_lifecycle_cardinality(&terminal, false)?;
        ctx.record("close_race_a", json!({"end": "eof", "events": 1}));
        ctx.dump_events("close-race-a", &terminal)?;
    }
    // (b) Root exits, a TERM-immune child holds the pts; stop's tiny output
    // wait races the post-kill EIO — exactly one OutputClosed either way,
    // and the child is still reclaimed by cgroup.kill.
    {
        let terminal = ctx
            .spawn_fixture(
                "close-race-b",
                "child-holds-noterm",
                &[],
                EnvSpec::Snapshot(path_env()),
                false,
            )
            .await?;
        terminal
            .wait_output_contains("PARKING", Duration::from_secs(5))
            .await
            .context("TERM-immune holder never parked")?;
        terminal
            .wait_event(
                |e| matches!(e, Event::ProcessExited { .. }),
                Duration::from_secs(5),
            )
            .await
            .context("root never exited")?;
        let params = StopParams {
            output_wait: Duration::from_millis(20),
            ..default_params()
        };
        let stats = terminal.stop(params).await?;
        check!(stats.forced, "TERM-immune holder must require cgroup.kill");
        check!(
            terminal.count_events(|e| matches!(e, Event::OutputClosed { .. })) == 1,
            "exactly one OutputClosed regardless of which side won the race"
        );
        check_lifecycle_cardinality(&terminal, true)?;
        ctx.record(
            "close_race_b",
            json!({"forced": true, "output_events": 1, "stop": stop_stats_value(&stats)}),
        );
        ctx.dump_events("close-race-b", &terminal)?;
    }
    Ok(())
}

/// A try_wait error is a separate fault: no fabricated ProcessExited, the
/// exited flag stays false, and a later real exit still produces exactly one
/// ProcessExited event.
async fn scenario_monitor_fault(ctx: &Ctx) -> Result<()> {
    let terminal = ctx
        .spawn_fixture_fault(
            "monitor-fault",
            "park",
            &[],
            EnvSpec::Snapshot(path_env()),
            false,
            Some(FaultStep::MonitorPoll(2)),
        )
        .await?;
    let _fault = terminal
        .wait_event(
            |e| matches!(e, Event::MonitorFault { .. }),
            Duration::from_secs(5),
        )
        .await
        .context("injected monitor fault never surfaced")?;
    check!(
        !terminal.is_exited(),
        "a try_wait error must NOT set the exited flag"
    );
    check!(
        terminal.count_events(|e| matches!(e, Event::ProcessExited { .. })) == 0,
        "a try_wait error must NOT fabricate ProcessExited"
    );
    let stats = terminal.stop(default_params()).await?;
    check!(terminal.is_exited(), "a real exit must still be reaped");
    check!(
        terminal.count_events(|e| matches!(e, Event::MonitorFault { .. })) == 1,
        "exactly one MonitorFault expected"
    );
    check_lifecycle_cardinality(&terminal, true)?;
    ctx.record(
        "monitor_fault",
        json!({"fabricated_exit": false, "fault_events": 1, "stop": stop_stats_value(&stats)}),
    );
    ctx.dump_events("monitor-fault", &terminal)
}

/// State-aware startup rollback at every fault point: quota, cgroups, and
/// processes all come back clean.
async fn scenario_start_faults(ctx: &Ctx) -> Result<()> {
    let steps = [
        FaultStep::BeforePty,
        FaultStep::AfterPtyOpen,
        FaultStep::AfterSpawn,
        FaultStep::AfterProcStat,
        FaultStep::AfterTaskStart,
    ];
    let mut cleaned = Vec::new();
    for step in steps {
        let occupying = ctx.terminals.quota().occupying();
        let names_before = ctx.terminals.terminal_cgroup_names();
        let err = expect_spawn_err(
            ctx.spawn_fixture_raw(
                "start-faults",
                "park",
                &[],
                EnvSpec::Snapshot(path_env()),
                Some(step),
            )
            .await,
            "injected start fault",
        )?;
        check!(
            matches!(&err, SpawnError::InjectedFault(name) if *name == step.name()),
            "expected InjectedFault({}), got {err:?}",
            step.name()
        );
        check!(
            ctx.terminals.quota().occupying() == occupying,
            "fault {} must not leak quota",
            step.name()
        );
        check!(
            ctx.terminals.terminal_cgroup_names() == names_before,
            "fault {} must remove its terminal cgroup: {:?} vs {:?}",
            step.name(),
            ctx.terminals.terminal_cgroup_names(),
            names_before
        );
        cleaned.push(step.name());
    }
    ctx.record("start_faults", json!({"rolled_back": cleaned}));
    Ok(())
}

/// Injected rollback cleanup failure (FaultStep::RollbackNoKill: the
/// rollback skips cgroup.kill): the slot must stay in Cleaning — never
/// Released — the terminal cgroup is kept populated (the fixture is only
/// reclaimed by the probe-root sweep at test end), and nothing is silently
/// reclaimed. Successful rollbacks (scenario_start_faults) must verify
/// cgroup empty + tasks joined + cgroup removed before releasing.
/// Injected rollback cleanup failures: (1) FaultStep::RollbackNoKill skips
/// cgroup.kill, so the empty-verification fails on a still-populated
/// cgroup; (2)/(3) FaultStep::RollbackReapError / RollbackReapTimeout fail
/// the root-reap verification (a synthetic try_wait error / a deadline
/// expiry). All three must count as cleanup failures: the slot stays in
/// Cleaning — never Released — and nothing is silently reclaimed.
async fn scenario_rollback_cleanup_failure(ctx: &Ctx) -> Result<()> {
    let names_before = ctx.terminals.terminal_cgroup_names();
    let occupying = ctx.terminals.quota().occupying();
    let err = expect_spawn_err(
        ctx.spawn_fixture_raw(
            "rollback-cleanup",
            "park",
            &[],
            EnvSpec::Snapshot(path_env()),
            Some(FaultStep::RollbackNoKill),
        )
        .await,
        "injected rollback cleanup failure",
    )?;
    check!(
        matches!(&err, SpawnError::CleanupFailed { fault, detail }
            if fault.contains("RollbackNoKill") && detail.contains("not empty")),
        "expected CleanupFailed with a not-empty verification, got {err:?}"
    );
    check!(
        ctx.terminals.quota().occupying() == occupying + 1,
        "a failed rollback cleanup must keep its slot occupied (Cleaning), got {}",
        ctx.terminals.quota().occupying()
    );
    let names_after = ctx.terminals.terminal_cgroup_names();
    check!(
        names_after.len() == names_before.len() + 1,
        "the failed-cleanup terminal cgroup must still exist: {names_after:?}"
    );
    // The kept cgroup must still hold the live fixture: nothing but the
    // probe-root sweep may reclaim it. The spawn path waits for the
    // fixture's PARKING line (printed only after its SIGHUP-ignore handlers
    // are installed) before executing RollbackNoKill, so the rollback's
    // master-fd close cannot SIGHUP the fixture away nondeterministically —
    // the cgroup provably stays populated here.
    let new_name = names_after
        .iter()
        .find(|n| !names_before.contains(n))
        .context("no new terminal cgroup after the failed cleanup")?;
    let marker = fs::read_to_string(ctx.workdir.join("cgroup-root.json"))
        .context("read cgroup-root.json")?;
    let root_path = serde_json::from_str::<serde_json::Value>(&marker)?
        .get("path")
        .and_then(|v| v.as_str())
        .context("cgroup-root.json has no path")?
        .to_string();
    let cg_dir = Path::new(&root_path).join(new_name);
    check!(
        cg_dir.is_dir(),
        "the failed-cleanup terminal cgroup must exist: {}",
        cg_dir.display()
    );
    let populated_deadline = Instant::now() + Duration::from_secs(2);
    while !cgroup::cg_populated(&cg_dir) {
        check!(
            Instant::now() < populated_deadline,
            "the failed-cleanup terminal cgroup must stay populated ({}): nothing but the probe-root sweep may reclaim it",
            cg_dir.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let no_kill_name = new_name.clone();

    // (2)/(3) Root-reap verification failures: the kill itself succeeded and
    // the emptied cgroup is removed, but the slot STILL stays in Cleaning —
    // a reap try_wait error and a reap deadline expiry are cleanup failures
    // and must never reach cleanup_ok/release.
    let mut reap_faults = Vec::new();
    for (step, needle) in [
        (FaultStep::RollbackReapError, "root reap try_wait error"),
        (FaultStep::RollbackReapTimeout, "root reap timed out"),
    ] {
        let occupying = ctx.terminals.quota().occupying();
        let names_before = ctx.terminals.terminal_cgroup_names();
        let err = expect_spawn_err(
            ctx.spawn_fixture_raw(
                "rollback-reap",
                "park",
                &[],
                EnvSpec::Snapshot(path_env()),
                Some(step),
            )
            .await,
            "injected rollback root-reap fault",
        )?;
        check!(
            matches!(&err, SpawnError::CleanupFailed { fault, detail }
                if fault.contains(step.name()) && detail.contains(needle)),
            "expected CleanupFailed({}) mentioning {needle:?}, got {err:?}",
            step.name()
        );
        check!(
            ctx.terminals.quota().occupying() == occupying + 1,
            "a root-reap cleanup failure must keep its slot occupied (Cleaning), got {}",
            ctx.terminals.quota().occupying()
        );
        check!(
            ctx.terminals.terminal_cgroup_names() == names_before,
            "an emptied cgroup is removed even when the reap verification fails"
        );
        reap_faults.push(format!("{}:Cleaning", step.name()));
    }
    ctx.record(
        "rollback_cleanup_failure",
        json!({
            "slot": "Cleaning",
            "released": false,
            "cgroup_kept": no_kill_name,
            "cgroup_populated": true,
            "reap_faults": reap_faults,
            "cleanup": "probe-root sweep at test end",
        }),
    );
    Ok(())
}

async fn scenario_quota(ctx: &Ctx) -> Result<()> {
    // Bounded-wait instrumentation: every await gets a 30s ceiling and a step
    // log so a hang pinpoints its exact wait point.
    const STEP_TIMEOUT: Duration = Duration::from_secs(30);
    let step = |n: u8, what: &str| {
        println!(
            "  quota-step {n} {what} occupying={}",
            ctx.terminals.quota().occupying()
        );
    };

    step(1, "spawn t1");
    let t1 = tokio::time::timeout(
        STEP_TIMEOUT,
        ctx.spawn_fixture("q-s1", "park", &[], EnvSpec::Snapshot(path_env()), false),
    )
    .await
    .map_err(|_| anyhow!("quota hang: spawn t1 exceeded 30s"))??;
    step(2, "spawn t2");
    let t2 = tokio::time::timeout(
        STEP_TIMEOUT,
        ctx.spawn_fixture("q-s1", "park", &[], EnvSpec::Snapshot(path_env()), false),
    )
    .await
    .map_err(|_| anyhow!("quota hang: spawn t2 exceeded 30s"))??;
    step(3, "spawn t3");
    let t3 = tokio::time::timeout(
        STEP_TIMEOUT,
        ctx.spawn_fixture("q-s2", "park", &[], EnvSpec::Snapshot(path_env()), false),
    )
    .await
    .map_err(|_| anyhow!("quota hang: spawn t3 exceeded 30s"))??;
    step(4, "spawn t3b");
    let t3b = tokio::time::timeout(
        STEP_TIMEOUT,
        ctx.spawn_fixture("q-s3", "park", &[], EnvSpec::Snapshot(path_env()), false),
    )
    .await
    .map_err(|_| anyhow!("quota hang: spawn t3b exceeded 30s"))??;
    check!(
        ctx.terminals.quota().occupying() == GLOBAL_LIMIT,
        "occupancy must equal the global limit"
    );

    step(5, "session-limit rejection");
    let err = expect_spawn_err(
        ctx.spawn_fixture_raw("q-s1", "park", &[], EnvSpec::Snapshot(path_env()), None)
            .await,
        "session limit",
    )?;
    check!(
        matches!(err, SpawnError::Quota(quota::QuotaError::SessionExhausted)),
        "expected SessionExhausted, got {err:?}"
    );
    step(6, "global-limit rejection");
    let err = expect_spawn_err(
        ctx.spawn_fixture_raw("q-s4", "park", &[], EnvSpec::Snapshot(path_env()), None)
            .await,
        "global limit",
    )?;
    check!(
        matches!(err, SpawnError::Quota(quota::QuotaError::GlobalExhausted)),
        "expected GlobalExhausted, got {err:?}"
    );

    step(7, "stop t1");
    tokio::time::timeout(STEP_TIMEOUT, t1.stop(default_params()))
        .await
        .map_err(|_| anyhow!("quota hang: stop(t1) exceeded 30s"))??;
    check!(
        ctx.terminals.quota().occupying() == GLOBAL_LIMIT - 1,
        "released slot must free occupancy"
    );

    // Failed start (nonexistent program) frees its reservation atomically.
    step(8, "failed start rollback");
    let before = ctx.terminals.quota().occupying();
    let err = expect_spawn_err(
        ctx.terminals
            .spawn(SpawnParams::new(
                "/nonexistent/qingluan-probe-program",
                &[],
                &ctx.workdir,
                EnvSpec::Snapshot(path_env()),
                "q-s2",
            ))
            .await,
        "nonexistent program",
    )?;
    check!(
        matches!(err, SpawnError::StartFailed(_)),
        "expected StartFailed, got {err:?}"
    );
    check!(
        ctx.terminals.quota().occupying() == before,
        "failed start must free its slot"
    );

    step(9, "spawn t4 (reused slot)");
    let t4 = tokio::time::timeout(
        STEP_TIMEOUT,
        ctx.spawn_fixture("q-s1", "park", &[], EnvSpec::Snapshot(path_env()), false),
    )
    .await
    .map_err(|_| anyhow!("quota hang: spawn t4 exceeded 30s"))??;
    check!(
        ctx.terminals.quota().occupying() == GLOBAL_LIMIT,
        "freed slot must be reusable"
    );

    step(10, "stop t2");
    tokio::time::timeout(STEP_TIMEOUT, t2.stop(default_params()))
        .await
        .map_err(|_| anyhow!("quota hang: stop(t2) exceeded 30s"))??;
    step(11, "stop t3");
    tokio::time::timeout(STEP_TIMEOUT, t3.stop(default_params()))
        .await
        .map_err(|_| anyhow!("quota hang: stop(t3) exceeded 30s"))??;
    step(12, "stop t3b");
    tokio::time::timeout(STEP_TIMEOUT, t3b.stop(default_params()))
        .await
        .map_err(|_| anyhow!("quota hang: stop(t3b) exceeded 30s"))??;
    step(13, "stop t4");
    tokio::time::timeout(STEP_TIMEOUT, t4.stop(default_params()))
        .await
        .map_err(|_| anyhow!("quota hang: stop(t4) exceeded 30s"))??;
    check!(
        ctx.terminals.quota().occupying() == 0,
        "all slots must be released at the end"
    );

    // Same-slot Stop/finalize race on a naturally completed terminal: no
    // panic, no double release.
    step(12, "spawn q-race trailing");
    let t = tokio::time::timeout(
        STEP_TIMEOUT,
        ctx.spawn_fixture(
            "q-race",
            "trailing",
            &[],
            EnvSpec::Snapshot(path_env()),
            false,
        ),
    )
    .await
    .map_err(|_| anyhow!("quota hang: spawn q-race exceeded 30s"))??;
    step(13, "wait q-race exited+closed");
    t.wait_event(
        |e| matches!(e, Event::ProcessExited { .. }),
        Duration::from_secs(5),
    )
    .await
    .context("root never exited")?;
    t.wait_event(
        |e| matches!(e, Event::OutputClosed { .. }),
        Duration::from_secs(5),
    )
    .await
    .context("output never closed")?;
    step(14, "join finalize + stop race");
    let race = tokio::time::timeout(STEP_TIMEOUT, async {
        tokio::join!(t.finalize(), t.stop(default_params()))
    })
    .await
    .map_err(|_| anyhow!("quota hang: finalize/stop race exceeded 30s"))?;
    // Whichever path claims the lifecycle, neither may fail hard; a stop that
    // raced the finalize is allowed to report the existing state.
    let _ = race.0?;
    let stats = race.1?;
    check!(
        stats.existing_state,
        "a stop racing a finished finalize is existing-state"
    );
    check!(
        t.count_events(|e| matches!(e, Event::QuotaReleased)) == 1,
        "exactly one QuotaReleased after the race"
    );
    check!(
        t.quota_state() == quota::SlotState::Released,
        "slot must be Released after the stop/finalize race"
    );

    ctx.record(
        "quota",
        json!({"session_limit": SESSION_LIMIT, "global_limit": GLOBAL_LIMIT, "final_occupying": 0}),
    );
    Ok(())
}

async fn scenario_registry(ctx: &Ctx) -> Result<()> {
    let registry: RegistryFile = serde_json::from_str(
        &fs::read_to_string(ctx.workdir.join("registry.json"))
            .context("registry.json missing (run the crash phase first)")?,
    )
    .context("parse registry.json")?;
    let entry = registry.active.first().context("registry empty")?;
    check!(
        proc::alive_with_starttime(entry.pid, entry.starttime),
        "crash-phase fixture must still be alive (pid {})",
        entry.pid
    );

    // Real recovery entry point with an injectable signal backend: recovery
    // must send ZERO signals and invent no exit result.
    let backend = RecordingBackend::default();
    let interrupted = recover_registry(&registry, &backend);
    check!(
        backend.calls.lock().unwrap().is_empty(),
        "recovery must not signal any recorded pid"
    );
    check!(
        interrupted.len() == registry.active.len(),
        "every active entry must be recovered"
    );
    let record = interrupted.first().context("no interrupted record")?;
    check!(
        record.exit_code.is_none() && record.signal.is_none(),
        "Interrupted must not fabricate an exit result"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    check!(
        proc::alive_with_starttime(entry.pid, entry.starttime),
        "recovery must not signal the old pid"
    );
    // The fixture stays alive on purpose: run.sh reclaims it through the
    // probe cgroup root (cgroup.kill), never through a bare pid signal.
    ctx.record(
        "registry",
        json!({
            "recovered": record.terminal_id,
            "recovery_signal_calls": 0,
            "exit_fabricated": false,
        }),
    );
    Ok(())
}

/// A setsid() child escapes the terminal SESSION but not the terminal CGROUP:
/// under the Gate B direction stop reclaims it (previously a documented
/// non-guarantee — local session escapes are now covered; escaping the cgroup
/// itself, e.g. by migrating out, remains a non-guarantee).
async fn scenario_detached(ctx: &Ctx) -> Result<()> {
    let terminal = ctx
        .spawn_fixture(
            "detached",
            "detach-parent",
            &[],
            EnvSpec::Snapshot(path_env()),
            false,
        )
        .await?;
    let output = terminal
        .wait_output_contains("DETACHED-RUNNING", Duration::from_secs(5))
        .await
        .context("detached child never ran")?;
    let text = String::from_utf8_lossy(&output).into_owned();
    let detached_pid = text
        .split(['\n', '\r'])
        .find_map(|line| line.strip_prefix("DETACHED "))
        .and_then(|rest| rest.trim().parse::<i32>().ok())
        .context("no DETACHED pid line in output")?;
    let detached_stat = proc::read_stat(detached_pid).context("detached stat missing")?;

    let exited = terminal
        .wait_event(
            |e| matches!(e, Event::ProcessExited { .. }),
            Duration::from_secs(5),
        )
        .await
        .context("detach root never exited")?;
    check!(
        matches!(
            exited,
            Event::ProcessExited {
                code: Some(0),
                signal: None
            }
        ),
        "expected clean exit, got {exited:?}"
    );
    check!(
        detached_stat.session != terminal.sid,
        "detached process must leave the terminal session"
    );
    check!(
        detached_stat.session == detached_pid,
        "detached process must lead its own session"
    );
    let cg_path = terminal.cgroup_path().context("no cgroup event")?;
    let detached_cg = cgroup::proc_cgroup_path(detached_pid).context("detached cgroup path")?;
    check!(
        detached_cg.starts_with(&cg_path),
        "setsid escape must stay inside the terminal cgroup: {:?} !~ {:?}",
        detached_cg,
        cg_path
    );

    let stats = terminal.stop(default_params()).await?;
    // cgroup.kill reclaims the session-escaped pts holder.
    wait_gone(
        detached_pid,
        detached_stat.starttime,
        Duration::from_secs(3),
    )
    .await
    .context("setsid child must be reclaimed by the terminal cgroup")?;
    check!(
        terminal.count_events(|e| matches!(e, Event::OutputClosed { .. })) == 1,
        "exactly one OutputClosed"
    );
    check_lifecycle_cardinality(&terminal, true)?;
    ctx.record(
        "detached",
        json!({
            "pid": detached_pid,
            "escaped_session": true,
            "stayed_in_cgroup": true,
            "reclaimed_by_cgroup": true,
            "stop": stop_stats_value(&stats),
        }),
    );
    ctx.dump_events("detached", &terminal)
}

/// Adversarial stop: the root traps SIGTERM and forks TERM-immune children in
/// new process groups on every TERM round. TERM can never complete here;
/// cgroup.kill is the fixed point that reclaims every late fork.
async fn scenario_term_fork(ctx: &Ctx) -> Result<()> {
    let terminal = ctx
        .spawn_fixture(
            "term-fork",
            "term-fork",
            &[],
            EnvSpec::Snapshot(path_env()),
            false,
        )
        .await?;
    terminal
        .wait_output_contains("TERM-FORK-READY", Duration::from_secs(5))
        .await
        .context("term-fork root never became ready")?;

    let stats = terminal.stop(default_params()).await?;
    check!(stats.forced, "TERM-forking root must require cgroup.kill");
    check!(
        stats.term_rounds >= 1,
        "the TERM phase must have run at least one signalling round"
    );
    // The root forked 3 children per TERM receipt; signalled must exceed the
    // root alone, proving the rescan caught the late forks.
    check!(
        stats.signalled >= 2,
        "rescanning must have signalled the late-forked children (signalled={})",
        stats.signalled
    );
    check!(
        stats.total_ms < 5000,
        "stop must complete in bounded time, took {}ms",
        stats.total_ms
    );
    check_lifecycle_cardinality(&terminal, true)?;
    ctx.record(
        "term_fork",
        json!({
            "children_per_term": 3,
            "children_immune_to_term": true,
            "stop": stop_stats_value(&stats),
        }),
    );
    ctx.dump_events("term-fork", &terminal)
}

// --- Phases --------------------------------------------------------------------

async fn phase_crash(workdir: PathBuf) -> Result<()> {
    let fixture = fixture_path()?;
    let terminals = Terminals::new(SESSION_LIMIT, GLOBAL_LIMIT, &workdir, "crash")?;
    let terminal = terminals
        .spawn(SpawnParams::new(
            &fixture.to_string_lossy(),
            &["park"],
            &workdir,
            EnvSpec::Snapshot(path_env()),
            "crash",
        ))
        .await
        .map_err(|e| anyhow!("crash spawn: {e:?}"))?;
    let registry = RegistryFile {
        active: vec![RegistryEntry {
            terminal_id: "crash-t1".into(),
            pid: terminal.root_pid,
            starttime: terminal.starttime,
        }],
    };
    fs::write(
        workdir.join("registry.json"),
        serde_json::to_string_pretty(&registry)?,
    )?;
    println!(
        "CRASH-READY pid={} starttime={} cgroup={:?}",
        terminal.root_pid,
        terminal.starttime,
        terminal.cgroup_path()
    );
    // Abrupt exit: no stop, no drops — the SIGHUP-ignoring fixture survives
    // inside its terminal cgroup under the probe root.
    std::process::exit(0);
}

async fn phase_main(workdir: PathBuf) -> Result<()> {
    // Hard gate: this probe only runs where a delegated cgroup v2 subtree
    // with cgroup.kill and working pidfds exists. No /proc-snapshot fallback.
    cgroup::selftest_pidfd().context("pidfd selftest")?;

    let fixture = fixture_path()?;
    let ctx = Ctx {
        workdir: workdir.clone(),
        fixture,
        terminals: Terminals::new(SESSION_LIMIT, GLOBAL_LIMIT, &workdir, "main")?,
        measurements: Mutex::new(BTreeMap::new()),
    };
    fs::write(workdir.join("events.jsonl"), b"").context("reset events.jsonl")?;

    let scenarios: usize = 25;
    println!("=== scenario identity ===");
    scenario_identity(&ctx).await?;
    println!("=== scenario env ===");
    scenario_env(&ctx).await?;
    println!("=== scenario write-bytes ===");
    scenario_write_bytes(&ctx).await?;
    println!("=== scenario resize ===");
    scenario_resize(&ctx).await?;
    println!("=== scenario trailing ===");
    scenario_trailing(&ctx).await?;
    println!("=== scenario child-holds ===");
    scenario_child_holds(&ctx).await?;
    println!("=== scenario shell-jobs ===");
    scenario_shell_jobs(&ctx).await?;
    println!("=== scenario kill-escalation ===");
    scenario_kill_escalation(&ctx).await?;
    println!("=== scenario blocked-write-stop ===");
    scenario_blocked_write_stop(&ctx).await?;
    println!("=== scenario generation ===");
    scenario_generation(&ctx).await?;
    println!("=== scenario generation-race ===");
    scenario_generation_race(&ctx).await?;
    println!("=== scenario write-deadline ===");
    scenario_write_deadline(&ctx).await?;
    println!("=== scenario service-shutdown ===");
    scenario_service_shutdown(&ctx).await?;
    println!("=== scenario oversize ===");
    scenario_oversize(&ctx).await?;
    println!("=== scenario queue-full ===");
    scenario_queue_full(&ctx).await?;
    println!("=== scenario stop-idempotent ===");
    scenario_stop_idempotent(&ctx).await?;
    println!("=== scenario close-race ===");
    scenario_close_race(&ctx).await?;
    println!("=== scenario monitor-fault ===");
    scenario_monitor_fault(&ctx).await?;
    println!("=== scenario start-faults ===");
    scenario_start_faults(&ctx).await?;
    println!("=== scenario quota ===");
    scenario_quota(&ctx).await?;
    println!("=== scenario registry-interrupted ===");
    scenario_registry(&ctx).await?;
    println!("=== scenario detached ===");
    scenario_detached(&ctx).await?;
    println!("=== scenario term-fork ===");
    scenario_term_fork(&ctx).await?;
    // The remaining scenarios deliberately leave slots occupied (failed
    // stop / failed rollback cleanups), so they run LAST: nothing after them
    // needs a slot, and the global limit (4) covers their leftovers.
    println!("=== scenario stop-cancellation ===");
    scenario_stop_cancellation(&ctx).await?;
    println!("=== scenario rollback-cleanup-failure ===");
    scenario_rollback_cleanup_failure(&ctx).await?;

    let measurements = ctx.measurements.lock().unwrap().clone();
    let summary = json!({"ok": true, "scenarios": scenarios, "measurements": measurements});
    fs::write(
        workdir.join("summary.json"),
        serde_json::to_string_pretty(&summary)?,
    )?;
    println!("PROBE_OK scenarios={scenarios}");
    println!("SUMMARY {summary}");
    Ok(())
}

/// Isolated runner for the quota scenario (deadlock reproduction only).
async fn phase_quota(workdir: PathBuf) -> Result<()> {
    cgroup::selftest_pidfd().context("pidfd selftest")?;
    let fixture = fixture_path()?;
    let ctx = Ctx {
        workdir: workdir.clone(),
        fixture,
        terminals: Terminals::new(SESSION_LIMIT, GLOBAL_LIMIT, &workdir, "quota")?,
        measurements: Mutex::new(BTreeMap::new()),
    };
    fs::write(workdir.join("events.jsonl"), b"").context("reset events.jsonl")?;
    println!("=== scenario quota (isolated) ===");
    scenario_quota(&ctx).await?;
    println!("QUOTA_OK");
    Ok(())
}

/// Kill and remove the probe cgroup root recorded in the workdir. This is
/// the ONLY cleanup path: identity comes from the cgroup itself (the probe
/// process is never inside it), never from pgrep or bare pids.
fn cleanup_workdir(workdir: &Path) -> Result<()> {
    let marker = workdir.join("cgroup-root.json");
    let text = fs::read_to_string(&marker).context("read cgroup-root.json")?;
    let value: serde_json::Value = serde_json::from_str(&text).context("parse cgroup-root.json")?;
    let path = value["path"]
        .as_str()
        .context("cgroup-root.json has no path")?;
    let root = cgroup::ProbeCgroupRoot {
        path: PathBuf::from(path),
    };
    root.remove()?;
    println!("CLEANUP removed={path}");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let usage = "usage: pty-probe --phase crash|main <workdir> | --cleanup <workdir>";
    match args.get(1).map(String::as_str) {
        Some("--cleanup") => {
            let path = args.get(2).ok_or_else(|| anyhow!(usage))?;
            cleanup_workdir(Path::new(path))
        }
        Some("--phase") => {
            let phase = args
                .get(2)
                .map(String::as_str)
                .ok_or_else(|| anyhow!(usage))?;
            let workdir = PathBuf::from(args.get(3).ok_or_else(|| anyhow!(usage))?);
            fs::create_dir_all(&workdir).context("create workdir")?;
            match phase {
                "crash" => phase_crash(workdir).await,
                "main" => phase_main(workdir).await,
                "quota" => phase_quota(workdir).await,
                _ => Err(anyhow!(usage)),
            }
        }
        _ => Err(anyhow!(usage)),
    }
}
