//! Local stdio MCP server for SQLDiff (design section 6.1).
//!
//! Speaks JSON-RPC 2.0 over newline-delimited stdio. Tools are coarse,
//! deterministic and token-bounded: `diff` and `check` return a verdict and
//! compact finding cards; `finding` returns one card in full from this
//! session's results; `queue` ranks every pair of a source for review;
//! `assumptions` and `set_assumption` show and flip the assumption ledger
//! (session-local, verdicts are re-derived); `comment_text` drafts a review
//! comment from the deterministic phrase grammar; `open_in_gui` gives a deep
//! link. Findings accepted in the review store, or marked as deliberate in
//! source, are suppressed and counted. The server only reads files and the
//! review store, and never writes anywhere.

use std::collections::{BTreeMap, HashMap};

use serde_json::{json, Map, Value};
use sqldiff_core::model::{Assumption, Location};
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

const INSTRUCTIONS: &str = "SQLDiff compares SQL artifacts in T-SQL (SQL Server 2022), \
PostgreSQL 18 and Db2 for i. Verdicts are deterministic and never a false \"equal\": \
\"equal_under_assumptions\" lists the assumptions it relies on, and \"unknown\" says why \
equivalence could not be shown. Use `queue` to rank every pair of a source for review, `diff` \
for two versions of one object or one object in two dialects, `check` to lint a single file and \
`finding` to expand a card by fingerprint. `assumptions` lists what a verdict relies on and \
`set_assumption` flips one for this session only. `comment_text` drafts a plain-text review \
comment and `open_in_gui` gives a deep link for a human.";

/// A diffed pair (or checked artifact) kept for later lookups.
struct Held {
    /// The pair as diffed, before session flips.
    pair: PairReport,
    /// Files the GUI can open, when both sides came from disk.
    files: Option<(String, Option<String>)>,
}

pub struct Server {
    config: Resolved,
    gui_url: String,
    flips: Flips,
    pairs: BTreeMap<String, Held>,
    /// Fingerprint → pair id.
    findings: HashMap<String, String>,
    /// Read-only; re-read before each analysis so decisions made in the CLI
    /// or GUI show up.
    store: Store,
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

    fn build(config: Resolved, gui_url: Option<String>, store: Store) -> Self {
        Server {
            config,
            gui_url: gui_url.unwrap_or_else(|| queue::DEFAULT_GUI_URL.to_string()),
            flips: Flips::new(),
            pairs: BTreeMap::new(),
            findings: HashMap::new(),
            store,
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
        let out = match name {
            "diff" => self.diff(args),
            "check" => self.check(args),
            "finding" => self.finding(args),
            "queue" => self.queue(args),
            "assumptions" => self.assumptions(args),
            "set_assumption" => self.set_assumption(args),
            "comment_text" => self.comment_text(args),
            "open_in_gui" => self.open_in_gui(args),
            other => return Err((-32602, format!("unknown tool: {other}"))),
        };
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

    fn options(&self, args: &Value) -> Options {
        Options {
            excerpt_lines: args
                .get("excerpt_lines")
                .and_then(Value::as_u64)
                .map(|n| n as usize)
                .unwrap_or(self.config.excerpt_lines),
            ..self.config.core_options()
        }
    }

    fn diff(&mut self, args: &Value) -> Result<(String, Value), String> {
        let (a, fa) = artifact(args.get("a"), "a")?;
        let (b, fb) = artifact(args.get("b"), "b")?;
        let mut report = sqldiff_core::diff(&a, &b, &self.options(args));
        self.reload_store()?;
        self.finish(&mut report, &a, Some(&b), &sqldiff_review::time::now());
        let files = fa.zip(fb).map(|(x, y)| (x, Some(y)));
        let id = self.hold(report, files);
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
        self.pairs.insert(pair.id.clone(), Held { pair, files });
    }

    /// A held pair's report with session flips applied.
    fn view(&self, id: &str) -> Option<Report> {
        self.pairs
            .get(id)
            .map(|h| queue::view(&h.pair.report, &self.flips))
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
        let structured = json!({
            "pair": pair,
            "pair_id": id,
            "finding": f,
            "assumptions": assumptions,
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
            "verdict_before_suppression": report.verdict,
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
        let report = queue::view(&held.pair.report, &self.flips);
        let ledger = queue::ledger(&held.pair.report, &self.flips);
        let mut text = format!(
            "{}  [{id}]\nVerdict: {}\n",
            pair_label(&report),
            report.verdict.label().to_uppercase()
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
            json!({ "pair_id": id, "verdict": report.verdict, "assumptions": ledger }),
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
        let mut changed = Vec::new();
        let mut affected = 0;
        for (k, h) in &self.pairs {
            if !h.pair.report.assumptions.iter().any(|a| a.id == aid) {
                continue;
            }
            affected += 1;
            let now = queue::view(&h.pair.report, &self.flips).open_verdict();
            if before.get(k).copied().flatten() != Some(now) {
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

fn status_label(s: queue::Status) -> &'static str {
    match s {
        queue::Status::Open => "open (tool default, nobody declared it)",
        queue::Status::Declared => "declared (pack or config)",
        queue::Status::Proven => "proven from composed output",
        queue::Status::Confirmed => "confirmed in this session",
        queue::Status::Denied => "denied in this session",
    }
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
        a.version = ver.to_string();
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
            "description": "Confirm (holds=true) or deny (holds=false) an assumption for this session only, or reset it (holds=null); every held pair's verdict is re-derived and changes are listed. A denied assumption makes the findings that rely on it unknown. Nothing is written to packs, config or sources.",
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
    ])
}
