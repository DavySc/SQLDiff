//! Implicit conversion of a string literal to the type of the value it is
//! compared with, or explicitly cast to.
//!
//! `e = '2020-01-01'` with `e` a datetime2 column compares dates, not
//! strings: SQL Server converts the string to the other operand's type (data
//! type precedence), and PostgreSQL resolves the untyped literal to it. So
//! the literal stands for the value it converts to, and collation plays no
//! part. The value only counts as known where the conversion below models
//! the engine; everything else stays dialect-specific (unknown across
//! dialects), never "equal".
//!
//! Checked on SQL Server 2022 and PostgreSQL 18 (2026-10-26):
//! - A comparison converts the literal to the type, not to the column's
//!   precision: a datetime2(0) or time(0) column against `'… 10:00:00.4'`
//!   does not match 10:00:00 on either engine. SQL Server reads it as
//!   datetime2(7) / time(7); PostgreSQL rounds it to microseconds, half to
//!   even (`'… 23:59:59.9999999'` becomes 10000-01-01 00:00:00).
//! - SQL Server converts to `datetime` and `smalldatetime` themselves:
//!   milliseconds round to 1/300 s, seconds round to minutes, more than three
//!   fractional digits fail. A `yyyy-mm-dd` literal is read as year-day-month
//!   for them under DATEFORMAT dmy, ydm and dym (date and datetime2 are not
//!   affected; `yyyymmdd` and `yyyy-mm-ddThh:mm:ss` never are).
//! - SQL Server converts to the column's decimal(p, s), rounding (`'1.005'`
//!   equals 1.01 in decimal(10,2)); PostgreSQL compares the exact numeric
//!   value, so there it does not.
//! - Integers: both accept a sign and leading zeros and fail past the
//!   type's range. bit: SQL Server reads `'true'`/`'false'` in any case and
//!   `'0'`/`'1'`; boolean: PostgreSQL reads those and more spellings.
//! - uniqueidentifier and uuid: case and braces do not matter on either.
//! - A cast to a type with a precision or scale rounds to it on both.
//! - Db2 for i is not checked, so nothing is modelled for it.

use sqldiff_syntax::Dialect;

use super::ast::Type;
use super::term::{a, n, Term};

/// What a string literal converts to when it meets a value of a declared
/// type: the facts `Ty` leaves out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// An integer type with its range.
    Int {
        lo: i128,
        hi: i128,
    },
    /// T-SQL bit.
    Bit,
    /// PostgreSQL boolean.
    Bool,
    /// decimal / numeric (precision, scale) when declared.
    Exact {
        p: Option<u32>,
        s: Option<u32>,
    },
    Date,
    /// datetime2(p) / timestamp(p); the digits matter for casts only.
    Timestamp {
        digits: u32,
    },
    Time {
        digits: u32,
    },
    /// T-SQL datetime (1/300 s, from 1753).
    Datetime,
    /// T-SQL smalldatetime (minutes, 1900 to 2079-06-06).
    Smalldatetime,
    Uuid,
}

impl Target {
    /// The target for a declared type (base name upper case, synonyms
    /// folded as in `catalog::types::canonical`).
    pub fn of(base: &str, args: &[u32], d: Dialect) -> Option<Target> {
        let int = |bits: u32| Target::Int {
            lo: -(1i128 << (bits - 1)),
            hi: (1i128 << (bits - 1)) - 1,
        };
        let digits = |default: u32| args.first().copied().unwrap_or(default);
        Some(match (d, base) {
            (Dialect::Db2i, _) => return None,
            (_, "INTEGER" | "INT" | "INT4") => int(32),
            (_, "SMALLINT" | "INT2") => int(16),
            (_, "BIGINT" | "INT8") => int(64),
            (Dialect::Tsql, "TINYINT") => Target::Int { lo: 0, hi: 255 },
            (Dialect::Tsql, "BIT") => Target::Bit,
            (Dialect::Postgres, "BOOLEAN" | "BOOL") => Target::Bool,
            (_, "DECIMAL" | "NUMERIC" | "DEC") => {
                let (p, s) = match (d, args) {
                    // T-SQL defaults to (18, 0); PostgreSQL to unconstrained.
                    (Dialect::Tsql, []) => (Some(18), Some(0)),
                    (_, [p]) => (Some(*p), Some(0)),
                    (_, [p, s, ..]) => (Some(*p), Some(*s)),
                    _ => (None, None),
                };
                Target::Exact { p, s }
            }
            (_, "DATE") => Target::Date,
            (Dialect::Tsql, "DATETIME2") => Target::Timestamp { digits: digits(7) },
            (Dialect::Tsql, "TIME") => Target::Time { digits: digits(7) },
            (Dialect::Tsql, "DATETIME") => Target::Datetime,
            (Dialect::Tsql, "SMALLDATETIME") => Target::Smalldatetime,
            (Dialect::Tsql, "UNIQUEIDENTIFIER") => Target::Uuid,
            (Dialect::Postgres, "TIMESTAMP") => Target::Timestamp { digits: digits(6) },
            (Dialect::Postgres, "TIME" | "TIME WITHOUT TIME ZONE") => {
                Target::Time { digits: digits(6) }
            }
            (Dialect::Postgres, "UUID") => Target::Uuid,
            _ => return None,
        })
    }

    /// The target of a type written in a statement (CAST, DECLARE).
    pub fn of_type(t: &Type, d: Dialect) -> Option<Target> {
        let args: Option<Vec<u32>> = t.args.iter().map(|x| x.parse().ok()).collect();
        let base = crate::catalog::types::canonical(&t.name.to_uppercase()).to_string();
        Target::of(&base, &args?, d)
    }

    pub fn name(&self) -> &'static str {
        match self {
            Target::Int { .. } => "int",
            Target::Bit => "bit",
            Target::Bool => "bool",
            Target::Exact { .. } => "exact",
            Target::Date => "date",
            Target::Timestamp { .. } => "timestamp",
            Target::Time { .. } => "time",
            Target::Datetime => "datetime",
            Target::Smalldatetime => "smalldatetime",
            Target::Uuid => "uuid",
        }
    }
}

/// The value a literal converts to, as a term, and the assumption it rests
/// on, if any. None when the conversion is not modelled for certain (it may
/// round, fail or depend on settings).
///
/// `cast`: an explicit CAST, which rounds to the target's precision and
/// scale; a comparison does not (see the module notes).
pub fn convert(
    s: &str,
    t: Target,
    d: Dialect,
    cast: bool,
) -> Option<(Term, Option<(&'static str, &'static str)>)> {
    if d == Dialect::Db2i {
        return None;
    }
    let tval = |kind: &str, v: String| n("tval", vec![a(kind), a(v)]);
    match t {
        Target::Int { lo, hi } => {
            let v = integer(s)?;
            (lo..=hi)
                .contains(&v)
                .then(|| (n("num", vec![a(v.to_string())]), None))
        }
        Target::Bit => {
            let v = match s.to_ascii_lowercase().as_str() {
                "1" | "true" => "1",
                "0" | "false" => "0",
                _ => return None,
            };
            Some((n("num", vec![a(v)]), None))
        }
        Target::Bool => {
            let v = match s.to_ascii_lowercase().as_str() {
                "true" | "t" | "yes" | "y" | "on" | "1" => "TRUE",
                "false" | "f" | "no" | "n" | "off" | "0" => "FALSE",
                _ => return None,
            };
            Some((n("bool", vec![a(v)]), None))
        }
        Target::Exact { p, s: scale } => {
            let (neg, int, frac) = decimal(s)?;
            // SQL Server converts to the declared (p, s), and so does a cast
            // on PostgreSQL: only a value that fits stays unchanged.
            if d == Dialect::Tsql || cast {
                if let (Some(p), Some(sc)) = (p, scale) {
                    if frac.len() as u32 > sc || int.len() as u32 > p.saturating_sub(sc) {
                        return None;
                    }
                } else if d == Dialect::Tsql {
                    return None;
                }
            }
            let mut v = if int.is_empty() { "0".to_string() } else { int };
            if !frac.is_empty() {
                v = format!("{v}.{frac}");
            }
            if neg && v != "0" {
                v = format!("-{v}");
            }
            Some((n("num", vec![a(v)]), None))
        }
        Target::Uuid => {
            let g = s
                .strip_prefix('{')
                .and_then(|x| x.strip_suffix('}'))
                .unwrap_or(s);
            let groups: Vec<&str> = g.split('-').collect();
            let shape = groups.iter().map(|x| x.len()).collect::<Vec<_>>() == [8, 4, 4, 4, 12];
            (shape
                && groups
                    .iter()
                    .all(|x| x.bytes().all(|b| b.is_ascii_hexdigit())))
            .then(|| (n("uuid", vec![a(g.to_ascii_lowercase())]), None))
        }
        Target::Time { digits } => {
            let tm = time(s)?;
            // SQL Server keeps 7 digits, PostgreSQL 6 (it rounds further ones).
            let keep = if cast {
                digits
            } else if d == Dialect::Tsql {
                7
            } else {
                6
            };
            (tm.frac.len() as u32 <= keep).then(|| (tval("time", tm.show()), None))
        }
        Target::Date => {
            let dt = datetime(s, d)?;
            // Both engines drop the time of day; longer fractions are not checked.
            let fits = dt.time.as_ref().map_or(0, |t| t.frac.len()) <= 6;
            (fits && dt.valid_year(1, 9999)).then(|| (tval("date", dt.date()), None))
        }
        Target::Timestamp { digits } => {
            let dt = datetime(s, d)?;
            let keep = if cast {
                digits
            } else if d == Dialect::Tsql {
                7
            } else {
                6
            };
            let fits = dt.time.as_ref().map_or(0, |t| t.frac.len() as u32) <= keep;
            (fits && dt.valid_year(1, 9999)).then(|| (tval("ts", dt.show()), None))
        }
        Target::Datetime | Target::Smalldatetime => {
            if d != Dialect::Tsql {
                return None;
            }
            let dt = datetime(s, d)?;
            let exact = match (&dt.time, t) {
                (None, _) => true,
                // Multiples of 10 ms are whole 1/300 s ticks; others round.
                (Some(tm), Target::Datetime) => {
                    tm.frac.len() <= 3 && (tm.frac.len() < 3 || tm.frac.ends_with('0'))
                }
                (Some(tm), _) => tm.sec == 0 && tm.frac.bytes().all(|b| b == b'0'),
            };
            let range = if t == Target::Datetime {
                dt.valid_year(1753, 9999)
            } else {
                dt.valid_year(1900, 2078)
            };
            if !exact || !range {
                return None;
            }
            let assume = (dt.form == Form::Dash).then_some(DATEFORMAT);
            Some((tval("ts", dt.show()), assume))
        }
    }
}

pub const DATEFORMAT: (&str, &str) = (
    "ASM-TSQL-DATEFORMAT-YMD",
    "The session's DATEFORMAT is mdy (the us_english default), ymd or myd, so a 'yyyy-mm-dd' literal compared with or converted to datetime or smalldatetime reads as year-month-day (under dmy, ydm or dym SQL Server reads it as year-day-month)",
);

/// Fractional-second digits of a literal written as a date and time or a
/// time of day; None for any other text.
pub fn fraction_digits(s: &str) -> Option<usize> {
    match datetime(s, Dialect::Postgres) {
        Some(dt) => Some(dt.time.map_or(0, |t| t.frac.len())),
        None => time(s).map(|t| t.frac.len()),
    }
}

/// `[+-]digits`, nothing else.
fn integer(s: &str) -> Option<i128> {
    let body = s.strip_prefix(['+', '-']).unwrap_or(s);
    if body.is_empty() || body.len() > 30 || !body.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `[+-]digits[.digits]`: sign, integer digits and fraction digits without
/// leading or trailing zeros.
fn decimal(s: &str) -> Option<(bool, String, String)> {
    let neg = s.starts_with('-');
    let body = s.strip_prefix(['+', '-']).unwrap_or(s);
    let (int, frac) = body.split_once('.').unwrap_or((body, ""));
    let digits = |x: &str| x.bytes().all(|b| b.is_ascii_digit());
    if int.is_empty() || !digits(int) || !digits(frac) || (body.contains('.') && frac.is_empty()) {
        return None;
    }
    Some((
        neg,
        int.trim_start_matches('0').to_string(),
        frac.trim_end_matches('0').to_string(),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// yyyy-mm-dd, optionally with ` hh:mm…`.
    Dash,
    /// yyyy-mm-ddThh:mm:ss…
    Iso,
    /// yyyymmdd, optionally with ` hh:mm…`.
    Compact,
}

#[derive(Debug)]
struct Tm {
    h: u32,
    m: u32,
    sec: u32,
    /// Fraction digits as written.
    frac: String,
}

impl Tm {
    fn show(&self) -> String {
        let f = self.frac.trim_end_matches('0');
        let mut s = format!("{:02}:{:02}:{:02}", self.h, self.m, self.sec);
        if !f.is_empty() {
            s.push('.');
            s.push_str(f);
        }
        s
    }
}

#[derive(Debug)]
struct Dt {
    y: u32,
    mo: u32,
    d: u32,
    time: Option<Tm>,
    form: Form,
}

impl Dt {
    fn date(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.y, self.mo, self.d)
    }

    fn show(&self) -> String {
        let t = self.time.as_ref().map_or("00:00:00".to_string(), Tm::show);
        format!("{} {t}", self.date())
    }

    fn valid_year(&self, lo: u32, hi: u32) -> bool {
        (lo..=hi).contains(&self.y)
    }
}

fn num(s: &str, min: usize, max: usize) -> Option<u32> {
    (s.len() >= min && s.len() <= max && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// `h:mm[:ss[.f+]]`, hour 0-23 (both engines read one or two hour digits).
fn time(s: &str) -> Option<Tm> {
    let mut parts = s.split(':');
    let h = num(parts.next()?, 1, 2)?;
    let m = num(parts.next()?, 2, 2)?;
    let (sec, frac) = match parts.next() {
        None => (0, String::new()),
        Some(x) => {
            let (sec, frac) = x.split_once('.').unwrap_or((x, ""));
            if x.contains('.') && (frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit())) {
                return None;
            }
            (num(sec, 2, 2)?, frac.to_string())
        }
    };
    if parts.next().is_some() || h > 23 || m > 59 || sec > 59 {
        return None;
    }
    Some(Tm { h, m, sec, frac })
}

/// A date with an optional time of day in the forms both engines read the
/// same way. SQL Server does not read `yyyy-mm-ddThh:mm` (no seconds).
fn datetime(s: &str, d: Dialect) -> Option<Dt> {
    if !s.is_ascii() {
        return None;
    }
    let (date, rest, sep) = match s.find([' ', 'T']) {
        Some(i) => (&s[..i], Some(&s[i + 1..]), s.as_bytes()[i]),
        None => (s, None, b' '),
    };
    let (y, mo, day, form) = if date.contains('-') {
        let mut p = date.split('-');
        let y = num(p.next()?, 4, 4)?;
        let mo = num(p.next()?, 1, 2)?;
        let day = num(p.next()?, 1, 2)?;
        if p.next().is_some() {
            return None;
        }
        (y, mo, day, if sep == b'T' { Form::Iso } else { Form::Dash })
    } else {
        if sep == b'T' || date.len() != 8 {
            return None;
        }
        (
            num(&date[..4], 4, 4)?,
            num(&date[4..6], 2, 2)?,
            num(&date[6..], 2, 2)?,
            Form::Compact,
        )
    };
    if !valid_date(y, mo, day) {
        return None;
    }
    let time = match rest {
        None => None,
        Some(r) => {
            let t = time(r)?;
            if form == Form::Iso && d == Dialect::Tsql && r.matches(':').count() < 2 {
                return None;
            }
            Some(t)
        }
    };
    Some(Dt {
        y,
        mo,
        d: day,
        time,
        form,
    })
}

fn valid_date(y: u32, m: u32, d: u32) -> bool {
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    y >= 1 && d >= 1 && d <= days
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Dialect = Dialect::Tsql;
    const P: Dialect = Dialect::Postgres;

    fn v(s: &str, t: Target, d: Dialect) -> Option<String> {
        convert(s, t, d, false).map(|(t, _)| t.to_string())
    }

    #[test]
    fn timestamps_keep_what_each_engine_keeps() {
        let ts = Target::Timestamp { digits: 7 };
        let pg = Target::Timestamp { digits: 6 };
        assert_eq!(v("2020-01-01", ts, T), v("2020-01-01 00:00", pg, P));
        assert_eq!(
            v("2020-01-01 10:00:00.5", ts, T),
            v("20200101 10:00:00.500000", pg, P)
        );
        // Seven digits: exact on SQL Server, rounded on PostgreSQL.
        assert!(v("9999-12-31 23:59:59.9999999", ts, T).is_some());
        assert_eq!(v("9999-12-31 23:59:59.9999999", pg, P), None);
        assert_eq!(v("2020-01-01 10:00:00.12345678", ts, T), None);
        assert_eq!(v("2020-02-30", ts, T), None);
        assert_eq!(v("2020-01-01T10:00", ts, T), None);
        assert!(v("2020-01-01T10:00", pg, P).is_some());
        assert_eq!(v("2020-01-01 10:00:00.", pg, P), None);
        assert_eq!(v("2020-01-01 24:00:00", pg, P), None);
        assert_eq!(v("20›0101", ts, T), None);
        assert_eq!(fraction_digits("abc›defgh"), None);
    }

    #[test]
    fn datetime_and_smalldatetime_round() {
        assert!(v("2020-01-01 10:00:00.120", Target::Datetime, T).is_some());
        assert_eq!(v("2020-01-01 10:00:00.001", Target::Datetime, T), None);
        assert_eq!(v("2020-01-01 10:00:00.1234", Target::Datetime, T), None);
        assert_eq!(v("1752-12-31", Target::Datetime, T), None);
        assert_eq!(v("2020-01-01 10:00:30", Target::Smalldatetime, T), None);
        assert!(v("2020-01-01 10:01", Target::Smalldatetime, T).is_some());
        let dash = convert("2020-01-02", Target::Datetime, T, false).unwrap();
        assert_eq!(dash.1, Some(DATEFORMAT));
        let compact = convert("20200102", Target::Datetime, T, false).unwrap();
        assert_eq!(compact.1, None);
        assert_eq!(dash.0, compact.0);
    }

    #[test]
    fn numbers_follow_the_declared_scale_on_sql_server_only() {
        let dec = Target::Exact {
            p: Some(10),
            s: Some(2),
        };
        assert_eq!(v("1.010", dec, T), v("+1.01", dec, P));
        assert_eq!(v("1.005", dec, T), None);
        assert!(v("1.005", dec, P).is_some());
        assert!(convert("1.005", dec, P, true).is_none());
        assert_eq!(v("123456789", dec, T), None);
        let int = Target::of("INTEGER", &[], T).unwrap();
        assert_eq!(v("05", int, T).as_deref(), Some("(num 5)"));
        assert_eq!(v("5.0", int, T), None);
        assert_eq!(v("9999999999", int, P), None);
        assert_eq!(v(" 5", int, P), None);
    }

    #[test]
    fn bits_booleans_and_uuids() {
        assert_eq!(v("TRUE", Target::Bit, T), v("1", Target::Bit, T));
        assert_eq!(v("yes", Target::Bit, T), None);
        assert_eq!(v("yes", Target::Bool, P).as_deref(), Some("(bool TRUE)"));
        let g = "6F9619FF-8B86-D011-B42D-00C04FC964FF";
        assert_eq!(
            v(g, Target::Uuid, T),
            v(&format!("{{{}}}", g.to_lowercase()), Target::Uuid, P)
        );
        assert_eq!(v("6F9619FF8B86D011B42D00C04FC964FF", Target::Uuid, P), None);
    }

    #[test]
    fn db2_for_i_is_not_modelled() {
        assert_eq!(Target::of("DATE", &[], Dialect::Db2i), None);
        assert_eq!(v("2020-01-01", Target::Date, Dialect::Db2i), None);
    }
}
