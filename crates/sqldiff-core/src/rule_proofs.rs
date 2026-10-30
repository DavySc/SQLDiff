//! The rule proof registry (docs/verified-conversion.md, C0.9): every
//! conversion rule id the converter's notes and trace steps can carry
//! (`tsql.*`, `pg.*`, `idiom.*`, `db2.*`) and every rule-file head
//! (`translate`, `idiom`, `lint`), with what backs it: a lemma of the Lean
//! lowering, a vendor fact, or the engine alone. The data is
//! `profiles/rule-proofs.toml`; `docs/proof-coverage.md` is generated from
//! it ([`markdown`]).
//!
//! Completeness is checked by `tests/rule_proofs.rs`, which scans the
//! converter's sources and the rule files: an id used but not registered,
//! or registered but not used, fails it.
//!
//! The Lean checker does not read rule ids: it re-lowers the output on disk.
//! A rule is lemma-backed when the construct it rewrites, in the form the
//! converter writes it, is inside the lowering's spec; the proof then cites
//! the section of `formal/lean/Sqldiff/Mirror/Lower.lean` whose rule reads
//! that form. The citation is a pointer for reviewers, not something the
//! checker consumes.
//!
//! Ids come in three shapes:
//! - `tsql.ctl.if`, registered as is;
//! - `tsql.func.*`, a family whose rest the code builds (the function, the
//!   type, the global variable);
//! - a member of a family registered on its own (`tsql.func.len`, with
//!   `family = "tsql.func.*"`), so that it can carry its own proof.
//!
//! An id registered as is wins over a family it also matches.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

const DATA: &str = include_str!("../profiles/rule-proofs.toml");

/// Where a rule id comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A string literal in the converter's Rust sources.
    Converter,
    /// The head of a `(translate ...)` form in a rule file.
    Translate,
    /// The head of an `(idiom ...)` form in a rule file.
    Idiom,
    /// The head of a `(lint ...)` form in a rule file.
    Lint,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Converter, Kind::Translate, Kind::Idiom, Kind::Lint];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Converter => "converter",
            Kind::Translate => "translate",
            Kind::Idiom => "idiom",
            Kind::Lint => "lint",
        }
    }

    /// Whether ids of this kind are the converter's dotted literals.
    pub fn is_converter(self) -> bool {
        self == Kind::Converter
    }
}

/// What backs a rule, parsed from the registry's `proof` string.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Proof {
    /// `lemma:<citation>`: the construct is mirrored by the Lean lowering;
    /// the citation names a section of `Lower.lean` and, after a slash,
    /// the rule within it.
    Lemma(String),
    /// `axiom:V<n>`: rests on vendor fact `V<n>` of
    /// docs/vendor-facts-verification.md, with no lemma.
    Axiom(u32),
    /// `engine`: backed only by the Rust engine comparison, the tests and
    /// the probes.
    Engine,
}

/// The three statuses, in the order the tables show them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    Lemma,
    Axiom,
    Engine,
}

impl Status {
    pub const ALL: [Status; 3] = [Status::Lemma, Status::Axiom, Status::Engine];

    pub fn label(self) -> &'static str {
        match self {
            Status::Lemma => "lemma",
            Status::Axiom => "axiom",
            Status::Engine => "engine",
        }
    }
}

impl Proof {
    /// Parses the registry's string form; the error says what is wrong.
    pub fn parse(s: &str) -> Result<Proof, String> {
        if s == "engine" {
            return Ok(Proof::Engine);
        }
        if let Some(c) = s.strip_prefix("lemma:") {
            let ok = !c.is_empty()
                && !c.starts_with('/')
                && !c.ends_with('/')
                && !c.contains("//")
                && !c.contains('\n')
                && c.trim() == c;
            return if ok {
                Ok(Proof::Lemma(c.to_string()))
            } else {
                Err(format!(
                    "`{s}`: a lemma cites `<section>[/<rule>]` of Lower.lean"
                ))
            };
        }
        if let Some(v) = s.strip_prefix("axiom:") {
            let n = v
                .strip_prefix('V')
                .filter(|d| !d.is_empty() && !d.starts_with('0'))
                .and_then(|d| d.parse::<u32>().ok());
            return match n {
                Some(n) => Ok(Proof::Axiom(n)),
                None => Err(format!("`{s}`: an axiom names a vendor fact `V<n>`")),
            };
        }
        Err(format!(
            "`{s}`: proof is `lemma:<citation>`, `axiom:V<n>` or `engine`"
        ))
    }

    pub fn status(&self) -> Status {
        match self {
            Proof::Lemma(_) => Status::Lemma,
            Proof::Axiom(_) => Status::Axiom,
            Proof::Engine => Status::Engine,
        }
    }

    /// The citation (`Sqldiff.Mirror/C2b strings/LEN`, `V19`), if any.
    pub fn cite(&self) -> Option<String> {
        match self {
            Proof::Lemma(c) => Some(c.clone()),
            Proof::Axiom(n) => Some(format!("V{n}")),
            Proof::Engine => None,
        }
    }
}

/// One registered rule id, a family of them, or a member of a family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// `tsql.ctl.if`, `len/length`, or `tsql.func.*` for a family.
    pub id: String,
    pub kind: Kind,
    /// `lemma:<citation>`, `axiom:V<n>` or `engine` ([`Proof::parse`]).
    pub proof: String,
    /// The family a member registered on its own belongs to.
    #[serde(default)]
    pub family: Option<String>,
    /// Where a lemma is partial, or what the status rests on.
    #[serde(default)]
    pub note: Option<String>,
    /// The files that use the id, relative to `crates/`.
    pub sites: Vec<String>,
}

impl Entry {
    pub fn is_family(&self) -> bool {
        self.id.ends_with('*')
    }

    /// The prefix every member of a family starts with (`tsql.func.`).
    pub fn family_prefix(&self) -> Option<&str> {
        self.id.strip_suffix('*')
    }

    /// The parsed proof; the registry is validated, so this cannot fail.
    pub fn proof(&self) -> Proof {
        Proof::parse(&self.proof).expect("validated by registry()")
    }

    pub fn status(&self) -> Status {
        self.proof().status()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    rule: Vec<Entry>,
}

/// Whether an id has the converter's shape: `tsql.x`, `pg.x`, `idiom.x`,
/// `db2.x` (also with a trailing `*` for a family).
pub fn converter_id(id: &str) -> bool {
    let body = id.strip_suffix('*').unwrap_or(id);
    ["tsql.", "pg.", "idiom.", "db2."]
        .iter()
        .any(|p| body.starts_with(p))
        && body.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' || c == '_'
        })
        && !body.ends_with('-')
}

/// Checks the registry's rules; the errors name the entry.
fn validate(v: &[Entry]) -> Result<(), String> {
    let mut problems = Vec::new();
    for w in v.windows(2) {
        if w[0].id == w[1].id {
            problems.push(format!("`{}` is registered twice", w[0].id));
        }
    }
    for e in v {
        let id = &e.id;
        if let Err(why) = Proof::parse(&e.proof) {
            problems.push(format!("`{id}`: {why}"));
        }
        if e.kind.is_converter() != converter_id(id) {
            problems.push(format!(
                "`{id}`: kind `{}` does not fit the id's shape",
                e.kind.label()
            ));
        }
        if e.is_family() {
            if !e.kind.is_converter() {
                problems.push(format!("`{id}`: only converter ids come in families"));
            }
            if e.family.is_some() {
                problems.push(format!("`{id}`: a family is not a member"));
            }
        }
        if let Some(f) = &e.family {
            match v.binary_search_by(|x| x.id.as_str().cmp(f)) {
                Ok(i) => {
                    let fam = &v[i];
                    let p = fam.family_prefix().unwrap_or("");
                    if !fam.is_family() || !id.starts_with(p) || id.len() <= p.len() {
                        problems.push(format!("`{id}`: not a member of `{f}`"));
                    }
                    if fam.sites != e.sites {
                        problems.push(format!(
                            "`{id}`: a member's sites are its family's ({:?})",
                            fam.sites
                        ));
                    }
                }
                Err(_) => problems.push(format!("`{id}`: family `{f}` is not registered")),
            }
        }
        if let Some(n) = &e.note {
            if n.is_empty() || n.contains('\n') {
                problems.push(format!("`{id}`: the note is one non-empty line"));
            }
        }
        if e.sites.is_empty() {
            problems.push(format!("`{id}`: no sites"));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// Every registered entry, sorted by id.
pub fn registry() -> &'static [Entry] {
    static R: OnceLock<Vec<Entry>> = OnceLock::new();
    R.get_or_init(|| {
        let mut v = toml::from_str::<File>(DATA)
            .expect("profiles/rule-proofs.toml is valid")
            .rule;
        v.sort_by(|a, b| a.id.cmp(&b.id));
        if let Err(why) = validate(&v) {
            panic!("profiles/rule-proofs.toml:\n{why}");
        }
        v
    })
}

/// The entry an id carried by a note, a step or a rule file belongs to: the
/// id registered as is, else the family with the longest prefix that
/// matches it.
pub fn lookup(id: &str) -> Option<&'static Entry> {
    let r = registry();
    if let Ok(i) = r.binary_search_by(|e| e.id.as_str().cmp(id)) {
        return Some(&r[i]);
    }
    r.iter()
        .filter_map(|e| Some((e, e.family_prefix()?)))
        .filter(|(_, p)| id.len() > p.len() && id.starts_with(p))
        .max_by_key(|(_, p)| p.len())
        .map(|(e, _)| e)
}

/// Rules per kind and status: `counts()[kind][status]`.
pub fn counts() -> BTreeMap<Kind, BTreeMap<Status, usize>> {
    let mut m: BTreeMap<Kind, BTreeMap<Status, usize>> = BTreeMap::new();
    for k in Kind::ALL {
        let row = m.entry(k).or_default();
        for s in Status::ALL {
            row.insert(s, 0);
        }
    }
    for e in registry() {
        *m.entry(e.kind).or_default().entry(e.status()).or_default() += 1;
    }
    m
}

/// `docs/proof-coverage.md`.
pub fn markdown() -> String {
    let r = registry();
    let mut s = String::new();
    let _ = writeln!(s, "# Proof coverage");
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "Generated from `crates/sqldiff-core/profiles/rule-proofs.toml`; do not edit. \
         After changing the registry run \
         `SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test rule_proofs`."
    );
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "Every conversion rule id the converter's notes and trace steps can carry \
         (`tsql.*`, `pg.*`, `idiom.*`, `db2.*`) and every rule-file head \
         (`translate`, `idiom`, `lint`), with what backs it \
         (docs/verified-conversion.md, C0.9). The test that regenerates this file \
         also scans the sources and fails when an id is used but not registered, \
         registered but not used, or used in a file its entry does not list."
    );
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "The Lean checker (`formal/lean/Sqldiff/Mirror/Lower.lean`) does not read \
         rule ids: it re-lowers the output on disk, and `Sqldiff.Mirror.check_sound` \
         (or `check_sound_c2a`) is about that lowering. A rule is **lemma**-backed \
         when the construct it rewrites, in the form the converter writes it, is \
         inside the lowering's spec; its citation names the section of `Lower.lean` \
         whose rule reads that form, and the note says where the coverage is \
         partial. **axiom** rests on a vendor fact of \
         docs/vendor-facts-verification.md with no lemma. **engine** is backed only \
         by the Rust engine comparison, the tests and the probes."
    );
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "A lemma backs a rule, not a unit: a unit is confirmed only when the checker \
         accepts its whole routine pair, and the conversion report's `proof` field \
         says so per unit."
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "## Summary");
    let _ = writeln!(s);
    let _ = writeln!(s, "| Kind | Rules | Lemma | Axiom | Engine |");
    let _ = writeln!(s, "|---|---|---|---|---|");
    let counts = counts();
    let mut total = BTreeMap::new();
    for k in Kind::ALL {
        let row = &counts[&k];
        let n: usize = row.values().sum();
        let mut line = format!("| `{}` | {n} |", k.label());
        for st in Status::ALL {
            let c = row[&st];
            *total.entry(st).or_insert(0) += c;
            line.push_str(&format!(" {} |", pct(c, n)));
        }
        let _ = writeln!(s, "{line}");
    }
    let mut line = format!("| **All** | {} |", r.len());
    for st in Status::ALL {
        line.push_str(&format!(" {} |", pct(total[&st], r.len())));
    }
    let _ = writeln!(s, "{line}");
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "A family (`tsql.func.*`) counts as one rule; the members registered on \
         their own count on their own."
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "## Registry");
    let _ = writeln!(s);
    let _ = writeln!(s, "| Id | Kind | Proof | Note | Used in |");
    let _ = writeln!(s, "|---|---|---|---|---|");
    for e in r {
        let proof = match e.proof() {
            Proof::Lemma(c) => format!("lemma: `{}`", cell(&c)),
            Proof::Axiom(n) => format!("axiom: V{n}"),
            Proof::Engine => "engine".to_string(),
        };
        let note = e.note.as_deref().map(cell).unwrap_or_default();
        let sites: Vec<String> = e.sites.iter().map(|x| format!("`{x}`")).collect();
        let _ = writeln!(
            s,
            "| `{}` | `{}` | {proof} | {note} | {} |",
            e.id,
            e.kind.label(),
            sites.join("<br>")
        );
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "## Checker refusal reasons");
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "A lemma-backed rule does not make a unit confirmed: the checker takes the \
         whole routine pair and refuses it when any construct of either side is \
         outside the slice, with one reason of the fixed taxonomy in \
         [formal/lean/README.md](../formal/lean/README.md). The conversion \
         report's per-unit `proof` field carries that reason, and the per-corpus \
         histogram of reasons says which gaps of the slice matter most. The \
         registry above answers the other question: for a rule the converter \
         applied, whether the checker could read its output at all."
    );
    s
}

fn pct(c: usize, n: usize) -> String {
    match (c * 100 + n / 2).checked_div(n) {
        Some(p) => format!("{c} ({p}%)"),
        None => "0".to_string(),
    }
}

/// Markdown table cell text.
fn cell(s: &str) -> String {
    s.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proof_strings_parse() {
        assert_eq!(Proof::parse("engine"), Ok(Proof::Engine));
        assert_eq!(Proof::parse("axiom:V19"), Ok(Proof::Axiom(19)));
        assert_eq!(
            Proof::parse("lemma:Sqldiff.Mirror/C2b strings/LEN"),
            Ok(Proof::Lemma("Sqldiff.Mirror/C2b strings/LEN".into()))
        );
        for bad in [
            "",
            "lemma",
            "lemma:",
            "lemma:/x",
            "lemma:x/",
            "axiom",
            "axiom:19",
            "axiom:V",
            "axiom:V0",
            "axiom:V01",
            "axiom:Vx",
            "Engine",
            "probe",
        ] {
            assert!(Proof::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn lookup_resolves_families_and_members() {
        assert_eq!(lookup("tsql.func.len").unwrap().id, "tsql.func.len");
        assert_eq!(lookup("tsql.func.getdate").unwrap().id, "tsql.func.*");
        assert_eq!(lookup("tsql.rowcount").unwrap().id, "tsql.*");
        assert_eq!(lookup("tsql.ctl.if").unwrap().id, "tsql.ctl.if");
        assert_eq!(lookup("len/length").unwrap().id, "len/length");
        assert!(lookup("pg.not-there").is_none());
        assert!(lookup("tsql.").is_none());
    }

    #[test]
    fn registry_is_well_formed() {
        let r = registry();
        assert!(r.len() > 100);
        for e in r {
            if e.kind.is_converter() {
                assert!(converter_id(&e.id), "{}", e.id);
            } else {
                assert!(!e.id.contains(' ') && !e.id.is_empty(), "{}", e.id);
            }
        }
        // Every lemma cites a section of Lower.lean, every axiom a fact.
        for e in r {
            if let Proof::Lemma(c) = e.proof() {
                assert!(c.starts_with("Sqldiff.Mirror/"), "{}: {c}", e.id);
            }
        }
        let c = counts();
        let n: usize = c.values().map(|m| m.values().sum::<usize>()).sum();
        assert_eq!(n, r.len());
    }

    #[test]
    fn converter_id_shape() {
        assert!(converter_id("tsql.ctl.if"));
        assert!(converter_id("tsql.func.*"));
        assert!(converter_id("tsql.*"));
        assert!(converter_id("tsql.func.string_agg"));
        assert!(converter_id("db2.lob"));
        assert!(!converter_id("len/length"));
        assert!(!converter_id("tsql.ctl.if-"));
        assert!(!converter_id("equiv.unproven"));
        assert!(!converter_id("TSQL.x"));
    }
}
