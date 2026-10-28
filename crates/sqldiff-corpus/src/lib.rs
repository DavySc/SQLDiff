//! Corpus golden runner (design §12, §14 gates).
//!
//! Loads every case under `corpus/pairs` and `corpus/mutations`, runs each
//! `[[expect]]` through the engine and compares the result with the expected
//! verdict, classes and assumptions. Expectations the engine cannot meet yet
//! are listed in `known-failing.toml`; the gate fails when an unlisted one
//! fails (regression) and when a listed one passes (the list must shrink).

pub mod fuzz;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sqldiff_core::{diff, Artifact, ChangeClass, Dialect, Options, Report, Verdict};

/// `corpus/` at the root of the repository.
pub fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus")
}

/// `known-failing.toml` next to this crate's manifest.
pub fn known_failing_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("known-failing.toml")
}

#[derive(Debug, Clone, Deserialize)]
pub struct Side {
    pub dialect: String,
    pub file: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Expect {
    pub a: String,
    pub b: String,
    pub verdict: String,
    #[serde(default)]
    pub classes: Vec<String>,
    #[serde(default)]
    pub assumptions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    pub id: String,
    pub sides: BTreeMap<String, Side>,
    #[serde(default)]
    pub expect: Vec<Expect>,
    #[serde(skip)]
    pub dir: PathBuf,
}

impl Case {
    /// Stable id of one expectation: `<case>:<a>~<b>`.
    pub fn expect_id(&self, e: &Expect) -> String {
        format!("{}:{}~{}", self.id, e.a, e.b)
    }
}

/// Every case under `pairs/` and `mutations/`, sorted by id.
pub fn load_cases(root: &Path) -> Result<Vec<Case>, String> {
    let mut cases = Vec::new();
    for group in ["pairs", "mutations"] {
        let dir = root.join(group);
        let entries = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries {
            let path = entry.map_err(|e| e.to_string())?.path();
            let file = path.join("case.toml");
            if !file.is_file() {
                continue;
            }
            let text =
                std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            let mut case: Case =
                toml::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?;
            case.dir = path;
            cases.push(case);
        }
    }
    cases.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(cases)
}

pub fn verdict_key(v: Verdict) -> &'static str {
    match v {
        Verdict::Equal => "equal",
        Verdict::EqualUnderAssumptions => "equal-under-assumptions",
        Verdict::Unknown => "unknown",
        Verdict::Different => "different",
    }
}

pub fn class_key(c: ChangeClass) -> &'static str {
    c.label()
}

/// Result of one expectation.
#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub id: String,
    pub case: String,
    pub expected: String,
    pub actual: String,
    /// Classes of the findings whose verdict is "different".
    pub different_classes: Vec<String>,
    pub assumptions: usize,
    /// Empty when the expectation is met.
    pub problems: Vec<String>,
}

impl Outcome {
    pub fn passed(&self) -> bool {
        self.problems.is_empty()
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:<48} expected {:<24} got {:<24}",
            self.id, self.expected, self.actual
        )?;
        if !self.problems.is_empty() {
            write!(f, " {}", self.problems.join("; "))?;
        }
        Ok(())
    }
}

fn load_side(case: &Case, name: &str) -> Result<Artifact, String> {
    let side = case
        .sides
        .get(name)
        .ok_or_else(|| format!("{}: no side {name:?}", case.id))?;
    let dialect: Dialect = side
        .dialect
        .parse()
        .map_err(|_| format!("{}: unknown dialect {:?}", case.id, side.dialect))?;
    let path = case.dir.join(&side.file);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Artifact::new(
        format!("{}/{}", case.id, side.file),
        dialect,
        text,
    ))
}

/// Run the engine on one expectation.
pub fn run_expect(case: &Case, e: &Expect) -> Result<(Outcome, Report), String> {
    let a = load_side(case, &e.a)?;
    let b = load_side(case, &e.b)?;
    let report = diff(&a, &b, &Options::default());
    Ok((judge(case, e, &report), report))
}

/// Compare a report with an expectation.
///
/// - The verdict must match exactly.
/// - For "different", every expected class must have a finding of that class
///   whose own verdict is "different": the case is caught for the right reason,
///   not by an unrelated edit.
/// - For "equal-under-assumptions", the report must carry at least as many
///   assumptions as the case lists. Wording is not compared (corpus README).
/// - For "equal", classes are noise and are not checked.
pub fn judge(case: &Case, e: &Expect, report: &Report) -> Outcome {
    let actual = verdict_key(report.verdict());
    let different_classes: BTreeSet<&str> = report
        .findings
        .iter()
        .filter(|f| f.verdict == Verdict::Different)
        .map(|f| class_key(f.class))
        .collect();
    let mut problems = Vec::new();
    if actual != e.verdict {
        problems.push(format!("verdict {actual} != {}", e.verdict));
    } else {
        match e.verdict.as_str() {
            "different" => {
                for c in &e.classes {
                    if !different_classes.contains(c.as_str()) {
                        problems.push(format!("no {c} finding with verdict different"));
                    }
                }
            }
            "equal-under-assumptions" if report.assumptions.len() < e.assumptions.len() => {
                problems.push(format!(
                    "{} assumption(s), expected at least {}",
                    report.assumptions.len(),
                    e.assumptions.len()
                ));
            }
            _ => {}
        }
    }
    Outcome {
        id: case.expect_id(e),
        case: case.id.clone(),
        expected: e.verdict.clone(),
        actual: actual.to_string(),
        different_classes: different_classes.iter().map(|s| s.to_string()).collect(),
        assumptions: report.assumptions.len(),
        problems,
    }
}

/// Run every expectation of every case.
pub fn run_all(root: &Path) -> Result<Vec<Outcome>, String> {
    let mut out = Vec::new();
    for case in load_cases(root)? {
        for e in &case.expect {
            out.push(run_expect(&case, e)?.0);
        }
    }
    Ok(out)
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct KnownFailing {
    #[serde(default)]
    pub failing: Vec<KnownEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct KnownEntry {
    /// Expectation id, `<case>:<a>~<b>`.
    pub id: String,
    /// What is missing; usually the work package that will fix it.
    #[serde(default)]
    pub reason: String,
}

pub fn load_known_failing(path: &Path) -> Result<KnownFailing, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// What the ratchet found wrong.
#[derive(Debug, Default)]
pub struct Gate {
    /// Failing and not listed: a regression, or a new case that needs listing.
    pub regressions: Vec<String>,
    /// Listed but now passing: remove from the list.
    pub fixed: Vec<String>,
    /// Listed ids that match no expectation.
    pub stale: Vec<String>,
    /// Listed more than once.
    pub duplicates: Vec<String>,
}

impl Gate {
    pub fn ok(&self) -> bool {
        self.regressions.is_empty()
            && self.fixed.is_empty()
            && self.stale.is_empty()
            && self.duplicates.is_empty()
    }
}

pub fn gate(outcomes: &[Outcome], known: &KnownFailing) -> Gate {
    let mut listed = BTreeSet::new();
    let mut g = Gate::default();
    for k in &known.failing {
        if !listed.insert(k.id.as_str()) {
            g.duplicates.push(k.id.clone());
        }
    }
    let ids: BTreeSet<&str> = outcomes.iter().map(|o| o.id.as_str()).collect();
    for o in outcomes {
        match (o.passed(), listed.contains(o.id.as_str())) {
            (false, false) => g.regressions.push(o.to_string()),
            (true, true) => g.fixed.push(o.id.clone()),
            _ => {}
        }
    }
    g.stale = listed
        .iter()
        .filter(|id| !ids.contains(*id))
        .map(|s| s.to_string())
        .collect();
    g
}

/// Render a known-failing list for the current outcomes, keeping existing reasons.
pub fn render_known_failing(outcomes: &[Outcome], previous: &KnownFailing) -> String {
    let reasons: BTreeMap<&str, &str> = previous
        .failing
        .iter()
        .map(|k| (k.id.as_str(), k.reason.as_str()))
        .collect();
    let mut s = String::from(KNOWN_FAILING_HEADER);
    for o in outcomes.iter().filter(|o| !o.passed()) {
        let reason = reasons
            .get(o.id.as_str())
            .map(|r| r.to_string())
            .unwrap_or_else(|| o.problems.join("; "));
        s.push_str(&format!(
            "\n[[failing]]\nid = {}\nreason = {}\n",
            toml_str(&o.id),
            toml_str(&reason)
        ));
    }
    s
}

fn toml_str(s: &str) -> String {
    serde_json::to_string(s).expect("string serializes")
}

const KNOWN_FAILING_HEADER: &str = "\
# Corpus expectations the engine does not meet yet (design §12, §14 gates).
# `cargo test -p sqldiff-corpus` fails when an expectation not listed here fails,
# and when one listed here passes: remove it in the same change that fixes it.
# Regenerate with `cargo run -p sqldiff-corpus --bin corpus-report -- --write-known-failing`,
# then review the diff: only removals should appear unless you added cases.
";
