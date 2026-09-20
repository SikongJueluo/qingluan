//! Pure on-disk codec for terminal log segment files (S2 foundation).
//!
//! Layout (all integers little-endian):
//!
//! Segment header (64 bytes, written once at file creation):
//!   `[0..4]` magic `QLSG`, `[4..6]` version u16 = 1, `[6]` kind, `[7]` flags,
//!   `[8..24]` terminal UUID16, `[24..40]` log epoch UUID16,
//!   `[40..48]` segment_id u64, `[48..56]` created_ms u64,
//!   `[56..60]` crc32 over bytes `0..56`, `[60..64]` reserved u32 = 0.
//!
//! Frame (40-byte header, payload, 4-byte payload crc32):
//!   `[0..4]` magic `QLFR`, `[4..6]` version u16 = 1, `[6]` kind,
//!   `[7]` flags (bit0 = last frame of the line), `[8..16]` frame_seq u64,
//!   `[16..24]` line u64, `[24..32]` line_offset u64,
//!   `[32..36]` payload_len u32 (<= MAX_PAYLOAD), `[36..40]` crc32 over `0..36`.
//!
//! Two stream kinds share the layout (the `kind` byte of both headers):
//! raw segments carry arbitrary output bytes (`FRAME_KIND_RAW` chunks,
//! addressed by stream byte offset in `line_offset`, `line` reserved 0),
//! normalized segments carry UTF-8 line text (`FRAME_KIND_LINE` chunks,
//! addressed by `(line, line_offset)`); a frame's kind must match its
//! segment's kind, so the two streams can never be mixed in one file.
//! Normalized payloads cut only on UTF-8 character boundaries; raw
//! payloads cut on arbitrary byte boundaries (including inside what
//! would be a character).
//!
//! All continuity arithmetic on scanned data is checked: `frame_seq` and
//! `line_offset` additions use `checked_add`, so u64 overflow classifies as
//! corruption instead of wrapping to an accepted value. Bounds are validated
//! before `payload_len` is ever used to allocate or slice.

pub(crate) const SEGMENT_MAGIC: [u8; 4] = *b"QLSG";
pub(crate) const FRAME_MAGIC: [u8; 4] = *b"QLFR";
pub(crate) const FORMAT_VERSION: u16 = 1;
pub(crate) const SEGMENT_HEADER_LEN: usize = 64;
pub(crate) const FRAME_HEADER_LEN: usize = 40;
/// Hard payload bound in bytes: `payload_len` above this is rejected before
/// any allocation or further parsing. This is the confirmed 64 KiB
/// persistence-policy bound for one frame payload.
pub const MAX_PAYLOAD: u32 = 64 * 1024;
pub(crate) const SEGMENT_KIND_RAW: u8 = 0x01;
pub(crate) const SEGMENT_KIND_NORMALIZED: u8 = 0x02;
pub(crate) const FRAME_KIND_RAW: u8 = 0x01;
pub(crate) const FRAME_KIND_LINE: u8 = 0x02;
pub(crate) const FRAME_FLAG_LINE_END: u8 = 0x01;

pub(crate) fn crc32(data: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(data);
    hasher.finalize()
}

fn put_u16(buf: &mut [u8], pos: usize, v: u16) {
    buf[pos..pos + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(buf: &mut [u8], pos: usize, v: u32) {
    buf[pos..pos + 4].copy_from_slice(&v.to_le_bytes());
}

fn put_u64(buf: &mut [u8], pos: usize, v: u64) {
    buf[pos..pos + 8].copy_from_slice(&v.to_le_bytes());
}

fn get_u16(buf: &[u8], pos: usize) -> u16 {
    u16::from_le_bytes(buf[pos..pos + 2].try_into().unwrap())
}

fn get_u32(buf: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap())
}

fn get_u64(buf: &[u8], pos: usize) -> u64 {
    u64::from_le_bytes(buf[pos..pos + 8].try_into().unwrap())
}

/// Failure of a header parse (segment or frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeaderError {
    TooShort,
    BadMagic,
    UnsupportedVersion(u16),
    CrcMismatch,
    BadReserved(u32),
}

impl std::fmt::Display for HeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HeaderError::TooShort => write!(f, "buffer shorter than header"),
            HeaderError::BadMagic => write!(f, "bad magic"),
            HeaderError::UnsupportedVersion(version) => {
                write!(f, "unsupported version {version}")
            }
            HeaderError::CrcMismatch => write!(f, "header crc mismatch"),
            HeaderError::BadReserved(reserved) => {
                write!(f, "reserved field not zero: {reserved}")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SegmentHeader {
    pub kind: u8,
    pub flags: u8,
    pub terminal: [u8; 16],
    pub epoch: [u8; 16],
    pub segment_id: u64,
    pub created_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FrameHeader {
    pub kind: u8,
    pub flags: u8,
    pub frame_seq: u64,
    pub line: u64,
    pub line_offset: u64,
    pub payload_len: u32,
}

impl SegmentHeader {
    pub(crate) fn encode(&self) -> [u8; SEGMENT_HEADER_LEN] {
        let mut buf = [0u8; SEGMENT_HEADER_LEN];
        buf[0..4].copy_from_slice(&SEGMENT_MAGIC);
        put_u16(&mut buf, 4, FORMAT_VERSION);
        buf[6] = self.kind;
        buf[7] = self.flags;
        buf[8..24].copy_from_slice(&self.terminal);
        buf[24..40].copy_from_slice(&self.epoch);
        put_u64(&mut buf, 40, self.segment_id);
        put_u64(&mut buf, 48, self.created_ms);
        let header_crc = crc32(&buf[0..56]);
        put_u32(&mut buf, 56, header_crc);
        // [60..64] reserved, left zero.
        buf
    }

    pub(crate) fn parse(buf: &[u8]) -> Result<SegmentHeader, HeaderError> {
        if buf.len() < SEGMENT_HEADER_LEN {
            return Err(HeaderError::TooShort);
        }
        if buf[0..4] != SEGMENT_MAGIC {
            return Err(HeaderError::BadMagic);
        }
        let version = get_u16(buf, 4);
        if version != FORMAT_VERSION {
            return Err(HeaderError::UnsupportedVersion(version));
        }
        let reserved = get_u32(buf, 60);
        if reserved != 0 {
            return Err(HeaderError::BadReserved(reserved));
        }
        let stored = get_u32(buf, 56);
        if crc32(&buf[0..56]) != stored {
            return Err(HeaderError::CrcMismatch);
        }
        Ok(SegmentHeader {
            kind: buf[6],
            flags: buf[7],
            terminal: buf[8..24].try_into().unwrap(),
            epoch: buf[24..40].try_into().unwrap(),
            segment_id: get_u64(buf, 40),
            created_ms: get_u64(buf, 48),
        })
    }
}

impl FrameHeader {
    fn encode_into(&self, buf: &mut [u8; FRAME_HEADER_LEN]) {
        buf[0..4].copy_from_slice(&FRAME_MAGIC);
        put_u16(buf, 4, FORMAT_VERSION);
        buf[6] = self.kind;
        buf[7] = self.flags;
        put_u64(buf, 8, self.frame_seq);
        put_u64(buf, 16, self.line);
        put_u64(buf, 24, self.line_offset);
        put_u32(buf, 32, self.payload_len);
        let header_crc = crc32(&buf[0..36]);
        put_u32(buf, 36, header_crc);
    }

    pub(crate) fn parse(buf: &[u8]) -> Result<FrameHeader, HeaderError> {
        if buf.len() < FRAME_HEADER_LEN {
            return Err(HeaderError::TooShort);
        }
        if buf[0..4] != FRAME_MAGIC {
            return Err(HeaderError::BadMagic);
        }
        let version = get_u16(buf, 4);
        if version != FORMAT_VERSION {
            return Err(HeaderError::UnsupportedVersion(version));
        }
        let stored = get_u32(buf, 36);
        if crc32(&buf[0..36]) != stored {
            return Err(HeaderError::CrcMismatch);
        }
        Ok(FrameHeader {
            kind: buf[6],
            flags: buf[7],
            frame_seq: get_u64(buf, 8),
            line: get_u64(buf, 16),
            line_offset: get_u64(buf, 24),
            payload_len: get_u32(buf, 32),
        })
    }
}

/// Failure of frame encoding (caller invariant violation; never panics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EncodeError {
    PayloadTooLarge { len: usize },
    LengthMismatch { header: u32, actual: usize },
}

/// Encode one complete frame (header + payload + payload crc32). The payload
/// bound is checked before any encoding work.
pub(crate) fn encode_frame(header: &FrameHeader, payload: &[u8]) -> Result<Vec<u8>, EncodeError> {
    let len = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    if len > MAX_PAYLOAD {
        return Err(EncodeError::PayloadTooLarge { len: payload.len() });
    }
    if header.payload_len != len {
        return Err(EncodeError::LengthMismatch {
            header: header.payload_len,
            actual: payload.len(),
        });
    }
    let mut out = Vec::with_capacity(FRAME_HEADER_LEN + payload.len() + 4);
    let mut hdr = [0u8; FRAME_HEADER_LEN];
    header.encode_into(&mut hdr);
    out.extend_from_slice(&hdr);
    out.extend_from_slice(payload);
    let payload_crc = crc32(payload);
    out.extend_from_slice(&payload_crc.to_le_bytes());
    Ok(out)
}

/// One validated frame located inside a scanned buffer.
#[derive(Debug, Clone)]
pub(crate) struct ScannedFrame {
    pub header: FrameHeader,
    /// Offset of the payload within the buffer.
    pub payload_at: usize,
    /// Exclusive end offset (after the payload crc32).
    pub end: usize,
}

/// Outcome of scanning a frame sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScanOutcome {
    /// The scanned region is an exact sequence of valid frames.
    Clean,
    /// The region ends inside a frame (truncated header, payload or crc).
    PartialTail,
    /// A structurally complete frame failed validation at the given offset.
    CorruptAt(usize, &'static str),
}

#[derive(Debug, Clone)]
pub(crate) struct ScanReport {
    pub frames: Vec<ScannedFrame>,
    pub outcome: ScanOutcome,
}

/// Scan a frame sequence starting at `from` inside a segment of the
/// given `kind` (see [`SEGMENT_KIND_RAW`] / [`SEGMENT_KIND_NORMALIZED`]).
///
/// Every frame's kind must equal the segment's kind; unknown segment
/// kinds are corruption. Bounds are validated before any use of
/// `payload_len`, and continuity runs on checked arithmetic: `frame_seq`
/// increases by exactly one per frame (segment starts at 0); normalized
/// line frames follow the line rules (lines are 1-based — line 0 is the
/// raw stream's reserved value; every new line is exactly
/// `previous.line + 1`, so jumps fabricating continuity classify as
/// corruption; every new line starts at offset 0 and may only follow a
/// frame carrying [`FRAME_FLAG_LINE_END`]; same-line chunks continue at
/// exactly `previous_offset + previous_payload_len`); raw frames instead
/// carry the stream byte offset in `line_offset` with `line` reserved to
/// 0 and no flags, and continue at exactly `previous_offset +
/// previous_payload_len`. A region starting at [`SEGMENT_HEADER_LEN`] is
/// the segment's first frame (`frame_seq` 0, and for normalized segments
/// line offset 0); regions starting later (recovery tail scans) may begin
/// mid-stream, so their first frame is unconstrained except by kind.
pub(crate) fn scan_frames(data: &[u8], from: usize, segment_kind: u8) -> ScanReport {
    let mut frames = Vec::new();
    let mut off = from;
    let at_segment_start = from == SEGMENT_HEADER_LEN;
    let raw_stream = match segment_kind {
        SEGMENT_KIND_RAW => true,
        SEGMENT_KIND_NORMALIZED => false,
        _ => {
            return ScanReport {
                frames,
                outcome: ScanOutcome::CorruptAt(from, "unknown segment kind"),
            };
        }
    };
    let expected_kind = if raw_stream {
        FRAME_KIND_RAW
    } else {
        FRAME_KIND_LINE
    };
    let mut prev: Option<FrameHeader> = None;
    while off < data.len() {
        let remaining = data.len() - off;
        if remaining < FRAME_HEADER_LEN {
            return ScanReport {
                frames,
                outcome: ScanOutcome::PartialTail,
            };
        }
        let header = match FrameHeader::parse(&data[off..off + FRAME_HEADER_LEN]) {
            Ok(h) => h,
            Err(HeaderError::BadMagic) => {
                return ScanReport {
                    frames,
                    outcome: ScanOutcome::CorruptAt(off, "bad frame magic"),
                };
            }
            Err(HeaderError::UnsupportedVersion(_)) => {
                return ScanReport {
                    frames,
                    outcome: ScanOutcome::CorruptAt(off, "unsupported frame version"),
                };
            }
            Err(HeaderError::CrcMismatch) => {
                return ScanReport {
                    frames,
                    outcome: ScanOutcome::CorruptAt(off, "frame header crc mismatch"),
                };
            }
            Err(HeaderError::TooShort) | Err(HeaderError::BadReserved(_)) => {
                return ScanReport {
                    frames,
                    outcome: ScanOutcome::CorruptAt(off, "malformed frame header"),
                };
            }
        };
        if header.payload_len > MAX_PAYLOAD {
            return ScanReport {
                frames,
                outcome: ScanOutcome::CorruptAt(off, "payload_len exceeds 64 KiB bound"),
            };
        }
        if header.kind != expected_kind {
            return ScanReport {
                frames,
                outcome: ScanOutcome::CorruptAt(off, "frame kind does not match segment kind"),
            };
        }
        if raw_stream {
            // Raw frames are byte-addressed: no line number, no line-end
            // flag; anything else would let raw bytes masquerade as line
            // positions (or vice versa).
            if header.line != 0 {
                return ScanReport {
                    frames,
                    outcome: ScanOutcome::CorruptAt(off, "raw frame carries a line number"),
                };
            }
            if header.flags != 0 {
                return ScanReport {
                    frames,
                    outcome: ScanOutcome::CorruptAt(off, "raw frame carries line flags"),
                };
            }
        } else {
            if header.flags & !FRAME_FLAG_LINE_END != 0 {
                return ScanReport {
                    frames,
                    outcome: ScanOutcome::CorruptAt(off, "unknown frame flags"),
                };
            }
            // Lines are 1-based: line 0 is the raw stream's reserved
            // value and can never address normalized text.
            if header.line == 0 {
                return ScanReport {
                    frames,
                    outcome: ScanOutcome::CorruptAt(off, "normalized frame carries line 0"),
                };
            }
        }
        let need = FRAME_HEADER_LEN + header.payload_len as usize + 4;
        if remaining < need {
            return ScanReport {
                frames,
                outcome: ScanOutcome::PartialTail,
            };
        }
        let payload_at = off + FRAME_HEADER_LEN;
        let end = off + need;
        let stored = get_u32(data, end - 4);
        if crc32(&data[payload_at..end - 4]) != stored {
            return ScanReport {
                frames,
                outcome: ScanOutcome::CorruptAt(off, "payload crc mismatch"),
            };
        }
        match prev {
            None if at_segment_start => {
                if header.frame_seq != 0 {
                    return ScanReport {
                        frames,
                        outcome: ScanOutcome::CorruptAt(off, "segment must start at frame_seq 0"),
                    };
                }
                if !raw_stream && header.line_offset != 0 {
                    return ScanReport {
                        frames,
                        outcome: ScanOutcome::CorruptAt(
                            off,
                            "segment first frame must start at line offset 0",
                        ),
                    };
                }
                // A raw segment may start at any stream byte offset (it
                // continues the stream after rotation), so only frame_seq 0
                // is pinned at its start.
            }
            Some(p) => {
                // Continuity arithmetic runs on untrusted tail data: after a
                // previous frame_seq of u64::MAX no successor can be
                // consecutive, so overflow classifies as corruption instead
                // of wrapping to a bogus accepted value.
                let seq_consecutive = p
                    .frame_seq
                    .checked_add(1)
                    .is_some_and(|next| header.frame_seq == next);
                if !seq_consecutive {
                    return ScanReport {
                        frames,
                        outcome: ScanOutcome::CorruptAt(off, "frame_seq not consecutive"),
                    };
                }
                if raw_stream {
                    // `line_offset + payload_len` also comes from untrusted
                    // data; if the expected continuation offset would pass
                    // u64::MAX no chunk can validly continue the stream.
                    let offset_continues = p
                        .line_offset
                        .checked_add(u64::from(p.payload_len))
                        .is_some_and(|expected| header.line_offset == expected);
                    if !offset_continues {
                        return ScanReport {
                            frames,
                            outcome: ScanOutcome::CorruptAt(off, "raw chunk offset discontinuity"),
                        };
                    }
                } else if header.line < p.line {
                    return ScanReport {
                        frames,
                        outcome: ScanOutcome::CorruptAt(off, "line went backwards"),
                    };
                } else if header.line == p.line {
                    if p.flags & FRAME_FLAG_LINE_END != 0 {
                        return ScanReport {
                            frames,
                            outcome: ScanOutcome::CorruptAt(
                                off,
                                "line continued past its line-end frame",
                            ),
                        };
                    }
                    // `line_offset + payload_len` also comes from untrusted
                    // data; if the expected continuation offset would pass
                    // u64::MAX no chunk can validly continue the line.
                    let offset_continues = p
                        .line_offset
                        .checked_add(u64::from(p.payload_len))
                        .is_some_and(|expected| header.line_offset == expected);
                    if !offset_continues {
                        return ScanReport {
                            frames,
                            outcome: ScanOutcome::CorruptAt(
                                off,
                                "same-line chunk offset discontinuity",
                            ),
                        };
                    }
                } else {
                    if p.flags & FRAME_FLAG_LINE_END == 0 {
                        return ScanReport {
                            frames,
                            outcome: ScanOutcome::CorruptAt(
                                off,
                                "new line before the previous line ended",
                            ),
                        };
                    }
                    if header.line_offset != 0 {
                        return ScanReport {
                            frames,
                            outcome: ScanOutcome::CorruptAt(
                                off,
                                "new line does not start at offset 0",
                            ),
                        };
                    }
                    // A jump (line 1 -> line 3) would fabricate continuity
                    // without an explicit gap; the next line must be exactly
                    // `previous + 1`, and after a line number of u64::MAX no
                    // successor can be consecutive (overflow classifies as
                    // corruption, never wraps).
                    if p.line.checked_add(1) != Some(header.line) {
                        return ScanReport {
                            frames,
                            outcome: ScanOutcome::CorruptAt(
                                off,
                                "line number is not previous line + 1",
                            ),
                        };
                    }
                }
            }
            None => {}
        }
        prev = Some(header.clone());
        frames.push(ScannedFrame {
            header,
            payload_at,
            end,
        });
        off = end;
    }
    ScanReport {
        frames,
        outcome: ScanOutcome::Clean,
    }
}

/// Split a normalized UTF-8 line into frame payload chunks of at most
/// [`MAX_PAYLOAD`] bytes, cutting only on character boundaries. Line numbers
/// are never reused and chunk offsets are strictly increasing, so consumers
/// can reassemble the line from (line, line_offset) ordering.
pub(crate) fn split_line(line: &str) -> Vec<&[u8]> {
    let bytes = line.as_bytes();
    if bytes.is_empty() {
        return vec![bytes];
    }
    let max = MAX_PAYLOAD as usize;
    let mut out = Vec::new();
    let mut start = 0usize;
    while bytes.len() - start > max {
        // Back up to the nearest character start; a UTF-8 char is at most 4
        // bytes and max >= 4, so the cut always stays inside (start, start+max].
        let mut cut = start + max;
        while cut > start && (bytes[cut] & 0xC0) == 0x80 {
            cut -= 1;
        }
        out.push(&bytes[start..cut]);
        start = cut;
    }
    out.push(&bytes[start..]);
    out
}

/// Split a raw byte append into frame payload chunks of at most
/// [`MAX_PAYLOAD`] bytes. Raw payloads have no character boundary to
/// respect: the cut is purely at the byte bound, so arbitrary bytes
/// (NUL, invalid UTF-8, split multibyte sequences) round-trip exactly.
pub(crate) fn split_raw(bytes: &[u8]) -> Vec<&[u8]> {
    if bytes.is_empty() {
        return vec![bytes];
    }
    bytes.chunks(MAX_PAYLOAD as usize).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_header() -> SegmentHeader {
        SegmentHeader {
            kind: SEGMENT_KIND_NORMALIZED,
            flags: 0,
            terminal: [1u8; 16],
            epoch: [2u8; 16],
            segment_id: 7,
            created_ms: 1_700_000_000_123,
        }
    }

    #[test]
    fn segment_header_roundtrip() {
        let enc = sample_header().encode();
        assert_eq!(enc.len(), SEGMENT_HEADER_LEN);
        assert_eq!(&enc[0..4], b"QLSG");
        let dec = SegmentHeader::parse(&enc).unwrap();
        assert_eq!(dec, sample_header());
    }

    #[test]
    fn segment_header_rejects_tampering() {
        let mut enc = sample_header().encode();
        enc[41] ^= 0xFF; // inside segment_id / crc region
        assert_eq!(SegmentHeader::parse(&enc), Err(HeaderError::CrcMismatch));
        let mut bad_magic = sample_header().encode();
        bad_magic[0] = b'X';
        assert_eq!(SegmentHeader::parse(&bad_magic), Err(HeaderError::BadMagic));
        let mut bad_version = sample_header().encode();
        bad_version[4] = 9;
        bad_version[5] = 0;
        // fix crc so the failure is the version, not the checksum
        let fixed_crc = crc32(&bad_version[0..56]);
        put_u32(&mut bad_version, 56, fixed_crc);
        assert_eq!(
            SegmentHeader::parse(&bad_version),
            Err(HeaderError::UnsupportedVersion(9))
        );
        let mut bad_reserved = sample_header().encode();
        put_u32(&mut bad_reserved, 60, 7);
        let fixed_crc = crc32(&bad_reserved[0..56]);
        put_u32(&mut bad_reserved, 56, fixed_crc);
        assert_eq!(
            SegmentHeader::parse(&bad_reserved),
            Err(HeaderError::BadReserved(7))
        );
        assert_eq!(SegmentHeader::parse(&enc[..32]), Err(HeaderError::TooShort));
    }

    fn frame(seq: u64, line: u64, offset: u64, payload: &[u8], flags: u8) -> Vec<u8> {
        let header = FrameHeader {
            kind: FRAME_KIND_LINE,
            flags,
            frame_seq: seq,
            line,
            line_offset: offset,
            payload_len: payload.len() as u32,
        };
        encode_frame(&header, payload).unwrap()
    }

    #[test]
    fn encode_rejects_oversized_and_mismatched_payloads() {
        let big = vec![b'x'; MAX_PAYLOAD as usize + 1];
        let header = FrameHeader {
            kind: FRAME_KIND_LINE,
            flags: FRAME_FLAG_LINE_END,
            frame_seq: 0,
            line: 1,
            line_offset: 0,
            payload_len: u32::try_from(big.len()).unwrap(),
        };
        assert_eq!(
            encode_frame(&header, &big),
            Err(EncodeError::PayloadTooLarge { len: big.len() })
        );
        let header = FrameHeader {
            payload_len: 3,
            ..header
        };
        assert_eq!(
            encode_frame(&header, b"abcd"),
            Err(EncodeError::LengthMismatch {
                header: 3,
                actual: 4
            })
        );
    }

    #[test]
    fn frame_roundtrip_and_scan_clean() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&frame(0, 1, 0, b"hello", FRAME_FLAG_LINE_END));
        buf.extend_from_slice(&frame(1, 2, 0, "世界".as_bytes(), FRAME_FLAG_LINE_END));
        let report = scan_frames(&buf, 0, SEGMENT_KIND_NORMALIZED);
        assert_eq!(report.outcome, ScanOutcome::Clean);
        assert_eq!(report.frames.len(), 2);
        assert_eq!(report.frames[0].header.line, 1);
        assert_eq!(
            &buf[report.frames[1].payload_at..report.frames[1].end - 4],
            "世界".as_bytes()
        );
        assert_eq!(
            report.frames[1].header.flags & FRAME_FLAG_LINE_END,
            FRAME_FLAG_LINE_END
        );
    }

    #[test]
    fn partial_tail_detected() {
        let f = frame(0, 1, 0, b"hello world", 0);
        let report = scan_frames(&f[..f.len() - 3], 0, SEGMENT_KIND_NORMALIZED);
        assert_eq!(report.outcome, ScanOutcome::PartialTail);
        // truncated before the full header too
        assert_eq!(
            scan_frames(&f[..10], 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::PartialTail
        );
    }

    #[test]
    fn payload_crc_tamper_detected() {
        let mut f = frame(0, 1, 0, b"hello world", 0);
        let n = f.len();
        f[n - 5] ^= 0xFF;
        let report = scan_frames(&f, 0, SEGMENT_KIND_NORMALIZED);
        assert_eq!(
            report.outcome,
            ScanOutcome::CorruptAt(0, "payload crc mismatch")
        );
    }

    #[test]
    fn frame_header_crc_and_magic_tamper_detected() {
        let mut f = frame(0, 1, 0, b"hello", FRAME_FLAG_LINE_END);
        f[8] ^= 0xFF; // inside frame_seq, covered by the header crc
        assert_eq!(
            scan_frames(&f, 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::CorruptAt(0, "frame header crc mismatch")
        );
        let mut f = frame(0, 1, 0, b"hello", FRAME_FLAG_LINE_END);
        f[0] = b'X';
        assert_eq!(
            scan_frames(&f, 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::CorruptAt(0, "bad frame magic")
        );
    }

    #[test]
    fn oversized_payload_len_rejected_without_allocation() {
        // Build a header claiming a huge payload; the scanner must classify
        // it as corruption purely from the bounded length field.
        let mut f = frame(0, 1, 0, b"x", 0);
        put_u32(&mut f, 32, MAX_PAYLOAD + 1);
        let fixed_crc = crc32(&f[0..36]);
        put_u32(&mut f, 36, fixed_crc);
        let report = scan_frames(&f, 0, SEGMENT_KIND_NORMALIZED);
        assert_eq!(
            report.outcome,
            ScanOutcome::CorruptAt(0, "payload_len exceeds 64 KiB bound")
        );
    }

    #[test]
    fn sequence_rules_enforced() {
        // frame_seq skipped or repeated: the rule breaks at the second frame.
        let mut buf = frame(0, 1, 0, b"a", FRAME_FLAG_LINE_END);
        buf.extend_from_slice(&frame(2, 2, 0, b"b", FRAME_FLAG_LINE_END));
        let second = FRAME_HEADER_LEN + 1 + 4;
        assert_eq!(
            scan_frames(&buf, 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::CorruptAt(second, "frame_seq not consecutive")
        );

        let mut buf = frame(5, 1, 0, b"a", FRAME_FLAG_LINE_END);
        buf.extend_from_slice(&frame(5, 2, 0, b"b", FRAME_FLAG_LINE_END));
        assert_eq!(
            scan_frames(&buf, 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::CorruptAt(second, "frame_seq not consecutive")
        );

        // same-line chunk offset does not continue the previous payload.
        let mut buf = frame(0, 3, 0, b"ab", 0);
        buf.extend_from_slice(&frame(1, 3, 8, b"c", FRAME_FLAG_LINE_END));
        let second_ab = FRAME_HEADER_LEN + 2 + 4;
        assert_eq!(
            scan_frames(&buf, 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::CorruptAt(second_ab, "same-line chunk offset discontinuity")
        );

        // line went backwards.
        let mut buf = frame(0, 4, 0, b"a", FRAME_FLAG_LINE_END);
        buf.extend_from_slice(&frame(1, 3, 0, b"b", FRAME_FLAG_LINE_END));
        assert_eq!(
            scan_frames(&buf, 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::CorruptAt(second, "line went backwards")
        );
    }

    #[test]
    fn line_numbers_must_advance_by_exactly_one_from_one() {
        // A jump (line 1 -> line 3) fabricates continuity without a gap.
        let mut buf = frame(0, 1, 0, b"a", FRAME_FLAG_LINE_END);
        buf.extend_from_slice(&frame(1, 3, 0, b"b", FRAME_FLAG_LINE_END));
        let second = FRAME_HEADER_LEN + 1 + 4;
        assert_eq!(
            scan_frames(&buf, 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::CorruptAt(second, "line number is not previous line + 1")
        );

        // Line 0 is the raw stream's reserved value.
        let buf = frame(0, 0, 0, b"a", FRAME_FLAG_LINE_END);
        assert_eq!(
            scan_frames(&buf, 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::CorruptAt(0, "normalized frame carries line 0")
        );

        // The overflow neighbor: after a line number of u64::MAX no
        // successor line can exist (never wraps to 0).
        let mut buf = frame(0, u64::MAX - 1, 0, b"a", FRAME_FLAG_LINE_END);
        buf.extend_from_slice(&frame(1, 0, 0, b"b", FRAME_FLAG_LINE_END));
        let second_a = FRAME_HEADER_LEN + 1 + 4;
        assert_eq!(
            scan_frames(&buf, 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::CorruptAt(second_a, "normalized frame carries line 0")
        );

        // Consecutive lines stay clean.
        let mut buf = frame(0, 1, 0, b"a", FRAME_FLAG_LINE_END);
        buf.extend_from_slice(&frame(1, 2, 0, b"b", FRAME_FLAG_LINE_END));
        assert_eq!(
            scan_frames(&buf, 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::Clean
        );
    }

    #[test]
    fn line_continuity_rules_reject_each_discontinuity() {
        // A line that continues after its line-end frame.
        let mut buf = frame(0, 1, 0, b"ab", FRAME_FLAG_LINE_END);
        buf.extend_from_slice(&frame(1, 1, 2, b"c", FRAME_FLAG_LINE_END));
        let second = FRAME_HEADER_LEN + 2 + 4;
        assert_eq!(
            scan_frames(&buf, 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::CorruptAt(second, "line continued past its line-end frame")
        );

        // A new line that starts before the previous line ended.
        let mut buf = frame(0, 1, 0, b"ab", 0);
        buf.extend_from_slice(&frame(1, 2, 0, b"c", FRAME_FLAG_LINE_END));
        assert_eq!(
            scan_frames(&buf, 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::CorruptAt(second, "new line before the previous line ended")
        );

        // A new line that does not start at offset 0.
        let mut buf = frame(0, 1, 0, b"ab", FRAME_FLAG_LINE_END);
        buf.extend_from_slice(&frame(1, 2, 3, b"c", FRAME_FLAG_LINE_END));
        assert_eq!(
            scan_frames(&buf, 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::CorruptAt(second, "new line does not start at offset 0")
        );

        // A multi-chunk line plus a following line stays clean.
        let mut buf = frame(0, 1, 0, b"ab", 0);
        buf.extend_from_slice(&frame(1, 1, 2, b"cde", FRAME_FLAG_LINE_END));
        buf.extend_from_slice(&frame(2, 2, 0, b"next", FRAME_FLAG_LINE_END));
        assert_eq!(
            scan_frames(&buf, 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::Clean
        );
    }

    #[test]
    fn segment_start_must_begin_at_seq_zero_offset_zero() {
        // The region starts at the segment header, so the first frame is the
        // segment's first frame: nonzero line offset is a discontinuity.
        let mut buf = vec![0u8; SEGMENT_HEADER_LEN];
        buf.extend_from_slice(&frame(0, 1, 5, b"a", FRAME_FLAG_LINE_END));
        assert_eq!(
            scan_frames(&buf, SEGMENT_HEADER_LEN, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::CorruptAt(
                SEGMENT_HEADER_LEN,
                "segment first frame must start at line offset 0"
            )
        );

        // Nonzero starting frame_seq is a discontinuity too.
        let mut buf = vec![0u8; SEGMENT_HEADER_LEN];
        buf.extend_from_slice(&frame(3, 1, 0, b"a", FRAME_FLAG_LINE_END));
        assert_eq!(
            scan_frames(&buf, SEGMENT_HEADER_LEN, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::CorruptAt(SEGMENT_HEADER_LEN, "segment must start at frame_seq 0")
        );

        // A region starting later (recovery tail scan) may begin mid-stream.
        let mut buf = frame(0, 1, 0, b"a", FRAME_FLAG_LINE_END);
        buf.extend_from_slice(&frame(7, 5, 21, b"b", 0));
        buf.extend_from_slice(&frame(8, 5, 22, b"c", FRAME_FLAG_LINE_END));
        let tail_start = FRAME_HEADER_LEN + 1 + 4;
        assert_eq!(
            scan_frames(&buf, tail_start, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::Clean
        );
    }

    #[test]
    fn split_line_cuts_only_on_char_boundaries() {
        // Multibyte tail straddling the 64 KiB cut.
        let s = "a".repeat(MAX_PAYLOAD as usize - 10) + &"界".repeat(20);
        let chunks = split_line(&s);
        assert!(chunks.len() >= 2);
        let mut pos = 0usize;
        for chunk in &chunks {
            assert!(chunk.len() <= MAX_PAYLOAD as usize);
            // chunk must start and end on a char boundary
            let bytes = s.as_bytes();
            assert!(
                (bytes[pos] & 0xC0) != 0x80,
                "chunk start not on char boundary"
            );
            assert_eq!(&bytes[pos..pos + chunk.len()], *chunk);
            pos += chunk.len();
        }
        assert_eq!(pos, s.len());

        // Maximal multibyte stress: 4-byte chars straddling the cut.
        let t = "😀".repeat(MAX_PAYLOAD as usize / 2 + 3);
        for chunk in split_line(&t) {
            assert!(chunk.len() <= MAX_PAYLOAD as usize);
            assert!(
                std::str::from_utf8(chunk).is_ok(),
                "chunk split inside a character"
            );
        }

        // empty and short lines
        assert_eq!(split_line(""), vec![&b""[..]]);
        assert_eq!(split_line("abc").len(), 1);
    }

    #[test]
    fn frame_seq_overflow_is_corruption_not_wraparound() {
        // First frame of an unconstrained tail region at u64::MAX, followed
        // by frame_seq 0: `prev + 1` must not wrap to 0 and accept.
        let mut buf = frame(u64::MAX, 1, 0, b"a", FRAME_FLAG_LINE_END);
        buf.extend_from_slice(&frame(0, 2, 0, b"b", FRAME_FLAG_LINE_END));
        let second = FRAME_HEADER_LEN + 1 + 4;
        assert_eq!(
            scan_frames(&buf, 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::CorruptAt(second, "frame_seq not consecutive")
        );

        // The chain u64::MAX-1 -> u64::MAX is consecutive, then the overflow
        // hits the NEXT frame after the u64::MAX one.
        let mut buf = frame(0, 1, 0, b"a", FRAME_FLAG_LINE_END);
        buf.extend_from_slice(&frame(u64::MAX - 1, 2, 0, b"b", FRAME_FLAG_LINE_END));
        buf.extend_from_slice(&frame(u64::MAX, 3, 0, b"c", FRAME_FLAG_LINE_END));
        buf.extend_from_slice(&frame(0, 4, 0, b"d", FRAME_FLAG_LINE_END));
        let tail_start = FRAME_HEADER_LEN + 1 + 4;
        let third_in_region = 3 * (FRAME_HEADER_LEN + 1 + 4);
        assert_eq!(
            scan_frames(&buf, tail_start, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::CorruptAt(third_in_region, "frame_seq not consecutive")
        );

        // A tail that merely ends on frame_seq u64::MAX stays clean.
        let buf = frame(u64::MAX, 1, 0, b"a", FRAME_FLAG_LINE_END);
        assert_eq!(
            scan_frames(&buf, 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::Clean
        );
    }

    #[test]
    fn line_offset_overflow_is_corruption_not_wraparound() {
        // Same-line chunk after line_offset u64::MAX - 1 with a 2-byte
        // payload: the expected continuation offset overflows u64.
        let mut buf = frame(0, 1, u64::MAX - 1, b"ab", 0);
        buf.extend_from_slice(&frame(1, 1, 0, b"c", FRAME_FLAG_LINE_END));
        let second_ab = FRAME_HEADER_LEN + 2 + 4;
        assert_eq!(
            scan_frames(&buf, 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::CorruptAt(second_ab, "same-line chunk offset discontinuity")
        );

        // Exactly u64::MAX with a nonzero payload: no offset can ever
        // continue that line.
        let mut buf = frame(0, 1, u64::MAX, b"x", 0);
        buf.extend_from_slice(&frame(1, 1, 0, b"c", FRAME_FLAG_LINE_END));
        let second_x = FRAME_HEADER_LEN + 1 + 4;
        assert_eq!(
            scan_frames(&buf, 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::CorruptAt(second_x, "same-line chunk offset discontinuity")
        );

        // A tail that merely ends on the overflowing frame stays clean.
        let buf = frame(0, 1, u64::MAX, b"x", FRAME_FLAG_LINE_END);
        assert_eq!(
            scan_frames(&buf, 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::Clean
        );
    }

    #[test]
    fn scan_from_offset_skips_prefix() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&frame(0, 1, 0, b"a", FRAME_FLAG_LINE_END));
        buf.extend_from_slice(&frame(1, 2, 0, b"b", FRAME_FLAG_LINE_END));
        let second_start = FRAME_HEADER_LEN + 1 + 4;
        assert_eq!(buf.len(), second_start + FRAME_HEADER_LEN + 1 + 4);
        let tail = scan_frames(&buf, second_start, SEGMENT_KIND_NORMALIZED);
        assert_eq!(tail.outcome, ScanOutcome::Clean);
        assert_eq!(tail.frames.len(), 1);
        assert_eq!(tail.frames[0].header.line, 2);
    }

    fn raw_frame(seq: u64, offset: u64, payload: &[u8]) -> Vec<u8> {
        let header = FrameHeader {
            kind: FRAME_KIND_RAW,
            flags: 0,
            frame_seq: seq,
            line: 0,
            line_offset: offset,
            payload_len: payload.len() as u32,
        };
        encode_frame(&header, payload).unwrap()
    }

    #[test]
    fn raw_frames_scan_clean_with_arbitrary_bytes() {
        // NUL, invalid UTF-8, and a torn multibyte sequence: raw payloads
        // are opaque bytes and round-trip exactly.
        let payload = b"\x00\xff\xfe\xc3raw\x28";
        let mut buf = Vec::new();
        buf.extend_from_slice(&raw_frame(0, 7, payload));
        buf.extend_from_slice(&raw_frame(1, 7 + payload.len() as u64, b"more"));
        let report = scan_frames(&buf, 0, SEGMENT_KIND_RAW);
        assert_eq!(report.outcome, ScanOutcome::Clean);
        assert_eq!(report.frames.len(), 2);
        assert_eq!(
            &buf[report.frames[0].payload_at..report.frames[0].end - 4],
            payload
        );
        assert_eq!(
            report.frames[1].header.line_offset,
            7 + payload.len() as u64
        );
        assert_eq!(report.frames[1].header.line, 0);
    }

    #[test]
    fn raw_segment_rules_reject_line_semantics_and_kind_mixing() {
        // A raw frame carrying a line number.
        let mut f = raw_frame(0, 0, b"a");
        put_u64(&mut f, 16, 3);
        let fixed_crc = crc32(&f[0..36]);
        put_u32(&mut f, 36, fixed_crc);
        assert_eq!(
            scan_frames(&f, 0, SEGMENT_KIND_RAW).outcome,
            ScanOutcome::CorruptAt(0, "raw frame carries a line number")
        );

        // A raw frame carrying the line-end flag.
        let mut f = raw_frame(0, 0, b"a");
        f[7] = FRAME_FLAG_LINE_END;
        let fixed_crc = crc32(&f[0..36]);
        put_u32(&mut f, 36, fixed_crc);
        assert_eq!(
            scan_frames(&f, 0, SEGMENT_KIND_RAW).outcome,
            ScanOutcome::CorruptAt(0, "raw frame carries line flags")
        );

        // A line frame inside a raw segment (kind mixing).
        let f = frame(0, 1, 0, b"a", FRAME_FLAG_LINE_END);
        assert_eq!(
            scan_frames(&f, 0, SEGMENT_KIND_RAW).outcome,
            ScanOutcome::CorruptAt(0, "frame kind does not match segment kind")
        );

        // A raw frame inside a normalized segment.
        let f = raw_frame(0, 0, b"a");
        assert_eq!(
            scan_frames(&f, 0, SEGMENT_KIND_NORMALIZED).outcome,
            ScanOutcome::CorruptAt(0, "frame kind does not match segment kind")
        );

        // Unknown segment kind (including the retired 0 value).
        let f = raw_frame(0, 0, b"a");
        assert_eq!(
            scan_frames(&f, 0, 0).outcome,
            ScanOutcome::CorruptAt(0, "unknown segment kind")
        );
    }

    #[test]
    fn raw_chunk_offsets_must_continue_exactly() {
        // Offset discontinuity between raw chunks.
        let mut buf = raw_frame(0, 0, b"ab");
        buf.extend_from_slice(&raw_frame(1, 9, b"c"));
        let second_ab = FRAME_HEADER_LEN + 2 + 4;
        assert_eq!(
            scan_frames(&buf, 0, SEGMENT_KIND_RAW).outcome,
            ScanOutcome::CorruptAt(second_ab, "raw chunk offset discontinuity")
        );

        // Overflow: no offset can continue a chunk ending past u64::MAX.
        let mut buf = raw_frame(0, u64::MAX - 1, b"ab");
        buf.extend_from_slice(&raw_frame(1, 0, b"c"));
        assert_eq!(
            scan_frames(&buf, 0, SEGMENT_KIND_RAW).outcome,
            ScanOutcome::CorruptAt(second_ab, "raw chunk offset discontinuity")
        );

        // A raw segment may start at any stream offset (rotation continues
        // the stream): only frame_seq 0 is pinned at its start.
        let mut buf = vec![0u8; SEGMENT_HEADER_LEN];
        buf.extend_from_slice(&raw_frame(0, 4 * 1024 * 1024, b"a"));
        buf.extend_from_slice(&raw_frame(1, 4 * 1024 * 1024 + 1, b"b"));
        assert_eq!(
            scan_frames(&buf, SEGMENT_HEADER_LEN, SEGMENT_KIND_RAW).outcome,
            ScanOutcome::Clean
        );

        // But it still must start at frame_seq 0.
        let mut buf = vec![0u8; SEGMENT_HEADER_LEN];
        buf.extend_from_slice(&raw_frame(3, 0, b"a"));
        assert_eq!(
            scan_frames(&buf, SEGMENT_HEADER_LEN, SEGMENT_KIND_RAW).outcome,
            ScanOutcome::CorruptAt(SEGMENT_HEADER_LEN, "segment must start at frame_seq 0")
        );
    }

    #[test]
    fn split_raw_cuts_at_exact_byte_boundaries() {
        // Arbitrary bytes, including NUL and invalid UTF-8: the cut is at
        // the byte bound, never adjusted for character boundaries.
        let bytes: Vec<u8> = (0..2 * MAX_PAYLOAD + 7).map(|i| (i % 256) as u8).collect();
        let chunks = split_raw(&bytes);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), MAX_PAYLOAD as usize);
        assert_eq!(chunks[1].len(), MAX_PAYLOAD as usize);
        assert_eq!(chunks[2].len(), 7);
        let rejoined: Vec<u8> = chunks.concat();
        assert_eq!(rejoined, bytes);

        // A multibyte sequence straddling the cut is split mid-character
        // (raw has no character boundaries to respect).
        let straddling = vec![b'x'; MAX_PAYLOAD as usize - 1]
            .into_iter()
            .chain([0xE4, 0xB8, 0x96, 0x8C])
            .collect::<Vec<u8>>();
        let chunks = split_raw(&straddling);
        assert_eq!(chunks[0].last(), Some(&0xE4));
        assert_eq!(chunks[1], &[0xB8, 0x96, 0x8C]);

        // Empty and short inputs.
        assert_eq!(split_raw(&[]), vec![&b""[..]]);
        assert_eq!(split_raw(b"abc").len(), 1);
    }
}
