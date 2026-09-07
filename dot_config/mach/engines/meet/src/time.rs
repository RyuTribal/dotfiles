//! Minimal UTC time helpers, hand-rolled with no `chrono`/`time` crate
//! dependency -- the same civil-calendar algorithm and RFC3339 format
//! `kb-engine`'s `store` module already uses, duplicated here (a couple
//! dozen lines) rather than pulled in as a cross-engine dependency.
//!
//! Everything in this crate is UTC, including the meeting directory's
//! `YYYY-MM-DD-HHMM` timestamp -- matching the rest of the codebase's
//! `now_rfc3339` convention rather than adding local-timezone handling
//! (no `libc`/tzdata lookup) for phase A.

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64; // [0, 399]
    let mp = if m > 2 { m as i64 - 3 } else { m as i64 + 9 };
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn ymdhms_from_secs(secs: u64) -> (i64, u32, u32, u64, u64, u64) {
    let days = secs / 86400;
    let tod = secs % 86400;
    let (h, mi, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    (y, mo, d, h, mi, s)
}

pub fn rfc3339_from_secs(secs: u64) -> String {
    let (y, mo, d, h, mi, s) = ymdhms_from_secs(secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, s)
}

/// The current instant as an RFC3339 UTC timestamp -- `meeting.json`'s
/// `started_at`/`ended_at` format.
pub fn now_rfc3339() -> String {
    rfc3339_from_secs(now_secs())
}

/// `YYYY-MM-DD-HHMM`, minute resolution -- the meeting directory name's
/// timestamp prefix.
pub fn dir_timestamp_from_secs(secs: u64) -> String {
    let (y, mo, d, h, mi, _s) = ymdhms_from_secs(secs);
    format!("{:04}-{:02}-{:02}-{:02}{:02}", y, mo, d, h, mi)
}

/// Parses a `YYYY-MM-DDTHH:MM:SSZ` timestamp (the only format this crate
/// writes) into Unix seconds. Returns `None` on anything that doesn't match.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 {
        return None;
    }
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let mo: u32 = s.get(5..7)?.parse().ok()?;
    let d: u32 = s.get(8..10)?.parse().ok()?;
    let h: i64 = s.get(11..13)?.parse().ok()?;
    let mi: i64 = s.get(14..16)?.parse().ok()?;
    let se: i64 = s.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    Some(days * 86400 + h * 3600 + mi * 60 + se)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_roundtrips_through_parse() {
        let secs = 1_757_000_000u64;
        let s = rfc3339_from_secs(secs);
        assert_eq!(parse_rfc3339(&s), Some(secs as i64));
    }

    #[test]
    fn dir_timestamp_is_minute_resolution() {
        // 2026-09-07T17:33:45Z and 17:33:59Z land on the same minute.
        let base = parse_rfc3339("2026-09-07T17:33:00Z").unwrap() as u64;
        assert_eq!(dir_timestamp_from_secs(base), "2026-09-07-1733");
        assert_eq!(dir_timestamp_from_secs(base + 45), "2026-09-07-1733");
        assert_eq!(dir_timestamp_from_secs(base + 60), "2026-09-07-1734");
    }

    #[test]
    fn parse_rejects_malformed_input() {
        assert_eq!(parse_rfc3339(""), None);
        assert_eq!(parse_rfc3339("not a timestamp"), None);
        assert_eq!(parse_rfc3339("2026-13-07T00:00:00Z"), None); // month 13
    }
}
