//! Throwaway read path for probe C Gate C. NOT production code.
//!
//! Reading resumes from a (line, byte_offset-within-line) cursor, spans frame
//! boundaries of long lines and segment rotation, and never fabricates
//! continuity: a cursor below the earliest available position, inside a
//! log_gap, or inside a vanished segment returns an explicit CURSOR_EXPIRED
//! carrying the earliest available (line, byte_offset) position.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::db::{CursorExpired, SegmentRow, Store};
use crate::frame::{FRAME_FLAG_LINE_END, SEGMENT_HEADER_LEN, ScanOutcome, scan_frames};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ReadCursor {
    pub line: u64,
    pub byte_offset: u64,
}

#[derive(Debug, Serialize)]
pub struct ReadData {
    pub bytes: Vec<u8>,
    pub next: ReadCursor,
    /// True when every available byte after the cursor was returned (the
    /// stream is exhausted); false when `max_bytes` or a gap stopped it.
    pub at_end: bool,
}

#[derive(Debug, Serialize)]
pub enum ReadOutcome {
    Data(ReadData),
    Expired(CursorExpired),
}

async fn live_segments(store: &Store, terminal_id: &str) -> Result<Vec<SegmentRow>> {
    Ok(store
        .segments(terminal_id)
        .await?
        .into_iter()
        .filter(|s| s.state == "active" || s.state == "sealed")
        .collect())
}

/// Earliest still-readable position: the first line of the oldest live
/// segment, or (watermark + 1, 0) when history was fully reclaimed.
pub async fn earliest_position(store: &Store, terminal_id: &str) -> Result<ReadCursor> {
    let term = store.get_terminal(terminal_id).await?;
    let segs = live_segments(store, terminal_id).await?;
    let line = segs
        .iter()
        .map(|s| s.first_line)
        .min()
        .unwrap_or(term.line_watermark + 1);
    Ok(ReadCursor {
        line,
        byte_offset: 0,
    })
}

/// Read up to `max_bytes` starting exactly at `cursor`.
///
/// Visibility rule: only the DB-committed prefix of each segment file is
/// scanned. Bytes appended and fsynced but not yet committed by the SQLite
/// transaction are invisible to every read (commit-before-query); a file
/// shorter than its committed boundary is rejected outright because that
/// state requires recovery, not silent narrowing.
pub async fn read_from(
    store: &Store,
    root: &Path,
    terminal_id: &str,
    cursor: ReadCursor,
    max_bytes: usize,
) -> Result<ReadOutcome> {
    let term = store.get_terminal(terminal_id).await?;
    let segs = live_segments(store, terminal_id).await?;
    let earliest = earliest_position(store, terminal_id).await?;
    if cursor.line > term.line_watermark {
        bail!(
            "cursor line {} beyond watermark {}",
            cursor.line,
            term.line_watermark
        );
    }
    let expired = || CursorExpired {
        earliest_line: earliest.line,
        earliest_byte_offset: earliest.byte_offset,
    };
    if cursor.line < earliest.line {
        return Ok(ReadOutcome::Expired(expired()));
    }
    // The segment whose available range covers the cursor line.
    let Some(start_idx) = segs
        .iter()
        .position(|s| s.first_line <= cursor.line && cursor.line < s.last_line)
    else {
        // Inside a gap or a vanished segment: explicit expiry, no continuity.
        return Ok(ReadOutcome::Expired(expired()));
    };

    let mut bytes: Vec<u8> = Vec::with_capacity(max_bytes.min(1 << 20));
    let mut emitted = 0usize;
    let mut started = false;
    let mut skip_in_frame = 0usize;
    let mut next = cursor;
    let mut prev_line: Option<u64> = None;
    let mut stopped_at_gap = false;
    // Set when the cursor line's frames were fully consumed (cursor pinned at
    // the end of its line): the following line is a legal continuation.
    let mut cursor_line_consumed = false;
    // Exact validated end offset of the cursor line: either its line-end
    // frame's end, or the end of its last available chunk when the line is
    // cut short. Offsets beyond it are invalid, never rounded forward.
    let mut cursor_line_end: Option<u64> = None;

    'outer: for seg in &segs[start_idx..] {
        let path = root.join(&seg.file_name);
        let data =
            std::fs::read(&path).with_context(|| format!("read segment {}", path.display()))?;
        if (data.len() as u64) < seg.committed_bytes {
            bail!(
                "segment {} is {} bytes on disk but its committed boundary is {}; \
                 run recovery before reading",
                seg.file_name,
                data.len(),
                seg.committed_bytes
            );
        }
        // Scan only the DB-committed prefix: synced-but-uncommitted tail
        // frames are physically present but not queryable yet.
        let committed = seg.committed_bytes as usize;
        let scan = scan_frames(&data[..committed], SEGMENT_HEADER_LEN);
        if scan.outcome != ScanOutcome::Clean {
            bail!(
                "segment {} is not clean; run recovery before reading",
                seg.file_name
            );
        }
        for f in scan.frames {
            let h = &f.header;
            let payload = &data[f.payload_at..f.end - 4];
            if !started {
                if h.line < cursor.line {
                    prev_line = Some(h.line);
                    continue;
                }
                if h.line == cursor.line {
                    let frame_end = h.line_offset + payload.len() as u64;
                    if cursor.byte_offset >= frame_end {
                        // Cursor at or after the end of this chunk.
                        if h.flags & FRAME_FLAG_LINE_END != 0 {
                            // Definitive end of the line: anything beyond it
                            // is an invalid cursor, not a consumed line.
                            if cursor.byte_offset > frame_end {
                                bail!(
                                    "cursor byte_offset {} is beyond the end {} of line {}",
                                    cursor.byte_offset,
                                    frame_end,
                                    cursor.line
                                );
                            }
                            cursor_line_consumed = true;
                        }
                        cursor_line_end = Some(frame_end);
                        prev_line = Some(h.line);
                        continue;
                    }
                    if cursor.byte_offset < h.line_offset {
                        bail!(
                            "cursor byte_offset {} falls inside unavailable bytes of line {} \
                             (next chunk starts at {})",
                            cursor.byte_offset,
                            cursor.line,
                            h.line_offset
                        );
                    }
                    started = true;
                    skip_in_frame = (cursor.byte_offset - h.line_offset) as usize;
                } else {
                    // h.line > cursor.line: the requested line either ended
                    // exactly at the cursor (legal continuation) or its data
                    // stops short of the cursor / is missing (expiry or an
                    // explicit invalid-cursor failure).
                    if !cursor_line_consumed {
                        if let Some(end) = cursor_line_end {
                            if cursor.byte_offset > end {
                                bail!(
                                    "cursor byte_offset {} is beyond the available end {} \
                                     of line {}",
                                    cursor.byte_offset,
                                    end,
                                    cursor.line
                                );
                            }
                        }
                        // The requested line is inside a hole: explicit expiry.
                        return Ok(ReadOutcome::Expired(expired()));
                    }
                    if h.line != cursor.line + 1 {
                        return Ok(ReadOutcome::Expired(expired()));
                    }
                    started = true;
                    skip_in_frame = 0;
                }
            } else if let Some(prev) = prev_line {
                // Continuation only across consecutive lines (or further
                // chunks of the same line); anything else is a gap.
                if h.line != prev && h.line != prev + 1 {
                    stopped_at_gap = true;
                    break 'outer;
                }
            }
            let take = (max_bytes - emitted).min(payload.len() - skip_in_frame);
            bytes.extend_from_slice(&payload[skip_in_frame..skip_in_frame + take]);
            emitted += take;
            let pos_in_frame = skip_in_frame + take;
            next = ReadCursor {
                line: h.line,
                byte_offset: h.line_offset + pos_in_frame as u64,
            };
            prev_line = Some(h.line);
            skip_in_frame = 0;
            if emitted >= max_bytes {
                break 'outer;
            }
        }
    }
    if !started {
        // Nothing at or after the cursor (e.g. pinned at the fixed read end).
        // A cursor beyond the validated end of its (cut-short) line is an
        // explicit failure, never a silent round-forward to "consumed".
        if !cursor_line_consumed {
            if let Some(end) = cursor_line_end {
                if cursor.byte_offset > end {
                    bail!(
                        "cursor byte_offset {} is beyond the available end {} of line {}",
                        cursor.byte_offset,
                        end,
                        cursor.line
                    );
                }
            }
        }
        return Ok(ReadOutcome::Data(ReadData {
            bytes,
            next: cursor,
            at_end: true,
        }));
    }
    Ok(ReadOutcome::Data(ReadData {
        bytes,
        next,
        at_end: !stopped_at_gap && emitted < max_bytes,
    }))
}

/// Resolve a persisted tail revision to its fixed read cursor. Returns
/// `None` when the revision is unknown or was expired by resource recovery
/// (its holding segment was reclaimed in the same transaction that deleted
/// the segment); the caller must treat that as an explicit CURSOR_EXPIRED,
/// never as a fabricated position.
pub async fn resolve_tail_revision(
    store: &Store,
    terminal_id: &str,
    revision: u64,
) -> Result<Option<ReadCursor>> {
    Ok(store
        .tail_revision(terminal_id, revision)
        .await?
        .map(|r| ReadCursor {
            line: r.line,
            byte_offset: r.byte_offset,
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{CommitInput, NewSegment};
    use crate::frame::{
        FRAME_KIND_DATA, FrameHeader, SEGMENT_KIND_DATA, SegmentHeader, encode_frame, split_line,
    };
    use std::path::PathBuf;

    fn test_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ql-storage-reader-{}-{}",
            tag,
            uuid::Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn migrations() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations")
    }

    fn frame(seq: u64, line: u64, offset: u64, payload: &[u8]) -> Vec<u8> {
        encode_frame(
            &FrameHeader {
                kind: FRAME_KIND_DATA,
                flags: FRAME_FLAG_LINE_END,
                frame_seq: seq,
                line,
                line_offset: offset,
                payload_len: payload.len() as u32,
            },
            payload,
        )
        .unwrap()
    }

    fn frame_mid(seq: u64, line: u64, offset: u64, payload: &[u8]) -> Vec<u8> {
        encode_frame(
            &FrameHeader {
                kind: FRAME_KIND_DATA,
                flags: 0,
                frame_seq: seq,
                line,
                line_offset: offset,
                payload_len: payload.len() as u32,
            },
            payload,
        )
        .unwrap()
    }

    fn header(seg: i64) -> Vec<u8> {
        SegmentHeader {
            kind: SEGMENT_KIND_DATA,
            flags: 0,
            terminal: [7u8; 16],
            epoch: [9u8; 16],
            segment_id: seg as u64,
            created_ms: 42,
        }
        .encode()
        .to_vec()
    }

    /// Store with one terminal/segment; the caller passes the exact file
    /// bytes (including the 64-byte header placeholder, which is replaced
    /// with a valid header) and how much of them the DB has committed.
    async fn fixture(
        tag: &str,
        data: &[u8],
        committed: u64,
        watermark: u64,
        last_line: u64,
    ) -> (PathBuf, Store) {
        let root = test_root(tag);
        let store = Store::open(&root.join("terminal.db"), &migrations())
            .await
            .unwrap();
        store
            .create_terminal("t", [7u8; 16], [9u8; 16])
            .await
            .unwrap();
        let seg = store
            .insert_segment(&NewSegment {
                terminal_id: "t".into(),
                file_name: "seg-000001.log".into(),
                first_line: 1,
                last_line: 1,
            })
            .await
            .unwrap();
        let mut file = data.to_vec();
        file[..SEGMENT_HEADER_LEN].copy_from_slice(&header(seg));
        std::fs::write(root.join("seg-000001.log"), &file).unwrap();
        store
            .commit_visible(
                "t",
                &CommitInput {
                    segment_id: seg,
                    committed_bytes: committed,
                    fsynced_bytes: committed,
                    segment_last_line: last_line,
                    line_watermark: watermark,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        (root, store)
    }

    /// Encode whole lines exactly the way the writer does: chunks cut on
    /// character boundaries, LINE_END only on the last chunk, consecutive
    /// frame_seq, offset 0 at each new line.
    fn lines_data(lines: &[&[u8]]) -> Vec<u8> {
        let mut data = vec![0u8; SEGMENT_HEADER_LEN];
        let mut seq = 0u64;
        for (i, l) in lines.iter().enumerate() {
            let line_no = i as u64 + 1;
            let mut offset = 0u64;
            for chunk in split_line(std::str::from_utf8(l).unwrap()) {
                let is_last = offset + chunk.len() as u64 == l.len() as u64;
                data.extend_from_slice(
                    &encode_frame(
                        &FrameHeader {
                            kind: FRAME_KIND_DATA,
                            flags: if is_last { FRAME_FLAG_LINE_END } else { 0 },
                            frame_seq: seq,
                            line: line_no,
                            line_offset: offset,
                            payload_len: chunk.len() as u32,
                        },
                        chunk,
                    )
                    .unwrap(),
                );
                offset += chunk.len() as u64;
                seq += 1;
            }
        }
        data
    }

    #[tokio::test]
    async fn read_stops_at_committed_boundary() {
        // Two committed lines plus a synced-but-uncommitted third frame on
        // disk: reads must expose exactly the committed prefix.
        let l1 = b"line-one-aaaaaaaa";
        let l2 = b"line-two-bbbbbbb";
        let mut data = lines_data(&[l1, l2]);
        let committed = data.len() as u64;
        data.extend_from_slice(&frame(2, 3, 0, b"uncommitted-frame"));
        let (root, store) = fixture("committed-prefix", &data, committed, 2, 3).await;

        let got = expect(
            &store,
            &root,
            ReadCursor {
                line: 1,
                byte_offset: 0,
            },
            500,
        )
        .await;
        let want: Vec<u8> = l1.iter().chain(l2).copied().collect();
        assert_eq!(got, want, "uncommitted frame leaked into the read");

        // Pinned at the committed end: empty, at end, no fabricated bytes.
        let rd = read_from(
            &store,
            &root,
            "t",
            ReadCursor {
                line: 2,
                byte_offset: l2.len() as u64,
            },
            50,
        )
        .await
        .unwrap();
        match rd {
            ReadOutcome::Data(d) => {
                assert!(d.bytes.is_empty());
                assert!(d.at_end);
            }
            _ => panic!("pinned cursor expired"),
        }

        // A file shorter than its committed boundary is rejected, not
        // silently narrowed.
        let mut truncated = data[..committed as usize].to_vec();
        truncated.truncate(committed as usize - 10);
        std::fs::write(root.join("seg-000001.log"), &truncated).unwrap();
        assert!(
            read_from(
                &store,
                &root,
                "t",
                ReadCursor {
                    line: 1,
                    byte_offset: 0
                },
                10
            )
            .await
            .is_err()
        );
        store.close().await;
        std::fs::remove_dir_all(&root).ok();
    }

    async fn expect(store: &Store, root: &Path, cursor: ReadCursor, max: usize) -> Vec<u8> {
        match read_from(store, root, "t", cursor, max).await.unwrap() {
            ReadOutcome::Data(d) => d.bytes,
            ReadOutcome::Expired(e) => panic!("unexpected expiry: {e}"),
        }
    }

    #[tokio::test]
    async fn cursor_beyond_end_of_line_rejected() {
        let l1 = b"line-one-aaaaaaaa";
        let l2 = b"line-two-bbbbbbb";
        let data = lines_data(&[l1, l2]);
        let committed = data.len() as u64;
        let (root, store) = fixture("beyond-end", &data, committed, 2, 3).await;

        // end + 1 of an earlier line: explicit failure, never rounded
        // forward to "line consumed, continue at the next line".
        let err = read_from(
            &store,
            &root,
            "t",
            ReadCursor {
                line: 1,
                byte_offset: l1.len() as u64 + 1,
            },
            50,
        )
        .await;
        assert!(err.is_err(), "end+1 cursor silently accepted: {err:?}");

        // end + 1 of the last line: also an explicit failure.
        let err = read_from(
            &store,
            &root,
            "t",
            ReadCursor {
                line: 2,
                byte_offset: l2.len() as u64 + 1,
            },
            50,
        )
        .await;
        assert!(err.is_err(), "end+1 of the last line accepted: {err:?}");

        // Exact ends remain valid pinned cursors.
        let rd = read_from(
            &store,
            &root,
            "t",
            ReadCursor {
                line: 1,
                byte_offset: l1.len() as u64,
            },
            50,
        )
        .await
        .unwrap();
        match rd {
            ReadOutcome::Data(d) => assert_eq!(d.bytes, l2),
            _ => panic!("exact end cursor expired"),
        }
        store.close().await;
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn cursor_inside_unavailable_chunk_rejected() {
        // Line 2 is a two-chunk line, but only its first chunk is committed
        // (the committed boundary cuts the line). A cursor inside the second
        // chunk's range must fail explicitly instead of rounding to the next
        // available position.
        let l1 = b"line-one-aaaaaaaa";
        let l2a = b"chunk-aaaa";
        let l2b = b"chunk-bbbb";
        let mut data = vec![0u8; SEGMENT_HEADER_LEN];
        data.extend_from_slice(&frame(0, 1, 0, l1));
        data.extend_from_slice(&frame_mid(1, 2, 0, l2a));
        let committed = data.len() as u64;
        let l2_offset = committed - SEGMENT_HEADER_LEN as u64;
        data.extend_from_slice(&frame(2, 2, l2_offset, l2b));
        let (root, store) = fixture("missing-chunk", &data, committed, 2, 3).await;

        let err = read_from(
            &store,
            &root,
            "t",
            ReadCursor {
                line: 2,
                byte_offset: l2a.len() as u64 + 3,
            },
            50,
        )
        .await;
        assert!(err.is_err(), "cursor in the unavailable chunk accepted");

        // Exactly at the available end is a valid pinned cursor.
        let rd = read_from(
            &store,
            &root,
            "t",
            ReadCursor {
                line: 2,
                byte_offset: l2a.len() as u64,
            },
            50,
        )
        .await
        .unwrap();
        match rd {
            ReadOutcome::Data(d) => {
                assert!(d.bytes.is_empty());
                assert!(d.at_end);
            }
            _ => panic!("available-end cursor expired"),
        }
        store.close().await;
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn multibyte_line_offsets_are_byte_exact() {
        // One 90000-byte multibyte line (two chunks) plus a short line.
        let long: Vec<u8> = "\u{754c}".repeat(30000).into_bytes();
        let short = b"next-line";
        let data = lines_data(&[&long, short]);
        let committed = data.len() as u64;
        let (root, store) = fixture("multibyte", &data, committed, 2, 3).await;

        for off in [0u64, 3, 60000, 60003, 89997] {
            let got = expect(
                &store,
                &root,
                ReadCursor {
                    line: 1,
                    byte_offset: off,
                },
                100,
            )
            .await;
            // Reads continue across the frame and line boundary, so the
            // expectation spills into the next line exactly like the reader.
            let take_from_long = (long.len() - off as usize).min(100);
            let mut want: Vec<u8> = long[off as usize..off as usize + take_from_long].to_vec();
            if want.len() < 100 {
                let take = (100 - want.len()).min(short.len());
                want.extend_from_slice(&short[..take]);
            }
            assert_eq!(got, want, "offset {off} mismatch");
        }

        // Exact end of the multibyte line continues into the next line.
        let got = expect(
            &store,
            &root,
            ReadCursor {
                line: 1,
                byte_offset: long.len() as u64,
            },
            200,
        )
        .await;
        assert_eq!(got, short);

        // End + 1 is rejected even for multibyte lines.
        let err = read_from(
            &store,
            &root,
            "t",
            ReadCursor {
                line: 1,
                byte_offset: long.len() as u64 + 1,
            },
            10,
        )
        .await;
        assert!(err.is_err(), "multibyte end+1 accepted");
        store.close().await;
        std::fs::remove_dir_all(&root).ok();
    }
}
