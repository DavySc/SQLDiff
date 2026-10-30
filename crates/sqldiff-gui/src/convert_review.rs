//! Review mode: a converted tree's review queue, one item at a time, with
//! the decisions people make recorded in the tree's ledger.
//!
//! `sqldiff-gui review SRC OUT --to D` converts SRC in memory the way
//! `sqldiff convert` does (`sqldiff_convert::tree`), applies
//! `OUT/review-decisions.jsonl` and ranks what is left (`queue::build`). The
//! page shows the queue by tier and, per item, the source and the converted
//! output side by side with the rules' notes and the engine's findings.
//!
//! Its actions append records to the ledger and nothing else: OUT's
//! converted files are never written. Every record is built here: `by` is
//! always `human` (the page cannot say otherwise; unknown fields are
//! refused), the author is the name the page sends, and the hashes are those
//! of the input and of the conversion's output (`Conversion::output`, before
//! the status lines are added), which is what `review::review_file` compares
//! against. A record is only written when it applies to the unit as the
//! server sees it now: the page sends the hashes it showed, and when the
//! file changed since, nothing is written.
//!
//! After every action the sources are read again and the ledger applied
//! again, so the queue always reflects the files and the ledger as they are.
//! The tree is converted again whenever a source file changed; while none
//! did, the previous conversion is kept, since converting the same files with
//! the same options gives the same conversion.
//!
//! - `GET /`: the page
//! - `GET /api/state[?all=1]`: counts per tier, ledger summary, warnings,
//!   the first reviewed units, the done gate (`all` adds tier 6; the gate
//!   lists its first [`GATE_BLOCKERS_SHOWN`] blockers, and `blockers_total`
//!   counts them all). The queue's items come from the list endpoints.
//! - `GET /api/gate`: the done gate alone, as `/api/state` gives it
//! - `GET /api/list?tiers=..&group=cause|file|tier&q=..[&from=N&count=N]`: a
//!   lens's groups (see [`crate::review_list`]); the same `tiers`, `group`
//!   and `q` (and `all`) go with the next four
//! - `GET /api/rows?..&key=G[&from=N&count=N]`: rows of the group `G`
//! - `GET /api/accept?..&key=G`: its units with `G` as an open assumption
//! - `GET /api/near?..&from=K[&step=N|&jump=N]` or `?..&index=N`: where an
//!   item is listed, the item `N` on, the first of the group `N` on, or the
//!   item at `N` of the walk order (`null` when nothing is shown)
//! - `GET /api/reviewed[?from=N&count=N]`: reviewed units, a page
//! - `GET /api/item?key=K[&all=1]`: one queue item in detail
//! - `GET /api/unit?path=P&unit=U`: one unit in detail (also reviewed ones)
//! - `GET /api/file?path=P&side=in|out`: a whole input, or its conversion
//! - `POST /api/record`: append records (see [`RecordRequest`])
//! - `POST /api/refresh`: read the sources and the ledger again

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sqldiff_convert::gate::{self, Gate};
use sqldiff_convert::queue::{self, Instances, Item, Kind};
use sqldiff_convert::review::{self, By, Record, State};
use sqldiff_convert::tree::{self, Input, Tree, UnitReviewReport};
use sqldiff_convert::{Status, Unit};

use crate::packs::LoadedPack;
use crate::review_list::{self, Listing, Place, Query};
use crate::server::{
    allowed_hosts, bad, json, not_found, read_request, respond, Request, Response,
};

/// The review page; the payload is injected at `/*__SQLDIFF_PAYLOAD__*/null`
/// and convert mode's highlighter at `/*__SQLDIFF_LEXER__*/`.
pub const REVIEW_HTML: &str = include_str!("../assets/review.html");

/// What to review.
pub struct Setup {
    pub tree: tree::Args,
    /// The converted tree: where the ledger is, and what `sqldiff convert` wrote.
    pub out: PathBuf,
    /// `instance-report.json`, or the directory holding it.
    pub instances: Option<PathBuf>,
    /// The packs the config of this review loaded, as the page confirms them.
    pub packs: Vec<LoadedPack>,
}

/// The tree as last read and converted, with the ledger applied.
struct Loaded {
    inputs: Vec<Input>,
    unreadable: Vec<String>,
    tree: Tree,
    /// Per file (report order): unit ids, in the conversion's unit order.
    ids: Vec<Vec<String>>,
    /// Indices into `inputs`, sorted by path, and (file, unit) indices
    /// into `ids`, sorted by id (then file and unit): lookups without a
    /// copy of a path or an id.
    by_path: Vec<u32>,
    by_id: Vec<(u32, u32)>,
    instances: Option<Instances>,
    /// Ledger lines that were not applied, with why.
    not_applied: Vec<String>,
    /// Files in OUT that differ from what this conversion writes (or are missing).
    out_differs: Vec<String>,
    /// Milliseconds the last conversion took, and whether this read converted.
    convert_ms: f64,
    converted: bool,
    /// The hand-written texts in the ledger the conversion used: they go
    /// into the conversion itself, so a new one means converting again.
    written: Vec<String>,
    /// The done gate as `sqldiff review-status` judged the tree on this
    /// read, or why it could not be judged; when, and the files it judged
    /// (`stamp`): once they change, the verdict no longer stands.
    gate: Result<ShownGate, String>,
    judged: String,
    stamp: Stamp,
    /// The queue as the page lists it, tier 6 included and without the
    /// hashes (rows get those): built on first use, once per read.
    listed: OnceLock<Vec<ListedItem>>,
    /// The last listing asked for, which the page pages through.
    listing: Mutex<Option<Listing>>,
}

/// Every file under the source, OUT, the instance report and the base
/// catalogs, with its size and modification time: what a gate verdict was
/// judged on. Names starting
/// with a dot are skipped and links not followed below the given paths, as
/// the engine reads a tree.
type Stamp = Vec<(PathBuf, u64, Option<std::time::SystemTime>)>;

fn stamp(s: &Setup) -> Stamp {
    fn dir(p: &Path, out: &mut Stamp) {
        let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(p)
            .map(|d| d.flatten().collect())
            .unwrap_or_default();
        entries.retain(|e| !e.file_name().to_string_lossy().starts_with('.'));
        entries.sort_by_key(|e| e.file_name());
        out.push((p.to_path_buf(), entries.len() as u64, None));
        for e in entries {
            match e.file_type() {
                Ok(t) if t.is_dir() => dir(&e.path(), out),
                _ => file(&e.path(), std::fs::symlink_metadata(e.path()), out),
            }
        }
    }
    fn file(p: &Path, m: std::io::Result<std::fs::Metadata>, out: &mut Stamp) {
        match m {
            Ok(m) => out.push((p.to_path_buf(), m.len(), m.modified().ok())),
            Err(_) => out.push((p.to_path_buf(), u64::MAX, None)),
        }
    }
    let mut out = Vec::new();
    let given = [Some(&s.tree.src), Some(&s.out), s.instances.as_ref()];
    let base = s.tree.catalogs.iter().chain(&s.tree.catalogs_b);
    for p in given.into_iter().flatten().chain(base) {
        match std::fs::metadata(p) {
            Ok(m) if m.is_dir() => dir(p, &mut out),
            m => file(p, m, &mut out),
        }
    }
    out
}

/// Why a verdict is not shown once its files changed.
pub const GATE_STALE: &str =
    "files changed on disk since the done gate was judged; Refresh to judge it again";

pub struct Review {
    setup: Setup,
    loaded: Loaded,
}

/// A queue item as the page lists it.
#[derive(Debug, Clone, Serialize)]
pub struct ListedItem {
    #[serde(flatten)]
    pub item: Item,
    /// For a unit: the assumptions still open on it (bulk accept offers these).
    pub open_assumptions: Vec<String>,
    /// For a unit: what no decision can discharge on it (the page groups
    /// units that need a fix by these).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blocking: Vec<String>,
    /// For a unit: the hashes an action on it sends back, as of this state
    /// (bulk accept of a whole group sends these; the server checks them).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hashes: Option<(String, String)>,
}

/// A unit the ledger says is reviewed: accepted in review, never "converted".
#[derive(Debug, Clone, Serialize)]
pub struct ReviewedUnit {
    pub path: String,
    pub unit: String,
    pub status: Status,
    pub src_line: usize,
    /// People whose current approval, acceptance or pick is on it.
    pub by: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StateView {
    pub from: String,
    pub to: String,
    pub src: String,
    pub out: String,
    pub ledger_path: String,
    pub templates: bool,
    pub files: usize,
    pub units: usize,
    /// Items in tiers 1 to 5. The items themselves come a page at a time
    /// (`/api/list`, `/api/rows`).
    pub open: usize,
    pub tiers: BTreeMap<u8, usize>,
    pub tier_labels: BTreeMap<u8, &'static str>,
    /// Units reviewed by people (not in the queue): the first
    /// [`REVIEWED_SHOWN`] (`/api/reviewed` pages through the rest), and
    /// how many there are.
    pub reviewed: Vec<ReviewedUnit>,
    pub reviewed_total: usize,
    /// Whether an instance report was read.
    pub instances: bool,
    /// Whether tier 6 (plainly converted units nobody approved) is listed.
    pub all: bool,
    pub records: usize,
    pub not_applied: Vec<String>,
    /// Decisions on units that no longer exist, per file.
    pub orphaned: usize,
    /// Units with a decision that no longer applies.
    pub outdated: usize,
    pub out_differs: Vec<String>,
    pub unreadable: Vec<String>,
    pub convert_ms: f64,
    /// The done gate (`sqldiff review-status`): the only place "done" comes
    /// from. `gate_error` says why it could not be judged. Sent with its
    /// first [`GATE_BLOCKERS_SHOWN`] blockers and `blockers_total`.
    pub gate: Option<ShownGate>,
    pub gate_error: Option<String>,
    pub gate_judged: String,
    /// The packs loaded (a private pack by path only).
    pub packs: Vec<LoadedPack>,
}

/// The done gate alone, as [`StateView`] has it (`GET /api/gate`).
#[derive(Debug, Clone, Serialize)]
pub struct GateState {
    pub gate: Option<ShownGate>,
    pub gate_error: Option<String>,
    pub gate_judged: String,
}

/// Blockers the page gets of a gate; it shows these and counts the rest
/// (`blockers_total`). A large tree has tens of thousands, which made the
/// page's payload several times larger than everything else on it.
pub const GATE_BLOCKERS_SHOWN: usize = 100;

/// Reviewed units the state carries; the page asks for more.
pub const REVIEWED_SHOWN: usize = 200;

/// Most rows one `/api/rows` or `/api/reviewed` answer carries, and most
/// groups one `/api/list` answer does.
pub const PAGE_MAX: usize = 1000;

/// The done gate as the review keeps and shows it: its first
/// [`GATE_BLOCKERS_SHOWN`] blockers and how many there are, the rest as
/// `sqldiff review-status` judged it. Only these are kept, not every
/// blocker: on a large tree they took a sixth of what the review held, and
/// every request copied them.
#[derive(Debug, Clone, Serialize)]
pub struct ShownGate {
    pub complete: bool,
    pub blockers: Vec<gate::Blocker>,
    pub blockers_total: usize,
    pub counts: BTreeMap<String, usize>,
    pub accepted: Vec<gate::Accepted>,
    pub units: usize,
    pub nothing_found: usize,
    pub instances: Option<bool>,
}

impl From<Gate> for ShownGate {
    fn from(g: Gate) -> ShownGate {
        // Every field named, so a field added to `Gate` cannot be left out here.
        let Gate {
            complete,
            mut blockers,
            counts,
            accepted,
            units,
            nothing_found,
            instances,
        } = g;
        let blockers_total = blockers.len();
        blockers.truncate(GATE_BLOCKERS_SHOWN);
        blockers.shrink_to_fit();
        ShownGate {
            complete,
            blockers,
            blockers_total,
            counts,
            accepted,
            units,
            nothing_found,
            instances,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct NoteView {
    pub status: Status,
    pub rule: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FindingView {
    /// `diff` (the engine's diff of input against output) or `output` (the
    /// output checked on its own).
    pub from: &'static str,
    pub rule: String,
    pub verdict: sqldiff_core::Verdict,
    pub severity: sqldiff_core::Severity,
    pub witness: String,
    pub assumptions: Vec<String>,
}

/// One unit, everything the page shows about it and what it may record.
#[derive(Debug, Clone, Serialize)]
pub struct UnitView {
    pub path: String,
    pub unit: String,
    pub status: Status,
    /// The engine's part of the status.
    pub checked: Status,
    /// 1-based lines of the unit in the input and in the conversion's output
    /// (without status lines), and its first line in the file OUT holds.
    pub src: (usize, usize),
    pub out: (usize, usize),
    pub written_line: usize,
    pub notes: Vec<NoteView>,
    pub findings: Vec<FindingView>,
    pub review: Option<UnitReviewReport>,
    /// What the page sends back with an action.
    pub input_hash: String,
    pub output_hash: String,
    /// Rules that `accept` / `pick` may name.
    pub assumptions: Vec<String>,
    pub decisions: Vec<String>,
    /// Why `approve` is refused, when it is.
    pub approve_blocked: Option<String>,
    /// Instances woven from this file's template, and those not converted.
    pub instances: usize,
    pub failing_instances: Vec<(String, String)>,
}

/// `/api/list`: a lens's groups, a page of them.
#[derive(Debug, Clone, Serialize)]
pub struct ListView {
    /// Items in the lens's tiers, and those the filter shows.
    pub total: usize,
    pub shown: usize,
    pub groups_total: usize,
    pub groups: Vec<GroupView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupView {
    pub key: String,
    /// Items in the group.
    pub n: usize,
    pub desc: String,
    /// Units with the group's cause as an open assumption.
    pub accept: usize,
}

/// `/api/rows`: a page of a group's items.
#[derive(Debug, Clone, Serialize)]
pub struct RowsView {
    pub n: usize,
    pub rows: Vec<ListedItem>,
}

/// What `/api/near` looks for.
#[derive(Debug, Clone, Copy)]
pub enum Near<'a> {
    At(&'a str),
    Step(&'a str, i64),
    Group(&'a str, i64),
    Index(usize),
}

#[derive(Debug, Clone, Serialize)]
pub struct ItemView {
    pub item: Item,
    /// The unit itself (kind `unit`), or each unit a missing object stands for.
    pub units: Vec<UnitView>,
    /// For a template item, or a unit in a template file: its failing instances.
    pub failing_instances: Vec<(String, String)>,
}

/// One unit an action is about, as the page showed it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub path: String,
    pub unit: String,
    pub input_hash: String,
    pub output_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Accept,
    Pick,
    Approve,
    Reject,
    Comment,
}

/// `POST /api/record`. There is no `by`: every record is a person's, and a
/// body that tries to say otherwise is refused.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordRequest {
    pub action: Action,
    /// The assumption (`accept`) or decision (`pick`); for `reject` and
    /// `comment`, optionally the note it is about.
    #[serde(default)]
    pub rule: String,
    /// Why; the comment's text for `comment`.
    pub reason: String,
    pub author: String,
    pub targets: Vec<Target>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordOutcome {
    /// Records appended.
    pub written: usize,
    pub state: StateView,
}

impl Review {
    pub fn open(setup: Setup) -> Result<Review, String> {
        Review::open_with(setup, &tree::Hooks::default())
    }

    /// [`Review::open`], reporting the conversion's progress to `hooks` and
    /// stopping when they say so.
    pub fn open_with(setup: Setup, hooks: &tree::Hooks) -> Result<Review, String> {
        if !setup.out.is_dir() {
            return Err(format!(
                "{}: not a directory; give the converted tree `sqldiff convert -o` wrote",
                setup.out.display()
            ));
        }
        let loaded = load(&setup, None, hooks)?;
        Ok(Review { setup, loaded })
    }

    /// A review of what `job::convert_into` just wrote: its conversion is
    /// used again, unless a source or the ledger changed since.
    pub fn open_converted(
        setup: Setup,
        written: sqldiff_convert::job::Written,
    ) -> Result<Review, String> {
        let prev = Loaded {
            convert_ms: written.tree.report.totals.seconds * 1000.0,
            unreadable: written.tree.report.unreadable.clone(),
            inputs: written.inputs,
            tree: written.tree,
            written: written_texts(&setup.out)?,
            ..empty()
        };
        let loaded = load(&setup, Some(prev), &tree::Hooks::default())?;
        Ok(Review { setup, loaded })
    }

    /// A review of a stored conversion of `setup`'s tree (see
    /// [`crate::stored`]), with the tree as read now (`now`, from
    /// [`stored_key`], whose key it was stored under): the ledger, the
    /// instance report and the done gate are read and judged as for a
    /// fresh conversion; only converting is skipped.
    pub fn open_stored(setup: Setup, tree: Tree, now: Now) -> Result<Review, String> {
        if !setup.out.is_dir() {
            return Err(format!(
                "{}: not a directory; give the converted tree `sqldiff convert -o` wrote",
                setup.out.display()
            ));
        }
        let prev = Loaded {
            convert_ms: tree.report.totals.seconds * 1000.0,
            inputs: now.inputs,
            unreadable: now.unreadable,
            written: now.written,
            tree,
            ..empty()
        };
        let loaded = load(&setup, Some(prev), &tree::Hooks::default())?;
        Ok(Review { setup, loaded })
    }

    /// The key a stored copy of this conversion goes under: what it was
    /// converted from, and `extra` (the build, the command, its config).
    pub fn stored_key(&self, extra: &[u8]) -> String {
        let l = &self.loaded;
        key_of(
            extra,
            &l.inputs,
            &l.unreadable,
            &l.written,
            l.tree.report.base_hash.as_deref(),
        )
    }

    /// The conversion as it is stored ([`crate::stored`]).
    pub fn tree_json(&self) -> Vec<u8> {
        serde_json::to_vec(&self.loaded.tree).unwrap_or_default()
    }

    /// What the review was opened on.
    pub fn setup(&self) -> &Setup {
        &self.setup
    }

    pub fn ledger_path(&self) -> PathBuf {
        self.setup.out.join(review::LEDGER_FILE)
    }

    /// Reads the sources, the instance report and the ledger again.
    pub fn refresh(&mut self) -> Result<(), String> {
        let prev = std::mem::replace(&mut self.loaded, empty());
        match load(&self.setup, Some(prev), &tree::Hooks::default()) {
            Ok(l) => {
                self.loaded = l;
                Ok(())
            }
            Err(e) => {
                // Keep nothing stale: the next read starts over.
                self.loaded =
                    load(&self.setup, None, &tree::Hooks::default()).unwrap_or_else(|_| empty());
                Err(e)
            }
        }
    }

    /// Whether the last refresh converted the tree (else it reused the
    /// conversion of unchanged sources).
    pub fn converted_last(&self) -> bool {
        self.loaded.converted
    }

    /// The report as `sqldiff convert` would write it now (for tests).
    pub fn report_json(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(&self.loaded.tree.report).unwrap();
        if let Some(t) = v.get_mut("totals") {
            t["seconds"] = serde_json::Value::Null;
        }
        if let Some(fs) = v.get_mut("files").and_then(|f| f.as_array_mut()) {
            for f in fs {
                f["seconds"] = serde_json::Value::Null;
            }
        }
        v
    }

    pub fn queue(&self, all: bool) -> queue::Queue {
        queue::build(
            &self.loaded.tree.report,
            self.loaded.instances.as_ref(),
            all,
        )
    }

    /// The done gate as last judged, or why it could not be judged; never
    /// a verdict on files that changed since (`GATE_STALE`).
    pub fn gate(&self) -> Result<ShownGate, String> {
        match stamp(&self.setup) == self.loaded.stamp {
            true => self.loaded.gate.clone(),
            false => Err(GATE_STALE.into()),
        }
    }

    /// When the done gate was last judged.
    pub fn judged(&self) -> &str {
        &self.loaded.judged
    }

    /// The done gate as the page shows it, judged or stale.
    pub fn gate_state(&self) -> GateState {
        let gate = self.gate();
        GateState {
            gate: gate.as_ref().ok().cloned(),
            gate_error: gate.err(),
            gate_judged: self.loaded.judged.clone(),
        }
    }

    /// Units people reviewed, in path order, and decisions on units that
    /// no longer exist, units with an outdated decision, and all units.
    fn tally(&self) -> (Vec<ReviewedUnit>, usize, usize, usize) {
        let l = &self.loaded;
        let r = &l.tree.report;
        let mut reviewed = Vec::new();
        let (mut orphaned, mut outdated, mut units) = (0, 0, 0);
        for f in &r.files {
            if let Some(fr) = &f.review {
                orphaned += fr.orphaned.len();
                outdated += fr.units_outdated;
            }
            for u in f.units.iter().chain(&f.converted_units) {
                units += 1;
                let Some(rv) = u.review.as_ref().filter(|rv| rv.reviewed) else {
                    continue;
                };
                let mut by: Vec<String> = rv
                    .decisions
                    .iter()
                    .filter(|d| d.current && d.record.by == By::Human)
                    .filter(|d| {
                        matches!(
                            d.record.state,
                            State::Accepted | State::Picked | State::Approved
                        )
                    })
                    .map(|d| d.record.author.trim().to_string())
                    .collect();
                by.sort();
                by.dedup();
                reviewed.push(ReviewedUnit {
                    path: f.path.clone(),
                    unit: u.id.clone(),
                    status: u.status,
                    src_line: u.src_line,
                    by,
                });
            }
        }
        reviewed.sort_by(|a, b| a.path.cmp(&b.path).then(a.src_line.cmp(&b.src_line)));
        (reviewed, orphaned, outdated, units)
    }

    pub fn state(&self, all: bool) -> StateView {
        let gate = self.gate_state();
        let l = &self.loaded;
        let r = &l.tree.report;
        let mut tiers: BTreeMap<u8, usize> = (1..=6u8).map(|t| (t, 0)).collect();
        for it in self.listed_all() {
            if it.item.tier != 6 || all {
                *tiers.entry(it.item.tier).or_default() += 1;
            }
        }
        let (mut reviewed, orphaned, outdated, units) = self.tally();
        let reviewed_total = reviewed.len();
        reviewed.truncate(REVIEWED_SHOWN);
        StateView {
            from: r.from.clone(),
            to: r.to.clone(),
            src: r.src.clone(),
            out: self.setup.out.display().to_string(),
            ledger_path: self.ledger_path().display().to_string(),
            templates: r.templates,
            files: r.files.len(),
            units,
            open: (1..=5u8).map(|t| tiers[&t]).sum(),
            tiers,
            tier_labels: (1..=6u8).map(|t| (t, queue::tier_label(t))).collect(),
            reviewed,
            reviewed_total,
            instances: l.instances.is_some(),
            all,
            records: r.ledger.as_ref().map_or(0, |x| x.records),
            not_applied: l.not_applied.clone(),
            orphaned,
            outdated,
            out_differs: l.out_differs.clone(),
            unreadable: l.unreadable.clone(),
            convert_ms: l.convert_ms,
            gate: gate.gate,
            gate_error: gate.gate_error,
            gate_judged: gate.gate_judged,
            packs: self.setup.packs.clone(),
        }
    }

    /// The queue as the page lists it, tier 6 included, without hashes.
    fn listed_all(&self) -> &[ListedItem] {
        self.loaded.listed.get_or_init(|| {
            let l = &self.loaded;
            self.queue(true)
                .items
                .into_iter()
                .map(|item| {
                    let found = match (&item.kind, &item.unit) {
                        (Kind::Unit, Some(id)) => self.find(&item.path, id),
                        _ => None,
                    };
                    let Some((f, k)) = found else {
                        return ListedItem {
                            item,
                            open_assumptions: Vec::new(),
                            blocking: Vec::new(),
                            hashes: None,
                        };
                    };
                    let u = &l.tree.conversions[f].units[k];
                    let rv = self.unit_review(f, &l.ids[f][k]);
                    ListedItem {
                        open_assumptions: open_assumptions(u, rv),
                        blocking: rv.map_or_else(
                            || review::undischargeable(u),
                            |rv| rv.undischargeable.clone(),
                        ),
                        hashes: None,
                        item,
                    }
                })
                .collect()
        })
    }

    /// Items as a page row shows them: with the hashes an action on a
    /// unit sends back (those of its files as of this read).
    fn rows_of<'a>(&self, items: impl Iterator<Item = &'a ListedItem>) -> Vec<ListedItem> {
        let l = &self.loaded;
        let mut hashes: BTreeMap<usize, (String, String)> = BTreeMap::new();
        items
            .map(|it| {
                let mut it = it.clone();
                if it.item.kind == Kind::Unit && it.item.unit.is_some() {
                    if let Some(f) = self.file_index(&it.item.path) {
                        let h = hashes.entry(f).or_insert_with(|| {
                            (
                                review::file_hash(&l.inputs[f].text),
                                review::file_hash(&l.tree.conversions[f].output),
                            )
                        });
                        it.hashes = Some(h.clone());
                    }
                }
                it
            })
            .collect()
    }

    /// Every item of the queue as the page lists it (tier 6 with `all`).
    pub fn items(&self, all: bool) -> Vec<ListedItem> {
        self.rows_of(self.listed_all().iter().filter(|i| all || i.item.tier != 6))
    }

    /// Runs `f` on the listing of `q`, made once per query and read.
    fn with_listing<R>(&self, q: &Query, f: impl FnOnce(&Listing, &[ListedItem]) -> R) -> R {
        let items = self.listed_all();
        let mut cached = self
            .loaded
            .listing
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if cached.as_ref().map_or(true, |l| l.query != *q) {
            *cached = Some(review_list::list(items, q));
        }
        f(cached.as_ref().expect("listed"), items)
    }

    /// `/api/list`: the groups of a lens, `count` from `from`.
    pub fn list(&self, q: &Query, from: usize, count: usize) -> ListView {
        let total = self
            .listed_all()
            .iter()
            .filter(|i| q.tiers.contains(&i.item.tier))
            .count();
        self.with_listing(q, |l, _| ListView {
            total,
            shown: l.shown,
            groups_total: l.groups.len(),
            groups: l
                .groups
                .iter()
                .skip(from)
                .take(count.min(PAGE_MAX))
                .map(|g| GroupView {
                    key: g.key.clone(),
                    n: g.items.len(),
                    desc: g.desc.clone(),
                    accept: g.accept,
                })
                .collect(),
        })
    }

    /// `/api/rows`: `count` items of the group `key` from `from`.
    pub fn rows(&self, q: &Query, key: &str, from: usize, count: usize) -> Option<RowsView> {
        self.with_listing(q, |l, items| {
            let g = l.group(key)?;
            Some(RowsView {
                n: g.items.len(),
                rows: self.rows_of(
                    g.items
                        .iter()
                        .skip(from)
                        .take(count.min(PAGE_MAX))
                        .map(|&i| &items[i as usize]),
                ),
            })
        })
    }

    /// `/api/accept`: the units of the group `key` with `key` as an open
    /// assumption, which "accept for all" selects.
    pub fn acceptable(&self, q: &Query, key: &str) -> Vec<ListedItem> {
        self.with_listing(q, |l, items| {
            let Some(g) = l.group(key) else {
                return Vec::new();
            };
            self.rows_of(
                g.items
                    .iter()
                    .map(|&i| &items[i as usize])
                    .filter(|i| i.open_assumptions.iter().any(|r| r == key)),
            )
        })
    }

    /// `/api/near`: where an item is listed (`at`), the one `step` on
    /// from it, the first of the group `group` on from its, or the one at
    /// `index` of the walk order.
    pub fn near(&self, q: &Query, how: Near<'_>) -> Option<Place> {
        self.with_listing(q, |l, items| match how {
            Near::At(key) => l.find(items, key),
            Near::Step(key, d) => l.step(items, key, d),
            Near::Group(key, d) => l.step_group(items, key, d),
            Near::Index(i) => l.at(items, i),
        })
    }

    /// `/api/reviewed`: `count` reviewed units from `from`, as the state
    /// lists them.
    pub fn reviewed(&self, from: usize, count: usize) -> Vec<ReviewedUnit> {
        let mut v = self.tally().0;
        v.drain(..from.min(v.len()));
        v.truncate(count.min(PAGE_MAX));
        v
    }

    /// File index and unit index of a unit, by path and id.
    fn find(&self, path: &str, unit: &str) -> Option<(usize, usize)> {
        self.with_id(unit)
            .find(|&(f, _)| self.loaded.inputs[f].rel == path)
    }

    /// File index of a path.
    fn file_index(&self, path: &str) -> Option<usize> {
        let l = &self.loaded;
        l.by_path
            .binary_search_by(|&f| l.inputs[f as usize].rel.as_str().cmp(path))
            .ok()
            .map(|i| l.by_path[i] as usize)
    }

    /// File and unit indices of the units with this id, in report order.
    fn with_id<'a>(&'a self, unit: &'a str) -> impl Iterator<Item = (usize, usize)> + 'a {
        let l = &self.loaded;
        let id = |&(f, k): &(u32, u32)| l.ids[f as usize][k as usize].as_str();
        let start = l.by_id.partition_point(|e| id(e) < unit);
        l.by_id[start..]
            .iter()
            .take_while(move |e| id(e) == unit)
            .map(|&(f, k)| (f as usize, k as usize))
    }

    fn unit_review(&self, f: usize, id: &str) -> Option<&UnitReviewReport> {
        let fr = &self.loaded.tree.report.files[f];
        fr.units
            .iter()
            .chain(&fr.converted_units)
            .find(|u| u.id == id)
            .and_then(|u| u.review.as_ref())
    }

    /// Instances woven from `path`'s template, and those not converted.
    fn instances_of(&self, path: &str) -> (usize, Vec<(String, String)>) {
        let Some(i) = &self.loaded.instances else {
            return (0, Vec::new());
        };
        let n = i
            .templates
            .iter()
            .find(|t| t.path == path)
            .map_or(0, |t| t.instances);
        let failing = i
            .instances
            .iter()
            .filter(|x| x.status != "converted" && x.templates.iter().any(|t| t == path))
            .map(|x| (x.name.clone(), x.status.clone()))
            .collect();
        (n, failing)
    }

    pub fn unit(&self, path: &str, unit: &str) -> Option<UnitView> {
        let (f, k) = self.find(path, unit)?;
        let l = &self.loaded;
        let c = &l.tree.conversions[f];
        let u = &c.units[k];
        let fr = &l.tree.report.files[f];
        let ur = fr
            .units
            .iter()
            .chain(&fr.converted_units)
            .find(|x| x.id == unit)?;
        let review = ur.review.clone();
        let findings = u
            .findings
            .iter()
            .filter_map(|&i| c.report.findings.get(i).map(|x| ("diff", x)))
            .chain(
                u.output_findings
                    .iter()
                    .filter_map(|&i| c.output_check.findings.get(i).map(|x| ("output", x))),
            )
            .map(|(from, x)| FindingView {
                from,
                rule: x.rule.clone(),
                verdict: x.verdict,
                severity: x.severity,
                witness: x.witness.clone(),
                assumptions: x.assumptions.clone(),
            })
            .collect();
        let rules = |s: Status| -> Vec<String> {
            let mut v: Vec<String> = u
                .notes
                .iter()
                .filter(|n| n.status == s)
                .map(|n| n.rule.clone())
                .collect();
            v.dedup();
            v
        };
        let (instances, failing_instances) = self.instances_of(path);
        Some(UnitView {
            path: path.to_string(),
            unit: unit.to_string(),
            status: u.status,
            checked: u.checked,
            src: (u.src.line, u.src.end_line),
            out: (u.out.line, u.out.end_line),
            written_line: ur.out_line,
            notes: u
                .notes
                .iter()
                .map(|n| NoteView {
                    status: n.status,
                    rule: n.rule.clone(),
                    text: n.text.clone(),
                })
                .collect(),
            findings,
            approve_blocked: approve_blocked(review.as_ref(), u),
            review,
            input_hash: review::file_hash(&l.inputs[f].text),
            output_hash: review::file_hash(&c.output),
            assumptions: rules(Status::Assumptions),
            decisions: rules(Status::Decision),
            instances,
            failing_instances,
        })
    }

    pub fn item(&self, key: &str, all: bool) -> Option<ItemView> {
        let item = self
            .listed_all()
            .iter()
            .find(|i| i.item.key == key && (all || i.item.tier != 6))?
            .item
            .clone();
        let units = match (&item.kind, &item.unit) {
            (Kind::Unit, Some(id)) => self.unit(&item.path, id).into_iter().collect(),
            (Kind::MissingObject, _) => item
                .units
                .iter()
                .filter_map(|id| {
                    // The ids are unique per file; the item names only its first file.
                    self.with_id(id)
                        .next()
                        .and_then(|(f, _)| self.unit(&self.loaded.inputs[f].rel, id))
                })
                .collect(),
            _ => Vec::new(),
        };
        let failing_instances = if item.kind == Kind::Template {
            item.failing_instances.clone()
        } else {
            self.instances_of(&item.path).1
        };
        Some(ItemView {
            item,
            units,
            failing_instances,
        })
    }

    /// A whole input file (`side` `in`) or its conversion (`out`), as the
    /// unit lines count them.
    pub fn file(&self, path: &str, side: &str) -> Option<String> {
        let f = self.file_index(path)?;
        match side {
            "in" => Some(self.loaded.inputs[f].text.clone()),
            "out" => Some(self.loaded.tree.conversions[f].output.clone()),
            _ => None,
        }
    }

    /// Checks a request against the tree as it is now and appends its
    /// records, all or none. Reads everything again first and after.
    pub fn record(&mut self, req: RecordRequest, at: &str) -> Result<usize, String> {
        self.refresh()?;
        let records = self.build(&req, at)?;
        let path = review::ledger_file(&self.setup.out)?;
        let mut text = String::new();
        // A ledger written by hand may lack its final newline.
        if let Ok(old) = std::fs::read(&path) {
            if !old.is_empty() && !old.ends_with(b"\n") {
                text.push('\n');
            }
        }
        for r in &records {
            text.push_str(&serde_json::to_string(r).map_err(|e| e.to_string())?);
            text.push('\n');
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        file.write_all(text.as_bytes())
            .and_then(|_| file.flush())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        self.refresh()?;
        Ok(records.len())
    }

    fn build(&self, req: &RecordRequest, at: &str) -> Result<Vec<Record>, String> {
        let author = req.author.trim();
        if author.is_empty() {
            return Err("give your name first: every decision says who made it".into());
        }
        if req.reason.trim().is_empty() {
            return Err(match req.action {
                Action::Comment => "a comment needs text".into(),
                _ => "say why: every decision needs a reason".into(),
            });
        }
        if req.targets.is_empty() {
            return Err("no unit selected".into());
        }
        if req.targets.len() > 1 && req.action != Action::Accept {
            return Err(
                "only accepting an assumption can be done for several units at once".into(),
            );
        }
        let state = match req.action {
            Action::Accept => State::Accepted,
            Action::Pick => State::Picked,
            Action::Approve => State::Approved,
            Action::Reject => State::Rejected,
            Action::Comment => State::Commented,
        };
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for t in &req.targets {
            if !seen.insert((t.path.as_str(), t.unit.as_str())) {
                return Err(format!("{} {}: listed twice", t.path, t.unit));
            }
            let where_ = format!("{} ({})", t.path, t.unit);
            let (f, k) = self.find(&t.path, &t.unit).ok_or_else(|| {
                format!("{where_}: no such unit now; the file changed since the page showed it")
            })?;
            let input = &self.loaded.inputs[f].text;
            let c = &self.loaded.tree.conversions[f];
            let (ih, oh) = (review::file_hash(input), review::file_hash(&c.output));
            if ih != t.input_hash || oh != t.output_hash {
                return Err(format!(
                    "{where_}: the file changed since the page showed it; look at it again"
                ));
            }
            let u = &c.units[k];
            let rule = req.rule.trim();
            let note = |s: Status| u.notes.iter().any(|n| n.rule == rule && n.status == s);
            match req.action {
                Action::Accept if !note(Status::Assumptions) => {
                    return Err(format!(
                        "{where_}: `{rule}` is not an assumption of this unit"
                    ))
                }
                Action::Pick if !note(Status::Decision) => {
                    return Err(format!("{where_}: `{rule}` is not a decision of this unit"))
                }
                Action::Approve => {
                    if !rule.is_empty() {
                        return Err("an approval is about the whole unit and names no rule".into());
                    }
                    if let Some(why) = approve_blocked(self.unit_review(f, &t.unit), u) {
                        return Err(format!("{where_}: {why}"));
                    }
                }
                Action::Reject | Action::Comment
                    if !rule.is_empty() && !u.notes.iter().any(|n| n.rule == rule) =>
                {
                    return Err(format!("{where_}: the unit has no note `{rule}`"))
                }
                _ => {}
            }
            // Built from JSON so fields the ledger gains later keep their defaults.
            let r: Record = serde_json::from_value(serde_json::json!({
                "unit": t.unit,
                "path": t.path,
                "rule": rule,
                "state": state,
                "by": By::Human,
                "author": author,
                "reason": req.reason.trim(),
                "at": at,
                "input_hash": ih,
                "output_hash": oh,
                "base_catalogs": self.loaded.tree.report.base_hash,
            }))
            .map_err(|e| e.to_string())?;
            review::allowed(&r).map_err(|e| format!("{where_}: {e}"))?;
            out.push(r);
        }
        Ok(out)
    }
}

/// Assumptions of a unit nobody accepted yet (all of them without a review).
fn open_assumptions(u: &Unit, rv: Option<&UnitReviewReport>) -> Vec<String> {
    let mut v: Vec<String> = u
        .notes
        .iter()
        .filter(|n| n.status == Status::Assumptions)
        .map(|n| n.rule.clone())
        .filter(|r| rv.map_or(true, |rv| rv.open.contains(r)))
        .collect();
    v.dedup();
    v
}

/// An approval says the output is right as written; it cannot stand in for
/// fixing what the engine found, so it is refused while anything
/// undischargeable is on the unit.
fn approve_blocked(rv: Option<&UnitReviewReport>, u: &Unit) -> Option<String> {
    let left: Vec<String> = match rv {
        Some(rv) => rv.undischargeable.clone(),
        None if u.status >= Status::Unproven || u.checked != Status::Converted => {
            vec![u.status.label().to_string()]
        }
        None => Vec::new(),
    };
    (!left.is_empty()).then(|| {
        format!(
            "cannot be approved while {} is on it: an approval never discharges a difference, a refusal or what the engine could not prove; fix the output and convert again",
            left.join(", ")
        )
    })
}

/// The tree as [`stored_key`] read it, for [`Review::open_stored`].
pub struct Now {
    inputs: Vec<Input>,
    unreadable: Vec<String>,
    written: Vec<String>,
}

/// The key of a stored conversion of `s`'s tree as it is on disk now
/// (sources, the ledger's hand-written texts, base catalogs), with `extra`.
pub fn stored_key(s: &Setup, extra: &[u8]) -> Result<(String, Now), String> {
    let (inputs, unreadable) = tree::read(&s.tree)?;
    let written = written_texts(&s.out)?;
    let base = sqldiff_convert::base::hash(
        &sqldiff_convert::base::read(&s.tree.catalogs, s.tree.no_catalog)?,
        &sqldiff_convert::base::read_b(&s.tree.catalogs_b, &s.tree.catalogs, s.tree.no_catalog)?,
    );
    let key = key_of(extra, &inputs, &unreadable, &written, base.as_deref());
    Ok((
        key,
        Now {
            inputs,
            unreadable,
            written,
        },
    ))
}

/// SHA-256 over every part, each with its length, so no two lists of parts
/// run together.
fn key_of(
    extra: &[u8],
    inputs: &[Input],
    unreadable: &[String],
    written: &[String],
    base: Option<&str>,
) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    let mut part = |b: &[u8]| {
        h.update((b.len() as u64).to_le_bytes());
        h.update(b);
    };
    part(extra);
    part(&(inputs.len() as u64).to_le_bytes());
    for i in inputs {
        part(i.rel.as_bytes());
        part(i.text.as_bytes());
    }
    part(&(unreadable.len() as u64).to_le_bytes());
    for u in unreadable {
        part(u.as_bytes());
    }
    part(&(written.len() as u64).to_le_bytes());
    for w in written {
        part(w.as_bytes());
    }
    part(&[base.is_some() as u8]);
    part(base.unwrap_or("").as_bytes());
    format!("{:x}", h.finalize())
}

fn empty() -> Loaded {
    Loaded {
        inputs: Vec::new(),
        unreadable: Vec::new(),
        tree: Tree {
            report: tree::Report {
                from: String::new(),
                to: String::new(),
                src: String::new(),
                out: None,
                catalogs: false,
                templates: false,
                base_catalogs: Vec::new(),
                base_catalogs_b: Vec::new(),
                base_hash: None,
                base_objects: Vec::new(),
                base_statements: Vec::new(),
                files: Vec::new(),
                unreadable: Vec::new(),
                totals: Default::default(),
                ledger: None,
                deployment: None,
            },
            outputs: Vec::new(),
            conversions: Vec::new(),
            base_ddl: Default::default(),
        },
        ids: Vec::new(),
        by_path: Vec::new(),
        by_id: Vec::new(),
        listed: OnceLock::new(),
        listing: Mutex::new(None),
        instances: None,
        not_applied: Vec::new(),
        out_differs: Vec::new(),
        convert_ms: 0.0,
        converted: false,
        written: Vec::new(),
        gate: Err("not judged yet".into()),
        judged: String::new(),
        stamp: Vec::new(),
    }
}

/// Reads and converts the tree (reusing `prev`'s conversion when no source
/// changed), applies the ledger in OUT and reads the instance report.
fn load(s: &Setup, prev: Option<Loaded>, hooks: &tree::Hooks) -> Result<Loaded, String> {
    // Taken before anything is read: a file that changes while this read
    // judges makes the verdict stale.
    let stamp = stamp(s);
    let instances = match &s.instances {
        Some(p) => Some(queue::read_instances(p)?),
        None => None,
    };
    let (inputs, unreadable) = tree::read(&s.tree)?;
    let written = written_texts(&s.out)?;
    // Base catalogs go into the conversion too: changed contents, convert again.
    let base = sqldiff_convert::base::hash(
        &sqldiff_convert::base::read(&s.tree.catalogs, s.tree.no_catalog)?,
        &sqldiff_convert::base::read_b(&s.tree.catalogs_b, &s.tree.catalogs, s.tree.no_catalog)?,
    );
    let same = prev.as_ref().is_some_and(|p| {
        p.written == written
            && p.tree.report.base_hash == base
            && p.unreadable == unreadable
            && p.inputs.len() == inputs.len()
            && p.inputs
                .iter()
                .zip(&inputs)
                .all(|(a, b)| a.rel == b.rel && a.text == b.text)
    });
    let (mut t, convert_ms, converted) = match prev {
        Some(p) if same => {
            let mut t = p.tree;
            clear_review(&mut t);
            (t, p.convert_ms, false)
        }
        _ => {
            let started = Instant::now();
            let t = tree::convert_with(&s.tree, &inputs, unreadable.clone(), Some(&s.out), hooks)?;
            (t, started.elapsed().as_secs_f64() * 1000.0, true)
        }
    };
    // A conversion handed over (a run's, a stored one) may not be trimmed yet.
    trim(&mut t);
    let not_applied = tree::review_in(&mut t, &inputs, &s.out)?;
    let gate = judge(&mut t, &inputs, s, instances.as_ref()).map(ShownGate::from);
    let ids: Vec<Vec<String>> = inputs
        .iter()
        .zip(&t.conversions)
        .map(|(i, c)| review::unit_ids(&i.rel, &i.text, c))
        .collect();
    let mut by_path: Vec<u32> = (0..inputs.len() as u32).collect();
    by_path.sort_by(|&a, &b| inputs[a as usize].rel.cmp(&inputs[b as usize].rel));
    let mut by_id: Vec<(u32, u32)> = ids
        .iter()
        .enumerate()
        .flat_map(|(f, v)| (0..v.len() as u32).map(move |k| (f as u32, k)))
        .collect();
    // Stable: the same id stays in report order.
    by_id.sort_by(|&(f, k), &(g, j)| ids[f as usize][k as usize].cmp(&ids[g as usize][j as usize]));
    let out_differs = t
        .report
        .files
        .iter()
        .zip(&t.outputs)
        .filter(|(f, text)| {
            std::fs::read_to_string(s.out.join(&f.path)).map_or(true, |on_disk| on_disk != **text)
        })
        .map(|(f, _)| f.path.clone())
        .collect();
    Ok(Loaded {
        inputs,
        unreadable,
        tree: t,
        ids,
        by_path,
        by_id,
        instances,
        not_applied,
        out_differs,
        convert_ms,
        converted,
        written,
        gate,
        judged: sqldiff_review::time::now(),
        stamp,
        listed: OnceLock::new(),
        listing: Mutex::new(None),
    })
}

/// Drops what nothing reads of a conversion once it is converted: of the
/// engine's two reports per file (its diff, and the output's own check)
/// only the findings stay, which units point into and the unit view shows;
/// the statuses, notes and report were taken from the rest already. On a
/// large tree this is a fifth of what the review holds. Done before the
/// ledger is applied and the gate judged, which reuse the memory. A stored
/// conversion is stored trimmed: reopening it reads what this one does.
fn trim(t: &mut Tree) {
    for c in &mut t.conversions {
        for r in [&mut c.report, &mut c.output_check] {
            r.witnesses = Vec::new();
            r.diagnostics = Vec::new();
            r.normalizations = Vec::new();
            r.assumptions = Vec::new();
            r.unknown_reasons = Vec::new();
            r.discharged = Vec::new();
            r.suppressed = Vec::new();
            r.refinements = Vec::new();
            r.proof = None;
        }
    }
}

/// Back to the conversion as it came from the engine: no ledger applied.
fn clear_review(t: &mut Tree) {
    for f in &mut t.report.files {
        f.review = None;
        for u in f.units.iter_mut().chain(f.converted_units.iter_mut()) {
            u.review = None;
        }
    }
    t.report.ledger = None;
}

/// The done gate of `t` (the ledger in OUT applied by `tree::review_in`),
/// judged as `sqldiff review-status` judges it: without a ledger, with an
/// empty one, which is taken off again afterwards.
fn judge(
    t: &mut Tree,
    inputs: &[Input],
    s: &Setup,
    inst: Option<&Instances>,
) -> Result<Gate, String> {
    let none = t.report.ledger.is_none();
    if none {
        tree::review(t, inputs, &review::Ledger::default(), "(none)");
    }
    let g = gate::check(t, &s.tree, &s.out, inst);
    if none {
        clear_review(t);
    }
    g
}

/// The ledger's hand-written texts (every `written` record with a text),
/// in ledger order: what `tree::convert` reads from the ledger (which of
/// equal texts it credits follows that order).
fn written_texts(out: &std::path::Path) -> Result<Vec<String>, String> {
    let path = out.join(review::LEDGER_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let v: Vec<String> = review::Ledger::parse(&text)
        .records()
        .filter(|r| r.state == State::Written && r.text.is_some())
        .map(|r| serde_json::to_string(r).unwrap_or_default())
        .collect();
    Ok(v)
}

/// The page with a payload embedded.
pub fn page(state: &StateView) -> String {
    // Serialized straight from the state: a `serde_json::Value` of a large
    // tree's queue takes many times the memory of its text.
    #[derive(Serialize)]
    struct DialectView {
        id: &'static str,
        name: &'static str,
        lex: crate::convert::LexInfo,
    }
    #[derive(Serialize)]
    struct Payload<'a> {
        state: &'a StateView,
        dialects: Vec<DialectView>,
        keywords: Vec<&'static str>,
    }
    let payload = Payload {
        state,
        dialects: sqldiff_core::Dialect::ALL
            .iter()
            .map(|d| DialectView {
                id: d.id(),
                name: d.display_name(),
                lex: crate::convert::LexInfo::of(*d),
            })
            .collect(),
        keywords: crate::styled_keywords(),
    };
    let json = serde_json::to_string(&payload).unwrap();
    let html = REVIEW_HTML.replace("/*__SQLDIFF_LEXER__*/", lexer());
    let (head, tail) = html
        .split_once("/*__SQLDIFF_PAYLOAD__*/null")
        .expect("payload marker in review.html");
    let mut out = String::with_capacity(html.len() + json.len() + json.len() / 64);
    out.push_str(head);
    out.push_str("/*__SQLDIFF_PAYLOAD__*/");
    // `</` would end the script.
    let mut rest = json.as_str();
    while let Some(i) = rest.find("</") {
        out.push_str(&rest[..i]);
        out.push_str("<\\/");
        rest = &rest[i + 2..];
    }
    out.push_str(rest);
    out.push_str(tail);
    out
}

/// Convert mode's highlighter (`lexSql`, `hlLines` and what they need), so
/// both pages colour SQL the same way; `tests/highlight.rs` checks it
/// against the engine.
fn lexer() -> &'static str {
    let page = crate::convert::CONVERT_HTML;
    let start = page
        .find("const KEYWORDS = ")
        .expect("lexer in convert.html");
    let end = page[start..]
        .find("\nfunction render(")
        .map(|e| start + e)
        .expect("end of the lexer in convert.html");
    &page[start..end]
}

fn num(req: &Request, key: &str, or: usize) -> usize {
    param(&req.query, key)
        .and_then(|v| v.parse().ok())
        .unwrap_or(or)
}

fn param(query: &str, key: &str) -> Option<String> {
    crate::server::param(query, key)
}

pub(crate) fn route(r: &Mutex<Review>, req: &Request) -> Response {
    let all = param(&req.query, "all").is_some_and(|v| v != "0");
    let mut s = r.lock().unwrap_or_else(|e| e.into_inner());
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => ("200 OK", "text/html; charset=utf-8", page(&s.state(all))),
        ("GET", "/api/state") => json(&s.state(all)),
        ("GET", "/api/gate") => json(&s.gate_state()),
        ("GET", "/api/list") => {
            let q = Query::parse(&req.query, all);
            json(&s.list(&q, num(req, "from", 0), num(req, "count", 200)))
        }
        ("GET", "/api/rows") => {
            let q = Query::parse(&req.query, all);
            let key = param(&req.query, "key").unwrap_or_default();
            match s.rows(&q, &key, num(req, "from", 0), num(req, "count", 200)) {
                Some(v) => json(&v),
                None => not_found(),
            }
        }
        ("GET", "/api/accept") => {
            let q = Query::parse(&req.query, all);
            json(&s.acceptable(&q, &param(&req.query, "key").unwrap_or_default()))
        }
        ("GET", "/api/near") => {
            let q = Query::parse(&req.query, all);
            let from = param(&req.query, "from").unwrap_or_default();
            let d = |k| param(&req.query, k).and_then(|v| v.parse::<i64>().ok());
            // `jump`: `group` names the grouping.
            let how = match (d("step"), d("jump"), d("index")) {
                (Some(n), _, _) => Near::Step(&from, n),
                (_, Some(n), _) => Near::Group(&from, n),
                (_, _, Some(i)) => Near::Index(i.max(0) as usize),
                _ => Near::At(&from),
            };
            // Nothing shown: `null`.
            json(&s.near(&q, how))
        }
        ("GET", "/api/reviewed") => json(&s.reviewed(num(req, "from", 0), num(req, "count", 200))),
        ("GET", "/api/item") => match param(&req.query, "key").and_then(|k| s.item(&k, all)) {
            Some(v) => json(&v),
            None => not_found(),
        },
        ("GET", "/api/unit") => match (param(&req.query, "path"), param(&req.query, "unit")) {
            (Some(p), Some(u)) => match s.unit(&p, &u) {
                Some(v) => json(&v),
                None => not_found(),
            },
            _ => bad("give path and unit"),
        },
        ("GET", "/api/file") => match (param(&req.query, "path"), param(&req.query, "side")) {
            (Some(p), Some(side)) => match s.file(&p, &side) {
                Some(t) => ("200 OK", "text/plain; charset=utf-8", t),
                None => not_found(),
            },
            _ => bad("give path and side=in|out"),
        },
        ("POST", "/api/record") => match serde_json::from_slice::<RecordRequest>(&req.body) {
            Ok(rq) => match s.record(rq, &sqldiff_review::time::now()) {
                Ok(written) => json(&RecordOutcome {
                    written,
                    state: s.state(all),
                }),
                Err(e) => ("409 Conflict", "text/plain", e),
            },
            Err(e) => bad(e),
        },
        ("POST", "/api/refresh") => match s.refresh() {
            Ok(()) => json(&s.state(all)),
            Err(e) => ("500 Internal Server Error", "text/plain", e),
        },
        _ => not_found(),
    }
}

pub fn serve(listener: TcpListener, review: Review, extra_hosts: &[String]) -> std::io::Result<()> {
    let review = Arc::new(Mutex::new(review));
    let hosts: Arc<Vec<String>> = Arc::new(allowed_hosts(&listener, extra_hosts)?);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let review = Arc::clone(&review);
        let hosts = Arc::clone(&hosts);
        std::thread::Builder::new()
            .stack_size(sqldiff_core::ENGINE_STACK)
            .spawn(move || {
                let _ = handle(stream, &review, &hosts);
            })?;
    }
    Ok(())
}

fn handle(mut stream: TcpStream, review: &Mutex<Review>, hosts: &[String]) -> std::io::Result<()> {
    let req = match read_request(&stream, hosts)? {
        Ok(r) => r,
        Err(res) => return respond(&mut stream, res),
    };
    let res = route(review, &req);
    respond(&mut stream, res)
}
