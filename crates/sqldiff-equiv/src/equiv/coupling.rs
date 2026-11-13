//! Relational variable coupling of two routines (region-comparison design
//! `region-comparison-design.md`, section 3.2, stage 0a).
//!
//! **Report only.** Nothing on the diff path reads the result: it is stored
//! in `Outcome::coupling` and printed by the `coupling` example. It is
//! computed only by `compare_report` (the `coupling` entry point of the
//! engine), so a diff does neither the work nor counts it.
//!
//! # What is computed
//!
//! For each cross-dialect routine pair the analysis computes kappa, the set of
//! variable pairs claimed equal whenever both sides are at an aligned point
//! in the same lockstep state, as a greatest fixpoint over the aligned
//! structured sequence of statements and `IF` / `WHILE` / `TRY` markers. The
//! alignment is `identity.rs`'s item pairing; the candidate pairs are the
//! variable pairs `variable_facts` accepts (names declared twice on one side,
//! output parameters and every variable a call, dynamic SQL or an uncovered
//! construct mentions are not candidates).
//!
//! * Entry: parameters couple when the parameter check of identity passes.
//!   A local couples at its own aligned declaration: with a value, as an
//!   assignment pair; without one, both NULL, only outside every loop (a
//!   T-SQL `DECLARE` without a value is a no-op at run time).
//! * Transfer: an aligned single-target allow-list assignment pair keeps the
//!   pair when the items are proven equal, both values are value terms and
//!   every variable they read is coupled before. Any other write of either
//!   variable (one-sided, unpaired, `SELECT ... FROM`, `FETCH INTO`, a call,
//!   dynamic SQL) drops the pairs with it.
//! * Joins are intersections. An uncoupled `IF` / `WHILE`, a one-sided
//!   region, a loop with a jump and a forward `GOTO` block keep only the
//!   pairs neither side writes in that region.
//! * Loop heads: the greatest fixpoint, started at the entry state and
//!   intersected until stable. In configuration (a) a loop is asynchronous:
//!   its head keeps only pairs the loop does not write.
//! * Handler entry: the intersection of kappa before every statement of the
//!   TRY part that can route there. `kappa_ff` counts only statements with
//!   an error route that needs no injected fault (a statement that is not a
//!   plain value assignment: raise, call, data statement, transaction
//!   statement); `kappa_all` counts every statement.
//!
//! The condition classes are those of section 9 of the design.
//!
//! # Not modelled (conservative)
//!
//! `RETURN e` is not a coupled variable (the return status is not a
//! candidate); table coupling (R13) does not exist here, so every condition
//! that reads a table is `reads-table`; whether `Canon`'s equality covers
//! evaluation errors is not checked here (it is the statement level's).

use std::collections::{BTreeMap, BTreeSet};

use sqldiff_model::work::{self, Site};
use sqldiff_syntax::{ast as cst, Dialect};

use super::ast::Stmt;
use super::canon::Item;
use super::identity::{build, kind_of, structure, syms, Analysis, Blk, Ev, Kind, Side, Var, Write};
use super::lower::{Ctl, CtlKind, Unit};
use super::term::Term;
use crate::artifact::Analyzed;

/// The class of a paired condition.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum CClass {
    Coupled,
    /// A parameter it reads is not coupled at entry (type or default rule).
    ParamType,
    /// Coupled with the loop-head fixpoint but not with loops asynchronous.
    LoopCarried,
    /// Reads a table (a stage-5 class, never coupled here).
    ReadsTable,
    /// `@@...`, `SERVERPROPERTY` and the like.
    ServerFn,
    AfterOutputCall,
    AfterDynamicSql,
    Volatile,
    StatementNotEqual,
    Other(String),
}

impl CClass {
    pub fn label(&self) -> &'static str {
        match self {
            CClass::Coupled => "coupled",
            CClass::ParamType => "param-type",
            CClass::LoopCarried => "loop-carried",
            CClass::ReadsTable => "reads-table",
            CClass::ServerFn => "server-fn",
            CClass::AfterOutputCall => "after-output-call",
            CClass::AfterDynamicSql => "after-dynamic-sql",
            CClass::Volatile => "volatile",
            CClass::StatementNotEqual => "statement-not-equal",
            CClass::Other(_) => "other",
        }
    }

    pub fn why(&self) -> Option<&str> {
        match self {
            CClass::Other(w) => Some(w),
            _ => None,
        }
    }
}

/// One paired `IF` / `WHILE` condition under the four runs: (a) loops
/// asynchronous, (b) loop-head fixpoint, each over `kappa_ff` and
/// `kappa_all`.
#[derive(Debug, Clone)]
pub struct CouplingRow {
    pub kind: &'static str,
    pub a_line: usize,
    pub b_line: usize,
    pub a_start: usize,
    pub b_start: usize,
    pub ff_a: CClass,
    pub ff_b: CClass,
    pub all_a: CClass,
    pub all_b: CClass,
}

/// What one run (configuration and fault variant) found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunSummary {
    /// Variable pairs coupled at the end of the routine.
    pub exit: usize,
    /// Rounds of the loop-head fixpoints, summed over the loops.
    pub rounds: usize,
    /// Transfer steps.
    pub steps: u64,
}

#[derive(Debug, Clone, Default)]
pub struct UnitCoupling {
    /// Candidate variable pairs.
    pub candidates: usize,
    /// Paired conditions, sorted by node.
    pub rows: Vec<CouplingRow>,
    /// Runs: (a) / (b) over `kappa_ff`, then over `kappa_all`.
    pub ff_a: RunSummary,
    pub ff_b: RunSummary,
    pub all_a: RunSummary,
    pub all_b: RunSummary,
    pub refused: Option<String>,
}

/// Why a candidate pair is not in kappa.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Why {
    Param(String),
    Output,
    Dynamic,
    Volatile,
    Table,
    ServerFn,
    NotEqual,
    /// `SELECT @a = .., @b = ..` with more than one target.
    MultiTarget,
    Other(String),
}

/// A kappa: the coupled candidates (side A variable index) and, for the rest,
/// why they are not.
#[derive(Debug, Clone)]
struct K {
    set: BTreeSet<Var>,
    why: BTreeMap<Var, Why>,
}

impl K {
    fn drop_var(&mut self, v: Var, why: Why) {
        self.set.remove(&v);
        // The multi-target rule is the reason that names the statement shape;
        // a later drop of the same variable does not hide it.
        if self.why.get(&v) != Some(&Why::MultiTarget) {
            self.why.insert(v, why);
        }
    }

    fn meet(&self, o: &K) -> K {
        let set: BTreeSet<Var> = self.set.intersection(&o.set).copied().collect();
        let mut why = BTreeMap::new();
        for v in self.why.keys().chain(o.why.keys()) {
            if !set.contains(v) {
                let w = self.why.get(v).or_else(|| o.why.get(v));
                if let Some(w) = w {
                    why.insert(*v, w.clone());
                }
            }
        }
        K { set, why }
    }
}

enum Node<'b> {
    Stmts(Vec<usize>),
    Blk(&'b Blk),
}

fn nodes<'b>(blks: &'b [Blk], side: &Side<'_>) -> Vec<Node<'b>> {
    let mut out: Vec<Node<'b>> = Vec::new();
    for b in blks {
        match b {
            Blk::Ev(e) => {
                let first = side.item_of[*e].map_or(true, |(_, f)| f);
                if !first {
                    if let Some(Node::Stmts(v)) = out.last_mut() {
                        v.push(*e);
                        continue;
                    }
                }
                out.push(Node::Stmts(vec![*e]));
            }
            other => out.push(Node::Blk(other)),
        }
    }
    out
}

fn key(side: &Side<'_>, n: &Node<'_>) -> Option<usize> {
    let e = match n {
        Node::Stmts(v) => *v.first()?,
        Node::Blk(Blk::Ev(e)) => *e,
        Node::Blk(Blk::If { cond, .. }) | Node::Blk(Blk::While { cond, .. }) => *cond,
        Node::Blk(Blk::Try { at, .. }) | Node::Blk(Blk::Block { at, .. }) => *at,
    };
    side.item_of[e].map(|(k, _)| k)
}

fn has_jump(side: &Side<'_>, blks: &[Blk]) -> bool {
    blks.iter().any(|b| match b {
        Blk::Ev(e) => matches!(
            &side.ev[*e],
            Ev::Ctl(Ctl {
                kind: CtlKind::Break | CtlKind::Continue | CtlKind::Exit(_) | CtlKind::Rethrow,
                ..
            })
        ),
        Blk::If { then, els, .. } => {
            has_jump(side, then) || els.as_deref().is_some_and(|e| has_jump(side, e))
        }
        Blk::While { body, .. } | Blk::Block { body, .. } => has_jump(side, body),
        Blk::Try { body, catch, .. } => has_jump(side, body) || has_jump(side, catch),
    })
}

fn has_events(blks: &[Blk]) -> bool {
    !blks.is_empty()
}

/// `fn` names that read the server or session rather than the inputs.
const SERVER_FNS: &[&str] = &[
    "serverproperty",
    "db_name",
    "db_id",
    "host_name",
    "suser_sname",
    "suser_name",
    "user_name",
    "app_name",
    "object_id",
    "object_name",
    "schema_name",
    "schema_id",
    "original_login",
    "current_user",
    "session_user",
    "version",
    "current_database",
    "current_setting",
    "pg_backend_pid",
];

/// `fn` names that draw a random value or read a sequence.
const RANDOM_FNS: &[&str] = &[
    "newid",
    "newsequentialid",
    "gen_random_uuid",
    "rand",
    "random",
    "nextval",
    "currval",
    "uuid_generate_v4",
];

fn volatile_kind(t: &Term) -> Option<Why> {
    let mut out: Option<Why> = None;
    fn go(t: &Term, out: &mut Option<Why>) {
        let Term::N(h, kids) = t else { return };
        let hit = match h.as_str() {
            "dfn" => Some(Why::ServerFn),
            "fresh" | "time" => Some(Why::Volatile),
            "fn" => {
                let n = kids.first().and_then(|k| k.atom()).unwrap_or("?");
                if SERVER_FNS.contains(&n) {
                    Some(Why::ServerFn)
                } else if RANDOM_FNS.contains(&n) {
                    Some(Why::Volatile)
                } else {
                    None
                }
            }
            _ => None,
        };
        if out.is_none() {
            *out = hit;
        }
        for k in kids {
            go(k, out);
        }
    }
    go(t, &mut out);
    out
}

/// The reason text of a multi-target assignment.
pub const MULTI_TARGET: &str =
    "a SELECT with more than one assignment target is not on the allow-list (multi-target rule)";

fn why_class(w: &Why) -> CClass {
    match w {
        Why::Param(_) => CClass::ParamType,
        Why::Output => CClass::AfterOutputCall,
        Why::Dynamic => CClass::AfterDynamicSql,
        Why::Volatile => CClass::Volatile,
        Why::Table => CClass::ReadsTable,
        Why::ServerFn => CClass::ServerFn,
        Why::NotEqual => CClass::StatementNotEqual,
        Why::MultiTarget => CClass::Other(MULTI_TARGET.into()),
        Why::Other(s) => CClass::Other(s.clone()),
    }
}

struct Ctx<'x, 'a> {
    an: &'x mut Analysis<'a>,
    /// Configuration (b): loop-head fixpoint.
    fix: bool,
    /// Variant kappa_all.
    all: bool,
    rec: bool,
    rows: BTreeMap<(usize, usize), CClass>,
    rounds: usize,
    steps: u64,
    taps: Vec<Option<K>>,
    loops: usize,
    rev: BTreeMap<usize, usize>,
    cand: BTreeSet<Var>,
}

impl Ctx<'_, '_> {
    fn step(&mut self) {
        self.steps += 1;
        work::spend(Site::Coupling, 1);
    }

    /// The side A index of a variable of side `b?`.
    fn a_index(&self, b: bool, v: Var) -> Option<Var> {
        if b {
            self.an.a.var_of_sym(&self.an.b.names[v])
        } else {
            Some(v)
        }
    }

    fn side(&self, b: bool) -> &Side<'_> {
        if b {
            &self.an.b
        } else {
            &self.an.a
        }
    }

    /// Why a write that is not an allow-list pair drops its variable.
    fn kill_why(&self, b: bool, e: usize) -> Why {
        let s = self.side(b);
        match &s.ev[e] {
            Ev::Stmt(st)
                if matches!(&st.ast, Ok(Stmt::Select(q))
                    if q.from.is_empty() && q.items.iter().filter(|i| i.assign.is_some()).count() > 1) =>
            {
                Why::MultiTarget
            }
            Ev::Ctl(_) => Why::Output,
            Ev::Stmt(st) => match &st.ast {
                Ok(Stmt::Select(q)) if !q.from.is_empty() => Why::Table,
                Ok(Stmt::Update { .. } | Stmt::Insert { .. } | Stmt::Delete { .. }) => Why::Table,
                _ => Why::Other("written by a statement the analysis does not couple".into()),
            },
        }
    }

    fn apply_writes(&self, b: bool, evs: &[usize], k: &mut K, why: Option<Why>) {
        for &e in evs {
            for w in self.side(b).writes(e, false) {
                let (Write::Plain(v) | Write::Implicit(v) | Write::Kill(v)) = w;
                if let Some(av) = self.a_index(b, v) {
                    if self.cand.contains(&av) {
                        let y = match self.kill_why(b, e) {
                            Why::MultiTarget => Why::MultiTarget,
                            k => why.clone().unwrap_or(k),
                        };
                        k.drop_var(av, y);
                    }
                }
            }
        }
    }

    fn written(&self, b: bool, ns: &[Node<'_>]) -> BTreeSet<Var> {
        let mut set = BTreeSet::new();
        let s = self.side(b);
        for n in ns {
            match n {
                Node::Stmts(v) => {
                    for &e in v {
                        for w in s.writes(e, false) {
                            let (Write::Plain(x) | Write::Implicit(x) | Write::Kill(x)) = w;
                            set.insert(x);
                        }
                    }
                }
                Node::Blk(blk) => s.assigned_in(std::slice::from_ref(*blk), false, &mut set),
            }
        }
        set.into_iter().filter_map(|v| self.a_index(b, v)).collect()
    }

    fn region(&self, k: &K, vars: &BTreeSet<Var>, why: &str) -> K {
        let mut o = k.clone();
        for v in vars {
            if self.cand.contains(v) {
                o.drop_var(*v, Why::Other(why.to_string()));
            }
        }
        o
    }

    /// Whether a statement may route to a handler: every statement in
    /// `kappa_all`; in `kappa_ff` those with an error route that needs no
    /// injected fault (everything but a plain value assignment).
    fn qualifies(&self, b: bool, evs: &[usize]) -> bool {
        if self.all {
            return true;
        }
        let s = self.side(b);
        evs.iter().any(|&e| match &s.ev[e] {
            Ev::Ctl(m) => matches!(m.kind, CtlKind::Call | CtlKind::Rethrow),
            Ev::Stmt(st) => {
                let plain = st.msg.is_none()
                    && st.cursor.is_none()
                    && st.ast.as_ref().is_ok_and(|x| x.assignment().is_some());
                if !plain {
                    return true;
                }
                match s.item_of[e] {
                    Some((k, true)) => s.items[k]
                        .term
                        .kid(1)
                        .map_or(true, |v| kind_of(v) != Kind::Value),
                    _ => true,
                }
            }
        })
    }

    fn tap(&mut self, k: &K, q: bool) {
        if !q {
            return;
        }
        for acc in &mut self.taps {
            *acc = Some(match acc.take() {
                None => k.clone(),
                Some(a) => a.meet(k),
            });
        }
    }

    fn evs_of(blk: &Blk, out: &mut Vec<usize>) {
        match blk {
            Blk::Ev(e) => out.push(*e),
            Blk::If {
                cond, then, els, ..
            } => {
                out.push(*cond);
                then.iter().for_each(|b| Self::evs_of(b, out));
                els.iter().flatten().for_each(|b| Self::evs_of(b, out));
            }
            Blk::While { cond, body } => {
                out.push(*cond);
                body.iter().for_each(|b| Self::evs_of(b, out));
            }
            Blk::Try { at, body, catch } => {
                out.push(*at);
                body.iter().for_each(|b| Self::evs_of(b, out));
                catch.iter().for_each(|b| Self::evs_of(b, out));
            }
            Blk::Block { at, body } => {
                out.push(*at);
                body.iter().for_each(|b| Self::evs_of(b, out));
            }
        }
    }

    /// One node of one side with no partner: its writes drop their pairs.
    fn one_sided(&mut self, b: bool, n: &Node<'_>, k: &mut K, lock: bool) {
        let mut evs = Vec::new();
        match n {
            Node::Stmts(v) => evs.extend(v.iter().copied()),
            Node::Blk(blk) => Self::evs_of(blk, &mut evs),
        }
        let q = self.qualifies(b, &evs);
        if !lock {
            self.tap(&k.clone(), q);
            return;
        }
        let mut kk = k.clone();
        match n {
            Node::Stmts(v) => self.apply_writes(
                b,
                v,
                &mut kk,
                Some(Why::Other(
                    "written by a statement with no partner on the other side".into(),
                )),
            ),
            Node::Blk(_) => {
                let w = self.written(b, std::slice::from_ref(n));
                kk = self.region(k, &w, "written by a region with no partner");
            }
        }
        self.tap(&kk, q);
        *k = kk;
    }

    fn seq(&mut self, ba: &[Blk], bb: &[Blk], k: K, lock: bool) -> K {
        let (na, nb) = (nodes(ba, &self.an.a), nodes(bb, &self.an.b));
        let ka: Vec<Option<usize>> = na.iter().map(|n| key(&self.an.a, n)).collect();
        let kb: Vec<Option<usize>> = nb.iter().map(|n| key(&self.an.b, n)).collect();
        let mut k = k;
        let (mut i, mut j) = (0, 0);
        while i < na.len() || j < nb.len() {
            self.step();
            let pa = ka
                .get(i)
                .copied()
                .flatten()
                .and_then(|x| self.an.pairs.get(&x).copied());
            let pb = kb
                .get(j)
                .copied()
                .flatten()
                .and_then(|y| self.rev.get(&y).copied());
            if i < na.len() && j < nb.len() && pa.is_some() && pa == kb[j] {
                k = self.aligned(&na[i], &nb[j], ka[i], kb[j], k, lock);
                i += 1;
                j += 1;
                continue;
            }
            let a_has = i < na.len();
            let b_has = j < nb.len();
            let a_partner_later = pa.is_some_and(|p| kb.iter().skip(j).any(|x| *x == Some(p)));
            let b_partner_later = pb.is_some_and(|p| ka.iter().skip(i).any(|x| *x == Some(p)));
            if a_has && (!b_has || !a_partner_later) {
                self.one_sided(false, &na[i], &mut k, lock);
                i += 1;
            } else if b_has && (!a_has || !b_partner_later) {
                self.one_sided(true, &nb[j], &mut k, lock);
                j += 1;
            } else {
                // Both have partners, but elsewhere: a crossing. Side A's
                // element is skipped, so its pair is never aligned.
                self.one_sided(false, &na[i], &mut k, lock);
                i += 1;
            }
        }
        k
    }

    fn aligned(
        &mut self,
        x: &Node<'_>,
        y: &Node<'_>,
        ia: Option<usize>,
        ib: Option<usize>,
        k: K,
        lock: bool,
    ) -> K {
        let (Some(ia), Some(ib)) = (ia, ib) else {
            return k;
        };
        match (x, y) {
            (Node::Stmts(ga), Node::Stmts(gb)) => self.stmt_pair(ga, gb, ia, ib, k, lock),
            (Node::Blk(Blk::Ev(ea)), Node::Blk(Blk::Ev(eb))) => {
                self.stmt_pair(&[*ea], &[*eb], ia, ib, k, lock)
            }
            (Node::Blk(a), Node::Blk(b)) => self.blk_pair(a, b, ia, ib, k, lock),
            _ => {
                let mut k = k;
                self.one_sided(false, x, &mut k, lock);
                self.one_sided(true, y, &mut k, lock);
                k
            }
        }
    }

    fn is_decl_without_value(s: &Side<'_>, e: usize) -> bool {
        matches!(&s.ev[e], Ev::Stmt(st) if matches!(&st.ast, Ok(Stmt::Declare { default: None, .. })))
    }

    fn stmt_pair(
        &mut self,
        ga: &[usize],
        gb: &[usize],
        ia: usize,
        ib: usize,
        k: K,
        lock: bool,
    ) -> K {
        let q = self.qualifies(false, ga) || self.qualifies(true, gb);
        self.tap(&k, q);
        if !lock {
            return k;
        }
        let mut k = k;
        if let ([ea], [eb]) = (ga, gb) {
            let (wa, wb) = (self.an.a.writes(*ea, false), self.an.b.writes(*eb, false));
            if let ([Write::Plain(va)], [Write::Plain(vb)]) = (&wa[..], &wb[..]) {
                if self.an.a.names[*va] == self.an.b.names[*vb] && self.cand.contains(va) {
                    match self.assign_ok(ia, ib, &k) {
                        Ok(()) => {
                            k.set.insert(*va);
                            k.why.remove(va);
                        }
                        Err(w) => k.drop_var(*va, w),
                    }
                    return k;
                }
            }
            // A declaration without a value: both NULL, but only at a point
            // that runs once (a T-SQL DECLARE does not reset in a loop).
            if Self::is_decl_without_value(&self.an.a, *ea)
                && Self::is_decl_without_value(&self.an.b, *eb)
            {
                let one = |ws: &[Write]| match ws {
                    [Write::Implicit(v) | Write::Kill(v)] => Some(*v),
                    _ => None,
                };
                if let (Some(va), Some(vb)) = (one(&wa), one(&wb)) {
                    if self.an.a.names[va] == self.an.b.names[vb] && self.cand.contains(&va) {
                        if self.loops == 0 {
                            k.set.insert(va);
                            k.why.remove(&va);
                        } else {
                            k.drop_var(va, Why::Other("declared inside a loop".into()));
                        }
                        return k;
                    }
                }
            }
        }
        self.apply_writes(false, ga, &mut k, None);
        self.apply_writes(true, gb, &mut k, None);
        k
    }

    /// An aligned assignment pair of allow-list form keeps its pair.
    fn assign_ok(&mut self, ia: usize, ib: usize, k: &K) -> Result<(), Why> {
        self.an.pair_equal(ia, ib).map_err(|_| Why::NotEqual)?;
        let (ta, tb) = (&self.an.a.items[ia].term, &self.an.b.items[ib].term);
        let (Some(va), Some(vb)) = (ta.kid(1), tb.kid(1)) else {
            return Err(Why::Other("an assignment has no value".into()));
        };
        for t in [va, vb] {
            match kind_of(t) {
                Kind::Value => {}
                Kind::State => return Err(Why::Table),
                Kind::Rows => return Err(Why::Other("assigned a row test".into())),
                Kind::No(w) => return Err(volatile_kind(t).unwrap_or(Why::Other(w))),
            }
        }
        let mut reads = BTreeSet::new();
        syms(va, &mut reads);
        syms(vb, &mut reads);
        for r in reads {
            match self.an.a.var_of_sym(&r) {
                Some(v) if k.set.contains(&v) => {}
                Some(v) => {
                    return Err(k
                        .why
                        .get(&v)
                        .cloned()
                        .unwrap_or_else(|| Why::Other(format!("{r} is not coupled"))));
                }
                None => return Err(Why::Other(format!("{r} is not a variable"))),
            }
        }
        Ok(())
    }

    /// Whether the condition pair is coupled at `k`, and its class.
    fn cond_class(&mut self, k: &K, ia: usize, ib: usize) -> CClass {
        let (x, y) = (&self.an.a.items[ia], &self.an.b.items[ib]);
        if x.term.head() != y.term.head() || !matches!(x.term.head(), "ctl-if" | "ctl-while") {
            return CClass::Other("not an IF / WHILE test (row or cursor test)".into());
        }
        if self.an.pair_equal(ia, ib).is_err() {
            return CClass::StatementNotEqual;
        }
        let (x, y) = (&self.an.a.items[ia], &self.an.b.items[ib]);
        let (tx, ty) = (
            x.term.kid(0).unwrap_or(&x.term),
            y.term.kid(0).unwrap_or(&y.term),
        );
        for t in [tx, ty] {
            match kind_of(t) {
                Kind::Value => {}
                Kind::State => return CClass::ReadsTable,
                Kind::Rows => return CClass::Other("a row test".into()),
                Kind::No(w) => {
                    return volatile_kind(t).map_or(CClass::Other(w), |w| why_class(&w));
                }
            }
        }
        let mut reads = BTreeSet::new();
        syms(tx, &mut reads);
        syms(ty, &mut reads);
        let mut vars: BTreeMap<Var, String> = BTreeMap::new();
        for r in reads {
            match self.an.a.var_of_sym(&r) {
                Some(v) => {
                    vars.insert(v, r);
                }
                None => return CClass::Other(format!("{r} is not a variable")),
            }
        }
        for (v, r) in vars {
            if !k.set.contains(&v) {
                return k
                    .why
                    .get(&v)
                    .map_or_else(|| CClass::Other(format!("{r} is not coupled")), why_class);
            }
        }
        CClass::Coupled
    }

    fn record(&mut self, ia: usize, ib: usize, c: &CClass) {
        if self.rec {
            self.rows.insert((ia, ib), c.clone());
        }
    }

    fn blk_pair(&mut self, a: &Blk, b: &Blk, ia: usize, ib: usize, k: K, lock: bool) -> K {
        match (a, b) {
            (
                Blk::If {
                    then: ta, els: ea, ..
                },
                Blk::If {
                    then: tb, els: eb, ..
                },
            ) => {
                let class = self.cond_class(&k, ia, ib);
                self.record(ia, ib, &class);
                self.tap(&k, self.all);
                let (ea, eb) = (ea.as_deref().unwrap_or(&[]), eb.as_deref().unwrap_or(&[]));
                if lock && class == CClass::Coupled {
                    let kt = self.seq(ta, tb, k.clone(), true);
                    let ke = self.seq(ea, eb, k, true);
                    return kt.meet(&ke);
                }
                let mut w = BTreeSet::new();
                for (side_b, blks) in [(false, [ta.as_slice(), ea]), (true, [tb.as_slice(), eb])] {
                    for bl in blks {
                        let ns = nodes(bl, self.side(side_b));
                        w.extend(self.written(side_b, &ns));
                    }
                }
                let kr = if lock {
                    self.region(&k, &w, "written in an uncoupled branch")
                } else {
                    k
                };
                self.seq(ta, tb, kr.clone(), false);
                self.seq(ea, eb, kr.clone(), false);
                kr
            }
            (Blk::While { body: ba, .. }, Blk::While { body: bb, .. }) => {
                self.while_pair(ba, bb, ia, ib, k, lock)
            }
            (
                Blk::Try {
                    body: ba,
                    catch: ca,
                    ..
                },
                Blk::Try {
                    body: bb,
                    catch: cb,
                    ..
                },
            ) => {
                let entry = k.clone();
                self.taps.push(None);
                let kbody = self.seq(ba, bb, k, lock);
                let acc = self.taps.pop().flatten();
                let kc = acc.unwrap_or(entry);
                let kcatch = self.seq(ca, cb, kc, lock);
                kbody.meet(&kcatch)
            }
            (Blk::Block { body: ba, .. }, Blk::Block { body: bb, .. }) => {
                let mut w = BTreeSet::new();
                for (side_b, bl) in [(false, ba), (true, bb)] {
                    let ns = nodes(
                        std::slice::from_ref(if side_b { b } else { a }),
                        self.side(side_b),
                    );
                    let _ = bl;
                    w.extend(self.written(side_b, &ns));
                }
                let inner = self.seq(ba, bb, k.clone(), lock);
                let _ = inner;
                if lock {
                    self.region(&k, &w, "written in a block a jump leaves")
                } else {
                    k
                }
            }
            _ => {
                let mut k = k;
                for (side_b, blk) in [(false, a), (true, b)] {
                    let n = Node::Blk(blk);
                    self.one_sided(side_b, &n, &mut k, lock);
                }
                k
            }
        }
    }

    fn while_pair(&mut self, ba: &[Blk], bb: &[Blk], ia: usize, ib: usize, k: K, lock: bool) -> K {
        let mut w = BTreeSet::new();
        for (side_b, bl) in [(false, ba), (true, bb)] {
            let ns = nodes(bl, self.side(side_b));
            w.extend(self.written(side_b, &ns));
        }
        let jumps = has_jump(&self.an.a, ba) || has_jump(&self.an.b, bb);
        let _ = has_events(ba);
        let restricted = if lock {
            self.region(&k, &w, "written in a loop")
        } else {
            k.clone()
        };
        if !lock || !self.fix || jumps {
            // Loops asynchronous (a), or a loop with a jump: the head keeps
            // the pairs the loop does not write.
            let class = self.cond_class(&restricted, ia, ib);
            self.record(ia, ib, &class);
            self.tap(&restricted, self.all);
            self.loops += 1;
            self.seq(ba, bb, restricted.clone(), false);
            self.loops -= 1;
            return restricted;
        }
        // Configuration (b): the greatest fixpoint from the entry state.
        let rec = std::mem::replace(&mut self.rec, false);
        let mut head = k.clone();
        loop {
            self.rounds += 1;
            self.step();
            let class = self.cond_class(&head, ia, ib);
            self.loops += 1;
            let next = if class == CClass::Coupled {
                let end = self.seq(ba, bb, head.clone(), true);
                k.meet(&end).meet(&head)
            } else {
                head.meet(&restricted)
            };
            self.loops -= 1;
            if next.set == head.set {
                head = next;
                break;
            }
            head = next;
        }
        self.rec = rec;
        let class = self.cond_class(&head, ia, ib);
        self.record(ia, ib, &class);
        self.tap(&head, self.all);
        self.loops += 1;
        if class == CClass::Coupled {
            self.seq(ba, bb, head.clone(), true);
        } else {
            self.seq(ba, bb, head.clone(), false);
        }
        self.loops -= 1;
        head
    }
}

/// Why a candidate is not coupled at entry.
fn entry(an: &Analysis<'_>, ua: &Unit, ub: &Unit) -> (K, BTreeSet<Var>) {
    let n = an.a.names.len();
    let mut cand = BTreeSet::new();
    let mut k = K {
        set: BTreeSet::new(),
        why: BTreeMap::new(),
    };
    let dup = |u: &Unit| -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut dups = BTreeSet::new();
        for name in u.params.iter().map(|p| p.name.clone()).chain(
            u.locals
                .iter()
                .map(|l| l.name.trim_start_matches('@').to_lowercase()),
        ) {
            if !seen.insert(name.clone()) {
                dups.insert(name);
            }
        }
        dups
    };
    let (da, db) = (dup(ua), dup(ub));
    let taint = |s: &Side<'_>, u: &Unit| {
        let mut call = BTreeSet::new();
        let mut dynamic = BTreeSet::new();
        let mut other = BTreeSet::new();
        for (node, _) in &u.calls {
            call.extend(s.mentioned(&super::ast::tokens(node)));
        }
        for (node, _) in &u.dynamic {
            dynamic.extend(s.mentioned(&super::ast::tokens(node)));
        }
        for (node, _) in &u.uncovered {
            other.extend(s.mentioned(&super::ast::tokens(node)));
        }
        (call, dynamic, other)
    };
    let (ca, dya, oa) = taint(&an.a, ua);
    let (cb, dyb, ob) = taint(&an.b, ub);
    for v in 0..n {
        let name = &an.a.names[v];
        let vb = an.b.var_of_sym(name);
        let plain = |s: &Side<'_>, x: Var| -> String {
            if x < s.unit.params.len() {
                s.unit.params[x].name.clone()
            } else {
                s.unit.locals[x - s.unit.params.len()]
                    .name
                    .trim_start_matches('@')
                    .to_lowercase()
            }
        };
        let why: Option<Why> = (|| {
            let Some(vb) = vb else {
                return Some(Why::Other("no partner on side B".into()));
            };
            if da.contains(&plain(&an.a, v)) || db.contains(&plain(&an.b, vb)) {
                return Some(Why::Other("the name is declared twice on one side".into()));
            }
            if dya.contains(&v) || dyb.contains(&vb) {
                return Some(Why::Dynamic);
            }
            if ca.contains(&v) || cb.contains(&vb) {
                return Some(Why::Output);
            }
            if oa.contains(&v) || ob.contains(&vb) {
                return Some(Why::Other(
                    "named by a construct outside the comparison".into(),
                ));
            }
            match an.var_ok.get(v) {
                Some(Ok(_)) => None,
                Some(Err(e)) => Some(if v < an.a.unit.params.len() {
                    if e.contains("output parameter") {
                        Why::Other(e.clone())
                    } else {
                        Why::Param(e.clone())
                    }
                } else {
                    Why::Other(e.clone())
                }),
                None => Some(Why::Other("not paired".into())),
            }
        })();
        match why {
            Some(w) => {
                k.why.insert(v, w);
            }
            None => {
                cand.insert(v);
                if v < an.a.unit.params.len() {
                    k.set.insert(v);
                } else {
                    k.why
                        .insert(v, Why::Other("not declared at this point".into()));
                }
            }
        }
    }
    (k, cand)
}

fn run(
    an: &mut Analysis<'_>,
    blocks: (&[Blk], &[Blk]),
    k0: &K,
    cand: &BTreeSet<Var>,
    fix: bool,
    all: bool,
    pairs: &[(usize, usize)],
) -> (BTreeMap<(usize, usize), CClass>, RunSummary) {
    let rev = pairs.iter().map(|&(i, j)| (j, i)).collect();
    let mut cx = Ctx {
        an,
        fix,
        all,
        rec: true,
        rows: BTreeMap::new(),
        rounds: 0,
        steps: 0,
        taps: Vec::new(),
        loops: 0,
        rev,
        cand: cand.clone(),
    };
    let out = cx.seq(blocks.0, blocks.1, k0.clone(), true);
    let summary = RunSummary {
        exit: out.set.len(),
        rounds: cx.rounds,
        steps: cx.steps,
    };
    (cx.rows, summary)
}

/// The coupling report of one routine pair. `pairs` are the item pairs
/// `pair_items` chose.
pub(super) fn analyze(
    a: &Analyzed,
    b: &Analyzed,
    ua: &Unit,
    ub: &Unit,
    ia: &[Item],
    ib: &[Item],
    pairs: &[(usize, usize)],
) -> UnitCoupling {
    let mut rep = UnitCoupling::default();
    if ua.dialect == ub.dialect {
        rep.refused = Some("same dialect".into());
        return rep;
    }
    let mut an = match build(ua, ub, ia, ib, pairs) {
        Ok(x) => x,
        Err(e) => {
            rep.refused = Some(e);
            return rep;
        }
    };
    let (Some(blocks_a), Some(blocks_b)) = (structure(&an.a.ev), structure(&an.b.ev)) else {
        rep.refused = Some("IF / loop markers do not nest".into());
        return rep;
    };
    let (k0, cand) = entry(&an, ua, ub);
    rep.candidates = cand.len();
    let blocks = (blocks_a.as_slice(), blocks_b.as_slice());
    let (r_ff_a, s_ff_a) = run(&mut an, blocks, &k0, &cand, false, false, pairs);
    let (r_ff_b, s_ff_b) = run(&mut an, blocks, &k0, &cand, true, false, pairs);
    let (r_all_a, s_all_a) = run(&mut an, blocks, &k0, &cand, false, true, pairs);
    let (r_all_b, s_all_b) = run(&mut an, blocks, &k0, &cand, true, true, pairs);
    rep.ff_a = s_ff_a;
    rep.ff_b = s_ff_b;
    rep.all_a = s_all_a;
    rep.all_b = s_all_b;
    let (first_a, first_b) = (first_events(&an.a.item_of), first_events(&an.b.item_of));
    for &(i, j) in pairs {
        let (x, y) = (&ia[i], &ib[j]);
        let kind = match x.term.head() {
            "ctl-if" => "if",
            "ctl-while" => "while",
            _ => continue,
        };
        if y.term.head() != x.term.head() {
            continue;
        }
        let (Some(&ea), Some(&eb)) = (first_a.get(&i), first_b.get(&j)) else {
            continue;
        };
        let (Ev::Ctl(ma), Ev::Ctl(mb)) = (&an.a.ev[ea], &an.b.ev[eb]) else {
            continue;
        };
        let miss = || CClass::Other("not reached by the aligned walk".into());
        let get =
            |r: &BTreeMap<(usize, usize), CClass>| r.get(&(i, j)).cloned().unwrap_or_else(miss);
        // A loop test that couples only with the fixpoint is loop-carried
        // under loops asynchronous.
        let carried = |a: CClass, b: &CClass| {
            if a != CClass::Coupled && *b == CClass::Coupled {
                CClass::LoopCarried
            } else {
                a
            }
        };
        let (ff_b, all_b) = (get(&r_ff_b), get(&r_all_b));
        rep.rows.push(CouplingRow {
            kind,
            a_line: a.line_of(&ma.node),
            b_line: b.line_of(&mb.node),
            a_start: cst::sig_range(&ma.node).0,
            b_start: cst::sig_range(&mb.node).0,
            ff_a: carried(get(&r_ff_a), &ff_b),
            all_a: carried(get(&r_all_a), &all_b),
            ff_b,
            all_b,
        });
    }
    rep.rows.sort_by_key(|r| (r.a_start, r.b_start));
    let _ = Dialect::Tsql;
    rep
}

fn first_events(item_of: &[Option<(usize, bool)>]) -> BTreeMap<usize, usize> {
    let mut m = BTreeMap::new();
    for (e, x) in item_of.iter().enumerate() {
        if let Some((k, true)) = *x {
            m.entry(k).or_insert(e);
        }
    }
    m
}
