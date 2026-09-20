//! Stream-scoped gap coalescing (pure; no I/O).
//!
//! Gap records are stored per stream: a normalized gap covers 1-based
//! line numbers, a raw gap covers stream byte offsets, and the two kinds
//! never merge. Within one stream, adjacent or overlapping ranges
//! coalesce into one record (a hole of exactly zero lines/bytes between
//! two losses is one loss), and the merged list is capped at
//! [`crate::MAX_GAP_RECORDS`]: above the cap, conservative coarsening
//! merges the pair of consecutive gaps with the smallest hole between
//! them, declaring the swallowed intact range missing rather than
//! letting the row count grow unboundedly. Coarsening only ever widens
//! a gap, never narrows one.

/// Why a stream range was declared missing. Severity orders merges:
/// a merged record carries its worst member's reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GapReason {
    /// The indexed segment backing the range is absent.
    Missing,
    /// The backing file is shorter than its committed boundary.
    Truncated,
    /// The backing file or its row fails validation (header, identity,
    /// CRC, frame continuity).
    Corrupt,
}

impl GapReason {
    /// SQLite `reason` text of this cause.
    pub(crate) fn db_text(self) -> &'static str {
        match self {
            GapReason::Missing => "missing",
            GapReason::Truncated => "truncated",
            GapReason::Corrupt => "corrupt",
        }
    }

    /// The [`GapReason`] of a SQLite `reason` text, if known.
    pub(crate) fn from_db_text(text: &str) -> Option<Self> {
        match text {
            "missing" => Some(GapReason::Missing),
            "truncated" => Some(GapReason::Truncated),
            "corrupt" => Some(GapReason::Corrupt),
            _ => None,
        }
    }
}

/// One missing range of one stream: the half-open interval
/// `[start, end)` in that stream's coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GapSpan {
    /// First missing line (1-based) or byte offset.
    pub start: u64,
    /// One past the last missing line or byte offset (`end > start`).
    pub end: u64,
    /// Why the range is missing.
    pub reason: GapReason,
}

/// Coalesce `spans` (any order, possibly duplicated) into a sorted list
/// of disjoint gaps capped at `cap`. Merging keeps the worst member's
/// reason; coarsening beyond the cap merges the consecutive pair with
/// the smallest hole, swallowing at most that hole.
pub(crate) fn coalesce(mut spans: Vec<GapSpan>, cap: usize) -> Vec<GapSpan> {
    // A nonempty loss set cannot be represented in zero rows; the cap
    // degenerates to one record at minimum.
    let cap = cap.max(1);
    spans.sort_by_key(|span| (span.start, span.end));
    let mut merged: Vec<GapSpan> = Vec::with_capacity(spans.len());
    for span in spans {
        match merged.last_mut() {
            // Adjacent (start == end) or overlapping: one loss.
            Some(last) if span.start <= last.end => {
                last.end = last.end.max(span.end);
                last.reason = last.reason.max(span.reason);
            }
            _ => merged.push(span),
        }
    }
    while merged.len() > cap {
        // Disjoint by construction, so every hole is a checked
        // subtraction; ties resolve to the earliest pair.
        let mut best = 0usize;
        let mut best_hole = u64::MAX;
        for i in 0..merged.len() - 1 {
            let hole = merged[i + 1].start - merged[i].end;
            if hole < best_hole {
                best_hole = hole;
                best = i;
            }
        }
        let swallowed = merged.remove(best + 1);
        let kept = &mut merged[best];
        kept.end = kept.end.max(swallowed.end);
        kept.reason = kept.reason.max(swallowed.reason);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: u64, end: u64, reason: GapReason) -> GapSpan {
        GapSpan { start, end, reason }
    }

    #[test]
    fn adjacent_and_overlapping_ranges_merge() {
        let out = coalesce(
            vec![
                span(2, 4, GapReason::Missing),
                span(4, 6, GapReason::Missing),
            ],
            1024,
        );
        assert_eq!(out, vec![span(2, 6, GapReason::Missing)]);

        let out = coalesce(
            vec![
                span(2, 5, GapReason::Missing),
                span(4, 8, GapReason::Missing),
            ],
            1024,
        );
        assert_eq!(out, vec![span(2, 8, GapReason::Missing)]);

        // Unsorted input and exact duplicates.
        let out = coalesce(
            vec![
                span(9, 12, GapReason::Missing),
                span(2, 4, GapReason::Missing),
                span(9, 12, GapReason::Missing),
            ],
            1024,
        );
        assert_eq!(
            out,
            vec![
                span(2, 4, GapReason::Missing),
                span(9, 12, GapReason::Missing)
            ]
        );

        // A one-position hole keeps the gaps separate.
        let out = coalesce(
            vec![
                span(2, 4, GapReason::Missing),
                span(5, 6, GapReason::Missing),
            ],
            1024,
        );
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn merged_reason_is_the_worst_member() {
        let out = coalesce(
            vec![
                span(2, 4, GapReason::Missing),
                span(4, 6, GapReason::Corrupt),
            ],
            1024,
        );
        assert_eq!(out, vec![span(2, 6, GapReason::Corrupt)]);

        let out = coalesce(
            vec![
                span(2, 4, GapReason::Truncated),
                span(4, 6, GapReason::Missing),
            ],
            1024,
        );
        assert_eq!(out, vec![span(2, 6, GapReason::Truncated)]);
    }

    #[test]
    fn coarsening_merges_the_smallest_hole_and_stops_at_the_cap() {
        // 5 gaps with holes 1, 10, 3, 100 (in order) and a cap of 4:
        // the pair with hole 1 merges first.
        let spans = vec![
            span(10, 11, GapReason::Missing),
            span(12, 13, GapReason::Missing),
            span(23, 25, GapReason::Missing),
            span(28, 30, GapReason::Missing),
            span(130, 140, GapReason::Missing),
        ];
        let out = coalesce(spans, 4);
        assert_eq!(out.len(), 4);
        assert_eq!(out[0], span(10, 13, GapReason::Missing));
        assert_eq!(
            out,
            vec![
                span(10, 13, GapReason::Missing),
                span(23, 25, GapReason::Missing),
                span(28, 30, GapReason::Missing),
                span(130, 140, GapReason::Missing),
            ]
        );

        // Continuing past the cap next merges the hole of 3 (25 -> 28),
        // swallowing exactly that hole: coarsening widens, never narrows.
        let spans = vec![
            span(10, 13, GapReason::Missing),
            span(23, 25, GapReason::Missing),
            span(28, 30, GapReason::Missing),
            span(130, 140, GapReason::Missing),
            span(150, 151, GapReason::Missing),
        ];
        let out = coalesce(spans, 4);
        assert_eq!(out.len(), 4);
        assert_eq!(out[1], span(23, 30, GapReason::Missing));
    }

    #[test]
    fn coarsening_carries_the_worst_reason_into_the_merged_record() {
        let spans = vec![
            span(10, 11, GapReason::Missing),
            span(12, 13, GapReason::Corrupt),
            span(50, 60, GapReason::Missing),
            span(70, 80, GapReason::Missing),
            span(90, 100, GapReason::Missing),
        ];
        let out = coalesce(spans, 4);
        assert_eq!(out[0], span(10, 13, GapReason::Corrupt));
    }

    #[test]
    fn empty_input_and_degenerate_cap() {
        assert!(coalesce(Vec::new(), 1024).is_empty());
        // A cap of zero degenerates to one widening record when forced.
        let out = coalesce(
            vec![
                span(1, 2, GapReason::Missing),
                span(5, 6, GapReason::Missing),
            ],
            0,
        );
        assert_eq!(out, vec![span(1, 6, GapReason::Missing)]);
    }

    #[test]
    fn raw_and_normalized_coordinate_systems_both_coalesce_by_value() {
        // Raw gaps start at offset 0 and use byte semantics; the merge
        // logic is coordinate-agnostic.
        let out = coalesce(
            vec![
                span(0, 4096, GapReason::Truncated),
                span(4096, 8192, GapReason::Truncated),
            ],
            1024,
        );
        assert_eq!(out, vec![span(0, 8192, GapReason::Truncated)]);
    }
}
