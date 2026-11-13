//! Local stdio MCP server for SQLDiff (design section 6.1).
//!
//! Speaks JSON-RPC 2.0 over newline-delimited stdio. Tools are coarse,
//! deterministic and token-bounded: `diff` and `check` return a verdict and
//! compact finding cards; `finding` returns one card in full from this
//! session's results; `queue` ranks every pair of a source for review;
//! `assumptions` and `set_assumption` show and flip the assumption ledger
//! (session-local, verdicts are re-derived); `comment_text` drafts a review
//! comment from the deterministic phrase grammar; `open_in_gui` gives a deep
//! link. `convert`, `convert_tree` and `converted_file` run the conversion
//! engine of `sqldiff convert` (`sqldiff_convert::tree`), so statuses and
//! reasons are the CLI's. Findings accepted in the review store, or marked
//! as deliberate in source, are suppressed and counted. The server only
//! reads files and the review store; it writes only when `convert_tree` is
//! given an output directory, exactly what `sqldiff convert -o` writes.

// Determinism guard (perf plan W0): iterating a hash map or set in a `for` loop is
// as order-dependent as `.iter()`; each site needs a reasoned `#[allow]`.
#![warn(clippy::iter_over_hash_type)]
#![recursion_limit = "512"]

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};
use sqldiff_convert::tree;
use sqldiff_core::ir::behaviour::RERUN_ASSUMPTIONS;
use sqldiff_core::ir::witness::{SideOutcome, WhatIf};
use sqldiff_core::ir::ErrorKind;
use sqldiff_core::model::{Assumption, Location, Side};
use sqldiff_core::phrase;
use sqldiff_core::{Artifact, Dialect, Finding, Options, Report, TOOL_VERSION};
use sqldiff_review::{markers, Lookup, Store};
use sqldiff_source::protocol::{Pair, PairScope};
use sqldiff_source::queue::{self, Filter, Flips, PairReport};
use sqldiff_source::{policy, Resolved, SourceSpec};

/// Protocol revisions this server understands, newest first.
pub const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// Default cap on finding cards per `diff` or `check` result.
const DEFAULT_MAX_FINDINGS: usize = 20;
/// Default cap on `queue` rows.
const DEFAULT_QUEUE_LIMIT: usize = 25;
/// Pairs kept for later lookups before the session cache is reset.
const CACHE_LIMIT: usize = 5_000;
/// Default cap on `convert_tree` file rows.
const DEFAULT_TREE_LIMIT: usize = 25;
/// Default cap on unit rows per file in `convert_tree`.
const DEFAULT_TREE_UNITS: usize = 5;
/// Converted trees kept for `converted_file`; the oldest goes first.
const TREE_LIMIT: usize = 4;

const INSTRUCTIONS: &str = "SQLDiff compares SQL artifacts in T-SQL (SQL Server 2022), \
PostgreSQL 18 and Db2 for i. Verdicts are deterministic and never a false \"equal\": \
\"equal_under_assumptions\" lists the assumptions it relies on, and \"unknown\" says why \
equivalence could not be shown. Use `queue` to rank every pair of a source for review, `diff` \
for two versions of one object or one object in two dialects, `check` to lint a single file and \
`finding` to expand a card by fingerprint. `convert` converts one file or inline text (T-SQL to \
PostgreSQL or Db2 for i) and `convert_tree` a whole tree, exactly as `sqldiff convert`: each unit \
is converted, under assumptions, needs a decision, not proven, differs or not converted, with the rules and \
reason; `converted_file` shows one file of a converted tree. `assumptions` lists what a verdict relies on and \
`set_assumption` flips one for this session only. `comment_text` drafts a plain-text review \
comment and `open_in_gui` gives a deep link for a human. `what_if` shows both sides' outcome \
when one statement fails, and `overlay` the transaction brackets of both sides.";

/// A diffed pair (or checked artifact) kept for later lookups.
struct Held {
    /// The pair as diffed, before session flips.
    pair: PairReport,
    /// Files the GUI can open, when both sides came from disk.
    files: Option<(String, Option<String>)>,
    /// The artifacts of a `diff`, kept so scenario flips can re-run it and
    /// `what_if` can answer on it.
    artifacts: Option<(Artifact, Artifact)>,
    /// Options the pair was diffed with (excerpt cap, catalogs, declared facts).
    opts: Option<Options>,
    /// The pair re-diffed under the session's scenario flips, when any are
    /// set and the artifacts are held.
    rerun: Option<Report>,
}

pub struct Server {
    config: Resolved,
    gui_url: String,
    flips: Flips,
    pairs: BTreeMap<String, Held>,
    /// Fingerprint → pair id.
    findings: BTreeMap<String, String>,
    /// Read-only; re-read before each analysis so decisions made in the CLI
    /// or GUI show up.
    store: Store,
    /// Converted trees by id, oldest first.
    trees: Vec<(String, tree::Tree)>,
    /// Trees converted in this session, for ids.
    tree_count: usize,
    /// Resolves the configuration again (`reload_config`); set by the
    /// binary, which knows where the configuration comes from.
    reload: Option<Reloader>,
    /// Background `convert_tree` runs, oldest first.
    runs: Vec<Run>,
    run_count: usize,
}

/// Resolves the configuration again from the sources the server started with.
pub type Reloader = Box<dyn Fn() -> Result<Resolved, String>>;

/// What a background run returns: the tree, and the report files written.
type RunResult = Result<(tree::Tree, Option<(std::path::PathBuf, std::path::PathBuf)>), String>;

/// A `convert_tree` running on its own thread. Collected by `run_status`,
/// which reports it exactly as the foreground call would.
struct Run {
    id: String,
    args: Value,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// (checked, total) files, from the engine's progress hook.
    progress: std::sync::Arc<(
        std::sync::atomic::AtomicUsize,
        std::sync::atomic::AtomicUsize,
    )>,
    /// (path, status label) of each file whose own check has finished, in
    /// the order the worker threads finished them.
    files: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
    handle: Option<std::thread::JoinHandle<RunResult>>,
    /// The final answer once collected: reported the same on every call.
    done: Option<(String, Value)>,
}

/// Runs kept in a session, finished ones included.
const RUN_LIMIT: usize = 8;

/// The client went away (end of input) or the server is dropped: a run
/// still working is asked to stop and waited for. It stops before its next
/// check, or finishes writing its output tree if it had begun; the process
/// never exits halfway through a write, which would leave a cut-off report
/// in `out` that the next conversion there cannot read.
impl Drop for Server {
    fn drop(&mut self) {
        for r in &self.runs {
            r.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        for r in &mut self.runs {
            if let Some(h) = r.handle.take() {
                let _ = h.join();
            }
        }
    }
}

impl Default for Server {
    fn default() -> Self {
        Self::build(Resolved::builtin(), None, Store::in_memory())
    }
}

impl Server {
    /// A server with built-in configuration only.
    pub fn new() -> Self {
        Self::default()
    }

    /// A server with resolved configuration (packs, excerpt cap, source,
    /// review store in the data directory) and the GUI address deep links
    /// point at.
    pub fn with_config(config: Resolved, gui_url: Option<String>) -> Result<Self, String> {
        let store = Store::from_config(&config).map_err(|e| e.0)?;
        Ok(Self::build(config, gui_url, store))
    }

    /// Lets `reload_config` resolve the configuration again.
    pub fn with_reloader(mut self, reload: Reloader) -> Self {
        self.reload = Some(reload);
        self
    }

    fn build(config: Resolved, gui_url: Option<String>, store: Store) -> Self {
        Server {
            config,
            gui_url: gui_url.unwrap_or_else(|| queue::DEFAULT_GUI_URL.to_string()),
            flips: Flips::new(),
            pairs: BTreeMap::new(),
            findings: BTreeMap::new(),
            store,
            trees: Vec::new(),
            tree_count: 0,
            reload: None,
            runs: Vec::new(),
            run_count: 0,
        }
    }

    /// Pack policy, author markers and review suppression on an engine report.
    fn finish(&self, r: &mut Report, a: &Artifact, b: Option<&Artifact>, now: &str) {
        policy::add_limits(r, a, b, &self.config);
        policy::apply(r, &self.config);
        markers::apply(r, &a.text, b.map(|b| b.text.as_str()), &self.config);
        sqldiff_review::suppress(r, &self.store, now);
    }

    fn reload_store(&mut self) -> Result<(), String> {
        self.store.reload().map_err(|e| e.0)
    }

    /// Handle one incoming JSON-RPC message (already parsed). Returns the
    /// response to write, or `None` for notifications.
    pub fn handle(&mut self, msg: &Value) -> Option<Value> {
        let Some(obj) = msg.as_object() else {
            return Some(error(Value::Null, -32600, "invalid request"));
        };
        let id = obj.get("id").cloned();
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            // A response from the client (we send no requests) or garbage.
            return id.map(|id| error(id, -32600, "invalid request: missing method"));
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => self.call(&params),
            m if m.starts_with("notifications/") => return None,
            m => Err((-32601, format!("method not found: {m}"))),
        };
        // Notifications get no response, whatever the method.
        let id = id?;
        Some(match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err((code, m)) => error(id, code, &m),
        })
    }

    /// Handle one line of input. Returns the line to write, if any.
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let resp = match serde_json::from_str::<Value>(line) {
            Ok(v) => self.handle(&v)?,
            Err(e) => error(Value::Null, -32700, &format!("parse error: {e}")),
        };
        Some(resp.to_string())
    }

    fn call(&mut self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((-32602, "tools/call needs a tool name".to_string()))?;
        let empty = Value::Object(Map::new());
        let args = params.get("arguments").unwrap_or(&empty);
        // An engine panic is reported as a tool error instead of ending the
        // server and every session result with it.
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.tool(name, args)))
            .unwrap_or_else(|p| {
                let what = p
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| p.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown cause".to_string());
                Some(Err(format!(
                    "internal error in `{name}`: {what}; please report it"
                )))
            })
            .ok_or_else(|| (-32602, format!("unknown tool: {name}")))?;
        // Bad tool input is reported in-band so the model can correct it.
        Ok(match out {
            Ok((text, structured)) => json!({
                "content": [{ "type": "text", "text": text }],
                "structuredContent": structured,
                "isError": false,
            }),
            Err(e) => json!({
                "content": [{ "type": "text", "text": e }],
                "isError": true,
            }),
        })
    }

    /// Runs a tool; `None` when there is no tool of that name.
    fn tool(&mut self, name: &str, args: &Value) -> Option<Result<(String, Value), String>> {
        // A run works on the store and the output tree: nothing else
        // converts while one does.
        if matches!(name, "convert_tree" | "conversion_queue" | "write_unit")
            && self
                .runs
                .iter()
                .any(|r| r.handle.as_ref().is_some_and(|h| !h.is_finished()))
        {
            return Some(Err(
                "a background run is working: `run_status` shows it, `cancel_run` stops it"
                    .to_string(),
            ));
        }
        Some(match name {
            "diff" => self.diff(args),
            "check" => self.check(args),
            "finding" => self.finding(args),
            "queue" => self.queue(args),
            "assumptions" => self.assumptions(args),
            "set_assumption" => self.set_assumption(args),
            "comment_text" => self.comment_text(args),
            "what_if" => self.what_if(args),
            "overlay" => self.overlay(args),
            "open_in_gui" => self.open_in_gui(args),
            "convert" => self.convert(args),
            "convert_tree" => self.convert_tree(args),
            "conversion_queue" => self.conversion_queue(args),
            "write_unit" => self.write_unit(args),
            "converted_file" => self.converted_file(args),
            "run_status" => self.run_status(args),
            "cancel_run" => self.cancel_run(args),
            "compare_runs" => self.compare_runs(args),
            "engine" => self.engine(args),
            "reload_config" => self.reload_config(args),
            _ => return None,
        })
    }

    fn options(&self, args: &Value) -> Options {
        let base = self.config.options();
        Options {
            excerpt_lines: args
                .get("excerpt_lines")
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                .unwrap_or(base.excerpt_lines),
            ..base
        }
    }

    /// Diffs a pair as held: raw (no session flips), with pack policy,
    /// author markers and review suppression.
    fn run_diff(&self, a: &Artifact, b: &Artifact, opts: &Options) -> Report {
        let mut report = sqldiff_core::diff(a, b, opts);
        self.finish(&mut report, a, Some(b), &sqldiff_review::time::now());
        report
    }

    fn diff(&mut self, args: &Value) -> Result<(String, Value), String> {
        let (a, fa) = artifact(args.get("a"), "a")?;
        let (b, fb) = artifact(args.get("b"), "b")?;
        let opts = self.options(args);
        self.reload_store()?;
        let report = self.run_diff(&a, &b, &opts);
        let files = fa.zip(fb).map(|(x, y)| (x, Some(y)));
        let id = self.hold(report, files);
        if let Some(h) = self.pairs.get_mut(&id) {
            h.artifacts = Some((a, b));
            h.opts = Some(opts);
        }
        self.refresh_rerun(&id);
        Ok(self.summarize(&id, args))
    }

    fn check(&mut self, args: &Value) -> Result<(String, Value), String> {
        let (a, fa) = artifact(args.get("artifact"), "artifact")?;
        let mut report = sqldiff_core::check(&a, &self.options(args));
        self.reload_store()?;
        self.finish(&mut report, &a, None, &sqldiff_review::time::now());
        let id = self.hold(report, fa.map(|x| (x, None)));
        Ok(self.summarize(&id, args))
    }

    /// Keeps a report for later lookups and returns its pair id. The id
    /// covers names and content, so two inline texts never share one.
    fn hold(&mut self, report: Report, files: Option<(String, Option<String>)>) -> String {
        let side = |i: &sqldiff_core::model::ArtifactInfo| format!("{}@{}", i.path, i.content_hash);
        let id = queue::pair_id(&side(&report.a), report.b.as_ref().map(side).as_deref());
        let (a, b) = (
            report.a.path.clone(),
            report
                .b
                .as_ref()
                .map(|b| b.path.clone())
                .unwrap_or_default(),
        );
        self.make_room(1);
        self.hold_pair(
            PairReport {
                id: id.clone(),
                pair: Pair {
                    a,
                    b,
                    key: String::new(),
                    rule: "diff".to_string(),
                },
                report,
                stale: false,
                last_changed: None,
            },
            files,
        );
        id
    }

    /// Resets the session cache if `n` more pairs would overflow it, so ids
    /// handed out by one call stay valid.
    fn make_room(&mut self, n: usize) {
        if self.pairs.len() + n > CACHE_LIMIT {
            self.pairs.clear();
            self.findings.clear();
        }
    }

    fn hold_pair(&mut self, pair: PairReport, files: Option<(String, Option<String>)>) {
        for f in &pair.report.findings {
            self.findings.insert(f.fingerprint.clone(), pair.id.clone());
        }
        self.pairs.insert(
            pair.id.clone(),
            Held {
                pair,
                files,
                artifacts: None,
                opts: None,
                rerun: None,
            },
        );
    }

    /// Re-diffs a held pair under the session's scenario flips (or drops
    /// the re-run when there are none) and registers its findings, so
    /// findings that only show under a flip can be looked up.
    fn refresh_rerun(&mut self, id: &str) {
        let scen = sqldiff_core::ledger::scenario_flips(&self.flips);
        let Some(h) = self.pairs.get(id) else { return };
        let r = match (&h.artifacts, &h.opts) {
            (Some((a, b)), Some(o)) if !scen.is_empty() => Some(self.run_diff(
                a,
                b,
                &Options {
                    flips: scen,
                    ..o.clone()
                },
            )),
            _ => None,
        };
        if let Some(r) = &r {
            for f in &r.findings {
                self.findings.insert(f.fingerprint.clone(), id.to_string());
            }
        }
        if let Some(h) = self.pairs.get_mut(id) {
            h.rerun = r;
        }
    }

    /// A held pair's report with session flips applied. Flips of scenario
    /// assumptions re-run the analysis when the pair's artifacts are held.
    fn view(&self, id: &str) -> Option<Report> {
        let h = self.pairs.get(id)?;
        Some(queue::view(
            h.rerun.as_ref().unwrap_or(&h.pair.report),
            &self.flips,
        ))
    }

    /// The pair a `pair` id or `fingerprint` argument names.
    fn pair_arg(&self, args: &Value) -> Result<String, String> {
        if let Some(p) = args.get("pair").and_then(Value::as_str) {
            return self
                .pairs
                .contains_key(p)
                .then(|| p.to_string())
                .ok_or_else(|| {
                    format!("no pair `{p}` in this session; run `queue` or `diff` first")
                });
        }
        let fp = args
            .get("fingerprint")
            .and_then(Value::as_str)
            .ok_or("give `pair` (from `queue` or `diff`) or a finding `fingerprint`")?;
        self.findings.get(fp).cloned().ok_or_else(|| {
            format!("no finding `{fp}` in this session; run `diff`, `check` or `queue` first")
        })
    }

    fn finding(&self, args: &Value) -> Result<(String, Value), String> {
        let fp = args
            .get("fingerprint")
            .and_then(Value::as_str)
            .ok_or("`fingerprint` is required")?;
        let (id, report) = self
            .findings
            .get(fp)
            .and_then(|id| Some((id, self.view(id)?)))
            .ok_or_else(|| {
                format!("no finding `{fp}` in this session; run `diff`, `check` or `queue` first")
            })?;
        let f = report
            .findings
            .iter()
            .find(|f| f.fingerprint == fp)
            .ok_or_else(|| format!("finding `{fp}` is gone from pair {id}"))?;
        let assumptions: Vec<&Assumption> = report
            .assumptions
            .iter()
            .filter(|x| f.assumptions.contains(&x.id))
            .collect();
        let pair = pair_label(&report);
        let mut text = format!(
            "{} · {} · {} · {}  {} {}\n{}\n{}\n",
            pair,
            f.class.label(),
            f.severity.label(),
            f.verdict.label(),
            f.rule,
            f.fingerprint,
            "-".repeat(40),
            f.witness
        );
        for loc in [&f.a, &f.b].into_iter().flatten() {
            text.push_str(&format!(
                "\n{:?}: {}:{}:{}-{}:{}\n",
                loc.side, loc.path, loc.start_line, loc.start_col, loc.end_line, loc.end_col
            ));
            for l in &loc.excerpt {
                text.push_str(&format!("  | {}\n", l.trim_end()));
            }
        }
        if !assumptions.is_empty() {
            text.push_str("\nAssumes:\n");
            for x in &assumptions {
                let flip = match self.flips.get(&x.id) {
                    Some(true) => " (confirmed in this session)",
                    Some(false) => " (denied in this session)",
                    None => "",
                };
                text.push_str(&format!("  {}  {}{flip}\n", x.id, x.text));
            }
        }
        let detail = report.witnesses.iter().find(|w| w.fingerprint == fp);
        if let Some(w) = detail {
            text.push_str(&format!("\n{}\n", w.scenario.text));
            text.push_str(&outcome_text("A", &w.a));
            text.push_str(&outcome_text("B", &w.b));
        }
        let structured = json!({
            "pair": pair,
            "pair_id": id,
            "finding": f,
            "assumptions": assumptions,
            "witness": detail,
        });
        Ok((text, structured))
    }

    /// Compact result: verdict, counts and up to `max_findings` cards, most
    /// severe first.
    fn summarize(&self, id: &str, args: &Value) -> (String, Value) {
        let report = self.view(id).expect("pair was just held");
        let max = args
            .get("max_findings")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_FINDINGS);
        let pair = pair_label(&report);

        let mut order: Vec<&Finding> = report.findings.iter().collect();
        order.sort_by_key(|f| std::cmp::Reverse((f.severity, f.class)));
        let shown = &order[..order.len().min(max)];
        let omitted = order.len() - shown.len();

        let side = |i: &sqldiff_core::model::ArtifactInfo| {
            json!({
                "path": i.path,
                "dialect": i.dialect,
                "dialect_version": i.dialect_version,
                "object": i.object,
                "content_hash": i.content_hash,
            })
        };
        let structured = json!({
            "pair_id": id,
            "verdict": report.open_verdict(),
            "verdict_before_suppression": report.verdict(),
            "suppressed": report.suppressed.iter().map(|x| json!({
                "fingerprint": x.finding.fingerprint,
                "kind": x.kind,
                "justification": x.justification,
            })).collect::<Vec<_>>(),
            "a": side(&report.a),
            "b": report.b.as_ref().map(side),
            "finding_count": report.findings.len(),
            "findings": shown.iter().map(|f| card(f)).collect::<Vec<_>>(),
            "omitted_findings": omitted,
            "noise": report.noise,
            "assumptions": report.assumptions,
            "unknown_reasons": report.unknown_reasons,
            "normalizations": report.normalizations,
            "diagnostics": report.diagnostics,
            "tool_version": report.tool_version,
        });

        let mut text = format!(
            "{pair}  [{id}]\nVerdict: {}\n{} finding(s)",
            report.open_verdict().label().to_uppercase(),
            report.findings.len()
        );
        if !report.suppressed.is_empty() {
            text.push_str(&format!(
                "; {} accepted or marked deliberate (suppressed)",
                report.suppressed.len()
            ));
        }
        if report.noise.cosmetic + report.noise.structural > 0 {
            text.push_str(&format!(
                "; hidden: {} cosmetic, {} structural",
                report.noise.cosmetic, report.noise.structural
            ));
        }
        text.push('\n');
        for f in shown {
            text.push_str(&format!(
                "\n[{}] {} · {} · {}  {}\n    {}\n",
                f.fingerprint,
                f.class.label(),
                f.severity.label(),
                f.verdict.label(),
                f.rule,
                f.witness
            ));
            for loc in [&f.a, &f.b].into_iter().flatten() {
                text.push_str(&format!("    {}\n", short_loc(loc)));
            }
            if !f.assumptions.is_empty() {
                text.push_str(&format!("    assumes: {}\n", f.assumptions.join(", ")));
            }
        }
        if omitted > 0 {
            text.push_str(&format!(
                "\n{omitted} lower-severity finding(s) omitted; raise `max_findings` to see them.\n"
            ));
        }
        if !report.assumptions.is_empty() {
            text.push_str("\nAssumptions:\n");
            for x in &report.assumptions {
                text.push_str(&format!("  {}  {}\n", x.id, x.text));
            }
        }
        for r in &report.unknown_reasons {
            text.push_str(&format!("Not proven: {r}\n"));
        }
        for d in &report.diagnostics {
            text.push_str(&format!(
                "parse {:?}:{}:{}: {}\n",
                d.side, d.line, d.col, d.message
            ));
        }
        (text, structured)
    }
}

impl Server {
    fn queue(&mut self, args: &Value) -> Result<(String, Value), String> {
        let spec = match args.get("source").and_then(Value::as_str) {
            Some(s) => SourceSpec::parse(s).map_err(|e| e.0)?,
            None => self
                .config
                .source
                .clone()
                .ok_or("give `source` (a directory, snapshot bundle, git:REPO[@REV] or adapter:NAME) or set [source] in sqldiff.toml")?,
        };
        let src = sqldiff_source::open(&spec, &self.config).map_err(|e| e.0)?;
        let scope = PairScope {
            a_dialect: str_arg(args, "dialect_a"),
            b_dialect: str_arg(args, "dialect_b"),
        };
        self.reload_store()?;
        let now = sqldiff_review::time::now();
        let run = queue::run_with(src.as_ref(), &self.config, &scope, &mut |r, a, b| {
            markers::apply(r, &a.text, Some(&b.text), &self.config);
            sqldiff_review::suppress(r, &self.store, &now);
        })
        .map_err(|e| e.0)?;
        let filter = Filter {
            classes: list_arg(args, "classes")?,
            min_severity: enum_arg(args, "min_severity")?,
            verdicts: list_arg(args, "verdicts")?,
            folder: str_arg(args, "folder"),
            review: enum_arg(args, "review")?,
        };
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_QUEUE_LIMIT);
        let root = match &spec {
            SourceSpec::Fs { root } => Some(root.clone()),
            _ => None,
        };
        let file = |id: &str| root.as_ref().map(|r| r.join(id).display().to_string());
        let lookup = Lookup {
            store: &self.store,
            now: now.clone(),
        };
        let rows = queue::entries(&run.pairs, &self.flips, &filter, &lookup);
        self.make_room(run.pairs.len());
        for p in run.pairs {
            let files = file(&p.pair.a)
                .zip(file(&p.pair.b))
                .map(|(a, b)| (a, Some(b)));
            self.hold_pair(p, files);
        }
        let shown = &rows[..rows.len().min(limit)];
        let leftovers = &run.pairing;
        let structured = json!({
            "source": run.source.source,
            "pair_count": rows.len(),
            "pairs": shown,
            "omitted_pairs": rows.len() - shown.len(),
            "unpaired": leftovers.unpaired,
            "ambiguous": leftovers.ambiguous,
            "no_dialect": leftovers.no_dialect,
            "missing": leftovers.missing,
            "flips": self.flips,
        });
        let mut text = format!(
            "{}: {} pair(s) match{}\n",
            run.source.source,
            rows.len(),
            if self.flips.is_empty() {
                String::new()
            } else {
                format!(" ({} assumption flip(s) applied)", self.flips.len())
            }
        );
        for e in shown {
            text.push_str(&format!(
                "\n[{}] {} · {} · {} finding(s)  {} ↔ {}\n",
                e.id,
                e.verdict.label().to_uppercase(),
                match (e.highest_class, e.highest_severity) {
                    (Some(c), Some(s)) => format!("{} {}", s.label(), c.label()),
                    _ => "no findings".to_string(),
                },
                e.findings,
                e.a,
                e.b
            ));
            let mut notes = Vec::new();
            if !e.verdict_flipping.is_empty() {
                notes.push(format!(
                    "verdict hinges on {}",
                    e.verdict_flipping.join(", ")
                ));
            }
            if e.review.unreviewed > 0 && e.review.unreviewed < e.findings {
                notes.push(format!("{} unreviewed", e.review.unreviewed));
            }
            if e.stale {
                notes.push("stale".to_string());
            }
            if let Some(t) = &e.last_changed {
                notes.push(format!("changed {t}"));
            }
            if !notes.is_empty() {
                text.push_str(&format!("    {}\n", notes.join(" · ")));
            }
        }
        if rows.len() > shown.len() {
            text.push_str(&format!(
                "\n{} more pair(s) omitted; raise `limit` or narrow the filter.\n",
                rows.len() - shown.len()
            ));
        }
        let n = leftovers.unpaired.len() + leftovers.ambiguous.len() + leftovers.missing.len();
        if n > 0 {
            text.push_str(&format!(
                "{n} object(s) unpaired, ambiguous or missing; see structured output.\n"
            ));
        }
        Ok((text, structured))
    }

    fn assumptions(&self, args: &Value) -> Result<(String, Value), String> {
        let id = self.pair_arg(args)?;
        let held = &self.pairs[&id];
        if let (Some((a, b)), Some(o)) = (&held.artifacts, &held.opts) {
            // Full ledger: sources, proven-from-source facts, and flips that re-run.
            let (report, ledger) =
                sqldiff_core::ledger::ledger_by(&self.flips, &o.declared, &mut |f| {
                    let opts = Options {
                        flips: f.clone(),
                        ..o.clone()
                    };
                    self.run_diff(a, b, &opts)
                });
            let mut text = format!(
                "{}  [{id}]\nVerdict: {}\n",
                pair_label(&report),
                report.verdict().label().to_uppercase()
            );
            if ledger.is_empty() {
                text.push_str("No assumptions.\n");
            }
            for e in &ledger {
                text.push_str(&format!("\n{}  {:?}\n    {}\n", e.id, e.status, e.text));
                if let Some(c) = &e.cite {
                    text.push_str(&format!("    {c}\n"));
                }
                if let (true, Some(v)) = (e.flips_verdict, e.verdict_if_flipped) {
                    text.push_str(&format!(
                        "    flipping it {} and makes the verdict {}\n",
                        if e.reruns {
                            "re-runs the analysis"
                        } else {
                            "relabels its findings"
                        },
                        v.label()
                    ));
                }
                if !e.findings.is_empty() {
                    text.push_str(&format!("    relied on by {}\n", e.findings.join(", ")));
                }
            }
            return Ok((
                text,
                json!({ "pair_id": id, "verdict": report.verdict(), "assumptions": ledger }),
            ));
        }
        let report = queue::view(&held.pair.report, &self.flips);
        let ledger = queue::ledger(&held.pair.report, &self.flips);
        let mut text = format!(
            "{}  [{id}]\nVerdict: {}\n",
            pair_label(&report),
            report.verdict().label().to_uppercase()
        );
        if ledger.is_empty() {
            text.push_str("No assumptions.\n");
        }
        for e in &ledger {
            text.push_str(&format!(
                "\n{}  {}\n    {}{}\n",
                e.id,
                status_label(e.status),
                e.text,
                if e.flips_verdict {
                    format!(
                        "\n    flipping it makes the verdict {}",
                        e.verdict_if_flipped.label()
                    )
                } else {
                    String::new()
                }
            ));
            if !e.findings.is_empty() {
                text.push_str(&format!("    relied on by {}\n", e.findings.join(", ")));
            }
        }
        Ok((
            text,
            json!({ "pair_id": id, "verdict": report.verdict(), "assumptions": ledger }),
        ))
    }

    fn set_assumption(&mut self, args: &Value) -> Result<(String, Value), String> {
        let aid = args
            .get("id")
            .and_then(Value::as_str)
            .ok_or("`id` is required")?
            .to_string();
        let holds = match args.get("holds") {
            None | Some(Value::Null) => None,
            Some(Value::Bool(b)) => Some(*b),
            Some(_) => return Err("`holds` must be true, false or null".to_string()),
        };
        let before: BTreeMap<String, _> = self
            .pairs
            .iter()
            .filter(|(_, h)| h.pair.report.assumptions.iter().any(|a| a.id == aid))
            .map(|(k, _)| (k.clone(), self.view(k).map(|r| r.open_verdict())))
            .collect();
        match holds {
            Some(h) => self.flips.insert(aid.clone(), h),
            None => self.flips.remove(&aid),
        };
        if RERUN_ASSUMPTIONS.contains(&aid.as_str()) {
            let ids: Vec<String> = self.pairs.keys().cloned().collect();
            for k in ids {
                self.refresh_rerun(&k);
            }
        }
        let mut changed = Vec::new();
        let mut affected = 0;
        for (k, h) in &self.pairs {
            if !h.pair.report.assumptions.iter().any(|a| a.id == aid) {
                continue;
            }
            affected += 1;
            let now = self.view(k).map(|r| r.open_verdict());
            if before.get(k).copied().flatten() != now {
                changed.push(json!({
                    "pair_id": k,
                    "pair": pair_label(&h.pair.report),
                    "from": before.get(k).copied().flatten(),
                    "to": now,
                }));
            }
        }
        let state = match holds {
            Some(true) => "confirmed",
            Some(false) => "denied",
            None => "reset to its source",
        };
        let mut text = format!(
            "{aid} {state} for this session; {affected} held pair(s) rely on it, {} verdict(s) changed.\n",
            changed.len()
        );
        for c in &changed {
            text.push_str(&format!(
                "  [{}] {}: {} → {}\n",
                c["pair_id"].as_str().unwrap_or(""),
                c["pair"].as_str().unwrap_or(""),
                c["from"].as_str().unwrap_or("?"),
                c["to"].as_str().unwrap_or("?"),
            ));
        }
        Ok((
            text,
            json!({ "id": aid, "holds": holds, "affected_pairs": affected, "changed": changed, "flips": self.flips }),
        ))
    }

    /// The artifacts and options of a held `diff` pair, or of `a` and `b` given inline.
    fn pair_artifacts(&self, args: &Value) -> Result<(Artifact, Artifact, Options), String> {
        if args.get("a").is_some() || args.get("b").is_some() {
            let (a, _) = artifact(args.get("a"), "a")?;
            let (b, _) = artifact(args.get("b"), "b")?;
            return Ok((
                a,
                b,
                Options {
                    flips: self.flips.clone(),
                    ..self.options(args)
                },
            ));
        }
        let id = self.pair_arg(args)?;
        let h = &self.pairs[&id];
        match (&h.artifacts, &h.opts) {
            (Some((a, b)), Some(o)) => Ok((
                a.clone(),
                b.clone(),
                Options {
                    flips: self.flips.clone(),
                    ..o.clone()
                },
            )),
            _ => Err(format!(
                "pair `{id}` came from `queue` or `check`; give `a` and `b` (or diff them first)"
            )),
        }
    }

    fn what_if(&self, args: &Value) -> Result<(String, Value), String> {
        let (a, b, opts) = self.pair_artifacts(args)?;
        let line = args
            .get("line")
            .and_then(Value::as_u64)
            .ok_or("`line` is required: the line of the statement")? as usize;
        let side = match args.get("side").and_then(Value::as_str) {
            None | Some("a") => Side::A,
            Some("b") => Side::B,
            Some(x) => return Err(format!("`side` must be a or b, not {x}")),
        };
        let kind: ErrorKind = match enum_arg(args, "error_kind")? {
            Some(k) => k,
            None => ErrorKind::Constraint,
        };
        let opts = Options {
            flips: self.flips.clone(),
            ..opts
        };
        let w = sqldiff_core::what_if(&a, &b, &opts, side, line, kind);
        Ok((
            what_if_text(&w),
            serde_json::to_value(&w).unwrap_or(Value::Null),
        ))
    }

    fn overlay(&self, args: &Value) -> Result<(String, Value), String> {
        let (a, b, _) = self.pair_artifacts(args)?;
        let o = sqldiff_core::overlay(&a, &b);
        let mut text = String::new();
        for (name, side, other) in [("A", &o.a, "B"), ("B", &o.b, "A")] {
            text.push_str(&format!("Side {name}:\n"));
            for (i, br) in side.iter().enumerate() {
                let partner = br
                    .partner
                    .map(|p| format!("  <-> {other}#{p}"))
                    .unwrap_or_default();
                text.push_str(&format!(
                    "  #{i} lines {}-{}  {}{partner}\n",
                    br.start_line, br.end_line, br.label
                ));
            }
        }
        Ok((text, serde_json::to_value(&o).unwrap_or(Value::Null)))
    }

    fn comment_text(&self, args: &Value) -> Result<(String, Value), String> {
        let ids: Vec<String> = args
            .get("finding_ids")
            .and_then(Value::as_array)
            .ok_or("`finding_ids` must be a list of fingerprints")?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or("fingerprints are strings")
            })
            .collect::<Result<_, _>>()?;
        if ids.is_empty() {
            return Err("`finding_ids` is empty".to_string());
        }
        let missing: Vec<&String> = ids
            .iter()
            .filter(|i| !self.findings.contains_key(*i))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "not in this session: {}; run `diff`, `check` or `queue` first",
                missing
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        // Group by pair, in the order pairs are first named.
        let mut order: Vec<String> = Vec::new();
        for i in &ids {
            let p = &self.findings[i];
            if !order.contains(p) {
                order.push(p.clone());
            }
        }
        let reports: Vec<(&String, Report)> = order
            .iter()
            .filter_map(|p| Some((p, self.view(p)?)))
            .collect();
        let denied: Vec<String> = self
            .flips
            .iter()
            .filter(|(_, holds)| !**holds)
            .map(|(k, _)| k.clone())
            .collect();
        // A fingerprint seen in several pairs belongs to the one it was last seen in.
        let groups: Vec<phrase::Group> = reports
            .iter()
            .map(|(p, r)| phrase::Group {
                label: pair_label(r),
                findings: r
                    .findings
                    .iter()
                    .filter(|f| {
                        ids.contains(&f.fingerprint) && &self.findings[&f.fingerprint] == *p
                    })
                    .collect(),
                assumptions: &r.assumptions,
                denied: &denied,
            })
            .collect();
        let style = phrase::Style {
            excerpt_lines: args
                .get("excerpt_lines")
                .and_then(Value::as_u64)
                .map(|n| (n as usize).min(self.config.excerpt_lines))
                .unwrap_or(1),
            fingerprints: args
                .get("fingerprints")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        };
        let text = phrase::comment(&groups, &style);
        Ok((text.clone(), json!({ "text": text, "pairs": order })))
    }

    fn open_in_gui(&self, args: &Value) -> Result<(String, Value), String> {
        let fp = args.get("finding_id").and_then(Value::as_str);
        let id = match fp {
            Some(fp) => self.findings.get(fp).cloned().ok_or_else(|| {
                format!("no finding `{fp}` in this session; run `diff`, `check` or `queue` first")
            })?,
            None => self.pair_arg(args)?,
        };
        let held = &self.pairs[&id];
        let (a, b) = held.files.clone().ok_or(
            "no file paths for this pair (inline text, or a git, bundle or adapter source), so the GUI cannot open it; diff files by `path` instead",
        )?;
        let Some(b) = b else {
            return Err("the GUI opens pairs; this is a single-file `check`, so diff it against another file first".to_string());
        };
        let base = args
            .get("gui_url")
            .and_then(Value::as_str)
            .unwrap_or(&self.gui_url);
        let url = queue::deep_link(base, &a, Some(&b), fp);
        let r = &held.pair.report;
        let command = format!(
            "sqldiff-gui serve --dialect-a {} --dialect-b {} {} {}",
            r.a.dialect,
            r.b.as_ref().map(|x| x.dialect.as_str()).unwrap_or(""),
            queue::shell_word(&a),
            queue::shell_word(&b)
        );
        let text = format!("{url}\nIf the GUI is not running: {command}\n");
        Ok((
            text,
            json!({ "url": url, "command": command, "pair_id": id }),
        ))
    }
}

/// Conversion: the engine of `sqldiff convert`, with its statuses, rules
/// and reasons passed through unchanged.
impl Server {
    /// `template: true` reads every file as a template in the loaded
    /// `[template]` syntax, as `sqldiff convert --template` does.
    fn template_mode(&self, args: &Value) -> Result<Option<tree::TemplateMode>, String> {
        if !args
            .get("template")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            if args.get("parameters").is_some() {
                return Err("`parameters` needs `template: true`".to_string());
            }
            return Ok(None);
        }
        if self.config.policy.template.is_none() {
            return Err("`template` needs a [template] syntax: start the server with --pack or a sqldiff.toml that has one".to_string());
        }
        let parameters: Vec<String> = list_arg(args, "parameters")?;
        tree::TemplateMode::configured(
            &self.config,
            (!parameters.is_empty()).then(|| parameters.into_iter().collect()),
        )
        .map(Some)
    }

    fn convert(&mut self, args: &Value) -> Result<(String, Value), String> {
        let v = args.get("artifact").and_then(Value::as_object).ok_or(
            "`artifact` must be an object with `dialect` (the source dialect) and `path` or `text`",
        )?;
        let from: Dialect = v
            .get("dialect")
            .and_then(Value::as_str)
            .ok_or("`artifact.dialect` is required (the source dialect: tsql, postgres or db2i)")?
            .parse()?;
        let to = target(args)?;
        let template = self.template_mode(args)?;
        let name = v.get("name").and_then(Value::as_str);
        let (src, inputs) = match (
            v.get("path").and_then(Value::as_str),
            v.get("text").and_then(Value::as_str),
        ) {
            (Some(p), None) => {
                // As `sqldiff convert FILE`: read (and decode) like a tree of one.
                let path = std::path::PathBuf::from(p);
                if !path.is_file() {
                    return Err(format!(
                        "artifact: {p}: not a file; use `convert_tree` for a directory"
                    ));
                }
                let (inputs, unreadable) = tree::collect(&path, &[])?;
                if let Some(u) = unreadable.first() {
                    return Err(format!("artifact: {u}: not UTF-8 or UTF-16 text"));
                }
                (path, inputs)
            }
            (None, Some(t)) => {
                let rel = name.unwrap_or("input.sql").to_string();
                let input = tree::Input {
                    rel: rel.clone(),
                    text: t.to_string(),
                };
                (
                    std::path::PathBuf::from(format!("{rel} (inline)")),
                    vec![input],
                )
            }
            _ => return Err("`artifact` needs exactly one of `path` or `text`".to_string()),
        };
        let a = tree::Args {
            rules: Default::default(),
            src,
            from,
            to,
            jobs: Some(1),
            no_catalog: false,
            recheck: Default::default(),
            template,
            extensions: Vec::new(),
            helpers: self.config.options().helpers,
            deployment: self.config.deployment.clone(),
            catalogs: Vec::new(),
            catalogs_b: Vec::new(),
        };
        let t = tree::convert(&a, &inputs, Vec::new(), None)?;
        let f = &t.report.files[0];
        let output = &t.outputs[0];
        let mut text = format!(
            "{}: {} → {}: {}\n",
            f.path,
            t.report.from,
            t.report.to,
            f.status.label().to_uppercase()
        );
        text.push_str(&counts_line(&f.counts));
        text.push_str(&units_text(f, usize::MAX));
        text.push_str(&format!("\nOutput:\n{output}"));
        if !output.ends_with('\n') {
            text.push('\n');
        }
        let structured = json!({
            "from": t.report.from,
            "to": t.report.to,
            "ok": t.report.ok(),
            "file": f,
            "output": output,
        });
        Ok((text, structured))
    }

    /// The tree arguments `convert_tree` and `conversion_queue` share.
    fn tree_args(&self, args: &Value) -> Result<tree::Args, String> {
        let src = str_arg(args, "src").ok_or("`src` is required (a directory or one file)")?;
        let from: Dialect = match args.get("from").and_then(Value::as_str) {
            Some(d) => d.parse()?,
            None => Dialect::Tsql,
        };
        Ok(tree::Args {
            rules: sqldiff_convert::RuleOptions {
                temporal_historyless: args
                    .get("temporal_historyless")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            },
            recheck: match str_arg(args, "recheck") {
                Some(r) => r.parse()?,
                None => Default::default(),
            },
            src: src.into(),
            from,
            to: target(args)?,
            jobs: args.get("jobs").and_then(Value::as_u64).map(|n| n as usize),
            no_catalog: args
                .get("no_catalog")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            template: self.template_mode(args)?,
            extensions: tree::extensions(list_arg(args, "extensions")?),
            helpers: self.config.options().helpers,
            deployment: self.config.deployment.clone(),
            catalogs: list_arg::<String>(args, "catalogs")?
                .into_iter()
                .map(Into::into)
                .collect(),
            catalogs_b: list_arg::<String>(args, "catalogs_b")?
                .into_iter()
                .map(Into::into)
                .collect(),
        })
    }

    fn conversion_queue(&mut self, args: &Value) -> Result<(String, Value), String> {
        use sqldiff_convert::queue;
        let a = self.tree_args(args)?;
        let out = str_arg(args, "out")
            .ok_or("`out` is required: the converted tree, where review-decisions.jsonl lives")?;
        let out = std::path::PathBuf::from(out);
        let inst = match str_arg(args, "instances") {
            Some(p) => Some(queue::read_instances(std::path::Path::new(&p))?),
            None => None,
        };
        let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
        let max_tier = args
            .get("max_tier")
            .and_then(Value::as_u64)
            .map_or(6, |n| n.clamp(1, 6) as u8);
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_TREE_LIMIT);
        // Converted again in memory, so decisions recorded since count; nothing is written.
        let (inputs, unreadable) = tree::read(&a)?;
        let mut t = build_tree(
            args,
            &a,
            &inputs,
            unreadable,
            Some(&out),
            &tree::Hooks::default(),
            false,
        )?;
        tree::review_in(&mut t, &inputs, &out)?;
        let q = queue::build(&t.report, inst.as_ref(), all);
        let listed: Vec<&queue::Item> = q
            .items
            .iter()
            .filter(|i| i.tier <= max_tier)
            .take(limit)
            .collect();
        let matched = q.items.iter().filter(|i| i.tier <= max_tier).count();
        let mut text = queue::summary(&q);
        if inst.is_none() {
            text.push_str(
                "No instance report given: templates are judged on their own text only.\n",
            );
        }
        for i in &listed {
            text.push_str(&format!(
                "[{}] tier {} {}{} {}{}: {}\n",
                i.key,
                i.tier,
                i.path,
                if i.out_line > 0 {
                    format!(":{}", i.out_line)
                } else {
                    String::new()
                },
                i.status,
                if i.rules.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", i.rules.join(", "))
                },
                i.reason.lines().next().unwrap_or("")
            ));
        }
        if matched > listed.len() {
            text.push_str(&format!(
                "{} more item(s); raise `limit` or lower `max_tier`.\n",
                matched - listed.len()
            ));
        }
        let structured = json!({
            "tiers": q.tiers,
            "reviewed": q.reviewed,
            "instances": q.instances,
            "items": listed,
            "omitted_items": matched - listed.len(),
            "ledger": t.report.ledger,
        });
        Ok((text, structured))
    }

    /// Records hand-written output for one unit as the agent's draft, then
    /// converts and writes the tree again so the text is checked like rule
    /// output. The record is always `by: agent`: this server never speaks
    /// for a person, and a person approves the result in the GUI or CLI.
    fn write_unit(&mut self, args: &Value) -> Result<(String, Value), String> {
        use sqldiff_convert::{queue, review};
        let a = self.tree_args(args)?;
        let out = std::path::PathBuf::from(
            str_arg(args, "out").ok_or("`out` is required: the converted tree to write into")?,
        );
        let path = str_arg(args, "path").ok_or("`path` is required")?;
        let unit = str_arg(args, "unit")
            .ok_or("`unit` is required (a unit id from `conversion_queue` or `convert_tree`)")?;
        let text = str_arg(args, "text").ok_or("`text` is required")?;
        let reason = str_arg(args, "reason")
            .ok_or("`reason` is required: why this text does what the source does")?;
        let author = str_arg(args, "author").unwrap_or_else(|| "agent".to_string());
        if !out.join("conversion-report.json").exists() {
            return Err(format!(
                "{} holds no converted tree: run `convert_tree` with `out` first",
                out.display()
            ));
        }
        // Refused before anything is recorded, so a refusal changes nothing.
        tree::prepare_out(&a.src, &out, true)?;
        tree::guard_hand_edits(&out, false)?;
        let (inputs, unreadable) = tree::read(&a)?;
        let t = build_tree(
            args,
            &a,
            &inputs,
            unreadable.clone(),
            Some(&out),
            &tree::Hooks::default(),
            false,
        )?;
        let r = tree::written_record(
            &t,
            &inputs,
            &path,
            &unit,
            &text,
            review::By::Agent,
            &author,
            &reason,
            &sqldiff_review::time::now(),
        )?;
        review::append(&out, &r)?;
        let mut t = build_tree(
            args,
            &a,
            &inputs,
            unreadable,
            Some(&out),
            &tree::Hooks::default(),
            false,
        )?;
        tree::review_in(&mut t, &inputs, &out)?;
        tree::write(&t, &out)?;
        queue::write(&queue::build(&t.report, None, false), &out)?;

        let f = t
            .report
            .files
            .iter()
            .find(|f| f.path == path)
            .ok_or_else(|| format!("`{path}` left the tree"))?;
        let k = t
            .report
            .files
            .iter()
            .position(|f| f.path == path)
            .unwrap_or(0);
        let c = &t.conversions[k];
        let ids = review::unit_ids(&path, &inputs[k].text, c);
        let now = c
            .units
            .iter()
            .zip(&ids)
            .find(|(u, _)| u.written.as_ref().is_some_and(|w| w.origin == r.unit));
        let structured = match now {
            Some((u, id)) => {
                let rep = f.units.iter().find(|x| &x.id == id);
                json!({
                    "recorded": r,
                    "applied": true,
                    "unit": id,
                    "status": u.status.label(),
                    "notes": u.notes.iter().map(|n| json!({"status": n.status.label(), "rule": n.rule, "text": n.text})).collect::<Vec<_>>(),
                    "findings": rep.map(|x| x.findings.clone()).unwrap_or_default(),
                    "file_status": f.status.label(),
                })
            }
            None => json!({
                "recorded": r,
                "applied": false,
                "file_status": f.status.label(),
            }),
        };
        let text = match now {
            Some((u, id)) => format!(
                "Recorded the text for {path} unit {} as an agent draft and converted again: the unit is now `{id}`, {}{}. It stays in the review queue at tier 3 or worse until a person approves it.\n",
                r.unit,
                u.status.label(),
                u.notes
                    .first()
                    .map(|n| format!(" ({}: {})", n.rule, n.text))
                    .unwrap_or_default()
            ),
            None => format!(
                "Recorded the text for {path} unit {}, but it did not apply on the new conversion (another author's text for the unit may conflict); the file is {}.\n",
                r.unit,
                f.status.label()
            ),
        };
        Ok((text, structured))
    }

    fn convert_tree(&mut self, args: &Value) -> Result<(String, Value), String> {
        let a = self.tree_args(args)?;
        if args
            .get("background")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return self.start_run(args, a);
        }
        let (t, written) = run_tree(args, &a, &tree::Hooks::default())?;
        self.report_tree(args, t, written)
    }

    /// Starts `convert_tree` on its own thread and returns at once with a
    /// run id. One run works at a time, and never two on the same `out`.
    /// The run stops only between checks (`cancel_run`), and writes its
    /// output tree only after its last check, so a stopped run changes
    /// nothing but the store.
    fn start_run(&mut self, args: &Value, a: tree::Args) -> Result<(String, Value), String> {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::Arc;
        if self
            .runs
            .iter()
            .any(|r| r.handle.as_ref().is_some_and(|h| !h.is_finished()))
        {
            return Err(
                "a run is still working: `run_status` shows it, `cancel_run` stops it".to_string(),
            );
        }
        // A finished run not yet collected holds its tree; collect it first.
        if self.runs.iter().any(|r| r.handle.is_some()) {
            return Err("a finished run is waiting: read it with `run_status` first".to_string());
        }
        if self.runs.len() >= RUN_LIMIT {
            self.runs.remove(0);
        }
        self.run_count += 1;
        let id = format!("run-{}", self.run_count);
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new((AtomicUsize::new(0), AtomicUsize::new(0)));
        let files: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>> = Default::default();
        let hooks = tree::Hooks {
            file_done: Some({
                let f = files.clone();
                Box::new(move |path, status| {
                    if let Ok(mut v) = f.lock() {
                        v.push((path.to_string(), status.to_string()));
                    }
                })
            }),
            cancel: Some(cancel.clone()),
            progress: Some({
                let p = progress.clone();
                Box::new(move |checked, total| {
                    p.0.store(checked, Ordering::Relaxed);
                    p.1.store(total, Ordering::Relaxed);
                })
            }),
        };
        let worker_args = args.clone();
        let handle = std::thread::Builder::new()
            .stack_size(sqldiff_core::ENGINE_STACK)
            .spawn(move || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_tree(&worker_args, &a, &hooks)
                }))
                .unwrap_or_else(|_| Err("the engine panicked".to_string()))
            })
            .map_err(|e| format!("could not start the run: {e}"))?;
        self.runs.push(Run {
            id: id.clone(),
            args: args.clone(),
            cancel,
            progress,
            files,
            handle: Some(handle),
            done: None,
        });
        let text = format!(
            "[{id}] started; poll `run_status` with run `{id}`, stop it with `cancel_run`.\n"
        );
        Ok((text, json!({ "run": id, "state": "running" })))
    }

    fn run_index(&self, args: &Value) -> Result<usize, String> {
        let id = str_arg(args, "run")
            .ok_or("`run` is required (an id from `convert_tree` with `background`)")?;
        self.runs
            .iter()
            .position(|r| r.id == id)
            .ok_or_else(|| format!("no run `{id}` in this session (the last {RUN_LIMIT} are kept)"))
    }

    /// The configuration layers and packs in effect, each with the hash of
    /// its content.
    fn config_state(&self) -> Value {
        json!({
            "layers": self.config.layers.iter().map(|l| json!({
                "kind": l.kind, "name": l.name, "version": l.version, "hash": l.hash,
            })).collect::<Vec<_>>(),
            "packs": self.config.packs.iter().map(|p| json!({
                "path": p.path, "hash": p.hash,
            })).collect::<Vec<_>>(),
        })
    }

    /// What this server runs: the engine's fingerprints (a hash of the
    /// sources and settings that decide a conversion result, from the build)
    /// and the configuration in effect. After a rebuild and a restart the
    /// fingerprints show whether the new engine is the one running, and
    /// stored results made by an engine with the same check fingerprint are
    /// still reused.
    fn engine(&self, _args: &Value) -> Result<(String, Value), String> {
        let fp = json!({
            "version": TOOL_VERSION,
            "engine_fingerprint": sqldiff_convert::ENGINE_FINGERPRINT,
            "check_fingerprint": sqldiff_convert::CHECK_FINGERPRINT,
            "engine_crates": sqldiff_convert::FINGERPRINTED_CRATES,
            "check_crates": sqldiff_convert::CHECK_FINGERPRINTED_CRATES,
            "config": self.config_state(),
            "can_reload_config": self.reload.is_some(),
        });
        let text = format!(
            "sqldiff {TOOL_VERSION}\nengine fingerprint {}\ncheck fingerprint  {}\n{} configuration layer(s), {} pack(s)\n",
            sqldiff_convert::ENGINE_FINGERPRINT,
            sqldiff_convert::CHECK_FINGERPRINT,
            self.config.layers.len(),
            self.config.packs.len()
        );
        Ok((text, fp))
    }

    /// Resolves the configuration (config files, packs) again without a
    /// restart, so a pack or config edit takes effect in the next call. A
    /// configuration that does not load changes nothing. Stored conversion
    /// results are unaffected: their keys hold what the packs contribute to
    /// a check (helpers, rules), so a changed pack misses exactly the
    /// results it can change. Trees already converted in this session keep
    /// the configuration they were converted under.
    fn reload_config(&mut self, _args: &Value) -> Result<(String, Value), String> {
        if self
            .runs
            .iter()
            .any(|r| r.handle.as_ref().is_some_and(|h| !h.is_finished()))
        {
            return Err(
                "a background run is working: reload when it has finished or been cancelled"
                    .to_string(),
            );
        }
        let Some(reload) = &self.reload else {
            return Err("this server cannot reload its configuration".to_string());
        };
        let fresh = reload()?;
        let before = self.config_state();
        // Rebuild the review store from the fresh configuration before assigning anything, so
        // an unresolvable data key is refused exactly as at startup and leaves the old state.
        let store = Store::from_config(&fresh).map_err(|e| e.0)?;
        self.config = fresh;
        self.store = store;
        let after = self.config_state();
        let changed = before != after;
        let text = format!(
            "Configuration reloaded: {} layer(s), {} pack(s); {}.\n",
            self.config.layers.len(),
            self.config.packs.len(),
            if changed {
                "something changed"
            } else {
                "nothing changed"
            }
        );
        Ok((
            text,
            json!({ "changed": changed, "before": before, "after": after }),
        ))
    }

    /// What changed between two conversions (`sqldiff_convert::compare`):
    /// units that improved, regressed, changed, appeared or went. Each side
    /// is a tree id of this session or a `conversion-report.json` (or the
    /// folder holding it), so a baseline survives a server restart.
    fn compare_runs(&self, args: &Value) -> Result<(String, Value), String> {
        use sqldiff_convert::compare;
        let side = |key: &str| -> Result<(String, Option<tree::Report>), String> {
            let v = str_arg(args, key).ok_or_else(|| {
                format!("`{key}` is required (a tree id from `convert_tree`, or a conversion-report.json)")
            })?;
            if self.trees.iter().any(|(k, _)| *k == v) {
                return Ok((v, None));
            }
            let p = std::path::Path::new(&v);
            let file = if p.is_dir() {
                p.join("conversion-report.json")
            } else {
                p.to_path_buf()
            };
            let text = std::fs::read_to_string(&file).map_err(|e| {
                format!("`{v}` is no tree of this session and not a readable report: {e}")
            })?;
            let r: tree::Report = serde_json::from_str(&text)
                .map_err(|e| format!("{}: not a conversion report: {e}", file.display()))?;
            Ok((v, Some(r)))
        };
        let (na, ra) = side("a")?;
        let (nb, rb) = side("b")?;
        let held = |n: &str| {
            self.trees
                .iter()
                .find(|(k, _)| k == n)
                .map(|(_, t)| &t.report)
        };
        let a = ra.as_ref().or_else(|| held(&na)).expect("resolved above");
        let b = rb.as_ref().or_else(|| held(&nb)).expect("resolved above");
        let c = compare::compare(a, b)?;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map_or(DEFAULT_TREE_LIMIT * 4, |n| n as usize);
        let mut text = format!(
            "{na} -> {nb}: {} regressed, {} improved, {} changed, {} new, {} gone, {} unchanged\n",
            c.regressed, c.improved, c.changed, c.new, c.gone, c.unchanged
        );
        if c.src_differs {
            text.push_str("Note: the two conversions read different source folders.\n");
        }
        if c.unchecked_a + c.unchecked_b > 0 {
            text.push_str(&format!(
                "Note: {} file(s) of `a` and {} of `b` were not checked (an increment); their units count as not converted, so they show as regressed.\n",
                c.unchecked_a, c.unchecked_b
            ));
        }
        let show = |s: &Option<compare::Side>| match s {
            Some(s) => format!(
                "{}{}",
                s.status.label(),
                if s.rules.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", s.rules.join(", "))
                }
            ),
            None => "-".to_string(),
        };
        for r in c.rows.iter().take(limit) {
            let line = r.b.as_ref().or(r.a.as_ref()).map_or(0, |s| s.src_line);
            let why =
                r.b.as_ref()
                    .or(r.a.as_ref())
                    .and_then(|s| s.reason.lines().next())
                    .unwrap_or("");
            text.push_str(&format!(
                "{:?} {}:{line}: {} -> {}{}: {why}\n",
                r.change,
                r.path,
                show(&r.a),
                show(&r.b),
                if r.differs.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", r.differs.join(", "))
                }
            ));
        }
        if c.rows.len() > limit {
            text.push_str(&format!(
                "{} more row(s); raise `limit`.\n",
                c.rows.len() - limit
            ));
        }
        let mut structured = serde_json::to_value(&c).expect("a comparison serializes");
        structured["omitted_rows"] = json!(c.rows.len().saturating_sub(limit));
        structured["rows"] =
            serde_json::to_value(&c.rows[..c.rows.len().min(limit)]).expect("rows serialize");
        Ok((text, structured))
    }

    /// Where a run is; once it has finished, its report as `convert_tree`
    /// returns it, and the same on every later call.
    fn run_status(&mut self, args: &Value) -> Result<(String, Value), String> {
        use std::sync::atomic::Ordering;
        let k = self.run_index(args)?;
        if let Some((t, s)) = &self.runs[k].done {
            return Ok((t.clone(), s.clone()));
        }
        let r = &self.runs[k];
        let id = r.id.clone();
        if r.handle.as_ref().is_some_and(|h| !h.is_finished()) {
            let (c, n) = (
                r.progress.0.load(Ordering::Relaxed),
                r.progress.1.load(Ordering::Relaxed),
            );
            let cancelling = r.cancel.load(Ordering::Relaxed);
            let mut rows: Vec<(String, String)> =
                r.files.lock().map(|v| v.clone()).unwrap_or_default();
            // The finishing order depends on the threads; the report does not.
            rows.sort();
            let mut by_status: BTreeMap<&str, usize> = BTreeMap::new();
            for (_, st) in &rows {
                *by_status.entry(st.as_str()).or_default() += 1;
            }
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .map_or(DEFAULT_TREE_LIMIT, |n| n as usize);
            let state = if cancelling { "stopping" } else { "running" };
            let mut text = format!("[{id}] {state} {c}/{n} checks\n");
            let shown: Vec<&(String, String)> = rows
                .iter()
                .filter(|(_, st)| st != "converted")
                .take(limit)
                .collect();
            if !rows.is_empty() {
                text.push_str(&format!(
                    "{} file(s) checked so far (provisional: the passes across files that follow can only make a status worse):\n",
                    rows.len()
                ));
                for (st, count) in &by_status {
                    text.push_str(&format!("  {count:>5}  {st}\n"));
                }
                for (path, st) in &shown {
                    text.push_str(&format!("{path}: {st}\n"));
                }
            }
            let listed: Vec<Value> = shown
                .iter()
                .map(|(p, st)| json!({ "path": p, "status": st }))
                .collect();
            return Ok((
                text,
                json!({
                    "run": id, "state": state, "checked": c, "total": n,
                    "provisional": true,
                    "files_checked": rows.len(),
                    "by_status": by_status,
                    "files": listed,
                }),
            ));
        }
        let handle = self.runs[k]
            .handle
            .take()
            .expect("an uncollected run has its thread");
        let run_args = self.runs[k].args.clone();
        let answer = match handle.join().unwrap_or_else(|_| Err("the engine panicked".to_string())) {
            Ok((t, written)) => {
                let (text, mut structured) = self.report_tree(&run_args, t, written)?;
                structured["run"] = json!(id);
                structured["state"] = json!("finished");
                (format!("[{id}] finished\n{text}"), structured)
            }
            Err(e) if e == tree::CANCELLED => (
                format!("[{id}] cancelled: nothing was written to the output tree; the checks that finished are kept in the store.\n"),
                json!({ "run": id, "state": "cancelled" }),
            ),
            Err(e) => (
                format!("[{id}] failed: {e}\n"),
                json!({ "run": id, "state": "failed", "error": e }),
            ),
        };
        self.runs[k].done = Some(answer.clone());
        Ok(answer)
    }

    /// Asks a running run to stop before its next check. A run that has
    /// finished is not changed.
    fn cancel_run(&mut self, args: &Value) -> Result<(String, Value), String> {
        let k = self.run_index(args)?;
        let r = &self.runs[k];
        let running = r.handle.as_ref().is_some_and(|h| !h.is_finished());
        if running {
            r.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let id = r.id.clone();
        let text = if running {
            format!("[{id}] asked to stop; `run_status` shows when it has.\n")
        } else {
            format!("[{id}] is not running; `run_status` shows how it ended.\n")
        };
        Ok((
            text,
            json!({ "run": id, "state": if running { "stopping" } else { "not running" } }),
        ))
    }

    /// The report of a converted tree, as `convert_tree` returns it.
    fn report_tree(
        &mut self,
        args: &Value,
        t: tree::Tree,
        written: Option<(std::path::PathBuf, std::path::PathBuf)>,
    ) -> Result<(String, Value), String> {
        let statuses: Vec<sqldiff_convert::Status> = list_arg(args, "statuses")?;
        let folder = str_arg(args, "folder");
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_TREE_LIMIT);
        let max_units = args
            .get("max_units")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_TREE_UNITS);
        // Worst first, as in conversion-report.md.
        let mut rows: Vec<&tree::FileReport> = t
            .report
            .files
            .iter()
            .filter(|f| statuses.is_empty() || statuses.contains(&f.status))
            .filter(|f| folder.as_deref().map_or(true, |p| f.path.starts_with(p)))
            .collect();
        rows.sort_by(|x, y| y.status.cmp(&x.status).then(x.path.cmp(&y.path)));
        let matched = rows.len();
        rows.truncate(limit);

        self.tree_count += 1;
        let id = format!("tree-{}", self.tree_count);
        let mut text = format!("[{id}] {}", tree::summary(&t.report));
        text.push_str(if t.report.ok() {
            "Every file is converted or converted under assumptions.\n"
        } else {
            "Not every file is converted or converted under assumptions.\n"
        });
        if let Some((json, md)) = &written {
            text.push_str(&format!("Wrote {} and {}\n", json.display(), md.display()));
        } else {
            text.push_str("Nothing written (no `out`); `converted_file` shows a file's output.\n");
        }
        if !t.report.totals.top_reasons.is_empty() {
            text.push_str("\nMost frequent blockers:\n");
            for (k, n) in t.report.totals.top_reasons.iter().take(10) {
                text.push_str(&format!("  {n:>5}  {k}\n"));
            }
        }
        for u in &t.report.unreadable {
            text.push_str(&format!("unreadable (not converted): {u}\n"));
        }
        for c in &t.report.base_catalogs {
            text.push_str(&format!(
                "base catalog {} (sha256 {}): {} tables and views, {} routines\n",
                c.path, c.sha256, c.tables, c.routines
            ));
        }
        for c in &t.report.base_catalogs_b {
            text.push_str(&format!(
                "target base catalog {} (sha256 {}): {} tables and views, {} routines\n",
                c.path, c.sha256, c.tables, c.routines
            ));
        }
        for o in t.report.base_objects.iter().filter(|o| o.suspect) {
            text.push_str(&format!(
                "suspect base {} {} ({}:{}), flagged where used: {} ({})\n",
                o.kind,
                o.name,
                o.file,
                o.line,
                o.status.label(),
                o.rules.join(", ")
            ));
        }
        let files: Vec<Value> = rows
            .iter()
            .map(|f| {
                let mut v = serde_json::to_value(f).expect("a file report serializes");
                let shown = f.units.len().min(max_units);
                v["units"] = serde_json::to_value(&f.units[..shown]).expect("units serialize");
                v["omitted_units"] = json!(f.units.len() - shown);
                // Plainly converted units are only counted here; the report
                // file lists them.
                v["converted_units"] = json!(f.converted_units.len());
                v
            })
            .collect();
        text.push('\n');
        for f in &rows {
            text.push_str(&format!("{}: {}\n", f.path, f.status.label()));
            text.push_str(&units_text(f, max_units));
        }
        if matched > rows.len() {
            text.push_str(&format!(
                "\n{} more file(s) omitted; raise `limit` or narrow with `statuses` or `folder`.\n",
                matched - rows.len()
            ));
        }
        let structured = json!({
            "tree": id,
            "from": t.report.from,
            "to": t.report.to,
            "src": t.report.src,
            "out": t.report.out,
            "catalogs": t.report.catalogs,
            "templates": t.report.templates,
            "base_catalogs": t.report.base_catalogs,
            "base_catalogs_b": t.report.base_catalogs_b,
            "base_hash": t.report.base_hash,
            "base_objects": t.report.base_objects,
            "base_statements": t.report.base_statements,
            "ok": t.report.ok(),
            "totals": t.report.totals,
            "unreadable": t.report.unreadable,
            "file_count": matched,
            "files": files,
            "omitted_files": matched - rows.len(),
        });
        if self.trees.len() >= TREE_LIMIT {
            self.trees.remove(0);
        }
        self.trees.push((id, t));
        Ok((text, structured))
    }

    fn converted_file(&self, args: &Value) -> Result<(String, Value), String> {
        let id = str_arg(args, "tree").ok_or("`tree` is required (an id from `convert_tree`)")?;
        let path =
            str_arg(args, "path").ok_or("`path` is required (as `convert_tree` lists it)")?;
        let (_, t) = self.trees.iter().find(|(k, _)| *k == id).ok_or_else(|| {
            format!("no tree `{id}` in this session (the last {TREE_LIMIT} are kept); run `convert_tree` first")
        })?;
        let k = t
            .report
            .files
            .iter()
            .position(|f| f.path == path)
            .ok_or_else(|| format!("{id} has no file `{path}`"))?;
        let f = &t.report.files[k];
        let output = &t.outputs[k];
        let mut text = format!("[{id}] {}: {}\n", f.path, f.status.label().to_uppercase());
        text.push_str(&counts_line(&f.counts));
        text.push_str(&units_text(f, usize::MAX));
        text.push_str(&format!("\nOutput:\n{output}"));
        if !output.ends_with('\n') {
            text.push('\n');
        }
        Ok((text, json!({ "tree": id, "file": f, "output": output })))
    }
}

/// The target dialect: `to`, required.
fn target(args: &Value) -> Result<Dialect, String> {
    args.get("to")
        .and_then(Value::as_str)
        .ok_or("`to` is required (postgres or db2i)")?
        .parse()
}

fn counts_line(c: &sqldiff_convert::Counts) -> String {
    format!(
        "units: {} converted, {} under assumptions, {} needs a decision, {} not proven, {} differs, {} not converted\n",
        c.converted, c.assumptions, c.decision, c.unproven, c.differs, c.not_converted
    )
}

/// A file's units that are not plainly converted, with rules and reason,
/// and output findings outside any unit; at most `max` units.
fn units_text(f: &tree::FileReport, max: usize) -> String {
    let mut t = String::new();
    for u in f.units.iter().take(max) {
        t.push_str(&format!(
            "  line {} → {}: {} ({}) {}\n",
            u.src_line,
            u.out_line,
            u.status.label(),
            u.rules.join(", "),
            u.reason
        ));
    }
    if f.units.len() > max {
        t.push_str(&format!(
            "  {} more unit(s); raise `max_units` or use `converted_file`\n",
            f.units.len() - max
        ));
    }
    for p in &f.unplaced {
        t.push_str(&format!("  outside any unit: differs {p}\n"));
    }
    for p in &f.unproven {
        t.push_str(&format!("  outside any unit: not proven {p}\n"));
    }
    t
}

fn status_label(s: queue::Status) -> &'static str {
    match s {
        queue::Status::Open => "open (tool default, nobody declared it)",
        queue::Status::Declared => "declared (pack or config)",
        queue::Status::Proven => "proven from composed output",
        queue::Status::Confirmed => "confirmed in this session",
        queue::Status::Denied => "denied in this session",
    }
}

/// The increment `select` names (see `sqldiff convert --next` and
/// friends); empty checks the whole tree. `next` must be a whole number.
fn select_arg(args: &Value) -> Result<sqldiff_convert::plan::Select, String> {
    let Some(v) = args.get("select").filter(|v| !v.is_null()) else {
        return Ok(Default::default());
    };
    let next = match v.get("next").filter(|n| !n.is_null()) {
        None => None,
        Some(n) => Some(
            n.as_u64()
                .ok_or("`select.next` must be a whole number of groups (0 reports coverage)")?
                as usize,
        ),
    };
    Ok(sqldiff_convert::plan::Select {
        next,
        schemas: list_arg(v, "schemas")?,
        objects: list_arg(v, "objects")?,
        paths: list_arg(v, "paths")?,
        changed: v.get("changed").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// Converts the tree as `sqldiff convert` does: with an `out`, each file's
/// checked result is kept in the store of `out` and reused when its key
/// matches, so a second call over an unchanged tree (a queue read, a
/// drafted unit) costs a lookup. Without `out` nothing is kept. `fresh`
/// and `select` belong to `convert_tree` alone (`incremental`): the queue
/// and `write_unit` write nothing but the store.
fn build_tree(
    args: &Value,
    a: &tree::Args,
    inputs: &[tree::Input],
    unreadable: Vec<String>,
    out: Option<&std::path::Path>,
    hooks: &tree::Hooks,
    incremental: bool,
) -> Result<tree::Tree, String> {
    let fresh = args.get("fresh").and_then(Value::as_bool).unwrap_or(false);
    let select = select_arg(args)?;
    if !incremental && (fresh || !select.is_empty()) {
        return Err("`fresh` and `select` belong to `convert_tree`".to_string());
    }
    let Some(o) = out else {
        if !select.is_empty() || fresh {
            return Err("`select` and `fresh` need `out`: the store lives there".to_string());
        }
        return tree::convert_with(a, inputs, unreadable, None, hooks);
    };
    let t = tree::convert_increment(a, inputs, unreadable, o, hooks, fresh, &select)?;
    if !select.is_empty() {
        sqldiff_convert::job::write_plan(
            &sqldiff_convert::plan::Plan::build(&tree::plan_inputs(a, inputs), a.from),
            o,
        )?;
    }
    Ok(t)
}

/// What `convert_tree` does before it reports: the steps of
/// `sqldiff convert`, in the same order. Runs on the calling thread or, for
/// a background run, on its worker.
fn run_tree(
    args: &Value,
    a: &tree::Args,
    hooks: &tree::Hooks,
) -> Result<(tree::Tree, Option<(std::path::PathBuf, std::path::PathBuf)>), String> {
    let out = str_arg(args, "out").map(std::path::PathBuf::from);
    let flag = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);
    let (inputs, unreadable) = tree::read(a)?;
    if let Some(o) = &out {
        tree::prepare_out(&a.src, o, flag("overwrite"))?;
        tree::guard_hand_edits(o, flag("discard_edits"))?;
    }
    let mut t = build_tree(args, a, &inputs, unreadable, out.as_deref(), hooks, true)?;
    // Review decisions committed next to the converted tree.
    if let Some(o) = &out {
        tree::review_in(&mut t, &inputs, o)?;
    }
    // The last point a run stops at: from here on the output is written.
    hooks.stop_if_cancelled()?;
    let written = match &out {
        Some(o) => {
            let w = tree::write(&t, o)?;
            let q = sqldiff_convert::queue::build(&t.report, None, false);
            sqldiff_convert::queue::write(&q, o)?;
            Some(w)
        }
        None => None,
    };
    Ok((t, written))
}

fn str_arg(args: &Value, k: &str) -> Option<String> {
    args.get(k).and_then(Value::as_str).map(str::to_string)
}

fn enum_arg<T: serde::de::DeserializeOwned>(args: &Value, k: &str) -> Result<Option<T>, String> {
    match args.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone())
            .map(Some)
            .map_err(|e| format!("`{k}`: {e}")),
    }
}

fn list_arg<T: serde::de::DeserializeOwned>(args: &Value, k: &str) -> Result<Vec<T>, String> {
    Ok(enum_arg::<Vec<T>>(args, k)?.unwrap_or_default())
}

/// A finding card: everything but the excerpts beyond their first line.
fn card(f: &Finding) -> Value {
    json!({
        "fingerprint": f.fingerprint,
        "rule": f.rule,
        "class": f.class,
        "severity": f.severity,
        "verdict": f.verdict,
        "witness": f.witness,
        "a": f.a.as_ref().map(short_loc),
        "b": f.b.as_ref().map(short_loc),
        "assumptions": f.assumptions,
    })
}

fn short_loc(loc: &Location) -> String {
    let first = loc.excerpt.first().map(|s| s.trim()).unwrap_or("");
    format!("{:?}: {}:{}  {}", loc.side, loc.path, loc.start_line, first)
}

fn pair_label(r: &Report) -> String {
    match &r.b {
        Some(b) => format!("{} ↔ {}", r.a.path, b.path),
        None => r.a.path.clone(),
    }
}

/// Build an artifact from `{path | text, dialect, name?, version?}`, with
/// the file it was read from, if any.
fn artifact(v: Option<&Value>, field: &str) -> Result<(Artifact, Option<String>), String> {
    let v = v.and_then(Value::as_object).ok_or_else(|| {
        format!("`{field}` must be an object with `dialect` and `path` or `text`")
    })?;
    let dialect: Dialect = v
        .get("dialect")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{field}.dialect` is required (tsql, postgres or db2i)"))?
        .parse()?;
    let name = v.get("name").and_then(Value::as_str);
    let (path, text, file) = match (
        v.get("path").and_then(Value::as_str),
        v.get("text").and_then(Value::as_str),
    ) {
        (Some(p), None) => {
            let t = std::fs::read_to_string(p).map_err(|e| format!("{field}: {p}: {e}"))?;
            (name.unwrap_or(p).to_string(), t, Some(p.to_string()))
        }
        (None, Some(t)) => (name.unwrap_or(field).to_string(), t.to_string(), None),
        _ => return Err(format!("`{field}` needs exactly one of `path` or `text`")),
    };
    let mut a = Artifact::new(path, dialect, text);
    if let Some(ver) = v.get("version").and_then(Value::as_str) {
        a.set_version(ver).map_err(|e| format!("{field}: {e}"))?;
    }
    Ok((a, file))
}

fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked
        .filter(|v| PROTOCOL_VERSIONS.contains(v))
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "sqldiff", "version": TOOL_VERSION },
        "instructions": INSTRUCTIONS,
    })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn artifact_schema(what: &str) -> Value {
    json!({
        "type": "object",
        "description": format!("{what}. Give exactly one of `path` (read from disk) or `text`."),
        "properties": {
            "dialect": { "type": "string", "enum": ["tsql", "postgres", "db2i"] },
            "path": { "type": "string", "description": "File to read" },
            "text": { "type": "string", "description": "SQL source inline" },
            "name": { "type": "string", "description": "Display name used in locations" },
            "version": { "type": "string", "description": "Target engine version; defaults to SQL Server 2022, PostgreSQL 18, Db2 for i 7.6" },
        },
        "required": ["dialect"],
    })
}

fn tools() -> Value {
    let ro = json!({ "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false });
    let bounds = json!({
        "max_findings": { "type": "integer", "minimum": 0, "description": "Cap on finding cards returned (default 20); the rest are counted" },
        "excerpt_lines": { "type": "integer", "minimum": 1, "description": "Excerpt lines kept per location (default: config, else 3)" },
    });
    let with = |mut props: Map<String, Value>| {
        props.extend(bounds.as_object().unwrap().clone());
        Value::Object(props)
    };
    let mut diff_props = Map::new();
    diff_props.insert(
        "a".into(),
        artifact_schema("Side A: the original or reference"),
    );
    diff_props.insert(
        "b".into(),
        artifact_schema("Side B: the new version or conversion"),
    );
    let classes = [
        "cosmetic",
        "structural",
        "semantic",
        "cardinality",
        "transactional",
        "concurrency",
        "binding",
        "residue",
        "limit",
    ];
    let severities = ["info", "low", "medium", "high", "critical"];
    let verdicts = ["equal", "equal_under_assumptions", "unknown", "different"];
    let mut check_props = Map::new();
    check_props.insert("artifact".into(), artifact_schema("The artifact to lint"));
    json!([
        {
            "name": "diff",
            "title": "Semantic diff",
            "description": "Compare two SQL artifacts: two versions of one object (same dialect) or one object in two dialects (drift or conversion review). Returns a verdict (equal, equal_under_assumptions, unknown, different) and finding cards ranked by severity; cosmetic and structural noise is counted, not listed.",
            "inputSchema": { "type": "object", "properties": with(diff_props), "required": ["a", "b"] },
            "annotations": ro,
        },
        {
            "name": "check",
            "title": "Lint one artifact",
            "description": "Lint one SQL artifact: residue from other dialects, terminator hazards (e.g. a missing ';' before THROW) and parse problems.",
            "inputSchema": { "type": "object", "properties": with(check_props), "required": ["artifact"] },
            "annotations": ro,
        },
        {
            "name": "finding",
            "title": "Finding in full",
            "description": "One finding from an earlier `diff`, `check` or `queue` in this session, by fingerprint: witness, both locations with excerpts, and the text of each assumption it relies on.",
            "inputSchema": {
                "type": "object",
                "properties": { "fingerprint": { "type": "string" } },
                "required": ["fingerprint"],
            },
            "annotations": ro,
        },
        {
            "name": "queue",
            "title": "Review queue",
            "description": "Pair every object of a source across dialects (pack pairing rules), diff each pair and rank them for review: unreviewed first, then verdict, severity, whether an assumption alone decides the verdict, recency and size. A few lines per pair; use `diff`-style ids with `assumptions`, `finding` and `open_in_gui`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "source": { "type": "string", "description": "A directory, a snapshot bundle (.json), git:REPO[@REV][//SUBDIR] or adapter:NAME; default: [source] in sqldiff.toml" },
                    "dialect_a": { "type": "string", "enum": ["tsql", "postgres", "db2i"] },
                    "dialect_b": { "type": "string", "enum": ["tsql", "postgres", "db2i"] },
                    "classes": { "type": "array", "items": { "type": "string", "enum": classes }, "description": "Keep pairs with a finding of one of these classes" },
                    "min_severity": { "type": "string", "enum": severities },
                    "verdicts": { "type": "array", "items": { "type": "string", "enum": verdicts } },
                    "folder": { "type": "string", "description": "Keep pairs where either object id starts with this" },
                    "review": { "type": "string", "enum": ["unreviewed", "reviewed"] },
                    "limit": { "type": "integer", "minimum": 0, "description": "Cap on pairs listed (default 25); the rest are counted" },
                },
            },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false },
        },
        {
            "name": "assumptions",
            "title": "Assumption ledger",
            "description": "The assumptions a pair's verdict relies on: open (tool default), declared (pack or config), or confirmed/denied in this session; which findings rely on each, and which flip the verdict. Name the pair by `pair` id or any of its findings' `fingerprint`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pair": { "type": "string", "description": "Pair id from `queue` or `diff`" },
                    "fingerprint": { "type": "string", "description": "Any finding of the pair" },
                },
            },
            "annotations": ro,
        },
        {
            "name": "set_assumption",
            "title": "Flip an assumption",
            "description": "Confirm (holds=true) or deny (holds=false) an assumption for this session only, or reset it (holds=null); every held pair's verdict is re-derived and changes are listed. Flipping ASM-NO-CALLER-TXN or ASM-TSQL-CALLER-XACT-ABORT-OFF re-runs the behavioural analysis of pairs from `diff` under that scenario; any other denied assumption makes the findings that rely on it unknown. Nothing is written to packs, config or sources.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Assumption id, e.g. ASM-TSQL-CALLER-XACT-ABORT-OFF" },
                    "holds": { "type": ["boolean", "null"] },
                },
                "required": ["id", "holds"],
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false },
        },
        {
            "name": "comment_text",
            "title": "Draft a review comment",
            "description": "A plain-text review comment for the given findings, built by a deterministic phrase grammar from their witness, locations and assumptions (no AI): same findings, same text. Grouped by pair, most severe first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "finding_ids": { "type": "array", "items": { "type": "string" }, "description": "Fingerprints from this session" },
                    "excerpt_lines": { "type": "integer", "minimum": 0, "description": "Excerpt lines quoted per location (default 1, capped by config)" },
                    "fingerprints": { "type": "boolean", "description": "End each item with its fingerprint (default true)" },
                },
                "required": ["finding_ids"],
            },
            "annotations": ro,
        },
        {
            "name": "what_if",
            "title": "What if this statement fails?",
            "description": "For the statement on (or before) `line` of one side, failing with `error_kind`: both sides' control target (handler, next statement or exit), rollback scope, durable and undone effects, transaction state and path, plus the divergences as findings. Give the pair by `pair` id (from `diff`) or `a` and `b`. Session flips of ASM-NO-CALLER-TXN / ASM-TSQL-CALLER-XACT-ABORT-OFF change the scenario.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pair": { "type": "string", "description": "Pair id from `diff`" },
                    "a": artifact_schema("Side A, instead of `pair`"),
                    "b": artifact_schema("Side B, instead of `pair`"),
                    "line": { "type": "integer", "minimum": 1 },
                    "side": { "type": "string", "enum": ["a", "b"], "description": "Side the line refers to (default a)" },
                    "error_kind": { "type": "string", "enum": ["constraint", "conversion", "arithmetic", "cardinality", "not_found", "lock", "user_signal", "name_resolution", "cancel", "other"], "description": "Default constraint" },
                },
                "required": ["line"],
            },
            "annotations": ro,
        },
        {
            "name": "overlay",
            "title": "Transaction overlay",
            "description": "Brackets for transactions (or who owns the transaction), savepoints, atomic and exception regions and handlers on both sides, each paired with its partner on the other side.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pair": { "type": "string", "description": "Pair id from `diff`" },
                    "a": artifact_schema("Side A, instead of `pair`"),
                    "b": artifact_schema("Side B, instead of `pair`"),
                },
            },
            "annotations": ro,
        },
        {
            "name": "open_in_gui",
            "title": "Deep link to the GUI",
            "description": "A URL that opens the pair in the SQLDiff GUI with the finding selected, and the command that starts the GUI on it. Only for pairs read from files.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "finding_id": { "type": "string", "description": "Fingerprint to select" },
                    "pair": { "type": "string", "description": "Pair id, when no finding is to be selected" },
                    "gui_url": { "type": "string", "description": "GUI address (default: --gui-url, $SQLDIFF_GUI_URL or http://127.0.0.1:7878)" },
                },
            },
            "annotations": ro,
        },
        {
            "name": "convert",
            "title": "Convert one file",
            "description": "Convert one SQL file or inline text into another dialect with the engine of `sqldiff convert` (rules: T-SQL to PostgreSQL 18 or Db2 for i). Every unit is converted, under assumptions, needs a decision, not proven, differs or not converted; a unit counts as converted only when its rules needed no assumption and neither the engine's diff nor the output's own check (parse, binding, leftover source syntax, target limits) finds anything in it. Returns the file's status, unit counts, every unit that is not plainly converted with its rules and reason, and the output as `sqldiff convert` writes it (with `-- [sqldiff: <status>]` lines). Names defined in other files do not bind; use `convert_tree` for those. Writes nothing.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "artifact": artifact_schema("The source; `dialect` is the source dialect"),
                    "to": { "type": "string", "enum": ["tsql", "postgres", "db2i"], "description": "Target dialect" },
                    "template": { "type": "boolean", "description": "Read the input as a template in the loaded packs' [template] syntax (as `sqldiff convert --template`)" },
                    "parameters": { "type": "array", "items": { "type": "string" }, "description": "With `template`: the only placeholder names, besides the pack's known_parameters" },
                },
                "required": ["artifact", "to"],
            },
            "annotations": ro,
        },
        {
            "name": "convert_tree",
            "title": "Convert a whole tree",
            "description": "Convert every file of a source tree into another dialect, exactly as `sqldiff convert SRC -o OUT --to TO`: rules with the source catalog of the whole tree, then each file checked against catalogs from both trees, so names defined in other files bind. A file's status is its worst unit's. Returns totals, the most frequent blockers and files worst first, each with its units that are not plainly converted (rules, reason, source and output line). With `out`, also writes the annotated output tree and conversion-report.json/.md there (the directory must be empty unless `overwrite`, and not inside the source tree); without it nothing is written and `converted_file` shows any file's output. `ok` is true when every file is converted or converted under assumptions (the CLI's exit code 0).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "src": { "type": "string", "description": "Source directory, or one file" },
                    "from": { "type": "string", "enum": ["tsql", "postgres", "db2i"], "description": "Source dialect (default tsql)" },
                    "to": { "type": "string", "enum": ["tsql", "postgres", "db2i"], "description": "Target dialect" },
                    "out": { "type": "string", "description": "Output directory to write, as `sqldiff convert -o`; default: write nothing" },
                    "overwrite": { "type": "boolean", "description": "Replace files in a non-empty `out` (default false)" },
                    "discard_edits": { "type": "boolean", "description": "With `overwrite`: also replace files edited by hand since the last run (refused otherwise)" },
                    "no_catalog": { "type": "boolean", "description": "Check each file alone, without catalogs built from the whole tree" },
                    "catalogs": { "type": "array", "items": { "type": "string" }, "description": "Base catalogs, as `sqldiff convert --catalog` (in order): DDL files in the source dialect of objects that exist already but are not part of `src`. They bind on both sides (an object `src` defines replaces one of the same name), are converted for the target catalog only, and are never written or counted; a statement that uses a base object whose conversion is not plain carries ASM-BASE-SUSPECT. Not with `no_catalog`" },
                    "catalogs_b": { "type": "array", "items": { "type": "string" }, "description": "The target's base catalogs, as `sqldiff convert --catalog-b` (in order): the same objects as `catalogs`, as DDL in the target dialect. The target side binds against these as given instead of converting `catalogs`. Needs `catalogs`" },
                    "template": { "type": "boolean", "description": "Every file is a template in the loaded packs' [template] syntax (as `sqldiff convert --template`)" },
                    "parameters": { "type": "array", "items": { "type": "string" }, "description": "With `template`: the only placeholder names, besides the pack's known_parameters" },
                    "extensions": { "type": "array", "items": { "type": "string" }, "description": "File extensions to convert (default [\"sql\"])" },
                    "jobs": { "type": "integer", "minimum": 1, "description": "Parallel jobs (default: available cores)" },
                    "background": { "type": "boolean", "description": "Return at once with a run id and convert on a worker thread; poll `run_status`, stop with `cancel_run`. One run works at a time, and no other conversion tool runs meanwhile. The finished run reports exactly as this call would" },
                    "temporal_historyless": { "type": "boolean", "description": "Rule option `--temporal-historyless` (ASM-TEMPORAL-HISTORYLESS), off by default. Give the same value to `conversion_queue` and `write_unit`" },
                    "recheck": { "type": "string", "enum": ["none", "sample", "all"], "description": "With `out`: how much of what is taken from the store is built again and compared (default sample). `none` trusts the store and the report says so; `all` verifies every stored result" },
                    "fresh": { "type": "boolean", "description": "With `out`: reuse no stored result; every file is checked again and stored" },
                    "select": { "type": "object", "description": "With `out`: check only this increment, as `sqldiff convert --next/--schema/--object/--path/--changed`; a file outside it is taken from the store or counted not converted", "properties": {
                        "next": { "type": "integer", "minimum": 0, "description": "0 reports coverage" },
                        "schemas": { "type": "array", "items": { "type": "string" } },
                        "objects": { "type": "array", "items": { "type": "string" } },
                        "paths": { "type": "array", "items": { "type": "string" } },
                        "changed": { "type": "boolean" } } },
                    "statuses": { "type": "array", "items": { "type": "string", "enum": ["converted", "assumptions", "decision", "unproven", "differs", "not_converted"] }, "description": "List only files with one of these statuses (totals always cover the whole tree)" },
                    "folder": { "type": "string", "description": "List only files whose path starts with this" },
                    "limit": { "type": "integer", "minimum": 0, "description": "Cap on files listed (default 25); the rest are counted" },
                    "max_units": { "type": "integer", "minimum": 0, "description": "Cap on units listed per file (default 5); the rest are counted" },
                },
                "required": ["src", "to"],
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": false },
        },
        {
            "name": "conversion_queue",
            "title": "Review queue of a converted tree",
            "description": "What a person should review in a converted tree, worst first, by tier (never a score): 1 differs, not converted, rejected, a missing object many units fail to bind on (one item), templates whose instances differ; 2 not proven, instances unknown or not woven, templates no instance uses; 3 output an agent wrote that no person approved; 4 open decisions; 5 open assumptions (unverified vendor facts and template premises first); 6 (with `all`) plainly converted units nobody approved. Converts `src` again with the same arguments as `convert_tree` (each file's checked result is reused from the store in `out`, so an unchanged tree costs lookups) and applies the review-decisions.jsonl in `out`, so decisions recorded since count; writes nothing but that store. Options that differ from the run that produced `out` (rules, catalogs) store other results and drop that run's from the store at the end of the call, so pass the same values. Reviewed units are not listed. Verdicts are recorded by people in the GUI or CLI, never by this server; `write_unit` only records an agent's draft.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "src": { "type": "string", "description": "Source directory, or one file, as given to `convert_tree`" },
                    "from": { "type": "string", "enum": ["tsql", "postgres", "db2i"], "description": "Source dialect (default tsql)" },
                    "to": { "type": "string", "enum": ["tsql", "postgres", "db2i"], "description": "Target dialect" },
                    "out": { "type": "string", "description": "The converted tree (where review-decisions.jsonl lives)" },
                    "no_catalog": { "type": "boolean" },
                    "catalogs": { "type": "array", "items": { "type": "string" }, "description": "As given to `convert_tree`" },
                    "catalogs_b": { "type": "array", "items": { "type": "string" }, "description": "As given to `convert_tree`" },
                    "temporal_historyless": { "type": "boolean", "description": "As given to `convert_tree`" },
                    "recheck": { "type": "string", "enum": ["none", "sample", "all"], "description": "As given to `convert_tree`" },
                    "template": { "type": "boolean" },
                    "parameters": { "type": "array", "items": { "type": "string" } },
                    "extensions": { "type": "array", "items": { "type": "string" } },
                    "instances": { "type": "string", "description": "instance-report.json from `sqldiff instances`, or its directory: templates are then judged by their woven instances" },
                    "all": { "type": "boolean", "description": "Also list plainly converted units nobody approved (tier 6)" },
                    "max_tier": { "type": "integer", "minimum": 1, "maximum": 6, "description": "List items up to this tier (default 6)" },
                    "limit": { "type": "integer", "minimum": 0, "description": "Cap on items listed (default 25); the rest are counted" },
                },
                "required": ["src", "to", "out"],
            },
            "annotations": ro,
        },
        {
            "name": "write_unit",
            "title": "Draft one unit's output by hand",
            "description": "Replace the output of one unit of a converted tree with your own text, as an agent's draft. Records a `written` record (always `by: agent`) with the text in OUT/review-decisions.jsonl, then converts `src` again and rewrites `out`, so the text is checked against the source like rule output in a plain file; in a template it is spliced in unchecked and marked not proven until the woven instances are checked. Returns the unit's new id, status, notes and findings. The draft is never reviewed: it stays in the queue (tier 3 or worse) until a person approves it, and a person's own text for the unit replaces it. Refused if a file in `out` was edited by hand since the last conversion. Unit ids change with the text: take the id from a fresh `conversion_queue` or `convert_tree`.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "src": { "type": "string", "description": "Source directory, or one file, as given to `convert_tree`" },
                    "from": { "type": "string", "enum": ["tsql", "postgres", "db2i"], "description": "Source dialect (default tsql)" },
                    "to": { "type": "string", "enum": ["tsql", "postgres", "db2i"], "description": "Target dialect" },
                    "out": { "type": "string", "description": "The converted tree, written earlier by `convert_tree`" },
                    "no_catalog": { "type": "boolean" },
                    "catalogs": { "type": "array", "items": { "type": "string" }, "description": "As given to `convert_tree`" },
                    "catalogs_b": { "type": "array", "items": { "type": "string" }, "description": "As given to `convert_tree`" },
                    "temporal_historyless": { "type": "boolean", "description": "As given to `convert_tree`" },
                    "recheck": { "type": "string", "enum": ["none", "sample", "all"], "description": "As given to `convert_tree`" },
                    "template": { "type": "boolean" },
                    "parameters": { "type": "array", "items": { "type": "string" } },
                    "extensions": { "type": "array", "items": { "type": "string" } },
                    "path": { "type": "string", "description": "File path as the report lists it" },
                    "unit": { "type": "string", "description": "The unit's id in the current conversion" },
                    "text": { "type": "string", "description": "The unit's full output text, replacing what the rules wrote" },
                    "reason": { "type": "string", "description": "Why this text does what the source does" },
                    "author": { "type": "string", "description": "Name to record (default \"agent\")" },
                },
                "required": ["src", "to", "out", "path", "unit", "text", "reason"],
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false },
        },
        {
            "name": "run_status",
            "title": "Status of a background run",
            "description": "Where a `convert_tree` started with `background` is: running (checks done of total, with the files checked so far and their statuses, provisional because the passes across files that follow can only make a status worse), stopping, or finished. A finished run is reported here exactly as `convert_tree` reports (same arguments, same tree id for `converted_file`), cancelled, or failed; later calls return the same answer.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run": { "type": "string", "description": "Run id from `convert_tree`" },
                    "limit": { "type": "integer", "minimum": 0, "description": "While running: cap on files listed (default 25; only files not plainly converted are listed, the counts cover all)" },
                },
                "required": ["run"],
            },
            "annotations": ro,
        },
        {
            "name": "cancel_run",
            "title": "Stop a background run",
            "description": "Asks a running background `convert_tree` to stop before its next check. A stopped run writes no output tree (the checks it finished stay in the store, so a rerun resumes); a run that already finished is not changed. Poll `run_status` to see it stop.",
            "inputSchema": {
                "type": "object",
                "properties": { "run": { "type": "string", "description": "Run id from `convert_tree`" } },
                "required": ["run"],
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false },
        },
        {
            "name": "engine",
            "title": "Engine and configuration in effect",
            "description": "What this server runs: the tool version, the engine fingerprint and the check fingerprint (hashes of the sources and settings that decide a conversion, fixed at build time; stored results are keyed by them), the crates each covers, and the configuration layers and packs in effect with their content hashes. After `cargo build` and a restart, compare with an earlier answer to see which engine is running; results stored by an engine with the same check fingerprint are reused.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": ro,
        },
        {
            "name": "reload_config",
            "title": "Reload packs and configuration",
            "description": "Resolve the configuration (project and user config, packs) again from the sources this server started with, without a restart; the next call uses it. A configuration that fails to load changes nothing. Returns whether the layers or packs changed, before and after, with content hashes. Conversions already made in this session keep the configuration they were made under; stored results need no handling, since their keys hold what packs contribute to a check. Refused while a background run works.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false },
        },
        {
            "name": "compare_runs",
            "title": "Compare two conversions",
            "description": "What changed between two conversions of a tree, from their reports without converting again: units that regressed (a worse status), improved, changed (same status, but other rules, findings, reason or rewrites: a rule edit that changes a converted unit's output shows here), are new or are gone, in a fixed order (regressed worst first, improved, changed, new, gone; then path and source line). Units are joined by id, then by path and source line. Refused for conversions of different dialects; a different source folder is noted. Each side `a` (before) and `b` (after) is a tree id of this session or a conversion-report.json (or the folder holding it); copy the report before converting into the same `out` again to keep a baseline across a server restart. Returns the counts, the units per status in each run and the rows.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "a": { "type": "string", "description": "The earlier conversion: a tree id or a conversion-report.json path" },
                    "b": { "type": "string", "description": "The later conversion: a tree id or a conversion-report.json path" },
                    "limit": { "type": "integer", "minimum": 0, "description": "Cap on rows listed (default 100); the counts cover all" },
                },
                "required": ["a", "b"],
            },
            "annotations": ro,
        },
        {
            "name": "converted_file",
            "title": "One file of a converted tree",
            "description": "One file from an earlier `convert_tree` in this session: its status, every unit that is not plainly converted with rules and reason, and its output as `sqldiff convert` writes it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tree": { "type": "string", "description": "Tree id from `convert_tree`" },
                    "path": { "type": "string", "description": "File path as `convert_tree` lists it" },
                },
                "required": ["tree", "path"],
            },
            "annotations": ro,
        },
    ])
}

fn outcome_text(side: &str, o: &SideOutcome) -> String {
    let mut t = format!(
        "Side {side}: {}; {}; rolls back {}; {}.\n",
        o.control, o.exit, o.rollback, o.transaction
    );
    for (label, xs) in [
        ("durable", &o.durable),
        ("undone", &o.undone),
        ("kept partly", &o.partial),
        ("left to the caller", &o.left_to_caller),
    ] {
        if !xs.is_empty() {
            t.push_str(&format!("  {label}: {}\n", xs.join("; ")));
        }
    }
    t
}

fn what_if_text(w: &WhatIf) -> String {
    let mut t = format!("{}\n", w.scenario.text);
    for (side, v) in [("A", &w.a), ("B", &w.b)] {
        if v.is_empty() {
            t.push_str(&format!("Side {side}: no path reaches the statement.\n"));
        }
        for o in v {
            t.push_str(&outcome_text(side, o));
        }
    }
    if w.findings.is_empty() {
        t.push_str("No divergence under this scenario.\n");
    }
    for f in &w.findings {
        t.push_str(&format!(
            "[{}] {} {} {}: {}\n",
            f.fingerprint,
            f.class.label(),
            f.verdict.label(),
            f.rule,
            f.witness
        ));
    }
    for n in &w.notes {
        t.push_str(&format!("Note: {n}\n"));
    }
    t
}
