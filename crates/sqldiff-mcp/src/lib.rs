//! Local stdio MCP server for SQLDiff (design section 6.1).
//!
//! Speaks JSON-RPC 2.0 over newline-delimited stdio. Tools are coarse,
//! deterministic and token-bounded: `diff` and `check` return a verdict and
//! compact finding cards; `finding` returns one card in full from this
//! session's results. The server only reads files and never writes anywhere.

use std::collections::HashMap;

use serde_json::{json, Map, Value};
use sqldiff_core::model::{Assumption, Location};
use sqldiff_core::{Artifact, Dialect, Finding, Options, Report, TOOL_VERSION};

/// Protocol revisions this server understands, newest first.
pub const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// Default cap on finding cards per `diff` or `check` result.
const DEFAULT_MAX_FINDINGS: usize = 20;
/// Findings kept for `finding` lookups before the cache is reset.
const CACHE_LIMIT: usize = 10_000;

const INSTRUCTIONS: &str = "SQLDiff compares SQL artifacts in T-SQL (SQL Server 2022), \
PostgreSQL 18 and Db2 for i. Verdicts are deterministic and never a false \"equal\": \
\"equal_under_assumptions\" lists the assumptions it relies on, and \"unknown\" says why \
equivalence could not be shown. Use `diff` for two versions of one object or one object in \
two dialects, `check` to lint a single file, and `finding` to expand a card by fingerprint.";

/// A finding with the context needed to show it in full later.
struct Cached {
    finding: Finding,
    assumptions: Vec<Assumption>,
    pair: String,
}

#[derive(Default)]
pub struct Server {
    findings: HashMap<String, Cached>,
}

impl Server {
    pub fn new() -> Self {
        Self::default()
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

    fn diff(&mut self, args: &Value) -> Result<(String, Value), String> {
        let a = artifact(args.get("a"), "a")?;
        let b = artifact(args.get("b"), "b")?;
        let report = sqldiff_core::diff(&a, &b, &options(args));
        Ok(self.summarize(&report, args))
    }

    fn check(&mut self, args: &Value) -> Result<(String, Value), String> {
        let a = artifact(args.get("artifact"), "artifact")?;
        let report = sqldiff_core::check(&a, &options(args));
        Ok(self.summarize(&report, args))
    }

    fn finding(&self, args: &Value) -> Result<(String, Value), String> {
        let fp = args
            .get("fingerprint")
            .and_then(Value::as_str)
            .ok_or("`fingerprint` is required")?;
        let c = self.findings.get(fp).ok_or_else(|| {
            format!("no finding `{fp}` in this session; run `diff` or `check` first")
        })?;
        let f = &c.finding;
        let assumptions: Vec<&Assumption> = c
            .assumptions
            .iter()
            .filter(|x| f.assumptions.contains(&x.id))
            .collect();
        let mut text = format!(
            "{} · {} · {} · {}  {} {}\n{}\n{}\n",
            c.pair,
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
                text.push_str(&format!("  {}  {}\n", x.id, x.text));
            }
        }
        let structured = json!({
            "pair": c.pair,
            "finding": f,
            "assumptions": assumptions,
        });
        Ok((text, structured))
    }

    /// Compact result: verdict, counts and up to `max_findings` cards, most
    /// severe first. Every finding is cached for `finding` lookups.
    fn summarize(&mut self, report: &Report, args: &Value) -> (String, Value) {
        let max = args
            .get("max_findings")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_FINDINGS);
        let pair = match &report.b {
            Some(b) => format!("{} ↔ {}", report.a.path, b.path),
            None => report.a.path.clone(),
        };
        if self.findings.len() + report.findings.len() > CACHE_LIMIT {
            self.findings.clear();
        }
        for f in &report.findings {
            self.findings.insert(
                f.fingerprint.clone(),
                Cached {
                    finding: f.clone(),
                    assumptions: report.assumptions.clone(),
                    pair: pair.clone(),
                },
            );
        }

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
            "verdict": report.verdict,
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
            "{pair}\nVerdict: {}\n{} finding(s)",
            report.verdict.label().to_uppercase(),
            report.findings.len()
        );
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

fn options(args: &Value) -> Options {
    let mut o = Options::default();
    if let Some(n) = args.get("excerpt_lines").and_then(Value::as_u64) {
        o.excerpt_lines = n as usize;
    }
    o
}

/// Build an artifact from `{path | text, dialect, name?, version?}`.
fn artifact(v: Option<&Value>, field: &str) -> Result<Artifact, String> {
    let v = v.and_then(Value::as_object).ok_or_else(|| {
        format!("`{field}` must be an object with `dialect` and `path` or `text`")
    })?;
    let dialect: Dialect = v
        .get("dialect")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{field}.dialect` is required (tsql, postgres or db2i)"))?
        .parse()?;
    let name = v.get("name").and_then(Value::as_str);
    let (path, text) = match (
        v.get("path").and_then(Value::as_str),
        v.get("text").and_then(Value::as_str),
    ) {
        (Some(p), None) => {
            let t = std::fs::read_to_string(p).map_err(|e| format!("{field}: {p}: {e}"))?;
            (name.unwrap_or(p).to_string(), t)
        }
        (None, Some(t)) => (name.unwrap_or(field).to_string(), t.to_string()),
        _ => return Err(format!("`{field}` needs exactly one of `path` or `text`")),
    };
    let mut a = Artifact::new(path, dialect, text);
    if let Some(ver) = v.get("version").and_then(Value::as_str) {
        a.version = ver.to_string();
    }
    Ok(a)
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
        "excerpt_lines": { "type": "integer", "minimum": 1, "description": "Excerpt lines kept per location (default 3)" },
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
            "description": "One finding from an earlier `diff` or `check` in this session, by fingerprint: witness, both locations with excerpts, and the text of each assumption it relies on.",
            "inputSchema": {
                "type": "object",
                "properties": { "fingerprint": { "type": "string" } },
                "required": ["fingerprint"],
            },
            "annotations": ro,
        },
    ])
}
