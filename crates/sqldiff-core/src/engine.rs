//! Entry points: diff two artifacts, or check one.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use crate::artifact::{content_hash, Analyzed, Artifact};
use crate::bind::{self, BindInput, Bindings};
use crate::catalog::builtins::Helper;
use crate::catalog::{ddl, Catalog, CatalogView};
use crate::classify::pairing_key;
use crate::equiv;
use crate::facts;
use crate::lint;
use crate::model::{
    fingerprint, Assumption, ChangeClass, Finding, FingerprintInput, Location, NoiseCounts, Report,
    Severity, Side, Verdict,
};
use crate::normalize::lcs_pairs;
use crate::rules;
use crate::treediff;

#[derive(Debug, Clone)]
pub struct Options {
    /// Maximum lines of excerpt per location.
    pub excerpt_lines: usize,
    /// Catalog DDL for side A (`CREATE TABLE`, `CREATE UNIQUE INDEX`,
    /// `CREATE PROCEDURE`, ...), each read in its own dialect. Binding checks
    /// names against it; statement equivalence uses keys, nullability and types
    /// to discharge side conditions. `check` uses these for its one artifact.
    pub catalog_a: Vec<Artifact>,
    /// Catalog DDL for side B.
    pub catalog_b: Vec<Artifact>,
    /// Structured catalog facts for side A (adapter `get_catalog`, snapshot bundles).
    pub facts_a: Vec<Arc<Catalog>>,
    /// Structured catalog facts for side B.
    pub facts_b: Vec<Arc<Catalog>>,
    /// Pack-supplied helper functions, added to the built-in catalog.
    pub helpers: Vec<Helper>,
    /// Session assumption flips (id → holds). Flips of scenario assumptions
    /// (`ir::behaviour::RERUN_ASSUMPTIONS`) re-run the behavioural analysis
    /// under the flipped environment; the rest are applied to verdicts by
    /// the caller (see `sqldiff_source::queue::apply`).
    pub flips: BTreeMap<String, bool>,
    /// Facts declared in packs or project config, by assumption id (the
    /// declaration's text). A declared Db2 for i commit level is used for
    /// routines that do not state one.
    pub declared: BTreeMap<String, String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            excerpt_lines: 3,
            catalog_a: Vec::new(),
            catalog_b: Vec::new(),
            facts_a: Vec::new(),
            facts_b: Vec::new(),
            helpers: Vec::new(),
            flips: BTreeMap::new(),
            declared: BTreeMap::new(),
        }
    }
}

/// Catalog DDL files folded into one catalog in order, so a later migration's
/// `ALTER TABLE` applies to a table an earlier file created. Cached by paths and
/// content (tree diffs reuse one catalog for every pair); the cache is bounded
/// because the GUI and MCP server run for a long time.
fn ddl_catalog(dialect: sqldiff_syntax::Dialect, ddl: &[Artifact]) -> Arc<Catalog> {
    type Cache = Mutex<HashMap<String, Arc<Catalog>>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    const MAX_ENTRIES: usize = 32;
    let mut key = dialect.id().to_string();
    for a in ddl {
        key.push('\0');
        key.push_str(&a.path);
        key.push('\0');
        key.push_str(a.dialect.id());
        key.push('\0');
        key.push_str(&content_hash(&a.text));
    }
    let cache = CACHE.get_or_init(Default::default);
    if let Some(c) = cache.lock().ok().and_then(|m| m.get(&key).cloned()) {
        return c;
    }
    let mut cat = Catalog {
        origin: ddl
            .iter()
            .map(|a| a.path.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        ..Catalog::default()
    };
    for a in ddl {
        let root = sqldiff_syntax::parse(&a.text, a.dialect).syntax();
        ddl::load_tree(&mut cat, &root, a.dialect);
    }
    let c = Arc::new(cat);
    if let Ok(mut m) = cache.lock() {
        if m.len() >= MAX_ENTRIES {
            m.clear();
        }
        m.insert(key, c.clone());
    }
    c
}

/// The catalog one side binds against.
fn catalog_view(
    dialect: sqldiff_syntax::Dialect,
    ddl: &[Artifact],
    facts: &[Arc<Catalog>],
) -> CatalogView {
    let loaded = (!ddl.is_empty()).then(|| ddl_catalog(dialect, ddl));
    let sources: Vec<&Catalog> = loaded.iter().chain(facts).map(|c| c.as_ref()).collect();
    CatalogView::new(dialect, &sources)
}

pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Compare two artifacts. Same dialect: version review (UC2). Different
/// dialects: drift and conversion review (UC1, UC3).
pub fn diff(a: &Artifact, b: &Artifact, opts: &Options) -> Report {
    let aa = Analyzed::new(a, Side::A, opts.excerpt_lines);
    let ab = Analyzed::new(b, Side::B, opts.excerpt_lines);
    let same = a.dialect == b.dialect;

    let mut findings = Vec::new();
    let mut assumptions: Vec<Assumption> = Vec::new();
    let mut normalizations = vec![
        "whitespace, line breaks and comments".to_string(),
        "keyword case".to_string(),
        "statement terminators".to_string(),
    ];
    let mut unknown_reasons = Vec::new();
    let mut noise = NoiseCounts::default();
    // One catalog per side serves binding and statement equivalence.
    let view_a = catalog_view(a.dialect, &opts.catalog_a, &opts.facts_a);
    let view_b = catalog_view(b.dialect, &opts.catalog_b, &opts.facts_b);
    let schema_a = equiv::schema::Schema::from_view(&view_a);
    let schema_b = equiv::schema::Schema::from_view(&view_b);
    let mut eq = equiv::Outcome::default();

    if same {
        let mut td = treediff::diff(&aa, &ab);
        equiv::refine_modified(&aa, &ab, &schema_a, &schema_b, &mut td.findings, &mut eq);
        equiv::edit::decide(&aa, &ab, &schema_a, &schema_b, &mut td.findings, &mut eq);
        findings.extend(td.findings);
        noise.cosmetic = td.cosmetic;
        noise.structural = td.structural + eq.structural;
        match a.dialect.lexical().folding {
            sqldiff_syntax::IdentFolding::Lower => {
                normalizations.push("unquoted identifiers folded to lower case".to_string())
            }
            sqldiff_syntax::IdentFolding::Upper => {
                normalizations.push("unquoted identifiers folded to upper case".to_string())
            }
            sqldiff_syntax::IdentFolding::Collation => {
                normalizations.push("identifier quoting ([x], \"x\", x)".to_string());
                if td.used_case_insensitive_idents {
                    normalizations.push("identifier and variable case".to_string());
                    assumptions.push(equiv::canon::asm::make(equiv::canon::asm::TSQL_CI_NAMES));
                }
            }
        }
        // Lints that are new in B.
        let old: HashSet<String> = lint::all_hits(&aa)
            .into_iter()
            .map(|h| format!("{}|{}", h.rule, h.key))
            .collect();
        for h in lint::all_hits(&ab) {
            if !old.contains(&format!("{}|{}", h.rule, h.key)) {
                findings.push(lint::to_finding(&ab, &h));
            }
        }
        // Idiom-rule lints that are new in B (design section 8).
        let old = rules::analysis::hits(&aa, &schema_a, false);
        for h in rules::analysis::new_hits(&old, rules::analysis::hits(&ab, &schema_b, false)) {
            let v = if h.diff_different {
                Verdict::Different
            } else {
                Verdict::Unknown
            };
            findings.push(rules::analysis::to_finding(&ab, &h, v));
        }
    } else {
        findings.extend(pair_writes(&aa, &ab));
        for (side, schema) in [(&aa, &schema_a), (&ab, &schema_b)] {
            for h in lint::all_hits(side) {
                findings.push(lint::to_finding(side, &h));
            }
            // Rule lints describe one side's code: they do not decide the verdict.
            for h in rules::analysis::hits(side, schema, false) {
                findings.push(rules::analysis::to_finding(side, &h, Verdict::Unknown));
            }
        }
        eq = equiv::compare(&aa, &ab, &schema_a, &schema_b);
        findings.append(&mut eq.findings);
        noise.structural += eq.structural;
        unknown_reasons.append(&mut eq.unknown_reasons);
        normalizations.push(
            "statement lowering: parameters by position, aliases resolved, dialect built-ins and idioms mapped to one form".to_string(),
        );
    }
    for x in eq.normalizations.drain(..) {
        if !normalizations.contains(&x) {
            normalizations.push(x);
        }
    }
    for x in eq.assumptions.drain(..) {
        if !assumptions.iter().any(|y| y.id == x.id) {
            assumptions.push(x);
        }
    }

    let bound_a = bind::bind(
        &aa,
        &BindInput {
            view: &view_a,
            helpers: &opts.helpers,
        },
    );
    let bound_b = bind::bind(
        &ab,
        &BindInput {
            view: &view_b,
            helpers: &opts.helpers,
        },
    );
    let (bindf, binda) = bind::binding_findings(&Bindings {
        a: (&aa, &bound_a, view_a.supplied),
        b: Some((&ab, &bound_b, view_b.supplied)),
    });
    absorb_residue(&mut findings, &bindf);
    findings.extend(bindf);
    for x in binda {
        if !assumptions.iter().any(|y| y.id == x.id) {
            assumptions.push(x);
        }
    }
    if view_a.supplied || view_b.supplied {
        normalizations.push("names bound against the catalog".to_string());
    }

    let env = crate::ir::behaviour::Env::from_options(opts);
    let beh = crate::ir::behaviour::analyze(&aa, &ab, &env);
    let witnesses = beh.witnesses;
    findings.extend(beh.findings);
    for x in beh.assumptions {
        if !assumptions.iter().any(|y| y.id == x.id) {
            assumptions.push(x);
        }
    }

    findings.extend(crate::ledger::contradictions(&[&aa, &ab], opts));
    let mut discharged = crate::ledger::discharges(&aa);
    discharged.extend(crate::ledger::discharges(&ab));

    let mut diagnostics = aa.diagnostics();
    diagnostics.extend(ab.diagnostics());
    if !diagnostics.is_empty() {
        unknown_reasons.push(format!(
            "{} parse problem(s); affected statements may be misaligned",
            diagnostics.len()
        ));
    }

    finish(Report {
        tool_version: TOOL_VERSION.to_string(),
        verdict: Verdict::Equal,
        a: aa.info(),
        b: Some(ab.info()),
        findings,
        noise,
        normalizations,
        assumptions,
        unknown_reasons,
        suppressed: Vec::new(),
        verdict_before_suppression: None,
        diagnostics,
        witnesses,
        discharged,
    })
}

/// "What if this statement fails?" (design §6.1 `what_if`): the statement
/// on or before `line` of side `side`, failing with `kind`, under the
/// environment `opts.flips` and `opts.declared` describe.
pub fn what_if(
    a: &Artifact,
    b: &Artifact,
    opts: &Options,
    side: Side,
    line: usize,
    kind: crate::ir::ErrorKind,
) -> crate::ir::witness::WhatIf {
    let aa = Analyzed::new(a, Side::A, opts.excerpt_lines);
    let ab = Analyzed::new(b, Side::B, opts.excerpt_lines);
    let env = crate::ir::behaviour::Env::from_options(opts);
    crate::ir::behaviour::what_if(&aa, &ab, side, line, kind, &env)
}

/// Transaction overlay brackets of both sides (design §6.2).
pub fn overlay(a: &Artifact, b: &Artifact) -> crate::ir::overlay::Overlay {
    let aa = Analyzed::new(a, Side::A, 1);
    let ab = Analyzed::new(b, Side::B, 1);
    crate::ir::overlay::overlay(&aa, &ab)
}

/// Lint one artifact on its own.
pub fn check(a: &Artifact, opts: &Options) -> Report {
    let aa = Analyzed::new(a, Side::A, opts.excerpt_lines);
    let view = catalog_view(a.dialect, &opts.catalog_a, &opts.facts_a);
    let schema = equiv::schema::Schema::from_view(&view);
    let mut findings: Vec<Finding> = lint::all_hits(&aa)
        .iter()
        .map(|h| lint::to_finding(&aa, h))
        .collect();
    for h in rules::analysis::hits(&aa, &schema, true) {
        findings.push(rules::analysis::to_finding(&aa, &h, Verdict::Unknown));
    }
    let bound = bind::bind(
        &aa,
        &BindInput {
            view: &view,
            helpers: &opts.helpers,
        },
    );
    let (bindf, assumptions) = bind::binding_findings(&Bindings {
        a: (&aa, &bound, view.supplied),
        b: None,
    });
    absorb_residue(&mut findings, &bindf);
    findings.extend(bindf);
    let diagnostics = aa.diagnostics();
    let mut unknown_reasons = Vec::new();
    if !diagnostics.is_empty() {
        unknown_reasons.push(format!("{} parse problem(s)", diagnostics.len()));
    }
    finish(Report {
        tool_version: TOOL_VERSION.to_string(),
        verdict: Verdict::Equal,
        a: aa.info(),
        b: None,
        findings,
        noise: NoiseCounts::default(),
        normalizations: Vec::new(),
        assumptions,
        unknown_reasons,
        suppressed: Vec::new(),
        verdict_before_suppression: None,
        diagnostics,
        witnesses: Vec::new(),
        discharged: Vec::new(),
    })
}

/// A proven binding error (`GETDATE()` in PostgreSQL fails to bind) says more
/// than the residue hit on the same text, so the residue finding is dropped.
fn absorb_residue(findings: &mut Vec<Finding>, binding: &[Finding]) {
    let overlaps = |x: &Option<Location>, y: &Option<Location>| match (x, y) {
        (Some(x), Some(y)) => x.byte_start < y.byte_end && y.byte_start < x.byte_end,
        _ => false,
    };
    findings.retain(|f| {
        f.class != ChangeClass::Residue
            || !binding.iter().any(|b| {
                b.verdict == Verdict::Different && (overlaps(&f.a, &b.a) || overlaps(&f.b, &b.b))
            })
    });
}

pub(crate) fn finish(mut r: Report) -> Report {
    // Same fingerprint twice (e.g. the same residue on two lines): keep both, they have different locations.
    r.findings.sort_by(|x, y| {
        y.severity
            .cmp(&x.severity)
            .then(y.verdict.cmp(&x.verdict))
            .then(first_line(x).cmp(&first_line(y)))
    });
    r.settle();
    r
}

fn first_line(f: &Finding) -> (usize, usize) {
    (
        f.a.as_ref().map(|l| l.start_line).unwrap_or(usize::MAX),
        f.b.as_ref().map(|l| l.start_line).unwrap_or(usize::MAX),
    )
}

/// Inside a CATCH block, an EXCEPTION section or a condition handler.
fn in_error_handler(node: &sqldiff_syntax::SyntaxNode) -> bool {
    use sqldiff_syntax::SyntaxKind as K;
    node.ancestors().any(|n| {
        matches!(
            n.kind(),
            K::CATCH_BLOCK | K::EXCEPTION_SECTION | K::EXCEPTION_HANDLER | K::HANDLER
        )
    })
}

/// Cross-dialect: every durable write on one side should have a counterpart
/// (same kind, same target object after case and `_` folding) on the other.
///
/// Writes on the normal path are matched in order. Writes inside error
/// handlers are matched by key only, and reported once per key: one
/// EXCEPTION block that records a failure covers several CATCH blocks that
/// each record it, so how often and in what order they occur is not compared
/// here (the behavioural diff compares the error paths themselves).
fn pair_writes(a: &Analyzed, b: &Analyzed) -> Vec<Finding> {
    // A T-SQL trigger for several events became one PostgreSQL function per
    // event, each with the trigger's writes: pair them per function.
    if let Some(fns) = crate::equiv::trigger_functions(a, b) {
        let mut out: Vec<Finding> = Vec::new();
        for f in &fns {
            for x in pair_writes_in(a, b, Some(f)) {
                if !out.iter().any(|y| y.fingerprint == x.fingerprint) {
                    out.push(x);
                }
            }
        }
        return out;
    }
    pair_writes_in(a, b, None)
}

/// `pair_writes` with side B's writes limited to those inside `within`.
fn pair_writes_in(
    a: &Analyzed,
    b: &Analyzed,
    within: Option<&sqldiff_syntax::SyntaxNode>,
) -> Vec<Finding> {
    // MERGE and INSERT ... ON CONFLICT are both upserts.
    let key = |w: &facts::Write| {
        let kind = if w.upsert {
            "Upsert".to_string()
        } else {
            format!("{:?}", w.kind)
        };
        format!("{kind}:{}", pairing_key(std::slice::from_ref(&w.target)))
    };
    let target_of = |k: &String| {
        k.split_once(':')
            .map(|x| x.1.to_string())
            .unwrap_or_default()
    };
    let split = |x: &Analyzed| -> (Vec<facts::Write>, Vec<facts::Write>) {
        facts::writes(x)
            .into_iter()
            .filter(|w| {
                x.side == Side::A
                    || within.map_or(true, |f| f.text_range().contains_range(w.node.text_range()))
            })
            .partition(|w| !in_error_handler(&w.node))
    };
    let (wa, ha) = split(a);
    let (wb, hb) = split(b);
    let ka: Vec<String> = wa.iter().map(key).collect();
    let kb: Vec<String> = wb.iter().map(key).collect();
    let kha: Vec<String> = ha.iter().map(key).collect();
    let khb: Vec<String> = hb.iter().map(key).collect();
    let unpaired = |side: &Analyzed,
                    other: &Analyzed,
                    w: &facts::Write,
                    anchor: &str,
                    (verdict, tail): (Verdict, &str),
                    place: &str| {
        let witness = format!(
            "Side {:?} ({}) runs {} {}{place}; on side {:?} ({}) {tail}.",
            side.side,
            side.dialect().display_name(),
            crate::treediff::article(crate::classify::StmtKind::label(w.kind)),
            match w.kind {
                crate::classify::StmtKind::Insert | crate::classify::StmtKind::Merge =>
                    format!("into {}", w.target),
                crate::classify::StmtKind::Delete => format!("from {}", w.target),
                _ => format!("of {}", w.target),
            },
            other.side,
            other.dialect().display_name(),
        );
        let is_a = side.side == Side::A;
        let loc = side.node_location(&w.node);
        let class = ChangeClass::Semantic;
        Finding {
            fingerprint: fingerprint(&FingerprintInput {
                object: &a.object,
                class,
                rule: "write.unpaired",
                anchor_a: if is_a { anchor } else { "" },
                anchor_b: if is_a { "" } else { anchor },
            }),
            rule: "write.unpaired".to_string(),
            class,
            severity: Severity::High,
            verdict,
            witness,
            a: is_a.then(|| loc.clone()),
            b: (!is_a).then_some(loc),
            assumptions: Vec::new(),
        }
    };
    const SAME_TABLE: &str = "the same table is written by a different kind of statement";

    let pairs = lcs_pairs(&ka, &kb);
    let matched_a: HashSet<usize> = pairs.iter().map(|p| p.0).collect();
    let matched_b: HashSet<usize> = pairs.iter().map(|p| p.1).collect();
    // Lookups built once: keys, targets and the normal-path writes left over
    // after in-order matching, per side and family.
    let set = |ks: &[String]| -> HashSet<String> { ks.iter().cloned().collect() };
    let targets = |ks: &[String]| -> HashSet<String> { ks.iter().map(&target_of).collect() };
    let leftover = |ks: &[String], m: &HashSet<usize>| -> HashSet<String> {
        ks.iter()
            .enumerate()
            .filter(|(i, _)| !m.contains(i))
            .map(|(_, k)| k.clone())
            .collect()
    };
    let (main_a, main_b) = (set(&ka), set(&kb));
    let (main_ta, main_tb) = (targets(&ka), targets(&kb));
    let (hand_a, hand_b) = (set(&kha), set(&khb));
    let (hand_ta, hand_tb) = (targets(&kha), targets(&khb));
    let (left_a, left_b) = (leftover(&ka, &matched_a), leftover(&kb, &matched_b));

    let mut out = Vec::new();
    // Normal path: only the other side's normal path can be a counterpart.
    for (side, other, ws, keys, matched, o_main, o_main_t, o_hand) in [
        (a, b, &wa, &ka, &matched_a, &main_b, &main_tb, &hand_b),
        (b, a, &wb, &kb, &matched_b, &main_a, &main_ta, &hand_a),
    ] {
        for (i, w) in ws.iter().enumerate() {
            if matched.contains(&i) {
                continue;
            }
            let k = &keys[i];
            let outcome = if o_main.contains(k) {
                (Verdict::Unknown, "it appears there in a different order")
            } else if o_main_t.contains(&target_of(k)) {
                (Verdict::Unknown, SAME_TABLE)
            } else if o_hand.contains(k) {
                (
                    Verdict::Different,
                    "it is written there only in an error handler",
                )
            } else {
                (Verdict::Different, "it has no corresponding write")
            };
            let place = format!(" (line {})", side.line_of(&w.node));
            out.push(unpaired(side, other, w, k, outcome, &place));
        }
    }
    // Error handlers: by key, once per key.
    for (side, other, ws, keys, o_hand, o_hand_t, o_left) in [
        (a, b, &ha, &kha, &hand_b, &hand_tb, &left_b),
        (b, a, &hb, &khb, &hand_a, &hand_ta, &left_a),
    ] {
        let mut count: HashMap<&String, usize> = HashMap::new();
        for k in keys.iter() {
            *count.entry(k).or_default() += 1;
        }
        let mut seen = HashSet::new();
        for (w, k) in ws.iter().zip(keys) {
            if o_hand.contains(k) || !seen.insert(k) {
                continue;
            }
            let outcome = if o_left.contains(k) {
                // A normal-path write the in-order matching left over may be
                // where the other side records the same thing.
                (
                    Verdict::Unknown,
                    "it is written there outside an error handler",
                )
            } else if o_hand_t.contains(&target_of(k)) {
                (
                    Verdict::Unknown,
                    "an error handler writes the same table with a different kind of statement",
                )
            } else {
                (Verdict::Different, "no error handler writes it")
            };
            let more = count[k] - 1;
            let place = format!(
                " in an error handler (line {}{})",
                side.line_of(&w.node),
                if more > 0 {
                    format!(", and {more} more")
                } else {
                    String::new()
                }
            );
            out.push(unpaired(
                side,
                other,
                w,
                &format!("handler:{k}"),
                outcome,
                &place,
            ));
        }
    }
    out
}
