//! Concrete evaluation of one statement over a small database, used to find
//! a witness for a same-dialect edit (`edit`).
//!
//! The evaluator runs both versions of a statement on the same inputs: a
//! "world" of at most two rows per table, parameter and variable values, and
//! the values of subexpressions it does not model. Inputs it needs are
//! created on first use and filled in by a backtracking search over a few
//! candidate values each (`search`). A witness is an assignment under which
//! both versions finish and their observable results differ for certain.
//!
//! Everything it cannot decide for certain stops the evaluation instead of
//! guessing: collation-dependent string comparisons, numeric results that
//! depend on a type it does not know, nondeterministic order, functions it
//! does not model (their results are opaque values that only compare equal
//! to the same function of the same arguments). So a witness is never more
//! than the evaluator can show; when no witness is found the edit stays
//! "unknown".

use std::collections::{HashMap, HashSet};

use sqldiff_syntax::Dialect;

use super::ast::{Call, Cte, Expr, From, InsertSource, OrderItem, Select, Stmt, Type};
use super::schema::{Fam, Schema, Ty};

// ---- values ------------------------------------------------------------

/// A value in a world.
#[derive(Debug, Clone, PartialEq)]
pub enum V {
    Null,
    Int(i128),
    /// Exact or approximate number other than an integer.
    Num(f64),
    Str(String),
    Bool(bool),
    /// A value of a type the evaluator does not know; values are ordered.
    Gen(u8),
    /// The result of a deterministic subexpression it does not model,
    /// identified by function and argument values. It may stand for NULL
    /// or an error, so it is never known to differ from either.
    Opq(usize),
    /// Some value of the same type that differs from opaque value `k`.
    Ne(usize),
    /// A PostgreSQL interval: months, days and microseconds.
    Ivl(i64, i64, i128),
    /// A number computed from random values (RAND()): the hull of every
    /// value it can take (lo, hi), and a range (lo, hi) whose ends it
    /// certainly gets arbitrarily close to. Never NULL.
    Rng(f64, f64, f64, f64),
    /// The statement raises an error.
    Err(String),
}

/// A string literal; long runs of one character are counted.
fn show_str(s: &str) -> String {
    let quote = |t: &str| format!("'{}'", t.replace('\'', "''"));
    let n = s.chars().count();
    if n <= 20 {
        return quote(s);
    }
    let chars: Vec<char> = s.chars().collect();
    let mut parts = Vec::new();
    let mut plain = String::new();
    let mut i = 0;
    while i < chars.len() {
        let mut j = i;
        while j < chars.len() && chars[j] == chars[i] {
            j += 1;
        }
        if j - i >= 4 {
            if !plain.is_empty() {
                parts.push(quote(&std::mem::take(&mut plain)));
            }
            let what = if chars[i] == ' ' {
                "blanks".to_string()
            } else {
                quote(&chars[i].to_string())
            };
            parts.push(format!("{} {what}", j - i));
        } else {
            plain.extend(&chars[i..j]);
        }
        i = j;
    }
    if !plain.is_empty() {
        parts.push(quote(&plain));
    }
    format!("{} ({n} characters)", parts.join(" + "))
}

impl V {
    fn is_null(&self) -> bool {
        matches!(self, V::Null)
    }

    /// Plain-language rendering for witnesses.
    pub fn show(&self, opaque: &[String]) -> String {
        match self {
            V::Null => "NULL".into(),
            V::Int(i) => i.to_string(),
            V::Num(f) => {
                let s = format!("{f}");
                if s.contains('.') || s.contains('e') {
                    s
                } else {
                    format!("{s}.0")
                }
            }
            V::Str(s) => show_str(s),
            V::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
            V::Gen(g) => format!("value {}", (b'a' + g) as char),
            V::Opq(i) => opaque
                .get(*i)
                .map(|s| format!("the value of {s}"))
                .unwrap_or_else(|| "an opaque value".into()),
            V::Ne(i) => opaque
                .get(*i)
                .map(|s| format!("a value other than {s}"))
                .unwrap_or_else(|| "another value".into()),
            V::Err(e) => format!("an error ({e})"),
            V::Ivl(m, d, us) => format!("interval '{}'", show_interval(*m, *d, *us)),
            V::Rng(lo, hi, _, _) => {
                format!("a random value between {} and {}", show_f(*lo), show_f(*hi))
            }
        }
    }
}

fn show_f(f: f64) -> String {
    let r = (f * 1e6).round() / 1e6;
    if r.fract() == 0.0 {
        format!("{r:.0}")
    } else {
        format!("{r}")
    }
}

/// The range of RAND(): the hull [0, 1], and values certainly produced
/// near 0 and near 1 (it returns many values in any interval this wide).
const RAND_RANGE: V = V::Rng(0.0, 1.0, 1e-6, 1.0 - 1e-6);

/// Interval arithmetic on random ranges: `f` applied to both hulls. `f`
/// must be monotone in each argument or the hull must be recomputed from
/// all corner values, which this does.
fn rng_op(
    a: (f64, f64, f64, f64),
    b: (f64, f64, f64, f64),
    f: impl Fn(f64, f64) -> f64,
) -> Option<V> {
    let hull = |x: (f64, f64), y: (f64, f64)| {
        let c = [f(x.0, y.0), f(x.0, y.1), f(x.1, y.0), f(x.1, y.1)];
        let lo = c.iter().cloned().fold(f64::INFINITY, f64::min);
        let hi = c.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        (lo.is_finite() && hi.is_finite() && lo.abs() < 1e12 && hi.abs() < 1e12).then_some((lo, hi))
    };
    let (lo, hi) = hull((a.0, a.1), (b.0, b.1))?;
    let (ilo, ihi) = hull((a.2, a.3), (b.2, b.3))?;
    Some(V::Rng(lo, hi, ilo, ihi))
}

fn rng_parts(v: &V) -> Option<(f64, f64, f64, f64)> {
    match v {
        V::Rng(a, b, c, d) => Some((*a, *b, *c, *d)),
        V::Int(i) => Some((*i as f64, *i as f64, *i as f64, *i as f64)),
        V::Num(f) => Some((*f, *f, *f, *f)),
        _ => None,
    }
}

/// Whether a random range and another value certainly differ as outcomes:
/// one side gets values the other never takes.
fn rng_differs(a: &V, b: &V) -> Option<bool> {
    let (x, y) = (rng_parts(a)?, rng_parts(b)?);
    let eps = |v: f64| 1e-9 * v.abs().max(1.0);
    // x reaches below y's lowest value or above its highest, or the reverse.
    let beyond = |x: (f64, f64, f64, f64), y: (f64, f64, f64, f64)| {
        x.2 < y.0 - eps(y.0) || x.3 > y.1 + eps(y.1)
    };
    (beyond(x, y) || beyond(y, x)).then_some(true)
}

/// An interval as PostgreSQL shows it (IntervalStyle postgres).
fn show_interval(months: i64, days: i64, us: i128) -> String {
    let mut parts = Vec::new();
    let unit =
        |n: i64, one: &str, many: &str| format!("{n} {}", if n.abs() == 1 { one } else { many });
    let (y, mo) = (months / 12, months % 12);
    if y != 0 {
        parts.push(unit(y, "year", "years"));
    }
    if mo != 0 {
        parts.push(unit(mo, "mon", "mons"));
    }
    if days != 0 {
        parts.push(unit(days, "day", "days"));
    }
    if us != 0 || parts.is_empty() {
        let sign = if us < 0 { "-" } else { "" };
        let a = us.abs();
        let (secs, frac) = (a / 1_000_000, a % 1_000_000);
        let mut t = format!(
            "{sign}{:02}:{:02}:{:02}",
            secs / 3600,
            secs / 60 % 60,
            secs % 60
        );
        if frac != 0 {
            t.push_str(format!(".{frac:06}").trim_end_matches('0'));
        }
        parts.push(t);
    }
    parts.join(" ")
}

const DAY_US: i128 = 86_400_000_000;

/// The PostgreSQL interval input forms with whole amounts: `N unit ...`
/// (year, month, week, day, hour, minute, second and their abbreviations),
/// `HH:MM[:SS]`, an `@` prefix and an `ago` suffix. None for anything else.
fn parse_interval(s: &str) -> Option<(i64, i64, i128)> {
    let mut words: Vec<String> = s.split_whitespace().map(|w| w.to_lowercase()).collect();
    if words.first().is_some_and(|w| w == "@") {
        words.remove(0);
    }
    let ago = words.last().is_some_and(|w| w == "ago");
    if ago {
        words.pop();
    }
    if words.is_empty() {
        return None;
    }
    let (mut mo, mut d, mut us) = (0i64, 0i64, 0i128);
    let mut i = 0;
    while i < words.len() {
        let w = &words[i];
        if w.contains(':') {
            let (neg, t) = match w.strip_prefix('-') {
                Some(t) => (true, t),
                None => (false, w.strip_prefix('+').unwrap_or(w)),
            };
            let f: Vec<i128> = t
                .split(':')
                .map(|x| {
                    (!x.is_empty() && x.bytes().all(|c| c.is_ascii_digit()))
                        .then(|| x.parse().ok())
                        .flatten()
                })
                .collect::<Option<_>>()?;
            let secs = match f.as_slice() {
                [h, m] => h * 3600 + m * 60,
                [h, m, s] if *m < 60 && *s < 60 => h * 3600 + m * 60 + s,
                _ => return None,
            };
            us += if neg { -secs } else { secs } * 1_000_000;
            i += 1;
            continue;
        }
        // A number and its unit, written apart or together (`1day`).
        let digits_end = w
            .char_indices()
            .find(|(k, c)| !(c.is_ascii_digit() || (*k == 0 && (*c == '-' || *c == '+'))))
            .map(|(k, _)| k)
            .unwrap_or(w.len());
        let (num, unit) = if digits_end == w.len() {
            i += 1;
            (w.as_str(), words.get(i)?.as_str())
        } else {
            (&w[..digits_end], &w[digits_end..])
        };
        let n: i64 = num.parse().ok()?;
        match unit.trim_end_matches(',') {
            "year" | "years" | "y" | "yr" | "yrs" => mo += n.checked_mul(12)?,
            "month" | "months" | "mon" | "mons" => mo += n,
            "week" | "weeks" | "w" => d += n.checked_mul(7)?,
            "day" | "days" | "d" => d += n,
            "hour" | "hours" | "h" | "hr" | "hrs" => us += n as i128 * 3_600_000_000,
            "minute" | "minutes" | "min" | "mins" | "m" => us += n as i128 * 60_000_000,
            "second" | "seconds" | "sec" | "secs" | "s" => us += n as i128 * 1_000_000,
            _ => return None,
        }
        i += 1;
    }
    Some(if ago { (-mo, -d, -us) } else { (mo, d, us) })
}

/// Interval ordering key: PostgreSQL compares intervals as if a month had
/// 30 days and a day 24 hours.
fn interval_key(m: i64, d: i64, us: i128) -> i128 {
    (m as i128 * 30 + d as i128) * DAY_US + us
}

/// A timestamp (or a date, at midnight) plus an interval, as PostgreSQL
/// adds them: months first (the day clamped to the month's end), then days,
/// then time. None when the value is not a date or the result leaves years
/// 1 to 9999.
fn add_interval(ts: &str, m: i64, d: i64, us: i128) -> Option<String> {
    let dl = parse_date(ts)?;
    let t = instant(&dl.rest)?;
    let (y, mo, dd) = civil_from_days(dl.days);
    let months = y * 12 + (mo - 1) + m;
    let (ny, nm) = (months.div_euclid(12), months.rem_euclid(12) + 1);
    let mut nd = dd;
    while civil_from_days(days_from_civil(ny, nm, nd)) != (ny, nm, nd) {
        nd -= 1;
    }
    let days = days_from_civil(ny, nm, nd) + d;
    // `instant` counts 100 ns.
    let total = days as i128 * DAY_US + t / 10 + us;
    let (days, rem) = (total.div_euclid(DAY_US), total.rem_euclid(DAY_US));
    let (y, _, _) = civil_from_days(days as i64);
    if !(1..=9999).contains(&y) {
        return None;
    }
    let secs = rem / 1_000_000;
    let frac = rem % 1_000_000;
    let mut out = format_date(&DateLit {
        sep: true,
        days: days as i64,
        rest: format!(" {:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60),
    });
    if frac != 0 {
        out.push_str(format!(".{frac:06}").trim_end_matches('0'));
    }
    Some(out)
}

/// Why an evaluation did not produce a value.
#[derive(Debug, Clone, PartialEq)]
pub enum Stop {
    /// Needs the value of this input; the search assigns it and retries.
    Need(usize),
    /// This assignment cannot serve as a witness (ambiguous result).
    Reject,
    /// The statement uses something the evaluator does not model at all.
    Unsupported(String),
}

pub type R<T> = Result<T, Stop>;

fn unsupported<T>(what: impl Into<String>) -> R<T> {
    Err(Stop::Unsupported(what.into()))
}

/// Three-valued truth of a value used as a condition.
fn truth(v: &V) -> R<Option<bool>> {
    match v {
        V::Null => Ok(None),
        V::Bool(b) => Ok(Some(*b)),
        V::Err(_) => Ok(None),
        _ => rej(),
    }
}

fn tri(b: Option<bool>) -> V {
    match b {
        None => V::Null,
        Some(b) => V::Bool(b),
    }
}

// ---- dates ---------------------------------------------------------------

/// A date or timestamp literal: format pattern and value (days, seconds of
/// the day as a fraction string kept verbatim).
#[derive(Debug, Clone, PartialEq)]
struct DateLit {
    sep: bool,
    days: i64,
    rest: String,
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn parse_date(s: &str) -> Option<DateLit> {
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        if part.bytes().all(|c| c.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    let (sep, y, m, d, end) = if b.len() >= 10 && b[4] == b'-' && b[7] == b'-' {
        (true, digits(0..4)?, digits(5..7)?, digits(8..10)?, 10)
    } else if b.len() >= 8 {
        (false, digits(0..4)?, digits(4..6)?, digits(6..8)?, 8)
    } else {
        return None;
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let days = days_from_civil(y, m, d);
    if civil_from_days(days) != (y, m, d) {
        return None;
    }
    let rest = &s[end..];
    // A time part: ` 23:59:59.9999999` or `T10:00:00`.
    if !rest.is_empty() {
        let t = rest.strip_prefix(' ').or_else(|| rest.strip_prefix('T'))?;
        if !t
            .bytes()
            .all(|c| c.is_ascii_digit() || c == b':' || c == b'.')
            || t.len() < 5
        {
            return None;
        }
    }
    Some(DateLit {
        sep,
        days,
        rest: rest.to_string(),
    })
}

fn format_date(d: &DateLit) -> String {
    let (y, m, dd) = civil_from_days(d.days);
    if d.sep {
        format!("{y:04}-{m:02}-{dd:02}{}", d.rest)
    } else {
        format!("{y:04}{m:02}{dd:02}{}", d.rest)
    }
}

/// Time of day in units of 100 ns, from a date literal's time part
/// (midnight when there is none).
fn instant(rest: &str) -> Option<i128> {
    let (h, m, s, frac) = time_of(rest)?;
    let digits = frac.trim_start_matches('.');
    if digits.len() > 7 || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let f: i128 = format!("{digits:0<7}").parse().ok()?;
    Some((((h * 60 + m) * 60 + s) as i128) * 10_000_000 + f)
}

/// The order of two date and time values that holds whatever date or time
/// type stores or converts them, or None when they may be equal. A column
/// rounds what it stores (smalldatetime to the minute, datetime to 1/300 s,
/// datetime2(p) and timestamp(p) to p digits) and a literal compared with it
/// is converted to its type first: a time moves by at most 30 seconds, so
/// whole minutes, and times two minutes or more apart, keep their order. A
/// date column drops the time of what it is compared with.
fn date_order(x: &DateLit, y: &DateLit) -> Option<std::cmp::Ordering> {
    const MINUTE: i128 = 60 * 10_000_000;
    const LAST_MINUTE: i128 = (23 * 60 + 59) * MINUTE;
    if x.days != y.days {
        let (early, late) = if x.days < y.days { (x, y) } else { (y, x) };
        // Rounding into the next day: a time after 23:59 that is not a whole
        // minute, or one not read here.
        let may_round = !early.rest.is_empty()
            && instant(&early.rest).map_or(true, |p| p > LAST_MINUTE && p % MINUTE != 0);
        if late.days - early.days == 1 && x.rest != y.rest && may_round {
            return None;
        }
        return Some(x.days.cmp(&y.days));
    }
    if x.rest.is_empty() || y.rest.is_empty() {
        return None;
    }
    let (p, q) = (instant(&x.rest)?, instant(&y.rest)?);
    let whole = p % MINUTE == 0 && q % MINUTE == 0;
    (p != q && (whole || (p - q).abs() >= 2 * MINUTE)).then(|| p.cmp(&q))
}

/// Whether a column of type `t` stores `v` as it is: in range, within its
/// length and precision, and (for times) a whole minute. False when unsure.
pub fn fits_type(v: &V, t: &Ty) -> bool {
    match (t.fam, v) {
        (_, V::Null) => true,
        (Fam::Int, V::Int(i)) => match t.len {
            Some(1) => *i == 0 || *i == 1,
            Some(bits @ (16 | 32 | 64)) => {
                let lim = 1i128 << (bits - 1);
                (-lim..lim).contains(i)
            }
            _ => false,
        },
        (Fam::Exact, V::Int(_) | V::Num(_)) => {
            let Some(x) = num_of(v) else { return false };
            match (t.len, t.scale) {
                (Some(p), Some(s)) => {
                    let m = x * 10f64.powi(s as i32);
                    m.fract() == 0.0 && m.abs() < 10f64.powi(p as i32) && m.abs() < 1e15
                }
                _ => false,
            }
        }
        (Fam::Float, V::Int(i)) => i.abs() < 1 << 50,
        (Fam::Float, V::Num(_)) => true,
        (Fam::VarChar | Fam::Char, V::Str(s)) => {
            s.is_ascii() && t.len.map_or(true, |l| s.len() as u32 <= l)
        }
        (Fam::Date, V::Str(s)) => parse_date(s).is_some_and(|d| d.rest.is_empty()),
        (Fam::Timestamp, V::Str(s)) => parse_date(s).is_some_and(|d| {
            d.rest.is_empty() || instant(&d.rest).is_some_and(|p| p % (60 * 10_000_000) == 0)
        }),
        (Fam::Bool, V::Bool(_)) => true,
        _ => false,
    }
}

/// Same date literal one day later or earlier.
pub fn date_neighbour(s: &str, delta: i64) -> Option<String> {
    let mut d = parse_date(s)?;
    d.days += delta;
    let (y, _, _) = civil_from_days(d.days);
    if !(1..=9999).contains(&y) {
        return None;
    }
    Some(format_date(&d))
}

// ---- string comparison ---------------------------------------------------

/// Equality of two strings that holds under every common collation, or
/// None when it depends on the collation (case, trailing spaces, accents).
fn str_eq(a: &str, b: &str) -> Option<bool> {
    if a == b {
        return Some(true);
    }
    // Dates written differently may be the same date and time.
    if let (Some(x), Some(y)) = (parse_date(a), parse_date(b)) {
        if x.sep != y.sep || x.rest != y.rest {
            return date_order(&x, &y).map(|_| false);
        }
    }
    let fold = |s: &str| s.trim_end_matches(' ').to_lowercase();
    if fold(a) == fold(b) || !a.is_ascii() || !b.is_ascii() {
        return None;
    }
    Some(false)
}

/// Order of two strings that holds under every common collation.
fn str_cmp(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering::*;
    if a == b {
        return Some(Equal);
    }
    // Two dates in the same format order by date and time in every type
    // and collation.
    if let (Some(x), Some(y)) = (parse_date(a), parse_date(b)) {
        if x.sep == y.sep {
            return date_order(&x, &y);
        }
    }
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let common = ab.iter().zip(bb).take_while(|(x, y)| x == y).count();
    if !ab[..common].iter().all(u8::is_ascii_alphanumeric) {
        return None;
    }
    match (ab.get(common), bb.get(common)) {
        (Some(x), Some(y)) => {
            let same_class = (x.is_ascii_digit() && y.is_ascii_digit())
                || (x.is_ascii_lowercase() && y.is_ascii_lowercase())
                || (x.is_ascii_uppercase() && y.is_ascii_uppercase());
            same_class.then(|| x.cmp(y))
        }
        // A proper prefix sorts first when the rest starts with a letter or digit.
        (None, Some(y)) if y.is_ascii_alphanumeric() => Some(Less),
        (Some(x), None) if x.is_ascii_alphanumeric() => Some(Greater),
        _ => None,
    }
}

// ---- inputs ----------------------------------------------------------------

/// What an input stands for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// A parameter or variable, lower case without `@`.
    Var(String),
    /// A column of a table row: table key, row index, column name.
    Col(String, usize, String),
    /// A field of a record variable.
    Field(String, String),
}

/// How much the search may choose an input's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Chosen by the caller or the database state (parameters, columns).
    Free,
    /// A local value whose earlier assignments were not traced: using it
    /// needs the assumption that the value can occur.
    Assumed,
    /// Fixed by the routine (a constant), never chosen.
    Fixed,
    /// Assigned a random draw: each version reads its own unknown number.
    Random,
}

/// Value class of an input, from its declared type or how it is compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Int,
    /// T-SQL bit: 0 or 1.
    Bit,
    Num,
    Str,
    Bool,
    /// Dates, as ISO strings.
    Date,
    /// Timestamps, as ISO strings with a time of day.
    Stamp,
    /// PostgreSQL intervals.
    Ivl,
    /// Unknown type: only ordered, distinct values.
    Gen,
}

#[derive(Debug, Clone)]
pub struct Input {
    pub key: Key,
    pub origin: Origin,
    pub cands: Vec<V>,
}

/// Facts about a parameter or variable of the routine.
#[derive(Debug, Clone)]
pub struct VarFact {
    pub ty: Option<Ty>,
    pub origin: Origin,
    /// Constant value (origin Fixed).
    pub value: Option<V>,
    pub nullable: bool,
    /// Whether only non-negative integers occur (counts, row counts).
    pub non_negative: bool,
    /// Extra candidate values (e.g. `@@FETCH_STATUS`).
    pub cands: Vec<V>,
}

/// An updated or deleted row: its index, and (column, new value, old value)
/// for each SET column.
pub type Write = (usize, Vec<(String, V, V)>);

/// What a statement shows its caller or the rest of the routine.
#[derive(Debug, Clone, PartialEq)]
pub enum Obs {
    /// Result rows (ordered when the query orders them).
    Rows(Vec<Vec<V>>, bool),
    /// A value (assignment, RETURN, argument).
    Value(V),
    /// Assignment from a query: the value, or None when no row qualifies
    /// (the variable keeps its value).
    Assigned(Vec<Option<V>>),
    /// Rows updated or deleted: (table row index, (column, new value, old
    /// value) for each SET column; empty for a delete).
    Writes(Vec<Write>),
    /// Rows inserted, with the column of each value (lower-cased; empty
    /// when the table's columns are not known).
    Inserted(Vec<String>, Vec<Vec<V>>),
    /// Which branch of a condition chain runs (None: no branch).
    Branch(Option<usize>),
    /// The statement fails.
    Error(String),
}

// ---- evaluation context ---------------------------------------------------------

/// One FROM item's row in scope.
#[derive(Debug, Clone)]
enum Src {
    /// Row `row` of base table `table`.
    Base(String, usize),
    /// A derived row: column name → value.
    Derived(Vec<(String, V)>),
    /// The NULL-extended side of an outer join.
    Nulls,
}

#[derive(Debug, Clone)]
struct Binding {
    /// Names the row can be referred to by (alias, or table name and its last part).
    names: Vec<String>,
    /// Table key for base tables (to find catalog columns).
    table: Option<String>,
    src: Src,
}

type Scope = Vec<Binding>;

/// Hints about an input name: value class and literal values it is
/// compared with.
#[derive(Debug, Clone, Default)]
pub struct Hint {
    pub class: Option<Class>,
    pub lits: Vec<V>,
    /// Tested with IS [NOT] NULL.
    pub null_tested: bool,
}

pub struct Ctx<'s> {
    pub d: Dialect,
    pub schema: &'s Schema,
    pub vars: HashMap<String, VarFact>,
    /// Record variables (PL/pgSQL `r` of `FOR r IN ...`).
    pub records: Vec<String>,
    /// Hints by lower-case column or variable name (last name part).
    pub hints: HashMap<String, Hint>,
    /// Rows per table key.
    pub shape: HashMap<String, usize>,
    pub inputs: Vec<Input>,
    index: HashMap<Key, usize>,
    pub values: Vec<Option<V>>,
    /// Opaque values by identity.
    pub opaque: Vec<String>,
    /// Opaque values known never to be NULL.
    pub nonnull: Vec<bool>,
    opaque_ids: HashMap<String, usize>,
    /// Side tag mixed into nondeterministic opaque values (A and B never agree).
    pub side: u8,
    /// Whether a nondeterministic value was used.
    pub nondeterministic: bool,
    /// Tables seen, in order.
    pub tables: Vec<String>,
    /// A candidate value for the next input created (the other side of a comparison).
    extra: Option<V>,
    /// Conditions each side's statement is only reached under: (condition,
    /// whether it must be true or must not be true), for side A and side B.
    pub guards: [Vec<(Expr, bool)>; 2],
    /// Tables read near the change: their two-row worlds are tried first.
    pub focus: Vec<String>,
    /// A guard could not be evaluated in the last run (reaching the
    /// statement with the witness inputs is then assumed).
    pub guard_skipped: bool,
    /// Common table expressions in scope, innermost last.
    ctes: Vec<Vec<Cte>>,
    /// Result columns of table functions by table key.
    func_schema: HashMap<String, Vec<(String, Type)>>,
    /// Origin of a table function's rows (that of the variable it reads).
    func_origin: HashMap<String, Origin>,
    /// Variables read only through a table function, and variables read
    /// directly, in this run (one variable may not be both).
    func_vars: HashSet<String>,
    read_vars: HashSet<String>,
}

impl<'s> Ctx<'s> {
    pub fn new(d: Dialect, schema: &'s Schema) -> Self {
        Ctx {
            d,
            schema,
            vars: HashMap::new(),
            records: Vec::new(),
            hints: HashMap::new(),
            shape: HashMap::new(),
            inputs: Vec::new(),
            index: HashMap::new(),
            values: Vec::new(),
            opaque: Vec::new(),
            nonnull: Vec::new(),
            opaque_ids: HashMap::new(),
            side: 0,
            nondeterministic: false,
            tables: Vec::new(),
            extra: None,
            guards: [Vec::new(), Vec::new()],
            focus: Vec::new(),
            guard_skipped: false,
            ctes: Vec::new(),
            func_schema: HashMap::new(),
            func_origin: HashMap::new(),
            func_vars: HashSet::new(),
            read_vars: HashSet::new(),
        }
    }

    fn input(&mut self, key: Key, origin: Origin, mut cands: Vec<V>) -> R<V> {
        if let Some(&i) = self.index.get(&key) {
            return match &self.values[i] {
                Some(v) => Ok(v.clone()),
                None => Err(Stop::Need(i)),
            };
        }
        if let Some(x) = self.extra.take() {
            if !matches!(x, V::Null | V::Err(_)) && !cands.contains(&x) {
                if let V::Opq(k) = x {
                    cands.insert(0, V::Ne(k));
                }
                cands.insert(0, x);
            }
        }
        // A further row of a table starts as a copy of the row before it, so
        // that one change to a qualifying row makes both rows qualify.
        if let Key::Col(t, row, c) = &key {
            if *row > 0 {
                let prev = Key::Col(t.clone(), row - 1, c.clone());
                if let Some(Some(v)) = self.index.get(&prev).map(|&j| self.values[j].clone()) {
                    if let Some(at) = cands.iter().position(|x| *x == v) {
                        let v = cands.remove(at);
                        cands.insert(0, v);
                    }
                }
            }
        }
        let i = self.inputs.len();
        self.inputs.push(Input {
            key: key.clone(),
            origin,
            cands,
        });
        self.index.insert(key, i);
        self.values.push(None);
        Err(Stop::Need(i))
    }

    fn opaque_value(&mut self, id: String, show: String, nonnull: bool) -> V {
        if let Some(&i) = self.opaque_ids.get(&id) {
            return V::Opq(i);
        }
        let i = self.opaque.len();
        self.opaque.push(show);
        self.nonnull.push(nonnull);
        self.opaque_ids.insert(id, i);
        V::Opq(i)
    }

    fn hint(&self, name: &str) -> Hint {
        self.hints.get(name).cloned().unwrap_or_default()
    }

    /// Candidate values for an input of a class.
    fn candidates(&self, class: Class, hint: &Hint, nullable: bool, non_negative: bool) -> Vec<V> {
        let mut out: Vec<V> = Vec::new();
        let mut push = |v: V| {
            if !out.contains(&v) {
                out.push(v);
            }
        };
        match class {
            Class::Bit => {
                push(V::Int(0));
                push(V::Int(1));
            }
            Class::Int | Class::Num => {
                for l in &hint.lits {
                    match l {
                        V::Int(i) => {
                            push(V::Int(*i));
                            push(V::Int(i + 1));
                            push(V::Int(i - 1));
                        }
                        V::Num(f) if class == Class::Num => {
                            push(V::Num(*f));
                            push(V::Num(f + 1.0));
                            push(V::Num(f - 1.0));
                        }
                        V::Opq(k) => {
                            push(l.clone());
                            push(V::Ne(*k));
                        }
                        _ => {}
                    }
                }
                for i in [0, 1, 2, -1] {
                    push(if class == Class::Int {
                        V::Int(i)
                    } else {
                        V::Num(i as f64)
                    });
                }
            }
            Class::Str => {
                for l in &hint.lits {
                    match l {
                        V::Str(s) => {
                            push(V::Str(s.clone()));
                            if let Some(n) = date_neighbour(s, 1) {
                                push(V::Str(n));
                            }
                            if let Some(n) = date_neighbour(s, -1) {
                                push(V::Str(n));
                            }
                        }
                        V::Opq(k) => {
                            push(l.clone());
                            push(V::Ne(*k));
                        }
                        _ => {}
                    }
                }
                if !hint
                    .lits
                    .iter()
                    .any(|l| matches!(l, V::Str(s) if parse_date(s).is_some()))
                {
                    push(V::Str("x".into()));
                    push(V::Str("y".into()));
                }
            }
            Class::Bool => {
                push(V::Bool(true));
                push(V::Bool(false));
            }
            Class::Date | Class::Stamp => {
                for l in &hint.lits {
                    match l {
                        V::Str(s) if parse_date(s).is_some() => {
                            let d = parse_date(s).unwrap();
                            // A value the column can store: a date holds no
                            // time, and only whole minutes survive every
                            // timestamp type.
                            if class == Class::Date && !d.rest.is_empty() {
                                push(V::Str(format_date(&DateLit {
                                    sep: d.sep,
                                    days: d.days,
                                    rest: String::new(),
                                })));
                                continue;
                            }
                            if !d.rest.is_empty()
                                && !instant(&d.rest).is_some_and(|p| p % (60 * 10_000_000) == 0)
                            {
                                continue;
                            }
                            push(V::Str(s.clone()));
                            for delta in [1, -1] {
                                if let Some(n) = date_neighbour(s, delta) {
                                    push(V::Str(n));
                                }
                            }
                        }
                        V::Opq(k) => {
                            push(l.clone());
                            push(V::Ne(*k));
                        }
                        _ => {}
                    }
                }
                if class == Class::Stamp {
                    // A time of day, for time units and the format of the clock;
                    // a month apart, so that intervals fit between them.
                    push(V::Str("2020-01-01 10:00:00".into()));
                    push(V::Str("2020-02-01 10:00:00".into()));
                    // Another year, for date parts and ranges.
                    push(V::Str("2019-12-01 10:00:00".into()));
                } else {
                    push(V::Str("2020-01-01".into()));
                    push(V::Str("2020-01-02".into()));
                    // Another year, for date parts and ranges.
                    push(V::Str("2019-12-31".into()));
                }
            }
            Class::Ivl => {
                for l in &hint.lits {
                    match l {
                        V::Ivl(..) => push(l.clone()),
                        V::Str(t) => {
                            if let Some((m, d, us)) = parse_interval(t) {
                                push(V::Ivl(m, d, us));
                            }
                        }
                        V::Opq(k) => {
                            push(l.clone());
                            push(V::Ne(*k));
                        }
                        _ => {}
                    }
                }
                push(V::Ivl(0, 1, 0));
                push(V::Ivl(0, 0, 3_600_000_000));
                push(V::Ivl(1, 0, 0));
            }
            Class::Gen => {
                for l in &hint.lits {
                    if let V::Opq(k) = l {
                        push(l.clone());
                        push(V::Ne(*k));
                    }
                }
                push(V::Gen(0));
                push(V::Gen(1));
            }
        }
        if non_negative {
            out.retain(|v| !matches!(v, V::Int(i) if *i < 0));
        }
        if nullable {
            out.push(V::Null);
        }
        out
    }

    fn class_of_ty(ty: &Ty) -> Option<Class> {
        Some(match ty.fam {
            Fam::Int if ty.len == Some(1) => Class::Bit,
            Fam::Int => Class::Int,
            Fam::Exact | Fam::Float => Class::Num,
            Fam::Char | Fam::VarChar => Class::Str,
            Fam::Bool => Class::Bool,
            Fam::Date => Class::Date,
            Fam::Timestamp | Fam::TimestampTz => Class::Stamp,
            Fam::Interval => Class::Ivl,
            Fam::Time => return None,
            Fam::Binary | Fam::Other => return None,
        })
    }

    /// A parameter or variable.
    fn var(&mut self, name: &str) -> R<V> {
        let n = var_key(name);
        if self.func_vars.contains(&n) {
            return unsupported(format!("{name} is read besides a table function"));
        }
        self.read_vars.insert(n.clone());
        let Some(f) = self.vars.get(&n).cloned() else {
            // T-SQL system functions such as @@ROWCOUNT are declared by the caller.
            return unsupported(format!("unknown variable {name}"));
        };
        if let Some(v) = &f.value {
            return Ok(v.clone());
        }
        if f.origin == Origin::Random {
            // Each version draws its own number: values of the two versions
            // never agree, and none is chosen (not even NULL).
            self.nondeterministic = true;
            let id = format!("@{n}#random#{}", self.side);
            return Ok(self.opaque_value(id, format!("{name} (a random number)"), false));
        }
        let hint = self.hint(&format!("@{n}"));
        // A declared type the evaluator does not model (interval, xml, ...)
        // takes no class from how the variable is compared.
        let class = match &f.ty {
            Some(ty) => Self::class_of_ty(ty).unwrap_or(Class::Gen),
            None => hint.class.unwrap_or(Class::Gen),
        };
        let mut cands = f.cands.clone();
        for c in self.candidates(class, &hint, f.nullable, f.non_negative) {
            if !cands.contains(&c) {
                cands.push(c);
            }
        }
        self.input(Key::Var(n), f.origin, cands)
    }

    fn column(&mut self, table: &str, row: usize, col: &str) -> R<V> {
        let col = col.to_lowercase();
        let hint = self.hint(&col);
        if let Some(schema) = self.func_schema.get(table) {
            // A table function's column: typed by its WITH schema, nullable.
            // The document's value is the input; each version converts it to
            // its own declared type (strings are truncated, decimals rounded
            // or overflow), so values just beyond the type are candidates too.
            let Some((_, decl)) = schema.iter().find(|(c, _)| *c == col).cloned() else {
                return unsupported(format!("unknown column {col} of {table}"));
            };
            let ty = Ty::from_type(&decl, self.d, false);
            let class = Self::class_of_ty(&ty).or(hint.class).unwrap_or(Class::Gen);
            let mut cands = self.candidates(class, &hint, true, false);
            match (ty.fam, ty.len, ty.scale) {
                (Fam::VarChar | Fam::Char, Some(len), _) if len < 4000 => {
                    let at = cands.len().saturating_sub(1);
                    cands.insert(at, V::Str("x".repeat(len as usize + 1)));
                    // Blanks before the cut: a narrower column keeps only them.
                    if len > 1 {
                        let spaced = format!("x{}x", " ".repeat(len as usize - 1));
                        cands.insert(at + 1, V::Str(spaced));
                    }
                }
                (Fam::Exact, Some(p), Some(sc)) if p <= 30 => {
                    cands.push(V::Num(10f64.powi(-(sc as i32) - 1)));
                    cands.push(V::Num(10f64.powi(p as i32 - sc as i32)));
                }
                _ => {}
            }
            let origin = self.func_origin.get(table).copied().unwrap_or(Origin::Free);
            let v = self.input(Key::Col(table.to_string(), row, col), origin, cands)?;
            return self.cast(v, &decl);
        }
        // Temporal reads (`t FOR SYSTEM_TIME AS OF x`) use the catalog of `t`.
        let base = table.split(' ').next().unwrap_or(table);
        let cat = self
            .schema
            .table(base)
            .and_then(|t| t.column(&col))
            .cloned();
        let class = match &cat {
            Some(c) => Self::class_of_ty(&c.ty).unwrap_or(Class::Gen),
            None => hint.class.unwrap_or(Class::Gen),
        };
        // Without catalog facts a column may be NULL only when the code tests it.
        let nullable = match &cat {
            Some(c) => !c.not_null,
            None => hint.null_tested,
        };
        let cands = self.candidates(class, &hint, nullable, false);
        self.input(Key::Col(table.to_string(), row, col), Origin::Free, cands)
    }

    fn field(&mut self, rec: &str, col: &str) -> R<V> {
        let col = col.to_lowercase();
        let hint = self.hint(&col);
        let class = hint.class.unwrap_or(Class::Gen);
        let cands = self.candidates(class, &hint, hint.null_tested, false);
        self.input(Key::Field(rec.to_string(), col), Origin::Assumed, cands)
    }

    fn rows(&mut self, table: &str) -> usize {
        if !self.tables.iter().any(|t| t == table) {
            self.tables.push(table.to_string());
        }
        *self.shape.get(table).unwrap_or(&1)
    }
}

/// Key of a parameter or variable: lower case without `@`; T-SQL system
/// values (`@@ROWCOUNT`) keep one `@`.
pub fn var_key(name: &str) -> String {
    match name.strip_prefix("@@") {
        Some(rest) => format!("@{}", rest.to_lowercase()),
        None => name.trim_start_matches('@').to_lowercase(),
    }
}

/// Key of a table read: temporal reads (`FOR SYSTEM_TIME AS OF x`) are a
/// table of their own.
pub fn table_key_with(name: &[String], hints: &[String]) -> String {
    let base = table_key(name);
    match hints.iter().find(|h| h.starts_with("SYSTEM_TIME")) {
        Some(h) => format!("{base} {}", h.to_lowercase()),
        None => base,
    }
}

/// Key of a table function call: its name and arguments.
pub fn func_key(name: &[String], args: &[Expr]) -> String {
    let shown: Vec<String> = args
        .iter()
        .map(|a| match a {
            Expr::Var(v) => v.clone(),
            Expr::Name(p) => p.join("."),
            Expr::Str(s) => format!("'{}'", s.replace('\'', "''")),
            Expr::Num(n) => n.clone(),
            _ => "…".into(),
        })
        .collect();
    format!("{}({})", name.join("."), shown.join(", "))
}

/// The JSON text of `FOR JSON` rows, as a value that is equal exactly when
/// the rows are (NULL properties are left out, as FOR JSON does; unordered
/// rows are compared as a multiset).
fn for_json(r: &Rows) -> R<V> {
    if r.rows.is_empty() {
        return Ok(V::Null);
    }
    let mut objs = Vec::new();
    for row in &r.rows {
        let mut props = Vec::new();
        for (n, v) in r.names.iter().zip(row) {
            let s = match v {
                V::Null => continue,
                V::Int(i) => i.to_string(),
                V::Num(f) => format!("{f}"),
                V::Str(s) => format!("{s:?}"),
                V::Bool(b) => b.to_string(),
                V::Gen(g) => format!("\"<value {}>\"", (b'a' + g) as char),
                V::Err(e) => return Ok(V::Err(e.clone())),
                // Opaque values may equal anything.
                V::Opq(_) | V::Ne(_) | V::Ivl(..) | V::Rng(..) => return rej(),
            };
            props.push(format!("{n:?}:{s}"));
        }
        objs.push(format!("{{{}}}", props.join(",")));
    }
    if !r.ordered {
        objs.sort();
    }
    Ok(V::Str(format!("[{}]", objs.join(","))))
}

pub fn last_part(t: &str) -> &str {
    t.rsplit('.').next().unwrap_or(t)
}

pub fn table_key(name: &[String]) -> String {
    name.iter()
        .map(|p| p.trim_matches('"').to_lowercase())
        .collect::<Vec<_>>()
        .join(".")
}

// ---- expressions --------------------------------------------------------------

/// Aggregate context: the rows of the current group.
struct Group<'g> {
    rows: &'g [Scope],
}

const AGGREGATES: &[&str] = &[
    "COUNT",
    "SUM",
    "MIN",
    "MAX",
    "AVG",
    "COUNT_BIG",
    "STRING_AGG",
    "ARRAY_AGG",
    "BOOL_AND",
    "BOOL_OR",
    "EVERY",
    "JSON_AGG",
    "JSONB_AGG",
    "STDEV",
    "VAR",
];

/// Functions that read the clock: one input per function, the same for both versions.
const CLOCK: &[&str] = &[
    "GETDATE",
    "SYSDATETIME",
    "SYSUTCDATETIME",
    "GETUTCDATE",
    "CURRENT_TIMESTAMP",
    "NOW",
    "LOCALTIMESTAMP",
];

/// Hours, minutes, seconds and the verbatim fraction of a time part
/// (` 10:00:00.123`); midnight when there is none.
fn time_of(rest: &str) -> Option<(i64, i64, i64, String)> {
    if rest.is_empty() {
        return Some((0, 0, 0, String::new()));
    }
    let t = &rest[1..];
    let (hms, frac) = match t.find('.') {
        Some(i) => (&t[..i], t[i..].to_string()),
        None => (t, String::new()),
    };
    let parts: Vec<i64> = hms
        .split(':')
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    match parts.as_slice() {
        [h, m] => Some((*h, *m, 0, frac)),
        [h, m, s] => Some((*h, *m, *s, frac)),
        _ => None,
    }
}

/// T-SQL CONVERT date styles.
fn date_style(d: &str, style: u32) -> Option<String> {
    let dl = parse_date(d)?;
    let (y, m, dd) = civil_from_days(dl.days);
    let (h, mi, s, frac) = time_of(&dl.rest)?;
    const MON: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let ms = format!("{:0<3}", frac.trim_start_matches('.'))
        .chars()
        .take(3)
        .collect::<String>();
    Some(match style {
        112 => format!("{y:04}{m:02}{dd:02}"),
        23 => format!("{y:04}-{m:02}-{dd:02}"),
        101 => format!("{m:02}/{dd:02}/{y:04}"),
        103 => format!("{dd:02}/{m:02}/{y:04}"),
        111 => format!("{y:04}/{m:02}/{dd:02}"),
        113 => format!(
            "{dd:02} {} {y:04} {h:02}:{mi:02}:{s:02}:{ms}",
            MON[(m - 1) as usize]
        ),
        _ => return None,
    })
}

const NONDETERMINISTIC: &[&str] = &[
    "NEXT VALUE FOR",
    "NEWID",
    "NEWSEQUENTIALID",
    "RAND",
    "RANDOM",
    "GETDATE",
    "SYSDATETIME",
    "SYSUTCDATETIME",
    "GETUTCDATE",
    "SYSDATETIMEOFFSET",
    "NOW",
    "CLOCK_TIMESTAMP",
    "STATEMENT_TIMESTAMP",
    "TRANSACTION_TIMESTAMP",
    "TIMEOFDAY",
    "GEN_RANDOM_UUID",
    "UUID_GENERATE_V4",
    "CRYPT_GEN_RANDOM",
    "NEXTVAL",
    "CURRVAL",
    "SETVAL",
];

fn call_name(c: &Call) -> String {
    c.name.last().map(|s| s.to_uppercase()).unwrap_or_default()
}

pub fn is_aggregate(e: &Expr) -> bool {
    match e {
        Expr::Call(c) => {
            AGGREGATES.contains(&call_name(c).as_str()) && c.name.len() == 1 && !c.over
        }
        _ => false,
    }
}

fn has_aggregate(e: &Expr) -> bool {
    let mut found = false;
    walk_expr(e, &mut |x| {
        if is_aggregate(x) {
            found = true;
        }
    });
    found
}

/// Visits an expression and its subexpressions (not into subqueries).
/// Rebuilds an expression bottom-up where `f` returns a replacement
/// (subqueries are left as they are).
pub fn map_expr(e: Expr, f: &dyn Fn(&Expr) -> Option<Expr>) -> Expr {
    if let Some(r) = f(&e) {
        return r;
    }
    let m = |x: Box<Expr>| Box::new(map_expr(*x, f));
    let ml = |l: Vec<Expr>| l.into_iter().map(|y| map_expr(y, f)).collect::<Vec<_>>();
    match e {
        Expr::Call(mut c) => {
            c.args = ml(std::mem::take(&mut c.args));
            Expr::Call(c)
        }
        Expr::Bin(op, l, r) => Expr::Bin(op, m(l), m(r)),
        Expr::Unary(op, x) => Expr::Unary(op, m(x)),
        Expr::IsNull(x, n) => Expr::IsNull(m(x), n),
        Expr::Cast(x, t) => Expr::Cast(m(x), t),
        Expr::Extract(p, x) => Expr::Extract(p, m(x)),
        Expr::InList(x, l, n) => Expr::InList(m(x), ml(l), n),
        Expr::InQuery(x, q, n) => Expr::InQuery(m(x), q, n),
        Expr::Between(x, l, h, n) => Expr::Between(m(x), m(l), m(h), n),
        Expr::Like(x, p, a, b) => Expr::Like(m(x), m(p), a, b),
        Expr::Case(o, w, e2) => Expr::Case(
            o.map(m),
            w.into_iter()
                .map(|(a, b)| (map_expr(a, f), map_expr(b, f)))
                .collect(),
            e2.map(m),
        ),
        Expr::Tuple(l) => Expr::Tuple(ml(l)),
        other => other,
    }
}

pub fn walk_expr(e: &Expr, f: &mut dyn FnMut(&Expr)) {
    f(e);
    match e {
        Expr::Call(c) => {
            for a in &c.args {
                walk_expr(a, f);
            }
        }
        Expr::Bin(_, l, r) => {
            walk_expr(l, f);
            walk_expr(r, f);
        }
        Expr::Unary(_, x) | Expr::IsNull(x, _) | Expr::Cast(x, _) | Expr::Extract(_, x) => {
            walk_expr(x, f)
        }
        Expr::InList(x, l, _) => {
            walk_expr(x, f);
            for y in l {
                walk_expr(y, f);
            }
        }
        Expr::InQuery(x, _, _) => walk_expr(x, f),
        Expr::Between(x, l, h, _) => {
            walk_expr(x, f);
            walk_expr(l, f);
            walk_expr(h, f);
        }
        Expr::Like(x, p, _, _) => {
            walk_expr(x, f);
            walk_expr(p, f);
        }
        Expr::Case(o, w, e2) => {
            if let Some(o) = o {
                walk_expr(o, f);
            }
            for (a, b) in w {
                walk_expr(a, f);
                walk_expr(b, f);
            }
            if let Some(e2) = e2 {
                walk_expr(e2, f);
            }
        }
        Expr::Tuple(l) => {
            for y in l {
                walk_expr(y, f);
            }
        }
        _ => {}
    }
}

fn num_of(v: &V) -> Option<f64> {
    match v {
        V::Int(i) => Some(*i as f64),
        V::Num(f) => Some(*f),
        _ => None,
    }
}

fn parse_num(s: &str) -> R<V> {
    if let Ok(i) = s.parse::<i128>() {
        return Ok(V::Int(i));
    }
    match s.parse::<f64>() {
        Ok(f) => Ok(V::Num(f)),
        Err(_) => unsupported(format!("number {s}")),
    }
}

/// SQL equality of two non-NULL values; None when it is not certain.
fn eq_vals(a: &V, b: &V) -> Option<bool> {
    match (a, b) {
        (V::Int(x), V::Int(y)) => Some(x == y),
        (V::Str(x), V::Str(y)) => str_eq(x, y),
        (V::Bool(x), V::Bool(y)) => Some(x == y),
        (V::Gen(x), V::Gen(y)) => Some(x == y),
        (V::Ivl(..), _) | (_, V::Ivl(..)) => {
            let (x, y) = (ivl_key(a)?, ivl_key(b)?);
            Some(x == y)
        }
        (V::Rng(..), _) | (_, V::Rng(..)) => {
            let (x, y) = (rng_parts(a)?, rng_parts(b)?);
            (x.1 < y.0 || y.1 < x.0).then_some(false)
        }
        (V::Opq(x), V::Opq(y)) if x == y => Some(true),
        (V::Ne(x), V::Ne(y)) if x == y => Some(true),
        (V::Opq(x), V::Ne(y)) | (V::Ne(y), V::Opq(x)) if x == y => Some(false),
        _ => match (num_of(a), num_of(b)) {
            (Some(x), Some(y)) => {
                if x == y {
                    Some(true)
                } else if (x - y).abs() <= 1e-9 * x.abs().max(y.abs()).max(1.0) {
                    None
                } else {
                    Some(false)
                }
            }
            _ => None,
        },
    }
}

/// PostgreSQL boolean input: a unique prefix of true, false, yes, no, on,
/// off, or 1 and 0, in any case and surrounded by blanks.
fn pg_bool(t: &str) -> Option<bool> {
    let w = t.trim().to_lowercase();
    if w.is_empty() {
        return None;
    }
    let pre = |full: &str, min: usize| w.len() >= min && full.starts_with(w.as_str());
    if pre("true", 1) || pre("yes", 1) || w == "on" || w == "1" {
        Some(true)
    } else if pre("false", 1) || pre("no", 1) || pre("off", 2) || w == "0" {
        Some(false)
    } else {
        None
    }
}

/// Ordering key of an interval, or of a string compared with one (PostgreSQL
/// reads the literal as an interval).
fn ivl_key(v: &V) -> Option<i128> {
    match v {
        V::Ivl(m, d, us) => Some(interval_key(*m, *d, *us)),
        V::Str(t) => parse_interval(t).map(|(m, d, us)| interval_key(m, d, us)),
        _ => None,
    }
}

fn cmp_vals(a: &V, b: &V) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (V::Int(x), V::Int(y)) => Some(x.cmp(y)),
        (V::Str(x), V::Str(y)) => str_cmp(x, y),
        (V::Gen(x), V::Gen(y)) => Some(x.cmp(y)),
        (V::Ivl(..), _) | (_, V::Ivl(..)) => Some(ivl_key(a)?.cmp(&ivl_key(b)?)),
        (V::Rng(..), _) | (_, V::Rng(..)) => {
            let (x, y) = (rng_parts(a)?, rng_parts(b)?);
            if x.1 < y.0 {
                Some(std::cmp::Ordering::Less)
            } else if x.0 > y.1 {
                Some(std::cmp::Ordering::Greater)
            } else {
                None
            }
        }
        (V::Bool(x), V::Bool(y)) => Some(x.cmp(y)),
        (V::Opq(x), V::Opq(y)) if x == y => Some(std::cmp::Ordering::Equal),
        _ => match (num_of(a), num_of(b)) {
            (Some(x), Some(y)) => match eq_vals(a, b) {
                Some(true) => Some(std::cmp::Ordering::Equal),
                Some(false) => x.partial_cmp(&y),
                None => None,
            },
            _ => None,
        },
    }
}

/// Whether two values are the same for an observer (NULL equals NULL).
pub fn same(a: &V, b: &V) -> Option<bool> {
    same_nn(a, b, &[])
}

/// [`same`], where `nonnull[k]` says opaque value `k` is known not to be
/// NULL.
pub fn same_nn(a: &V, b: &V, nonnull: &[bool]) -> Option<bool> {
    let known = |k: &usize| nonnull.get(*k).copied().unwrap_or(false);
    match (a, b) {
        (V::Null, V::Null) => Some(true),
        (V::Rng(..), V::Null | V::Err(_)) | (V::Null | V::Err(_), V::Rng(..)) => Some(false),
        (V::Rng(..), _) | (_, V::Rng(..)) => rng_differs(a, b).map(|d| !d),
        // An unmodelled function may return NULL or fail for the same
        // arguments, so its result is not known to differ from either,
        // except from NULL for a function that never returns NULL. (Even
        // those can fail: `format()` with a bad format string raises.)
        (V::Null, V::Opq(k)) | (V::Opq(k), V::Null) if known(k) => Some(false),
        (V::Null | V::Err(_), V::Opq(_) | V::Ne(_))
        | (V::Opq(_) | V::Ne(_), V::Null | V::Err(_)) => None,
        (V::Null, _) | (_, V::Null) => Some(false),
        (V::Err(_), V::Err(_)) => Some(true),
        (V::Err(_), _) | (_, V::Err(_)) => Some(false),
        (V::Opq(x), V::Ne(y)) | (V::Ne(y), V::Opq(x)) if x == y => Some(false),
        (V::Opq(x), V::Opq(y)) | (V::Ne(x), V::Ne(y)) => Some(x == y).filter(|e| *e).or(None),
        (V::Opq(_) | V::Ne(_), _) | (_, V::Opq(_) | V::Ne(_)) => None,
        _ => {
            // Stored values: strings must be identical, numbers equal.
            if let (V::Str(x), V::Str(y)) = (a, b) {
                return if x == y {
                    Some(true)
                } else {
                    str_eq(x, y).map(|_| false).or(Some(false))
                };
            }
            eq_vals(a, b)
        }
    }
}

impl Ctx<'_> {
    fn resolve(&mut self, parts: &[String], scopes: &[Scope]) -> R<V> {
        let lower: Vec<String> = parts.iter().map(|p| p.to_lowercase()).collect();
        let col = lower.last().cloned().unwrap_or_default();
        // A parameter or variable (PL/pgSQL names; T-SQL variables always have `@`).
        if lower.len() == 1 && self.d != Dialect::Tsql && self.vars.contains_key(&col) {
            return self.var(&col);
        }
        if lower.len() == 2 && self.d != Dialect::Tsql && self.records.contains(&lower[0]) {
            return self.field(&lower[0], &lower[1]);
        }
        let qual = if lower.len() >= 2 {
            Some(lower[..lower.len() - 1].join("."))
        } else {
            None
        };
        // Innermost scope first.
        for scope in scopes.iter().rev() {
            let hits: Vec<&Binding> = match &qual {
                Some(q) => scope
                    .iter()
                    .filter(|b| b.names.iter().any(|n| n == q || n == last_part(q)))
                    .collect(),
                None => {
                    if scope.len() == 1 {
                        scope.iter().collect()
                    } else {
                        // Several tables: the catalog or derived columns decide.
                        let with: Vec<&Binding> = scope
                            .iter()
                            .filter(|b| self.has_column(b, &col) == Some(true))
                            .collect();
                        let unknown = scope.iter().any(|b| self.has_column(b, &col).is_none());
                        if with.len() == 1 && !unknown {
                            with
                        } else if with.is_empty() && !unknown {
                            Vec::new()
                        } else {
                            return unsupported(format!("ambiguous column {col}"));
                        }
                    }
                }
            };
            match hits.as_slice() {
                [] => continue,
                [b] => {
                    let b = (*b).clone();
                    return self.binding_col(&b, &col);
                }
                _ => return unsupported(format!("ambiguous name {}", lower.join("."))),
            }
        }
        unsupported(format!("unresolved name {}", lower.join(".")))
    }

    fn has_column(&self, b: &Binding, col: &str) -> Option<bool> {
        match &b.src {
            Src::Derived(cols) => Some(cols.iter().any(|(n, _)| n == col)),
            _ => {
                let t = self.schema.table(b.table.as_deref()?)?;
                Some(t.column(col).is_some())
            }
        }
    }

    fn binding_col(&mut self, b: &Binding, col: &str) -> R<V> {
        match &b.src {
            Src::Base(t, row) => self.column(t, *row, col),
            Src::Derived(cols) => match cols.iter().find(|(n, _)| n == col) {
                Some((_, v)) => Ok(v.clone()),
                None => unsupported(format!("unknown derived column {col}")),
            },
            Src::Nulls => Ok(V::Null),
        }
    }

    fn expr(&mut self, e: &Expr, scopes: &[Scope], g: Option<&Group>) -> R<V> {
        match e {
            Expr::Null => Ok(V::Null),
            Expr::Num(n) => parse_num(n),
            Expr::Str(s) => Ok(V::Str(s.clone())),
            Expr::Var(v) => self.var(v),
            Expr::Name(parts) => self.resolve(parts, scopes),
            Expr::Special(s) => match s.as_str() {
                "TRUE" => Ok(V::Bool(true)),
                "FALSE" => Ok(V::Bool(false)),
                _ => Ok(self.nondet(s)),
            },
            Expr::Star(_) => unsupported("* outside a select list"),
            Expr::Bin(op, l, r) => self.bin(op, l, r, scopes, g),
            Expr::Unary(op, x) => {
                let v = self.expr(x, scopes, g)?;
                match (op.as_str(), v) {
                    (_, V::Err(e)) => Ok(V::Err(e)),
                    ("NOT", v) => Ok(tri(truth(&v)?.map(|b| !b))),
                    ("-", V::Null) => Ok(V::Null),
                    ("-", V::Int(i)) => Ok(V::Int(-i)),
                    ("-", V::Num(f)) => Ok(V::Num(-f)),
                    _ => rej(),
                }
            }
            Expr::IsNull(x, neg) => {
                let v = self.expr(x, scopes, g)?;
                if let V::Err(e) = v {
                    return Ok(V::Err(e));
                }
                // Whether an unmodelled function returns NULL is not known.
                if let V::Opq(k) = v {
                    if self.nonnull.get(k).copied().unwrap_or(false) {
                        return Ok(V::Bool(*neg));
                    }
                }
                if matches!(v, V::Opq(_) | V::Ne(_)) {
                    return rej();
                }
                Ok(V::Bool(v.is_null() != *neg))
            }
            Expr::InList(x, list, neg) => {
                let v = self.expr(x, scopes, g)?;
                let mut acc = Some(false);
                for y in list {
                    let w = self.expr(y, scopes, g)?;
                    let t = self.compare("=", &v, &w)?;
                    acc = or3(acc, truth(&t)?);
                    if acc == Some(true) {
                        break;
                    }
                }
                Ok(tri(if *neg { acc.map(|b| !b) } else { acc }))
            }
            Expr::InQuery(x, q, neg) => {
                let v = self.expr(x, scopes, g)?;
                let rows = self.query(q, scopes, false)?;
                let mut acc = Some(false);
                for r in &rows.rows {
                    let Some(w) = r.first() else {
                        return rej();
                    };
                    let t = self.compare("=", &v, w)?;
                    acc = or3(acc, truth(&t)?);
                }
                Ok(tri(if *neg { acc.map(|b| !b) } else { acc }))
            }
            Expr::Between(x, lo, hi, neg) => {
                let v = self.expr(x, scopes, g)?;
                let l = self.expr(lo, scopes, g)?;
                let h = self.expr(hi, scopes, g)?;
                let a = truth(&self.compare(">=", &v, &l)?)?;
                let b = truth(&self.compare("<=", &v, &h)?)?;
                let r = and3(a, b);
                Ok(tri(if *neg { r.map(|b| !b) } else { r }))
            }
            Expr::Like(x, p, neg, ci) => {
                let v = self.expr(x, scopes, g)?;
                let pat = self.expr(p, scopes, g)?;
                match (&v, &pat) {
                    (V::Err(_), _) => Ok(v.clone()),
                    (_, V::Err(_)) => Ok(pat.clone()),
                    (V::Null, _) | (_, V::Null) => Ok(V::Null),
                    (V::Str(s), V::Str(pt)) => {
                        let m = like(s, pt, *ci).ok_or(Stop::Reject)?;
                        Ok(V::Bool(m != *neg))
                    }
                    _ => rej(),
                }
            }
            Expr::Cast(x, ty) => {
                let v = self.expr(x, scopes, g)?;
                self.cast(v, ty)
            }
            Expr::Case(op, whens, els) => {
                let base = match op {
                    Some(o) => Some(self.expr(o, scopes, g)?),
                    None => None,
                };
                for (w, t) in whens {
                    let c = match &base {
                        Some(b) => {
                            let wv = self.expr(w, scopes, g)?;
                            self.compare("=", b, &wv)?
                        }
                        None => self.expr(w, scopes, g)?,
                    };
                    if let V::Err(e) = c {
                        return Ok(V::Err(e));
                    }
                    if truth(&c)? == Some(true) {
                        return self.expr(t, scopes, g);
                    }
                }
                match els {
                    Some(e) => self.expr(e, scopes, g),
                    None => Ok(V::Null),
                }
            }
            Expr::Exists(q) => {
                let rows = self.query(q, scopes, true)?;
                Ok(V::Bool(!rows.rows.is_empty()))
            }
            Expr::Subquery(q) if q.for_json => {
                let rows = self.query(q, scopes, false)?;
                for_json(&rows)
            }
            Expr::Subquery(q) => {
                let rows = self.query(q, scopes, false)?;
                match rows.rows.as_slice() {
                    [] => Ok(V::Null),
                    [r] if r.len() == 1 => Ok(r[0].clone()),
                    [_] => rej(),
                    _ => Ok(V::Err("subquery returned more than one row".into())),
                }
            }
            Expr::Tuple(_) => unsupported("row value"),
            Expr::Extract(f, x) => {
                let v = self.expr(x, scopes, g)?;
                self.opaque_call(&format!("EXTRACT_{f}"), vec![v])
            }
            Expr::Call(c) => self.call(c, scopes, g),
        }
    }

    fn nondet(&mut self, what: &str) -> V {
        self.nondeterministic = true;
        let id = format!("{}#{}", what, self.side);
        // Clocks and NEWID() always return a value.
        self.opaque_value(id, what.to_string(), true)
    }

    fn bin(&mut self, op: &str, l: &Expr, r: &Expr, scopes: &[Scope], g: Option<&Group>) -> R<V> {
        match op {
            "AND" | "OR" => {
                let a = self.expr(l, scopes, g)?;
                if let V::Err(e) = a {
                    return Ok(V::Err(e));
                }
                let ta = truth(&a)?;
                // Short circuit only where the result is fixed either way.
                if op == "AND" && ta == Some(false) {
                    return Ok(V::Bool(false));
                }
                if op == "OR" && ta == Some(true) {
                    return Ok(V::Bool(true));
                }
                let b = self.expr(r, scopes, g)?;
                if let V::Err(e) = b {
                    return Ok(V::Err(e));
                }
                let tb = truth(&b)?;
                Ok(tri(if op == "AND" {
                    and3(ta, tb)
                } else {
                    or3(ta, tb)
                }))
            }
            "=" | "<>" | "<" | ">" | "<=" | ">=" => {
                // A bare name is read after the other side, which becomes
                // one of its candidate values (`col = f(x)`, `a.x = b.y`).
                let bare = |e: &Expr| matches!(e, Expr::Name(_) | Expr::Var(_));
                if bare(l) && !bare(r) {
                    let b = self.expr(r, scopes, g)?;
                    self.extra = Some(b.clone());
                    let a = self.expr(l, scopes, g);
                    self.extra = None;
                    return self.compare(op, &a?, &b);
                }
                let a = self.expr(l, scopes, g)?;
                if bare(r) {
                    self.extra = Some(a.clone());
                }
                let b = self.expr(r, scopes, g);
                self.extra = None;
                self.compare(op, &a, &b?)
            }
            "+" | "-" | "*" | "/" | "%" | "||" => {
                let a = self.expr(l, scopes, g)?;
                let b = self.expr(r, scopes, g)?;
                self.arith(op, a, b, l, r)
            }
            _ => unsupported(format!("operator {op}")),
        }
    }

    pub fn compare(&mut self, op: &str, a: &V, b: &V) -> R<V> {
        use std::cmp::Ordering::*;
        match (a, b) {
            (V::Err(e), _) | (_, V::Err(e)) => return Ok(V::Err(e.clone())),
            (V::Null, _) | (_, V::Null) => return Ok(V::Null),
            _ => {}
        }
        // PostgreSQL reads a string compared with a boolean as a boolean
        // (only a literal can be: text = boolean has no operator).
        if self.d == Dialect::Postgres {
            let as_bool = |v: &V| match v {
                V::Str(t) => pg_bool(t).map(V::Bool),
                _ => None,
            };
            match (a, b) {
                (V::Bool(_), V::Str(_)) => {
                    if let Some(y) = as_bool(b) {
                        return self.compare(op, a, &y);
                    }
                }
                (V::Str(_), V::Bool(_)) => {
                    if let Some(x) = as_bool(a) {
                        return self.compare(op, &x, b);
                    }
                }
                _ => {}
            }
        }
        let r = match op {
            "=" => eq_vals(a, b),
            "<>" => eq_vals(a, b).map(|x| !x),
            _ => cmp_vals(a, b).map(|o| match op {
                "<" => o == Less,
                ">" => o == Greater,
                "<=" => o != Greater,
                _ => o != Less,
            }),
        };
        match r {
            Some(x) => Ok(V::Bool(x)),
            None => {
                if std::env::var_os("SQLDIFF_DEBUG_SEARCH").is_some() {
                    eprintln!("    undecided {a:?} {op} {b:?}");
                }
                rej()
            }
        }
    }

    fn arith(&mut self, op: &str, a: V, b: V, le: &Expr, re: &Expr) -> R<V> {
        if let V::Err(e) = &a {
            return Ok(V::Err(e.clone()));
        }
        if let V::Err(e) = &b {
            return Ok(V::Err(e.clone()));
        }
        let strings = |x: &V, y: &V| matches!(x, V::Str(_)) && matches!(y, V::Str(_));
        // String operands of a numeric operator fail in both dialects, when
        // both are certainly strings.
        if matches!(op, "-" | "*" | "/" | "%")
            && self.static_string(le) == Some(true)
            && self.static_string(re) == Some(true)
        {
            return Ok(V::Err(format!(
                "the {} operator does not accept character strings",
                match op {
                    "-" => "subtract",
                    "*" => "multiply",
                    "/" => "divide",
                    _ => "modulo",
                }
            )));
        }
        if a.is_null() || b.is_null() {
            // PostgreSQL `||` and T-SQL `+` both yield NULL (CONCAT_NULL_YIELDS_NULL is ON by default).
            return Ok(V::Null);
        }
        let concat = op == "||" || (op == "+" && self.d == Dialect::Tsql && strings(&a, &b));
        if concat {
            return match (&a, &b) {
                (V::Str(x), V::Str(y)) => Ok(V::Str(format!("{x}{y}"))),
                (V::Str(x), V::Int(y)) if op == "||" => Ok(V::Str(format!("{x}{y}"))),
                (V::Int(x), V::Str(y)) if op == "||" => Ok(V::Str(format!("{x}{y}"))),
                (V::Opq(_) | V::Gen(_) | V::Ne(_), _) | (_, V::Opq(_) | V::Gen(_) | V::Ne(_)) => {
                    self.opaque_call(op, vec![a.clone(), b.clone()])
                }
                _ => rej(),
            };
        }
        if matches!(a, V::Ivl(..)) || matches!(b, V::Ivl(..)) {
            return self.interval_arith(op, a, b, le, re);
        }
        if matches!(a, V::Rng(..)) || matches!(b, V::Rng(..)) {
            let r = match (rng_parts(&a), rng_parts(&b), op) {
                (Some(x), Some(y), "+") => rng_op(x, y, |p, q| p + q),
                (Some(x), Some(y), "-") => rng_op(x, y, |p, q| p - q),
                (Some(x), Some(y), "*") => rng_op(x, y, |p, q| p * q),
                // Division by a constant that is not zero.
                (Some(x), Some(y), "/") if y.0 == y.1 && y.0 != 0.0 && !matches!(b, V::Rng(..)) => {
                    rng_op(x, y, |p, q| p / q)
                }
                _ => None,
            };
            return match r {
                Some(v) => Ok(v),
                None => self.opaque_call(op, vec![a, b]),
            };
        }
        match (&a, &b) {
            (V::Int(x), V::Int(y)) => {
                let r = match op {
                    "+" => x.checked_add(*y),
                    "-" => x.checked_sub(*y),
                    "*" => x.checked_mul(*y),
                    "/" | "%" if *y == 0 => return rej(),
                    "/" => x.checked_div(*y),
                    "%" => x.checked_rem(*y),
                    _ => None,
                };
                // Stay well inside every integer type the values could have.
                match r {
                    Some(v) if v.abs() < (1 << 30) => Ok(V::Int(v)),
                    _ => rej(),
                }
            }
            (V::Opq(_) | V::Gen(_) | V::Ne(_), _) | (_, V::Opq(_) | V::Gen(_) | V::Ne(_)) => {
                self.opaque_call(op, vec![a.clone(), b.clone()])
            }
            _ => {
                let (Some(x), Some(y)) = (num_of(&a), num_of(&b)) else {
                    return rej();
                };
                let r = match op {
                    "+" => x + y,
                    "-" => x - y,
                    "*" => x * y,
                    "/" if y == 0.0 => return rej(),
                    "/" => x / y,
                    _ => return rej(),
                };
                if r.is_finite() && r.abs() < 1e12 {
                    Ok(V::Num(r))
                } else {
                    rej()
                }
            }
        }
    }

    /// PostgreSQL interval arithmetic: interval plus or minus interval,
    /// interval times an integer, and a date or timestamp plus or minus an
    /// interval. The date operand must not be a bare literal (its type would
    /// be ambiguous) nor a character string (text has no such operator).
    fn interval_arith(&mut self, op: &str, a: V, b: V, le: &Expr, re: &Expr) -> R<V> {
        let opaque = |c: &mut Self, a: V, b: V| c.opaque_call(op, vec![a, b]);
        let stamp = |c: &Self, e: &Expr, v: &V| {
            matches!(v, V::Str(_)) && !matches!(e, Expr::Str(_)) && c.static_string(e) != Some(true)
        };
        let out_of_range = || V::Err("interval out of range".into());
        match (&a, &b, op) {
            (V::Ivl(m1, d1, u1), V::Ivl(m2, d2, u2), "+" | "-") => {
                let s = if op == "+" { 1 } else { -1 };
                match (m1.checked_add(s * m2), d1.checked_add(s * d2)) {
                    (Some(m), Some(d)) => Ok(V::Ivl(m, d, u1 + s as i128 * u2)),
                    _ => Ok(out_of_range()),
                }
            }
            (V::Ivl(m, d, u), V::Int(n), "*") | (V::Int(n), V::Ivl(m, d, u), "*") => {
                let n = i64::try_from(*n).ok();
                match n.and_then(|n| Some((m.checked_mul(n)?, d.checked_mul(n)?, n))) {
                    Some((m, d, n)) => Ok(V::Ivl(m, d, u * n as i128)),
                    None => Ok(out_of_range()),
                }
            }
            (V::Str(t), V::Ivl(m, d, u), "+" | "-") if stamp(self, le, &a) => {
                let s = if op == "+" { 1 } else { -1 };
                match add_interval(t, s * m, s * d, s as i128 * u) {
                    Some(r) => Ok(V::Str(r)),
                    None => opaque(self, a.clone(), b.clone()),
                }
            }
            (V::Ivl(m, d, u), V::Str(t), "+") if stamp(self, re, &b) => {
                match add_interval(t, *m, *d, *u) {
                    Some(r) => Ok(V::Str(r)),
                    None => opaque(self, a.clone(), b.clone()),
                }
            }
            _ => opaque(self, a, b),
        }
    }

    /// Whether an expression is certainly a character string (Some(true)),
    /// certainly not (Some(false)), or unknown.
    fn static_string(&self, e: &Expr) -> Option<bool> {
        static_string(e, &self.vars, self.d)
    }

    /// A value converted to a declared type, as a cast does.
    pub fn cast_value(&mut self, v: V, ty: &Type) -> R<V> {
        self.cast(v, ty)
    }

    fn cast(&mut self, v: V, ty: &Type) -> R<V> {
        if matches!(v, V::Null | V::Err(_)) {
            return Ok(v);
        }
        let t = Ty::from_type(ty, self.d, true);
        let tsql = self.d == Dialect::Tsql;
        // An integer that does not fit the target integer type.
        let int_fits = |i: i128| match t.len {
            Some(bits @ (16 | 32 | 64)) => {
                let lim = 1i128 << (bits - 1);
                (-lim..lim).contains(&i)
            }
            _ => true,
        };
        let overflow = || {
            V::Err(format!(
                "arithmetic overflow converting to {}",
                ty.name.to_lowercase()
            ))
        };
        match (t.fam, &v) {
            // Any non-zero number converts to bit 1.
            (Fam::Int, V::Int(i)) if t.len == Some(1) => Ok(V::Int(i128::from(*i != 0))),
            (Fam::Int, _) if t.len == Some(1) => rej(),
            (Fam::Int, V::Int(i)) => Ok(if int_fits(*i) { v } else { overflow() }),
            (Fam::Int, V::Str(s)) => match s.trim().parse::<i128>() {
                Ok(i) if int_fits(i) => Ok(V::Int(i)),
                _ => rej(),
            },
            (Fam::Int, V::Num(f)) => {
                // T-SQL truncates; PostgreSQL rounds (half away from zero for
                // numeric, half to even for float, so halves are left out).
                let r = if tsql {
                    f.trunc()
                } else if (f.fract().abs() - 0.5).abs() < 1e-9 {
                    return rej();
                } else {
                    f.round()
                };
                if r.abs() >= (1u64 << 30) as f64 {
                    return rej();
                }
                Ok(if int_fits(r as i128) {
                    V::Int(r as i128)
                } else {
                    overflow()
                })
            }
            (Fam::Exact, V::Int(_) | V::Num(_)) => {
                let x = num_of(&v).ok_or(Stop::Reject)?;
                let Some(scale) = t.scale else {
                    return Ok(if let V::Int(i) = v {
                        V::Num(i as f64)
                    } else {
                        v
                    });
                };
                // Rounded half away from zero to the scale; a value at a half
                // is left out (its binary approximation may round either way).
                let m = x * 10f64.powi(scale as i32);
                if (m.fract().abs() - 0.5).abs() < 1e-6 || m.abs() >= 1e15 {
                    return rej();
                }
                let r = m.round();
                if let Some(p) = t.len {
                    if r.abs() >= 10f64.powi(p as i32) {
                        return Ok(overflow());
                    }
                }
                let r = r / 10f64.powi(scale as i32);
                Ok(if r.fract() == 0.0 && scale == 0 {
                    V::Int(r as i128)
                } else {
                    V::Num(r)
                })
            }
            (Fam::Float, V::Int(i)) => Ok(V::Num(*i as f64)),
            (Fam::Float, V::Num(_)) => Ok(v),
            (Fam::VarChar | Fam::Char, V::Str(s)) => {
                // An explicit cast truncates silently in both dialects; a
                // shorter value in a fixed-length type is blank-padded.
                let n = s.chars().count() as u32;
                match t.len {
                    Some(l) if n > l => Ok(V::Str(s.chars().take(l as usize).collect())),
                    Some(l) if t.fam == Fam::Char && n < l => rej(),
                    _ => Ok(v),
                }
            }
            (Fam::VarChar | Fam::Char, V::Int(i)) => {
                let text = i.to_string();
                match t.len {
                    // T-SQL shows `*` (varchar) or fails (nvarchar) when too short.
                    Some(l) if text.len() as u32 > l => rej(),
                    Some(l) if t.fam == Fam::Char && (text.len() as u32) < l => rej(),
                    _ => Ok(V::Str(text)),
                }
            }
            (Fam::Date, V::Str(s)) => match parse_date(s) {
                Some(d) if d.rest.is_empty() || time_of(&d.rest).is_some() => {
                    Ok(V::Str(format_date(&DateLit {
                        sep: d.sep,
                        days: d.days,
                        rest: String::new(),
                    })))
                }
                _ => rej(),
            },
            (Fam::Timestamp, V::Str(s))
                if parse_date(s)
                    .is_some_and(|d| d.rest.is_empty() || time_of(&d.rest).is_some()) =>
            {
                Ok(v)
            }
            // PostgreSQL `name` and other string-like casts of strings.
            (Fam::Other, V::Str(_)) if matches!(ty.name.as_str(), "NAME" | "REGCLASS") => {
                if ty.name == "NAME" {
                    Ok(v)
                } else {
                    self.opaque_call(&format!("CAST_{}", ty.name), vec![v])
                }
            }
            (Fam::Bool, V::Bool(_)) => Ok(v),
            (Fam::Interval, V::Ivl(..)) => Ok(v),
            (Fam::Float, V::Rng(..)) => Ok(v),
            // Float to integer: T-SQL and Db2 truncate, PostgreSQL rounds;
            // both are monotone, so the ranges map end to end.
            (Fam::Int, V::Rng(lo, hi, ilo, ihi)) if t.len != Some(1) => {
                let f = |x: f64| {
                    if self.d == Dialect::Postgres {
                        // Halves go to the even neighbour.
                        if (x - x.trunc()).abs() == 0.5 {
                            2.0 * (x / 2.0).round()
                        } else {
                            x.round()
                        }
                    } else {
                        x.trunc()
                    }
                };
                if lo.abs() < 2e9 && hi.abs() < 2e9 {
                    Ok(V::Rng(f(*lo), f(*hi), f(*ilo), f(*ihi)))
                } else {
                    self.opaque_call(&format!("CAST_{}", ty.name), vec![v])
                }
            }
            (_, V::Rng(..)) => self.opaque_call(&format!("CAST_{}", ty.name), vec![v]),
            // Interval text in the forms it reads exactly; others stay opaque.
            (Fam::Interval, V::Str(s)) => match parse_interval(s) {
                Some((m, d, us)) => Ok(V::Ivl(m, d, us)),
                None => self.opaque_call("CAST_INTERVAL", vec![v]),
            },
            (_, V::Opq(_) | V::Gen(_) | V::Ne(_)) => {
                self.opaque_call(&format!("CAST_{}", ty.name), vec![v])
            }
            _ => rej(),
        }
    }

    /// A deterministic function the evaluator does not model: an opaque
    /// value per function and argument values.
    fn opaque_call(&mut self, name: &str, args: Vec<V>) -> R<V> {
        if args.iter().any(|a| matches!(a, V::Err(_))) {
            return Ok(args.into_iter().find(|a| matches!(a, V::Err(_))).unwrap());
        }
        let show_args: Vec<String> = args.iter().map(|a| a.show(&self.opaque)).collect();
        let mut id = format!("{name}({:?})", args);
        // A function of a random value: the two versions draw their own.
        if args.iter().any(|a| matches!(a, V::Rng(..))) {
            self.nondeterministic = true;
            id = format!("{id}#{}", self.side);
        }
        let show = format!("{}({})", name.to_lowercase(), show_args.join(", "));
        let nonnull = self.never_null(name, &args);
        Ok(self.opaque_value(id, show, nonnull))
    }

    /// Functions that never return NULL for arguments of these values
    /// (checked on PostgreSQL 18); they may still fail.
    fn never_null(&self, name: &str, args: &[V]) -> bool {
        let given = |i: usize| {
            args.get(i).is_some_and(|a| {
                !matches!(a, V::Null | V::Ne(_))
                    && !matches!(a, V::Opq(k) if !self.nonnull.get(*k).copied().unwrap_or(false))
            })
        };
        match (self.d, name.to_uppercase().as_str()) {
            // format(NULL) is NULL; NULL arguments format as ''.
            (Dialect::Postgres, "FORMAT") => given(0),
            // An unknown role id gives 'unknown (OID=n)'.
            (Dialect::Postgres, "PG_GET_USERBYID") => given(0),
            (Dialect::Postgres, "QUOTE_IDENT" | "QUOTE_LITERAL") => given(0),
            // An aggregate over one value that is not NULL.
            (_, "STRING_AGG" | "ARRAY_AGG") => given(0),
            _ => false,
        }
    }

    fn call(&mut self, c: &Call, scopes: &[Scope], g: Option<&Group>) -> R<V> {
        let name = call_name(c);
        if c.over || c.within_group {
            return unsupported("window or ordered-set function");
        }
        if is_aggregate(&Expr::Call(Box::new(c.clone()))) {
            let Some(g) = g else {
                return unsupported("aggregate outside a grouped query");
            };
            return self.aggregate(c, &name, scopes, g);
        }
        if CLOCK.contains(&name.as_str()) {
            // Both versions run at the same moment: the time is an input.
            let cands = vec![
                V::Str("2020-01-01 10:00:00".into()),
                V::Str("2020-01-31 23:59:59".into()),
            ];
            return self.input(
                Key::Var(format!("@now:{}", name.to_lowercase())),
                Origin::Free,
                cands,
            );
        }
        // Session and server state both versions share: an input.
        if let Some(cands) = environment(&name, &c.args, self.d) {
            let args: Vec<String> = c
                .args
                .iter()
                .map(|a| match a {
                    Expr::Str(s) if self.d == Dialect::Tsql => {
                        format!("N'{}'", s.replace('\'', "''"))
                    }
                    Expr::Str(s) => format!("'{}'", s.replace('\'', "''")),
                    Expr::Num(n) => n.clone(),
                    _ => String::new(),
                })
                .collect();
            let key = format!("@env:{name}({})", args.join(", "));
            return self.input(Key::Var(key), Origin::Free, cands);
        }
        // RAND() / random() without a seed: a random range.
        if c.args.is_empty()
            && ((self.d != Dialect::Postgres && name == "RAND")
                || (self.d == Dialect::Postgres && name == "RANDOM"))
        {
            self.nondeterministic = true;
            return Ok(RAND_RANGE);
        }
        if NONDETERMINISTIC.contains(&name.as_str()) {
            return Ok(self.nondet(&name));
        }
        // Date part names are words, not columns.
        if matches!(
            name.as_str(),
            "DATEADD" | "DATEPART" | "DATEDIFF" | "DATENAME"
        ) {
            if let Some(Expr::Name(p)) = c.args.first() {
                let part = p.join(".").to_uppercase();
                let mut args = Vec::new();
                for a in &c.args[1..] {
                    args.push(self.expr(a, scopes, g)?);
                }
                if let Some(e) = args.iter().find(|a| matches!(a, V::Err(_))) {
                    return Ok(e.clone());
                }
                return self.date_call(&name, &part, args);
            }
        }
        // CONVERT(type, value [, style]).
        if matches!(name.as_str(), "CONVERT" | "TRY_CONVERT")
            && self.d == Dialect::Tsql
            && c.args.len() >= 2
        {
            let ty = match &c.args[0] {
                Expr::Name(p) if p.len() == 1 => Type {
                    name: p[0].to_uppercase(),
                    args: Vec::new(),
                },
                Expr::Call(t) if t.name.len() == 1 => Type {
                    name: t.name[0].to_uppercase(),
                    args: t
                        .args
                        .iter()
                        .map(|a| match a {
                            Expr::Num(n) => n.clone(),
                            Expr::Name(p) => p.join("."),
                            _ => String::new(),
                        })
                        .collect(),
                },
                _ => return unsupported("CONVERT target type"),
            };
            let v = self.expr(&c.args[1], scopes, g)?;
            let style = match c.args.get(2) {
                Some(Expr::Num(n)) => n.parse::<u32>().ok(),
                None => None,
                _ => return unsupported("CONVERT style"),
            };
            return match (style, &v) {
                (_, V::Null | V::Err(_)) => Ok(v),
                (Some(st), V::Str(d)) if parse_date(d).is_some() => {
                    let Some(text) = date_style(d, st) else {
                        return unsupported(format!("CONVERT style {st}"));
                    };
                    self.cast(V::Str(text), &ty).or_else(|_| {
                        // Silent truncation to the target length.
                        let t = Ty::from_type(&ty, self.d, true);
                        match (t.fam, t.len) {
                            (Fam::VarChar | Fam::Char, Some(l)) => Ok(V::Str(
                                date_style(d, st)
                                    .unwrap()
                                    .chars()
                                    .take(l as usize)
                                    .collect(),
                            )),
                            _ => rej(),
                        }
                    })
                }
                (None, _) => self.cast(v, &ty),
                _ => self.opaque_call(
                    &format!("CONVERT_{}", ty.name),
                    vec![v, V::Int(style.unwrap_or(0) as i128)],
                ),
            };
        }
        let mut args = Vec::with_capacity(c.args.len());
        for a in &c.args {
            args.push(self.expr(a, scopes, g)?);
        }
        if let Some(e) = args.iter().find(|a| matches!(a, V::Err(_))) {
            return Ok(e.clone());
        }
        let tsql = self.d == Dialect::Tsql;
        match (name.as_str(), args.as_slice()) {
            ("COALESCE", _) if !args.is_empty() => Ok(args
                .iter()
                .find(|a| !a.is_null())
                .cloned()
                .unwrap_or(V::Null)),
            ("ISNULL", [a, b]) if tsql => {
                // The replacement is converted to the first argument's type.
                if a.is_null() && !matches!(b, V::Null | V::Int(_)) {
                    return rej();
                }
                Ok(if a.is_null() { b.clone() } else { a.clone() })
            }
            ("NULLIF", [a, b]) => {
                if a.is_null() {
                    return Ok(V::Null);
                }
                match truth(&self.compare("=", a, b)?)? {
                    Some(true) => Ok(V::Null),
                    _ => Ok(a.clone()),
                }
            }
            ("YEAR" | "MONTH" | "DAY", [V::Str(d)]) if tsql && parse_date(d).is_some() => {
                let (y, m, dd) = civil_from_days(parse_date(d).unwrap().days);
                Ok(V::Int(match name.as_str() {
                    "YEAR" => y,
                    "MONTH" => m,
                    _ => dd,
                } as i128))
            }
            ("NCHAR" | "CHAR", [V::Int(i)])
                if tsql && ((32..127).contains(i) || matches!(i, 9 | 10 | 13)) =>
            {
                Ok(V::Str(char::from(*i as u8).to_string()))
            }
            (
                "IS TRUE" | "IS NOT TRUE" | "IS FALSE" | "IS NOT FALSE" | "IS UNKNOWN"
                | "IS NOT UNKNOWN",
                [v],
            ) => {
                let t = truth(v)?;
                let hit = match name.trim_start_matches("IS ").trim_start_matches("NOT ") {
                    "TRUE" => t == Some(true),
                    "FALSE" => t == Some(false),
                    _ => t.is_none(),
                };
                Ok(V::Bool(hit != name.starts_with("IS NOT")))
            }
            ("SPLIT_PART", [V::Str(s), V::Str(sep), V::Int(n)]) if !tsql => {
                if *n == 0 {
                    return Ok(V::Err("field position must not be zero".into()));
                }
                let fields: Vec<&str> = if sep.is_empty() {
                    vec![s.as_str()]
                } else {
                    s.split(sep.as_str()).collect()
                };
                let k = if *n > 0 {
                    (*n - 1) as usize
                } else {
                    match fields.len().checked_sub(n.unsigned_abs() as usize) {
                        Some(k) => k,
                        None => return Ok(V::Str(String::new())),
                    }
                };
                Ok(V::Str(fields.get(k).copied().unwrap_or("").to_string()))
            }
            ("SUBSTRING" | "SUBSTR", [V::Str(s), V::Int(start), rest @ ..])
                if rest.len() <= 1 && (tsql == (rest.len() == 1)) =>
            {
                let chars: Vec<char> = s.chars().collect();
                let end = match rest {
                    [V::Int(l)] if *l < 0 => {
                        return Ok(V::Err("negative substring length".into()));
                    }
                    [V::Int(l)] => start.saturating_add(*l),
                    [] => chars.len() as i128 + 1,
                    _ => return self.opaque_call(&name, args.clone()),
                };
                let from = (*start).max(1);
                let to = end.min(chars.len() as i128 + 1);
                Ok(V::Str(if from >= to {
                    String::new()
                } else {
                    chars[(from - 1) as usize..(to - 1) as usize]
                        .iter()
                        .collect()
                }))
            }
            ("LEFT" | "RIGHT", [V::Str(s), V::Int(n)]) => {
                let chars: Vec<char> = s.chars().collect();
                let len = chars.len() as i128;
                // PostgreSQL counts a negative length from the other end.
                let k = match *n {
                    n if n < 0 && tsql => {
                        return Ok(V::Err(format!(
                            "invalid length passed to {}",
                            name.to_lowercase()
                        )));
                    }
                    n if n < 0 => (len + n).max(0),
                    n => n.min(len),
                } as usize;
                Ok(V::Str(if name == "LEFT" {
                    chars[..k].iter().collect()
                } else {
                    chars[chars.len() - k..].iter().collect()
                }))
            }
            ("LTRIM" | "RTRIM" | "TRIM", [V::Str(s)]) => Ok(V::Str(match name.as_str() {
                "LTRIM" => s.trim_start_matches(' ').to_string(),
                "RTRIM" => s.trim_end_matches(' ').to_string(),
                _ => s.trim_matches(' ').to_string(),
            })),
            ("REPLACE", [V::Str(s), V::Str(from), V::Str(to)]) => {
                if from.is_empty() {
                    return Ok(V::Str(s.clone()));
                }
                let exact = s.replace(from.as_str(), to);
                // T-SQL matches under the collation, usually case-insensitive:
                // keep only results that do not depend on it.
                if tsql
                    && (!s.is_ascii() || !from.is_ascii() || {
                        let (ls, lf) = (s.to_ascii_lowercase(), from.to_ascii_lowercase());
                        ls.matches(lf.as_str()).count() != s.matches(from.as_str()).count()
                    })
                {
                    return rej();
                }
                Ok(V::Str(exact))
            }
            ("REVERSE", [V::Str(s)]) => Ok(V::Str(s.chars().rev().collect())),
            ("REPLICATE" | "REPEAT", [V::Str(s), V::Int(n)]) => {
                if *n < 0 {
                    return Ok(if tsql { V::Null } else { V::Str(String::new()) });
                }
                if (*n as usize).saturating_mul(s.len()) > 4000 {
                    return rej();
                }
                Ok(V::Str(s.repeat(*n as usize)))
            }
            ("LPAD" | "RPAD", [V::Str(s), V::Int(n), fill @ ..]) if !tsql && fill.len() <= 1 => {
                let fill = match fill {
                    [V::Str(f)] => f.clone(),
                    [] => " ".to_string(),
                    _ => return self.opaque_call(&name, args.clone()),
                };
                let chars: Vec<char> = s.chars().collect();
                let n = (*n).max(0) as usize;
                if n > 4000 {
                    return rej();
                }
                if chars.len() >= n || fill.is_empty() {
                    return Ok(V::Str(chars.iter().take(n).collect()));
                }
                let pad: String = fill.chars().cycle().take(n - chars.len()).collect();
                Ok(V::Str(if name == "LPAD" {
                    pad + s
                } else {
                    s.clone() + &pad
                }))
            }
            ("STRPOS", [V::Str(s), V::Str(sub)]) if !tsql => {
                Ok(V::Int(match s.find(sub.as_str()) {
                    Some(i) => s[..i].chars().count() as i128 + 1,
                    None => 0,
                }))
            }
            ("CHARINDEX", [V::Str(sub), V::Str(s)]) if tsql => {
                if sub.is_empty() {
                    return Ok(V::Int(0));
                }
                let pos = |h: &str, n: &str| {
                    h.find(n)
                        .map(|i| h[..i].chars().count() as i128 + 1)
                        .unwrap_or(0)
                };
                let exact = pos(s, sub);
                if !s.is_ascii()
                    || !sub.is_ascii()
                    || pos(&s.to_ascii_lowercase(), &sub.to_ascii_lowercase()) != exact
                {
                    return rej();
                }
                Ok(V::Int(exact))
            }
            ("GREATEST" | "LEAST", _) if !args.is_empty() => {
                // Both dialects skip NULL arguments.
                let mut best: Option<V> = None;
                for a in args.iter().filter(|a| !a.is_null()) {
                    best = Some(match best {
                        None => a.clone(),
                        Some(b) => {
                            let o = cmp_vals(a, &b).ok_or(Stop::Reject)?;
                            let better = if name == "GREATEST" {
                                o == std::cmp::Ordering::Greater
                            } else {
                                o == std::cmp::Ordering::Less
                            };
                            if better {
                                a.clone()
                            } else {
                                b
                            }
                        }
                    });
                }
                Ok(best.unwrap_or(V::Null))
            }
            ("ROUND", [x @ (V::Int(_) | V::Num(_)), V::Int(n)]) if (-9..=9).contains(n) => {
                // Half away from zero (exact numerics); halves are left out
                // because a float may sit just beside them.
                let v = num_of(x).ok_or(Stop::Reject)?;
                let m = v * 10f64.powi(*n as i32);
                if (m.fract().abs() - 0.5).abs() < 1e-6 || m.abs() >= 1e12 {
                    return rej();
                }
                let r = m.round() / 10f64.powi(*n as i32);
                Ok(match x {
                    V::Int(_) => V::Int(r as i128),
                    _ => V::Num(r),
                })
            }
            ("FLOOR" | "CEILING" | "CEIL", [V::Int(i)]) => Ok(V::Int(*i)),
            ("FLOOR" | "CEILING" | "CEIL", [V::Rng(..)]) => {
                let x = rng_parts(&args[0]).unwrap();
                let f = |v: f64| if name == "FLOOR" { v.floor() } else { v.ceil() };
                Ok(rng_op(x, (0.0, 0.0, 0.0, 0.0), |p, _| f(p)).ok_or(Stop::Reject)?)
            }
            ("ABS", [V::Rng(..)]) => {
                let (lo, hi, ilo, ihi) = rng_parts(&args[0]).unwrap();
                let abs = |l: f64, h: f64, inner: bool| {
                    if l >= 0.0 {
                        (l, h)
                    } else if h <= 0.0 {
                        (-h, -l)
                    } else if inner {
                        // Values near both ends occur; whether values near 0 do is not known.
                        ((-l).min(h), (-l).max(h))
                    } else {
                        (0.0, h.max(-l))
                    }
                };
                let ((a, b), (c, d)) = (abs(lo, hi, false), abs(ilo, ihi, true));
                Ok(V::Rng(a, b, c, d))
            }
            ("FLOOR" | "CEILING" | "CEIL", [V::Num(f)]) if f.abs() < 1e12 => {
                Ok(V::Num(if name == "FLOOR" { f.floor() } else { f.ceil() }))
            }
            ("SIGN", [V::Int(i)]) => Ok(V::Int(i.signum())),
            ("ABS", [V::Null]) => Ok(V::Null),
            ("ABS", [V::Int(i)]) => Ok(V::Int(i.abs())),
            ("ABS", [V::Num(f)]) => Ok(V::Num(f.abs())),
            (
                "UPPER" | "LOWER" | "LEN" | "LENGTH" | "CHAR_LENGTH" | "LTRIM" | "RTRIM" | "TRIM",
                [V::Null],
            ) => Ok(V::Null),
            ("LEN", [V::Str(s)]) if tsql => {
                Ok(V::Int(s.trim_end_matches(' ').chars().count() as i128))
            }
            ("LENGTH" | "CHAR_LENGTH", [V::Str(s)]) if !tsql => {
                Ok(V::Int(s.chars().count() as i128))
            }
            ("UPPER", [V::Str(s)]) if s.is_ascii() => Ok(V::Str(s.to_uppercase())),
            ("LOWER", [V::Str(s)]) if s.is_ascii() => Ok(V::Str(s.to_lowercase())),
            ("CONCAT", _)
                if tsql
                    || args
                        .iter()
                        .all(|a| matches!(a, V::Str(_) | V::Null | V::Int(_))) =>
            {
                let mut s = String::new();
                for a in &args {
                    match a {
                        V::Null => {}
                        V::Str(x) => s.push_str(x),
                        V::Int(i) => s.push_str(&i.to_string()),
                        _ => return self.opaque_call(&name, args.clone()),
                    }
                }
                Ok(V::Str(s))
            }
            _ => {
                if args.iter().any(|a| a.is_null()) && null_strict(&name) {
                    return Ok(V::Null);
                }
                self.opaque_call(&name, args)
            }
        }
    }

    /// DATEADD / DATEPART / DATEDIFF on dates kept as ISO strings.
    fn date_call(&mut self, name: &str, part: &str, args: Vec<V>) -> R<V> {
        if args.iter().any(V::is_null) {
            return Ok(V::Null);
        }
        if name == "DATEPART" && matches!(part, "WEEKDAY" | "DW") && self.d == Dialect::Tsql {
            let [V::Str(d)] = args.as_slice() else {
                return self.opaque_call(name, args);
            };
            let Some(dl) = parse_date(d) else {
                return self.opaque_call(name, args);
            };
            // The day number depends on SET DATEFIRST, a session setting both
            // versions share: an input (7 is the us_english default).
            let first = self.input(
                Key::Var("@@datefirst".into()),
                Origin::Free,
                vec![V::Int(7), V::Int(1)],
            )?;
            let V::Int(first) = first else {
                return rej();
            };
            // ISO day of the week, Monday 1 to Sunday 7 (1970-01-01 was a Thursday).
            let iso = (dl.days + 3).rem_euclid(7) + 1;
            return Ok(V::Int(((iso - first as i64 + 7).rem_euclid(7) + 1) as i128));
        }
        let unit = match part {
            "YEAR" | "YY" | "YYYY" => "year",
            "MONTH" | "MM" | "M" => "month",
            "DAY" | "DD" | "D" => "day",
            "HOUR" | "HH" => "hour",
            "MINUTE" | "MI" | "N" => "minute",
            "SECOND" | "SS" | "S" => "second",
            _ => return self.opaque_call(&format!("{name}_{part}"), args),
        };
        match (name, args.as_slice()) {
            ("DATEPART", [V::Str(d)]) => {
                let Some(dl) = parse_date(d) else {
                    return self.opaque_call(name, args);
                };
                let (y, m, dd) = civil_from_days(dl.days);
                let Some((h, mi, sec, _)) = time_of(&dl.rest) else {
                    return self.opaque_call(name, args);
                };
                Ok(V::Int(match unit {
                    "year" => y,
                    "month" => m,
                    "day" => dd,
                    "hour" => h,
                    "minute" => mi,
                    _ => sec,
                } as i128))
            }
            ("DATEADD", [V::Int(n), V::Str(d)]) => {
                let Some(mut dl) = parse_date(d) else {
                    return self.opaque_call(name, args);
                };
                let n = *n as i64;
                match unit {
                    "day" => dl.days += n,
                    "month" | "year" => {
                        let (y, m, dd) = civil_from_days(dl.days);
                        let months = y * 12 + (m - 1) + if unit == "year" { n * 12 } else { n };
                        let (ny, nm) = (months.div_euclid(12), months.rem_euclid(12) + 1);
                        // The day is clamped to the end of the month.
                        let mut nd = dd;
                        while civil_from_days(days_from_civil(ny, nm, nd)) != (ny, nm, nd) {
                            nd -= 1;
                        }
                        dl.days = days_from_civil(ny, nm, nd);
                    }
                    _ => {
                        // Time units need a time part to add to.
                        let Some((h, mi, sec, frac)) =
                            time_of(&dl.rest).filter(|_| !dl.rest.is_empty())
                        else {
                            return self.opaque_call(name, args);
                        };
                        let step = match unit {
                            "hour" => 3600,
                            "minute" => 60,
                            _ => 1,
                        };
                        let total = h * 3600 + mi * 60 + sec + n * step;
                        dl.days += total.div_euclid(86400);
                        let t = total.rem_euclid(86400);
                        let lead = &dl.rest[..1];
                        dl.rest = format!(
                            "{lead}{:02}:{:02}:{:02}{frac}",
                            t / 3600,
                            t / 60 % 60,
                            t % 60
                        );
                    }
                }
                let (y, _, _) = civil_from_days(dl.days);
                if !(1..=9999).contains(&y) {
                    return Ok(V::Err("date out of range".into()));
                }
                Ok(V::Str(format_date(&dl)))
            }
            _ => self.opaque_call(&format!("{name}_{part}"), args),
        }
    }

    fn aggregate(&mut self, c: &Call, name: &str, scopes: &[Scope], g: &Group) -> R<V> {
        let rows = g.rows;
        if c.star {
            return Ok(V::Int(rows.len() as i128));
        }
        let Some(arg) = c.args.first() else {
            return unsupported("aggregate without argument");
        };
        let mut vals = Vec::new();
        for r in rows {
            let mut sc: Vec<Scope> = scopes.to_vec();
            sc.push(r.clone());
            let v = self.expr(arg, &sc, None)?;
            if let V::Err(e) = v {
                return Ok(V::Err(e));
            }
            // Aggregates skip NULLs: an unmodelled value that may be NULL
            // leaves the count of values unknown.
            if let V::Opq(k) = v {
                if !self.nonnull.get(k).copied().unwrap_or(false) {
                    return rej();
                }
            }
            if !v.is_null() {
                vals.push(v);
            }
        }
        if c.distinct {
            let mut d: Vec<V> = Vec::new();
            for v in vals {
                let mut dup = false;
                for x in &d {
                    match eq_vals(x, &v) {
                        Some(true) => dup = true,
                        Some(false) => {}
                        None => return rej(),
                    }
                }
                if !dup {
                    d.push(v);
                }
            }
            vals = d;
        }
        match name {
            "COUNT" | "COUNT_BIG" => Ok(V::Int(vals.len() as i128)),
            _ if vals.is_empty() => Ok(V::Null),
            "MIN" | "MAX" => {
                let mut best = vals[0].clone();
                for v in &vals[1..] {
                    let o = cmp_vals(v, &best).ok_or(Stop::Reject)?;
                    if (name == "MIN" && o == std::cmp::Ordering::Less)
                        || (name == "MAX" && o == std::cmp::Ordering::Greater)
                    {
                        best = v.clone();
                    }
                }
                Ok(best)
            }
            "SUM" => {
                let mut acc = vals[0].clone();
                for v in &vals[1..] {
                    acc = self.arith("+", acc, v.clone(), &Expr::Null, &Expr::Null)?;
                }
                Ok(acc)
            }
            // Other aggregates of one value: a function of that value.
            _ if vals.len() == 1 && c.order.is_empty() && name != "AVG" => {
                let extra: Vec<V> = Vec::new();
                let mut args = vals.clone();
                args.extend(extra);
                self.opaque_call(name, args)
            }
            // AVG of integers truncates in T-SQL and is exact in PostgreSQL;
            // only averages that come out the same either way are kept.
            "AVG" => {
                if vals.iter().all(|v| matches!(v, V::Int(_))) {
                    let sum: i128 = vals
                        .iter()
                        .map(|v| if let V::Int(i) = v { *i } else { 0 })
                        .sum();
                    let n = vals.len() as i128;
                    return if sum % n == 0 {
                        Ok(V::Int(sum / n))
                    } else {
                        rej()
                    };
                }
                let mut sum = 0.0;
                for v in &vals {
                    sum += num_of(v).ok_or(Stop::Reject)?;
                }
                let avg = sum / vals.len() as f64;
                if ((avg * 1e6).fract()).abs() > 1e-6 || avg.abs() >= 1e9 {
                    return rej();
                }
                Ok(V::Num(avg))
            }
            _ => rej(),
        }
    }
}

/// Whether an expression is certainly a character string (Some(true)),
/// certainly not (Some(false)), or unknown, from literals, declared variable
/// types, casts and string functions.
pub fn static_string(e: &Expr, vars: &HashMap<String, VarFact>, d: Dialect) -> Option<bool> {
    static_string_with(e, vars, d, &|_| None)
}

/// [`static_string`], with the types of columns (by their written name).
pub fn static_string_with(
    e: &Expr,
    vars: &HashMap<String, VarFact>,
    d: Dialect,
    col: &dyn Fn(&[String]) -> Option<bool>,
) -> Option<bool> {
    let static_string =
        |e: &Expr, vars: &HashMap<String, VarFact>, d: Dialect| static_string_with(e, vars, d, col);
    match e {
        Expr::Str(_) => Some(true),
        Expr::Num(_) => Some(false),
        Expr::Var(v) => {
            let n = var_key(v);
            vars.get(&n)?.ty.as_ref().map(|t| t.is_string())
        }
        Expr::Name(p) if p.len() == 1 && d != Dialect::Tsql => {
            let n = p[0].to_lowercase();
            vars.get(&n)?.ty.as_ref().map(|t| t.is_string())
        }
        Expr::Bin(op, l, r) if op == "+" || op == "||" => {
            match (static_string(l, vars, d), static_string(r, vars, d)) {
                (Some(true), Some(true)) => Some(true),
                _ => None,
            }
        }
        Expr::Cast(_, ty) => Some(Ty::from_type(ty, d, true).is_string()),
        // T-SQL types a CASE by its result expressions.
        Expr::Case(_, whens, els) => {
            let mut all = whens.iter().map(|(_, r)| r).chain(els.as_deref());
            let mut typed = false;
            let strings = all.all(|r| {
                let null = matches!(r, Expr::Null);
                typed |= !null;
                null || static_string(r, vars, d) == Some(true)
            });
            (strings && typed).then_some(true)
        }
        Expr::Name(p) => col(p),
        Expr::Call(c) => match call_name(c).as_str() {
            "QUOTENAME" | "UPPER" | "LOWER" | "LTRIM" | "RTRIM" | "TRIM" | "REPLACE" | "CONCAT"
            | "FORMAT" | "NCHAR" | "CHAR" | "LEFT" | "RIGHT" | "SUBSTRING" | "STUFF"
            | "REPLICATE" => Some(true),
            // Names of catalog objects (sysname or nvarchar).
            "SCHEMA_NAME" | "OBJECT_NAME" | "OBJECT_SCHEMA_NAME" | "DB_NAME" | "TYPE_NAME"
            | "COL_NAME" | "USER_NAME" | "SUSER_SNAME" | "SUSER_NAME" | "APP_NAME"
            | "HOST_NAME"
                if d == Dialect::Tsql =>
            {
                Some(true)
            }
            _ => None,
        },
        _ => None,
    }
}

/// Candidate values of a function that reads session or server state, when
/// its arguments are constants.
fn environment(name: &str, args: &[Expr], d: Dialect) -> Option<Vec<V>> {
    if !args
        .iter()
        .all(|a| matches!(a, Expr::Str(_) | Expr::Num(_)))
    {
        return None;
    }
    let ints = |v: &[i128]| v.iter().map(|i| V::Int(*i)).collect::<Vec<_>>();
    match (d, name, args) {
        (Dialect::Tsql, "IS_ROLEMEMBER" | "IS_MEMBER" | "IS_SRVROLEMEMBER", [_] | [_, _]) => {
            let mut v = ints(&[1, 0]);
            v.push(V::Null);
            Some(v)
        }
        // The documented engine editions.
        (Dialect::Tsql, "SERVERPROPERTY", [Expr::Str(p)])
            if p.eq_ignore_ascii_case("EngineEdition") =>
        {
            Some(ints(&[2, 3, 4, 5, 6, 8, 9, 11, 12]))
        }
        // Values an application stores under a key, compared as strings.
        (Dialect::Tsql, "SESSION_CONTEXT", [Expr::Str(_)]) => {
            Some(vec![V::Str("x".into()), V::Str("y".into()), V::Null])
        }
        // Yes/no server properties (1 or 0 for a valid property name).
        (Dialect::Tsql, "SERVERPROPERTY", [Expr::Str(p)])
            if matches!(
                p.to_ascii_uppercase().as_str(),
                "ISXTPSUPPORTED"
                    | "ISFULLTEXTINSTALLED"
                    | "ISHADRENABLED"
                    | "ISCLUSTERED"
                    | "ISINTEGRATEDSECURITYONLY"
                    | "ISSINGLEUSER"
                    | "ISPOLYBASEINSTALLED"
                    | "ISADVANCEDANALYTICSINSTALLED"
            ) =>
        {
            Some(ints(&[1, 0]))
        }
        (Dialect::Postgres, "PG_IS_IN_RECOVERY", []) => Some(vec![V::Bool(false), V::Bool(true)]),
        // The server version as text: the first release of each major
        // version still met in practice.
        (Dialect::Postgres, "CURRENT_SETTING", [Expr::Str(p)])
            if p.eq_ignore_ascii_case("server_version_num") =>
        {
            Some(
                [180000, 170000, 160000, 150000, 140000, 130000, 120000]
                    .iter()
                    .map(|v: &i32| V::Str(v.to_string()))
                    .collect(),
            )
        }
        _ => None,
    }
}

/// Functions that return NULL for any NULL argument.
fn null_strict(name: &str) -> bool {
    matches!(
        name,
        "UPPER"
            | "LOWER"
            | "LEN"
            | "LENGTH"
            | "SUBSTRING"
            | "SUBSTR"
            | "LEFT"
            | "RIGHT"
            | "REPLACE"
            | "QUOTENAME"
            | "SPLIT_PART"
            | "DATEADD"
            | "DATEDIFF"
            | "DATEPART"
            | "YEAR"
            | "MONTH"
            | "DAY"
            | "ROUND"
            | "FLOOR"
            | "CEILING"
            | "LPAD"
            | "RPAD"
            | "TO_HEX"
            | "POSITION"
            | "STRPOS"
    )
}

fn and3(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    }
}

fn or3(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    match (a, b) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    }
}

/// LIKE with `%` and `_` (no character classes); None when the answer could
/// depend on the collation.
fn like(s: &str, p: &str, ci: bool) -> Option<bool> {
    if p.contains('[') || !s.is_ascii() || !p.is_ascii() {
        return None;
    }
    let (s, p): (Vec<char>, Vec<char>) = if ci {
        (
            s.to_lowercase().chars().collect(),
            p.to_lowercase().chars().collect(),
        )
    } else {
        (s.chars().collect(), p.chars().collect())
    };
    fn m(s: &[char], p: &[char]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some('%') => (0..=s.len()).any(|i| m(&s[i..], &p[1..])),
            Some('_') => !s.is_empty() && m(&s[1..], &p[1..]),
            Some(c) => s.first() == Some(c) && m(&s[1..], &p[1..]),
        }
    }
    let r = m(&s, &p);
    // A case-sensitive mismatch that only differs by case is collation-dependent.
    if !ci && !r {
        let sl: String = s.iter().collect::<String>().to_lowercase();
        let pl: String = p.iter().collect::<String>().to_lowercase();
        let sl: Vec<char> = sl.chars().collect();
        let pl: Vec<char> = pl.chars().collect();
        if m(&sl, &pl) {
            return None;
        }
    }
    Some(r)
}

// ---- queries ---------------------------------------------------------------------

/// Rows a query returns, with output column names.
#[derive(Debug, Clone)]
pub struct Rows {
    pub names: Vec<String>,
    pub rows: Vec<Vec<V>>,
    pub ordered: bool,
}

impl Ctx<'_> {
    /// Rows of one FROM item: each a list of bindings.
    fn scope_of(&mut self, f: &From, outer: &[Scope]) -> R<Vec<Scope>> {
        match f {
            From::Table { name, alias, hints } => {
                if let [single] = name.as_slice() {
                    if let Some((level, j)) = self.find_cte(single) {
                        let cte = self.ctes[level][j].clone();
                        // A CTE sees only the CTEs defined before it.
                        let saved = std::mem::take(&mut self.ctes);
                        self.ctes = saved[..level].to_vec();
                        self.ctes.push(saved[level][..j].to_vec());
                        let sub = From::Sub {
                            query: Box::new(cte.query),
                            alias: Some(alias.clone().unwrap_or_else(|| single.clone())),
                            cols: cte.cols,
                        };
                        let r = self.scope_of(&sub, &[]);
                        self.ctes = saved;
                        return r;
                    }
                }
                let key = table_key_with(name, hints);
                let base = table_key(name);
                let n = self.rows(&key);
                let mut names = vec![base.clone(), last_part(&base).to_string()];
                if let Some(a) = alias {
                    names = vec![a.to_lowercase()];
                }
                Ok((0..n)
                    .map(|i| {
                        vec![Binding {
                            names: names.clone(),
                            table: Some(key.clone()),
                            src: Src::Base(key.clone(), i),
                        }]
                    })
                    .collect())
            }
            From::Sub { query, alias, cols } => {
                let r = self.query(query, outer, false)?;
                let names: Vec<String> = if cols.is_empty() {
                    r.names.clone()
                } else {
                    cols.iter().map(|c| c.to_lowercase()).collect()
                };
                let alias = alias.as_ref().map(|a| a.to_lowercase()).unwrap_or_default();
                Ok(r.rows
                    .into_iter()
                    .map(|row| {
                        vec![Binding {
                            names: vec![alias.clone()],
                            table: None,
                            src: Src::Derived(names.iter().cloned().zip(row).collect()),
                        }]
                    })
                    .collect())
            }
            From::Values { rows, alias, cols } => {
                let alias = alias.as_ref().map(|a| a.to_lowercase()).unwrap_or_default();
                let mut out = Vec::new();
                for r in rows {
                    let mut vals = Vec::new();
                    for e in r {
                        vals.push(self.expr(e, outer, None)?);
                    }
                    out.push(vec![Binding {
                        names: vec![alias.clone()],
                        table: None,
                        src: Src::Derived(
                            cols.iter().map(|c| c.to_lowercase()).zip(vals).collect(),
                        ),
                    }]);
                }
                Ok(out)
            }
            From::Func {
                name,
                args,
                schema,
                alias,
            } => {
                let key = func_key(name, args);
                if schema.is_empty() {
                    return unsupported(format!("{key} without a WITH schema"));
                }
                // Its rows are free when it reads a parameter (any document can
                // be passed), assumed when it reads a local variable.
                let mut origin = Origin::Free;
                for (i, a) in args.iter().enumerate() {
                    match a {
                        Expr::Var(v) if i == 0 => {
                            let k = var_key(v);
                            if self.read_vars.contains(&k) {
                                return unsupported(format!(
                                    "{v} is read besides a table function"
                                ));
                            }
                            match self.vars.get(&k).map(|f| f.origin) {
                                Some(Origin::Free) => {}
                                Some(Origin::Assumed) => origin = Origin::Assumed,
                                _ => return unsupported(format!("{key} reads a constant")),
                            }
                            self.func_vars.insert(k);
                        }
                        Expr::Str(_) | Expr::Num(_) if i > 0 => {}
                        _ => return unsupported(format!("{key} arguments")),
                    }
                }
                self.func_schema.insert(
                    key.clone(),
                    schema
                        .iter()
                        .map(|(c, t)| (c.to_lowercase(), t.clone()))
                        .collect(),
                );
                self.func_origin.insert(key.clone(), origin);
                let n = self.rows(&key);
                let names = vec![alias
                    .as_ref()
                    .map(|a| a.to_lowercase())
                    .unwrap_or_else(|| name.last().map(|x| x.to_lowercase()).unwrap_or_default())];
                Ok((0..n)
                    .map(|i| {
                        vec![Binding {
                            names: names.clone(),
                            table: Some(key.clone()),
                            src: Src::Base(key.clone(), i),
                        }]
                    })
                    .collect())
            }
            From::DataChange { .. } => unsupported("data change table reference"),
            From::Join {
                kind,
                left,
                right,
                on,
            } => {
                let ls = self.scope_of(left, outer)?;
                let rs = self.scope_of(right, outer)?;
                let mut out = Vec::new();
                let mut right_matched = vec![false; rs.len()];
                for l in &ls {
                    let mut matched = false;
                    for (j, r) in rs.iter().enumerate() {
                        let mut row = l.clone();
                        row.extend(r.iter().cloned());
                        let ok = match on {
                            None => true,
                            Some(c) => {
                                let mut sc = outer.to_vec();
                                sc.push(row.clone());
                                let v = self.expr(c, &sc, None)?;
                                if let V::Err(_) = v {
                                    return rej();
                                }
                                truth(&v)? == Some(true)
                            }
                        };
                        if ok {
                            matched = true;
                            right_matched[j] = true;
                            out.push(row);
                        }
                    }
                    if !matched && matches!(kind.as_str(), "LEFT" | "FULL") {
                        let mut row = l.clone();
                        row.extend(nulls_of(&rs, right));
                        out.push(row);
                    }
                }
                if matches!(kind.as_str(), "RIGHT" | "FULL") {
                    for (j, r) in rs.iter().enumerate() {
                        if !right_matched[j] {
                            let mut row = nulls_of(&ls, left);
                            row.extend(r.iter().cloned());
                            out.push(row);
                        }
                    }
                }
                Ok(out)
            }
        }
    }

    fn find_cte(&self, name: &str) -> Option<(usize, usize)> {
        for (level, ctes) in self.ctes.iter().enumerate().rev() {
            if let Some(j) = ctes.iter().position(|c| c.name.eq_ignore_ascii_case(name)) {
                return Some((level, j));
            }
        }
        None
    }

    /// Evaluates a query. `exists_only` skips the select list.
    fn query(&mut self, q: &Select, outer: &[Scope], exists_only: bool) -> R<Rows> {
        if !q.set_ops.is_empty() {
            if q.ctes.is_empty() {
                return self.set_query(q, outer);
            }
            self.ctes.push(q.ctes.clone());
            let r = self.set_query(q, outer);
            self.ctes.pop();
            return r;
        }
        if q.ctes.is_empty() {
            return self.query_body(q, outer, exists_only);
        }
        self.ctes.push(q.ctes.clone());
        let r = self.query_body(q, outer, exists_only);
        self.ctes.pop();
        r
    }

    /// UNION / EXCEPT / INTERSECT, then the ORDER BY and paging of the whole.
    fn set_query(&mut self, q: &Select, outer: &[Scope]) -> R<Rows> {
        let mut head = q.clone();
        head.set_ops.clear();
        head.ctes.clear();
        head.order_by.clear();
        head.offset = None;
        head.limit = None;
        head.for_json = false;
        let first = self.query(&head, outer, false)?;
        let names = first.names;
        let mut rows = first.rows;
        let contains = |set: &[Vec<V>], r: &[V]| -> R<bool> {
            let mut unsure = false;
            for x in set {
                match rows_group_same(x, r) {
                    Some(true) => return Ok(true),
                    Some(false) => {}
                    None => unsure = true,
                }
            }
            if unsure {
                rej()
            } else {
                Ok(false)
            }
        };
        let distinct = |rows: Vec<Vec<V>>| -> R<Vec<Vec<V>>> {
            let mut out: Vec<Vec<V>> = Vec::new();
            for r in rows {
                if !contains(&out, &r)? {
                    out.push(r);
                }
            }
            Ok(out)
        };
        for (op, o) in &q.set_ops {
            let r = self.query(o, outer, false)?;
            if r.names.len() != names.len() {
                return unsupported("set operation operands with different column counts");
            }
            rows = match op.as_str() {
                "UNION ALL" => {
                    rows.extend(r.rows);
                    rows
                }
                "UNION" => {
                    rows.extend(r.rows);
                    distinct(rows)?
                }
                "EXCEPT" | "INTERSECT" => {
                    let keep = op == "INTERSECT";
                    let mut out = Vec::new();
                    for x in distinct(rows)? {
                        if contains(&r.rows, &x)? == keep {
                            out.push(x);
                        }
                    }
                    out
                }
                _ => return unsupported(op.to_string()),
            };
        }
        let mut ordered = false;
        if !q.order_by.is_empty() {
            let mut keys = Vec::new();
            for o in &q.order_by {
                let i = match &o.expr {
                    Expr::Num(n) => n.parse::<usize>().ok().and_then(|k| k.checked_sub(1)),
                    Expr::Name(p) => p
                        .last()
                        .and_then(|c| names.iter().position(|n| n.eq_ignore_ascii_case(c))),
                    _ => None,
                };
                let Some(i) = i.filter(|i| *i < names.len()) else {
                    return unsupported("ORDER BY of a set operation by an expression");
                };
                let nulls_first = o.nulls_first.unwrap_or(if self.d == Dialect::Postgres {
                    o.desc
                } else {
                    !o.desc
                });
                keys.push((i, o.desc, nulls_first));
            }
            let cmp = |a: &Vec<V>, b: &Vec<V>| -> Option<std::cmp::Ordering> {
                for &(i, desc, nf) in &keys {
                    let ord = match (a[i].is_null(), b[i].is_null()) {
                        (true, true) => std::cmp::Ordering::Equal,
                        (true, false) => {
                            if nf {
                                std::cmp::Ordering::Less
                            } else {
                                std::cmp::Ordering::Greater
                            }
                        }
                        (false, true) => {
                            if nf {
                                std::cmp::Ordering::Greater
                            } else {
                                std::cmp::Ordering::Less
                            }
                        }
                        _ => {
                            let o = cmp_vals(&a[i], &b[i])?;
                            if desc {
                                o.reverse()
                            } else {
                                o
                            }
                        }
                    };
                    if ord != std::cmp::Ordering::Equal {
                        return Some(ord);
                    }
                }
                Some(std::cmp::Ordering::Equal)
            };
            // Insertion sort that gives up on undecidable comparisons.
            let mut sorted: Vec<Vec<V>> = Vec::new();
            ordered = true;
            for r in rows {
                let mut at = sorted.len();
                for (j, x) in sorted.iter().enumerate() {
                    match cmp(&r, x) {
                        None => return rej(),
                        Some(std::cmp::Ordering::Less) => {
                            at = j;
                            break;
                        }
                        Some(std::cmp::Ordering::Equal) if rows_same(&r, x) != Some(true) => {
                            ordered = false;
                        }
                        _ => {}
                    }
                }
                sorted.insert(at, r);
            }
            rows = sorted;
        }
        let num = |c: &mut Self, e: &Option<Expr>| -> R<Option<usize>> {
            match e {
                None => Ok(None),
                Some(e) => match c.expr(e, outer, None)? {
                    V::Int(i) if i >= 0 => Ok(Some(i as usize)),
                    _ => unsupported("non-constant paging"),
                },
            }
        };
        let offset = num(self, &q.offset)?.unwrap_or(0);
        let limit = num(self, &q.limit)?;
        if offset > 0 || limit.is_some_and(|l| l < rows.len()) {
            if !ordered {
                return rej();
            }
            rows = rows
                .into_iter()
                .skip(offset)
                .take(limit.unwrap_or(usize::MAX))
                .collect();
        }
        Ok(Rows {
            names,
            rows,
            ordered,
        })
    }

    fn query_body(&mut self, q: &Select, outer: &[Scope], exists_only: bool) -> R<Rows> {
        if let Some(rows) = &q.values {
            let mut out = Vec::new();
            for r in rows {
                let mut vals = Vec::new();
                for e in r {
                    vals.push(self.expr(e, outer, None)?);
                }
                out.push(vals);
            }
            return Ok(Rows {
                names: Vec::new(),
                rows: out,
                ordered: false,
            });
        }
        // FROM: cartesian product of the items.
        let mut product: Vec<Scope> = vec![Vec::new()];
        for f in &q.from {
            let rows = self.scope_of(f, outer)?;
            let mut next = Vec::new();
            for p in &product {
                for r in &rows {
                    let mut x = p.clone();
                    x.extend(r.iter().cloned());
                    next.push(x);
                }
            }
            product = next;
        }
        // WHERE.
        let mut kept = Vec::new();
        for row in product {
            let ok = match &q.where_ {
                None => true,
                Some(w) => {
                    let mut sc = outer.to_vec();
                    sc.push(row.clone());
                    let v = self.expr(w, &sc, None)?;
                    if let V::Err(_) = v {
                        return rej();
                    }
                    truth(&v)? == Some(true)
                }
            };
            if ok {
                kept.push(row);
            }
        }
        if q.group_by.iter().any(|k| matches!(k, Expr::Num(_))) {
            // PostgreSQL groups by output position; T-SQL refuses it.
            return unsupported("GROUP BY a position");
        }
        // With DISTINCT, ORDER BY sorts by output columns only.
        if q.distinct
            && q.order_by.iter().any(|o| {
                matches!(order_target(o, &q.items, &[]), OrderTarget::Expr)
                    && !q.items.iter().any(|i| i.expr == o.expr)
            })
        {
            return unsupported("DISTINCT ordered by an expression it does not return");
        }
        let grouped = !q.group_by.is_empty()
            || q.having.is_some()
            || q.items.iter().any(|i| has_aggregate(&i.expr));
        let names: Vec<String> = q
            .items
            .iter()
            .map(|i| match (&i.alias, &i.expr) {
                (Some(a), _) => a.to_lowercase(),
                (None, Expr::Name(p)) => p.last().map(|s| s.to_lowercase()).unwrap_or_default(),
                _ => String::new(),
            })
            .collect();
        // Output rows with their sort keys.
        let mut out: Vec<(Vec<V>, Vec<V>)> = Vec::new();
        if grouped {
            // Groups by key values; an aggregate without GROUP BY makes one group.
            let mut groups: Vec<(Vec<V>, Vec<Scope>)> = Vec::new();
            if q.group_by.is_empty() {
                groups.push((Vec::new(), kept));
            } else {
                for row in kept {
                    let mut sc = outer.to_vec();
                    sc.push(row.clone());
                    let mut key = Vec::new();
                    for e in &q.group_by {
                        key.push(self.expr(e, &sc, None)?);
                    }
                    let mut placed = false;
                    for (k, rows) in groups.iter_mut() {
                        let mut eq = true;
                        for (x, y) in k.iter().zip(&key) {
                            match group_same(x, y) {
                                Some(true) => {}
                                Some(false) => eq = false,
                                None => return rej(),
                            }
                        }
                        if eq {
                            rows.push(row.clone());
                            placed = true;
                            break;
                        }
                    }
                    if !placed {
                        groups.push((key, vec![row]));
                    }
                }
            }
            for (_, rows) in &groups {
                let g = Group { rows };
                if let Some(h) = &q.having {
                    let v = self.group_expr(h, outer, &g, &q.group_by)?;
                    if truth(&v)? != Some(true) {
                        continue;
                    }
                }
                let mut vals = Vec::new();
                if !exists_only {
                    for i in &q.items {
                        if matches!(i.expr, Expr::Star(_)) {
                            return unsupported("* in a grouped query");
                        }
                        vals.push(self.group_expr(&i.expr, outer, &g, &q.group_by)?);
                    }
                }
                // Keys are computed under EXISTS too: a key the group does
                // not determine is not a query the engine would run.
                let mut keys = Vec::new();
                for o in &q.order_by {
                    let e = match order_target(o, &q.items, &vals) {
                        OrderTarget::Val(v) => {
                            keys.push(v);
                            continue;
                        }
                        OrderTarget::Item(i) => &q.items[i].expr,
                        OrderTarget::Expr => &o.expr,
                        OrderTarget::Invalid => return rej(),
                    };
                    keys.push(self.group_expr(e, outer, &g, &q.group_by)?);
                }
                out.push((vals, keys));
            }
        } else {
            for row in kept {
                if exists_only {
                    // Neither the select list nor the order matters, only
                    // how many rows there are.
                    out.push((Vec::new(), Vec::new()));
                    continue;
                }
                let mut sc = outer.to_vec();
                sc.push(row.clone());
                let mut vals = Vec::new();
                for i in &q.items {
                    if let Expr::Star(_) = i.expr {
                        return unsupported("SELECT *");
                    }
                    vals.push(self.expr(&i.expr, &sc, None)?);
                }
                let mut keys = Vec::new();
                for o in &q.order_by {
                    keys.push(match order_target(o, &q.items, &vals) {
                        OrderTarget::Val(v) => v,
                        OrderTarget::Item(i) => self.expr(&q.items[i].expr, &sc, None)?,
                        OrderTarget::Expr => self.expr(&o.expr, &sc, None)?,
                        OrderTarget::Invalid => return rej(),
                    });
                }
                out.push((vals, keys));
            }
        }
        if q.distinct {
            let mut d: Vec<(Vec<V>, Vec<V>)> = Vec::new();
            for r in out {
                let mut dup = false;
                for x in &d {
                    if rows_group_same(&x.0, &r.0).ok_or(Stop::Reject)? {
                        // Which duplicate is kept is unspecified: they must
                        // look and sort the same.
                        if rows_same(&x.0, &r.0) != Some(true)
                            || (!exists_only && rows_same(&x.1, &r.1) != Some(true))
                        {
                            return rej();
                        }
                        dup = true;
                    }
                }
                if !dup {
                    d.push(r);
                }
            }
            out = d;
        }
        // ORDER BY: a stable sort is only meaningful without ties between
        // different rows. Under EXISTS only the number of rows counts.
        let mut ties = false;
        if !q.order_by.is_empty() && !exists_only {
            if out.iter().any(|r| r.1.len() != q.order_by.len()) {
                return unsupported("ORDER BY keys that were not computed");
            }
            let mut err = false;
            let descs: Vec<(bool, Option<bool>)> =
                q.order_by.iter().map(|o| (o.desc, o.nulls_first)).collect();
            let d = self.d;
            out.sort_by(|x, y| {
                for (i, (desc, nf)) in descs.iter().enumerate() {
                    let (a, b) = (&x.1[i], &y.1[i]);
                    let o = match (a, b) {
                        (V::Null, V::Null) => std::cmp::Ordering::Equal,
                        (V::Null, _) | (_, V::Null) => {
                            // T-SQL sorts NULL first ascending; PostgreSQL last.
                            let null_small = match nf {
                                Some(f) => *f != *desc,
                                None => d == Dialect::Tsql,
                            };
                            let o = if a.is_null() == null_small {
                                std::cmp::Ordering::Less
                            } else {
                                std::cmp::Ordering::Greater
                            };
                            if *desc {
                                o.reverse()
                            } else {
                                o
                            }
                        }
                        _ => match cmp_vals(a, b) {
                            Some(o) => {
                                if *desc {
                                    o.reverse()
                                } else {
                                    o
                                }
                            }
                            None => {
                                err = true;
                                std::cmp::Ordering::Equal
                            }
                        },
                    };
                    if o != std::cmp::Ordering::Equal {
                        return o;
                    }
                }
                std::cmp::Ordering::Equal
            });
            if err {
                return rej();
            }
            for w in out.windows(2) {
                let tie = w[0]
                    .1
                    .iter()
                    .zip(&w[1].1)
                    .all(|(a, b)| same(a, b) != Some(false));
                if tie && rows_same(&w[0].0, &w[1].0) != Some(true) {
                    // Rows the ORDER BY does not tell apart come in either order.
                    ties = true;
                }
            }
        }
        // TOP / LIMIT / OFFSET.
        let mut rows: Vec<Vec<V>> = out.into_iter().map(|r| r.0).collect();
        let offset = match &q.offset {
            Some(e) => match self.expr(e, outer, None)? {
                V::Int(i) if i >= 0 => i as usize,
                _ => return rej(),
            },
            None => 0,
        };
        let limit = match q.top.as_ref().or(q.limit.as_ref()) {
            Some(e) => match self.expr(e, outer, None)? {
                V::Int(i) if i >= 0 => Some(i as usize),
                V::Null if q.limit.is_some() => None,
                _ => return rej(),
            },
            None => None,
        };
        if ties && (offset > 0 || limit.is_some_and(|l| rows.len() > l)) {
            return rej();
        }
        if (limit.is_some() || offset > 0)
            && q.order_by.is_empty()
            && rows.len() > limit.unwrap_or(0) + offset
        {
            // Which rows are kept is unspecified, unless they all look the same.
            let first = rows.first().cloned();
            if !rows
                .iter()
                .all(|r| first.as_ref().and_then(|f| rows_same(f, r)) == Some(true))
            {
                return rej();
            }
        }
        rows = rows.into_iter().skip(offset).collect();
        if let Some(l) = limit {
            rows.truncate(l);
        }
        Ok(Rows {
            names,
            rows,
            ordered: !q.order_by.is_empty() && !ties && !exists_only,
        })
    }

    /// A grouped query's expression for one group. A column of the
    /// query's own rows outside an aggregate must be a grouping column:
    /// otherwise SQL Server and PostgreSQL (short of a grouped primary key)
    /// refuse the query, and there is no result to compare. What the check
    /// cannot see (subqueries) must at least have one value in the group,
    /// or the result is unspecified and the assignment is no witness.
    fn group_expr(&mut self, e: &Expr, outer: &[Scope], g: &Group, group_by: &[Expr]) -> R<V> {
        let mut names = Vec::new();
        ungrouped_names(e, group_by, &mut names);
        if !names.is_empty() {
            // Only names that bind to the query's own rows are wrong;
            // without a row there is no telling.
            let Some(row) = g.rows.first() else {
                return rej();
            };
            for p in &names {
                let covered = group_by.iter().any(|k| match k {
                    Expr::Name(q) => same_column(q, p, row.len()),
                    _ => false,
                });
                if !covered && self.binds_in(p, row) {
                    return unsupported(format!(
                        "column {} outside GROUP BY and aggregates",
                        p.join(".")
                    ));
                }
            }
        }
        let mut sc = outer.to_vec();
        sc.push(g.rows.first().cloned().unwrap_or_default());
        let v = self.expr(e, &sc, Some(g))?;
        for row in g.rows.iter().skip(1) {
            let last = sc.len() - 1;
            sc[last] = row.clone();
            let w = self.expr(e, &sc, Some(g))?;
            if v != w && same(&v, &w) != Some(true) {
                return rej();
            }
        }
        Ok(v)
    }

    /// Whether `resolve` would read name `p` from `scope` (the query's own
    /// row), as opposed to a variable or an outer query.
    fn binds_in(&self, p: &[String], scope: &Scope) -> bool {
        let lower: Vec<String> = p.iter().map(|x| x.to_lowercase()).collect();
        let col = lower.last().cloned().unwrap_or_default();
        if lower.len() == 1 && self.d != Dialect::Tsql && self.vars.contains_key(&col) {
            return false;
        }
        if lower.len() == 2 && self.d != Dialect::Tsql && self.records.contains(&lower[0]) {
            return false;
        }
        if lower.len() >= 2 {
            let q = lower[..lower.len() - 1].join(".");
            return scope
                .iter()
                .any(|b| b.names.iter().any(|n| *n == q || n == last_part(&q)));
        }
        scope.len() == 1
            || scope
                .iter()
                .any(|b| self.has_column(b, &col) != Some(false))
    }
}

fn nulls_of(rows: &[Scope], f: &From) -> Scope {
    // The NULL-extended side keeps its names so its columns read as NULL.
    match rows.first() {
        Some(r) => r
            .iter()
            .map(|b| Binding {
                names: b.names.clone(),
                table: b.table.clone(),
                src: Src::Nulls,
            })
            .collect(),
        None => from_names(f)
            .into_iter()
            .map(|(names, table)| Binding {
                names,
                table,
                src: Src::Nulls,
            })
            .collect(),
    }
}

fn from_names(f: &From) -> Vec<(Vec<String>, Option<String>)> {
    match f {
        From::Table { name, alias, hints } => {
            let base = table_key(name);
            let names = match alias {
                Some(a) => vec![a.to_lowercase()],
                None => vec![base.clone(), last_part(&base).to_string()],
            };
            vec![(names, Some(table_key_with(name, hints)))]
        }
        From::Func {
            name, args, alias, ..
        } => {
            let names = vec![alias
                .as_ref()
                .map(|a| a.to_lowercase())
                .unwrap_or_else(|| name.last().map(|x| x.to_lowercase()).unwrap_or_default())];
            vec![(names, Some(func_key(name, args)))]
        }
        From::Sub { alias, .. } | From::Values { alias, .. } | From::DataChange { alias, .. } => {
            vec![(
                vec![alias.as_ref().map(|a| a.to_lowercase()).unwrap_or_default()],
                None,
            )]
        }
        From::Join { left, right, .. } => {
            let mut v = from_names(left);
            v.extend(from_names(right));
            v
        }
    }
}

/// Whether two sets of updated (or deleted) rows leave the table in
/// different states. Columns are matched by name, so `SET a = 1, b = 2` and
/// `SET b = 2, a = 1` agree, and a column one side sets to the value it
/// already has is no change.
fn writes_differ(x: &[Write], y: &[Write], nonnull: &[bool]) -> Option<bool> {
    let deletes = |w: &[Write]| w.iter().all(|r| r.1.is_empty());
    if deletes(x) && deletes(y) {
        let xi: Vec<usize> = x.iter().map(|w| w.0).collect();
        let yi: Vec<usize> = y.iter().map(|w| w.0).collect();
        return Some(xi != yi);
    }
    let mut rows: Vec<usize> = x.iter().chain(y).map(|w| w.0).collect();
    rows.sort_unstable();
    rows.dedup();
    let mut unknown = false;
    for i in rows {
        let wx = x
            .iter()
            .find(|w| w.0 == i)
            .map(|w| w.1.as_slice())
            .unwrap_or(&[]);
        let wy = y
            .iter()
            .find(|w| w.0 == i)
            .map(|w| w.1.as_slice())
            .unwrap_or(&[]);
        let cols = wx.iter().chain(wy).map(|c| c.0.as_str());
        for col in cols {
            let old = wx
                .iter()
                .chain(wy)
                .find(|c| c.0 == col)
                .map(|c| &c.2)
                .expect("column is set on one side");
            let ax = wx.iter().find(|c| c.0 == col).map_or(old, |c| &c.1);
            let ay = wy.iter().find(|c| c.0 == col).map_or(old, |c| &c.1);
            match same_nn(ax, ay, nonnull) {
                Some(true) => {}
                Some(false) => return Some(true),
                None => unknown = true,
            }
        }
    }
    if unknown {
        None
    } else {
        Some(false)
    }
}

/// Whether two INSERTs add different rows. Values are matched by column;
/// a column only one side names gets that table's default on the other
/// side, which is not known, so then only the common columns can show a
/// difference.
fn inserts_differ(
    cx: &[String],
    x: &[Vec<V>],
    cy: &[String],
    y: &[Vec<V>],
    nonnull: &[bool],
) -> Option<bool> {
    if x.len() != y.len() {
        return Some(true);
    }
    if cx == cy {
        return differ(
            &Obs::Rows(x.to_vec(), false),
            &Obs::Rows(y.to_vec(), false),
            nonnull,
        );
    }
    if cx.is_empty() || cy.is_empty() {
        return None;
    }
    let common: Vec<&String> = cx.iter().filter(|c| cy.contains(c)).collect();
    let project = |cols: &[String], rows: &[Vec<V>]| -> Vec<Vec<V>> {
        rows.iter()
            .map(|r| {
                common
                    .iter()
                    .map(|c| r[cols.iter().position(|x| x == *c).unwrap()].clone())
                    .collect()
            })
            .collect()
    };
    let same_cols = cx.len() == cy.len() && common.len() == cx.len();
    match differ(
        &Obs::Rows(project(cx, x), false),
        &Obs::Rows(project(cy, y), false),
        nonnull,
    ) {
        Some(false) if !same_cols => None,
        r => r,
    }
}

/// What an ORDER BY item sorts by.
enum OrderTarget {
    /// An output column already computed.
    Val(V),
    /// Output column `i`, not computed (under EXISTS).
    Item(usize),
    /// An expression over the input rows.
    Expr,
    /// A position past the last output column.
    Invalid,
}

fn order_target(o: &OrderItem, items: &[super::ast::SelItem], vals: &[V]) -> OrderTarget {
    let at = |i: usize| match vals.get(i) {
        Some(v) => OrderTarget::Val(v.clone()),
        None => OrderTarget::Item(i),
    };
    // ORDER BY 2: the second output column.
    if let Expr::Num(n) = &o.expr {
        if let Ok(i) = n.parse::<usize>() {
            return match i.checked_sub(1).filter(|i| *i < items.len()) {
                Some(i) => at(i),
                // Out of range: the query does not compile.
                None => OrderTarget::Invalid,
            };
        }
    }
    // An output alias.
    if let Expr::Name(p) = &o.expr {
        if p.len() == 1 {
            let n = p[0].to_lowercase();
            if let Some(i) = items
                .iter()
                .position(|it| it.alias.as_ref().is_some_and(|a| a.to_lowercase() == n))
            {
                return at(i);
            }
        }
    }
    OrderTarget::Expr
}

/// Whether GROUP BY, DISTINCT and set operations put two values together.
/// Strings that differ only in case, trailing spaces or accents are one
/// group under some collations, so that is unknown.
fn group_same(a: &V, b: &V) -> Option<bool> {
    match (a, b) {
        (V::Str(_), V::Str(_)) => eq_vals(a, b),
        _ => same(a, b),
    }
}

fn rows_group_same(a: &[V], b: &[V]) -> Option<bool> {
    if a.len() != b.len() {
        return Some(false);
    }
    let mut all = true;
    for (x, y) in a.iter().zip(b) {
        match group_same(x, y) {
            Some(true) => {}
            Some(false) => return Some(false),
            None => all = false,
        }
    }
    all.then_some(true)
}

/// Column names in `e` outside aggregates and outside any subexpression
/// that is itself a GROUP BY expression. Subqueries are not entered.
fn ungrouped_names(e: &Expr, group_by: &[Expr], out: &mut Vec<Vec<String>>) {
    if is_aggregate(e) || group_by.iter().any(|k| k == e) {
        return;
    }
    let mut go = |x: &Expr| ungrouped_names(x, group_by, out);
    match e {
        Expr::Name(p) => out.push(p.clone()),
        Expr::Call(c) => c.args.iter().for_each(go),
        Expr::Bin(_, l, r) => {
            go(l);
            go(r);
        }
        Expr::Unary(_, x)
        | Expr::IsNull(x, _)
        | Expr::Cast(x, _)
        | Expr::Extract(_, x)
        | Expr::InQuery(x, _, _) => go(x),
        Expr::InList(x, l, _) => {
            go(x);
            l.iter().for_each(go);
        }
        Expr::Between(x, l, h, _) => {
            go(x);
            go(l);
            go(h);
        }
        Expr::Like(x, p, _, _) => {
            go(x);
            go(p);
        }
        Expr::Case(o, w, e2) => {
            if let Some(o) = o {
                go(o);
            }
            for (a, b) in w {
                go(a);
                go(b);
            }
            if let Some(e2) = e2 {
                go(e2);
            }
        }
        Expr::Tuple(l) => l.iter().for_each(go),
        _ => {}
    }
}

/// Whether GROUP BY name `k` and name `p` are certainly the same column of
/// a query over `tables` tables: same column name and, when the query
/// reads more than one table, the same qualifier.
fn same_column(k: &[String], p: &[String], tables: usize) -> bool {
    let low = |v: &[String]| v.iter().map(|x| x.to_lowercase()).collect::<Vec<_>>();
    let (k, p) = (low(k), low(p));
    if k.last() != p.last() {
        return false;
    }
    // `dbo.t.g` and `t.g` name the same table.
    match (&k[..k.len() - 1], &p[..p.len() - 1]) {
        ([], []) => true,
        ([.., x], [.., y]) => x == y,
        _ => tables == 1,
    }
}

fn rows_same(a: &[V], b: &[V]) -> Option<bool> {
    rows_same_nn(a, b, &[])
}

fn rows_same_nn(a: &[V], b: &[V], nonnull: &[bool]) -> Option<bool> {
    if a.len() != b.len() {
        return Some(false);
    }
    let mut all = true;
    for (x, y) in a.iter().zip(b) {
        match same_nn(x, y, nonnull) {
            Some(true) => {}
            Some(false) => return Some(false),
            None => all = false,
        }
    }
    if all {
        Some(true)
    } else {
        None
    }
}

/// Whether two observations certainly differ (Some(true)), certainly agree
/// (Some(false)) or cannot be told apart (None).
pub fn differ(a: &Obs, b: &Obs, nonnull: &[bool]) -> Option<bool> {
    match (a, b) {
        (Obs::Error(_), Obs::Error(_)) => Some(false),
        (Obs::Error(_), _) | (_, Obs::Error(_)) => Some(true),
        (Obs::Value(x), Obs::Value(y)) => same_nn(x, y, nonnull).map(|s| !s),
        (Obs::Branch(x), Obs::Branch(y)) => Some(x != y),
        (Obs::Assigned(x), Obs::Assigned(y)) => {
            let mut unknown = false;
            for (p, q) in x.iter().zip(y) {
                match (p, q) {
                    (None, None) => {}
                    (Some(p), Some(q)) => match same_nn(p, q, nonnull) {
                        Some(true) => {}
                        Some(false) => return Some(true),
                        None => unknown = true,
                    },
                    // One side assigns, the other keeps the previous value.
                    _ => return Some(true),
                }
            }
            if unknown {
                None
            } else {
                Some(false)
            }
        }
        (Obs::Writes(x), Obs::Writes(y)) => writes_differ(x, y, nonnull),
        (Obs::Inserted(cx, x), Obs::Inserted(cy, y)) => inserts_differ(cx, x, cy, y, nonnull),
        (Obs::Rows(x, ox), Obs::Rows(y, oy)) => {
            if x.len() != y.len() {
                return Some(true);
            }
            if *ox && *oy {
                let mut unknown = false;
                for (p, q) in x.iter().zip(y) {
                    match rows_same_nn(p, q, nonnull) {
                        Some(true) => {}
                        Some(false) => return Some(true),
                        None => unknown = true,
                    }
                }
                return if unknown { None } else { Some(false) };
            }
            // As multisets: every row of x matched by a distinct row of y.
            let mut used = vec![false; y.len()];
            for p in x {
                let mut found = false;
                for (j, q) in y.iter().enumerate() {
                    if used[j] {
                        continue;
                    }
                    match rows_same_nn(p, q, nonnull) {
                        Some(true) => {
                            used[j] = true;
                            found = true;
                            break;
                        }
                        Some(false) => {}
                        None => return None,
                    }
                }
                if !found {
                    return Some(true);
                }
            }
            Some(false)
        }
        _ => None,
    }
}

// ---- statements --------------------------------------------------------------------

/// What to observe of a statement.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Probe {
    /// A data statement (SELECT, INSERT, UPDATE, DELETE, assignment, RETURN, DECLARE).
    Stmt(Stmt),
    /// A chain of conditions (IF / ELSIF / WHILE / CONTINUE WHEN): the index
    /// of the first one that is TRUE.
    Conds(Vec<Expr>),
    /// The values of expressions (arguments of THROW, RAISE, PRINT, EXEC).
    Values(Vec<Expr>),
}

impl Ctx<'_> {
    /// Rejects inputs under which this side never reaches the statement.
    fn check_guards(&mut self) -> R<()> {
        let i = usize::from(self.side == 2);
        let guards = std::mem::take(&mut self.guards[i]);
        let mut r = Ok(());
        for (e, must) in &guards {
            // A guard that cannot be decided is skipped (and reported); one
            // that fails or is decided the wrong way rejects the inputs.
            let t = match self.expr(e, &[], None) {
                Ok(V::Err(_)) => {
                    r = rej();
                    break;
                }
                Ok(v) => truth(&v),
                Err(stop) => Err(stop),
            };
            match t {
                Ok(t) if (t == Some(true)) == *must => {}
                Ok(_) => {
                    r = rej();
                    break;
                }
                Err(Stop::Need(n)) => {
                    r = Err(Stop::Need(n));
                    break;
                }
                Err(_) => self.guard_skipped = true,
            }
        }
        self.guards[i] = guards;
        r
    }

    pub fn observe(&mut self, p: &Probe) -> R<Obs> {
        self.func_vars.clear();
        self.read_vars.clear();
        self.check_guards()?;
        let r = match p {
            Probe::Conds(cs) => {
                for (i, c) in cs.iter().enumerate() {
                    let v = self.expr(c, &[], None)?;
                    if let V::Err(e) = v {
                        return Ok(Obs::Error(e));
                    }
                    if truth(&v)? == Some(true) {
                        return Ok(Obs::Branch(Some(i)));
                    }
                }
                Ok(Obs::Branch(None))
            }
            Probe::Values(es) => {
                let mut vals = Vec::new();
                for e in es {
                    let v = self.expr(e, &[], None)?;
                    if let V::Err(e) = v {
                        return Ok(Obs::Error(e));
                    }
                    vals.push(v);
                }
                Ok(Obs::Rows(vec![vals], true))
            }
            Probe::Stmt(s) => self.stmt(s),
        }?;
        Ok(r)
    }

    fn stmt(&mut self, s: &Stmt) -> R<Obs> {
        match s {
            Stmt::Select(q) => {
                let assigns: Vec<bool> = q.items.iter().map(|i| i.assign.is_some()).collect();
                let r = self.query(q, &[], false)?;
                if let Some(e) = first_error(&r.rows) {
                    return Ok(Obs::Error(e));
                }
                if assigns.iter().any(|a| *a) || !q.into.is_empty() {
                    let width = r.rows.first().map(Vec::len).unwrap_or(q.items.len());
                    return match r.rows.as_slice() {
                        // T-SQL keeps the variables; PL/pgSQL SELECT INTO sets NULL.
                        [] if self.d == Dialect::Tsql => Ok(Obs::Assigned(vec![None; width])),
                        [] => Ok(Obs::Assigned(vec![Some(V::Null); width])),
                        [row] => Ok(Obs::Assigned(row.iter().cloned().map(Some).collect())),
                        rows => {
                            // Several rows: T-SQL assigns from each (the last one wins),
                            // PL/pgSQL takes the first; only certain when they agree.
                            let first = &rows[0];
                            if rows.iter().all(|r| rows_same(r, first) == Some(true)) || r.ordered {
                                let pick = if self.d == Dialect::Tsql {
                                    rows.last().unwrap()
                                } else {
                                    first
                                };
                                Ok(Obs::Assigned(pick.iter().cloned().map(Some).collect()))
                            } else {
                                rej()
                            }
                        }
                    };
                }
                Ok(Obs::Rows(r.rows, r.ordered))
            }
            Stmt::Assign(t, e) => {
                let v = self.expr(e, &[], None)?;
                // A string longer than the variable's declared length is cut or rejected.
                let name = match t {
                    Expr::Var(n) => Some(var_key(n)),
                    Expr::Name(p) if p.len() == 1 => Some(p[0].to_lowercase()),
                    _ => None,
                };
                if let (V::Str(s), Some(ty)) = (&v, name.and_then(|n| self.vars.get(&n)?.ty)) {
                    if ty.is_string() && ty.len.is_some_and(|l| s.chars().count() as u32 > l) {
                        return rej();
                    }
                }
                Ok(match v {
                    V::Err(e) => Obs::Error(e),
                    v => Obs::Value(v),
                })
            }
            Stmt::Return(Some(e)) => {
                let v = self.expr(e, &[], None)?;
                Ok(match v {
                    V::Err(e) => Obs::Error(e),
                    v => Obs::Value(v),
                })
            }
            Stmt::Declare { default, ty, .. } => {
                let Some(e) = default else {
                    return Ok(Obs::Value(V::Null));
                };
                let v = self.expr(e, &[], None)?;
                let v = match (ty, v) {
                    (Some(t), V::Str(s)) => {
                        let ty = Ty::from_type(t, self.d, false);
                        match ty.len {
                            Some(l) if ty.is_string() && s.chars().count() as u32 > l => {
                                return rej()
                            }
                            _ => V::Str(s),
                        }
                    }
                    (_, v) => v,
                };
                Ok(match v {
                    V::Err(e) => Obs::Error(e),
                    v => Obs::Value(v),
                })
            }
            Stmt::Message(_) => unsupported("a message"),
            Stmt::Return(None) => Ok(Obs::Value(V::Null)),
            Stmt::Insert {
                target,
                cols,
                source,
                returning,
                on_conflict,
                ..
            } => {
                if !returning.is_empty() || on_conflict.is_some() {
                    return unsupported("INSERT RETURNING / ON CONFLICT");
                }
                let rows = match source {
                    InsertSource::Values(rows) => {
                        let mut out = Vec::new();
                        for r in rows {
                            let mut vals = Vec::new();
                            for e in r {
                                vals.push(self.expr(e, &[], None)?);
                            }
                            out.push(vals);
                        }
                        out
                    }
                    InsertSource::Query(q) => self.query(q, &[], false)?.rows,
                    InsertSource::Default => vec![Vec::new()],
                };
                if let Some(e) = first_error(&rows) {
                    return Ok(Obs::Error(e));
                }
                // Each value is converted to its column's type.
                let key = table_key(target);
                let names: Vec<String> = if cols.is_empty() {
                    self.schema
                        .table(&key)
                        .map(|t| t.columns.iter().map(|c| c.name.clone()).collect())
                        .unwrap_or_default()
                } else {
                    cols.iter().map(|c| c.to_lowercase()).collect()
                };
                let mut stored = Vec::new();
                for r in rows {
                    let mut out = Vec::new();
                    for (j, v) in r.into_iter().enumerate() {
                        let v = match names.get(j) {
                            Some(c) => self.store(&key, c, v)?,
                            None => v,
                        };
                        if let V::Err(e) = v {
                            return Ok(Obs::Error(e));
                        }
                        out.push(v);
                    }
                    stored.push(out);
                }
                if names.len() < stored.iter().map(Vec::len).max().unwrap_or(0) {
                    return Ok(Obs::Inserted(Vec::new(), stored));
                }
                let mut cols = names;
                cols.truncate(stored.first().map_or(0, Vec::len));
                Ok(Obs::Inserted(cols, stored))
            }
            Stmt::Update {
                target,
                alias,
                sets,
                from,
                where_,
            } => self.update(target, alias.as_deref(), Some(sets), from, where_.as_ref()),
            Stmt::Delete {
                target,
                alias,
                from,
                where_,
            } => self.update(target, alias.as_deref(), None, from, where_.as_ref()),
            Stmt::Merge { .. } => unsupported("MERGE"),
        }
    }

    /// UPDATE (with `sets`) or DELETE: which target rows are written, and
    /// the new values.
    fn update(
        &mut self,
        target: &[String],
        alias: Option<&str>,
        sets: Option<&Vec<(Vec<String>, super::ast::Expr)>>,
        from: &[From],
        where_: Option<&Expr>,
    ) -> R<Obs> {
        let mut key = table_key(target);
        // T-SQL `UPDATE alias SET ... FROM t AS alias`: the target is the aliased table.
        let mut target_alias: Option<String> = alias.map(|a| a.to_lowercase());
        if !from.is_empty() && self.d == Dialect::Tsql && target.len() == 1 {
            let t = key.clone();
            for (names, table) in from.iter().flat_map(from_names) {
                if names.contains(&t) {
                    if let Some(table) = table {
                        if names.len() == 1 && names[0] != table {
                            key = table;
                            target_alias = Some(t.clone());
                        }
                    }
                }
            }
        }
        let n = self.rows(&key);
        let target_names = match &target_alias {
            Some(a) => vec![a.clone()],
            None => vec![key.clone(), last_part(&key).to_string()],
        };
        let joined_in_from = !from.is_empty()
            && from.iter().flat_map(from_names).any(|(names, table)| {
                table.as_deref() == Some(key.as_str())
                    && names.iter().any(|x| target_names.contains(x))
            });
        let mut writes = Vec::new();
        for i in 0..n {
            let tb = Binding {
                names: target_names.clone(),
                table: Some(key.clone()),
                src: Src::Base(key.clone(), i),
            };
            // FROM rows (T-SQL UPDATE ... FROM, PostgreSQL UPDATE ... FROM / DELETE ... USING).
            let mut product: Vec<Scope> = vec![Vec::new()];
            for f in from {
                let rows = self.scope_of(f, &[])?;
                let mut next = Vec::new();
                for p in &product {
                    for r in &rows {
                        let mut x = p.clone();
                        x.extend(r.iter().cloned());
                        next.push(x);
                    }
                }
                product = next;
            }
            let mut matches = Vec::new();
            for p in product {
                let mut row: Scope = if joined_in_from {
                    // The target appears in FROM: keep only rows that join to row i.
                    if !p
                        .iter()
                        .any(|b| matches!(&b.src, Src::Base(t, r) if *t == key && *r == i))
                    {
                        continue;
                    }
                    p
                } else {
                    let mut r = vec![tb.clone()];
                    r.extend(p);
                    r
                };
                if joined_in_from {
                    for b in row.iter_mut() {
                        if matches!(&b.src, Src::Base(t, _) if *t == key) {
                            b.names = target_names.clone();
                        }
                    }
                }
                let ok = match where_ {
                    None => true,
                    Some(w) => {
                        let v = self.expr(w, &[row.clone()], None)?;
                        if let V::Err(e) = v {
                            return Ok(Obs::Error(e));
                        }
                        truth(&v)? == Some(true)
                    }
                };
                if ok {
                    matches.push(row);
                }
            }
            match matches.as_slice() {
                [] => {}
                [row] => {
                    let mut vals = Vec::new();
                    if let Some(sets) = sets {
                        for (cols, e) in sets {
                            if cols.len() != 1 {
                                return unsupported("multi-column SET");
                            }
                            let col = cols[0].to_lowercase();
                            let v = self.expr(e, std::slice::from_ref(row), None)?;
                            let v = self.store(&key, &col, v)?;
                            if let V::Err(e) = v {
                                return Ok(Obs::Error(e));
                            }
                            // The value before the update: a row set to the
                            // value it already has does not change.
                            let old = Expr::Name(vec![target_names[0].clone(), col.clone()]);
                            let old = self.expr(&old, std::slice::from_ref(row), None)?;
                            vals.push((col, v, old));
                        }
                    }
                    writes.push((i, vals));
                }
                // Several source rows for one target row: the value is unspecified.
                _ => return rej(),
            }
        }
        Ok(Obs::Writes(writes))
    }
}

impl Ctx<'_> {
    /// A value stored into a table column, as both dialects convert it:
    /// a string longer than the column fails (trailing blanks are dropped),
    /// a decimal is rounded to the column's scale and fails when it does not
    /// fit, an integer fails when out of range. Columns the catalog does not
    /// type keep the value.
    fn store(&mut self, table: &str, col: &str, v: V) -> R<V> {
        let Some(ty) = self
            .schema
            .table(table)
            .and_then(|t| t.column(col))
            .map(|c| c.ty)
        else {
            return Ok(v);
        };
        let too_long = || V::Err(format!("value too long for column {col}"));
        match (ty.fam, &v) {
            (Fam::VarChar | Fam::Char, V::Str(s)) => match ty.len {
                Some(l) if s.chars().count() > l as usize => {
                    let kept: String = s.chars().take(l as usize).collect();
                    if s.chars().skip(l as usize).all(|c| c == ' ') {
                        Ok(V::Str(kept))
                    } else {
                        Ok(too_long())
                    }
                }
                _ => Ok(v),
            },
            (Fam::Exact, V::Int(_) | V::Num(_)) | (Fam::Int, V::Int(_) | V::Num(_)) => {
                let name = match (ty.fam, ty.len, ty.scale) {
                    (Fam::Int, Some(1), _) => "BIT".to_string(),
                    (Fam::Int, Some(16), _) => "SMALLINT".to_string(),
                    (Fam::Int, Some(32), _) => "INT".to_string(),
                    (Fam::Int, Some(64), _) => "BIGINT".to_string(),
                    (Fam::Exact, Some(p), Some(sc)) => format!("DECIMAL({p},{sc})"),
                    _ => return Ok(v),
                };
                let (name, args) = match name.split_once('(') {
                    Some((n, a)) => (
                        n.to_string(),
                        a.trim_end_matches(')')
                            .split(',')
                            .map(str::to_string)
                            .collect(),
                    ),
                    None => (name, Vec::new()),
                };
                self.cast(v, &Type { name, args })
            }
            _ => Ok(v),
        }
    }
}

fn first_error(rows: &[Vec<V>]) -> Option<String> {
    rows.iter().flatten().find_map(|v| match v {
        V::Err(e) => Some(e.clone()),
        _ => None,
    })
}

// ---- search -----------------------------------------------------------------------

/// A found witness: input values and both observations.
#[derive(Debug, Clone)]
pub struct Found {
    pub inputs: Vec<(Key, V)>,
    pub a: Obs,
    pub b: Obs,
    /// Some input is a local value whose occurrence is assumed.
    pub assumed: bool,
    pub opaque: Vec<String>,
    /// Opaque values known never to be NULL.
    pub nonnull: Vec<bool>,
    pub shape: HashMap<String, usize>,
    /// Some guard on the way to the statement could not be evaluated.
    pub unreached: bool,
}

/// Why no witness was found.
#[derive(Debug, Clone, PartialEq)]
pub enum Miss {
    Unsupported(String),
    /// Every assignment tried agreed (or was ambiguous).
    NoWitness,
}

const BUDGET: usize = 20_000;

/// Searches for inputs under which `a` and `b` observably differ. `make`
/// builds a fresh context (facts, hints, catalog) for each world shape.
pub fn search<'s>(make: &dyn Fn() -> Ctx<'s>, a: &Probe, b: &Probe) -> Result<Found, Miss> {
    // World shapes: one row per table first, then each table empty, then
    // each table with two rows.
    // Table functions and temporal reads must be the same reads on both
    // sides: otherwise their rows would be chosen independently.
    let (ta, tb) = (probe_tables(a), probe_tables(b));
    for t in ta
        .iter()
        .filter(|t| !tb.contains(t))
        .chain(tb.iter().filter(|t| !ta.contains(t)))
    {
        if t.contains('(') || t.contains(" system_time ") {
            return Err(Miss::Unsupported(format!(
                "{t} is read by one version only"
            )));
        }
    }
    let mut tables = Vec::new();
    let guards = make().guards;
    let guard_probe = Probe::Conds(guards.iter().flatten().map(|g| g.0.clone()).collect());
    for p in [a, b, &guard_probe] {
        for t in probe_tables(p) {
            if !tables.contains(&t) {
                tables.push(t);
            }
        }
    }
    let mut shapes: Vec<HashMap<String, usize>> = vec![HashMap::new()];
    let focus = make().focus;
    // Two rows of a table read near the change first: subqueries that
    // return more rows, joins that match more.
    for t in tables.iter().filter(|t| focus.contains(t)) {
        shapes.push(HashMap::from([(t.clone(), 2)]));
    }
    for t in &tables {
        shapes.push(HashMap::from([(t.clone(), 0)]));
    }
    for t in tables.iter().filter(|t| !focus.contains(t)) {
        shapes.push(HashMap::from([(t.clone(), 2)]));
    }
    if tables.len() >= 2 {
        shapes.push(tables.iter().map(|t| (t.clone(), 2)).collect());
    }
    let mut budget = BUDGET;
    let mut last_miss = Miss::NoWitness;
    // Limited discrepancy search: first every input at its first candidate,
    // then with one input elsewhere, then two, and so on. Witnesses rarely
    // need many inputs away from their first candidate, and most inputs
    // (say, columns that are only selected) do not matter at all.
    let mut done = vec![false; shapes.len()];
    for limit in 0..=MAX_DISCREPANCIES {
        for (k, shape) in shapes.iter().enumerate() {
            if done[k] {
                continue;
            }
            let mut c = make();
            c.shape = shape.clone();
            let mut pruned = false;
            match dfs(&mut c, a, b, &mut budget, limit, &mut pruned) {
                Ok(Some(mut f)) => {
                    f.shape = shape.clone();
                    return Ok(f);
                }
                Ok(None) => {}
                Err(m) => {
                    last_miss = m;
                    if matches!(last_miss, Miss::Unsupported(_)) {
                        return Err(last_miss);
                    }
                }
            }
            // Nothing was left out: this shape has no witness at all.
            done[k] = !pruned;
            if budget == 0 {
                return Err(last_miss);
            }
        }
        if done.iter().all(|d| *d) {
            break;
        }
    }
    Err(last_miss)
}

/// How many inputs may be away from their first candidate at most.
const MAX_DISCREPANCIES: usize = 64;

fn eval_side(c: &mut Ctx, p: &Probe, side: u8) -> R<Obs> {
    c.side = side;
    c.observe(p)
}

fn dfs(
    c: &mut Ctx,
    a: &Probe,
    b: &Probe,
    budget: &mut usize,
    left: usize,
    pruned: &mut bool,
) -> Result<Option<Found>, Miss> {
    if *budget == 0 {
        return Ok(None);
    }
    *budget -= 1;
    c.guard_skipped = false;
    let ra = eval_side(c, a, 1);
    let rb = match &ra {
        Ok(_) => eval_side(c, b, 2),
        Err(e) => Err(e.clone()),
    };
    if std::env::var_os("SQLDIFF_DEBUG_SEARCH").is_some() {
        eprintln!(
            "  shape {:?} values {:?}\n    A {:?}\n    B {:?}",
            c.shape, c.values, ra, rb
        );
    }
    let need = match (&ra, &rb) {
        (Ok(x), Ok(y)) => {
            return Ok(match differ(x, y, &c.nonnull) {
                Some(true) => Some(Found {
                    inputs: c
                        .inputs
                        .iter()
                        .zip(&c.values)
                        .filter_map(|(i, v)| v.clone().map(|v| (i.key.clone(), v)))
                        .collect(),
                    a: x.clone(),
                    b: y.clone(),
                    assumed: c
                        .inputs
                        .iter()
                        .zip(&c.values)
                        .any(|(i, v)| v.is_some() && i.origin == Origin::Assumed),
                    opaque: c.opaque.clone(),
                    nonnull: c.nonnull.clone(),
                    shape: HashMap::new(),
                    unreached: c.guard_skipped,
                }),
                _ => None,
            })
        }
        (Err(Stop::Need(i)), _) | (_, Err(Stop::Need(i))) => *i,
        (Err(Stop::Reject), _) | (_, Err(Stop::Reject)) => return Ok(None),
        (Err(Stop::Unsupported(m)), _) | (_, Err(Stop::Unsupported(m))) => {
            return Err(Miss::Unsupported(m.clone()))
        }
    };
    let cands = c.inputs[need].cands.clone();
    for (k, v) in cands.into_iter().enumerate() {
        let cost = usize::from(k > 0);
        if cost > left {
            *pruned = true;
            break;
        }
        c.values[need] = Some(v);
        let n_inputs = c.inputs.len();
        if let Some(f) = dfs(c, a, b, budget, left - cost, pruned)? {
            return Ok(Some(f));
        }
        // Forget inputs created below this choice.
        for j in n_inputs..c.inputs.len() {
            let k = c.inputs[j].key.clone();
            c.index.remove(&k);
        }
        c.inputs.truncate(n_inputs);
        c.values.truncate(n_inputs);
        if *budget == 0 {
            break;
        }
    }
    c.values[need] = None;
    Ok(None)
}

/// Base tables a probe reads or writes.
pub fn probe_tables(p: &Probe) -> Vec<String> {
    let mut out = Vec::new();
    match p {
        Probe::Stmt(s) => stmt_tables(s, &mut out),
        Probe::Conds(es) | Probe::Values(es) => {
            for e in es {
                expr_tables(e, &mut out);
            }
        }
    }
    out
}

fn push_table(out: &mut Vec<String>, t: String) {
    if !out.contains(&t) {
        out.push(t);
    }
}

fn stmt_tables(s: &Stmt, out: &mut Vec<String>) {
    match s {
        Stmt::Select(q) => query_tables(q, out),
        Stmt::Insert { source, .. } => match source {
            InsertSource::Values(rows) => {
                for e in rows.iter().flatten() {
                    expr_tables(e, out);
                }
            }
            InsertSource::Query(q) => query_tables(q, out),
            InsertSource::Default => {}
        },
        Stmt::Update {
            target,
            sets,
            from,
            where_,
            ..
        } => {
            push_table(out, table_key(target));
            for f in from {
                from_tables(f, out);
            }
            for (_, e) in sets {
                expr_tables(e, out);
            }
            if let Some(w) = where_ {
                expr_tables(w, out);
            }
        }
        Stmt::Delete {
            target,
            from,
            where_,
            ..
        } => {
            push_table(out, table_key(target));
            for f in from {
                from_tables(f, out);
            }
            if let Some(w) = where_ {
                expr_tables(w, out);
            }
        }
        Stmt::Assign(_, e) | Stmt::Return(Some(e)) => expr_tables(e, out),
        Stmt::Declare {
            default: Some(e), ..
        } => expr_tables(e, out),
        _ => {}
    }
}

fn query_tables(q: &Select, out: &mut Vec<String>) {
    let mut inner = Vec::new();
    for c in &q.ctes {
        query_tables(&c.query, &mut inner);
    }
    for f in &q.from {
        from_tables(f, &mut inner);
    }
    for i in &q.items {
        expr_tables(&i.expr, &mut inner);
    }
    for e in q.where_.iter().chain(q.having.iter()) {
        expr_tables(e, &mut inner);
    }
    for (_, o) in &q.set_ops {
        query_tables(o, &mut inner);
    }
    for t in inner {
        if !q.ctes.iter().any(|c| c.name.eq_ignore_ascii_case(&t)) {
            push_table(out, t);
        }
    }
}

fn from_tables(f: &From, out: &mut Vec<String>) {
    match f {
        From::Table { name, hints, .. } => push_table(out, table_key_with(name, hints)),
        From::Func { name, args, .. } => push_table(out, func_key(name, args)),
        From::Sub { query, .. } => query_tables(query, out),
        From::Join {
            left, right, on, ..
        } => {
            from_tables(left, out);
            from_tables(right, out);
            if let Some(o) = on {
                expr_tables(o, out);
            }
        }
        _ => {}
    }
}

fn expr_tables(e: &Expr, out: &mut Vec<String>) {
    walk_expr(e, &mut |x| match x {
        Expr::Exists(q) | Expr::Subquery(q) | Expr::InQuery(_, q, _) => query_tables(q, out),
        _ => {}
    });
}

/// Rejects the current inputs; with SQLDIFF_DEBUG_SEARCH set, says where.
#[track_caller]
fn rej<T>() -> R<T> {
    if std::env::var_os("SQLDIFF_DEBUG_SEARCH").is_some() {
        eprintln!(
            "    reject at line {}",
            std::panic::Location::caller().line()
        );
    }
    Err(Stop::Reject)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouping_does_not_split_strings_a_collation_may_join() {
        let s = |x: &str| V::Str(x.into());
        assert_eq!(group_same(&s("a"), &s("A")), None);
        assert_eq!(group_same(&s("a"), &s("a ")), None);
        assert_eq!(group_same(&s("a"), &s("b")), Some(false));
        assert_eq!(group_same(&s("a"), &s("a")), Some(true));
        assert_eq!(group_same(&V::Null, &V::Null), Some(true));
        // Output rows still tell 'a' and 'A' apart.
        assert_eq!(same(&s("a"), &s("A")), Some(false));
    }

    #[test]
    fn group_by_names_match_by_column_and_qualifier() {
        let n = |x: &str| x.split('.').map(String::from).collect::<Vec<_>>();
        assert!(same_column(&n("g"), &n("G"), 2));
        assert!(same_column(&n("dbo.t.g"), &n("t.g"), 2));
        assert!(same_column(&n("t.g"), &n("g"), 1));
        assert!(!same_column(&n("t.g"), &n("g"), 2));
        assert!(!same_column(&n("a.g"), &n("b.g"), 2));
        assert!(!same_column(&n("g"), &n("n"), 1));
    }
}
