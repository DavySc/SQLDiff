//! UTC timestamps as RFC 3339 strings (`YYYY-MM-DDTHH:MM:SSZ`).
//!
//! Every stored time is normalized to this one form, so times compare as
//! strings.

use std::time::{SystemTime, UNIX_EPOCH};

use sqldiff_source::{Error, Result};

/// The current time.
pub fn now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    from_unix(secs)
}

pub fn from_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Seconds since the Unix epoch of a normalized time.
pub fn to_unix(t: &str) -> Result<i64> {
    let n = normalize(t)?;
    let num = |r: std::ops::Range<usize>| n[r].parse::<i64>().expect("normalized digits");
    let days = days_from_civil(num(0..4), num(5..7), num(8..10));
    Ok(days * 86_400 + num(11..13) * 3600 + num(14..16) * 60 + num(17..19))
}

/// Accepts `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM[:SS[.frac]]` with `Z` or a
/// `±HH:MM` offset (a space may replace `T`), and returns the UTC form.
pub fn normalize(t: &str) -> Result<String> {
    let bad = || Error::new(format!("time `{t}`: expected YYYY-MM-DD or RFC 3339"));
    let s = t.trim();
    let digits = |r: &str| -> Result<i64> {
        if r.is_empty() || !r.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        r.parse().map_err(|_| bad())
    };
    if s.len() < 10 || s.as_bytes()[4] != b'-' || s.as_bytes()[7] != b'-' {
        return Err(bad());
    }
    let (y, mo, d) = (digits(&s[0..4])?, digits(&s[5..7])?, digits(&s[8..10])?);
    if !(1..=12).contains(&mo) || d < 1 || d > days_in_month(y, mo) {
        return Err(bad());
    }
    let mut secs = days_from_civil(y, mo, d) * 86_400;
    let rest = &s[10..];
    if !rest.is_empty() {
        let rest = rest.strip_prefix(['T', 't', ' ']).ok_or_else(bad)?;
        let (clock, offset) = match rest.find(['Z', 'z', '+', '-']) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => return Err(bad()),
        };
        let clock = clock.split('.').next().unwrap_or("");
        let parts: Vec<&str> = clock.split(':').collect();
        if !(2..=3).contains(&parts.len()) {
            return Err(bad());
        }
        let h = digits(parts[0])?;
        let mi = digits(parts[1])?;
        let se = if parts.len() == 3 { digits(parts[2])? } else { 0 };
        if h > 23 || mi > 59 || se > 60 {
            return Err(bad());
        }
        secs += h * 3600 + mi * 60 + se.min(59);
        if !offset.eq_ignore_ascii_case("z") {
            let sign = if offset.starts_with('-') { -1 } else { 1 };
            let (oh, om) = offset[1..].split_once(':').ok_or_else(bad)?;
            secs -= sign * (digits(oh)? * 3600 + digits(om)? * 60);
        }
    }
    Ok(from_unix(secs))
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

// Howard Hinnant's civil calendar algorithms.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { yoe + era * 400 + 1 } else { yoe + era * 400 }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        assert_eq!(from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(from_unix(1_792_713_600), "2026-10-23T00:00:00Z");
        assert_eq!(to_unix("2026-10-23").unwrap(), 1_792_713_600);
        assert_eq!(normalize("2024-02-29").unwrap(), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn offsets_and_fractions() {
        assert_eq!(
            normalize("2026-10-23T17:30:00.123+02:00").unwrap(),
            "2026-10-23T15:30:00Z"
        );
        assert_eq!(normalize("2026-10-23 01:00-01:30").unwrap(), "2026-10-23T02:30:00Z");
    }

    #[test]
    fn rejects() {
        for t in ["", "2026-13-01", "2023-02-29", "2026-10-23T25:00Z", "2026-10-23T10:00", "tomorrow"] {
            assert!(normalize(t).is_err(), "{t}");
        }
    }
}
