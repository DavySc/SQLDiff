//! Dialect targets: which engine version (and, for T-SQL, which platform and
//! compatibility level) an artifact is analysed for, validated against the
//! vendor lifecycle data in `profiles/lifecycle.toml` (decisions 6 and 7 in
//! docs/verification-roadmap.md).
//!
//! The analysis never reads the clock: [`status`] takes the date as an
//! argument, and only the CLI supplies today's.

use std::collections::BTreeSet;
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;
use sqldiff_syntax::Dialect;

const DATA: &str = include_str!("../profiles/lifecycle.toml");

#[derive(Debug, Deserialize)]
struct File {
    postgres: Vec<Entry>,
    tsql: Vec<Entry>,
    db2i: Vec<Entry>,
}

/// One version's vendor lifecycle.
#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    pub version: String,
    /// Absent: not released yet (preview).
    pub released: Option<String>,
    /// Absent: no end of support announced.
    pub support_ends: Option<String>,
    pub legacy_ends: Option<String>,
    pub source: String,
    /// `verified` once a person read the dates on the vendor's page.
    pub checked: String,
}

fn file() -> &'static File {
    static F: OnceLock<File> = OnceLock::new();
    F.get_or_init(|| toml::from_str(DATA).expect("lifecycle data is valid"))
}

/// The lifecycle entries of a dialect, in file order.
pub fn entries(d: Dialect) -> &'static [Entry] {
    let f = file();
    match d {
        Dialect::Tsql => &f.tsql,
        Dialect::Postgres => &f.postgres,
        Dialect::Db2i => &f.db2i,
    }
}

fn entry(d: Dialect, key: &str) -> Option<&'static Entry> {
    entries(d).iter().find(|e| e.version == key)
}

/// Where a version stands in its vendor's lifecycle on a given date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Preview,
    Supported,
    Legacy,
    Retired,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Preview => "preview",
            Status::Supported => "supported",
            Status::Legacy => "legacy",
            Status::Retired => "retired",
        }
    }
}

/// A date `YYYY-MM` is the first of that month, so it compares before every
/// `YYYY-MM-DD` of the month.
fn on_or_after(date: &str, bound: &str) -> bool {
    date >= bound
}

/// The status of a lifecycle entry on `date` (`YYYY-MM-DD`).
pub fn status_of(e: &Entry, date: &str) -> Status {
    match &e.released {
        None => return Status::Preview,
        Some(r) if !on_or_after(date, r) => return Status::Preview,
        _ => {}
    }
    match (&e.support_ends, &e.legacy_ends) {
        (Some(s), _) if !on_or_after(date, s) => Status::Supported,
        (None, _) => Status::Supported,
        (Some(_), Some(l)) if !on_or_after(date, l) => Status::Legacy,
        _ => Status::Retired,
    }
}

/// The platform a T-SQL target runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// SQL Server installed by its owner.
    Boxed,
    /// Azure SQL Managed Instance.
    ManagedInstance,
    /// Azure SQL Database.
    Azure,
    /// SQL database in Microsoft Fabric.
    Fabric,
}

/// A validated target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Postgres {
        major: u16,
    },
    Tsql {
        platform: Platform,
        /// SQL Server release year; `None` for an evergreen platform or
        /// update policy.
        engine: Option<u16>,
        compat: u16,
    },
    Db2i {
        release: String,
    },
}

/// Native database compatibility level of a SQL Server release.
fn native_level(year: u16) -> Option<u16> {
    Some(match year {
        2016 => 130,
        2017 => 140,
        2019 => 150,
        2022 => 160,
        2025 => 170,
        _ => return None,
    })
}

/// The newest SQL Server release in the lifecycle data, which evergreen
/// platforms are analysed as (decision 7, `ASM-AZURE-AS-LATEST`).
pub fn newest_sql_server() -> u16 {
    entries(Dialect::Tsql)
        .iter()
        .filter_map(|e| e.version.parse::<u16>().ok())
        .max()
        .expect("lifecycle data lists a SQL Server release")
}

impl Target {
    /// Parses and validates a version string for a dialect: PostgreSQL
    /// `18` (a minor such as `18.6` is accepted and dropped); T-SQL `2022`,
    /// `2022/150`, `mi-2022`, `mi-2025`, `mi-evergreen`, `azure` or `fabric`,
    /// each with an optional `/level`; Db2 for i `7.5`. A version without a
    /// lifecycle entry is refused, with the known ones.
    pub fn parse(d: Dialect, s: &str) -> Result<Target, String> {
        let s = s.trim().to_ascii_lowercase();
        let known = || {
            entries(d)
                .iter()
                .map(|e| e.version.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let unknown = |what: &str| {
            format!(
                "unknown {} version `{what}`; known: {}",
                d.display_name(),
                known()
            )
        };
        match d {
            Dialect::Postgres => {
                let major = s.split('.').next().unwrap_or_default();
                if entry(d, major).is_none() {
                    return Err(unknown(&s));
                }
                Ok(Target::Postgres {
                    major: major.parse().map_err(|_| unknown(&s))?,
                })
            }
            Dialect::Db2i => {
                if entry(d, &s).is_none() {
                    return Err(unknown(&s));
                }
                Ok(Target::Db2i { release: s })
            }
            Dialect::Tsql => {
                let (base, level) = match s.split_once('/') {
                    Some((b, l)) => (b, Some(l)),
                    None => (s.as_str(), None),
                };
                if entry(d, base).is_none() {
                    return Err(unknown(base));
                }
                let (platform, engine) = match base {
                    "azure" => (Platform::Azure, None),
                    "fabric" => (Platform::Fabric, None),
                    "mi-evergreen" => (Platform::ManagedInstance, None),
                    b => match b.strip_prefix("mi-") {
                        Some(y) => (Platform::ManagedInstance, y.parse().ok()),
                        None => (Platform::Boxed, b.parse().ok()),
                    },
                };
                let top = native_level(engine.unwrap_or_else(newest_sql_server))
                    .ok_or_else(|| unknown(base))?;
                let compat = match level {
                    None => top,
                    Some(l) => {
                        let n: u16 = l
                            .parse()
                            .map_err(|_| format!("compatibility level `{l}` is not a number"))?;
                        if n % 10 != 0 || !(100..=top).contains(&n) {
                            return Err(format!(
                                "compatibility level {n} is not one {base} offers (100 to {top})"
                            ));
                        }
                        n
                    }
                };
                Ok(Target::Tsql {
                    platform,
                    engine,
                    compat,
                })
            }
        }
    }

    /// The lifecycle key: `18`, `2022`, `mi-2022`, `azure`, `7.5`.
    pub fn key(&self) -> String {
        match self {
            Target::Postgres { major } => major.to_string(),
            Target::Db2i { release } => release.clone(),
            Target::Tsql {
                platform, engine, ..
            } => match (platform, engine) {
                (Platform::Boxed, Some(y)) => y.to_string(),
                (Platform::ManagedInstance, Some(y)) => format!("mi-{y}"),
                (Platform::ManagedInstance, None) => "mi-evergreen".into(),
                (Platform::Azure, _) => "azure".into(),
                (Platform::Fabric, _) => "fabric".into(),
                (Platform::Boxed, None) => unreachable!("a boxed target has a release"),
            },
        }
    }

    /// Canonical spelling: the key, plus `/level` when the compatibility
    /// level is not the engine's native one.
    pub fn canonical(&self) -> String {
        match self {
            Target::Tsql { engine, compat, .. }
                if Some(*compat) != native_level(engine.unwrap_or_else(newest_sql_server)) =>
            {
                format!("{}/{compat}", self.key())
            }
            _ => self.key(),
        }
    }

    /// The engine version built-in availability is checked against
    /// (`[*.since]` in `profiles/builtins.toml`): the PostgreSQL major, the
    /// SQL Server release (the newest for evergreen platforms), the IBM i
    /// release.
    pub fn engine_version(&self) -> String {
        match self {
            Target::Postgres { major } => major.to_string(),
            Target::Db2i { release } => release.clone(),
            Target::Tsql { engine, .. } => engine.unwrap_or_else(newest_sql_server).to_string(),
        }
    }

    pub fn lifecycle(&self, d: Dialect) -> &'static Entry {
        entry(d, &self.key()).expect("a parsed target has a lifecycle entry")
    }
}

/// The engine version for built-in availability from a version string, or
/// the string itself when it does not parse (callers validate at the edge).
pub fn engine_version(d: Dialect, version: &str) -> String {
    Target::parse(d, version)
        .map(|t| t.engine_version())
        .unwrap_or_else(|_| version.to_string())
}

/// The lifecycle status of a version string on `date`, with its entry.
pub fn status(d: Dialect, version: &str, date: &str) -> Option<(Status, &'static Entry)> {
    let t = Target::parse(d, version).ok()?;
    let e = t.lifecycle(d);
    Some((status_of(e, date), e))
}

/// Days since 1970-01-01 of a `YYYY-MM-DD` or `YYYY-MM` (the first) date.
fn days(date: &str) -> Option<i64> {
    let mut it = date.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next().map_or(Some(1), |s| s.parse().ok())?;
    // Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe - 719468)
}

/// The `YYYY-MM-DD` date `n` days after 1970-01-01.
pub fn date_from_days(n: i64) -> String {
    let z = n + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// How many days before a status change a notice warns.
pub const NOTICE_DAYS: i64 = 90;

/// A line for the user about a target's lifecycle on `date`: preview,
/// legacy or retired, or within [`NOTICE_DAYS`] of the next change. `None`
/// for a supported version with no change near.
pub fn notice(d: Dialect, version: &str, date: &str) -> Option<String> {
    let t = Target::parse(d, version).ok()?;
    let e = t.lifecycle(d);
    let name = format!("{} {}", d.display_name(), t.canonical());
    let hedge = if e.checked == "verified" {
        String::new()
    } else {
        "; the lifecycle dates are not yet checked against the vendor's page".to_string()
    };
    let soon = |bound: &Option<String>| -> Option<(String, i64)> {
        let b = bound.as_ref()?;
        let left = days(b)? - days(date)?;
        (0..=NOTICE_DAYS).contains(&left).then(|| (b.clone(), left))
    };
    let text = match status_of(e, date) {
        Status::Preview => format!(
            "{name} is a preview version: it is analysed, but the verdicts rest on a release that does not exist yet"
        ),
        Status::Supported => {
            let (b, left) = soon(&e.support_ends)?;
            format!("{name} leaves vendor support on {b} ({left} days); SQLDiff then treats it as legacy")
        }
        Status::Legacy => {
            let since = e.support_ends.clone().unwrap_or_default();
            match soon(&e.legacy_ends) {
                Some((b, left)) => format!(
                    "{name} is legacy (vendor support ended {since}) and is retired on {b} ({left} days)"
                ),
                None => format!(
                    "{name} is legacy: vendor support ended {since}; its semantics are frozen"
                ),
            }
        }
        Status::Retired => format!(
            "{name} is retired: vendor support ended {}; results for it are kept only for migration reviews",
            e.support_ends.clone().unwrap_or_default()
        ),
    };
    Some(text + &hedge)
}

fn used_set() -> &'static Mutex<BTreeSet<(String, String)>> {
    static USED: OnceLock<Mutex<BTreeSet<(String, String)>>> = OnceLock::new();
    USED.get_or_init(Default::default)
}

/// Records that a run analyses a target (called by `Artifact::set_version`),
/// so a front end can show its lifecycle notices once.
pub fn record_use(d: Dialect, canonical: &str) {
    if let Ok(mut s) = used_set().lock() {
        s.insert((d.id().to_string(), canonical.to_string()));
    }
}

/// The targets recorded so far, each once.
pub fn used() -> Vec<(Dialect, String)> {
    used_set()
        .lock()
        .map(|s| {
            s.iter()
                .filter_map(|(d, v)| Some((d.parse().ok()?, v.clone())))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Dialect = Dialect::Tsql;
    const P: Dialect = Dialect::Postgres;
    const D: Dialect = Dialect::Db2i;

    #[test]
    fn data_loads_and_defaults_parse() {
        for d in Dialect::ALL {
            let t = Target::parse(d, d.default_version()).unwrap();
            assert_eq!(t.canonical(), d.default_version());
        }
    }

    #[test]
    fn canonical_spellings_keep_todays_strings() {
        for (d, s, c) in [
            (P, "18", "18"),
            (P, "18.6", "18"),
            (T, "2019", "2019"),
            (T, "2019/150", "2019"),
            (T, "2019/130", "2019/130"),
            (T, "MI-2022", "mi-2022"),
            (T, "mi-2025/160", "mi-2025/160"),
            (T, "azure", "azure"),
            (T, "azure/170", "azure"),
            (T, "fabric/150", "fabric/150"),
            (D, "7.5", "7.5"),
        ] {
            assert_eq!(Target::parse(d, s).unwrap().canonical(), c, "{s}");
        }
    }

    #[test]
    fn unknown_versions_and_levels_are_refused() {
        for (d, s) in [
            (P, "11"),
            (P, ""),
            (T, "2012"),
            (T, "2019/160"),
            (T, "2022/95"),
            (T, "2022/155"),
            (T, "mi-2019"),
            (T, "azure/x"),
            (D, "7.7"),
        ] {
            assert!(Target::parse(d, s).is_err(), "{s} parsed");
        }
    }

    #[test]
    fn evergreen_targets_use_the_newest_release_for_builtins() {
        let newest = newest_sql_server().to_string();
        for s in ["azure", "fabric", "mi-evergreen"] {
            assert_eq!(Target::parse(T, s).unwrap().engine_version(), newest);
        }
        assert_eq!(
            Target::parse(T, "mi-2022/150").unwrap().engine_version(),
            "2022"
        );
        assert_eq!(engine_version(P, "17.4"), "17");
    }

    #[test]
    fn status_follows_the_dates() {
        let s = |d, v, date| status(d, v, date).unwrap().0;
        assert_eq!(s(P, "19", "2026-10-28"), Status::Preview);
        assert_eq!(s(P, "18", "2026-10-28"), Status::Supported);
        assert_eq!(s(P, "14", "2026-11-11"), Status::Supported);
        assert_eq!(s(P, "14", "2026-11-12"), Status::Legacy);
        assert_eq!(s(P, "13", "2026-11-13"), Status::Retired);
        assert_eq!(s(T, "2016", "2026-10-28"), Status::Legacy);
        assert_eq!(s(T, "azure", "2040-01-01"), Status::Supported);
        assert_eq!(s(D, "7.4", "2026-10-28"), Status::Legacy);
        // A month-only date is the first of the month.
        assert_eq!(s(P, "15", "2027-10-31"), Status::Supported);
        assert_eq!(s(P, "15", "2027-11-01"), Status::Legacy);
    }

    #[test]
    fn dates_round_trip_and_notices_say_why() {
        for date in ["1970-01-01", "2000-02-29", "2026-10-28", "2100-03-01"] {
            assert_eq!(date_from_days(days(date).unwrap()), date);
        }
        assert_eq!(
            days("2026-11-12").unwrap() - days("2026-10-28").unwrap(),
            15
        );
        let n = |d, v, date| notice(d, v, date);
        assert!(n(P, "18", "2026-10-28").is_none());
        assert!(n(P, "14", "2026-10-28")
            .unwrap()
            .contains("2026-11-12 (15 days)"));
        assert!(n(P, "19", "2026-10-28").unwrap().contains("preview"));
        assert!(n(T, "2016", "2026-10-28").unwrap().contains("legacy"));
        assert!(n(P, "13", "2027-01-01").unwrap().contains("retired"));
        assert!(n(T, "2019/130", "2026-10-28").is_none());
    }

    #[test]
    fn every_entry_is_consistent() {
        for d in Dialect::ALL {
            for e in entries(d) {
                let date = |x: &Option<String>| {
                    if let Some(x) = x {
                        assert!(
                            x.len() == 7 || x.len() == 10,
                            "{}: date `{x}` is not YYYY-MM or YYYY-MM-DD",
                            e.version
                        );
                    }
                };
                date(&e.released);
                date(&e.support_ends);
                date(&e.legacy_ends);
                if let (Some(s), Some(l)) = (&e.support_ends, &e.legacy_ends) {
                    assert!(l >= s, "{}: legacy ends before support", e.version);
                }
                assert!(e.support_ends.is_some() || e.legacy_ends.is_none());
                assert!(["verified", "unverified"].contains(&e.checked.as_str()));
                Target::parse(d, &e.version).unwrap();
            }
        }
    }
}
