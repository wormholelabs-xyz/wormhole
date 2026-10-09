//! UTC timestamps for the ledger: unix seconds ↔ RFC 3339 `Z` strings.
//! Integer civil-date math (Howard Hinnant's days_from_civil algorithm).

use anyhow::{bail, Context, Result};

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_secs()
}

/// `1735689600` → `"2025-01-01T00:00:00Z"`.
pub fn unix_to_rfc3339(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let secs_of_day = unix % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    )
}

/// Accepts unix seconds (`1735689600`) or RFC 3339 UTC (`2025-01-01T00:00:00Z`).
pub fn parse_at(input: &str) -> Result<u64> {
    if input.chars().all(|c| c.is_ascii_digit()) {
        return input.parse::<u64>().context("unix-seconds timestamp");
    }
    let bytes = input.as_bytes();
    if bytes.len() != 20 || bytes[10] != b'T' || bytes[19] != b'Z' {
        bail!("expected unix seconds or `YYYY-MM-DDTHH:MM:SSZ`, got `{input}`");
    }
    let field = |range: std::ops::Range<usize>, name: &str| -> Result<u64> {
        input[range]
            .parse::<u64>()
            .with_context(|| format!("{name} in `{input}`"))
    };
    let year = field(0..4, "year")?;
    let month = field(5..7, "month")?;
    let day = field(8..10, "day")?;
    let hour = field(11..13, "hour")?;
    let minute = field(14..16, "minute")?;
    let second = field(17..19, "second")?;
    if !(1970..=9999).contains(&year) {
        bail!("year {year} out of range 1970..=9999");
    }
    if !(1..=12).contains(&month) {
        bail!("month {month} out of range 1..=12");
    }
    if !(1..=31).contains(&day) {
        bail!("day {day} out of range 1..=31");
    }
    if hour > 23 || minute > 59 || second > 59 {
        bail!("time {hour:02}:{minute:02}:{second:02} out of range");
    }
    let days = days_from_civil(year as i64, month, day);
    let unix = (days as u64) * 86_400 + hour * 3600 + minute * 60 + second;
    // Round-trip check catches day-of-month overflow (e.g. Feb 30).
    let canonical = unix_to_rfc3339(unix);
    if canonical != input {
        bail!("`{input}` is not a real UTC instant (canonical form: {canonical})");
    }
    Ok(unix)
}

fn days_from_civil(year: i64, month: u64, day: u64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = (y - era * 400) as u64;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

fn civil_from_days(days: i64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    ((if month <= 2 { y + 1 } else { y }) as u64, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_known_instants() {
        let cases: [(u64, &str); 5] = [
            (0, "1970-01-01T00:00:00Z"),
            (951_782_400, "2000-02-29T00:00:00Z"),
            (1_735_689_600, "2025-01-01T00:00:00Z"),
            (1_788_220_800, "2026-09-01T00:00:00Z"),
            (4_102_444_799, "2099-12-31T23:59:59Z"),
        ];
        for (unix, rendered) in cases {
            assert_eq!(unix_to_rfc3339(unix), rendered, "format {unix}");
            assert_eq!(parse_at(rendered).unwrap(), unix, "parse {rendered}");
            assert_eq!(parse_at(&unix.to_string()).unwrap(), unix, "unix form");
        }
    }

    #[test]
    fn rejects_malformed_inputs() {
        let cases: [&str; 6] = [
            "2025-02-30T00:00:00Z",
            "2025-13-01T00:00:00Z",
            "2025-01-01T24:00:00Z",
            "2025-01-01 00:00:00Z",
            "2025-01-01T00:00:00",
            "not-a-time",
        ];
        for input in cases {
            assert!(parse_at(input).is_err(), "must reject `{input}`");
        }
    }
}
