//! The assumption registry (docs/verified-conversion.md, C0 item 7): every
//! `ASM-*` id the engine, the rule files and the converter can report, with
//! the assumption in one line, what can discharge it and the files that use
//! it. The data is `profiles/assumptions.toml`; `docs/assumptions.md` is
//! generated from it ([`markdown`]).
//!
//! Completeness is checked by `tests/assumptions.rs`, which scans the
//! sources: an id used but not registered, or registered but not used, fails
//! it. [`crate::model::Assumption::default`] also refuses an unregistered id
//! in debug builds.
//!
//! Ids come in three shapes:
//! - `ASM-NAME`, registered as is;
//! - `ASM-NAME:qualifier`, an instance of `ASM-NAME` (binding assumptions
//!   name the object they are about);
//! - `ASM-NAME-rest`, a member of the family `ASM-NAME-*`, whose rest the
//!   code builds (a vendor fact number, a table name, a hash).
//!
//! An id registered as is wins over a family it also matches.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

const DATA: &str = include_str!("../profiles/assumptions.toml");

/// What can discharge an assumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Discharge {
    /// A fact a catalog snapshot holds (keys, types, collation, triggers).
    Catalog,
    /// Code the converter or the generator writes (a constraint, the
    /// converted callee, the woven instances).
    Generated,
    /// A declaration about the deployment that no artifact shows (callers,
    /// sessions, data).
    Deployment,
    /// None of these: a vendor fact still to verify, an engine limit or a
    /// per-routine review. A deployment profile cannot accept it.
    Nothing,
}

impl Discharge {
    pub const ALL: [Discharge; 4] = [
        Discharge::Catalog,
        Discharge::Generated,
        Discharge::Deployment,
        Discharge::Nothing,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Discharge::Catalog => "catalog",
            Discharge::Generated => "generated",
            Discharge::Deployment => "deployment",
            Discharge::Nothing => "nothing",
        }
    }
}

/// One registered assumption, or a family of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// `ASM-NAME`, or `ASM-NAME-*` for a family.
    pub id: String,
    /// The assumption in one line. Reports show the emitting rule's own text.
    pub statement: String,
    pub discharge: Discharge,
    /// The fact that would discharge it.
    pub fact: String,
    /// The `[deployment.facts]` key that asserts that fact, if there is one.
    #[serde(default)]
    pub fact_key: Option<String>,
    /// The files that use the id, relative to `crates/`.
    pub sites: Vec<String>,
}

impl Entry {
    pub fn is_family(&self) -> bool {
        self.id.ends_with('*')
    }

    /// The prefix every member of a family starts with (`ASM-FACT-`).
    pub fn family_prefix(&self) -> Option<&str> {
        self.id.strip_suffix('*')
    }

    /// Whether a deployment profile may accept the assumption.
    pub fn acceptable(&self) -> bool {
        self.discharge != Discharge::Nothing
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    assumption: Vec<Entry>,
}

/// Every registered entry, sorted by id.
pub fn registry() -> &'static [Entry] {
    static R: OnceLock<Vec<Entry>> = OnceLock::new();
    R.get_or_init(|| {
        let mut v = toml::from_str::<File>(DATA)
            .expect("profiles/assumptions.toml is valid")
            .assumption;
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    })
}

/// The id without an instance qualifier: `ASM-BIND-COLUMN:qty` is
/// `ASM-BIND-COLUMN`.
pub fn base(id: &str) -> &str {
    id.split_once(':').map_or(id, |(b, _)| b)
}

/// The entry an id reported by the code belongs to: the id registered as is
/// (after its qualifier is removed), else the family with the longest prefix
/// that matches it.
pub fn lookup(id: &str) -> Option<&'static Entry> {
    let b = base(id);
    let r = registry();
    if let Ok(i) = r.binary_search_by(|e| e.id.as_str().cmp(b)) {
        return Some(&r[i]);
    }
    r.iter()
        .filter_map(|e| Some((e, e.family_prefix()?)))
        .filter(|(_, p)| b.len() > p.len() && b.starts_with(p))
        .max_by_key(|(_, p)| p.len())
        .map(|(e, _)| e)
}

/// Every `fact_key` the registry names, sorted.
pub fn fact_keys() -> BTreeSet<&'static str> {
    registry()
        .iter()
        .filter_map(|e| e.fact_key.as_deref())
        .collect()
}

/// The entries a `[deployment.facts]` key bears on.
pub fn bearing_on(key: &str) -> Vec<&'static Entry> {
    registry()
        .iter()
        .filter(|e| e.fact_key.as_deref() == Some(key))
        .collect()
}

/// Checks one id of a deployment profile's `accept` list: a registered id
/// (which also covers its `:qualified` instances), one instance, or one
/// member of a family; never a whole family, and never an assumption
/// nothing can discharge.
pub fn check_accept(id: &str) -> Result<&'static Entry, String> {
    if id.contains('*') {
        return Err(format!(
            "`{id}`: accept names assumptions one by one, not a whole family"
        ));
    }
    let e = lookup(id).ok_or_else(|| {
        format!("`{id}` is not a registered assumption id (see docs/assumptions.md)")
    })?;
    if e.is_family() && id.contains(':') {
        return Err(format!("`{id}`: a family member has no qualifier"));
    }
    if !e.acceptable() {
        return Err(format!(
            "`{id}` cannot be accepted by a deployment: nothing a deployment declares discharges it ({})",
            e.fact
        ));
    }
    Ok(e)
}

/// Whether an `accept` entry covers an id a report carries: the same id, or
/// an instance (`ASM-BIND-COLUMN:qty`) of a registered id it names.
pub fn covers(accepted: &str, reported: &str) -> bool {
    if accepted == reported {
        return true;
    }
    if accepted.contains(':') {
        return false;
    }
    // Only an id registered as is has instances; a family member is exact.
    let registered = registry()
        .binary_search_by(|e| e.id.as_str().cmp(accepted))
        .is_ok();
    registered && reported.split_once(':').is_some_and(|(b, _)| b == accepted)
}

/// `docs/assumptions.md`.
pub fn markdown() -> String {
    let r = registry();
    let mut s = String::new();
    let _ = writeln!(s, "# Assumptions");
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "Generated from `crates/sqldiff-core/profiles/assumptions.toml`; do not edit. \
         After changing the registry run \
         `SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test assumptions`."
    );
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "Every `ASM-*` id SQLDiff can report (docs/verified-conversion.md, C0 item 7). \
         The test that regenerates this file also scans the sources and fails when \
         an id is used but not registered, registered but not used, or used in a \
         file its entry does not list. A report shows the emitting rule's own text; \
         the statement here is the assumption in one line."
    );
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "An id `ASM-NAME:qualifier` is an instance of `ASM-NAME` (binding \
         assumptions name the object). `ASM-NAME-*` is a family whose rest the code \
         builds; an id registered as is wins over a family it also matches."
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "## What discharges an assumption");
    let _ = writeln!(s);
    let _ = writeln!(s, "| Discharge | Meaning | Ids |");
    let _ = writeln!(s, "|---|---|---|");
    for d in Discharge::ALL {
        let meaning = match d {
            Discharge::Catalog => "A fact a catalog snapshot holds: keys, types, collation, triggers, routines",
            Discharge::Generated => "Code the converter or the generator writes: a constraint, a cast, the converted callee, the woven instances",
            Discharge::Deployment => "A declaration about the deployment no artifact shows: callers, sessions, data",
            Discharge::Nothing => "None of these: a vendor fact still to verify, an engine limit, or a per-routine review. A deployment profile cannot accept it",
        };
        let n = r.iter().filter(|e| e.discharge == d).count();
        let _ = writeln!(s, "| `{}` | {meaning} | {n} |", d.label());
    }
    let _ = writeln!(s, "| | Total | {} |", r.len());
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "A deployment profile (`[deployment] accept`, docs/packs-and-config.md) may \
         accept any id whose discharge is not `nothing`. Accepting does not change a \
         verdict: the unit stays \"equal under assumptions\" and the report names \
         the profile that accepted each assumption."
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "## Fact keys");
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "Keys of `[deployment.facts]`. A fact asserted there is reported as the \
         user's own declaration; it changes no verdict yet."
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "| Key | Bears on |");
    let _ = writeln!(s, "|---|---|");
    for k in fact_keys() {
        let ids: Vec<String> = bearing_on(k)
            .iter()
            .map(|e| format!("`{}`", e.id))
            .collect();
        let _ = writeln!(s, "| `{k}` | {} |", ids.join(", "));
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "## Registry");
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "| Id | Assumption | Discharge | Fact that discharges it | Used in |"
    );
    let _ = writeln!(s, "|---|---|---|---|---|");
    for e in r {
        let key = e
            .fact_key
            .as_deref()
            .map(|k| format!(" (fact key `{k}`)"))
            .unwrap_or_default();
        let sites: Vec<String> = e.sites.iter().map(|x| format!("`{x}`")).collect();
        let _ = writeln!(
            s,
            "| `{}` | {} | `{}` | {}{key} | {} |",
            e.id,
            cell(&e.statement),
            e.discharge.label(),
            cell(&e.fact),
            sites.join("<br>")
        );
    }
    s
}

/// Markdown table cell text.
fn cell(s: &str) -> String {
    s.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_resolves_instances_and_families() {
        assert_eq!(lookup("ASM-BIND-COLUMN:qty").unwrap().id, "ASM-BIND-COLUMN");
        assert_eq!(lookup("ASM-FACT-V2").unwrap().id, "ASM-FACT-*");
        assert_eq!(
            lookup("ASM-IDENTITY-VALUES").unwrap().id,
            "ASM-IDENTITY-VALUES"
        );
        // A table name is an instance qualifier, never part of the id: a
        // table named `values` cannot become ASM-IDENTITY-VALUES.
        assert_eq!(
            lookup("ASM-IDENTITY-COLUMN:orders").unwrap().id,
            "ASM-IDENTITY-COLUMN"
        );
        assert!(lookup("ASM-IDENTITY-ORDERS").is_none());
        assert!(lookup("ASM-FACT-").is_none());
        assert!(lookup("ASM-NOT-THERE").is_none());
    }

    #[test]
    fn accept_checks_ids() {
        assert!(check_accept("ASM-OBSERVER-ROWCOUNT").is_ok());
        assert!(check_accept("ASM-BIND-COLUMN:qty").is_ok());
        assert!(check_accept("ASM-NOT-THERE").is_err());
        assert!(check_accept("ASM-FACT-*").is_err());
        // Nothing a deployment declares discharges a vendor fact.
        assert!(check_accept("ASM-FACT-V2").is_err());
        assert!(check_accept("ASM-HAND-WRITTEN").is_err());
    }

    #[test]
    fn covers_instances_of_registered_ids_only() {
        assert!(covers("ASM-BIND-COLUMN", "ASM-BIND-COLUMN:qty"));
        assert!(covers("ASM-BIND-COLUMN:qty", "ASM-BIND-COLUMN:qty"));
        assert!(!covers("ASM-BIND-COLUMN:qty", "ASM-BIND-COLUMN:price"));
        assert!(!covers("ASM-BIND-COLUMN:qty", "ASM-BIND-COLUMN"));
        assert!(!covers("ASM-BIND-COLUMN", "ASM-BIND-COLUMNS"));
        assert!(!covers(
            "ASM-IDENTITY-COLUMN:orders",
            "ASM-IDENTITY-COLUMN:values"
        ));
        assert!(!covers("ASM-IDENTITY", "ASM-IDENTITY-VALUES"));
    }

    #[test]
    fn registry_is_well_formed() {
        let r = registry();
        for w in r.windows(2) {
            assert_ne!(w[0].id, w[1].id, "registered twice");
        }
        for e in r {
            assert!(e.id.starts_with("ASM-"), "{}", e.id);
            let name = e.id.strip_suffix('*').unwrap_or(&e.id);
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-'),
                "{}",
                e.id
            );
            assert!(
                !e.statement.contains('\n') && !e.statement.is_empty(),
                "{}",
                e.id
            );
            assert!(!e.fact.is_empty(), "{}", e.id);
            assert!(!e.sites.is_empty(), "{}", e.id);
        }
    }
}
