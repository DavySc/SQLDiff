//! A review session: one pair of files or two paired trees, diffed with the
//! resolved config, plus the session's assumption flips and the review store.
//! Everything the queue, assumption panel and provenance bar show comes from here.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sqldiff_core::model::AssumptionSource;
use sqldiff_core::{session as flips, Artifact, ChangeClass, Dialect, Report, Severity, Verdict};
use sqldiff_source::tree::{self, Ambiguous, Tree, TreePair};
use sqldiff_source::{LayerRef, Resolved};

use crate::review::{self, Decision, ReviewState, Store};
use crate::{build_view, Payload};

/// What the session compares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    Files { a: PathBuf, b: PathBuf },
    Trees { a: PathBuf, b: PathBuf },
}

impl Origin {
    /// Two directories are trees; anything else is a pair of files.
    pub fn detect(a: PathBuf, b: PathBuf) -> Origin {
        if a.is_dir() && b.is_dir() {
            Origin::Trees { a, b }
        } else {
            Origin::Files { a, b }
        }
    }
}

/// A reviewer's flip of one assumption for this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Flip {
    /// As the tool or a pack states it.
    Open,
    /// Holds: marked declared, verdicts unchanged.
    Confirmed,
    /// Does not hold: findings and verdicts relying on it drop to unknown.
    Denied,
}

struct Loaded {
    pair: TreePair,
    sides: Option<(Artifact, Artifact)>,
    /// Engine report with pack policy, before session flips.
    base: Option<Report>,
    error: Option<String>,
}

pub struct Session {
    origin: Origin,
    dialect_a: Option<Dialect>,
    dialect_b: Option<Dialect>,
    cfg: Resolved,
    tree: Tree,
    pairs: Vec<Loaded>,
    flips: BTreeMap<String, Flip>,
    pub store: Store,
    loaded_at: u64,
}

/// One row of the review queue.
#[derive(Debug, Clone, Serialize)]
pub struct QueueEntry {
    pub index: usize,
    pub a: String,
    pub b: String,
    /// Directory of the pair relative to tree A's root (`.` at the root).
    pub folder: String,
    pub dialect_a: String,
    pub dialect_b: String,
    pub verdict: Option<Verdict>,
    pub highest_class: Option<ChangeClass>,
    pub severity: Option<Severity>,
    pub findings: usize,
    /// Findings with a decision on this snapshot.
    pub reviewed: usize,
    pub accepted: usize,
    pub assumptions: Vec<String>,
    pub error: Option<String>,
}

/// One assumption across the session.
#[derive(Debug, Clone, Serialize)]
pub struct AssumptionRow {
    pub id: String,
    pub text: String,
    /// Where the statement comes from: `default` (the tool assumes it) or `declared` (pack, config).
    pub source: AssumptionSource,
    pub flip: Flip,
    /// Pairs whose report relies on it.
    pub pairs: Vec<usize>,
    /// Pairs whose verdict changes if it is denied (or changed because it is).
    pub flips_verdict: Vec<usize>,
}

/// Provenance bar and session-wide state.
#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub origin: Origin,
    /// When the sources were last read (RFC 3339).
    pub loaded_at: String,
    pub tool_version: String,
    /// Config layers and packs, with hashes.
    pub layers: Vec<LayerRef>,
    pub dialect_versions: BTreeMap<String, String>,
    pub assumptions: Vec<AssumptionRow>,
    pub reviews: Vec<review::Entry>,
    pub only_a: Vec<String>,
    pub only_b: Vec<String>,
    pub ambiguous: Vec<Ambiguous>,
    pub no_dialect: Vec<String>,
}

/// What the page embeds.
#[derive(Debug, Clone, Serialize)]
pub struct Page {
    pub live: bool,
    pub session: SessionInfo,
    pub queue: Vec<QueueEntry>,
    /// Pair payloads by queue index: all of them in a static export, the opened one when live.
    pub pairs: Vec<Option<Payload>>,
    pub open: Option<usize>,
}

impl Session {
    pub fn open(
        origin: Origin,
        dialect_a: Option<Dialect>,
        dialect_b: Option<Dialect>,
        cfg: Resolved,
        store: Store,
    ) -> Result<Session, String> {
        let mut s = Session {
            origin,
            dialect_a,
            dialect_b,
            cfg,
            tree: Tree::default(),
            pairs: Vec::new(),
            flips: BTreeMap::new(),
            store,
            loaded_at: 0,
        };
        s.refresh()?;
        Ok(s)
    }

    /// Re-reads the sources (and re-pairs trees) and diffs every pair.
    pub fn refresh(&mut self) -> Result<(), String> {
        self.tree = match &self.origin {
            Origin::Trees { a, b } => {
                tree::pair_dirs(a, b, self.dialect_a, self.dialect_b, &self.cfg).map_err(|e| e.0)?
            }
            Origin::Files { a, b } => Tree {
                pairs: vec![TreePair {
                    id_a: tree::slashed(a),
                    id_b: tree::slashed(b),
                    path_a: a.clone(),
                    path_b: b.clone(),
                }],
                dialect_a: self.dialect_a,
                dialect_b: self.dialect_b,
                ..Tree::default()
            },
        };
        self.pairs = self
            .tree
            .pairs
            .iter()
            .map(|p| match self.tree.read(p, &self.cfg) {
                Ok((a, b)) => Loaded {
                    pair: p.clone(),
                    base: Some(tree::diff(&a, &b, &self.cfg)),
                    sides: Some((a, b)),
                    error: None,
                },
                Err(e) => Loaded {
                    pair: p.clone(),
                    sides: None,
                    base: None,
                    error: Some(e.0),
                },
            })
            .collect();
        if let (Origin::Files { .. }, Some(Loaded { error: Some(e), .. })) =
            (&self.origin, self.pairs.first())
        {
            return Err(e.clone());
        }
        self.loaded_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    fn ids(&self, f: Flip) -> Vec<String> {
        self.flips
            .iter()
            .filter(|(_, v)| **v == f)
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// A report with the session's flips applied.
    fn flipped(&self, mut r: Report) -> Report {
        flips::confirm_assumptions(&mut r, &self.ids(Flip::Confirmed));
        flips::deny_assumptions(&mut r, &self.ids(Flip::Denied));
        r
    }

    pub fn report(&self, i: usize) -> Option<Report> {
        self.pairs.get(i)?.base.clone().map(|r| self.flipped(r))
    }

    pub fn set_flip(&mut self, id: &str, f: Flip) {
        if f == Flip::Open {
            self.flips.remove(id);
        } else {
            self.flips.insert(id.to_string(), f);
        }
    }

    /// The pair view of pair `i`.
    pub fn payload(&self, i: usize, live: bool) -> Option<Payload> {
        let l = self.pairs.get(i)?;
        let (a, b) = l.sides.as_ref()?;
        let mut p = build_view(a, b, self.report(i)?, live);
        p.index = i;
        Some(p)
    }

    /// Engine options for pair `i`'s re-runs: pack config plus session flips.
    fn options(&self) -> sqldiff_core::Options {
        let mut o = self.cfg.options();
        for (id, f) in &self.flips {
            o.flips.insert(id.clone(), *f == Flip::Confirmed);
        }
        o
    }

    /// "What if the statement on `line` of `side` fails with `kind`?" for pair `i`.
    pub fn what_if(
        &self,
        i: usize,
        side: sqldiff_core::model::Side,
        line: usize,
        kind: sqldiff_core::ir::ErrorKind,
    ) -> Option<sqldiff_core::ir::witness::WhatIf> {
        let (a, b) = self.pairs.get(i)?.sides.as_ref()?;
        Some(sqldiff_core::what_if(
            a,
            b,
            &self.options(),
            side,
            line,
            kind,
        ))
    }

    /// Transaction brackets of both sides of pair `i`.
    pub fn overlay(&self, i: usize) -> Option<sqldiff_core::ir::overlay::Overlay> {
        let (a, b) = self.pairs.get(i)?.sides.as_ref()?;
        Some(sqldiff_core::overlay(a, b))
    }

    /// Scratchpad texts diffed as pair `i` (same paths, dialects and versions).
    pub fn scratch(&self, i: usize, ta: String, tb: String) -> Option<Payload> {
        let (a, b) = self.pairs.get(i)?.sides.as_ref()?;
        let (mut a, mut b) = (a.clone(), b.clone());
        a.text = ta;
        b.text = tb;
        let mut p = build_view(&a, &b, self.flipped(tree::diff(&a, &b, &self.cfg)), true);
        p.index = i;
        Some(p)
    }

    /// Records or clears (`None`) a decision on a finding.
    pub fn review(
        &mut self,
        fingerprint: &str,
        snapshot: &str,
        state: Option<ReviewState>,
        comment: Option<String>,
    ) -> Result<(), String> {
        match state {
            None => self.store.remove(fingerprint, snapshot),
            Some(state) => self.store.put(
                fingerprint,
                snapshot,
                Decision {
                    state,
                    comment: comment.filter(|c| !c.trim().is_empty()),
                    author: None,
                    at: review::now_rfc3339(),
                    expires: None,
                },
            ),
        }
    }

    /// The pair whose paths end with `a` and `b` (deep links from agents and scripts).
    pub fn find(&self, a: &str, b: &str) -> Option<usize> {
        let norm = |s: &str| s.replace('\\', "/");
        let (a, b) = (norm(a), norm(b));
        let ends = |p: &Path, id: &str, q: &str| {
            !q.is_empty()
                && (norm(&p.display().to_string()).ends_with(q)
                    || id.ends_with(q)
                    || q.ends_with(id))
        };
        self.pairs.iter().position(|l| {
            ends(&l.pair.path_a, &l.pair.id_a, &a) && ends(&l.pair.path_b, &l.pair.id_b, &b)
        })
    }

    pub fn queue(&self) -> Vec<QueueEntry> {
        (0..self.pairs.len()).map(|i| self.entry(i)).collect()
    }

    fn entry(&self, i: usize) -> QueueEntry {
        let l = &self.pairs[i];
        let folder = Path::new(&l.pair.id_a)
            .parent()
            .map(tree::slashed)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| ".".to_string());
        let mut e = QueueEntry {
            index: i,
            a: l.pair.path_a.display().to_string(),
            b: l.pair.path_b.display().to_string(),
            folder,
            dialect_a: String::new(),
            dialect_b: String::new(),
            verdict: None,
            highest_class: None,
            severity: None,
            findings: 0,
            reviewed: 0,
            accepted: 0,
            assumptions: Vec::new(),
            error: l.error.clone(),
        };
        let Some(r) = self.report(i) else { return e };
        let snap = review::snapshot(
            &r.a.content_hash,
            r.b.as_ref().map_or("", |b| b.content_hash.as_str()),
        );
        e.dialect_a = r.a.dialect.clone();
        e.dialect_b = r.b.as_ref().map(|b| b.dialect.clone()).unwrap_or_default();
        e.verdict = Some(r.verdict);
        e.highest_class = r.highest_class();
        e.severity = r.findings.iter().map(|f| f.severity).max();
        e.findings = r.findings.len();
        for f in &r.findings {
            if let Some(d) = self.store.get(&f.fingerprint, &snap) {
                e.reviewed += 1;
                if d.state == ReviewState::Accepted {
                    e.accepted += 1;
                }
            }
        }
        e.assumptions = used_assumptions(&r);
        e
    }

    pub fn info(&self) -> SessionInfo {
        let mut rows: BTreeMap<String, AssumptionRow> = BTreeMap::new();
        for (i, l) in self.pairs.iter().enumerate() {
            let Some(base) = &l.base else { continue };
            for id in used_assumptions(base) {
                let (text, source) = base
                    .assumptions
                    .iter()
                    .find(|a| a.id == id)
                    .map(|a| (a.text.clone(), a.source))
                    .unwrap_or((String::new(), AssumptionSource::Default));
                let row = rows.entry(id.clone()).or_insert_with(|| AssumptionRow {
                    flip: self.flips.get(&id).copied().unwrap_or(Flip::Open),
                    id: id.clone(),
                    text: String::new(),
                    source,
                    pairs: Vec::new(),
                    flips_verdict: Vec::new(),
                });
                if row.text.is_empty() {
                    row.text = text;
                }
                row.pairs.push(i);
                // Would denying it alone change this pair's verdict?
                let mut denied = base.clone();
                flips::deny_assumptions(&mut denied, std::slice::from_ref(&id));
                if denied.verdict != base.verdict {
                    row.flips_verdict.push(i);
                }
            }
        }
        // Flipped ids no pair uses any more stay visible so they can be reset.
        for (id, f) in &self.flips {
            rows.entry(id.clone()).or_insert_with(|| AssumptionRow {
                id: id.clone(),
                text: String::new(),
                source: AssumptionSource::Default,
                flip: *f,
                pairs: Vec::new(),
                flips_verdict: Vec::new(),
            });
        }
        SessionInfo {
            origin: self.origin.clone(),
            loaded_at: review::rfc3339(self.loaded_at),
            tool_version: sqldiff_core::TOOL_VERSION.to_string(),
            layers: self.cfg.layers.clone(),
            dialect_versions: Dialect::ALL
                .iter()
                .map(|d| (d.id().to_string(), self.cfg.dialect_version(*d)))
                .collect(),
            assumptions: rows.into_values().collect(),
            reviews: self.store.all().collect(),
            only_a: self.tree.only_a.clone(),
            only_b: self.tree.only_b.clone(),
            ambiguous: self.tree.ambiguous.clone(),
            no_dialect: self.tree.no_dialect.clone(),
        }
    }

    /// The page payload. Live pages carry only the opened pair; static exports carry all.
    pub fn page(&self, open: Option<usize>, live: bool) -> Page {
        let open = open.or((self.pairs.len() == 1).then_some(0));
        let pairs = (0..self.pairs.len())
            .map(|i| {
                if !live || Some(i) == open {
                    self.payload(i, live)
                } else {
                    None
                }
            })
            .collect();
        Page {
            live,
            session: self.info(),
            queue: self.queue(),
            pairs,
            open,
        }
    }
}

impl Session {
    /// The session report as Markdown, optionally redacted.
    pub fn session_report(&self, redact: bool) -> String {
        let mut red = crate::report::Redactor::new(redact);
        let info = self.info();
        let queue = self.queue();
        for l in &self.pairs {
            red.path(&l.pair.path_a.display().to_string());
            red.path(&l.pair.path_b.display().to_string());
            if let Some((a, b)) = &l.sides {
                red.source(&a.text, a.dialect);
                red.source(&b.text, b.dialect);
            }
            if let Some(r) = &l.base {
                red.word(&r.a.object);
                if let Some(b) = &r.b {
                    red.word(&b.object);
                }
            }
        }
        for p in info
            .only_a
            .iter()
            .chain(&info.only_b)
            .chain(&info.no_dialect)
        {
            red.path(p);
        }
        let (Origin::Files { a, b } | Origin::Trees { a, b }) = &self.origin;
        red.path(&a.display().to_string());
        red.path(&b.display().to_string());
        let reports: Vec<(Option<Report>, String)> = (0..self.pairs.len())
            .map(|i| {
                let r = self.report(i);
                let snap = r.as_ref().map_or(String::new(), |r| {
                    review::snapshot(
                        &r.a.content_hash,
                        r.b.as_ref().map_or("", |b| b.content_hash.as_str()),
                    )
                });
                (r, snap)
            })
            .collect();
        crate::report::markdown(&info, &queue, &reports, &self.store, &red)
    }
}

/// Assumption ids a report relies on: report-level and per finding.
fn used_assumptions(r: &Report) -> Vec<String> {
    let mut ids: BTreeSet<String> = r.assumptions.iter().map(|a| a.id.clone()).collect();
    for f in &r.findings {
        ids.extend(f.assumptions.iter().cloned());
    }
    ids.into_iter().collect()
}
