//! Throwaway selfcheck harness for probe C (foundation only). NOT production
//! code.
//!
//! Minimal single-process scenario, deliberately without the crash matrix:
//! runtime-loaded migrations, the append/fsync/commit/publish order, a long
//! multibyte line split across frames on character boundaries, one rotation,
//! events + exit committed in the same transaction, a normal restart (epoch
//! and line watermark preserved, no fabricated gaps), and a synced-but-
//! uncommitted tail that recovery must drop without breaking line numbering.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Result, bail};
use serde::Serialize;
use uuid::Uuid;

use crate::Writer;
use crate::db::{GapRow, Store};

#[derive(Debug, Serialize)]
pub struct SelfcheckSummary {
    pub stage: &'static str,
    pub terminal_id: String,
    pub workdir: String,
    pub first_epoch: String,
    pub final_epoch: String,
    pub epoch_preserved: bool,
    pub segments: usize,
    pub line_watermark: u64,
    pub long_line_frames: usize,
    pub events_committed: u64,
    pub uncommitted_tail_line: u64,
    pub gaps: Vec<GapRow>,
    pub degraded: bool,
    pub checks: Vec<String>,
}

/// Deterministically close the pool: the writer must be dropped first.
async fn close_store(store: Arc<Store>) -> Result<()> {
    match Arc::try_unwrap(store) {
        Ok(s) => Ok(s.close().await),
        Err(_) => bail!("writer still holds a store reference"),
    }
}

pub async fn selfcheck(workdir: &Path, migrations: &Path) -> Result<SelfcheckSummary> {
    let db_path = workdir.join(crate::DB_FILE);
    let mut checks: Vec<String> = Vec::new();

    // ── First open: recovery creates the terminal with a fresh epoch.
    let store = Arc::new(Store::open(&db_path, migrations).await?);
    let mut writer = Writer::open(store.clone(), workdir, crate::TERMINAL_ID).await?;
    let first_epoch = writer.epoch();

    // ── Append phase: short line, four long multibyte lines (each split
    // across the 64 KiB payload bound on a character boundary; together they
    // force one rotation), an output event, then exit + event in the same
    // transaction.
    writer.append_line("hello storage", None, None).await?;
    let long_line = "界".repeat(22 * 1024);
    let mut long_frames = 0;
    for _ in 0..4 {
        let published = writer.append_line(&long_line, None, None).await?;
        long_frames = published.frames;
    }
    writer
        .append_line(
            "with event",
            Some(("output", r#"{"seq":1}"#.to_string())),
            None,
        )
        .await?;
    let exit_line = writer
        .append_line(
            "bye",
            Some(("exit", r#"{"code":0}"#.to_string())),
            Some((0, "clean")),
        )
        .await?
        .line;
    let watermark_before_restart = writer.watermark();
    let segments = store.segments(crate::TERMINAL_ID).await?.len();
    let events_committed = store.event_count(crate::TERMINAL_ID).await?;

    if long_frames < 2 {
        bail!("long multibyte line stayed in one frame; payload bound not exercised");
    }
    if segments < 2 {
        bail!("no rotation happened; MAX_SEGMENT_BYTES path not exercised");
    }
    if events_committed != 2 {
        bail!("expected 2 committed session events, got {events_committed}");
    }
    if watermark_before_restart != 7 {
        bail!("expected watermark 7, got {watermark_before_restart}");
    }
    checks.push(format!(
        "append phase: epoch {}, {segments} segments (rotated), long line in {long_frames} frames, {events_committed} events",
        Uuid::from_bytes(first_epoch)
    ));

    drop(writer);
    close_store(store).await?;

    // ── Normal restart: epoch and watermark preserved, no gaps.
    let store = Arc::new(Store::open(&db_path, migrations).await?);
    let mut writer = Writer::open(store.clone(), workdir, crate::TERMINAL_ID).await?;
    if writer.epoch() != first_epoch {
        bail!("normal restart rotated the epoch");
    }
    if writer.watermark() != watermark_before_restart {
        bail!(
            "normal restart changed the watermark: {} != {watermark_before_restart}",
            writer.watermark()
        );
    }
    let gaps = store.list_gaps(crate::TERMINAL_ID).await?;
    if !gaps.is_empty() {
        bail!("normal restart fabricated gaps: {gaps:?}");
    }
    checks.push("normal restart: epoch and watermark preserved, no gaps".to_string());

    // ── Synced-but-uncommitted tail (writer killed between fsync and the
    // SQLite transaction): recovery must drop it; its line number was never
    // published, so numbering continues right past it.
    let ghost_line = writer.append_uncommitted_for_fixture("ghost frame never indexed")?;
    drop(writer);
    close_store(store).await?;

    let store = Arc::new(Store::open(&db_path, migrations).await?);
    let mut writer = Writer::open(store.clone(), workdir, crate::TERMINAL_ID).await?;
    if writer.watermark() != exit_line {
        bail!(
            "uncommitted tail leaked into the watermark: {} != {exit_line}",
            writer.watermark()
        );
    }
    if writer.epoch() != first_epoch {
        bail!("tail recovery rotated the epoch");
    }
    let gaps = store.list_gaps(crate::TERMINAL_ID).await?;
    if !gaps.is_empty() {
        bail!("uncommitted tail fabricated gaps: {gaps:?}");
    }
    let resumed = writer.append_line("after recovery", None, None).await?;
    if resumed.line != ghost_line {
        bail!(
            "line numbering broke after the dropped tail: {} != {}",
            resumed.line,
            ghost_line
        );
    }
    checks.push(format!(
        "uncommitted tail: line {ghost_line} dropped (never published, number re-assigned), watermark stays {exit_line}"
    ));

    let term = store.get_terminal(crate::TERMINAL_ID).await?;
    let summary = SelfcheckSummary {
        stage: "foundation",
        terminal_id: crate::TERMINAL_ID.to_string(),
        workdir: workdir.display().to_string(),
        first_epoch: Uuid::from_bytes(first_epoch).to_string(),
        final_epoch: Uuid::from_bytes(writer.epoch()).to_string(),
        epoch_preserved: writer.epoch() == first_epoch,
        segments: store.segments(crate::TERMINAL_ID).await?.len(),
        line_watermark: term.line_watermark,
        long_line_frames: long_frames,
        events_committed: store.event_count(crate::TERMINAL_ID).await?,
        uncommitted_tail_line: ghost_line,
        gaps: store.list_gaps(crate::TERMINAL_ID).await?,
        degraded: term.degraded,
        checks,
    };
    if summary.degraded {
        bail!("selfcheck left the terminal degraded");
    }
    drop(writer);
    close_store(store).await?;
    Ok(summary)
}
