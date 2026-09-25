//! S4 query-surface integration tests through `LogStore` only: fixed-range
//! reads of committed normalized lines (long lines paged by
//! `byte_offset`, UTF-8 boundaries, cross-frame and cross-segment
//! continuation, a fixed `end_line` that later appends never extend,
//! cursor expiry on identity/retention/gap mismatch, the explicit
//! degraded latch) and literal grep (matches across frame chunks and page
//! boundaries, separate scan and response budgets, a zero-match partial
//! scan that never claims completeness, and a refusal across an explicit
//! gap).
//!
//! Every test uses a temp root deleted on exit and drives only the public
//! seam.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use qingluan_core::terminal::{
    ExternalSessionId, GrepLimits, GrepQuery, GrepRequest, GrepStop, HistoryPosition, LogEpoch,
    LogIdentity, QueryError, ReadLimits, ReadRequest, ReadStart, ReadTruncation, SessionRef,
    SessionSource, TerminalId, TerminalRef,
};
use qingluan_storage::{LogStore, StorageError};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ql-storage-s4-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempRoot(path)
    }
}

impl std::ops::Deref for TempRoot {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const EPOCH: &str = "0a1b2c3d-0e0f-4a1b-8c2d-0e0f4a1b8c2d";

fn log_identity(epoch: &str) -> LogIdentity {
    LogIdentity {
        terminal: TerminalRef {
            session: SessionRef {
                source: SessionSource::new("pi"),
                external_id: ExternalSessionId::new("session-1"),
            },
            terminal_id: TerminalId::new("7c9e6679-7425-40de-944b-e07fc1f90ae7"),
        },
        log_epoch: LogEpoch::new(epoch),
    }
}

fn pos(line: u64, byte_offset: u64) -> HistoryPosition {
    HistoryPosition::new(line, byte_offset).expect("valid position")
}

/// Read one fixed range to completion, returning every fragment in order.
async fn read_all(
    store: &LogStore,
    log: &LogIdentity,
    start: Option<HistoryPosition>,
) -> Vec<(u64, u64, String, bool)> {
    let limits = ReadLimits::new(250, 32 * 1024).expect("limits");
    let start = start.map(ReadStart::At);
    let mut request = ReadRequest::first(log.clone(), start, limits).expect("request");
    let mut out = Vec::new();
    for _ in 0..10_000 {
        let result = store.read(&request).await.expect("read");
        for fragment in result.page().fragments() {
            out.push((
                fragment.position().line(),
                fragment.position().byte_offset(),
                fragment.text().to_owned(),
                fragment.suffix_remaining(),
            ));
        }
        match result.page().next() {
            Some(next) => request = ReadRequest::resume(next.clone(), limits),
            None => return out,
        }
    }
    panic!("read did not terminate");
}

/// Grep one query to completion through its scan points.
async fn grep_all(store: &LogStore, query: GrepQuery, limits: GrepLimits) -> Vec<(u64, u64)> {
    let mut request = GrepRequest::fresh(query);
    let mut out = Vec::new();
    for _ in 0..10_000 {
        let page = store.grep(&request, limits).await.expect("grep");
        for found in page.matches() {
            out.push((found.position().line(), found.position().byte_offset()));
        }
        match page.next_scan() {
            Some(point) => {
                request = GrepRequest::new(point.query().clone(), Some(point.clone()))
                    .expect("resume binds the same query");
            }
            None => return out,
        }
    }
    panic!("grep did not terminate");
}

/// Collapse fragments into whole lines: a page may split one line across
/// a byte-budget boundary, so fragments of the same line concatenate.
fn lines_of(fragments: &[(u64, u64, String, bool)]) -> Vec<(u64, String)> {
    let mut out: Vec<(u64, String)> = Vec::new();
    for (line, _, text, _) in fragments {
        match out.last_mut() {
            Some((last, text_so_far)) if last == line => text_so_far.push_str(text),
            _ => out.push((*line, text.clone())),
        }
    }
    out
}

fn grep_query(log: &LogIdentity, needle: &str, start: u64, end_line: u64) -> GrepQuery {
    GrepQuery::new(log.clone(), needle, true, pos(start, 0), end_line, 0).expect("query")
}

#[tokio::test]
async fn read_is_empty_before_the_first_line_is_committed() {
    let root = TempRoot::new("empty");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity(EPOCH);
    // Attaching a writer creates the terminal row; nothing is appended, so
    // the log has no committed history yet.
    let _writer = store.open_writer(&log).await.unwrap();

    let result = store
        .read(&ReadRequest::first(log.clone(), None, ReadLimits::DEFAULT).unwrap())
        .await
        .expect("read");
    assert!(result.page().fragments().is_empty());
    assert!(result.page().next().is_none());
    assert_eq!(result.page().truncation(), None);
    assert_eq!(result.page().retained(), None);
    assert!(!result.page().degraded());
    // Nothing is committed, so the minted fixed bound is the earliest
    // position itself and the page is complete and empty.
    assert_eq!(result.cursor().end_line(), 1);

    // Unknown terminals are unknown, never an empty log.
    let other = LogIdentity {
        terminal: TerminalRef {
            terminal_id: TerminalId::new("00000000-0000-4000-8000-000000000000"),
            ..log.terminal.clone()
        },
        ..log.clone()
    };
    assert!(matches!(
        store
            .read(&ReadRequest::first(other, None, ReadLimits::DEFAULT).unwrap())
            .await,
        Err(StorageError::UnknownLog(_))
    ));
}

#[tokio::test]
async fn read_pins_end_line_and_later_appends_never_extend_it() {
    let root = TempRoot::new("fixed-end");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity(EPOCH);
    let mut writer = store.open_writer(&log).await.unwrap();

    for line in 1..=5 {
        writer
            .append_line(line, &format!("line-{line}"))
            .await
            .unwrap();
    }
    writer.flush().await.unwrap();

    // The first page mints the fixed bound at the committed watermark.
    let limits = ReadLimits::new(2, 4096).unwrap();
    let first = store
        .read(&ReadRequest::first(log.clone(), None, limits).unwrap())
        .await
        .expect("read");
    assert_eq!(
        first.cursor().end_line(),
        5,
        "the fixed end is the watermark"
    );
    let lines: Vec<u64> = first
        .page()
        .fragments()
        .iter()
        .map(|fragment| fragment.position().line())
        .collect();
    assert_eq!(lines, vec![1, 2]);
    assert_eq!(first.page().truncation(), Some(ReadTruncation::LineBudget));

    // Later output (and its rotation) is invisible to the minted page.
    for line in 6..=20 {
        writer
            .append_line(line, &format!("line-{line}"))
            .await
            .unwrap();
    }
    writer.flush().await.unwrap();

    let mut request = ReadRequest::resume(first.page().next().unwrap().clone(), limits);
    let mut seen = lines;
    let mut completed = false;
    for _ in 0..10 {
        let page = store.read(&request).await.expect("read");
        for fragment in page.page().fragments() {
            assert!(
                fragment.position().line() <= 5,
                "the fixed bound never grows with later appends"
            );
            seen.push(fragment.position().line());
        }
        assert_eq!(page.cursor().end_line(), 5);
        match page.page().next() {
            Some(next) => request = ReadRequest::resume(next.clone(), limits),
            None => {
                assert_eq!(page.page().truncation(), None, "the fixed range completed");
                completed = true;
                break;
            }
        }
    }
    assert!(completed, "the page sequence terminates at the fixed end");
    assert_eq!(seen, vec![1, 2, 3, 4, 5]);

    // A position above the committed watermark is a benign empty page (the
    // log has not reached it yet), never a refusal.
    let ahead = store
        .read(&ReadRequest::first(log.clone(), Some(ReadStart::At(pos(100, 0))), limits).unwrap())
        .await
        .expect("read");
    assert!(ahead.page().fragments().is_empty());
    assert!(ahead.page().next().is_none());
    assert_eq!(ahead.page().truncation(), None);
    assert_eq!(ahead.cursor().end_line(), 100);

    // A fresh read now sees the later lines, so nothing was lost.
    let all = lines_of(&read_all(&store, &log, None).await);
    assert_eq!(all.len(), 20);
    assert_eq!(all[19].1, "line-20");
}

#[tokio::test]
async fn read_pages_an_over_long_line_by_byte_offset_across_frames() {
    let root = TempRoot::new("long-line");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity(EPOCH);
    let mut writer = store.open_writer(&log).await.unwrap();

    // Wider than one 64 KiB frame and full of multi-byte characters, so the
    // frames are cut inside the line and every page boundary is a
    // character boundary.
    let line = "你好世界".repeat(10_000);
    writer.append_line(1, &line).await.unwrap();
    writer.append_line(2, "after").await.unwrap();
    writer.flush().await.unwrap();
    assert!(
        line.len() > 64 * 1024,
        "the line must span more than one frame's payload"
    );

    // A tiny byte budget forces many continuation pages inside one line.
    let limits = ReadLimits::new(10, 1000).unwrap();
    let mut request = ReadRequest::first(log.clone(), None, limits).unwrap();
    let mut first_line = String::new();
    let mut second_line = String::new();
    let mut pages = 0usize;
    loop {
        let page = store.read(&request).await.expect("read");
        let fragments = page.page().fragments();
        for fragment in fragments {
            let offset = fragment.position().byte_offset();
            match fragment.position().line() {
                1 => {
                    assert!(
                        line.is_char_boundary(offset as usize),
                        "every returned offset is a rune boundary"
                    );
                    assert!(
                        line[offset as usize..].starts_with(fragment.text()),
                        "a fragment continues exactly at the offset it reports"
                    );
                    first_line.push_str(fragment.text());
                }
                2 => {
                    assert!(
                        "after"[offset as usize..].starts_with(fragment.text()),
                        "the short line continues at its reported offset"
                    );
                    second_line.push_str(fragment.text());
                }
                other => panic!("unexpected line {other}"),
            }
        }
        // A fragment continues exactly when its line is not finished by the
        // page: the only incomplete line is the page's last fragment.
        if let Some(last) = fragments.last() {
            assert_eq!(
                last.suffix_remaining(),
                page.page().next().is_some() && last.position().line() == 1
            );
        }
        pages += 1;
        match page.page().next() {
            Some(next) => request = ReadRequest::resume(next.clone(), limits),
            None => break,
        }
        assert!(pages < 1000, "paging terminates");
    }
    assert_eq!(first_line, line, "no byte is lost or duplicated");
    assert_eq!(second_line, "after");
    assert!(pages > 100, "the line really was paged, got {pages} pages");

    // The second line is readable afterwards, with its own position.
    let all = read_all(&store, &log, Some(pos(2, 0))).await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].2, "after");
    assert_eq!(all[0].1, 0);
}

#[tokio::test]
async fn read_continues_across_rotated_segments() {
    let root = TempRoot::new("cross-segment");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity(EPOCH);
    let mut writer = store.open_writer(&log).await.unwrap();
    // Rotate every batch so one history spans many segments.
    writer.set_rotation_threshold(4096).await;

    let mut line = 1u64;
    while line <= 40 {
        let text = format!("{line:02}-{}", "x".repeat(3000));
        writer.append_line(line, &text).await.unwrap();
        writer.flush().await.unwrap();
        line += 1;
    }

    let all = lines_of(&read_all(&store, &log, None).await);
    assert_eq!(all.len(), 40);
    for (index, (found_line, text)) in all.iter().enumerate() {
        assert_eq!(*found_line, index as u64 + 1);
        assert!(text.starts_with(&format!("{:02}-", index + 1)));
    }

    // A read that starts in the middle of the rotated history still sees
    // every later line in order.
    let tail = lines_of(&read_all(&store, &log, Some(pos(25, 0))).await);
    assert_eq!(tail.len(), 16);
    assert_eq!(tail[0].0, 25);
    assert_eq!(tail[15].0, 40);
}

#[tokio::test]
async fn read_refuses_epoch_retention_and_gap_mismatches() {
    let root = TempRoot::new("expiry");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity(EPOCH);
    let mut writer = store.open_writer(&log).await.unwrap();
    for line in 1..=6 {
        writer
            .append_line(line, &format!("line-{line}"))
            .await
            .unwrap();
    }
    writer.flush().await.unwrap();

    // Another epoch is no longer interpretable: a typed refusal, never a
    // silent re-anchor.
    let other_epoch = log_identity("1b2c3d4e-5f60-4712-9a3b-4c5d6e7f8091");
    let mismatch = store
        .grep(
            &GrepRequest::fresh(grep_query(&other_epoch, "line", 1, 6)),
            GrepLimits::DEFAULT,
        )
        .await;
    assert!(matches!(
        mismatch,
        Err(StorageError::Query(QueryError::CursorExpired { .. }))
    ));
    assert!(matches!(
        store
            .read(&ReadRequest::first(other_epoch, None, ReadLimits::DEFAULT).unwrap())
            .await,
        Err(StorageError::Query(QueryError::CursorExpired { .. }))
    ));

    // A recorded loss is an explicit gap, and a read that asks to start
    // inside it is refused with the missing range and the earliest position
    // afterwards. The retired numbers are consumed, never reused.
    let loss = writer.record_line_loss(7, 2).await.unwrap();
    assert_eq!(loss.first_line, 7);
    assert_eq!(loss.lines, 2);

    // A loss may consume line numbers, never skip one: after retiring
    // [7, 9) the next number is 9, so 11 would be a skip.
    assert!(matches!(
        writer.record_line_loss(11, 1).await,
        Err(StorageError::LineNotSequential { .. })
    ));

    let refused = store
        .read(
            &ReadRequest::first(
                log.clone(),
                Some(ReadStart::At(pos(7, 0))),
                ReadLimits::DEFAULT,
            )
            .unwrap(),
        )
        .await;
    match refused {
        Err(StorageError::Query(QueryError::CursorExpired { earliest, missing })) => {
            assert_eq!(earliest, Some(pos(9, 0)));
            assert_eq!(missing.map(|range| range.earliest()), Some(pos(7, 0)));
            assert_eq!(missing.map(|range| range.latest()), Some(pos(9, 0)));
        }
        other => panic!("expected a typed expiry, got {other:?}"),
    }

    // A range that crosses the gap stops before it with an explicit gap
    // reason and no continuation.
    writer.append_line(9, "after-loss").await.unwrap();
    writer.flush().await.unwrap();
    let page = store
        .read(
            &ReadRequest::first(
                log.clone(),
                Some(ReadStart::At(pos(1, 0))),
                ReadLimits::DEFAULT,
            )
            .unwrap(),
        )
        .await
        .expect("read");
    let lines: Vec<u64> = page
        .page()
        .fragments()
        .iter()
        .map(|fragment| fragment.position().line())
        .collect();
    assert_eq!(lines, vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(page.page().truncation(), Some(ReadTruncation::Gap));
    assert!(page.page().next().is_none());
    assert!(page.page().degraded(), "the explicit loss is reported");

    // Grep over a range that intersects the gap is refused, not silently
    // narrowed.
    let refused = store
        .grep(
            &GrepRequest::fresh(grep_query(&log, "line", 5, 9)),
            GrepLimits::DEFAULT,
        )
        .await;
    assert!(matches!(
        refused,
        Err(StorageError::Query(QueryError::Gap { .. }))
    ));

    // Lines after the loss are readable at their stable numbers.
    let after = read_all(&store, &log, Some(pos(9, 0))).await;
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].2, "after-loss");
}

#[tokio::test]
async fn read_refuses_a_position_below_the_retained_floor() {
    let root = TempRoot::new("floor");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity(EPOCH);
    let mut writer = store.open_writer(&log).await.unwrap();
    // A tiny rotation threshold makes retention advance the floor quickly:
    // every creation past the metadata budget reclaims the oldest sealed
    // segment.
    writer.set_rotation_threshold(2048).await;

    let mut line = 1u64;
    while line <= 90 {
        let text = format!("{line:03}-{}", "y".repeat(4000));
        writer.append_line(line, &text).await.unwrap();
        writer.flush().await.unwrap();
        line += 1;
    }

    let refused = store
        .read(
            &ReadRequest::first(
                log.clone(),
                Some(ReadStart::At(pos(1, 0))),
                ReadLimits::DEFAULT,
            )
            .unwrap(),
        )
        .await;
    match refused {
        Err(StorageError::Query(QueryError::CursorExpired { earliest, .. })) => {
            let earliest = earliest.expect("the earliest readable position is reported");
            assert!(earliest.line() > 1, "retention advanced the floor");
            assert!(earliest.byte_offset() == 0);
        }
        other => panic!("expected a typed expiry, got {other:?}"),
    }

    // What is still retained reads cleanly from its own floor.
    let retained = store
        .read(&ReadRequest::first(log.clone(), None, ReadLimits::DEFAULT).unwrap())
        .await
        .expect("read");
    let floor = retained
        .page()
        .retained()
        .expect("a retained window")
        .earliest();
    assert!(floor.line() > 1);
    let all = lines_of(&read_all(&store, &log, Some(floor)).await);
    assert_eq!(all.len(), 90 - floor.line() as usize + 1);
    assert_eq!(all[0].0, floor.line());
}

#[tokio::test]
async fn grep_matches_literals_across_frame_and_page_boundaries() {
    let root = TempRoot::new("grep-long");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity(EPOCH);
    let mut writer = store.open_writer(&log).await.unwrap();

    // One line wider than a frame with the needle split exactly at the
    // 64 KiB payload boundary, plus a second occurrence near the end.
    let filler = "a".repeat(64 * 1024 - 3);
    let line = format!("{filler}NEEDLE{}NEEDLE-tail", "b".repeat(70 * 1024));
    writer.append_line(1, &line).await.unwrap();
    writer
        .append_line(2, "NEEDLE on its own line")
        .await
        .unwrap();
    writer.flush().await.unwrap();

    let query = grep_query(&log, "NEEDLE", 1, 2);
    let found = grep_all(&store, query.clone(), GrepLimits::DEFAULT).await;
    assert_eq!(
        found,
        vec![
            (1, 64 * 1024 - 3),
            (1, 64 * 1024 - 3 + 6 + 70 * 1024),
            (2, 0)
        ],
        "a match split by a frame boundary is still found"
    );

    // A one-match response budget pages through the same line and finds
    // every match exactly once.
    let tiny = GrepLimits::new(1, 32 * 1024, 4 * 1024 * 1024).unwrap();
    let paged = grep_all(&store, query.clone(), tiny).await;
    assert_eq!(paged, found);

    // Case-insensitive matching folds text and needle alike.
    let insensitive = GrepQuery::new(log.clone(), "needle", false, pos(1, 0), 2, 0).unwrap();
    let found = grep_all(&store, insensitive, GrepLimits::DEFAULT).await;
    assert_eq!(found.len(), 3);
}

#[tokio::test]
async fn grep_keeps_scan_and_response_budgets_separate() {
    let root = TempRoot::new("grep-budgets");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity(EPOCH);
    let mut writer = store.open_writer(&log).await.unwrap();
    for line in 1..=40 {
        let text = if line % 10 == 0 {
            format!("line-{line} has a match")
        } else {
            format!("line-{line} is plain {}", "z".repeat(200))
        };
        writer.append_line(line, &text).await.unwrap();
    }
    writer.flush().await.unwrap();

    // The scan budget stops the page after whole lines only: a zero-match
    // page with an unfinished scan never claims completeness.
    let query = grep_query(&log, "match", 1, 40);
    let scan_limited = GrepLimits::new(200, 32 * 1024, 1024).unwrap();
    let page = store
        .grep(&GrepRequest::fresh(query.clone()), scan_limited)
        .await
        .expect("grep");
    assert!(
        page.matches().len() < 4,
        "the scan budget stopped the scan early"
    );
    assert_eq!(page.stopped(), GrepStop::ScanBudget);
    assert!(!page.degraded());
    let point = page.next_scan().expect("a partial scan is resumable");
    assert!(
        point.next().line() * 200 > 1024,
        "at least a page was scanned"
    );

    // The response budget stops the page with the matches found so far and
    // a bound resume point.
    let response_limited = GrepLimits::new(1, 32 * 1024, 4 * 1024 * 1024).unwrap();
    let page = store
        .grep(&GrepRequest::fresh(query.clone()), response_limited)
        .await
        .expect("grep");
    assert_eq!(page.matches().len(), 1);
    assert_eq!(page.stopped(), GrepStop::ResponseBudget);
    assert!(page.next_scan().is_some());

    // A complete scan of the whole range claims completeness and stops at
    // the last committed line.
    let complete = store
        .grep(&GrepRequest::fresh(query.clone()), GrepLimits::DEFAULT)
        .await
        .expect("grep");
    assert_eq!(complete.stopped(), GrepStop::RangeExhausted);
    assert!(complete.next_scan().is_none());
    assert_eq!(complete.matches().len(), 4);
    assert_eq!(complete.scanned_range().latest(), pos(40, 0));

    // Zero matches over an exhausted range is a real "no match" answer.
    let none = grep_query(&log, "not-present-anywhere", 1, 40);
    let page = store
        .grep(&GrepRequest::fresh(none), GrepLimits::DEFAULT)
        .await
        .expect("grep");
    assert!(page.matches().is_empty());
    assert_eq!(page.stopped(), GrepStop::RangeExhausted);
    assert!(page.next_scan().is_none());
}

#[tokio::test]
async fn grep_binds_a_scan_point_to_its_exact_query() {
    let root = TempRoot::new("grep-binding");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity(EPOCH);
    let mut writer = store.open_writer(&log).await.unwrap();
    for line in 1..=6 {
        writer
            .append_line(line, &format!("match-{line} {}", "q".repeat(300)))
            .await
            .unwrap();
    }
    writer.flush().await.unwrap();

    let query = grep_query(&log, "match", 1, 6);
    let page = store
        .grep(
            &GrepRequest::fresh(query.clone()),
            GrepLimits::new(2, 32 * 1024, 4 * 1024 * 1024).unwrap(),
        )
        .await
        .expect("grep");
    let point = page.next_scan().expect("partial scan").clone();

    // The same query resumes.
    assert!(
        store
            .grep(
                &GrepRequest::new(query.clone(), Some(point.clone())).unwrap(),
                GrepLimits::DEFAULT
            )
            .await
            .is_ok()
    );

    // A changed needle, option, or range cannot reuse the point.
    let changed = GrepQuery::new(log.clone(), "other", true, pos(1, 0), 6, 0).unwrap();
    assert!(matches!(
        GrepRequest::new(changed.clone(), Some(point.clone())),
        Err(QueryError::Invalid { .. })
    ));
    let insensitive = GrepQuery::new(log.clone(), "match", false, pos(1, 0), 6, 0).unwrap();
    assert!(GrepRequest::new(insensitive, Some(point.clone())).is_err());
    let narrower = GrepQuery::new(log.clone(), "match", true, pos(1, 0), 3, 0).unwrap();
    assert!(GrepRequest::new(narrower, Some(point.clone())).is_err());

    // A range above the committed watermark has nothing to scan yet.
    let ahead = grep_query(&log, "match", 1, 40);
    let page = store
        .grep(&GrepRequest::fresh(ahead), GrepLimits::DEFAULT)
        .await
        .expect("grep");
    assert_eq!(page.stopped(), GrepStop::RangeExhausted);
    assert_eq!(page.matches().len(), 6);
    assert_eq!(page.scanned_range().latest(), pos(6, 0));
}

#[tokio::test]
async fn grep_returns_bounded_context_lines() {
    let root = TempRoot::new("grep-context");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity(EPOCH);
    let mut writer = store.open_writer(&log).await.unwrap();
    for line in 1..=6 {
        let text = if line == 4 {
            "the needle line".to_owned()
        } else {
            format!("context line {line}")
        };
        writer.append_line(line, &text).await.unwrap();
    }
    writer.flush().await.unwrap();

    let query = GrepQuery::new(log.clone(), "needle", true, pos(1, 0), 6, 2).unwrap();
    let page = store
        .grep(&GrepRequest::fresh(query.clone()), GrepLimits::DEFAULT)
        .await
        .expect("grep");
    assert_eq!(page.matches().len(), 1);
    let lines: Vec<u64> = page.contexts().iter().map(|c| c.line()).collect();
    assert_eq!(lines, vec![2, 3, 5, 6]);
    assert!(!page.contexts_truncated());

    // A tiny response budget shortens contexts explicitly instead of
    // silently.
    let tiny = GrepLimits::new(200, 64, 4 * 1024 * 1024).unwrap();
    let page = store
        .grep(&GrepRequest::fresh(query), tiny)
        .await
        .expect("grep");
    assert!(page.contexts().len() < 4);
    assert!(page.contexts_truncated());
}

#[tokio::test]
async fn log_identity_resolves_without_a_live_handle() {
    let root = TempRoot::new("identity");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity(EPOCH);
    let mut writer = store.open_writer(&log).await.unwrap();
    writer.append_line(1, "one").await.unwrap();
    writer.flush().await.unwrap();

    let resolved = store.log_identity(&log.terminal).await.unwrap();
    assert_eq!(resolved, log);

    let unknown = TerminalRef {
        terminal_id: TerminalId::new("11111111-1111-4111-8111-111111111111"),
        ..log.terminal.clone()
    };
    assert!(matches!(
        store.log_identity(&unknown).await,
        Err(StorageError::UnknownLog(_))
    ));
}

#[tokio::test]
async fn read_keeps_empty_lines_and_clamps_offsets_to_rune_boundaries() {
    let root = TempRoot::new("offsets");
    let store = LogStore::open(&root).await.unwrap();
    let log = log_identity(EPOCH);
    let mut writer = store.open_writer(&log).await.unwrap();
    // Line 1 is empty (a legal history line), line 2 is multi-byte, line 3
    // is short.
    writer.append_line(1, "").await.unwrap();
    writer.append_line(2, "你好世界").await.unwrap();
    writer.append_line(3, "z").await.unwrap();
    writer.flush().await.unwrap();

    let all = read_all(&store, &log, None).await;
    assert_eq!(all.len(), 3);
    assert_eq!((all[0].0, all[0].1, all[0].2.as_str()), (1, 0, ""));
    assert_eq!((all[1].0, all[1].1, all[1].2.as_str()), (2, 0, "你好世界"));
    assert_eq!((all[2].0, all[2].1, all[2].2.as_str()), (3, 0, "z"));

    // An offset past the end of a line clamps to its end and reads nothing,
    // and an offset inside a character clamps forward to the next one.
    let limits = ReadLimits::new(10, 4096).unwrap();
    let beyond = store
        .read(&ReadRequest::first(log.clone(), Some(ReadStart::At(pos(2, 999))), limits).unwrap())
        .await
        .expect("read");
    let fragment = &beyond.page().fragments()[0];
    assert_eq!(fragment.text(), "");
    assert_eq!(fragment.position(), pos(2, 12));
    assert!(fragment.prefix_omitted());
    assert!(!fragment.suffix_remaining());

    let inside = store
        .read(&ReadRequest::first(log.clone(), Some(ReadStart::At(pos(2, 1))), limits).unwrap())
        .await
        .expect("read");
    let fragment = &inside.page().fragments()[0];
    assert_eq!(fragment.text(), "好世界");
    assert_eq!(fragment.position(), pos(2, 3));
    assert!(fragment.prefix_omitted());
}
