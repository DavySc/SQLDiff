//! The conversion review ledger: what people (and agents) decided about
//! converted units, kept in a file committed next to the converted tree.
//!
//! A decision names a unit by its [`unit_id`] (a hash of the unit's input
//! and output text) and records the hashes of both whole files it was made
//! on. It applies only while both files are unchanged: a unit whose own text
//! is the same but whose file changed elsewhere (a declaration, a session
//! option, the statement before it) may mean something else now, so the
//! decision is outdated and someone must confirm it again.
//!
//! A decision never changes a unit's [`Status`]. What it can do is discharge
//! a unit's assumptions and decisions, the two statuses that are a person's
//! call; a difference, a refusal or what the engine could not prove is never
//! discharged by a decision, only by changing the output and checking it
//! again. A comment discharges nothing. An agent may record only that it
//! wrote a unit's output, and a comment; a unit an agent wrote stays open
//! until a person approves it.
//!
//! The file is JSON lines, one [`Record`] per line, appended; for a unit,
//! rule and author the last record wins, so concurrent appends merge like
//! any text file. One person's verdict never replaces another's: a current
//! rejection by anyone keeps the unit open, whoever approved it.
//!
//! `by` is whatever the line says, so the guarantees about agents hold only
//! where records are built: a writer (CLI, GUI, MCP) sets `by` itself and
//! never takes it from its caller.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqldiff_core::model::hex;

use crate::{Conversion, Status, Unit};

/// The ledger's file name, at the root of the converted tree.
pub const LEDGER_FILE: &str = "review-decisions.jsonl";

/// Who made a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum By {
    Human,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// The named assumption holds here (`rule` is an assumption of the unit).
    Accepted,
    /// The target the conversion chose is the right one (`rule` is a
    /// decision of the unit).
    Picked,
    /// The unit's output is reviewed and right as written; needed for output
    /// an agent wrote (`rule` is empty).
    Approved,
    /// The unit is wrong and must be fixed; it stays open.
    Rejected,
    /// The unit's output was written by hand, not by the rules (`rule` is
    /// empty). With `by = agent`, a person must approve it.
    Written,
    /// A remark only; discharges nothing.
    Commented,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Accepted => "accepted",
            State::Picked => "picked",
            State::Approved => "approved",
            State::Rejected => "rejected",
            State::Written => "written",
            State::Commented => "commented",
        }
    }
}

/// One line of the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// The unit, by [`unit_id`].
    pub unit: String,
    /// Path of the file in the converted tree, `/`-separated.
    pub path: String,
    /// The assumption or decision this is about; empty for the unit as a whole.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rule: String,
    pub state: State,
    pub by: By,
    /// Who, for people reading the ledger.
    pub author: String,
    /// Why; required for `accepted`, `picked`, `approved` and `rejected`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// When (RFC 3339), as the caller gives it.
    pub at: String,
    /// [`file_hash`] of the whole input file and output file the decision
    /// was made on.
    pub input_hash: String,
    pub output_hash: String,
    /// With `written`: the unit's output as written by hand. `unit` is then
    /// the id of the unit as the rules wrote it, and the text replaces the
    /// rules' output on every later conversion (and is checked like it)
    /// while that unit exists; the hashes are not compared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `base::hash` of the base catalogs the conversion bound against
    /// (`--catalog`), none without any. A decision applies only while the
    /// conversion binds against the same contents: what a base object is
    /// changes what the unit binds to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_catalogs: Option<String>,
}

/// The decisions read from a ledger, last record per (unit, rule, state
/// family, author kind, author) winning.
#[derive(Debug, Clone, Default)]
pub struct Ledger {
    records: BTreeMap<(String, String, Family, By, String), Record>,
    /// Lines that did not parse, or records that are not allowed (an agent
    /// accepting an assumption, a decision without a reason), with why.
    pub rejected_lines: Vec<String>,
}

/// Records of one family replace each other; `written` and `commented` do
/// not replace a verdict on the same unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Family {
    Verdict,
    Written,
    Comment,
}

fn family(s: State) -> Family {
    match s {
        State::Written => Family::Written,
        State::Commented => Family::Comment,
        _ => Family::Verdict,
    }
}

impl Ledger {
    /// Reads a ledger's text. Bad lines are listed, never fatal, and never
    /// applied.
    pub fn parse(text: &str) -> Ledger {
        let mut l = Ledger::default();
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Record>(line) {
                Ok(r) => {
                    if let Err(e) = l.insert(r) {
                        l.rejected_lines.push(format!("line {}: {e}", i + 1));
                    }
                }
                Err(e) => l.rejected_lines.push(format!("line {}: {e}", i + 1)),
            }
        }
        l
    }

    /// Adds a record after checking that it is allowed at all (whether it
    /// fits the unit is decided when it is applied).
    pub fn insert(&mut self, r: Record) -> Result<(), String> {
        allowed(&r)?;
        let key = (
            r.unit.clone(),
            r.rule.clone(),
            family(r.state),
            r.by,
            r.author.trim().to_string(),
        );
        self.records.insert(key, r);
        Ok(())
    }

    pub fn records(&self) -> impl Iterator<Item = &Record> {
        self.records.values()
    }

    /// The records of each file, borrowed, in [`Ledger::records`] order:
    /// read once for a whole tree, so applying the ledger costs its records
    /// once and not once per unit.
    pub fn by_path(&self) -> HashMap<&str, Vec<&Record>> {
        let mut m: HashMap<&str, Vec<&Record>> = HashMap::new();
        for r in self.records() {
            m.entry(r.path.as_str()).or_default().push(r);
        }
        m
    }
}

/// What may be recorded at all, whatever the unit says.
pub fn allowed(r: &Record) -> Result<(), String> {
    if r.unit.is_empty() || r.path.is_empty() {
        return Err("a record needs a unit and a path".into());
    }
    if r.author.trim().is_empty() {
        return Err("a record needs an author".into());
    }
    if r.by == By::Agent && !matches!(r.state, State::Written | State::Commented) {
        return Err(format!(
            "an agent may record only `written` and `commented`, not `{}`",
            r.state.label()
        ));
    }
    match r.state {
        State::Accepted | State::Picked if r.rule.is_empty() => {
            return Err(format!(
                "`{}` names the assumption or decision it is about",
                r.state.label()
            ))
        }
        _ if r.text.is_some() && r.state != State::Written => {
            return Err(format!(
                "only `written` carries text, not `{}`",
                r.state.label()
            ))
        }
        State::Approved | State::Written if !r.rule.is_empty() => {
            return Err(format!(
                "`{}` is about the whole unit and names no rule",
                r.state.label()
            ))
        }
        _ => {}
    }
    if matches!(
        r.state,
        State::Accepted | State::Picked | State::Approved | State::Rejected
    ) && r.reason.trim().is_empty()
    {
        return Err(format!("`{}` needs a reason", r.state.label()));
    }
    Ok(())
}

/// The hash a record keeps of a whole file: SHA-256 of its exact bytes, so
/// any change at all (trailing blanks inside a string literal included)
/// outdates the decisions made on it. The one exception is the line ending:
/// CRLF is hashed as LF, because git rewrites line endings on checkout
/// (`core.autocrlf`), and the same commit must keep its decisions on Windows
/// and elsewhere. Whatever a CR inside a string literal means is still
/// checked: every run converts and checks the file again, and a decision
/// only discharges assumptions and decisions.
pub fn file_hash(text: &str) -> String {
    hex(&Sha256::digest(lf(text).as_bytes()))
}

/// `text` with every CRLF as LF (a lone CR is kept).
fn lf(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains("\r\n") {
        text.replace("\r\n", "\n").into()
    } else {
        text.into()
    }
}

/// A unit's id: its file, its input text and its output text. Units with the
/// same file and texts are told apart by their order (`~2`, `~3`, ...).
pub fn unit_ids(path: &str, input: &str, c: &Conversion) -> Vec<String> {
    ids_of(
        path,
        c.units.iter().map(|u| {
            (
                input.get(u.src.start..u.src.end).unwrap_or(""),
                c.output.get(u.out.start..u.out.end).unwrap_or(""),
            )
        }),
    )
}

/// [`unit_ids`] from each unit's input and output text, with CRLF hashed as
/// LF (see [`file_hash`]).
pub fn ids_of<'a>(path: &str, units: impl Iterator<Item = (&'a str, &'a str)>) -> Vec<String> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    units
        .map(|(src, out)| {
            let mut h = Sha256::new();
            h.update(path.as_bytes());
            h.update([0u8]);
            h.update(lf(src).as_bytes());
            h.update([0u8]);
            h.update(lf(out).as_bytes());
            let base = format!("u{}", hex(&h.finalize()[..8]));
            let n = seen.entry(base.clone()).or_insert(0);
            *n += 1;
            if *n == 1 {
                base
            } else {
                format!("{base}~{n}")
            }
        })
        .collect()
}

/// How a decision relates to the current conversion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    pub record: Record,
    /// Applies now: same unit, same files, and it fits the unit.
    pub current: bool,
    /// Why it does not apply, when it does not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why_not: Option<String>,
}

/// A unit and what the ledger says about it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitReview {
    pub id: String,
    /// The unit's status, unchanged by any decision.
    pub status: Status,
    /// Assumptions and decisions not discharged by a current decision.
    pub open: Vec<String>,
    /// Notes no decision can discharge (differs, not converted, not proven),
    /// and the engine's own part when it is not converted.
    pub undischargeable: Vec<String>,
    /// A current `written` record: who wrote the output.
    pub written_by: Option<By>,
    /// Reviewed: nothing undischargeable, nothing open, not rejected, and
    /// output an agent wrote approved by a person. Never the same as
    /// `converted`: show it as "accepted in review".
    pub reviewed: bool,
    /// Every record for this unit, current or not.
    pub decisions: Vec<Applied>,
}

/// The review of every unit of one converted file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileReview {
    pub path: String,
    pub units: Vec<UnitReview>,
    /// Gating findings of the output check that point into no unit (their
    /// rules). While there is any, the file is not reviewed, whatever its
    /// units say.
    pub unplaced: Vec<String>,
    /// What the engine did not compare, outside any unit: the same.
    pub unproven: Vec<String>,
    /// Records for this file whose unit no longer exists (its text changed).
    pub orphaned: Vec<Record>,
}

/// Applies the ledger to one converted file: `path` is the file's path in
/// the tree, `input` the text that was converted and `c` its conversion as
/// checked.
pub fn review_file(path: &str, input: &str, c: &Conversion, ledger: &Ledger) -> FileReview {
    review_file_in(path, input, c, ledger, None)
}

/// [`review_file`] for a conversion bound against base catalogs with
/// `base::hash` `base` (`None` without any): a decision recorded with other
/// base catalogs, or none, does not apply.
pub fn review_file_in(
    path: &str,
    input: &str,
    c: &Conversion,
    ledger: &Ledger,
    base: Option<&str>,
) -> FileReview {
    let records: Vec<&Record> = ledger.records().filter(|r| r.path == path).collect();
    review_records(path, input, c, &records, base)
}

/// [`review_file_in`] from the file's own records (`Ledger::by_path`), in
/// ledger order.
pub fn review_records(
    path: &str,
    input: &str,
    c: &Conversion,
    records: &[&Record],
    base: Option<&str>,
) -> FileReview {
    let ids = unit_ids(path, input, c);
    let (ih, oh) = (file_hash(input), file_hash(&c.output));
    // By unit, in ledger order: decisions, and hand-written texts.
    let mut decided: HashMap<&str, Vec<&Record>> = HashMap::new();
    let mut texts: HashMap<&str, Vec<&Record>> = HashMap::new();
    for &r in records {
        match r.text {
            None => decided.entry(r.unit.as_str()).or_default().push(r),
            Some(_) => texts.entry(r.unit.as_str()).or_default().push(r),
        }
    }
    let mut units = Vec::new();
    for (u, id) in c.units.iter().zip(&ids) {
        let mut decisions: Vec<Applied> = decided
            .get(id.as_str())
            .into_iter()
            .flatten()
            .copied()
            .map(|r| {
                let why_not = if r.input_hash != ih || r.output_hash != oh {
                    Some(FILE_CHANGED.to_string())
                } else if r.base_catalogs.as_deref() != base {
                    Some(CATALOGS_CHANGED.to_string())
                } else {
                    fits(u, r).err()
                };
                Applied {
                    record: r.clone(),
                    current: why_not.is_none(),
                    why_not,
                }
            })
            .collect();
        // Texts key to the rules' unit. The one the conversion put in place
        // is current whatever the file's hashes (it changed the file); one
        // a person's text replaced is left out; one that was not put in
        // place (the unit's notes say why) never applies.
        let key = u
            .written
            .as_ref()
            .map_or(id.as_str(), |w| w.origin.as_str());
        decisions.extend(
            texts
                .get(key)
                .into_iter()
                .flatten()
                .copied()
                .filter_map(|r| match &u.written {
                    Some(w) if r.unit == w.origin => {
                        (r.by == w.by && r.author == w.author).then(|| Applied {
                            record: r.clone(),
                            current: true,
                            why_not: None,
                        })
                    }
                    None if &r.unit == id => Some(Applied {
                        record: r.clone(),
                        current: false,
                        why_not: Some("the text was not put in place: see the unit's notes".into()),
                    }),
                    _ => None,
                }),
        );
        decisions.sort_by(|a, b| a.record.at.cmp(&b.record.at));
        units.push(judge(id, u, decisions));
    }
    let known: HashSet<&str> = ids
        .iter()
        .map(String::as_str)
        .chain(
            c.units
                .iter()
                .filter_map(|u| u.written.as_ref().map(|w| w.origin.as_str())),
        )
        .collect();
    let orphaned = records
        .iter()
        .filter(|r| !known.contains(r.unit.as_str()))
        .map(|&r| r.clone())
        .collect();
    let unplaced = c
        .unplaced
        .iter()
        .filter_map(|&i| c.output_check.findings.get(i))
        .map(|f| f.rule.clone())
        .collect();
    FileReview {
        path: path.to_string(),
        units,
        unplaced,
        unproven: c.unproven.clone(),
        orphaned,
    }
}

/// Whether a record fits the unit as it is.
fn fits(u: &Unit, r: &Record) -> Result<(), String> {
    let want = match r.state {
        State::Accepted => Status::Assumptions,
        State::Picked => Status::Decision,
        _ => return Ok(()),
    };
    match u.notes.iter().find(|n| n.rule == r.rule) {
        Some(n) if n.status == want => Ok(()),
        Some(n) => Err(format!(
            "`{}` is {} here; only {} can be {}",
            r.rule,
            n.status.label(),
            want.label(),
            r.state.label()
        )),
        None => Err(format!("the unit no longer has `{}`", r.rule)),
    }
}

/// What no review decision can discharge on a unit: the rules of its notes
/// that are not proven, differ or are not converted, else the engine's part
/// or the status when either is raised.
pub fn undischargeable(u: &Unit) -> Vec<String> {
    let mut v: Vec<String> = u
        .notes
        .iter()
        .filter(|n| match n.status {
            Status::Unproven | Status::Differs | Status::NotConverted => true,
            Status::Converted | Status::Assumptions | Status::Decision => false,
        })
        .map(|n| n.rule.to_string())
        .collect();
    // The status and the engine's part as backstops: a raised status is never
    // discharged, whatever notes it came with.
    if u.checked != Status::Converted && v.is_empty() {
        v.push(format!("engine: {}", u.checked.label()));
    }
    if u.status >= Status::Unproven && v.is_empty() {
        v.push(format!("status: {}", u.status.label()));
    }
    v
}

fn judge(id: &str, u: &Unit, decisions: Vec<Applied>) -> UnitReview {
    let cur = |s: State, rule: &str| {
        decisions
            .iter()
            .any(|a| a.current && a.record.state == s && a.record.rule == rule)
    };
    let mut open = Vec::new();
    for n in &u.notes {
        match n.status {
            Status::Assumptions if cur(State::Accepted, &n.rule) => {}
            Status::Decision if cur(State::Picked, &n.rule) => {}
            Status::Assumptions | Status::Decision => open.push(n.rule.to_string()),
            Status::Converted | Status::Unproven | Status::Differs | Status::NotConverted => {}
        }
    }
    let undischargeable = undischargeable(u);
    // Hand-written output (applied by the conversion), or a record that the
    // output as it stands was written by hand.
    let written_by = decisions
        .iter()
        .filter(|a| a.current && a.record.state == State::Written)
        .map(|a| a.record.by)
        .chain(u.written.as_ref().map(|w| w.by))
        .max();
    let rejected = decisions
        .iter()
        .any(|a| a.current && a.record.state == State::Rejected);
    let approved = decisions
        .iter()
        .any(|a| a.current && a.record.state == State::Approved && a.record.by == By::Human);
    let reviewed = undischargeable.is_empty()
        && open.is_empty()
        && !rejected
        && (written_by != Some(By::Agent) || approved)
        // A plainly converted unit nobody looked at is not reviewed.
        && (u.status != Status::Converted || approved || written_by.is_some());
    UnitReview {
        id: id.to_string(),
        status: u.status,
        open,
        undischargeable,
        written_by,
        reviewed,
        decisions,
    }
}

impl FileReview {
    /// Every unit reviewed and nothing unplaced. A file without units has
    /// nothing anyone accepted, so it is never reviewed.
    pub fn reviewed(&self) -> bool {
        !self.units.is_empty()
            && self.unplaced.is_empty()
            && self.unproven.is_empty()
            && self.units.iter().all(|u| u.reviewed)
    }
}

impl Ledger {
    /// Hand-written output for a file: the unit id as the rules wrote it,
    /// and the record. A person's text replaces an agent's draft; two
    /// different texts at the same level (two people, or agents only) are a
    /// conflict and listed separately: neither applies.
    pub fn texts(&self, path: &str) -> (BTreeMap<String, &Record>, Vec<String>) {
        let mut by_unit: BTreeMap<String, Vec<&Record>> = BTreeMap::new();
        for r in self.records() {
            if r.path == path && r.state == State::Written && r.text.is_some() {
                by_unit.entry(r.unit.clone()).or_default().push(r);
            }
        }
        let (mut texts, mut conflicts) = (BTreeMap::new(), Vec::new());
        for (unit, mut rs) in by_unit {
            if rs.iter().any(|r| r.by == By::Human) {
                rs.retain(|r| r.by == By::Human);
            }
            if rs.iter().all(|r| r.text == rs[0].text) {
                texts.insert(unit, rs[0]);
            } else {
                conflicts.push(unit);
            }
        }
        (texts, conflicts)
    }
}

/// `Applied::why_not` of a decision made on other bytes of the file.
pub const FILE_CHANGED: &str = "the file changed since: confirm it again";

/// `Applied::why_not` of a decision made with other base catalogs (or none).
pub const CATALOGS_CHANGED: &str =
    "the base catalogs changed since (other files or contents, or none then): confirm it again";

/// Appends a record to the ledger in `out` after checking it is allowed.
/// The caller sets `by` from who it is, never from its own caller.
/// The ledger in `out` for appending; refused when it is a symbolic link,
/// which would carry the decisions wherever it points.
pub fn ledger_file(out: &std::path::Path) -> Result<std::path::PathBuf, String> {
    crate::tree::dest_under(out, LEDGER_FILE)
}

pub fn append(out: &std::path::Path, r: &Record) -> Result<(), String> {
    use std::io::Write;
    allowed(r)?;
    let path = ledger_file(out)?;
    let line = serde_json::to_string(r).map_err(|e| e.to_string())?;
    // A file that does not end in a newline gets one first, so the record
    // starts its own line.
    let needs_nl = std::fs::read(&path)
        .ok()
        .is_some_and(|b| !b.is_empty() && !b.ends_with(b"\n"));
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let text = format!("{}{line}\n", if needs_nl { "\n" } else { "" });
    f.write_all(text.as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))
}
