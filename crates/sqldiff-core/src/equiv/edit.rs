//! Same-dialect edits that the structural diff could only call "modified"
//! and statement equivalence could not decide: a witness search over the
//! two versions of the statement (or condition) with `eval`.
//!
//! A finding becomes "different" only when the search finds inputs under
//! which both versions run to completion and their results certainly
//! differ, and the result is observable: rows written to a table, a result
//! set, a returned value or raised error, a variable that is read later, or
//! a condition that sends the two versions into branches that do different
//! things. Inputs are parameters and columns (chosen freely, within catalog
//! nullability), constants the routine sets, and local variables whose
//! earlier assignments are not traced; a witness that needs the latter
//! carries an assumption that names them.
//!
//! The search runs one changed statement and gives both versions the same
//! inputs at it, so it holds only while no other change is involved: when
//! a value the statement reads (or a condition on the way to it) may come
//! from another changed statement, or what it writes reaches one, two
//! edits that make up for each other would look like two differences, and
//! the finding stays unknown.

use std::collections::{HashMap, HashSet};

use sqldiff_syntax::{ast as cst, Dialect, SyntaxKind, SyntaxNode};

use super::ast::{parse_stmt_ext as parse_stmt, tokens, Expr, Parser, Stmt, Tok, K};
use super::eval::{self, Class, Ctx, Found, Hint, Key, Miss, Obs, Origin, Probe, VarFact, V};
use super::lower;
use super::schema::{Schema, Ty};
use super::Outcome;
use crate::artifact::Analyzed;
use crate::classify::{stmt_kind, StmtKind};
use crate::model::{fingerprint, Assumption, ChangeClass, Finding, FingerprintInput, Verdict};
use crate::normalize;

const RULE: &str = "equiv.edit";

pub fn witness_path() -> Assumption {
    Assumption::default(
        "ASM-WITNESS-PATH",
        "The witness inputs reach the changed code and its different result reaches a later effect (some conditions, jumps or later statements on the way are not evaluated)",
    )
}

pub fn local_values() -> Assumption {
    Assumption::default(
        "ASM-LOCAL-VALUES",
        "Local variables can hold the witness values at the changed statement (their earlier assignments are not traced)",
    )
}

/// Re-examines "modified" statements and headers that are still unknown.
pub fn decide(
    a: &Analyzed,
    b: &Analyzed,
    sa: &Schema,
    sb: &Schema,
    findings: &mut [Finding],
    out: &mut Outcome,
) {
    if !findings.iter().any(candidate) {
        return;
    }
    let ra = Routine::new(a);
    let rb = Routine::new(b);
    // What every finding marks as changed, on each side.
    let spans: Vec<(Option<Span>, Option<Span>)> = findings
        .iter()
        .map(|f| {
            if f.verdict == Verdict::Equal {
                return (None, None);
            }
            let span =
                |l: &Option<crate::model::Location>| l.as_ref().map(|l| (l.byte_start, l.byte_end));
            (span(&f.a), span(&f.b))
        })
        .collect();
    for (i, f) in findings.iter_mut().enumerate() {
        if !candidate(f) {
            continue;
        }
        let (Some(la), Some(lb)) = (&f.a, &f.b) else {
            continue;
        };
        let (Some(na), Some(nb)) = (ra.node_at(la.byte_start), rb.node_at(lb.byte_start)) else {
            continue;
        };
        if std::env::var_os("SQLDIFF_DEBUG_EDIT").is_some() {
            eprintln!("edit: {} at A line {}", f.rule, la.start_line);
        }
        let others = spans.iter().enumerate().filter(|(j, _)| *j != i);
        let changed = (
            others.clone().filter_map(|(_, s)| s.0).collect(),
            others.filter_map(|(_, s)| s.1).collect(),
        );
        let r = examine(&ra, &rb, &na, &nb, sa, sb, &changed);
        if std::env::var_os("SQLDIFF_DEBUG_EDIT").is_some() {
            match &r {
                Ok(g) => eprintln!("  => different: {}", g.witness),
                Err(e) => eprintln!("  => {e}"),
            }
        }
        if let Ok(g) = r {
            for id in &g.assumptions {
                if *id == local_values().id {
                    out.assume(local_values());
                } else if *id == witness_path().id {
                    out.assume(witness_path());
                }
            }
            let class = f.class.max(ChangeClass::Semantic);
            *f = Finding {
                fingerprint: fingerprint(&FingerprintInput {
                    object: &a.object,
                    class,
                    rule: RULE,
                    anchor_a: &hash_node(&na),
                    anchor_b: &hash_node(&nb),
                }),
                rule: RULE.to_string(),
                class,
                severity: class.default_severity(),
                verdict: Verdict::Different,
                witness: g.witness,
                a: f.a.take(),
                b: f.b.take(),
                assumptions: g.assumptions,
            };
        }
    }
}

fn candidate(f: &Finding) -> bool {
    matches!(f.rule.as_str(), "stmt.modified" | "header.modified") && f.verdict == Verdict::Unknown
}

fn hash_node(n: &SyntaxNode) -> String {
    let toks: Vec<String> = cst::sig_tokens(n).map(|t| t.text().to_string()).collect();
    crate::normalize::hash_strs(toks.iter().map(String::as_str))
}

struct Decided {
    witness: String,
    assumptions: Vec<String>,
}

// ---- the routine around a statement -----------------------------------------------

/// Where a variable is assigned.
#[derive(Debug, Clone)]
struct Site {
    start: usize,
    end: usize,
    /// The assigned expression when it is a plain constant.
    constant: Option<Expr>,
    /// The value is a row count (`COUNT(*)`, `ROW_COUNT`).
    count: bool,
    /// A declaration (its position fixes the variable's initial value).
    declare: bool,
}

struct Routine<'x, 'a> {
    side: &'x Analyzed<'a>,
    d: Dialect,
    params: HashMap<String, (Option<Ty>, bool)>,
    locals: HashMap<String, Option<Ty>>,
    records: HashSet<String>,
    sites: HashMap<String, Vec<Site>>,
    strict: bool,
    /// Byte ranges of loop bodies.
    loops: Vec<(usize, usize)>,
    /// Significant tokens of the routine body, in order: (offset, upper text, kind).
    toks: Vec<Tok>,
    /// A view, or a T-SQL procedure or script (a bare SELECT returns a
    /// result set).
    result_sets: bool,
    /// PostgreSQL `LANGUAGE sql` function returning a set: the rows of its
    /// last statement are the result.
    sql_set_result: bool,
    /// Statements and conditions with what they read and write, built on first use.
    units: std::cell::OnceCell<Vec<Unit>>,
}

impl<'x, 'a> Routine<'x, 'a> {
    fn new(side: &'x Analyzed<'a>) -> Self {
        let d = side.dialect();
        let u = lower::lower(side);
        let mut params = HashMap::new();
        for p in &u.params {
            params.insert(p.name.clone(), (p.ty, p.out));
        }
        // Names the routine header declares, also when the routine name is
        // a template placeholder the header lowering does not accept.
        if let Some(r) = side
            .root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::ROUTINE)
        {
            for p in header_params(&r, d) {
                params.entry(p.name.clone()).or_insert((p.ty, p.out));
            }
        }
        let strict = d == Dialect::Postgres
            && side
                .root
                .descendants()
                .find(|n| n.kind() == SyntaxKind::ROUTINE)
                .is_some_and(|r| {
                    cst::header_tokens(&r)
                        .iter()
                        .any(|t| t.text().eq_ignore_ascii_case("STRICT"))
                });
        let sql_set_result = d == Dialect::Postgres
            && side
                .root
                .descendants()
                .find(|n| n.kind() == SyntaxKind::ROUTINE)
                .is_some_and(|r| {
                    let words: Vec<String> = cst::header_tokens(&r)
                        .iter()
                        .filter(|t| cst::is_significant(t.kind()))
                        .map(|t| t.text().to_ascii_uppercase())
                        .collect();
                    let pair = |a: &str, b: &[&str]| {
                        words
                            .windows(2)
                            .any(|w| w[0] == a && b.contains(&w[1].as_str()))
                    };
                    pair("LANGUAGE", &["SQL", "'SQL'"]) && pair("RETURNS", &["TABLE", "SETOF"])
                });
        let mut r = Routine {
            side,
            d,
            params,
            locals: HashMap::new(),
            records: HashSet::new(),
            sites: HashMap::new(),
            strict,
            loops: Vec::new(),
            toks: tokens(&side.root),
            result_sets: u.kind == lower::UnitKind::View
                || d == Dialect::Tsql
                    && matches!(u.kind, lower::UnitKind::Proc | lower::UnitKind::Script),
            sql_set_result,
            units: std::cell::OnceCell::new(),
        };
        r.scan();
        r
    }

    fn node_at(&self, offset: usize) -> Option<SyntaxNode> {
        self.side.root.descendants().find(|n| {
            (n.kind().is_unit() || n.kind() == SyntaxKind::STATEMENT)
                && cst::sig_range(n).0 == offset
        })
    }

    /// Declarations, assignment sites and loops.
    fn scan(&mut self) {
        let d = self.d;
        for n in self.side.root.descendants() {
            match n.kind() {
                SyntaxKind::LOOP_STMT => {
                    let (s, e) = cst::sig_range(&n);
                    self.loops.push((s, e));
                    // PL/pgSQL `FOR r IN query LOOP` / `FOR i IN 1..n LOOP`.
                    if let Some(c) = n.children().find(|c| c.kind() == SyntaxKind::CONDITION) {
                        let t = tokens(&c);
                        if t.len() >= 2 && t[1].up == "IN" {
                            let name = t[0].text.to_lowercase();
                            if t.get(2)
                                .is_some_and(|x| x.up == "SELECT" || x.up == "EXECUTE")
                            {
                                self.records.insert(name.clone());
                            }
                            self.site(&name, &c, None, false, false);
                        }
                    }
                }
                SyntaxKind::STATEMENT => self.scan_statement(&n, d),
                _ => {}
            }
        }
    }

    fn site(
        &mut self,
        name: &str,
        node: &SyntaxNode,
        constant: Option<Expr>,
        count: bool,
        declare: bool,
    ) {
        let (start, end) = cst::sig_range(node);
        self.sites.entry(name.to_string()).or_default().push(Site {
            start,
            end,
            constant,
            count,
            declare,
        });
    }

    fn scan_statement(&mut self, n: &SyntaxNode, d: Dialect) {
        let t = tokens(n);
        if t.is_empty() {
            return;
        }
        let in_declare_section = n
            .parent()
            .is_some_and(|p| p.kind() == SyntaxKind::DECLARE_SECTION);
        let constant = |e: &[Tok]| -> Option<Expr> {
            let mut p = Parser::ext(e, d);
            let x = p.expr().ok()?;
            if !p.at_end() {
                return None;
            }
            matches!(
                x,
                Expr::Num(_) | Expr::Str(_) | Expr::Null | Expr::Special(_)
            )
            .then_some(x)
        };
        let is_count = |e: &[Tok]| {
            e.len() >= 4
                && matches!(e[0].up.as_str(), "COUNT" | "COUNT_BIG")
                && e[1].kind == K::LParen
        };
        if in_declare_section {
            // `name [CONSTANT] type [:= | = | DEFAULT init];`
            let name = t[0].text.to_lowercase();
            let mut p = Parser::ext(&t[1..], d);
            let ty = p.ty().ok();
            let at = 1 + p.i;
            match &ty {
                Some(ty) if ty.name == "RECORD" => {
                    self.records.insert(name.clone());
                }
                _ => {}
            }
            if t.iter().any(|x| x.up.contains("%ROWTYPE") || x.up == "%") {
                self.records.insert(name.clone());
            }
            let ty = ty.map(|ty| Ty::from_type(&ty, d, false));
            self.locals.insert(name.clone(), ty);
            let init = t
                .get(at)
                .is_some_and(|x| x.text == ":=" || x.text == "=" || x.up == "DEFAULT");
            let c = if init {
                constant(&t[at + 1..])
            } else {
                Some(Expr::Null)
            };
            if init || c.is_some() {
                self.site(&name, n, c, false, true);
            }
            return;
        }
        let first = t[0].up.as_str();
        match first {
            "DECLARE" if d == Dialect::Tsql => {
                // DECLARE @a int = 1, @b nvarchar(10);
                let mut i = 1;
                while i < t.len() {
                    if t[i].kind != K::Var {
                        break;
                    }
                    let name = t[i].text.trim_start_matches('@').to_lowercase();
                    let mut j = i + 1;
                    if t.get(j).is_some_and(|x| x.up == "AS") {
                        j += 1;
                    }
                    if t.get(j)
                        .is_some_and(|x| x.up == "CURSOR" || x.up == "TABLE")
                    {
                        break;
                    }
                    let mut p = Parser::ext(&t[j..], d);
                    let ty = p.ty().ok().map(|ty| Ty::from_type(&ty, d, false));
                    self.locals.insert(name.clone(), ty);
                    let mut k = j + p.i;
                    let mut depth = 0;
                    let init_start = if t.get(k).is_some_and(|x| x.text == "=") {
                        k += 1;
                        Some(k)
                    } else {
                        None
                    };
                    while k < t.len() {
                        match t[k].kind {
                            K::LParen => depth += 1,
                            K::RParen => depth -= 1,
                            K::Comma if depth == 0 => break,
                            _ => {}
                        }
                        k += 1;
                    }
                    let c = match init_start {
                        Some(s) => constant(&t[s..k]),
                        None => Some(Expr::Null),
                    };
                    let count = init_start.is_some_and(|s| {
                        t.get(s).is_some_and(|x| x.kind == K::LParen)
                            && is_count(&t[(s + 1).min(t.len())..])
                    });
                    self.site(&name, n, c, count, true);
                    i = k + 1;
                }
            }
            "SET" if t.len() >= 3 && (t[1].kind == K::Var || t[2].text.ends_with('=')) => {
                let name = t[1].text.trim_start_matches('@').to_lowercase();
                let plain = t[2].text == "=";
                let c = if plain { constant(&t[3..]) } else { None };
                self.site(&name, n, c, false, false);
            }
            "SELECT" if d == Dialect::Tsql => {
                // SELECT [ALL | DISTINCT] [TOP n | TOP (n) [PERCENT] [WITH TIES]] @a = x, @b = y FROM ...
                let mut list = 1;
                if t.get(list)
                    .is_some_and(|x| x.up == "ALL" || x.up == "DISTINCT")
                {
                    list += 1;
                }
                if t.get(list).is_some_and(|x| x.up == "TOP") {
                    list += 1;
                    if t.get(list).is_some_and(|x| x.kind == K::LParen) {
                        let mut depth = 0;
                        while let Some(x) = t.get(list) {
                            match x.kind {
                                K::LParen => depth += 1,
                                K::RParen => depth -= 1,
                                _ => {}
                            }
                            list += 1;
                            if depth == 0 {
                                break;
                            }
                        }
                    } else {
                        list += 1;
                    }
                    if t.get(list).is_some_and(|x| x.up == "PERCENT") {
                        list += 1;
                    }
                    if t.get(list).is_some_and(|x| x.up == "WITH")
                        && t.get(list + 1).is_some_and(|x| x.up == "TIES")
                    {
                        list += 2;
                    }
                }
                let mut depth = 0;
                for (i, x) in t.iter().enumerate() {
                    match x.kind {
                        K::LParen => depth += 1,
                        K::RParen => depth -= 1,
                        K::Var if depth == 0 && t.get(i + 1).is_some_and(|y| y.text == "=") => {
                            let prev_ok = i == list || t[i - 1].kind == K::Comma;
                            if prev_ok {
                                let name = x.text.trim_start_matches('@').to_lowercase();
                                let count = is_count(&t[i + 2..]);
                                self.site(&name, n, None, count, false);
                            }
                        }
                        K::Word if depth == 0 && x.up == "FROM" => break,
                        _ => {}
                    }
                }
            }
            "GET" => {
                // GET DIAGNOSTICS v = ROW_COUNT
                if let Some(i) = t.iter().position(|x| x.text == "=" || x.text == ":=") {
                    if i >= 1 {
                        let name = t[i - 1].text.to_lowercase();
                        let count = t.get(i + 1).is_some_and(|x| x.up == "ROW_COUNT");
                        self.site(&name, n, None, count, false);
                    }
                }
            }
            _ => {}
        }
        // PL/pgSQL `v := e` / `v = e`.
        if d != Dialect::Tsql
            && t.len() >= 3
            && matches!(t[0].kind, K::Word | K::Quoted)
            && (t[1].text == ":=" || (t[1].text == "=" && !matches!(first, "SET" | "UPDATE")))
        {
            let name = t[0].text.to_lowercase();
            self.site(&name, n, constant(&t[2..]), false, false);
        }
        if d != Dialect::Tsql && t.len() >= 4 && t[1].kind == K::LParen {
            // `arr[0] := e` style targets are not traced.
        }
        // INTO targets: SELECT ... INTO a, b / FETCH c INTO a / RETURNING ... INTO / EXECUTE ... INTO.
        let mut depth = 0;
        let mut i = 0;
        while i < t.len() {
            match t[i].kind {
                K::LParen => depth += 1,
                K::RParen => depth -= 1,
                K::Word if depth == 0 && t[i].up == "INTO" && first != "INSERT" => {
                    let count = first == "SELECT" && is_count(&t[1..]);
                    let mut j = i + 1;
                    if t.get(j).is_some_and(|x| x.up == "STRICT") {
                        j += 1;
                    }
                    while let Some(x) = t.get(j) {
                        if !matches!(x.kind, K::Word | K::Var | K::Quoted) || is_clause(&x.up) {
                            break;
                        }
                        let name = x.text.trim_start_matches('@').to_lowercase();
                        self.site(&name, n, None, count, false);
                        j += 1;
                        if t.get(j).is_some_and(|x| x.kind == K::Comma) {
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    i = j;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        // EXEC ... @v OUTPUT.
        if d == Dialect::Tsql {
            for (i, x) in t.iter().enumerate() {
                if x.kind == K::Var
                    && t.get(i + 1)
                        .is_some_and(|y| y.up == "OUTPUT" || y.up == "OUT")
                {
                    let name = x.text.trim_start_matches('@').to_lowercase();
                    self.site(&name, n, None, false, false);
                }
            }
        }
    }

    fn in_loop(&self, at: usize) -> Option<(usize, usize)> {
        self.loops
            .iter()
            .filter(|(s, e)| *s < at && at < *e)
            .min_by_key(|(s, e)| e - s)
            .copied()
    }

    /// Facts about every parameter and variable, as seen at `at`.
    fn facts(&self, at: usize, node: &SyntaxNode) -> HashMap<String, VarFact> {
        let mut out = HashMap::new();
        let names: HashSet<&String> = self.params.keys().chain(self.locals.keys()).collect();
        let lp = self.in_loop(at);
        for name in names {
            let sites = self.sites.get(name).cloned().unwrap_or_default();
            let ty = self
                .params
                .get(name)
                .map(|p| p.0)
                .or_else(|| self.locals.get(name).copied())
                .flatten();
            let is_param = self.params.contains_key(name);
            let assigns: Vec<&Site> = sites.iter().filter(|s| !s.declare).collect();
            let mut f = VarFact {
                ty,
                origin: Origin::Assumed,
                value: None,
                nullable: true,
                non_negative: false,
                cands: Vec::new(),
            };
            if is_param && assigns.is_empty() {
                f.origin = Origin::Free;
                f.nullable = !self.strict;
            } else if !is_param {
                // Constant: one site, a constant, before `at` and not repeated by a loop.
                let reachable: Vec<&Site> = sites
                    .iter()
                    .filter(|s| {
                        s.start < at || lp.is_some_and(|(ls, le)| ls <= s.start && s.end <= le)
                    })
                    .collect();
                if let [only] = reachable.as_slice() {
                    if let (Some(c), true) = (
                        &only.constant,
                        sites.len() == 1
                            || only.declare
                                && sites.iter().all(|s| {
                                    s.start > at
                                        && lp.map_or(true, |(ls, le)| {
                                            !(ls <= s.start && s.end <= le)
                                        })
                                }),
                    ) {
                        if let Some(v) = constant_value(c) {
                            f.origin = Origin::Fixed;
                            f.value = Some(v);
                        }
                    }
                }
                if f.origin != Origin::Fixed
                    && !reachable.is_empty()
                    && reachable.iter().all(|s| s.count)
                {
                    f.origin = Origin::Free;
                    f.non_negative = true;
                    f.nullable = false;
                }
            }
            out.insert(name.clone(), f);
        }
        // T-SQL system values that depend on the previous statement.
        if self.d == Dialect::Tsql {
            let prev_reads_table = prev_statement(node).is_some_and(|p| {
                let k = stmt_kind(&p, self.d);
                matches!(
                    k,
                    StmtKind::Select
                        | StmtKind::Insert
                        | StmtKind::Update
                        | StmtKind::Delete
                        | StmtKind::Merge
                ) || cst::first_words(&p, 1)
                    .first()
                    .is_some_and(|w| w == "FETCH")
            });
            let origin = if prev_reads_table {
                Origin::Free
            } else {
                Origin::Assumed
            };
            out.insert(
                "@rowcount".into(),
                VarFact {
                    ty: Some(Ty::of(super::schema::Fam::Int)),
                    origin,
                    value: None,
                    nullable: false,
                    non_negative: true,
                    cands: vec![V::Int(0), V::Int(1), V::Int(2)],
                },
            );
            out.insert(
                "@fetch_status".into(),
                VarFact {
                    ty: Some(Ty::of(super::schema::Fam::Int)),
                    origin,
                    value: None,
                    nullable: false,
                    non_negative: false,
                    cands: vec![V::Int(0), V::Int(-1)],
                },
            );
        } else {
            out.insert(
                "found".into(),
                VarFact {
                    ty: Some(Ty::of(super::schema::Fam::Bool)),
                    origin: Origin::Assumed,
                    value: None,
                    nullable: false,
                    non_negative: false,
                    cands: vec![V::Bool(true), V::Bool(false)],
                },
            );
        }
        out
    }

    fn is_out(&self, name: &str) -> bool {
        self.params.get(name).is_some_and(|p| p.1)
    }

    /// Whether a table is a temporary table or table variable that is not read after the statement.
    fn dead_table(&self, table: &str, end: usize) -> bool {
        let temp = table.starts_with('#') || table.starts_with('@');
        if !temp {
            return false;
        }
        !self
            .toks
            .iter()
            .any(|t| t.off >= end && t.text.eq_ignore_ascii_case(table))
    }
}

fn header_params(r: &SyntaxNode, d: Dialect) -> Vec<lower::Param> {
    let toks: Vec<Tok> = cst::header_tokens(r)
        .iter()
        .map(Tok::from_syntax)
        .filter(|t| t.kind != K::Semi)
        .collect();
    let Some(kw) = toks
        .iter()
        .position(|t| matches!(t.up.as_str(), "FUNCTION" | "PROCEDURE" | "PROC"))
    else {
        return Vec::new();
    };
    let list: &[Tok] = if d == Dialect::Tsql {
        let Some(first) = toks[kw..]
            .iter()
            .position(|t| t.kind == K::Var || t.kind == K::LParen)
        else {
            return Vec::new();
        };
        let start = kw + first;
        if toks[start].kind == K::LParen {
            let close = lower::matching(&toks, start);
            &toks[start + 1..close.min(toks.len())]
        } else {
            &toks[start..lower::tsql_params_end(&toks, start)]
        }
    } else {
        let Some(open) = toks[kw..].iter().position(|t| t.kind == K::LParen) else {
            return Vec::new();
        };
        let open = kw + open;
        let close = lower::matching(&toks, open);
        &toks[open + 1..close.min(toks.len())]
    };
    lower::split_commas(list)
        .into_iter()
        .filter_map(|p| lower::param(p, d))
        .collect()
}

fn is_clause(up: &str) -> bool {
    matches!(
        up,
        "FROM"
            | "WHERE"
            | "USING"
            | "GROUP"
            | "ORDER"
            | "LIMIT"
            | "RETURNING"
            | "VALUES"
            | "SELECT"
    )
}

fn prev_statement(n: &SyntaxNode) -> Option<SyntaxNode> {
    let mut cur = n.prev_sibling();
    while let Some(p) = cur {
        if p.kind() == SyntaxKind::STATEMENT {
            return Some(p);
        }
        if p.kind().is_unit() {
            return None;
        }
        cur = p.prev_sibling();
    }
    None
}

fn constant_value(e: &Expr) -> Option<V> {
    Some(match e {
        Expr::Null => V::Null,
        Expr::Num(n) => match n.parse::<i128>() {
            Ok(i) => V::Int(i),
            Err(_) => V::Num(n.parse().ok()?),
        },
        Expr::Str(s) => V::Str(s.clone()),
        Expr::Special(s) if s == "TRUE" => V::Bool(true),
        Expr::Special(s) if s == "FALSE" => V::Bool(false),
        _ => return None,
    })
}

// ---- reaching the statement and reading its result ---------------------------------

/// A jump that leaves the statement sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Jump {
    /// Always leaves (RETURN, THROW, RAISE EXCEPTION, GOTO, BREAK, ...).
    Always,
    /// May leave (RAISERROR inside TRY, EXIT WHEN, ...).
    Maybe,
}

/// How the changed unit is reached: conditions that must hold for the
/// witness, and the lines of conditions or jumps on the way that are not
/// evaluated.
#[derive(Debug, Default)]
struct Reach {
    guards: Vec<(Expr, bool)>,
    untraced: Vec<usize>,
}

/// Whether a value is read after the changed unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Read {
    /// Read on every path that continues (line of the read).
    Always(usize),
    /// Read only on some paths (line of the first possible read).
    Maybe(usize),
    /// Overwritten or never mentioned again.
    Never,
}

/// Outcome of scanning a unit for a variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scan {
    Read(usize),
    Overwritten,
    /// Possibly read (inside a branch or loop body).
    Maybe(usize),
    Untouched,
}

impl<'x, 'a> Routine<'x, 'a> {
    fn line(&self, n: &SyntaxNode) -> usize {
        self.side.line_of(n)
    }

    /// Whether a statement jumps out of the sequence that holds `target`.
    fn jump(&self, s: &SyntaxNode, target: &SyntaxNode) -> Option<Jump> {
        let w = cst::first_words(s, 2);
        let first = w.first().map(String::as_str).unwrap_or("");
        let second = w.get(1).map(String::as_str).unwrap_or("");
        let in_try = || {
            target
                .ancestors()
                .any(|a| a.kind() == SyntaxKind::TRY_BLOCK)
        };
        // Loop jumps only matter when the loop they leave holds the target.
        let loop_holds_target = || {
            s.ancestors()
                .find(|a| a.kind() == SyntaxKind::LOOP_STMT)
                .is_some_and(|lp| target.ancestors().any(|a| a == lp))
        };
        let conditional = tokens(s).iter().any(|t| t.up == "WHEN");
        match first {
            "RETURN" if matches!(second, "NEXT" | "QUERY") => None,
            "RETURN" | "GOTO" | "SIGNAL" | "RESIGNAL" => Some(Jump::Always),
            "THROW" => Some(Jump::Always),
            "RAISE" if self.d == Dialect::Postgres => {
                (!matches!(second, "NOTICE" | "INFO" | "LOG" | "DEBUG" | "WARNING"))
                    .then_some(Jump::Always)
            }
            "RAISERROR" => in_try().then_some(Jump::Maybe),
            "BREAK" | "CONTINUE" | "EXIT" | "LEAVE" | "ITERATE" if loop_holds_target() => {
                Some(if conditional {
                    Jump::Maybe
                } else {
                    Jump::Always
                })
            }
            _ => None,
        }
    }

    /// Whether some statement inside `u` may jump out of the sequence that holds `target`.
    fn has_jump(&self, u: &SyntaxNode, target: &SyntaxNode) -> bool {
        u.descendants_with_tokens().any(|e| {
            e.as_node().is_some_and(|x| {
                x.kind() == SyntaxKind::STATEMENT && self.jump(x, target).is_some()
            }) || e.as_node().is_some_and(|x| x.kind() == SyntaxKind::LABEL)
        }) || (u.kind() == SyntaxKind::STATEMENT && self.jump(u, target).is_some())
    }

    /// Whether running `u` always ends in a jump out of the sequence that holds `target`.
    fn always_jumps(&self, u: &SyntaxNode, target: &SyntaxNode) -> bool {
        match u.kind() {
            SyntaxKind::STATEMENT => self.jump(u, target) == Some(Jump::Always),
            SyntaxKind::BLOCK | SyntaxKind::BRANCH => u
                .children()
                .filter(|c| c.kind().is_unit())
                .last()
                .is_some_and(|c| self.always_jumps(&c, target)),
            _ => false,
        }
    }

    /// Conditions under which `n` runs, from enclosing IF/WHILE conditions
    /// and earlier `IF ... RETURN`-style guards. Err when `n` is never reached.
    fn reach(&self, n: &SyntaxNode) -> Result<Reach, String> {
        let at = cst::sig_range(n).0;
        let mut r = Reach::default();
        let mut cur = n.clone();
        while let Some(p) = cur.parent() {
            if matches!(
                p.kind(),
                SyntaxKind::ROUTINE | SyntaxKind::BATCH | SyntaxKind::SOURCE_FILE
            ) {
                break;
            }
            // Handlers run after an error, not after the units before them.
            let handler = matches!(
                cur.kind(),
                SyntaxKind::CATCH_BLOCK
                    | SyntaxKind::EXCEPTION_SECTION
                    | SyntaxKind::EXCEPTION_HANDLER
                    | SyntaxKind::HANDLER
            );
            let earlier: Vec<SyntaxNode> = if handler {
                Vec::new()
            } else {
                cur.siblings(rowan::Direction::Prev)
                    .skip(1)
                    .filter(|s| s.kind().is_unit())
                    .collect()
            };
            for s in earlier {
                self.earlier(&s, n, at, &mut r)?;
            }
            match p.kind() {
                SyntaxKind::BRANCH => {
                    if let Some(g) = p.parent() {
                        let conds: Vec<SyntaxNode> = g
                            .children()
                            .filter(|c| c.kind() == SyntaxKind::CONDITION)
                            .collect();
                        let idx = g
                            .children()
                            .filter(|c| c.kind() == SyntaxKind::BRANCH)
                            .position(|b| b == p)
                            .unwrap_or(0);
                        match g.kind() {
                            SyntaxKind::IF_STMT => {
                                for (j, c) in conds.iter().enumerate() {
                                    if j > idx {
                                        break;
                                    }
                                    self.guard(c, j == idx, at, &mut r);
                                }
                            }
                            SyntaxKind::LOOP_STMT => {
                                let w = cst::first_words(&g, 1);
                                match w.first().map(String::as_str) {
                                    Some("WHILE") if conds.len() == 1 => {
                                        self.guard(&conds[0], true, at, &mut r)
                                    }
                                    Some("LOOP") => {}
                                    _ => r.untraced.push(self.line(&g)),
                                }
                            }
                            _ => r.untraced.push(self.line(&g)),
                        }
                    }
                }
                SyntaxKind::CATCH_BLOCK
                | SyntaxKind::EXCEPTION_SECTION
                | SyntaxKind::EXCEPTION_HANDLER
                | SyntaxKind::HANDLER
                | SyntaxKind::CASE_STMT => r.untraced.push(self.line(&p)),
                _ => {}
            }
            cur = p;
        }
        r.untraced.sort_unstable();
        r.untraced.dedup();
        Ok(r)
    }

    /// An earlier unit `s` in a sequence that leads to `n`.
    fn earlier(
        &self,
        s: &SyntaxNode,
        n: &SyntaxNode,
        at: usize,
        r: &mut Reach,
    ) -> Result<(), String> {
        if !self.has_jump(s, n) {
            return Ok(());
        }
        if s.kind() == SyntaxKind::STATEMENT
            && self.jump(s, n) == Some(Jump::Always)
            && !self.has_labels()
        {
            return Err(format!(
                "the statement is never reached (it follows the jump at line {})",
                self.line(s)
            ));
        }
        if s.kind() == SyntaxKind::IF_STMT {
            let conds: Vec<SyntaxNode> = s
                .children()
                .filter(|c| c.kind() == SyntaxKind::CONDITION)
                .collect();
            let branches: Vec<SyntaxNode> = s
                .children()
                .filter(|c| c.kind() == SyntaxKind::BRANCH)
                .collect();
            if conds.len() == 1 && !self.has_labels() {
                let then = &branches[0];
                let els = branches.get(1);
                let then_jumps = self.always_jumps(then, n);
                let else_jumps = els.is_some_and(|e| self.always_jumps(e, n));
                if then_jumps && else_jumps {
                    return Err(format!(
                        "the statement is never reached (both branches at line {} jump)",
                        self.line(s)
                    ));
                }
                if then_jumps && !els.is_some_and(|e| self.has_jump(e, n)) {
                    self.guard(&conds[0], false, at, r);
                    return Ok(());
                }
                if else_jumps && !self.has_jump(then, n) {
                    self.guard(&conds[0], true, at, r);
                    return Ok(());
                }
            }
        }
        r.untraced.push(self.line(s));
        Ok(())
    }

    fn has_labels(&self) -> bool {
        self.side
            .root
            .descendants()
            .any(|x| x.kind() == SyntaxKind::LABEL)
    }

    /// Adds condition `c` as a guard when its value at `c` is still its
    /// value at `at`: the variables it reads are not assigned in between,
    /// and the tables it reads are not written in between.
    fn guard(&self, c: &SyntaxNode, must: bool, at: usize, r: &mut Reach) {
        let line = self.line(c);
        let Ok(e) = cond_expr(c, self.d) else {
            r.untraced.push(line);
            return;
        };
        let (cs, ce) = cst::sig_range(c);
        // Loops around `at` that do not hold the condition repeat assignments.
        let loops: Vec<(usize, usize)> = self
            .loops
            .iter()
            .filter(|(s, e)| *s < at && at < *e && !(*s <= cs && ce <= *e))
            .copied()
            .collect();
        let between = |pos: usize| {
            (ce <= pos && pos < at) || loops.iter().any(|(s, e)| *s <= pos && pos < *e)
        };
        let mut ok = true;
        let mut reads_table = false;
        eval::walk_expr(&e, &mut |x| match x {
            Expr::Var(v) => {
                let k = eval::var_key(v);
                if k.starts_with('@')
                    || self
                        .sites
                        .get(&k)
                        .is_some_and(|ss| ss.iter().any(|s| !s.declare && between(s.start)))
                {
                    ok = false;
                }
            }
            Expr::Name(p) if p.len() == 1 && self.d != Dialect::Tsql => {
                let k = p[0].to_lowercase();
                if k == "found"
                    || self
                        .sites
                        .get(&k)
                        .is_some_and(|ss| ss.iter().any(|s| !s.declare && between(s.start)))
                {
                    ok = false;
                }
            }
            Expr::Exists(_) | Expr::Subquery(_) | Expr::InQuery(..) => reads_table = true,
            Expr::Call(_) => {}
            _ => {}
        });
        if reads_table {
            let writes = self.side.root.descendants().any(|s| {
                s.kind() == SyntaxKind::STATEMENT && between(cst::sig_range(&s).0) && {
                    let k = stmt_kind(&s, self.d);
                    matches!(
                        k,
                        StmtKind::Insert
                            | StmtKind::Update
                            | StmtKind::Delete
                            | StmtKind::Merge
                            | StmtKind::Call
                            | StmtKind::Ddl
                    ) || cst::first_words(&s, 1).first().is_some_and(|w| {
                        matches!(
                            w.as_str(),
                            "EXEC" | "EXECUTE" | "PERFORM" | "CALL" | "TRUNCATE"
                        )
                    })
                }
            });
            if writes {
                ok = false;
            }
        }
        if ok {
            r.guards.push((e, must));
        } else {
            r.untraced.push(line);
        }
    }

    /// Whether variable `name` is read after unit `n` runs.
    fn read_after(&self, name: &str, n: &SyntaxNode) -> Read {
        let mut maybe: Option<usize> = None;
        let mut cur = n.clone();
        loop {
            let later: Vec<SyntaxNode> = cur
                .siblings(rowan::Direction::Next)
                .skip(1)
                .filter(|s| s.kind().is_unit())
                .collect();
            for s in later {
                match self.scan_var(name, &s) {
                    Scan::Read(l) => return maybe.map_or(Read::Always(l), Read::Maybe),
                    Scan::Overwritten => return maybe.map_or(Read::Never, Read::Maybe),
                    Scan::Maybe(l) => {
                        maybe.get_or_insert(l);
                    }
                    Scan::Untouched => {}
                }
            }
            let Some(p) = cur.parent() else { break };
            if matches!(
                p.kind(),
                SyntaxKind::ROUTINE | SyntaxKind::BATCH | SyntaxKind::SOURCE_FILE
            ) {
                break;
            }
            // The end of a loop body goes back to the condition and the body.
            if p.kind() == SyntaxKind::BRANCH {
                if let Some(g) = p.parent().filter(|g| g.kind() == SyntaxKind::LOOP_STMT) {
                    for c in g.children().filter(|c| c.kind() == SyntaxKind::CONDITION) {
                        if self.mentions(name, &c) {
                            return maybe.map_or(Read::Always(self.line(&c)), Read::Maybe);
                        }
                    }
                    if self.mentions(name, &p) {
                        maybe.get_or_insert(self.line(&p));
                    }
                }
            }
            cur = p;
        }
        if self.is_out(name) {
            return maybe.map_or(Read::Always(0), Read::Maybe);
        }
        maybe.map_or(Read::Never, Read::Maybe)
    }

    /// Whether a parameter's incoming value is read by the routine body.
    fn read_in_body(&self, name: &str) -> Read {
        let Some(body) = self
            .side
            .root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::ROUTINE_BODY)
        else {
            return Read::Never;
        };
        match self.scan_var(name, &body) {
            Scan::Read(l) => Read::Always(l),
            Scan::Maybe(l) => Read::Maybe(l),
            Scan::Overwritten => Read::Never,
            Scan::Untouched if self.is_out(name) => Read::Always(0),
            Scan::Untouched => Read::Never,
        }
    }

    /// Lines in the routine body that read variable `name` for its value:
    /// any mention but an assignment target and an `IS [NOT] NULL` test.
    fn value_reads(&self, name: &str) -> Vec<usize> {
        let Some(body) = self
            .side
            .root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::ROUTINE_BODY)
        else {
            return Vec::new();
        };
        let toks = tokens(&body);
        let mut lines = Vec::new();
        for (i, t) in toks.iter().enumerate() {
            let hit = match t.kind {
                K::Var => t.text.trim_start_matches('@').eq_ignore_ascii_case(name),
                _ => false,
            };
            if !hit {
                continue;
            }
            let next = |k: usize| toks.get(i + k).map(|t| t.up.as_str()).unwrap_or("");
            let prev = i.checked_sub(1).map(|j| toks[j].up.as_str()).unwrap_or("");
            let null_test =
                next(1) == "IS" && (next(2) == "NULL" || next(2) == "NOT" && next(3) == "NULL");
            let target = matches!(prev, "SET" | "SELECT" | ",")
                && toks
                    .get(i + 1)
                    .is_some_and(|n| n.kind == K::Op && n.text == "=");
            if null_test || target {
                continue;
            }
            let line = self.side.lines.line_col(t.off).0;
            if !lines.contains(&line) {
                lines.push(line);
            }
        }
        lines
    }

    fn mentions(&self, name: &str, n: &SyntaxNode) -> bool {
        tokens(n).iter().any(|t| match t.kind {
            K::Var => t.text.trim_start_matches('@').eq_ignore_ascii_case(name),
            K::Word | K::Quoted => self.d != Dialect::Tsql && t.text.eq_ignore_ascii_case(name),
            _ => false,
        })
    }

    /// What unit `u` does with variable `name`.
    fn scan_var(&self, name: &str, u: &SyntaxNode) -> Scan {
        match u.kind() {
            SyntaxKind::STATEMENT => {
                if !self.mentions(name, u) {
                    return Scan::Untouched;
                }
                let t = tokens(u);
                let is = |x: &Tok| match x.kind {
                    K::Var => x.text.trim_start_matches('@').eq_ignore_ascii_case(name),
                    K::Word | K::Quoted => {
                        self.d != Dialect::Tsql && x.text.eq_ignore_ascii_case(name)
                    }
                    _ => false,
                };
                let count = t.iter().filter(|x| is(x)).count();
                // SET @v = e / v := e without reading v: a plain overwrite.
                let plain_target = if t.first().is_some_and(|x| x.up == "SET") && t.len() > 2 {
                    is(&t[1]) && t[2].text == "="
                } else {
                    self.d != Dialect::Tsql
                        && t.len() > 2
                        && is(&t[0])
                        && (t[1].text == ":=" || t[1].text == "=")
                };
                if plain_target && count == 1 {
                    return Scan::Overwritten;
                }
                // T-SQL SELECT @v = e without FROM also always assigns.
                if self.d == Dialect::Tsql
                    && t.first().is_some_and(|x| x.up == "SELECT")
                    && !t.iter().any(|x| x.up == "FROM")
                    && count == 1
                    && t.iter()
                        .position(&is)
                        .is_some_and(|i| t.get(i + 1).is_some_and(|y| y.text == "="))
                {
                    return Scan::Overwritten;
                }
                // SELECT @v = col FROM ... assigns only when a row qualifies:
                // later reads may see either value.
                if self.d == Dialect::Tsql
                    && t.first().is_some_and(|x| x.up == "SELECT")
                    && count == 1
                    && t.iter().position(&is).is_some_and(|i| {
                        t.get(i + 1).is_some_and(|y| y.text == "=")
                            && (i == 1 || t[i - 1].kind == K::Comma)
                    })
                {
                    return Scan::Maybe(self.line(u));
                }
                // INTO targets: PL/pgSQL SELECT INTO always assigns, FETCH INTO may not.
                if count == 1 {
                    if let Some(i) = t.iter().position(&is) {
                        let mut j = i;
                        while j >= 2 && t[j - 1].kind == K::Comma {
                            j -= 2;
                        }
                        if j >= 1 && t[j - 1].up == "INTO" && t[0].up != "INSERT" {
                            return if t[0].up == "SELECT" && self.d != Dialect::Tsql {
                                Scan::Overwritten
                            } else {
                                Scan::Maybe(self.line(u))
                            };
                        }
                    }
                }
                Scan::Read(self.line(u))
            }
            SyntaxKind::BLOCK
            | SyntaxKind::TRY_CATCH
            | SyntaxKind::TRY_BLOCK
            | SyntaxKind::BRANCH
            | SyntaxKind::ROUTINE_BODY => {
                // Runs in order; a CATCH block may or may not run.
                for c in u.children() {
                    if c.kind() == SyntaxKind::CATCH_BLOCK
                        || c.kind() == SyntaxKind::EXCEPTION_SECTION
                    {
                        if self.mentions(name, &c) {
                            return Scan::Maybe(self.line(&c));
                        }
                        continue;
                    }
                    if !(c.kind().is_unit() || c.kind() == SyntaxKind::TRY_BLOCK) {
                        continue;
                    }
                    match self.scan_var(name, &c) {
                        Scan::Untouched => {}
                        other => return other,
                    }
                }
                Scan::Untouched
            }
            SyntaxKind::IF_STMT | SyntaxKind::LOOP_STMT => {
                // The first condition always runs; the rest may.
                if let Some(c) = u.children().find(|c| c.kind() == SyntaxKind::CONDITION) {
                    if self.mentions(name, &c) {
                        return Scan::Read(self.line(&c));
                    }
                }
                if self.mentions(name, u) {
                    // A branch that overwrites before reading still counts as a possible read.
                    let mut first = None;
                    for b in u
                        .children()
                        .filter(|c| matches!(c.kind(), SyntaxKind::BRANCH | SyntaxKind::CONDITION))
                    {
                        let s = if b.kind() == SyntaxKind::CONDITION {
                            if self.mentions(name, &b) {
                                Scan::Read(self.line(&b))
                            } else {
                                Scan::Untouched
                            }
                        } else {
                            self.scan_var(name, &b)
                        };
                        if let Scan::Read(l) | Scan::Maybe(l) = s {
                            first.get_or_insert(l);
                        }
                    }
                    return first.map_or(Scan::Untouched, Scan::Maybe);
                }
                Scan::Untouched
            }
            _ => {
                if self.mentions(name, u) {
                    Scan::Maybe(self.line(u))
                } else {
                    Scan::Untouched
                }
            }
        }
    }
}

// ---- other changes around the changed statement ------------------------------------

/// A byte range of one side that another finding marks as changed.
type Span = (usize, usize);

/// Something every statement sets: a later read of it sees the statement before.
const STATUS: &[&str] = &[
    "rowcount",
    "error",
    "fetch_status",
    "identity",
    "scope_identity",
    "trancount",
    "found",
    "row_count",
    "sqlstate",
    "sqlcode",
    "sqlerrm",
];

/// Stands for every name: the unit may write anything (a call, DDL, dynamic SQL).
const ANY: &str = "*";

/// A statement or a condition, with the names it reads and writes.
struct Unit {
    start: usize,
    end: usize,
    reads: HashSet<String>,
    writes: HashSet<String>,
    /// It may leave the sequence it is in (RETURN, BREAK, THROW, ...).
    jumps: bool,
}

fn unit_name(t: &Tok) -> Option<String> {
    matches!(t.kind, K::Var | K::Word | K::Quoted).then(|| {
        t.text
            .trim_start_matches('@')
            .trim_matches(|c| matches!(c, '[' | ']' | '"' | '`'))
            .to_lowercase()
    })
}

fn overlaps(a: Span, b: Span) -> bool {
    a.0 < b.1 && b.0 < a.1
}

fn meets(a: &HashSet<String>, b: &HashSet<String>) -> bool {
    !a.is_empty() && !b.is_empty() && (a.contains(ANY) || b.contains(ANY) || !a.is_disjoint(b))
}

impl<'x, 'a> Routine<'x, 'a> {
    /// Names unit `n` (a statement) writes: assigned variables, written
    /// tables and everything a statement sets as a side effect.
    fn statement_writes(&self, n: &SyntaxNode) -> HashSet<String> {
        let (s, e) = cst::sig_range(n);
        let mut w: HashSet<String> = STATUS.iter().map(|x| x.to_string()).collect();
        for (name, sites) in &self.sites {
            if sites.iter().any(|x| s <= x.start && x.start < e) {
                w.insert(name.trim_start_matches('@').to_string());
            }
        }
        let k = stmt_kind(n, self.d);
        let first = cst::first_words(n, 1);
        let first = first.first().map(String::as_str).unwrap_or("");
        let calls = matches!(k, StmtKind::Call | StmtKind::Ddl)
            || matches!(
                first,
                "EXEC" | "EXECUTE" | "PERFORM" | "CALL" | "CREATE" | "ALTER" | "DROP"
            );
        if calls {
            w.insert(ANY.to_string());
        }
        let dml = matches!(
            k,
            StmtKind::Insert | StmtKind::Update | StmtKind::Delete | StmtKind::Merge
        ) || matches!(first, "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "TRUNCATE");
        if dml {
            // Every name that is not a keyword or a variable: the target,
            // its alias and the tables it is joined with.
            let t = tokens(n);
            for (i, x) in t.iter().enumerate() {
                // Schema qualifiers are no table.
                let qualifier = t.get(i + 1).is_some_and(|y| y.kind == K::Dot);
                if qualifier || (x.kind != K::Var && normalize::is_keyword(&x.up)) {
                    continue;
                }
                // Scalar variables are read here, not written; a T-SQL table
                // variable (`@t`, never a scalar local) is a written table.
                let scalar_name = x.kind == K::Var || self.d != Dialect::Tsql;
                if let Some(x) = unit_name(x) {
                    let scalar = self.params.contains_key(&x) || self.locals.contains_key(&x);
                    if !(scalar_name && scalar) {
                        w.insert(x);
                    }
                }
            }
        }
        w
    }

    /// Every statement and condition of the routine.
    fn units(&self) -> &[Unit] {
        self.units.get_or_init(|| self.build_units())
    }

    fn build_units(&self) -> Vec<Unit> {
        let mut out = Vec::new();
        for n in self.side.root.descendants() {
            match n.kind() {
                SyntaxKind::STATEMENT => {
                    let (start, end) = cst::sig_range(&n);
                    let w = cst::first_words(&n, 2);
                    let first = w.first().map(String::as_str).unwrap_or("");
                    let jumps = match first {
                        "RETURN" => !matches!(w.get(1).map(String::as_str), Some("NEXT" | "QUERY")),
                        "GOTO" | "THROW" | "RAISE" | "RAISERROR" | "SIGNAL" | "RESIGNAL"
                        | "BREAK" | "CONTINUE" | "EXIT" | "LEAVE" | "ITERATE" => true,
                        _ => false,
                    };
                    out.push(Unit {
                        start,
                        end,
                        reads: tokens(&n).iter().filter_map(unit_name).collect(),
                        writes: self.statement_writes(&n),
                        jumps,
                    });
                }
                SyntaxKind::CONDITION => {
                    let (start, end) = cst::sig_range(&n);
                    // What the condition decides: the writes of its compound statement.
                    let mut writes = HashSet::new();
                    if let Some(p) = n.parent() {
                        for s in p
                            .descendants()
                            .filter(|x| x.kind() == SyntaxKind::STATEMENT)
                        {
                            writes.extend(self.statement_writes(&s));
                        }
                        let (ps, pe) = cst::sig_range(&p);
                        for (name, sites) in &self.sites {
                            if sites.iter().any(|x| ps <= x.start && x.start < pe) {
                                writes.insert(name.trim_start_matches('@').to_string());
                            }
                        }
                    }
                    out.push(Unit {
                        start,
                        end,
                        reads: tokens(&n).iter().filter_map(unit_name).collect(),
                        writes,
                        jumps: false,
                    });
                }
                _ => {}
            }
        }
        out
    }

    /// Conditions that decide whether `n` runs: those of the IF and WHILE
    /// statements around it, and those of earlier IF statements that may jump.
    fn deciding_conditions(&self, n: &SyntaxNode) -> Vec<Span> {
        let mut out = Vec::new();
        let mut cur = n.clone();
        while let Some(p) = cur.parent() {
            for s in cur
                .siblings(rowan::Direction::Prev)
                .skip(1)
                .filter(|s| s.kind().is_unit() && self.has_jump(s, n))
            {
                for c in s
                    .descendants()
                    .filter(|c| c.kind() == SyntaxKind::CONDITION)
                {
                    out.push(cst::sig_range(&c));
                }
            }
            for c in p.children().filter(|c| c.kind() == SyntaxKind::CONDITION) {
                out.push(cst::sig_range(&c));
            }
            cur = p;
        }
        out
    }

    /// Why a witness for `n` alone cannot be trusted when the code at
    /// `changed` differs between the versions too: a value `n` reads, or a
    /// condition on the way to it, may come from changed code, or what `n`
    /// writes reaches changed code. The witness gives both versions the same
    /// inputs at `n` and runs the same code after it, so two edits that
    /// make up for each other would look like two differences.
    fn interacts(&self, n: &SyntaxNode, changed: &[Span], reach: &Reach) -> Option<String> {
        if changed.is_empty() {
            return None;
        }
        let here = cst::sig_range(n);
        let units = self.units();
        let changed_unit = |u: &Unit| changed.iter().any(|c| overlaps(*c, (u.start, u.end)));
        let in_loop_with_n = |u: &Unit| {
            self.loops
                .iter()
                .any(|(ls, le)| *ls <= u.start && u.end <= *le && *ls <= here.0 && here.1 <= *le)
        };
        let others: Vec<&Unit> = units
            .iter()
            .filter(|u| !overlaps((u.start, u.end), here))
            .collect();
        let before = |u: &Unit| u.end <= here.0 || in_loop_with_n(u);
        let line = |off: usize| self.side.lines.line_col(off).0;

        // Values at `n` that changed code may have set.
        let mut tainted: HashSet<String> = HashSet::new();
        let mut first_change: Option<usize> = None;
        for u in others.iter().filter(|u| before(u) && changed_unit(u)) {
            tainted.extend(u.writes.iter().cloned());
            first_change = Some(first_change.map_or(u.start, |f| f.min(u.start)));
            // A changed jump on the way that is not evaluated as a guard.
            if u.jumps && !reach.untraced.is_empty() {
                return Some(format!(
                    "the changed jump at line {} may decide whether it runs",
                    line(u.start)
                ));
            }
        }
        if let Some(first) = first_change {
            loop {
                let mut grew = false;
                for u in others.iter().filter(|u| before(u) && !changed_unit(u)) {
                    if meets(&u.reads, &tainted) && !u.writes.is_subset(&tainted) {
                        tainted.extend(u.writes.iter().cloned());
                        grew = true;
                    }
                }
                if !grew {
                    break;
                }
            }
            let deciding = self.deciding_conditions(n);
            let needed: HashSet<String> = units
                .iter()
                .filter(|u| {
                    overlaps((u.start, u.end), here)
                        || deciding.iter().any(|c| overlaps(*c, (u.start, u.end)))
                })
                .flat_map(|u| u.reads.iter().cloned())
                .collect();
            if meets(&needed, &tainted) {
                let mut names: Vec<&String> = needed
                    .intersection(&tainted)
                    .filter(|x| x.as_str() != ANY)
                    .collect();
                names.sort();
                return Some(match names.first() {
                    Some(x) => format!(
                        "{} may hold a value set by the changed code at line {}",
                        spell(x),
                        line(first)
                    ),
                    None => format!(
                        "it reads what the changed code at line {} may write",
                        line(first)
                    ),
                });
            }
        }

        // What `n` writes that changed code later reads or overwrites.
        let seed: HashSet<String> = units
            .iter()
            .filter(|u| overlaps((u.start, u.end), here))
            .flat_map(|u| u.writes.iter().cloned())
            .collect();
        self.reaches_change(seed, here, units, changed)
            .map(|l| format!("its result reaches the changed code at line {l}"))
    }

    /// The line of the first changed unit after `here` (or in a loop with
    /// it) that reads or overwrites one of `seed`, directly or through
    /// unchanged units in between.
    fn reaches_change(
        &self,
        seed: HashSet<String>,
        here: Span,
        units: &[Unit],
        changed: &[Span],
    ) -> Option<usize> {
        let changed_unit = |u: &Unit| changed.iter().any(|c| overlaps(*c, (u.start, u.end)));
        let after = |u: &Unit| {
            !overlaps((u.start, u.end), here)
                && (u.start >= here.1
                    || self.loops.iter().any(|(ls, le)| {
                        *ls <= u.start && u.end <= *le && *ls <= here.0 && here.1 <= *le
                    }))
        };
        let mut reached: HashSet<String> = seed
            .into_iter()
            .filter(|x| !STATUS.contains(&x.as_str()))
            .collect();
        if reached.is_empty() {
            return None;
        }
        loop {
            let mut grew = false;
            for u in units.iter().filter(|u| after(u) && !changed_unit(u)) {
                if meets(&u.reads, &reached) {
                    for w in &u.writes {
                        if !STATUS.contains(&w.as_str()) && reached.insert(w.clone()) {
                            grew = true;
                        }
                    }
                }
            }
            if !grew {
                break;
            }
        }
        units
            .iter()
            .filter(|u| after(u) && changed_unit(u))
            .find(|u| {
                let touched: HashSet<String> = u.reads.union(&u.writes).cloned().collect();
                u.jumps || meets(&touched, &reached)
            })
            .map(|u| self.side.lines.line_col(u.start).0)
    }
}

// ---- code that cannot be created --------------------------------------------------

const CMP_OPS: &[&str] = &["<>", "!=", "<", ">", "<=", ">=", "!<", "!>"];

fn is_cmp(t: &Tok) -> bool {
    t.kind == K::Op && CMP_OPS.contains(&t.text.as_str())
}

/// Why the tokens of one statement (or routine header) are certainly
/// rejected when the routine is created: a comparison operator where the
/// grammar only allows `=` or `:=`, or a type size beyond its limit.
fn invalid(t: &[Tok], d: Dialect, header: bool) -> Option<String> {
    let first = t.first().map(|x| x.up.as_str()).unwrap_or("");
    let at = |i: usize| t.get(i);
    let cmp_at = |i: usize| at(i).is_some_and(is_cmp);
    let bad = |i: usize, what: &str| Some(format!("`{}` where {what} needs `=`", t[i].text));
    // An assignment target followed by a comparison.
    if first == "SET"
        && matches!(at(1).map(|x| x.kind), Some(K::Var | K::Word | K::Quoted))
        && cmp_at(2)
    {
        return bad(2, "SET");
    }
    if d != Dialect::Tsql && !header {
        // PL/pgSQL `v <> e;` / `v :<> e`.
        if matches!(at(0).map(|x| x.kind), Some(K::Word | K::Quoted))
            && !is_keyword_start(first)
            && cmp_at(1)
        {
            return bad(1, "an assignment");
        }
    }
    for i in 0..t.len().saturating_sub(1) {
        if t[i].kind == K::Other && t[i].text == ":" && is_cmp(&t[i + 1]) {
            return bad(i + 1, "`:=`");
        }
        // `SET name <> ...` (session settings, function SET clauses, UPDATE SET).
        if t[i].up == "SET"
            && t[i].kind == K::Word
            && matches!(t[i + 1].kind, K::Word | K::Quoted | K::Var)
            && cmp_at(i + 2)
        {
            return bad(i + 2, "SET");
        }
    }
    // Option lists hold `name = value` pairs, never comparisons:
    // `WITH (FILLFACTOR = 80)`, `OPTION (LABEL = '...')`, `SET (SYSTEM_VERSIONING
    // = ON (HISTORY_TABLE = ...))`, `MASKED WITH (FUNCTION = 'default()')`.
    let mut options: Vec<bool> = Vec::new();
    for i in 0..t.len() {
        match t[i].kind {
            K::LParen => {
                let prev = i.checked_sub(1).map(|j| &t[j]);
                let opens = prev.is_some_and(|p| {
                    p.kind == K::Word
                        && (matches!(p.up.as_str(), "WITH" | "OPTION" | "SET")
                            || p.up == "ON" && options.last() == Some(&true))
                });
                options.push(opens);
            }
            K::RParen => {
                options.pop();
            }
            _ if options.last() == Some(&true)
                && is_cmp(&t[i])
                && i >= 2
                && matches!(t[i - 1].kind, K::Word)
                && matches!(t[i - 2].kind, K::LParen | K::Comma) =>
            {
                return bad(i, "an option setting");
            }
            _ => {}
        }
    }
    let mut depth = 0i32;
    let mut i = 0;
    while i < t.len() {
        match t[i].kind {
            K::LParen => depth += 1,
            K::RParen => depth -= 1,
            _ => {}
        }
        // DECLARE @v type <> e / header `@p type <> default` / PL/pgSQL `v type <> e`.
        let decl_name = t[i].kind == K::Var
            && d == Dialect::Tsql
            && (header
                && depth <= 1
                && (i == 0
                    || matches!(t[i - 1].kind, K::Comma | K::LParen)
                    || t[i - 1].kind == K::Word
                        && matches!(t[i - 1].up.as_str(), "PROCEDURE" | "PROC" | "FUNCTION"))
                || first == "DECLARE" && depth == 0 && (i == 1 || t[i - 1].kind == K::Comma));
        if decl_name {
            let mut j = i + 1;
            if at(j).is_some_and(|x| x.up == "AS") {
                j += 1;
            }
            let mut p = Parser::ext(&t[j.min(t.len())..], d);
            if p.ty().is_ok() {
                let k = j + p.i;
                if cmp_at(k) {
                    return bad(k, "a default value");
                }
            }
        }
        i += 1;
    }
    if d == Dialect::Tsql && !header {
        // UPDATE ... SET col <> e, ...  and  EXEC proc @p <> v.
        if first == "UPDATE" {
            if let Some(s) = t.iter().position(|x| x.up == "SET") {
                let mut depth = 0;
                let mut item_start = s + 1;
                for k in s + 1..t.len() {
                    match t[k].kind {
                        K::LParen => depth += 1,
                        K::RParen => depth -= 1,
                        K::Comma if depth == 0 => item_start = k + 1,
                        K::Word
                            if depth == 0
                                && matches!(
                                    t[k].up.as_str(),
                                    "FROM" | "WHERE" | "OUTPUT" | "OPTION"
                                ) =>
                        {
                            break
                        }
                        _ => {}
                    }
                    if depth == 0 && is_cmp(&t[k]) {
                        // The target is a (qualified) column or a variable right before it.
                        let target = &t[item_start..k];
                        let plain = !target.is_empty()
                            && target
                                .iter()
                                .all(|x| matches!(x.kind, K::Word | K::Quoted | K::Var | K::Dot));
                        if plain {
                            return bad(k, "UPDATE ... SET");
                        }
                    }
                }
            }
        }
        if matches!(first, "EXEC" | "EXECUTE") {
            // Arguments are constants, variables or `@p = v`: never comparisons.
            let mut depth = 0;
            for (k, x) in t.iter().enumerate() {
                match x.kind {
                    K::LParen => depth += 1,
                    K::RParen => depth -= 1,
                    _ => {}
                }
                if depth == 0 && is_cmp(x) {
                    return bad(k, "a procedure argument");
                }
            }
        }
        // A comparison directly in a SELECT list (outside CASE and parentheses).
        if let Some(r) = select_list_comparison(t) {
            return Some(r);
        }
    }
    type_limits(t, d)
}

/// Statements that start with a keyword rather than an assignment target.
fn is_keyword_start(up: &str) -> bool {
    matches!(
        up,
        "SELECT"
            | "INSERT"
            | "UPDATE"
            | "DELETE"
            | "MERGE"
            | "WITH"
            | "RETURN"
            | "IF"
            | "WHILE"
            | "FOR"
            | "LOOP"
            | "PERFORM"
            | "EXECUTE"
            | "CALL"
            | "RAISE"
            | "EXIT"
            | "CONTINUE"
            | "OPEN"
            | "FETCH"
            | "CLOSE"
            | "MOVE"
            | "GET"
            | "BEGIN"
            | "END"
            | "ELSE"
            | "ELSIF"
            | "WHEN"
            | "CASE"
            | "DECLARE"
            | "NULL"
            | "COMMIT"
            | "ROLLBACK"
            | "ASSERT"
            | "SET"
            | "RESET"
            | "DROP"
            | "CREATE"
            | "ALTER"
            | "TRUNCATE"
            | "LOCK"
            | "GRANT"
            | "REVOKE"
            | "COPY"
            | "VALUES"
            | "TABLE"
            | "COMMENT"
            | "ANALYZE"
            | "VACUUM"
            | "REFRESH"
            | "NOTIFY"
            | "LISTEN"
            | "SAVEPOINT"
            | "RELEASE"
            | "DISCARD"
            | "IMPORT"
            | "SECURITY"
            | "REINDEX"
            | "CLUSTER"
    )
}

/// T-SQL: a comparison operator at the top level of a select list item.
fn select_list_comparison(t: &[Tok]) -> Option<String> {
    for (s, x) in t.iter().enumerate() {
        if x.up != "SELECT" {
            continue;
        }
        let mut depth = 0i32;
        let mut case = 0i32;
        for tk in t.iter().skip(s + 1) {
            match tk.kind {
                K::LParen => depth += 1,
                K::RParen => {
                    depth -= 1;
                    if depth < 0 {
                        break;
                    }
                }
                K::Semi => break,
                K::Word if depth == 0 => match tk.up.as_str() {
                    "CASE" => case += 1,
                    "END" => case -= 1,
                    "FROM" | "INTO" | "WHERE" | "GROUP" | "ORDER" | "UNION" | "EXCEPT"
                    | "INTERSECT" | "OPTION" | "FOR" => break,
                    _ => {}
                },
                _ => {}
            }
            if depth == 0 && case == 0 && is_cmp(tk) {
                return Some(format!(
                    "`{}` directly in a select list (T-SQL has no boolean values)",
                    tk.text
                ));
            }
        }
    }
    None
}

/// Type sizes beyond what the engine accepts, in type positions.
fn type_limits(t: &[Tok], d: Dialect) -> Option<String> {
    for i in 0..t.len() {
        let x = &t[i];
        if x.kind != K::Word || !t.get(i + 1).is_some_and(|y| y.kind == K::LParen) {
            continue;
        }
        let prev = i.checked_sub(1).map(|p| &t[p]);
        let type_pos = prev.is_some_and(|p| {
            p.kind == K::Var
                || p.up == "AS"
                || (p.kind == K::LParen
                    && i >= 2
                    && matches!(t[i - 2].up.as_str(), "CONVERT" | "TRY_CONVERT"))
        });
        if !type_pos {
            continue;
        }
        let num = |k: usize| {
            t.get(k)
                .filter(|y| y.kind == K::Num)
                .and_then(|y| y.text.parse::<i64>().ok())
        };
        let Some(a) = num(i + 2) else { continue };
        let b = if t.get(i + 3).is_some_and(|y| y.kind == K::Comma) {
            num(i + 4)
        } else {
            None
        };
        let ty = x.up.as_str();
        let limit = match (d, ty) {
            (Dialect::Tsql, "DATETIME2" | "TIME" | "DATETIMEOFFSET") => Some(7),
            (Dialect::Tsql, "DECIMAL" | "NUMERIC") => Some(38),
            (Dialect::Tsql, "VARCHAR" | "CHAR" | "VARBINARY" | "BINARY") => Some(8000),
            (Dialect::Tsql, "NVARCHAR" | "NCHAR") => Some(4000),
            (Dialect::Tsql, "FLOAT") => Some(53),
            (Dialect::Postgres, "TIMESTAMP" | "TIMESTAMPTZ" | "TIME" | "TIMETZ" | "INTERVAL") => {
                Some(6)
            }
            (Dialect::Postgres, "NUMERIC" | "DECIMAL") => Some(1000),
            _ => None,
        };
        let Some(limit) = limit else { continue };
        if a > limit
            || a < 1
                && !matches!(
                    ty,
                    "DATETIME2"
                        | "TIME"
                        | "DATETIMEOFFSET"
                        | "TIMESTAMP"
                        | "TIMESTAMPTZ"
                        | "TIMETZ"
                        | "INTERVAL"
                )
        {
            return Some(format!(
                "{}({a}) is beyond the type's limit of {limit}",
                x.text
            ));
        }
        if let Some(b) = b {
            if matches!(ty, "DECIMAL" | "NUMERIC") && d == Dialect::Tsql && b > a {
                return Some(format!(
                    "{}({a},{b}) has a scale larger than its precision",
                    x.text
                ));
            }
        }
    }
    None
}

// ---- probes ----------------------------------------------------------------------

/// What a changed unit runs, and how its result is observed.
struct Shape {
    probe: Probe,
    /// For conditions: the branch bodies (None: no branch taken).
    branches: Vec<SyntaxNode>,
    /// Statement following a CONTINUE/EXIT WHEN (the rest of the loop body).
    rest: Vec<SyntaxNode>,
    kind: ShapeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShapeKind {
    Data,
    If,
    While,
    ExitWhen,
    ContinueWhen,
    Raise,
    Message,
    /// The query of a cursor declaration (name of the cursor).
    Cursor,
    /// The query of a `FOR r IN query LOOP`.
    ForQuery,
    /// The query of a view definition: its rows are what the view returns.
    View,
}

fn cond_expr(c: &SyntaxNode, d: Dialect) -> Result<Expr, String> {
    let t = tokens(c);
    let mut p = Parser::ext(&t, d);
    let e = p.expr()?;
    if !p.at_end() {
        return Err(p.err("unexpected tokens after the condition"));
    }
    Ok(e)
}

fn shape(n: &SyntaxNode, d: Dialect) -> Result<Shape, String> {
    match n.kind() {
        SyntaxKind::IF_STMT => {
            let mut conds = Vec::new();
            let mut branches = Vec::new();
            for c in n.children() {
                match c.kind() {
                    SyntaxKind::CONDITION => conds.push(cond_expr(&c, d)?),
                    SyntaxKind::BRANCH => branches.push(c),
                    _ => {}
                }
            }
            Ok(Shape {
                probe: Probe::Conds(conds),
                branches,
                rest: Vec::new(),
                kind: ShapeKind::If,
            })
        }
        SyntaxKind::LOOP_STMT => {
            let words = cst::first_words(n, 1);
            if words.first().map(String::as_str) == Some("FOR") {
                // PL/pgSQL `FOR r IN query LOOP`: the rows the loop runs over.
                let c = n
                    .children()
                    .find(|c| c.kind() == SyntaxKind::CONDITION)
                    .ok_or("loop without a condition")?;
                let t = tokens(&c);
                if t.len() < 3 || t[1].up != "IN" || !matches!(t[2].up.as_str(), "SELECT" | "WITH")
                {
                    return Err("FOR loop over something other than a query".into());
                }
                let s = parse_stmt(&t[2..], d)?;
                return Ok(Shape {
                    probe: Probe::Stmt(s),
                    branches: n
                        .children()
                        .filter(|c| c.kind() == SyntaxKind::BRANCH)
                        .collect(),
                    rest: Vec::new(),
                    kind: ShapeKind::ForQuery,
                });
            }
            if words.first().map(String::as_str) != Some("WHILE") {
                return Err("only WHILE and FOR loops are compared".into());
            }
            let c = n
                .children()
                .find(|c| c.kind() == SyntaxKind::CONDITION)
                .ok_or("loop without a condition")?;
            Ok(Shape {
                probe: Probe::Conds(vec![cond_expr(&c, d)?]),
                branches: n
                    .children()
                    .filter(|c| c.kind() == SyntaxKind::BRANCH)
                    .collect(),
                rest: Vec::new(),
                kind: ShapeKind::While,
            })
        }
        SyntaxKind::STATEMENT => statement_shape(n, d),
        _ => Err("not a statement or condition".into()),
    }
}

fn statement_shape(n: &SyntaxNode, d: Dialect) -> Result<Shape, String> {
    let mut t = tokens(n);
    let first = t.first().map(|x| x.up.clone()).unwrap_or_default();
    let data = |p: Probe| Shape {
        probe: p,
        branches: Vec::new(),
        rest: Vec::new(),
        kind: ShapeKind::Data,
    };
    match first.as_str() {
        "THROW" | "RAISERROR" | "RAISE" | "SIGNAL" | "PRINT" => {
            let kind = match first.as_str() {
                "PRINT" => ShapeKind::Message,
                "RAISE"
                    if t.get(1).is_some_and(|x| {
                        matches!(
                            x.up.as_str(),
                            "NOTICE" | "INFO" | "LOG" | "DEBUG" | "WARNING"
                        )
                    }) =>
                {
                    ShapeKind::Message
                }
                _ => ShapeKind::Raise,
            };
            let mut start = 1;
            if first == "RAISE" {
                while t
                    .get(start)
                    .is_some_and(|x| x.kind == K::Word && x.up != "USING")
                {
                    start += 1;
                }
            }
            let inner: &[Tok] = if first == "RAISERROR"
                && t.get(1).is_some_and(|x| x.kind == K::LParen)
                && t.last().is_some_and(|x| x.kind == K::RParen)
            {
                &t[2..t.len() - 1]
            } else {
                &t[start.min(t.len())..]
            };
            if inner.iter().any(|x| x.up == "USING" || x.up == "WITH") {
                return Err("RAISE USING / WITH options".into());
            }
            let mut p = Parser::ext(inner, d);
            let es = if inner.is_empty() {
                Vec::new()
            } else {
                p.expr_list()?
            };
            if !p.at_end() {
                return Err(p.err("unexpected tokens"));
            }
            Ok(Shape {
                probe: Probe::Values(es),
                branches: Vec::new(),
                rest: Vec::new(),
                kind,
            })
        }
        "CONTINUE" | "EXIT" if t.iter().any(|x| x.up == "WHEN") => {
            let w = t.iter().position(|x| x.up == "WHEN").unwrap();
            let mut p = Parser::ext(&t[w + 1..], d);
            let e = p.expr()?;
            if !p.at_end() {
                return Err(p.err("unexpected tokens"));
            }
            let rest: Vec<SyntaxNode> = n
                .siblings(rowan::Direction::Next)
                .skip(1)
                .filter(|s| s.kind().is_unit())
                .collect();
            Ok(Shape {
                probe: Probe::Conds(vec![e]),
                branches: Vec::new(),
                rest,
                kind: if first == "EXIT" {
                    ShapeKind::ExitWhen
                } else {
                    ShapeKind::ContinueWhen
                },
            })
        }
        "CREATE" | "ALTER" if view_query(&t).is_some() => {
            let q = view_query(&t).unwrap();
            let s = parse_stmt(&t[q.0..q.1], d)?;
            if !matches!(s, Stmt::Select(_)) {
                return Err("view over something other than a query".into());
            }
            Ok(Shape {
                probe: Probe::Stmt(s),
                branches: Vec::new(),
                rest: Vec::new(),
                kind: ShapeKind::View,
            })
        }
        "RETURN" if t.get(1).is_some_and(|x| x.up == "QUERY") => {
            t.remove(1);
            let s = parse_stmt(&t[1..], d)?;
            Ok(data(Probe::Stmt(s)))
        }
        "SET"
            if d == Dialect::Tsql
                && t.len() >= 3
                && t[1].kind == K::Var
                && compound_op(&t[2].text).is_some() =>
        {
            // SET @v += e  →  SET @v = @v + (e)
            let op = compound_op(&t[2].text).unwrap();
            let mut p = Parser::ext(&t[3..], d);
            let e = p.expr()?;
            if !p.at_end() {
                return Err(p.err("unexpected tokens"));
            }
            let v = Expr::Var(t[1].text.clone());
            Ok(data(Probe::Stmt(Stmt::Assign(
                v.clone(),
                Expr::Bin(op.to_string(), Box::new(v), Box::new(e)),
            ))))
        }
        _ => {
            let in_declare_section = n
                .parent()
                .is_some_and(|p| p.kind() == SyntaxKind::DECLARE_SECTION);
            if in_declare_section {
                // `name type := init`
                let mut p = Parser::ext(&t[1..], d);
                let ty = p.ty()?;
                let at = 1 + p.i;
                let default = if t
                    .get(at)
                    .is_some_and(|x| x.text == ":=" || x.text == "=" || x.up == "DEFAULT")
                {
                    let mut q = Parser::ext(&t[at + 1..], d);
                    let e = q.expr()?;
                    if !q.at_end() {
                        return Err(q.err("unexpected tokens"));
                    }
                    Some(e)
                } else {
                    None
                };
                return Ok(data(Probe::Stmt(Stmt::Declare {
                    name: t[0].text.clone(),
                    ty: Some(ty),
                    default,
                })));
            }
            // PL/pgSQL `v = e` assignment.
            if d != Dialect::Tsql
                && t.len() >= 3
                && t[1].text == "="
                && matches!(t[0].kind, K::Word | K::Quoted)
                && !matches!(first.as_str(), "SET" | "UPDATE")
            {
                let mut p = Parser::ext(&t[2..], d);
                let e = p.expr()?;
                if !p.at_end() {
                    return Err(p.err("unexpected tokens"));
                }
                return Ok(data(Probe::Stmt(Stmt::Assign(
                    Expr::Name(vec![t[0].text.clone()]),
                    e,
                ))));
            }
            // DECLARE c [options] CURSOR [options] FOR query
            if first == "DECLARE"
                && d == Dialect::Tsql
                && t.get(1).is_some_and(|x| x.kind != K::Var)
            {
                if let Some(c) = t.iter().position(|x| x.up == "CURSOR") {
                    if let Some(f) = t[c..].iter().position(|x| x.up == "FOR").map(|f| c + f) {
                        let s = parse_stmt(&t[f + 1..], d)?;
                        if !matches!(s, Stmt::Select(_)) {
                            return Err("cursor over something other than a query".into());
                        }
                        return Ok(Shape {
                            probe: Probe::Stmt(s),
                            branches: Vec::new(),
                            rest: Vec::new(),
                            kind: ShapeKind::Cursor,
                        });
                    }
                }
            }
            if first == "DECLARE" && d == Dialect::Tsql {
                // Several variables in one DECLARE: compare the list of initial values.
                let s = parse_stmt(&t, d);
                if let Ok(s) = s {
                    return Ok(data(Probe::Stmt(s)));
                }
                return Err("DECLARE of several variables or a table/cursor".into());
            }
            Ok(data(Probe::Stmt(parse_stmt(&t, d)?)))
        }
    }
}

/// The query of `CREATE [OR REPLACE | OR ALTER] VIEW name [(columns)]
/// [WITH options] AS query [WITH [CASCADED | LOCAL] CHECK OPTION]` (or
/// T-SQL `ALTER VIEW`): its token range. Temporary, recursive and
/// materialized views are left out.
fn view_query(t: &[Tok]) -> Option<(usize, usize)> {
    let v = t.iter().position(|x| x.kind == K::Word && x.up == "VIEW")?;
    let lead: Vec<&str> = t[..v].iter().map(|x| x.up.as_str()).collect();
    if !matches!(
        lead.as_slice(),
        ["CREATE"] | ["ALTER"] | ["CREATE", "OR", "REPLACE"] | ["CREATE", "OR", "ALTER"]
    ) {
        return None;
    }
    let mut depth = 0i32;
    let mut start = None;
    for (i, x) in t.iter().enumerate().skip(v + 1) {
        match x.kind {
            K::LParen => depth += 1,
            K::RParen => depth -= 1,
            K::Word if depth == 0 && x.up == "AS" => {
                start = Some(i + 1);
                break;
            }
            _ => {}
        }
    }
    let start = start?;
    let mut end = t.len();
    // A trailing `WITH [CASCADED | LOCAL] CHECK OPTION`.
    if end >= start + 3 && t[end - 1].up == "OPTION" && t[end - 2].up == "CHECK" {
        end -= 2;
        if matches!(t[end - 1].up.as_str(), "CASCADED" | "LOCAL") {
            end -= 1;
        }
        if t[end - 1].up != "WITH" {
            return None;
        }
        end -= 1;
    }
    (start < end && matches!(t[start].up.as_str(), "SELECT" | "WITH" | "(")).then_some((start, end))
}

fn compound_op(op: &str) -> Option<&'static str> {
    Some(match op {
        "+=" => "+",
        "-=" => "-",
        "*=" => "*",
        "/=" => "/",
        "%=" => "%",
        _ => return None,
    })
}

// ---- hints ----------------------------------------------------------------------

fn name_key(e: &Expr) -> Option<String> {
    match e {
        Expr::Var(v) => Some(eval::var_key(v)),
        Expr::Name(p) => p.last().map(|s| s.to_lowercase()),
        Expr::Cast(x, _) => name_key(x),
        _ => None,
    }
}

/// Key of a name in the hints: variables (T-SQL `@x`, or a PL/pgSQL name
/// that is a variable) get a leading `@`, columns do not.
fn hint_key(e: &Expr, facts: &HashMap<String, VarFact>, d: Dialect) -> Option<String> {
    match e {
        Expr::Var(v) => Some(format!("@{}", eval::var_key(v))),
        Expr::Name(p)
            if p.len() == 1 && d != Dialect::Tsql && facts.contains_key(&p[0].to_lowercase()) =>
        {
            Some(format!("@{}", p[0].to_lowercase()))
        }
        Expr::Name(p) => p.last().map(|s| s.to_lowercase()),
        Expr::Cast(x, _) => hint_key(x, facts, d),
        _ => None,
    }
}

fn lit_value(e: &Expr) -> Option<V> {
    match e {
        Expr::Num(n) => Some(match n.parse::<i128>() {
            Ok(i) => V::Int(i),
            Err(_) => V::Num(n.parse().ok()?),
        }),
        Expr::Str(s) => Some(V::Str(s.clone())),
        Expr::Special(s) if s == "TRUE" || s == "FALSE" => Some(V::Bool(s == "TRUE")),
        Expr::Cast(x, _) => lit_value(x),
        _ => None,
    }
}

fn class_of(v: &V) -> Class {
    match v {
        V::Int(_) => Class::Int,
        V::Num(_) => Class::Num,
        V::Str(_) => Class::Str,
        V::Bool(_) => Class::Bool,
        _ => Class::Gen,
    }
}

struct Hints<'f> {
    map: HashMap<String, Hint>,
    facts: &'f HashMap<String, VarFact>,
    schema: &'f Schema,
    d: Dialect,
}

impl Hints<'_> {
    fn note(&mut self, name: &str, class: Option<Class>, lit: Option<V>) {
        let h = self.map.entry(name.to_string()).or_default();
        if h.class.is_none() {
            h.class = class;
        }
        if let Some(l) = lit {
            if !h.lits.contains(&l) {
                h.lits.push(l);
            }
        }
    }

    fn key(&self, e: &Expr) -> Option<String> {
        hint_key(e, self.facts, self.d)
    }

    fn typed_class(&self, e: &Expr) -> Option<Class> {
        let n = self.key(e)?;
        let ty = match n.strip_prefix('@') {
            Some(v) => self.facts.get(v)?.ty?,
            // A column: its type, when every catalog table with a column of
            // that name agrees on the class.
            None => {
                let Expr::Name(p) = e else { return None };
                let col = p.last()?.to_lowercase();
                let mut tys = self
                    .schema
                    .tables()
                    .filter_map(|t| t.column(&col))
                    .map(|c| c.ty);
                let first = tys.next()?;
                let class = |t: &super::schema::Ty| (t.fam, t.len == Some(1));
                if !tys.all(|t| class(&t) == class(&first)) {
                    return None;
                }
                first
            }
        };
        Some(match ty.fam {
            super::schema::Fam::Int if ty.len == Some(1) => Class::Bit,
            super::schema::Fam::Int => Class::Int,
            super::schema::Fam::Exact | super::schema::Fam::Float => Class::Num,
            super::schema::Fam::Bool => Class::Bool,
            super::schema::Fam::Date => Class::Date,
            super::schema::Fam::Timestamp | super::schema::Fam::TimestampTz => Class::Stamp,
            _ if ty.is_string() => Class::Str,
            _ => return None,
        })
    }

    fn pair(&mut self, l: &Expr, r: &Expr) {
        for (x, y) in [(l, r), (r, l)] {
            // `v::interval > '1 week'::interval`: the literal is a candidate for v.
            let x = match x {
                Expr::Cast(inner, _) if self.d == Dialect::Postgres => &**inner,
                _ => x,
            };
            // `DAY(d) = 1`, `MONTH(d) = 7`, `YEAR(d) = 2019`: a date with that part.
            if let (Expr::Call(c), Some(V::Int(k))) = (x, lit_value(y)) {
                let name = c.name.last().map(|s| s.to_uppercase()).unwrap_or_default();
                let part_arg = match (name.as_str(), c.args.as_slice()) {
                    ("DAY" | "MONTH" | "YEAR", [a]) => Some((name.clone(), a)),
                    ("DATEPART", [Expr::Name(p), a]) if p.len() == 1 => {
                        Some((p[0].to_uppercase(), a))
                    }
                    _ => None,
                };
                if let Some((part, n)) = part_arg.and_then(|(p, a)| Some((p, self.key(a)?))) {
                    let date = match part.as_str() {
                        "DAY" | "DD" | "D" if (1..=28).contains(&k) => {
                            Some(format!("2020-01-{k:02}"))
                        }
                        "MONTH" | "MM" | "M" if (1..=12).contains(&k) => {
                            Some(format!("2020-{k:02}-01"))
                        }
                        "YEAR" | "YY" | "YYYY" if (1900..=2100).contains(&k) => {
                            Some(format!("{k:04}-01-01"))
                        }
                        _ => None,
                    };
                    if let Some(dt) = date {
                        self.note(&n, None, Some(V::Str(dt)));
                    }
                }
            }
            if let Some(n) = self.key(x) {
                if let Some(v) = lit_value(y) {
                    self.note(&n, Some(class_of(&v)), Some(v));
                } else if let Some(c) = self.typed_class(y) {
                    self.note(&n, Some(c), None);
                }
            }
        }
    }

    fn expr(&mut self, e: &Expr) {
        eval::walk_expr(e, &mut |x| match x {
            Expr::Bin(op, l, r) => {
                if matches!(
                    op.as_str(),
                    "=" | "<>" | "<" | ">" | "<=" | ">=" | "+" | "-" | "*" | "/"
                ) {
                    self.pair(l, r);
                }
                if op == "||" {
                    for y in [l, r] {
                        if let Some(n) = self.key(y) {
                            self.note(&n, Some(Class::Str), None);
                        }
                    }
                }
            }
            Expr::IsNull(y, _) => {
                if let Some(n) = self.key(y) {
                    self.map.entry(n).or_default().null_tested = true;
                }
            }
            Expr::InList(y, list, _) => {
                for z in list {
                    self.pair(y, z);
                }
            }
            Expr::Between(y, lo, hi, _) => {
                self.pair(y, lo);
                self.pair(y, hi);
            }
            Expr::Like(y, _, _, _) => {
                if let Some(n) = self.key(y) {
                    self.note(&n, Some(Class::Str), None);
                }
            }
            // Text read as an interval: try interval text.
            Expr::Cast(y, ty) if self.d == Dialect::Postgres && ty.name == "INTERVAL" => {
                if let Some(n) = self.key(y) {
                    for t in ["1 day", "01:00:00"] {
                        self.note(&n, None, Some(V::Str(t.into())));
                    }
                }
            }
            Expr::Case(Some(o), whens, _) => {
                for (w, _) in whens {
                    self.pair(o, w);
                }
            }
            Expr::Exists(q) | Expr::Subquery(q) | Expr::InQuery(_, q, _) => self.query(q),
            Expr::Call(c) => {
                let name = c.name.last().map(|s| s.to_uppercase()).unwrap_or_default();
                if matches!(name.as_str(), "COALESCE" | "ISNULL" | "NULLIF") {
                    for y in &c.args {
                        if let Some(n) = self.key(y) {
                            self.map.entry(n).or_default().null_tested = true;
                        }
                    }
                    if c.args.len() >= 2 {
                        self.pair(&c.args[0], &c.args[1]);
                    }
                }
                // The length of a string: try the empty string.
                if matches!(
                    name.as_str(),
                    "LEN" | "LENGTH" | "CHAR_LENGTH" | "DATALENGTH"
                ) && c.args.len() == 1
                {
                    if let Some(n) = self.key(&c.args[0]) {
                        self.note(&n, Some(Class::Str), Some(V::Str(String::new())));
                    }
                }
                // A string that is split on a separator: try values with
                // one and two separators, so that every part is non-empty
                // for some candidate.
                if name == "SPLIT_PART" && c.args.len() == 3 {
                    if let (Some(n), Some(V::Str(sep))) =
                        (self.key(&c.args[0]), lit_value(&c.args[1]))
                    {
                        if !sep.is_empty() {
                            for v in [format!("x{sep}y"), format!("x{sep}y{sep}z")] {
                                self.note(&n, Some(Class::Str), Some(V::Str(v)));
                            }
                        }
                    }
                }
            }
            _ => {}
        });
    }

    fn query(&mut self, q: &super::ast::Select) {
        for c in &q.ctes {
            self.query(&c.query);
        }
        for (_, o) in &q.set_ops {
            self.query(o);
        }
        for i in &q.items {
            self.expr(&i.expr);
        }
        for e in q.where_.iter().chain(q.having.iter()) {
            self.expr(e);
        }
        for f in &q.from {
            self.from(f);
        }
    }

    fn from(&mut self, f: &super::ast::From) {
        match f {
            super::ast::From::Join {
                left, right, on, ..
            } => {
                self.from(left);
                self.from(right);
                if let Some(o) = on {
                    self.expr(o);
                }
            }
            super::ast::From::Sub { query, .. } => self.query(query),
            _ => {}
        }
    }

    fn probe(&mut self, p: &Probe) {
        match p {
            Probe::Conds(es) | Probe::Values(es) => {
                for e in es {
                    self.expr(e);
                }
            }
            Probe::Stmt(s) => match s {
                Stmt::Select(q) => self.query(q),
                Stmt::Insert { source, .. } => match source {
                    super::ast::InsertSource::Values(rows) => {
                        for e in rows.iter().flatten() {
                            self.expr(e);
                        }
                    }
                    super::ast::InsertSource::Query(q) => self.query(q),
                    _ => {}
                },
                Stmt::Update {
                    sets, from, where_, ..
                } => {
                    for (c, e) in sets {
                        self.expr(e);
                        if let Some(c) = c.first() {
                            self.pair(&Expr::Name(vec![c.clone()]), e);
                        }
                    }
                    for f in from {
                        self.from(f);
                    }
                    if let Some(w) = where_ {
                        self.expr(w);
                    }
                }
                Stmt::Delete { from, where_, .. } => {
                    for f in from {
                        self.from(f);
                    }
                    if let Some(w) = where_ {
                        self.expr(w);
                    }
                }
                Stmt::Assign(t, e) => {
                    self.expr(e);
                    self.pair(t, e);
                }
                Stmt::Return(Some(e)) => self.expr(e),
                Stmt::Declare {
                    default: Some(e), ..
                } => self.expr(e),
                _ => {}
            },
        }
    }
}

// ---- examination ------------------------------------------------------------------

fn examine(
    ra: &Routine,
    rb: &Routine,
    na: &SyntaxNode,
    nb: &SyntaxNode,
    sa: &Schema,
    sb: &Schema,
    changed: &(Vec<Span>, Vec<Span>),
) -> Result<Decided, String> {
    let d = ra.d;
    if na.kind() != nb.kind() {
        return Err("different kinds of statement".into());
    }
    learn_spelling(ra);
    if let Some(dec) = validity(ra, rb, na, nb, sa) {
        return Ok(dec);
    }
    if na.kind() == SyntaxKind::ROUTINE {
        return header_change(ra, rb, na, nb, changed);
    }
    if let (Some(ta), Some(tb)) = (table_elements(na), table_elements(nb)) {
        return table_change(ra, rb, na, &ta, &tb, sa);
    }
    let xa = shape(na, d)?;
    let xb = shape(nb, d)?;
    if xa.kind != xb.kind {
        return Err("different kinds of statement".into());
    }
    let at_a = cst::sig_range(na).0;
    let at_b = cst::sig_range(nb).0;
    let facts_a = ra.facts(at_a, na);
    let facts_b = rb.facts(at_b, nb);
    // Variables must mean the same on both sides.
    for (k, fa) in &facts_a {
        if let Some(fb) = facts_b.get(k) {
            if fa.value != fb.value || fa.origin != fb.origin || fa.ty != fb.ty {
                return Err(format!("variable {k} differs between the versions"));
            }
        }
    }
    let reach_a = ra.reach(na)?;
    let reach_b = rb.reach(nb)?;
    // The witness runs this statement alone: other changes must not feed it
    // or consume what it writes.
    for (r, n, ch, reach) in [
        (ra, na, &changed.0, &reach_a),
        (rb, nb, &changed.1, &reach_b),
    ] {
        if let Some(why) = r.interacts(n, ch, reach) {
            return Err(format!("another change interacts with it: {why}"));
        }
    }
    let mut hints = Hints {
        map: HashMap::new(),
        facts: &facts_a,
        schema: sa,
        d,
    };
    hints.probe(&xa.probe);
    hints.probe(&xb.probe);
    for (g, _) in reach_a.guards.iter().chain(&reach_b.guards) {
        hints.expr(g);
    }
    let hint_map = hints.map;
    let focus = focus_tables(&tokens(na), &tokens(nb));
    let records: Vec<String> = ra.records.iter().cloned().collect();
    // The catalog of side A serves both (same dialect, same database).
    let _ = sb;
    let make = || {
        let mut c = Ctx::new(d, sa);
        c.vars = facts_a.clone();
        c.records = records.clone();
        c.hints = hint_map.clone();
        c.guards = [reach_a.guards.clone(), reach_b.guards.clone()];
        c.focus = focus.clone();
        c
    };
    let found = match eval::search(&make, &xa.probe, &xb.probe) {
        Ok(f) => f,
        Err(m) => {
            // One version fails whenever it runs the statement.
            if let Some(dec) = run_error(ra, rb, na, nb, sa, &reach_a, &reach_b) {
                return Ok(dec);
            }
            return Err(match m {
                Miss::Unsupported(w) => format!("not evaluated: {w}"),
                Miss::NoWitness => "no witness found".to_string(),
            });
        }
    };
    let (sa_, sa_e) = cst::sig_range(na);
    let (effect, certain) = observable(ra, rb, na, nb, &xa, &xb, &found, sa_, sa_e)?;
    let mut assumptions = Vec::new();
    if found.assumed {
        assumptions.push(local_values().id);
    }
    let mut untraced: Vec<usize> = reach_a.untraced.clone();
    let path_assumed =
        !untraced.is_empty() || !reach_b.untraced.is_empty() || found.unreached || !certain;
    if path_assumed {
        assumptions.push(witness_path().id);
    }
    untraced.sort_unstable();
    untraced.dedup();
    let line_a = ra.side.line_of(na);
    let line_b = rb.side.line_of(nb);
    let inputs = describe_inputs(&found, d);
    let mut witness = format!(
        "{} (A line {line_a}, B line {line_b}){}: {effect}",
        what(&xa, na, d),
        if inputs.is_empty() {
            String::new()
        } else {
            format!(" when {inputs}")
        }
    );
    if !untraced.is_empty() {
        let lines: Vec<String> = untraced.iter().map(usize::to_string).collect();
        witness.push_str(&format!(
            " Reaching it with these inputs assumes the conditions or jumps at line{} {}.",
            if lines.len() > 1 { "s" } else { "" },
            lines.join(", ")
        ));
    }
    Ok(Decided {
        witness,
        assumptions,
    })
}

/// A string no conversion to a number, money or date and time accepts.
fn never_number_or_date(t: &str) -> bool {
    t.chars().any(|c| {
        matches!(
            c,
            '[' | ']' | '=' | ';' | '(' | ')' | '{' | '}' | '%' | '\'' | '"'
        )
    })
}

// ---- table definitions ----------------------------------------------------

/// The elements (columns and constraints) of `CREATE TABLE name (...)`, a
/// permanent table, as token lists.
fn table_elements(n: &SyntaxNode) -> Option<Vec<Vec<Tok>>> {
    if n.kind() != SyntaxKind::STATEMENT {
        return None;
    }
    let t = tokens(n);
    let at = t
        .iter()
        .position(|x| x.kind == K::Word && x.up == "TABLE")?;
    if t.first()?.up != "CREATE" || at > 1 {
        return None;
    }
    let open = t.iter().position(|x| x.kind == K::LParen)?;
    // Only the name stands between TABLE and the parenthesis.
    if t[at + 1..open].iter().any(|x| {
        !matches!(x.kind, K::Word | K::Quoted | K::Dot | K::Param)
                // pg_partman's `@extschema@` placeholder.
                && !(x.kind == K::Op && x.text == "@")
    }) || t[at + 1..open].iter().any(|x| x.text.starts_with('#'))
    {
        return None;
    }
    let mut out = Vec::new();
    let mut cur = Vec::new();
    let mut depth = 0i32;
    for x in &t[open..] {
        match x.kind {
            K::LParen => {
                depth += 1;
                if depth == 1 {
                    continue;
                }
            }
            K::RParen => {
                depth -= 1;
                if depth == 0 {
                    out.push(std::mem::take(&mut cur));
                    break;
                }
            }
            K::Comma if depth == 1 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(x.clone());
    }
    (depth == 0 && !out.is_empty()).then_some(out)
}

/// A table definition whose only change is one element: a column's length,
/// precision or scale, its constant DEFAULT, or a CHECK constraint. The
/// witness is a row stored differently or accepted by one version only.
#[allow(clippy::too_many_arguments)]
fn table_change(
    ra: &Routine,
    rb: &Routine,
    na: &SyntaxNode,
    ea: &[Vec<Tok>],
    eb: &[Vec<Tok>],
    schema: &Schema,
) -> Result<Decided, String> {
    let d = ra.d;
    let same = |x: &[Tok], y: &[Tok]| {
        x.len() == y.len()
            && x.iter()
                .zip(y)
                .all(|(p, q)| p.kind == q.kind && p.up == q.up)
    };
    if ea.len() != eb.len() {
        return Err("columns or constraints added or removed".into());
    }
    let changed: Vec<usize> = (0..ea.len()).filter(|&i| !same(&ea[i], &eb[i])).collect();
    let [i] = changed.as_slice() else {
        return Err("several table elements changed".into());
    };
    let (xa, xb) = (&ea[*i], &eb[*i]);
    let (la, lb) = (
        ra.side.lines.line_col(xa.first().map_or(0, |x| x.off)).0,
        rb.side.lines.line_col(xb.first().map_or(0, |x| x.off)).0,
    );
    let table = {
        let t = tokens(na);
        let at = t.iter().position(|x| x.up == "TABLE").unwrap_or(0);
        let open = t
            .iter()
            .position(|x| x.kind == K::LParen)
            .unwrap_or(t.len());
        t[at + 1..open]
            .iter()
            .map(|x| x.text.as_str())
            .collect::<String>()
    };
    let witness = |text: String| {
        Ok(Decided {
            witness: format!("The definition of table {table} (A line {la}, B line {lb}): {text}"),
            assumptions: Vec::new(),
        })
    };
    let check = |x: &[Tok]| -> Option<(usize, usize)> {
        let c = x
            .iter()
            .position(|y| y.kind == K::Word && y.up == "CHECK")?;
        let lead_ok = c == 0 || (c == 2 && x[0].up == "CONSTRAINT");
        (lead_ok && x.get(c + 1).is_some_and(|y| y.kind == K::LParen))
            .then_some((c + 2, close_of(x, c + 1)?))
    };
    // A CHECK constraint.
    if let (Some((sa_, ea_)), Some((sb_, eb_))) = (check(xa), check(xb)) {
        if ea_ + 1 != xa.len() || eb_ + 1 != xb.len() {
            return Err("CHECK constraint options".into());
        }
        let cols = column_types(ea, d);
        let parse = |t: &[Tok]| -> Result<Expr, String> {
            let mut p = Parser::ext(t, d);
            let e = p.expr()?;
            if !p.at_end() {
                return Err(p.err("unexpected tokens"));
            }
            Ok(e)
        };
        let ca = as_row_vars(parse(&xa[sa_..ea_])?, &cols);
        let cb = as_row_vars(parse(&xb[sb_..eb_])?, &cols);
        // The other CHECK constraints, the same in both versions, must accept the row too.
        let mut others = Vec::new();
        for (k, x) in ea.iter().enumerate() {
            if k == *i {
                continue;
            }
            if let Some((s0, e0)) = check(x) {
                others.push(as_row_vars(parse(&x[s0..e0])?, &cols));
            } else if x.iter().any(|y| y.up == "CHECK") {
                return Err("a column CHECK constraint is not read".into());
            }
        }
        // 1 when every constraint accepts the row (TRUE or NULL), 0 when one rejects it.
        let accept = |e: Expr| {
            let whens = std::iter::once(e)
                .chain(others.iter().cloned())
                .map(|c| {
                    (
                        Expr::Unary("NOT".into(), Box::new(c)),
                        Expr::Num("0".into()),
                    )
                })
                .collect();
            Expr::Case(None, whens, Some(Box::new(Expr::Num("1".into()))))
        };
        let (pa, pb) = (
            Probe::Values(vec![accept(ca)]),
            Probe::Values(vec![accept(cb)]),
        );
        let facts: HashMap<String, VarFact> = cols
            .iter()
            .map(|(n, ty, nullable)| {
                (
                    n.clone(),
                    VarFact {
                        ty: *ty,
                        origin: Origin::Free,
                        value: None,
                        nullable: *nullable,
                        non_negative: false,
                        cands: Vec::new(),
                    },
                )
            })
            .collect();
        let make = || {
            let mut c = Ctx::new(d, schema);
            c.vars = facts.clone();
            c
        };
        let found = eval::search(&make, &pa, &pb).map_err(|m| match m {
            Miss::Unsupported(w) => format!("not evaluated: {w}"),
            Miss::NoWitness => "no witness found".to_string(),
        })?;
        let first = |o: &Obs| match o {
            Obs::Rows(r, _) => r.first().and_then(|r| r.first()).cloned(),
            _ => None,
        };
        let (va, vb) = (first(&found.a), first(&found.b));
        let accepted = |v: &Option<V>| matches!(v, Some(V::Int(1)));
        let rejected = |v: &Option<V>| matches!(v, Some(V::Int(0)));
        let who = if accepted(&va) && rejected(&vb) {
            "A accepts it and B rejects it"
        } else if rejected(&va) && accepted(&vb) {
            "A rejects it and B accepts it"
        } else {
            return Err("no witness found".into());
        };
        return witness(format!(
            "storing a row with {} violates the CHECK constraint of one version only: {who}.",
            describe_inputs(&found, d)
        ));
    }
    // A column: the same name, then a type or DEFAULT change.
    let (Some(na_), Some(nb_)) = (xa.first(), xb.first()) else {
        return Err("empty table element".into());
    };
    if !matches!(na_.kind, K::Word | K::Quoted) || na_.up != nb_.up || na_.kind != nb_.kind {
        return Err("a different column".into());
    }
    if matches!(
        na_.up.as_str(),
        "CONSTRAINT"
            | "PRIMARY"
            | "UNIQUE"
            | "FOREIGN"
            | "CHECK"
            | "INDEX"
            | "EXCLUDE"
            | "PERIOD"
            | "LIKE"
    ) {
        return Err("a changed table constraint".into());
    }
    let name = na_.text.clone();
    // A CHECK constraint on the column could reject the witness value in both.
    let key = eval::var_key(&sqldiff_syntax::ast::unquote(&name));
    if ea.iter().flatten().any(|y| y.up == "CHECK")
        && ea.iter().flatten().any(|y| {
            matches!(y.kind, K::Word | K::Quoted)
                && eval::var_key(&sqldiff_syntax::ast::unquote(&y.text)) == key
        })
        && ea
            .iter()
            .filter(|x| x.iter().any(|y| y.up == "CHECK"))
            .any(|x| {
                x.iter().any(|y| {
                    matches!(y.kind, K::Word | K::Quoted)
                        && eval::var_key(&sqldiff_syntax::ast::unquote(&y.text)) == key
                })
            })
    {
        return Err("a CHECK constraint reads the column".into());
    }
    let (mut pa, mut pb) = (Parser::ext(&xa[1..], d), Parser::ext(&xb[1..], d));
    let (ta, tb) = (pa.ty()?, pb.ty()?);
    let (ra_, rb_) = (&xa[1 + pa.i..], &xb[1 + pb.i..]);
    if ta != tb {
        // Only the type changed, by length, precision or scale.
        if !same(ra_, rb_) || ta.name != tb.name {
            return Err("the column type and its options changed".into());
        }
        let (ya, yb) = (Ty::from_type(&ta, d, false), Ty::from_type(&tb, d, false));
        if ya.fam != yb.fam {
            return Err("a different type family".into());
        }
        if ya.is_string() {
            let (Some(a), Some(b)) = (ya.len, yb.len) else {
                return Err("an unbounded string column".into());
            };
            let (short, long) = if a < b { ("A", "B") } else { ("B", "A") };
            let n = a.min(b) + 1;
            return witness(format!(
                "column {name} holds {} characters in A and {} in B, so storing a value of {n} characters (none of them blanks) in it fails in {short} and succeeds in {long}.",
                a, b
            ));
        }
        if ya.fam == super::schema::Fam::Exact {
            let (Some(pa_), Some(pb_), Some(sa_), Some(sb_)) = (ya.len, yb.len, ya.scale, yb.scale)
            else {
                return Err("a decimal without precision".into());
            };
            if sa_ == sb_ && pa_ != pb_ {
                let digits = pa_.min(pb_) - sa_;
                let (short, long) = if pa_ < pb_ { ("A", "B") } else { ("B", "A") };
                let value = format!("1{}", "0".repeat(digits as usize));
                return witness(format!(
                    "column {name} is decimal({pa_},{sa_}) in A and decimal({pb_},{sb_}) in B, so storing {value} in it overflows in {short} and succeeds in {long}."
                ));
            }
            if pa_ == pb_ && sa_ != sb_ {
                let s = sa_.max(sb_);
                let value = format!("0.{}5", "0".repeat(s as usize - 1));
                let (fewer, more) = if sa_ < sb_ { ("A", "B") } else { ("B", "A") };
                return witness(format!(
                    "column {name} is decimal({pa_},{sa_}) in A and decimal({pb_},{sb_}) in B, so {value} is stored as it is in {more} and as a value with fewer decimals in {fewer}."
                ));
            }
            return Err("precision and scale both changed".into());
        }
        return Err("a type change other than length, precision or scale".into());
    }
    // The same type: a constant DEFAULT that changed.
    let default = |r: &[Tok]| -> Option<V> {
        let k = r
            .iter()
            .position(|x| x.kind == K::Word && x.up == "DEFAULT")?;
        let mut p = Parser::ext(&r[k + 1..], d);
        // T-SQL writes `DEFAULT ((30))` and `DEFAULT ((-1))`.
        fn bare(mut e: &Expr) -> Option<&Expr> {
            while let Expr::Tuple(v) = e {
                let [x] = v.as_slice() else { return None };
                e = x;
            }
            Some(e)
        }
        let e = p.expr().ok()?;
        match bare(&e)? {
            Expr::Unary(op, x) if op == "-" => match constant_value(bare(x)?)? {
                V::Int(i) => Some(V::Int(-i)),
                V::Num(f) => Some(V::Num(-f)),
                _ => None,
            },
            Expr::Unary(..) => None,
            e => constant_value(e),
        }
    };
    let (Some(da), Some(db)) = (default(ra_), default(rb_)) else {
        return Err("a column change other than its type or a constant DEFAULT".into());
    };
    let mut c = Ctx::new(d, schema);
    let (Ok(va), Ok(vb)) = (c.cast_value(da, &ta), c.cast_value(db, &tb)) else {
        return Err("the defaults are not evaluated".into());
    };
    if matches!(va, V::Err(_)) || matches!(vb, V::Err(_)) || eval::same(&va, &vb) != Some(false) {
        return Err("the defaults may store the same value".into());
    }
    witness(format!(
        "a row stored without a value for column {name} gets {} in A and {} in B.",
        va.show(&[]),
        vb.show(&[])
    ))
}

/// Index of the parenthesis closing the one at `open`.
fn close_of(t: &[Tok], open: usize) -> Option<usize> {
    let mut depth = 0;
    for (i, x) in t.iter().enumerate().skip(open) {
        match x.kind {
            K::LParen => depth += 1,
            K::RParen => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Column names (lower case), declared types and nullability of a table's elements.
fn column_types(els: &[Vec<Tok>], d: Dialect) -> Vec<(String, Option<Ty>, bool)> {
    els.iter()
        .filter(|x| {
            x.first().is_some_and(|y| {
                matches!(y.kind, K::Word | K::Quoted)
                    && !matches!(
                        y.up.as_str(),
                        "CONSTRAINT"
                            | "PRIMARY"
                            | "UNIQUE"
                            | "FOREIGN"
                            | "CHECK"
                            | "INDEX"
                            | "EXCLUDE"
                            | "PERIOD"
                            | "LIKE"
                    )
            })
        })
        .map(|x| {
            let ty = Parser::ext(&x[1..], d)
                .ty()
                .ok()
                .map(|t| Ty::from_type(&t, d, false));
            let not_null = x.windows(2).any(|w| w[0].up == "NOT" && w[1].up == "NULL")
                || x.iter().any(|y| y.up == "PRIMARY");
            (
                eval::var_key(&sqldiff_syntax::ast::unquote(&x[0].text)),
                ty,
                !not_null,
            )
        })
        .collect()
}

/// A CHECK condition with the table's columns read as variables (the row
/// being stored).
fn as_row_vars(e: Expr, cols: &[(String, Option<Ty>, bool)]) -> Expr {
    eval::map_expr(e, &|x| match x {
        Expr::Name(p)
            if p.len() == 1 && cols.iter().any(|(n, _, _)| *n == eval::var_key(&p[0])) =>
        {
            Some(Expr::Var(p[0].clone()))
        }
        _ => None,
    })
}

/// Tables named in the innermost parenthesised query around the first token
/// where the two versions differ (or in the whole statement when there is
/// no such query).
fn focus_tables(ta: &[Tok], tb: &[Tok]) -> Vec<String> {
    let at = ta
        .iter()
        .zip(tb)
        .position(|(x, y)| x.kind != y.kind || x.up != y.up)
        .unwrap_or(ta.len().min(tb.len()));
    // The innermost `( SELECT` that is still open at `at`.
    let mut opens: Vec<usize> = Vec::new();
    for (i, x) in ta.iter().enumerate().take(at.min(ta.len())) {
        match x.kind {
            K::LParen => opens.push(i),
            K::RParen => {
                opens.pop();
            }
            _ => {}
        }
    }
    let (start, end) = match opens
        .iter()
        .rev()
        .find(|&&o| ta.get(o + 1).is_some_and(|x| x.up == "SELECT"))
    {
        Some(&o) => (o + 1, close_of(ta, o).unwrap_or(ta.len())),
        None => (0, ta.len()),
    };
    let mut out = Vec::new();
    let mut depth = 0;
    let mut i = start;
    while i < end {
        let x = &ta[i];
        match x.kind {
            K::LParen => depth += 1,
            K::RParen => depth -= 1,
            K::Word if depth == 0 && (x.up == "FROM" || x.up == "JOIN") => {
                let mut parts = Vec::new();
                let mut j = i + 1;
                while let Some(y) = ta.get(j) {
                    if !matches!(y.kind, K::Word | K::Quoted) {
                        break;
                    }
                    parts.push(sqldiff_syntax::ast::unquote(&y.text));
                    if ta.get(j + 1).is_some_and(|z| z.kind == K::Dot) {
                        j += 2;
                    } else {
                        break;
                    }
                }
                if !parts.is_empty() {
                    let key = eval::table_key(&parts);
                    if !out.contains(&key) {
                        out.push(key);
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// One version's statement fails whenever it runs (a type error SQL Server
/// raises when it compiles the statement) and the other's does not have that
/// error. Running the statement, and the other version running it without
/// error, are assumed.
fn run_error(
    ra: &Routine,
    rb: &Routine,
    na: &SyntaxNode,
    nb: &SyntaxNode,
    schema: &Schema,
    reach_a: &Reach,
    reach_b: &Reach,
) -> Option<Decided> {
    let (la, lb) = (ra.side.line_of(na), rb.side.line_of(nb));
    let witness = match (
        type_error_at(ra, na, schema, true),
        type_error_at(rb, nb, schema, true),
    ) {
        (None, Some(why)) => format!(
            "B fails whenever it runs line {lb}: {why}, which SQL Server rejects when it compiles the statement; A line {la} does not have this error."
        ),
        (Some(why), None) => format!(
            "A fails whenever it runs line {la}: {why}, which SQL Server rejects when it compiles the statement; B line {lb} does not have this error."
        ),
        _ => return None,
    };
    let _ = (reach_a, reach_b);
    Some(Decided {
        witness,
        assumptions: vec![witness_path().id],
    })
}

/// A routine header whose only change is one parameter's default value or
/// decimal precision.
fn header_change(
    ra: &Routine,
    rb: &Routine,
    na: &SyntaxNode,
    nb: &SyntaxNode,
    others: &(Vec<Span>, Vec<Span>),
) -> Result<Decided, String> {
    let d = ra.d;
    let ta = checked_tokens(na).concat();
    let tb = checked_tokens(nb).concat();
    if ta.len() != tb.len() {
        return Err("routine headers differ in shape".into());
    }
    let pa = header_params(na, d);
    let pb = header_params(nb, d);
    if pa.len() != pb.len()
        || pa
            .iter()
            .zip(&pb)
            .any(|(x, y)| x.name != y.name || x.out != y.out)
    {
        return Err("parameter lists differ".into());
    }
    let changed: Vec<(&lower::Param, &lower::Param)> = pa
        .iter()
        .zip(&pb)
        .filter(|(x, y)| x.ty != y.ty || x.default != y.default)
        .collect();
    // Every differing header token must belong to that parameter's change.
    let differing = ta.iter().zip(&tb).filter(|(x, y)| x.text != y.text).count();
    let [(x, y)] = changed.as_slice() else {
        return Err("routine headers are not compared".into());
    };
    let shown = if d == Dialect::Tsql {
        format!("@{}", spell(&x.name))
    } else {
        spell(&x.name)
    };
    // The parameter's value must not reach a body change that could make up for it.
    for (r, n, ch) in [(ra, na, &others.0), (rb, nb, &others.1)] {
        let body = n
            .descendants()
            .find(|b| b.kind() == SyntaxKind::ROUTINE_BODY)
            .map_or((0, 0), |b| cst::sig_range(&b));
        let seed = HashSet::from([x.name.trim_start_matches('@').to_lowercase()]);
        if let Some(l) = r.reaches_change(seed, (body.0, body.0), r.units(), ch) {
            return Err(format!(
                "another change interacts with it: {shown} reaches the changed code at line {l}"
            ));
        }
    }
    if x.ty == y.ty && differing == 1 {
        let parse = |s: &Option<String>| -> Option<V> {
            let s = s.as_ref()?;
            let toks =
                super::ast::tokens(&sqldiff_syntax::parse(&format!("SELECT {s}"), d).syntax());
            let mut p = Parser::ext(&toks[1..], d);
            let e = p.expr().ok()?;
            if !p.at_end() {
                return None;
            }
            constant_value(&e)
        };
        let (Some(va), Some(vb)) = (parse(&x.default), parse(&y.default)) else {
            return Err("parameter defaults that are not constants".into());
        };
        if eval::same(&va, &vb) != Some(false) {
            return Err("parameter defaults that may be equal".into());
        }
        let r = ra.read_in_body(&x.name);
        if r == Read::Never {
            return Err(format!("{shown} is not read"));
        }
        let mut assumptions = Vec::new();
        if !matches!(r, Read::Always(_)) {
            assumptions.push(witness_path().id);
        }
        return Ok(Decided {
            witness: format!(
                "When the caller leaves out {shown} (A line {}, B line {}), A uses {} and B uses {} ({}).",
                ra.side.line_of(na),
                rb.side.line_of(nb),
                va.show(&[]),
                vb.show(&[]),
                read_note(&x.name, r, d)
            ),
            assumptions,
        });
    }
    // A T-SQL decimal parameter whose precision grows at the same scale: a
    // value that fits only the wider type fails the call on the narrower
    // side. (PostgreSQL ignores the precision of a parameter's type.)
    if let (Some(a), Some(b)) = (x.ty, y.ty) {
        use super::schema::Fam;
        if d == Dialect::Tsql
            && a.fam == Fam::Exact
            && b.fam == Fam::Exact
            && a.scale == b.scale
            && x.default == y.default
        {
            if let (Some(pa), Some(pb)) = (a.len, b.len) {
                let scale = a.scale.unwrap_or(0);
                let (lo, hi, narrow) = if pa < pb {
                    (pa, pb, "A")
                } else {
                    (pb, pa, "B")
                };
                let _ = hi;
                let digits = lo.saturating_sub(scale);
                let value = format!("1{}", "0".repeat(digits as usize));
                let wide = if narrow == "A" { "B" } else { "A" };
                let r = ra.read_in_body(&x.name);
                return Ok(Decided {
                    witness: format!(
                        "Passing {shown} = {value} (A line {}, B line {}): {narrow} fails converting the argument (arithmetic overflow) and {wide} accepts it ({}).",
                        ra.side.line_of(na),
                        rb.side.line_of(nb),
                        read_note(&x.name, r, d)
                    ),
                    // Whether the accepting side then behaves visibly differently is not traced.
                    assumptions: vec![witness_path().id],
                });
            }
        }
    }
    // A T-SQL string parameter of another length: SQL Server silently
    // truncates an argument to the parameter's length, so a longer argument
    // reaches the body cut short on the narrower side. (PostgreSQL ignores
    // the length of a parameter's type.)
    if let (Some(a), Some(b)) = (x.ty, y.ty) {
        use super::schema::Fam;
        if d == Dialect::Tsql
            && a.fam == b.fam
            && matches!(a.fam, Fam::VarChar | Fam::Char)
            && x.default == y.default
            && a.len != b.len
        {
            // None is MAX; the narrower length is known.
            let narrow = match (a.len, b.len) {
                (Some(p), Some(q)) => p.min(q),
                (Some(p), None) | (None, Some(p)) => p,
                _ => return Err("routine headers are not compared".into()),
            };
            let (cut, kept) = if a.len == Some(narrow) {
                ("A", "B")
            } else {
                ("B", "A")
            };
            let reads = ra.value_reads(&x.name);
            if reads.is_empty() {
                return Err(format!("{shown} is not read for its value"));
            }
            let lines: Vec<String> = reads.iter().map(usize::to_string).collect();
            return Ok(Decided {
                witness: format!(
                    "Passing {shown} with {} characters (A line {}, B line {}): {cut} truncates it to {narrow} characters and {kept} keeps all of them; {shown} is read at line{} {}.",
                    narrow + 1,
                    ra.side.line_of(na),
                    rb.side.line_of(nb),
                    if lines.len() > 1 { "s" } else { "" },
                    lines.join(", ")
                ),
                // That the cut value then changes an effect is not traced.
                assumptions: vec![witness_path().id],
            });
        }
    }
    Err("routine headers are not compared".into())
}

/// Token lists of a unit that are checked for validity: the statement, the
/// conditions of a compound statement, or a routine header.
fn checked_tokens(n: &SyntaxNode) -> Vec<Vec<Tok>> {
    match n.kind() {
        SyntaxKind::ROUTINE => vec![cst::header_tokens(n)
            .iter()
            .map(Tok::from_syntax)
            .filter(|t| t.kind != K::Semi)
            .collect()],
        SyntaxKind::STATEMENT => vec![tokens(n)],
        _ => n
            .children()
            .filter(|c| c.kind() == SyntaxKind::CONDITION)
            .map(|c| tokens(&c))
            .collect(),
    }
}

/// One version certainly fails to be created at the changed unit and the other does not.
fn validity(
    ra: &Routine,
    rb: &Routine,
    na: &SyntaxNode,
    nb: &SyntaxNode,
    schema: &Schema,
) -> Option<Decided> {
    let d = ra.d;
    let header = na.kind() == SyntaxKind::ROUTINE;
    let check = |n: &SyntaxNode| {
        let lists = checked_tokens(n);
        lists.iter().find_map(|t| invalid(t, d, header))
    };
    // Parentheses that do not balance, in the statement and in the whole text.
    let parens = |r: &Routine, n: &SyntaxNode| {
        let count = |t: &[Tok]| {
            (
                t.iter().filter(|x| x.kind == K::LParen).count(),
                t.iter().filter(|x| x.kind == K::RParen).count(),
            )
        };
        let (fo, fc) = count(&r.toks);
        let (open, close) = count(&tokens(n));
        (fo != fc && open != close && n.kind() == SyntaxKind::STATEMENT)
            .then(|| format!("{open} opening and {close} closing parentheses"))
    };
    let check = |r: &Routine, n: &SyntaxNode| {
        check(n)
            .or_else(|| parens(r, n))
            .or_else(|| type_error(r, n, schema))
    };
    let lang = match d {
        Dialect::Tsql => "T-SQL",
        Dialect::Postgres => "PL/pgSQL",
        Dialect::Db2i => "SQL PL",
    };
    let (la, lb) = (ra.side.line_of(na), rb.side.line_of(nb));
    match (check(ra, na), check(rb, nb)) {
        (None, Some(why)) => Some(Decided {
            witness: format!("B is not valid {lang} at line {lb}: {why}, so creating B fails; A line {la} is valid."),
            assumptions: Vec::new(),
        }),
        (Some(why), None) => Some(Decided {
            witness: format!("A is not valid {lang} at line {la}: {why}, so creating A fails; B line {lb} is valid."),
            assumptions: Vec::new(),
        }),
        _ => None,
    }
}

/// A T-SQL statement that SQL Server rejects when it compiles the routine:
/// arithmetic other than `+` on two character strings ("The data types
/// nvarchar and nvarchar are incompatible in the subtract operator"). A
/// statement that reads a table may be compiled only when it runs (deferred
/// name resolution), so only statements without tables, or reading `sys`
/// catalog views alone, are judged.
fn type_error(r: &Routine, n: &SyntaxNode, schema: &Schema) -> Option<String> {
    type_error_at(r, n, schema, false)
}

/// The same error in any statement, tables or not: SQL Server raises it
/// when it compiles the statement, at the latest when the statement runs.
fn type_error_at(r: &Routine, n: &SyntaxNode, schema: &Schema, run: bool) -> Option<String> {
    if r.d != Dialect::Tsql {
        return None;
    }
    let x = shape(n, r.d).ok()?;
    if !run
        && eval::probe_tables(&x.probe)
            .iter()
            .any(|t| !t.starts_with("sys."))
    {
        return None;
    }
    let mut roots: Vec<&Expr> = Vec::new();
    // Tables by alias (or name), for the types of columns.
    let mut tables: Vec<(String, String)> = Vec::new();
    fn collect(f: &super::ast::From, out: &mut Vec<(String, String)>) {
        match f {
            super::ast::From::Table { name, alias, .. } => {
                let key = eval::table_key(name);
                let shown = alias
                    .clone()
                    .unwrap_or_else(|| name.last().cloned().unwrap_or_default())
                    .to_lowercase();
                out.push((shown, key));
            }
            super::ast::From::Join { left, right, .. } => {
                collect(left, out);
                collect(right, out);
            }
            _ => out.push((String::new(), String::new())),
        }
    }
    if let Probe::Stmt(Stmt::Select(q)) = &x.probe {
        for f in &q.from {
            collect(f, &mut tables);
        }
    }
    let col = |p: &[String]| -> Option<bool> {
        let table = match p {
            [c] if tables.len() == 1 => {
                let t = schema.table(&tables[0].1)?;
                return t.column(&c.to_lowercase()).map(|c| c.ty.is_string());
            }
            [a, _] => &tables.iter().find(|(s, _)| *s == a.to_lowercase())?.1,
            _ => return None,
        };
        let c = p.last()?.to_lowercase();
        schema.table(table)?.column(&c).map(|c| c.ty.is_string())
    };
    match &x.probe {
        Probe::Conds(es) | Probe::Values(es) => roots.extend(es),
        Probe::Stmt(Stmt::Assign(_, e)) | Probe::Stmt(Stmt::Return(Some(e))) => roots.push(e),
        Probe::Stmt(Stmt::Declare {
            default: Some(e), ..
        }) => roots.push(e),
        Probe::Stmt(Stmt::Select(q)) if q.set_ops.is_empty() && q.ctes.is_empty() => {
            roots.extend(q.items.iter().map(|i| &i.expr));
            roots.extend(q.where_.iter());
        }
        _ => return None,
    }
    let facts = r.facts(cst::sig_range(n).0, n);
    let mut found = None;
    for e in roots {
        eval::walk_expr(e, &mut |x| {
            if let Expr::Bin(op, a, b) = x {
                if found.is_none()
                    && matches!(op.as_str(), "-" | "*" | "/" | "%")
                    && eval::static_string_with(a, &facts, r.d, &col) == Some(true)
                    && eval::static_string_with(b, &facts, r.d, &col) == Some(true)
                {
                    found = Some(format!("`{op}` is applied to two character strings"));
                }
                // Whatever the other operand's type, the string must be
                // converted to it: a number or a date it can never be.
                if found.is_none() && run && matches!(op.as_str(), "-" | "*" | "/" | "%") {
                    for side in [a, b] {
                        if let Expr::Str(t) = &**side {
                            if never_number_or_date(t) {
                                found = Some(format!(
                                    "`{op}` needs '{t}' as a number or date, which it is not"
                                ));
                            }
                        }
                    }
                }
            }
        });
    }
    found
}

fn what(x: &Shape, n: &SyntaxNode, d: Dialect) -> String {
    match x.kind {
        ShapeKind::If => "The IF condition".into(),
        ShapeKind::While => "The WHILE condition".into(),
        ShapeKind::ExitWhen => "The EXIT WHEN condition".into(),
        ShapeKind::ContinueWhen => "The CONTINUE WHEN condition".into(),
        ShapeKind::Raise => "The raised error".into(),
        ShapeKind::Message => "The message".into(),
        ShapeKind::Cursor => "The cursor query".into(),
        ShapeKind::ForQuery => "The FOR loop query".into(),
        ShapeKind::View => "The view query".into(),
        ShapeKind::Data => format!("The {}", stmt_kind(n, d).label()),
    }
}

thread_local! {
    /// Original spelling of the names in the routine being examined, by
    /// lower-cased name, so witnesses show `@MinPrice` rather than `@minprice`.
    static SPELLING: std::cell::RefCell<HashMap<String, String>> = Default::default();
}

fn learn_spelling(r: &Routine) {
    let mut map = HashMap::new();
    for t in &r.toks {
        let name = match t.kind {
            K::Var => t.text.trim_start_matches('@').to_string(),
            K::Word => t.text.clone(),
            K::Quoted => cst::unquote(&t.text).to_string(),
            _ => continue,
        };
        map.entry(name.to_lowercase()).or_insert(name);
    }
    SPELLING.with(|s| *s.borrow_mut() = map);
}

/// A lower-cased (possibly dotted) name in its original spelling.
fn spell(name: &str) -> String {
    SPELLING.with(|s| {
        let s = s.borrow();
        name.split('.')
            .map(|p| {
                let bare = p.trim_start_matches('@');
                match s.get(bare) {
                    Some(o) => format!("{}{o}", &p[..p.len() - bare.len()]),
                    None => p.to_string(),
                }
            })
            .collect::<Vec<_>>()
            .join(".")
    })
}

fn describe_inputs(f: &Found, d: Dialect) -> String {
    let mut parts = Vec::new();
    let mut tables: Vec<(String, usize, Vec<String>)> = Vec::new();
    for (k, v) in &f.inputs {
        let shown = v.show(&f.opaque);
        match k {
            Key::Var(n) if n.starts_with("@now:") => {
                parts.push(format!("{}() returns {shown}", n[5..].to_uppercase()))
            }
            Key::Var(n) if n == "@@datefirst" => parts.push(format!("DATEFIRST is {shown}")),
            Key::Var(n) if n.starts_with("@env:") => {
                parts.push(format!("{} returns {shown}", &n[5..]))
            }
            Key::Var(n) if n.starts_with('@') => {
                parts.push(format!("@{} is {shown}", n.to_uppercase()))
            }
            Key::Var(n) if d == Dialect::Tsql => parts.push(format!("@{} is {shown}", spell(n))),
            Key::Var(n) => parts.push(format!("{} is {shown}", spell(n))),
            Key::Field(r, c) => parts.push(format!("{}.{} is {shown}", spell(r), spell(c))),
            Key::Col(t, row, c) => {
                let item = format!("{} = {shown}", spell(c));
                match tables.iter_mut().find(|(x, r, _)| x == t && r == row) {
                    Some(e) => e.2.push(item),
                    None => tables.push((t.clone(), *row, vec![item])),
                }
            }
        }
    }
    for (t, n) in &f.shape {
        if *n == 0 {
            parts.push(format!("{} has no rows", spell(t)));
        }
    }
    for (t, row, cols) in tables {
        let n = f.shape.get(&t).copied().unwrap_or(1);
        let which = if n > 1 {
            format!(" (row {})", row + 1)
        } else {
            String::new()
        };
        parts.push(format!(
            "{} has a row{which} with {}",
            spell(&t),
            cols.join(", ")
        ));
    }
    parts.join(", ")
}

fn show_vals(vals: &[V], opaque: &[String]) -> String {
    vals.iter()
        .map(|v| v.show(opaque))
        .collect::<Vec<_>>()
        .join(", ")
}

fn describe_obs(o: &Obs, opaque: &[String], kind: ShapeKind, branches: usize) -> String {
    match o {
        Obs::Error(e) => format!("raises an error ({e})"),
        Obs::Value(v) => format!("yields {}", v.show(opaque)),
        Obs::Assigned(vs) => {
            if vs.iter().all(Option::is_none) {
                "finds no row and leaves the variables unchanged".into()
            } else {
                format!(
                    "assigns {}",
                    vs.iter()
                        .map(|v| v
                            .as_ref()
                            .map(|v| v.show(opaque))
                            .unwrap_or_else(|| "nothing".into()))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
        Obs::Rows(rows, _) => {
            if kind == ShapeKind::Raise || kind == ShapeKind::Message {
                return format!(
                    "uses {}",
                    rows.first()
                        .map(|r| show_vals(r, opaque))
                        .unwrap_or_default()
                );
            }
            match rows.len() {
                0 => "produces no rows".into(),
                1 => format!("produces one row ({})", show_vals(&rows[0], opaque)),
                n => format!("produces {n} rows"),
            }
        }
        Obs::Writes(w) => match w.len() {
            0 => "changes no row".into(),
            1 => "changes the row".into(),
            n => format!("changes {n} rows"),
        },
        Obs::Inserted(_, rows) => match rows.len() {
            0 => "inserts no row".into(),
            1 => "inserts one row".into(),
            n => format!("inserts {n} rows"),
        },
        Obs::Branch(b) => match (kind, b) {
            (ShapeKind::While, Some(_)) => "enters the loop".into(),
            (ShapeKind::While, None) => "skips the loop".into(),
            (ShapeKind::ExitWhen, Some(_)) => "leaves the loop".into(),
            (ShapeKind::ExitWhen, None) => "stays in the loop".into(),
            (ShapeKind::ContinueWhen, Some(_)) => "skips the rest of the iteration".into(),
            (ShapeKind::ContinueWhen, None) => "runs the rest of the iteration".into(),
            (_, Some(0)) => "takes the first branch".into(),
            (_, Some(i)) if *i + 1 == branches => "takes the ELSE branch".into(),
            (_, Some(i)) => format!("takes branch {}", i + 1),
            (_, None) if branches > 0 => "takes no branch".into(),
            (_, None) => "takes no branch".into(),
        },
    }
}

/// Whether the difference in results reaches something observable; the
/// plain-language effect when it does.
#[allow(clippy::too_many_arguments)]
fn observable(
    ra: &Routine,
    rb: &Routine,
    na: &SyntaxNode,
    nb: &SyntaxNode,
    xa: &Shape,
    xb: &Shape,
    f: &Found,
    start: usize,
    end: usize,
) -> Result<(String, bool), String> {
    let d = ra.d;
    let _ = start;
    let da = describe_obs(&f.a, &f.opaque, xa.kind, xa.branches.len());
    let db = describe_obs(&f.b, &f.opaque, xb.kind, xb.branches.len());
    let both = format!("A {da} and B {db}");
    // An error on one side is always visible.
    if matches!(f.a, Obs::Error(_)) || matches!(f.b, Obs::Error(_)) {
        return Ok((format!("{both}."), true));
    }
    match xa.kind {
        ShapeKind::Raise | ShapeKind::Message => Ok((format!("{both}."), true)),
        // Whoever selects from the view sees its rows.
        ShapeKind::View => Ok((format!("{both}."), true)),
        ShapeKind::ForQuery => {
            let body = xa.branches.first().ok_or("loop without body")?;
            if effect(ra, body) == Eff::No {
                return Err("the loop body has no observable effect".into());
            }
            // What the body does with each row is not traced.
            Ok((format!("{both} (the loop body runs once per row)."), false))
        }
        ShapeKind::Cursor => {
            let name = tokens(na).get(1).map(|x| x.up.clone()).unwrap_or_default();
            let fetch = ra.toks.iter().enumerate().find(|(i, x)| {
                x.up == "FETCH"
                    && x.off >= end
                    && ra.toks[i + 1..].iter().take(6).any(|y| y.up == name)
            });
            let Some((_, f)) = fetch else {
                return Err("the cursor is not fetched".into());
            };
            let line = ra.side.lines.line_col(f.off).0;
            // What the loop does with each fetched row is not traced.
            Ok((
                format!("{both} (the rows are fetched at line {line})."),
                false,
            ))
        }
        ShapeKind::If => {
            let (Obs::Branch(ia), Obs::Branch(ib)) = (&f.a, &f.b) else {
                return Err("unexpected observation".into());
            };
            // Without an explicit ELSE, "no branch" runs nothing.
            let body =
                |x: &Shape, r: &Routine, i: &Option<usize>, nconds: usize| -> Option<SyntaxNode> {
                    match i {
                        Some(i) => x.branches.get(*i).cloned(),
                        None => (x.branches.len() > nconds).then(|| x.branches[nconds].clone()),
                    }
                    .filter(|_| r.d == d)
                };
            let nca = match &xa.probe {
                Probe::Conds(c) => c.len(),
                _ => 0,
            };
            let ncb = match &xb.probe {
                Probe::Conds(c) => c.len(),
                _ => 0,
            };
            let ba = body(xa, ra, ia, nca);
            let bb = body(xb, rb, ib, ncb);
            let text = |n: &Option<SyntaxNode>| {
                n.as_ref()
                    .map(|n| {
                        tokens(n)
                            .iter()
                            .map(|t| t.up.clone())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default()
            };
            if text(&ba) == text(&bb) {
                return Err("both versions run the same code".into());
            }
            let ea = ba.as_ref().map_or(Eff::No, |n| effect(ra, n));
            let eb = bb.as_ref().map_or(Eff::No, |n| effect(rb, n));
            if ea == Eff::No && eb == Eff::No {
                return Err("the branches have no observable effect".into());
            }
            let certain = ea == Eff::Yes || eb == Eff::Yes;
            let line = |r: &Routine, n: &Option<SyntaxNode>| {
                n.as_ref()
                    .map(|n| format!("the branch at line {}", r.side.line_of(n)))
                    .unwrap_or_else(|| "no branch".into())
            };
            Ok((
                format!("A runs {} and B runs {}.", line(ra, &ba), line(rb, &bb)),
                certain,
            ))
        }
        ShapeKind::While => {
            let body = xa.branches.first().ok_or("loop without body")?;
            let e = effect(ra, body);
            if e == Eff::No {
                return Err("the loop body has no observable effect".into());
            }
            Ok((format!("{both}."), e == Eff::Yes))
        }
        ShapeKind::ExitWhen | ShapeKind::ContinueWhen => {
            let e = if xa.kind == ShapeKind::ContinueWhen {
                xa.rest
                    .iter()
                    .map(|n| effect(ra, n))
                    .max()
                    .unwrap_or(Eff::No)
            } else {
                // Leaving the loop early skips the remaining iterations.
                na.ancestors()
                    .find(|p| p.kind() == SyntaxKind::LOOP_STMT)
                    .map_or(Eff::No, |l| effect(ra, &l))
            };
            if e == Eff::No {
                return Err("the loop has no observable effect".into());
            }
            // Whether there is a further iteration or statement is not traced.
            Ok((format!("{both}."), false))
        }
        ShapeKind::Data => {
            let Probe::Stmt(s) = &xa.probe else {
                return Err("unexpected probe".into());
            };
            let _ = nb;
            match s {
                Stmt::Select(q) => {
                    if q.items.iter().any(|i| i.assign.is_some()) || !q.into.is_empty() {
                        let targets: Vec<String> = if q.into.is_empty() {
                            q.items
                                .iter()
                                .filter_map(|i| {
                                    i.assign
                                        .as_ref()
                                        .map(|v| v.trim_start_matches('@').to_lowercase())
                                })
                                .collect()
                        } else {
                            q.into.iter().filter_map(name_key).collect()
                        };
                        let (Obs::Assigned(x), Obs::Assigned(y)) = (&f.a, &f.b) else {
                            return Err("unexpected observation".into());
                        };
                        let mut live = Vec::new();
                        let mut certain = false;
                        for (i, t) in targets.iter().enumerate() {
                            let differs = match (x.get(i), y.get(i)) {
                                (Some(Some(p)), Some(Some(q))) => {
                                    eval::same_nn(p, q, &f.nonnull) == Some(false)
                                }
                                (Some(p), Some(q)) => p.is_some() != q.is_some(),
                                _ => false,
                            };
                            if !differs {
                                continue;
                            }
                            match ra.read_after(t, na) {
                                Read::Never => {}
                                r => {
                                    certain |= matches!(r, Read::Always(_));
                                    live.push(read_note(t, r, d));
                                }
                            }
                        }
                        if live.is_empty() {
                            return Err("the assigned variables are not read afterwards".into());
                        }
                        return Ok((format!("{both} ({}).", live.join("; ")), certain));
                    }
                    if ra.result_sets
                        || d != Dialect::Tsql && ra_is_query_result(na)
                        || ra.sql_set_result && is_last_statement(&ra.side.root, na)
                    {
                        Ok((format!("{both} as its result set."), true))
                    } else {
                        Err("a query whose result is not returned".into())
                    }
                }
                Stmt::Assign(t, _) => {
                    let n = name_key(t).unwrap_or_default();
                    let shown = shown_var(t, d);
                    match ra.read_after(&n, na) {
                        Read::Never => Err(format!("{n} is not read afterwards")),
                        r => Ok((
                            format!(
                                "A sets {shown} to {} and B to {} ({}).",
                                value_of(&f.a, &f.opaque),
                                value_of(&f.b, &f.opaque),
                                read_note(&n, r, d)
                            ),
                            matches!(r, Read::Always(_)),
                        )),
                    }
                }
                Stmt::Declare { name, .. } => {
                    let n = name.trim_start_matches('@').to_lowercase();
                    match ra.read_after(&n, na) {
                        Read::Never => Err(format!("{n} is not read afterwards")),
                        r => Ok((
                            format!(
                                "A initialises {name} to {} and B to {} ({}).",
                                value_of(&f.a, &f.opaque),
                                value_of(&f.b, &f.opaque),
                                read_note(&n, r, d)
                            ),
                            matches!(r, Read::Always(_)),
                        )),
                    }
                }
                Stmt::Return(_) => Ok((
                    format!(
                        "A returns {} and B returns {}.",
                        value_of(&f.a, &f.opaque),
                        value_of(&f.b, &f.opaque)
                    ),
                    true,
                )),
                Stmt::Insert { target, .. }
                | Stmt::Update { target, .. }
                | Stmt::Delete { target, .. } => {
                    let t = eval::table_key(target);
                    if ra.dead_table(&t, end) {
                        return Err(format!("{t} is not read afterwards"));
                    }
                    let verb = match s {
                        Stmt::Insert { .. } => "inserts",
                        Stmt::Update { .. } => "updates",
                        _ => "deletes",
                    };
                    Ok((
                        format!("{}.", write_effect(&f.a, &f.b, verb, &spell(&t), &f.opaque)),
                        true,
                    ))
                }
                Stmt::Merge { .. } => Err("MERGE".into()),
            }
        }
    }
}

/// Whether no statement starts after `n` ends.
fn is_last_statement(root: &SyntaxNode, n: &SyntaxNode) -> bool {
    let end = cst::sig_range(n).1;
    !root
        .descendants()
        .any(|m| m.kind() == SyntaxKind::STATEMENT && cst::sig_range(&m).0 >= end)
}

fn ra_is_query_result(n: &SyntaxNode) -> bool {
    // PL/pgSQL `RETURN QUERY` statements are observable.
    cst::first_words(n, 2).join(" ") == "RETURN QUERY"
}

fn value_of(o: &Obs, opaque: &[String]) -> String {
    match o {
        Obs::Value(v) => v.show(opaque),
        other => describe_obs(other, opaque, ShapeKind::Data, 0),
    }
}

fn write_effect(a: &Obs, b: &Obs, verb: &str, table: &str, opaque: &[String]) -> String {
    match (a, b) {
        (Obs::Writes(x), Obs::Writes(y)) => {
            let rows = |w: &Vec<eval::Write>| match w.len() {
                0 => "no row".to_string(),
                1 => "the row".to_string(),
                n => format!("{n} rows"),
            };
            let sets = |w: &Vec<eval::Write>| {
                w.iter()
                    .map(|r| {
                        let cols: Vec<String> =
                            r.1.iter()
                                .map(|(c, v, _)| format!("{} = {}", spell(c), v.show(opaque)))
                                .collect();
                        cols.join(", ")
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            let deletes = x.iter().chain(y).all(|w| w.1.is_empty());
            if deletes || x.iter().map(|w| w.0).ne(y.iter().map(|w| w.0)) {
                let one = |w: &Vec<eval::Write>| {
                    if deletes || w.is_empty() {
                        rows(w)
                    } else {
                        format!("{} (setting {})", rows(w), sets(w))
                    }
                };
                format!("A {verb} {} of {table} and B {verb} {}", one(x), one(y))
            } else {
                format!("A sets {} and B sets {} in {table}", sets(x), sets(y))
            }
        }
        (Obs::Inserted(cx, x), Obs::Inserted(cy, y)) => {
            let vals = |cols: &Vec<String>, w: &Vec<Vec<V>>| {
                if w.is_empty() {
                    return "no row".to_string();
                }
                w.iter()
                    .map(|r| {
                        if cols.len() == r.len() {
                            let named: Vec<String> = cols
                                .iter()
                                .zip(r)
                                .map(|(c, v)| format!("{} = {}", spell(c), v.show(opaque)))
                                .collect();
                            format!("({})", named.join(", "))
                        } else {
                            format!("({})", show_vals(r, opaque))
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            format!(
                "A {verb} {} into {table} and B {verb} {}",
                vals(cx, x),
                vals(cy, y)
            )
        }
        (Obs::Rows(x, _), Obs::Rows(y, _)) => {
            let vals = |w: &Vec<Vec<V>>| {
                if w.is_empty() {
                    "no row".to_string()
                } else {
                    w.iter()
                        .map(|r| format!("({})", show_vals(r, opaque)))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            };
            format!("A {verb} {} into {table} and B {verb} {}", vals(x), vals(y))
        }
        _ => format!("A and B write {table} differently"),
    }
}

/// How surely running a unit can be observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Eff {
    No,
    /// Only on some paths or for some data (a query that may assign
    /// nothing, an UPDATE that may match no row, a nested branch).
    Maybe,
    Yes,
}

fn read_eff(r: Read) -> Eff {
    match r {
        Read::Always(_) => Eff::Yes,
        Read::Maybe(_) => Eff::Maybe,
        Read::Never => Eff::No,
    }
}

/// Whether running a unit can be observed: it writes a table, returns a
/// result, raises an error, calls something, or sets a variable that is read
/// after it.
fn effect(r: &Routine, n: &SyntaxNode) -> Eff {
    let nodes: Vec<SyntaxNode> = if n.kind() == SyntaxKind::STATEMENT {
        vec![n.clone()]
    } else {
        n.descendants()
            .filter(|x| x.kind() == SyntaxKind::STATEMENT)
            .collect()
    };
    let mut out = Eff::No;
    if n.kind() != SyntaxKind::STATEMENT
        && n.descendants()
            .any(|x| matches!(x.kind(), SyntaxKind::TRY_CATCH))
    {
        out = Eff::Maybe;
    }
    for s in &nodes {
        let mut e = statement_effect(r, s);
        // Statements under a nested condition or loop may not run.
        let straight = s
            .ancestors()
            .skip(1)
            .take_while(|a| a != n)
            .all(|a| matches!(a.kind(), SyntaxKind::BLOCK | SyntaxKind::TRY_BLOCK));
        if !straight {
            e = e.min(Eff::Maybe);
        }
        out = out.max(e);
    }
    out
}

fn statement_effect(r: &Routine, s: &SyntaxNode) -> Eff {
    let k = stmt_kind(s, r.d);
    let t = tokens(s);
    let var_name = |x: &Tok| x.text.trim_start_matches('@').to_lowercase();
    match k {
        StmtKind::Assign => {
            let target = if t.first().is_some_and(|x| x.up == "SET") {
                t.get(1)
            } else {
                t.first()
            };
            target.map_or(Eff::No, |x| read_eff(r.read_after(&var_name(x), s)))
        }
        StmtKind::Select => {
            let assigns =
                t.iter().any(|x| x.up == "INTO") || t.get(1).is_some_and(|x| x.kind == K::Var);
            if !assigns {
                return Eff::Yes;
            }
            let from = t.iter().any(|x| x.up == "FROM");
            let e = t
                .iter()
                .filter(|x| x.kind == K::Var || x.kind == K::Word)
                .map(var_name)
                .filter(|v| r.locals.contains_key(v) || r.params.contains_key(v))
                .map(|v| read_eff(r.read_after(&v, s)))
                .max()
                .unwrap_or(Eff::No);
            if from {
                e.min(Eff::Maybe)
            } else {
                e
            }
        }
        StmtKind::Insert | StmtKind::Update | StmtKind::Delete | StmtKind::Merge => {
            let target = t
                .iter()
                .skip(1)
                .find(|x| !matches!(x.up.as_str(), "INTO" | "FROM" | "TOP" | "ONLY"))
                .map(|x| x.text.to_lowercase())
                .unwrap_or_default();
            let (_, end) = cst::sig_range(s);
            if r.dead_table(&target, end) {
                Eff::No
            } else if k == StmtKind::Insert
                && t.iter().any(|x| x.up == "VALUES")
                && !t.iter().any(|x| x.up == "SELECT")
            {
                Eff::Yes
            } else {
                Eff::Maybe
            }
        }
        StmtKind::Declare | StmtKind::Label | StmtKind::Other => {
            let w = cst::first_words(s, 1);
            let w = w.first().map(String::as_str).unwrap_or("");
            // A jump out of the iteration or the loop skips what the loop does.
            if matches!(w, "CONTINUE" | "EXIT" | "BREAK" | "LEAVE" | "ITERATE") {
                return s
                    .ancestors()
                    .find(|p| p.kind() == SyntaxKind::LOOP_STMT)
                    .map_or(Eff::No, |lp| {
                        let e = lp
                            .descendants()
                            .filter(|x| x.kind() == SyntaxKind::STATEMENT)
                            .filter(|x| {
                                let w = cst::first_words(x, 1);
                                !matches!(
                                    w.first().map(String::as_str),
                                    Some("CONTINUE" | "EXIT" | "BREAK" | "LEAVE" | "ITERATE")
                                )
                            })
                            .map(|x| statement_effect(r, &x))
                            .max()
                            .unwrap_or(Eff::No);
                        e.min(Eff::Maybe)
                    });
            }
            match w {
                "PRINT" | "RAISE" | "THROW" | "RAISERROR" | "RETURN" | "SIGNAL" | "RESIGNAL"
                | "TRUNCATE" | "ALTER" | "CREATE" | "DROP" => Eff::Yes,
                "EXEC" | "EXECUTE" | "PERFORM" | "CALL" | "FETCH" | "OPEN" | "CLOSE"
                | "DEALLOCATE" | "INSERT" | "UPDATE" | "DELETE" | "MERGE" => Eff::Maybe,
                _ => Eff::No,
            }
        }
        _ => {
            if k.is_effectful() {
                Eff::Yes
            } else {
                Eff::No
            }
        }
    }
}

/// How a later read of `name` is described.
fn read_note(name: &str, r: Read, d: Dialect) -> String {
    let name = spell(name);
    let shown = if d == Dialect::Tsql {
        format!("@{name}")
    } else {
        name
    };
    match r {
        Read::Always(0) => format!("{shown} is returned to the caller"),
        Read::Always(l) => format!("{shown} is read at line {l}"),
        Read::Maybe(l) => format!("{shown} may be read from line {l}"),
        Read::Never => format!("{shown} is not read"),
    }
}

fn shown_var(e: &Expr, d: Dialect) -> String {
    match e {
        Expr::Var(v) => v.clone(),
        Expr::Name(p) => p.join("."),
        _ => name_key(e)
            .map(|n| {
                if d == Dialect::Tsql {
                    format!("@{n}")
                } else {
                    n
                }
            })
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_neighbours_keep_the_format() {
        assert_eq!(
            eval::date_neighbour("2013-01-01", -1).as_deref(),
            Some("2012-12-31")
        );
        assert_eq!(
            eval::date_neighbour("99991231 23:59:59.9999999", -1).as_deref(),
            Some("99991230 23:59:59.9999999")
        );
        assert_eq!(eval::date_neighbour("99991231", 1), None);
        assert_eq!(eval::date_neighbour("hello", 1), None);
    }
}
