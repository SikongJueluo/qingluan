//! Minimal UTC ISO-8601 helpers for session timestamps.
//!
//! Pi writes timestamps as `Date.toISOString()` (UTC, milliseconds, `Z`).
//! These pure helpers parse and format that shape without pulling in a
//! datetime crate.

/// Parse an ISO-8601 timestamp into epoch milliseconds.
///
/// Supported: `YYYY-MM-DDTHH:MM:SS[.fff][Z|±HH:MM]`. Returns `None` for
/// out-of-range or malformed input (JS `NaN`). A missing timezone is treated
/// as UTC.
pub fn parse_iso8601_ms(s: &str) -> Option<u64> {
    let bytes = s.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let parse2 = |start: usize| -> Option<u32> {
        let hi = bytes.get(start).copied()?;
        let lo = bytes.get(start + 1).copied()?;
        if !hi.is_ascii_digit() || !lo.is_ascii_digit() {
            return None;
        }
        Some((hi - b'0') as u32 * 10 + (lo - b'0') as u32)
    };

    let year: i64 = {
        let mut v: i64 = 0;
        for &b in bytes.get(0..4)? {
            if !b.is_ascii_digit() {
                return None;
            }
            v = v * 10 + (b - b'0') as i64;
        }
        v
    };
    if bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return None;
    }
    let month = parse2(5)?;
    let day = parse2(8)?;
    let sep = bytes.get(10).copied()?;
    if sep != b'T' && sep != b't' && sep != b' ' {
        return None;
    }
    if bytes.get(13) != Some(&b':') || bytes.get(16) != Some(&b':') {
        return None;
    }
    let hour = parse2(11)?;
    let minute = parse2(14)?;
    let second = parse2(17)?;

    let mut rest = &s[19..];
    // Optional fractional seconds.
    let mut millis: i64 = 0;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits: String = frac.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            return None;
        }
        let padded = format!("{digits:0<3}");
        millis = padded[..3].parse::<i64>().ok()?;
        rest = &frac[digits.len()..];
    }

    // Optional timezone.
    let offset_ms: i64 = if rest.is_empty() || rest.eq_ignore_ascii_case("Z") {
        0
    } else {
        let sign = match rest.as_bytes().first() {
            Some(b'+') => 1,
            Some(b'-') => -1,
            _ => return None,
        };
        let body = &rest[1..];
        let (oh, om) = body.split_once(':')?;
        if oh.len() != 2 || om.len() != 2 {
            return None;
        }
        let oh: i64 = oh.parse().ok()?;
        let om: i64 = om.parse().ok()?;
        if oh > 23 || om > 59 {
            return None;
        }
        sign * (oh * 60 + om) * 60_000
    };

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let days = days_from_civil(year, month as i64, day as i64)?;
    let secs = days * 86_400 + hour as i64 * 3_600 + minute as i64 * 60 + second as i64;
    let total = secs * 1000 + millis - offset_ms;
    if total < 0 {
        return None;
    }
    Some(total as u64)
}

/// Format epoch milliseconds as UTC ISO-8601 with millisecond precision.
pub fn format_iso8601_ms(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let millis = ms % 1000;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year,
        month,
        day,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
        millis
    )
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// Inverse of [`days_from_civil`].
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_utc_iso_with_millis() {
        assert_eq!(
            parse_iso8601_ms("2026-08-16T10:55:34.971Z"),
            Some(1_786_877_734_971)
        );
    }

    #[test]
    fn round_trips_through_format() {
        let ms = parse_iso8601_ms("2026-08-16T10:55:34.971Z").unwrap();
        assert_eq!(format_iso8601_ms(ms), "2026-08-16T10:55:34.971Z");
        assert_eq!(format_iso8601_ms(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn handles_offsets_and_missing_parts() {
        let z = parse_iso8601_ms("2026-08-16T10:00:00.000Z").unwrap();
        assert_eq!(parse_iso8601_ms("2026-08-16T12:00:00+02:00"), Some(z));
        assert_eq!(parse_iso8601_ms("2026-08-16T08:00:00-02:00"), Some(z));
        // No fractional seconds, no timezone (treated as UTC).
        assert_eq!(parse_iso8601_ms("2026-08-16T10:00:00"), Some(z));
    }

    #[test]
    fn rejects_malformed_timestamps() {
        assert_eq!(parse_iso8601_ms(""), None);
        assert_eq!(parse_iso8601_ms("not-a-date"), None);
        assert_eq!(parse_iso8601_ms("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_iso8601_ms("2026-08-32T00:00:00Z"), None);
        assert_eq!(parse_iso8601_ms("2026-08-16T25:00:00Z"), None);
    }
}
