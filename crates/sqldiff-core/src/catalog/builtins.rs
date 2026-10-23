//! Built-in function, procedure and type catalog per dialect and version,
//! loaded from `profiles/builtins.toml`.

use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

use serde::Deserialize;
use sqldiff_syntax::Dialect;

const DATA: &str = include_str!("../../profiles/builtins.toml");

#[derive(Debug, Deserialize)]
struct File {
    common: Common,
    tsql: Section,
    postgres: Section,
    db2i: Section,
}

#[derive(Debug, Deserialize)]
struct Common {
    keywords: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Section {
    functions: Vec<String>,
    #[serde(default)]
    procedures: Vec<String>,
    types: Vec<String>,
    keywords: Vec<String>,
    #[serde(default)]
    since: BTreeMap<String, String>,
    /// Functions of other dialects verified not to exist in this one.
    #[serde(default)]
    absent: Vec<String>,
}

/// One dialect's built-ins.
#[derive(Debug)]
pub struct Builtins {
    pub dialect: Dialect,
    functions: HashSet<String>,
    procedures: HashSet<String>,
    types: HashSet<String>,
    keywords: HashSet<String>,
    since: BTreeMap<String, String>,
    absent: HashSet<String>,
}

/// Whether a built-in exists at the target version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Available,
    /// Exists from this version on, the target is older.
    Since(String),
    Unknown,
}

/// An organisation-supplied helper function, added by a pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Helper {
    pub name: String,
    pub dialect: Dialect,
    pub returns: Option<String>,
}

fn all() -> &'static [Builtins; 3] {
    static ALL: OnceLock<[Builtins; 3]> = OnceLock::new();
    ALL.get_or_init(|| {
        let f: File = toml::from_str(DATA).expect("built-in catalog is valid");
        let upper = |v: &[String]| v.iter().map(|s| s.to_ascii_uppercase()).collect();
        let common: HashSet<String> = upper(&f.common.keywords);
        let mk = |d: Dialect, s: &Section| {
            let mut keywords: HashSet<String> = upper(&s.keywords);
            keywords.extend(common.iter().cloned());
            // Type names are keywords too (`CONVERT(varchar(10), x)`, `x::int`).
            for t in &s.types {
                keywords.extend(t.split(' ').map(|w| w.to_ascii_uppercase()));
            }
            Builtins {
                dialect: d,
                functions: upper(&s.functions),
                procedures: upper(&s.procedures),
                types: upper(&s.types),
                keywords,
                since: s
                    .since
                    .iter()
                    .map(|(k, v)| (k.to_ascii_uppercase(), v.clone()))
                    .collect(),
                absent: upper(&s.absent),
            }
        };
        [
            mk(Dialect::Tsql, &f.tsql),
            mk(Dialect::Postgres, &f.postgres),
            mk(Dialect::Db2i, &f.db2i),
        ]
    })
}

pub fn of(dialect: Dialect) -> &'static Builtins {
    let i = match dialect {
        Dialect::Tsql => 0,
        Dialect::Postgres => 1,
        Dialect::Db2i => 2,
    };
    &all()[i]
}

/// Compares dotted version strings numerically (`7.5` < `7.10`, `2017` < `2022`).
pub fn version_lt(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split(['.', '-'])
            .map(|p| p.trim().parse().unwrap_or(0))
            .collect()
    };
    parse(a) < parse(b)
}

impl Builtins {
    fn avail(&self, set: &HashSet<String>, name: &str, version: &str) -> Availability {
        let n = name.to_ascii_uppercase();
        if !set.contains(&n) {
            return Availability::Unknown;
        }
        match self.since.get(&n) {
            Some(since) if version_lt(version, since) => Availability::Since(since.clone()),
            _ => Availability::Available,
        }
    }

    /// The function is verified missing from this dialect. The function list is
    /// never complete, so only these names make an unknown call a proven error.
    pub fn known_absent(&self, name: &str) -> bool {
        self.absent.contains(&name.to_ascii_uppercase())
    }

    pub fn function(&self, name: &str, version: &str) -> Availability {
        self.avail(&self.functions, name, version)
    }

    pub fn procedure(&self, name: &str) -> bool {
        let n = name.to_ascii_uppercase();
        self.procedures.contains(&n) || (self.dialect == Dialect::Tsql && n.starts_with("SP_"))
    }

    /// `base` as `DataType::base` spells it.
    pub fn data_type(&self, base: &str, version: &str) -> Availability {
        self.avail(&self.types, base.trim_end_matches("[]"), version)
    }

    pub fn is_keyword(&self, word: &str) -> bool {
        self.keywords.contains(&word.to_ascii_uppercase())
    }
}

/// Other dialects that have this function built in.
pub fn function_elsewhere(name: &str, here: Dialect) -> Vec<Dialect> {
    Dialect::ALL
        .iter()
        .copied()
        .filter(|d| *d != here && of(*d).functions.contains(&name.to_ascii_uppercase()))
        .collect()
}

pub fn type_elsewhere(base: &str, here: Dialect) -> Vec<Dialect> {
    let b = base.trim_end_matches("[]").to_ascii_uppercase();
    Dialect::ALL
        .iter()
        .copied()
        .filter(|d| *d != here && of(*d).types.contains(&b))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_and_versions() {
        let t = of(Dialect::Tsql);
        assert_eq!(t.function("getdate", "2022"), Availability::Available);
        assert_eq!(
            t.function("STRING_AGG", "2016"),
            Availability::Since("2017".into())
        );
        assert_eq!(t.function("NOW", "2022"), Availability::Unknown);
        assert_eq!(
            function_elsewhere("GETDATE", Dialect::Postgres),
            [Dialect::Tsql]
        );
        assert_eq!(
            of(Dialect::Postgres).function("uuidv7", "17"),
            Availability::Since("18".into())
        );
        assert!(of(Dialect::Db2i).is_keyword("leave"));
        assert!(version_lt("7.5", "7.10"));
        assert_eq!(
            of(Dialect::Postgres).data_type("TEXT", "18"),
            Availability::Available
        );
        assert_eq!(
            of(Dialect::Db2i).data_type("TEXT", "7.6"),
            Availability::Unknown
        );
    }
}
