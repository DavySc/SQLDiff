//! Review queue, assumption ledger and deep links over a source (design
//! sections 6.1 and 9, and section 11 Tier 0 prioritisation).
//!
//! [`run`] pairs a source and diffs every pair with pack policy applied.
//! The raw reports are kept; session-local assumption flips are applied on
//! top with [`view`], so a flip re-derives verdicts without re-reading the
//! source. [`entries`] ranks pairs by review state, verdict, severity,
//! whether an assumption flips the verdict, recency and size.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqldiff_core::model::{hex, AssumptionSource};
use sqldiff_core::session;
use sqldiff_core::{Artifact, ChangeClass, Options, Report, Severity, Verdict};

use crate::protocol::{Describe, ObjectFilter, Pair, PairScope};
use crate::{pairing, policy, Resolved, Result, Source};

/// One diffed pair, before session flips.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairReport {
    /// Short stable id for the pair: a hash of both object ids.
    pub id: String,
    pub pair: Pair,
    pub report: Report,
    pub stale: bool,
    /// Latest `last_changed` of the two objects, if the source reports it.
    pub last_changed: Option<String>,
}

/// Every pair of a source, diffed.
#[derive(Debug, Clone)]
pub struct Run {
    pub source: Describe,
    pub pairs: Vec<PairReport>,
    /// Objects that were not paired (unpaired, ambiguous, shared, ...).
    pub pairing: pairing::Pairing,
}

/// Stable short id for a pair of object ids (or one id for a single artifact).
pub fn pair_id(a: &str, b: Option<&str>) -> String {
    let mut h = Sha256::new();
    h.update(a.as_bytes());
    h.update([0u8]);
    h.update(b.unwrap_or("").as_bytes());
    format!("p{}", hex(&h.finalize()[..6]))
}

/// Pairs a source by pack rules (or the adapter's own pairs) and diffs each pair.
pub fn run(src: &dyn Source, cfg: &Resolved, scope: &PairScope) -> Result<Run> {
    run_with(src, cfg, scope, &mut |_, _, _| {})
}

/// [`run`], with `post` applied to each report after pack policy (author
/// markers and review suppression live in `sqldiff_review`).
pub fn run_with(
    src: &dyn Source,
    cfg: &Resolved,
    scope: &PairScope,
    post: &mut dyn FnMut(&mut Report, &Artifact, &Artifact),
) -> Result<Run> {
    let objects = src.list_objects(&ObjectFilter::default())?;
    let mut p = pairing::pair(&objects, cfg, scope)?;
    // An adapter that pairs by itself wins over pack rules.
    let adapter_pairs = src.get_pairs(scope)?;
    if !adapter_pairs.is_empty() {
        p.pairs = adapter_pairs;
    }
    let info = |id: &str| objects.iter().find(|o| o.id == id);
    let opts = Options {
        excerpt_lines: cfg.excerpt_lines,
        ..Options::default()
    };
    let mut pairs = Vec::new();
    for x in &p.pairs {
        let a = src.load_artifact(&x.a)?;
        let b = src.load_artifact(&x.b)?;
        let mut r = sqldiff_core::diff(&a, &b, &opts);
        crate::compose::apply_source(&mut r, src, (&x.a, &a), (&x.b, &b), &cfg.policy, &opts);
        policy::add_limits(&mut r, &a, Some(&b), cfg);
        policy::apply(&mut r, cfg);
        post(&mut r, &a, &b);
        let (ia, ib) = (info(&x.a), info(&x.b));
        let stale = ia.is_some_and(|o| o.stale) || ib.is_some_and(|o| o.stale);
        policy::cap_stale(
            &mut r,
            ia.is_some_and(|o| o.stale),
            ib.is_some_and(|o| o.stale),
        );
        let last_changed = [ia, ib]
            .into_iter()
            .flatten()
            .filter_map(|o| o.last_changed.clone())
            .max();
        pairs.push(PairReport {
            id: pair_id(&x.a, Some(&x.b)),
            pair: x.clone(),
            report: r,
            stale,
            last_changed,
        });
    }
    Ok(Run {
        source: src.describe()?,
        pairs,
        pairing: p,
    })
}

/// Session-local assumption flips: id → holds. `true` confirms the
/// assumption, `false` denies it.
pub type Flips = BTreeMap<String, bool>;

/// A report with session flips applied.
pub fn view(raw: &Report, flips: &Flips) -> Report {
    let mut r = raw.clone();
    apply(&mut r, flips);
    r
}

/// Applies session flips to a report in place. Without flips the report is
/// left exactly as computed.
pub fn apply(r: &mut Report, flips: &Flips) {
    let (deny, confirm): (Vec<_>, Vec<_>) = flips.iter().partition(|(_, holds)| !**holds);
    let ids = |v: Vec<(&String, &bool)>| v.into_iter().map(|(k, _)| k.clone()).collect::<Vec<_>>();
    session::confirm_assumptions(r, &ids(confirm));
    session::deny_assumptions(r, &ids(deny));
}

/// The denied ids among the flips.
pub fn denied(flips: &Flips) -> Vec<String> {
    flips
        .iter()
        .filter(|(_, holds)| !**holds)
        .map(|(k, _)| k.clone())
        .collect()
}

/// Where an assumption's status comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// A tool default no source states; the reviewer should check it.
    Open,
    /// Declared in a pack, project config or on the command line.
    Declared,
    /// Proven from composed output (a wrapper part states it); discharged.
    Proven,
    /// Confirmed in this session.
    Confirmed,
    /// Denied in this session.
    Denied,
}

/// One row of the assumption ledger for a pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub id: String,
    pub text: String,
    pub status: Status,
    /// Findings that rely on it.
    pub findings: Vec<String>,
    /// The pair's verdict if this assumption alone flipped from its current state.
    pub verdict_if_flipped: Verdict,
    /// Whether flipping it changes the pair's verdict (ranks it higher).
    pub flips_verdict: bool,
}

/// The assumption ledger of a pair under the current flips, verdict-changing
/// assumptions first.
pub fn ledger(raw: &Report, flips: &Flips) -> Vec<LedgerEntry> {
    let current = view(raw, flips);
    let mut out: Vec<LedgerEntry> = raw
        .assumptions
        .iter()
        .map(|a| {
            let status = match (flips.get(&a.id), a.source) {
                (Some(true), _) => Status::Confirmed,
                (Some(false), _) => Status::Denied,
                (None, AssumptionSource::Declared) => Status::Declared,
                (None, AssumptionSource::ComposedOutput) => Status::Proven,
                (None, AssumptionSource::Default | AssumptionSource::Author) => Status::Open,
            };
            let mut other = flips.clone();
            // Flipping a denied one restores it; anything else is denied.
            other.insert(a.id.clone(), status == Status::Denied);
            let verdict_if_flipped = view(raw, &other).open_verdict();
            LedgerEntry {
                id: a.id.clone(),
                text: a.text.clone(),
                status,
                findings: raw
                    .findings
                    .iter()
                    .filter(|f| f.assumptions.contains(&a.id))
                    .map(|f| f.fingerprint.clone())
                    .collect(),
                verdict_if_flipped,
                flips_verdict: verdict_if_flipped != current.open_verdict(),
            }
        })
        .collect();
    out.sort_by_key(|e| (!e.flips_verdict, e.status != Status::Open, e.id.clone()));
    out
}

/// A reviewer's decision on one finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Accepted,
    Rejected,
    Commented,
}

/// Read access to the review store (implemented by `sqldiff_review`).
/// [`NoReviews`] reports every finding as unreviewed.
pub trait ReviewLookup {
    /// The live decision on a finding, if any. `snapshot` identifies the
    /// content reviewed: see [`snapshot`].
    fn state(&self, fingerprint: &str, snapshot: &str) -> Option<ReviewState>;
}

pub struct NoReviews;

impl ReviewLookup for NoReviews {
    fn state(&self, _: &str, _: &str) -> Option<ReviewState> {
        None
    }
}

/// The snapshot key of a report: both sides' content hashes.
pub fn snapshot(r: &Report) -> String {
    match &r.b {
        Some(b) => format!("{}:{}", r.a.content_hash, b.content_hash),
        None => r.a.content_hash.clone(),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewCounts {
    /// Accepted in the review store or marked as deliberate in source;
    /// these findings are suppressed.
    pub accepted: usize,
    pub rejected: usize,
    pub commented: usize,
    pub unreviewed: usize,
}

/// One row of the queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub a: String,
    pub b: String,
    pub dialect_a: String,
    pub dialect_b: String,
    /// The verdict over open (not suppressed) findings.
    pub verdict: Verdict,
    /// Unreviewed findings, or a verdict short of equal with no finding to
    /// decide on (an unknown reason, a parse problem, a denied assumption).
    pub needs_review: bool,
    pub highest_class: Option<ChangeClass>,
    pub highest_severity: Option<Severity>,
    pub findings: usize,
    /// Assumptions still open (tool defaults nobody declared or flipped).
    pub open_assumptions: Vec<String>,
    /// Assumptions whose flip alone changes the verdict.
    pub verdict_flipping: Vec<String>,
    pub review: ReviewCounts,
    pub last_changed: Option<String>,
    pub stale: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewFilter {
    /// Pairs that need review: see [`Entry::needs_review`].
    Unreviewed,
    /// Pairs whose findings are all decided and whose verdict rests on them.
    Reviewed,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Filter {
    /// Keep pairs with a finding of one of these classes.
    pub classes: Vec<ChangeClass>,
    /// Keep pairs with a finding at least this severe.
    pub min_severity: Option<Severity>,
    pub verdicts: Vec<Verdict>,
    /// Keep pairs where either object id starts with this.
    pub folder: Option<String>,
    pub review: Option<ReviewFilter>,
}

/// Queue rows for pairs, filtered and ranked: unreviewed first, then
/// verdict, severity, verdict-flipping assumptions, recency and size.
pub fn entries(
    pairs: &[PairReport],
    flips: &Flips,
    filter: &Filter,
    reviews: &dyn ReviewLookup,
) -> Vec<Entry> {
    let mut out: Vec<Entry> = pairs
        .iter()
        .filter_map(|p| {
            let r = view(&p.report, flips);
            let snap = snapshot(&r);
            let verdict = r.open_verdict();
            let mut review = ReviewCounts {
                accepted: r.suppressed.len(),
                ..ReviewCounts::default()
            };
            for f in &r.findings {
                match reviews.state(&f.fingerprint, &snap) {
                    Some(ReviewState::Accepted) => review.accepted += 1,
                    Some(ReviewState::Rejected) => review.rejected += 1,
                    Some(ReviewState::Commented) => review.commented += 1,
                    None => review.unreviewed += 1,
                }
            }
            let needs_review = review.unreviewed > 0
                || (r.findings.is_empty()
                    && !matches!(verdict, Verdict::Equal | Verdict::EqualUnderAssumptions));
            let keep = (filter.classes.is_empty()
                || r.findings.iter().any(|f| filter.classes.contains(&f.class)))
                && filter
                    .min_severity
                    .map_or(true, |s| r.findings.iter().any(|f| f.severity >= s))
                && (filter.verdicts.is_empty() || filter.verdicts.contains(&verdict))
                && filter
                    .folder
                    .as_deref()
                    .map_or(true, |d| p.pair.a.starts_with(d) || p.pair.b.starts_with(d))
                && match filter.review {
                    None => true,
                    Some(ReviewFilter::Unreviewed) => needs_review,
                    Some(ReviewFilter::Reviewed) => !needs_review,
                };
            if !keep {
                return None;
            }
            let led = ledger(&p.report, flips);
            Some(Entry {
                id: p.id.clone(),
                a: p.pair.a.clone(),
                b: p.pair.b.clone(),
                dialect_a: r.a.dialect.clone(),
                dialect_b: r.b.as_ref().map(|b| b.dialect.clone()).unwrap_or_default(),
                verdict,
                needs_review,
                highest_class: r.highest_class(),
                highest_severity: r.findings.iter().map(|f| f.severity).max(),
                findings: r.findings.len(),
                open_assumptions: led
                    .iter()
                    .filter(|e| e.status == Status::Open)
                    .map(|e| e.id.clone())
                    .collect(),
                verdict_flipping: led
                    .iter()
                    .filter(|e| e.flips_verdict)
                    .map(|e| e.id.clone())
                    .collect(),
                review,
                last_changed: p.last_changed.clone(),
                stale: p.stale,
            })
        })
        .collect();
    out.sort_by(|x, y| {
        let key = |e: &Entry| {
            (
                e.needs_review,
                e.verdict,
                e.highest_severity,
                !e.verdict_flipping.is_empty(),
                e.last_changed.clone(),
                e.findings,
            )
        };
        key(y).cmp(&key(x)).then(x.id.cmp(&y.id))
    });
    out
}

/// Default address of `sqldiff-gui serve`.
pub const DEFAULT_GUI_URL: &str = "http://127.0.0.1:7878";

/// A deep link that opens a pair in the GUI with one finding selected:
/// `{base}/?a=<path>&b=<path>#f=<fingerprint>`.
pub fn deep_link(base: &str, a: &str, b: Option<&str>, fingerprint: Option<&str>) -> String {
    let mut s = format!("{}/?a={}", base.trim_end_matches('/'), encode(a));
    if let Some(b) = b {
        s.push_str(&format!("&b={}", encode(b)));
    }
    if let Some(fp) = fingerprint {
        s.push_str(&format!("#f={}", encode(fp)));
    }
    s
}

/// Quotes a word for a POSIX shell when it needs it.
pub fn shell_word(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+:@=,".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Percent-encodes everything but unreserved characters and `/`.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_links_encode_paths() {
        assert_eq!(
            deep_link(
                "http://127.0.0.1:7878/",
                "tsql/move stock.sql",
                Some("db2i/x#1.sql"),
                Some("ab12")
            ),
            "http://127.0.0.1:7878/?a=tsql/move%20stock.sql&b=db2i/x%231.sql#f=ab12"
        );
    }

    #[test]
    fn pair_ids_are_stable_and_distinct() {
        assert_eq!(pair_id("a", Some("b")), pair_id("a", Some("b")));
        assert_ne!(pair_id("a", Some("b")), pair_id("b", Some("a")));
        assert_eq!(pair_id("a", None).len(), 13);
    }
}
