//! Lowering of a parsed artifact into a control-flow graph with error edges
//! and effect annotations (design §3 Layer 3, items 2 and 3).
//!
//! One node per statement. Every statement that can fail has, per abstract
//! error kind, a route: the handler that catches it (if any), the scopes the
//! error leaves on its way there, and the statement that follows (where a
//! CONTINUE handler or a statement-only error resumes). What the error does
//! to the transaction is not fixed here: it depends on run-time state
//! (XACT_ABORT, commit level, open depth), so the interpreter decides it from
//! the profile tables. Commit statements carry error edges too, because
//! deferred constraints fail at commit time.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use sqldiff_syntax::{ast, Dialect, SyntaxKind, SyntaxNode, SyntaxToken};

use super::profile::{ErrorKind, Profile};
use crate::artifact::Analyzed;
use crate::classify::{self, StmtKind};

pub type NodeId = usize;
pub type ScopeId = usize;

/// Falls off the end of the routine, or `RETURN`.
pub const EXIT_OK: NodeId = 0;
/// Leaves the routine with an unhandled error.
pub const EXIT_ERR: NodeId = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutineKind {
    Procedure,
    Function,
    Trigger,
    Anonymous,
    Script,
}

impl RoutineKind {
    /// Key into the profile's `routine_mode` table.
    pub fn key(self) -> &'static str {
        match self {
            RoutineKind::Procedure => "procedure",
            RoutineKind::Function => "function",
            RoutineKind::Trigger => "trigger",
            RoutineKind::Anonymous => "anonymous",
            RoutineKind::Script => "script",
        }
    }
}

/// The transaction or control action of a statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Begin,
    Commit,
    Rollback,
    Savepoint(String),
    RollbackTo(String),
    Release(String),
    XactAbort(bool),
    ImplicitTransactions(bool),
    /// `RETURN [code]`; the code is kept as written.
    Return(Option<String>),
    /// THROW n / RAISERROR / SIGNAL / RAISE EXCEPTION. `form` selects
    /// form-specific profile rows (`throw`, `raiserror`, `signal`, `raise`).
    Raise {
        kind: ErrorKind,
        form: &'static str,
    },
    /// `THROW;`, `RESIGNAL;`, `RAISE;`: re-raise the error being handled.
    Reraise {
        form: &'static str,
    },
    Call(String),
    /// Something the model cannot follow (GOTO, unparsed text).
    Opaque(String),
}

/// A write the statement performs. Durable writes have `temp: None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effect {
    /// Stable key used to align effects across versions and dialects:
    /// `Kind:TARGET#n` (n-th such write in document order).
    pub key: String,
    pub kind: StmtKind,
    pub target: String,
    /// Key of the temporary object written, for temporary state.
    pub temp: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub action: Action,
    pub effects: Vec<Effect>,
    /// Temporary objects the statement reads.
    pub temp_reads: Vec<String>,
    /// Error kinds the statement can raise (profile `raises` table).
    pub raises: Vec<ErrorKind>,
    pub category: Option<&'static str>,
    /// The statement can change several rows, so a failure part-way through
    /// can leave some of them changed where statements are not atomic.
    pub multi_row: bool,
    /// Db2 for i `MERGE ... NOT ATOMIC`: keeps the rows before the error.
    pub not_atomic: bool,
}

impl Stmt {
    pub fn durable_effect(&self) -> Option<&Effect> {
        self.effects.iter().find(|e| e.temp.is_none())
    }
}

/// Conditions the interpreter can evaluate from its own state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Test {
    /// `@@ERROR <> 0` (true) or `@@ERROR = 0` (false).
    ErrorStatus(bool),
    /// The previous statement touched no rows: `@@ROWCOUNT = 0`, `NOT FOUND`,
    /// `SQLCODE = 100` (true) or the opposite (false).
    NoRows(bool),
    /// `@@TRANCOUNT > 0` (true) or `= 0` (false).
    TranCount(bool),
    XactStateNonZero(bool),
    XactStateDoomed,
    XactStateCommittable,
    /// Data-dependent; both branches are explored. The text identifies the
    /// condition so paths of two versions can be matched.
    Unknown(String),
}

#[derive(Debug, Clone)]
pub enum Op {
    Entry,
    ExitOk,
    ExitErr,
    Stmt(Stmt),
    Test {
        test: Test,
        is_loop: bool,
    },
    Enter(ScopeId),
    Leave(ScopeId),
    /// End of a handler body. `resume`: continue after the statement that
    /// raised the condition (CONTINUE handler); otherwise follow `Next`.
    HandlerDone {
        resume: bool,
    },
    Nop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Next,
    True,
    False,
    Error(ErrorKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerRef {
    /// `try_catch`, `exception_block`, `continue_handler`, `exit_handler`,
    /// `undo_handler`: the construct column of the profile's `caught` rows.
    pub construct: &'static str,
    pub entry: NodeId,
    /// The scope that declares the handler.
    pub scope: ScopeId,
}

/// Where an error of one kind goes from one statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub handler: Option<HandlerRef>,
    /// Scopes left before the handler runs, innermost first (not including
    /// the handler's own scope).
    pub unwind: Vec<ScopeId>,
    /// The statement after the failing one.
    pub resume: NodeId,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub op: Op,
    pub syntax: Option<SyntaxNode>,
    pub succ: Vec<(Edge, NodeId)>,
    pub routes: Vec<(ErrorKind, Route)>,
}

impl Node {
    pub fn next(&self, e: Edge) -> Option<NodeId> {
        self.succ.iter().find(|(x, _)| *x == e).map(|(_, n)| *n)
    }

    pub fn route(&self, k: ErrorKind) -> Option<&Route> {
        self.routes.iter().find(|(x, _)| *x == k).map(|(_, r)| r)
    }

    pub fn stmt(&self) -> Option<&Stmt> {
        match &self.op {
            Op::Stmt(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Block,
    /// Db2 `BEGIN ATOMIC`: a savepoint level.
    Atomic,
    /// PL/pgSQL block with an EXCEPTION clause: a subtransaction.
    ExceptionBlock,
    Try,
}

#[derive(Debug, Clone)]
pub struct Scope {
    pub kind: ScopeKind,
    pub parent: Option<ScopeId>,
}

/// A temporary object declared or referenced in the artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TempObject {
    pub key: String,
    /// Profile `temp` row object: `table_variable`, `temp_table`, `dgtt`.
    pub object: String,
    /// `keep`, `revert` or `empty`.
    pub on_rollback: String,
    /// `keep` or `empty`.
    pub on_commit: String,
}

#[derive(Debug, Clone)]
pub struct Cfg {
    pub dialect: Dialect,
    pub routine: RoutineKind,
    /// Db2 for i `SET OPTION COMMIT = *X`.
    pub commit_level: Option<String>,
    pub nodes: Vec<Node>,
    pub scopes: Vec<Scope>,
    pub entry: NodeId,
    pub temps: BTreeMap<String, TempObject>,
}

impl Cfg {
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    /// Statement nodes with a durable effect, in document order.
    pub fn effect_nodes(&self) -> Vec<(NodeId, &Effect)> {
        let mut v: Vec<(NodeId, &Effect)> = self
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| n.stmt().and_then(|s| s.durable_effect()).map(|e| (i, e)))
            .collect();
        v.sort_by_key(|(i, _)| {
            self.nodes[*i]
                .syntax
                .as_ref()
                .map(|s| s.text_range().start())
        });
        v
    }

    /// The statement node that produces the durable effect `key`.
    pub fn node_of_effect(&self, key: &str) -> Option<NodeId> {
        self.effect_nodes()
            .into_iter()
            .find(|(_, e)| e.key == key)
            .map(|(i, _)| i)
    }

    /// Text form for debugging and tests: one line per node with its edges.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for (i, n) in self.nodes.iter().enumerate() {
            let what = match &n.op {
                Op::Stmt(s) => {
                    let text = n
                        .syntax
                        .as_ref()
                        .map(|x| ast::first_words(x, 3).join(" "))
                        .unwrap_or_default();
                    let eff: Vec<&str> = s.effects.iter().map(|e| e.key.as_str()).collect();
                    format!("stmt {text} {:?} {eff:?}", s.action)
                }
                op => format!("{op:?}"),
            };
            let edges: Vec<String> = n
                .succ
                .iter()
                .map(|(e, t)| match e {
                    Edge::Error(k) => format!("{k:?}->{t}"),
                    e => format!("{e:?}->{t}"),
                })
                .collect();
            let _ = writeln!(out, "{i}: {what} [{}]", edges.join(", "));
        }
        out
    }
}

#[derive(Clone)]
enum Frame {
    Scope(ScopeId),
    Try {
        scope: ScopeId,
        catch: NodeId,
    },
    Handlers {
        scope: ScopeId,
        /// (kinds, construct, entry), most specific first.
        handlers: Vec<(Vec<ErrorKind>, &'static str, NodeId)>,
    },
}

#[derive(Clone)]
struct Ctx {
    frames: Vec<Frame>,
    scope: Option<ScopeId>,
    /// Innermost loop: (head, exit).
    loop_: Option<(NodeId, NodeId)>,
    /// Labels of enclosing loops and blocks: label -> (head, exit).
    labels: Vec<(String, NodeId, NodeId)>,
    /// Db2 `DECLARE c CONDITION FOR SQLSTATE '...'`.
    conditions: HashMap<String, Vec<ErrorKind>>,
}

struct Lowerer {
    prof: &'static Profile,
    dialect: Dialect,
    routine: RoutineKind,
    nodes: Vec<Node>,
    scopes: Vec<Scope>,
    temps: BTreeMap<String, TempObject>,
    keys: HashMap<(usize, usize), String>,
    savepoint_names: Vec<String>,
}

/// Lowers an analyzed artifact.
pub fn lower(a: &Analyzed) -> Cfg {
    let d = a.dialect();
    let prof = Profile::of(d);
    let routine_node = a
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::ROUTINE);
    let routine = routine_node
        .as_ref()
        .map(routine_kind)
        .unwrap_or(RoutineKind::Script);
    let units: Vec<SyntaxNode> = match routine_node
        .as_ref()
        .and_then(|r| r.children().find(|c| c.kind() == SyntaxKind::ROUTINE_BODY))
    {
        Some(body) => ast::structural_children(&body),
        None => {
            let mut v = Vec::new();
            for c in ast::structural_children(&a.root) {
                if c.kind() == SyntaxKind::BATCH {
                    v.extend(ast::structural_children(&c));
                } else {
                    v.push(c);
                }
            }
            v
        }
    };
    let scan_root = routine_node.clone().unwrap_or_else(|| a.root.clone());
    let mut l = Lowerer {
        prof,
        dialect: d,
        routine,
        nodes: Vec::new(),
        scopes: Vec::new(),
        temps: BTreeMap::new(),
        keys: HashMap::new(),
        savepoint_names: Vec::new(),
    };
    l.add(Op::ExitOk, None);
    l.add(Op::ExitErr, None);
    let entry = l.add(Op::Entry, None);
    l.prescan(&scan_root);
    let ctx = Ctx {
        frames: Vec::new(),
        scope: None,
        loop_: None,
        labels: Vec::new(),
        conditions: HashMap::new(),
    };
    let first = l.seq(&units, EXIT_OK, &ctx);
    l.edge(entry, Edge::Next, first);
    let commit_level = if d == Dialect::Db2i {
        crate::facts::find_commit_level(a).map(|(l, _)| l)
    } else {
        None
    };
    Cfg {
        dialect: d,
        routine,
        commit_level,
        nodes: l.nodes,
        scopes: l.scopes,
        entry,
        temps: l.temps,
    }
}

pub fn routine_kind(node: &SyntaxNode) -> RoutineKind {
    let words = ast::first_words(node, 4);
    if words.first().is_some_and(|w| w == "DO") {
        RoutineKind::Anonymous
    } else if words.iter().any(|w| w == "FUNCTION") {
        RoutineKind::Function
    } else if words.iter().any(|w| w == "TRIGGER") {
        RoutineKind::Trigger
    } else {
        RoutineKind::Procedure
    }
}

fn upper(t: &SyntaxToken) -> String {
    t.text().to_ascii_uppercase()
}

fn is_word(t: Option<&SyntaxToken>, w: &str) -> bool {
    t.is_some_and(|t| t.kind() == SyntaxKind::IDENT && t.text().eq_ignore_ascii_case(w))
}

/// Folded key of a temporary object name: no `@`/`#` prefix, no schema, upper case.
pub fn temp_key(name: &str) -> String {
    name.trim_start_matches(['@', '#']).to_ascii_uppercase()
}

impl Lowerer {
    fn add(&mut self, op: Op, syntax: Option<SyntaxNode>) -> NodeId {
        self.nodes.push(Node {
            op,
            syntax,
            succ: Vec::new(),
            routes: Vec::new(),
        });
        self.nodes.len() - 1
    }

    fn edge(&mut self, from: NodeId, e: Edge, to: NodeId) {
        self.nodes[from].succ.push((e, to));
    }

    fn new_scope(&mut self, kind: ScopeKind, parent: Option<ScopeId>) -> ScopeId {
        self.scopes.push(Scope { kind, parent });
        self.scopes.len() - 1
    }

    /// Temporary objects and effect keys, in document order.
    fn prescan(&mut self, root: &SyntaxNode) {
        let mut occ: HashMap<String, usize> = HashMap::new();
        for n in root.descendants() {
            if n.kind() != SyntaxKind::STATEMENT {
                continue;
            }
            let toks: Vec<SyntaxToken> = ast::sig_tokens(&n).collect();
            self.scan_temp_decl(&toks);
            let w0 = toks.first().map(upper).unwrap_or_default();
            if self.dialect == Dialect::Tsql && w0 == "SAVE" {
                if let Some(t) = toks.get(2) {
                    self.savepoint_names.push(t.text().to_ascii_uppercase());
                }
            }
        }
        for n in root.descendants() {
            if n.kind() != SyntaxKind::STATEMENT {
                continue;
            }
            if let Some((label, target)) = self.durable_write(&n) {
                let base = format!("{label}:{}", classify::pairing_key(&[target]));
                let c = occ.entry(base.clone()).or_insert(0);
                *c += 1;
                let r = n.text_range();
                self.keys
                    .insert((r.start().into(), r.end().into()), format!("{base}#{c}"));
            }
        }
    }

    fn scan_temp_decl(&mut self, toks: &[SyntaxToken]) {
        let w = |i: usize| toks.get(i).map(upper).unwrap_or_default();
        let words: Vec<String> = toks.iter().map(upper).collect();
        let has = |a: &str, b: &str| words.windows(2).any(|x| x[0] == a && x[1] == b);
        let mut add = |key: String, object: &str, on_rollback: String, on_commit: String| {
            self.temps.insert(
                key.clone(),
                TempObject {
                    key,
                    object: object.to_string(),
                    on_rollback,
                    on_commit,
                },
            );
        };
        let row = |o: &str| self.prof.temp_row(o).cloned();
        match self.dialect {
            Dialect::Tsql => {
                if w(0) == "DECLARE"
                    && toks.get(1).is_some_and(|t| t.text().starts_with('@'))
                    && w(2) == "TABLE"
                {
                    if let Some(r) = row("table_variable") {
                        add(
                            temp_key(toks[1].text()),
                            "table_variable",
                            r.on_rollback,
                            r.on_commit,
                        );
                    }
                }
                if w(0) == "CREATE" && w(1) == "TABLE" {
                    if let Some(t) = toks.get(2).filter(|t| t.text().starts_with('#')) {
                        if let Some(r) = row("temp_table") {
                            add(temp_key(t.text()), "temp_table", r.on_rollback, r.on_commit);
                        }
                    }
                }
            }
            Dialect::Postgres => {
                if w(0) == "CREATE"
                    && matches!(w(1).as_str(), "TEMP" | "TEMPORARY")
                    && w(2) == "TABLE"
                {
                    let mut i = 3;
                    if w(3) == "IF" {
                        i = 6;
                    }
                    let parts = ast::qualified_name_at(toks, i);
                    if let (Some(name), Some(r)) = (parts.last(), row("temp_table")) {
                        let on_commit = if has("DELETE", "ROWS") || has("COMMIT", "DROP") {
                            "empty".to_string()
                        } else {
                            r.on_commit.clone()
                        };
                        add(temp_key(name), "temp_table", r.on_rollback, on_commit);
                    }
                }
            }
            Dialect::Db2i => {
                if w(0) == "DECLARE" && w(1) == "GLOBAL" && w(2) == "TEMPORARY" && w(3) == "TABLE" {
                    let parts = ast::qualified_name_at(toks, 4);
                    if let (Some(name), Some(r)) = (parts.last(), row("dgtt")) {
                        // ON COMMIT ... ROWS and ON ROLLBACK ... ROWS (V7, V16).
                        let mut on_commit = r.on_commit.clone();
                        let mut on_rollback = r.on_rollback.clone();
                        for (i, x) in words.iter().enumerate() {
                            if x == "ON" && words.get(i + 2).is_some_and(|y| y == "PRESERVE") {
                                match words.get(i + 1).map(String::as_str) {
                                    Some("COMMIT") => on_commit = "keep".to_string(),
                                    Some("ROLLBACK") => on_rollback = "keep".to_string(),
                                    _ => {}
                                }
                            }
                        }
                        if words.iter().any(|x| x == "LOGGED") && !has("NOT", "LOGGED") {
                            on_rollback = "revert".to_string();
                        }
                        add(temp_key(name), "dgtt", on_rollback, on_commit);
                    }
                }
            }
        }
    }

    /// Whether name parts denote a temporary object, and its key.
    fn temp_of(&mut self, parts: &[String]) -> Option<String> {
        let last = parts.last()?;
        if self.dialect == Dialect::Tsql && last.starts_with('#') {
            let key = temp_key(last);
            if !self.temps.contains_key(&key) {
                if let Some(r) = self.prof.temp_row("temp_table") {
                    self.temps.insert(
                        key.clone(),
                        TempObject {
                            key: key.clone(),
                            object: "temp_table".into(),
                            on_rollback: r.on_rollback.clone(),
                            on_commit: r.on_commit.clone(),
                        },
                    );
                }
            }
            return Some(key);
        }
        let key = temp_key(last);
        let schema_temp = parts.len() > 1
            && parts[0].eq_ignore_ascii_case(match self.dialect {
                Dialect::Db2i => "SESSION",
                _ => "pg_temp",
            });
        if self.temps.contains_key(&key) && (schema_temp || parts.len() == 1) {
            return Some(key);
        }
        (schema_temp || classify::is_temp_target(parts)).then_some(key)
    }

    /// (`Insert`/`Update`/`Delete`/`Upsert`/`Call`, target) of a durable write.
    fn durable_write(&mut self, n: &SyntaxNode) -> Option<(String, String)> {
        let kind = classify::stmt_kind(n, self.dialect);
        let label = |k: StmtKind, upsert: bool| {
            if upsert || k == StmtKind::Merge {
                "Upsert".to_string()
            } else {
                format!("{k:?}")
            }
        };
        if kind.is_write() {
            let parts = classify::write_target(n)?;
            if self.temp_of(&parts).is_some() {
                return None;
            }
            return Some((label(kind, has_on_conflict(n)), parts.last()?.clone()));
        }
        if kind == StmtKind::Select {
            let (k, parts) = classify::nested_write(n)?;
            if self.temp_of(&parts).is_some() {
                return None;
            }
            return Some((label(k, false), parts.last()?.clone()));
        }
        if kind == StmtKind::Call {
            let name = call_name(n)?;
            return Some(("Call".to_string(), name));
        }
        None
    }

    fn seq(&mut self, units: &[SyntaxNode], next: NodeId, ctx: &Ctx) -> NodeId {
        let mut cur = next;
        for u in units.iter().rev() {
            cur = self.unit(u, cur, ctx);
        }
        cur
    }

    fn unit(&mut self, n: &SyntaxNode, next: NodeId, ctx: &Ctx) -> NodeId {
        match n.kind() {
            SyntaxKind::STATEMENT => self.statement(n, next, ctx),
            SyntaxKind::BLOCK => self.block(n, next, ctx),
            SyntaxKind::IF_STMT | SyntaxKind::CASE_STMT => self.branches(n, next, ctx),
            SyntaxKind::LOOP_STMT => self.loop_(n, next, ctx),
            SyntaxKind::TRY_CATCH => self.try_catch(n, next, ctx),
            SyntaxKind::BRANCH
            | SyntaxKind::TRY_BLOCK
            | SyntaxKind::CATCH_BLOCK
            | SyntaxKind::DECLARE_SECTION => {
                let units = ast::structural_children(n);
                if n.kind() == SyntaxKind::DECLARE_SECTION {
                    return next;
                }
                self.seq(&units, next, ctx)
            }
            SyntaxKind::LABEL => {
                // `l1: BEGIN ... END` / `l1: LOOP ...`: the labelled unit follows.
                next
            }
            SyntaxKind::ERROR => {
                let id = self.add(
                    Op::Stmt(Stmt {
                        kind: StmtKind::Other,
                        action: Action::Opaque("unparsed text".into()),
                        effects: Vec::new(),
                        temp_reads: Vec::new(),
                        raises: Vec::new(),
                        category: None,
                        multi_row: false,
                        not_atomic: false,
                    }),
                    Some(n.clone()),
                );
                self.edge(id, Edge::Next, next);
                id
            }
            // Handlers are lowered by their block; nested routines are DDL.
            _ => next,
        }
    }

    fn label_of(n: &SyntaxNode) -> Option<String> {
        let prev = n.prev_sibling().filter(|p| p.kind() == SyntaxKind::LABEL);
        let inner = n.children().find(|c| c.kind() == SyntaxKind::LABEL);
        prev.or(inner)
            .and_then(|l| ast::sig_tokens(&l).next())
            .map(|t| upper(&t))
    }

    fn block(&mut self, n: &SyntaxNode, next: NodeId, ctx: &Ctx) -> NodeId {
        let header: Vec<String> = ast::header_tokens(n).iter().take(3).map(upper).collect();
        let atomic = header
            .iter()
            .position(|w| w == "BEGIN")
            .is_some_and(|i| header.get(i + 1).is_some_and(|w| w == "ATOMIC"));
        let has_exception = n
            .children()
            .any(|c| c.kind() == SyntaxKind::EXCEPTION_SECTION);
        let kind = if atomic {
            ScopeKind::Atomic
        } else if has_exception {
            ScopeKind::ExceptionBlock
        } else {
            ScopeKind::Block
        };
        let scope = self.new_scope(kind, ctx.scope);
        let leave = self.add(Op::Leave(scope), None);
        self.edge(leave, Edge::Next, next);

        let mut inner = ctx.clone();
        inner.scope = Some(scope);
        if let Some(l) = Self::label_of(n) {
            inner.labels.push((l, leave, leave));
        }
        // Db2 condition declarations.
        for c in n.children().filter(|c| c.kind() == SyntaxKind::STATEMENT) {
            let w = ast::first_words(&c, 3);
            if w.first().is_some_and(|x| x == "DECLARE")
                && w.get(2).is_some_and(|x| x == "CONDITION")
            {
                let toks: Vec<SyntaxToken> = ast::sig_tokens(&c).collect();
                if let Some(code) = toks.iter().find(|t| t.kind() == SyntaxKind::STRING) {
                    let kinds = self.prof.condition_kinds(code.text());
                    inner.conditions.insert(w[1].clone(), kinds);
                }
            }
        }

        // Handlers: lowered with the context outside this block's handlers.
        let mut handlers = Vec::new();
        let mut handler_ctx = inner.clone();
        handler_ctx.frames.push(Frame::Scope(scope));
        for h in n.children().filter(|c| c.kind() == SyntaxKind::HANDLER) {
            let words: Vec<String> = ast::header_tokens(&h).iter().take(3).map(upper).collect();
            let construct = match words.get(1).map(String::as_str) {
                Some("CONTINUE") => "continue_handler",
                Some("UNDO") => "undo_handler",
                _ => "exit_handler",
            };
            let kinds = h
                .children()
                .find(|c| c.kind() == SyntaxKind::CONDITION)
                .map(|c| self.db2_condition_kinds(&c, &inner.conditions))
                .unwrap_or_default();
            let done = self.add(
                Op::HandlerDone {
                    resume: construct == "continue_handler",
                },
                None,
            );
            if construct != "continue_handler" {
                self.edge(done, Edge::Next, leave);
            }
            let body: Vec<SyntaxNode> = h
                .children()
                .filter(|c| c.kind() == SyntaxKind::BRANCH)
                .collect();
            let entry = self.seq(&body, done, &handler_ctx);
            handlers.push((kinds, construct, entry));
        }
        if let Some(ex) = n
            .children()
            .find(|c| c.kind() == SyntaxKind::EXCEPTION_SECTION)
        {
            // PL/pgSQL: the handler runs after the block's subtransaction is
            // rolled back, outside the block.
            for h in ex
                .children()
                .filter(|c| c.kind() == SyntaxKind::EXCEPTION_HANDLER)
            {
                let kinds = h
                    .children()
                    .find(|c| c.kind() == SyntaxKind::CONDITION)
                    .map(|c| self.pg_condition_kinds(&c))
                    .unwrap_or_default();
                let body: Vec<SyntaxNode> = h
                    .children()
                    .filter(|c| c.kind() == SyntaxKind::BRANCH)
                    .collect();
                let entry = self.seq(&body, next, ctx);
                handlers.push((kinds, "exception_block", entry));
            }
        }
        handlers.sort_by_key(|(k, _, _)| k.len());
        inner.frames.push(if handlers.is_empty() {
            Frame::Scope(scope)
        } else {
            Frame::Handlers { scope, handlers }
        });
        let units: Vec<SyntaxNode> = ast::structural_children(n)
            .into_iter()
            .filter(|c| {
                !matches!(
                    c.kind(),
                    SyntaxKind::HANDLER
                        | SyntaxKind::EXCEPTION_SECTION
                        | SyntaxKind::DECLARE_SECTION
                )
            })
            .collect();
        let body = self.seq(&units, leave, &inner);
        let enter = self.add(Op::Enter(scope), Some(n.clone()));
        self.edge(enter, Edge::Next, body);
        enter
    }

    fn db2_condition_kinds(
        &self,
        c: &SyntaxNode,
        declared: &HashMap<String, Vec<ErrorKind>>,
    ) -> Vec<ErrorKind> {
        let toks: Vec<SyntaxToken> = ast::sig_tokens(c).collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < toks.len() {
            let t = &toks[i];
            let u = upper(t);
            let kinds = if u == "NOT" && is_word(toks.get(i + 1), "FOUND") {
                i += 1;
                self.prof.condition_kinds("NOT FOUND")
            } else if u == "SQLSTATE" {
                let mut j = i + 1;
                if is_word(toks.get(j), "VALUE") {
                    j += 1;
                }
                i = j;
                toks.get(j)
                    .map(|s| self.prof.condition_kinds(s.text()))
                    .unwrap_or_default()
            } else if t.kind() == SyntaxKind::COMMA {
                Vec::new()
            } else if let Some(k) = declared.get(&u) {
                k.clone()
            } else {
                self.prof.condition_kinds(&u)
            };
            for k in kinds {
                if !out.contains(&k) {
                    out.push(k);
                }
            }
            i += 1;
        }
        out
    }

    fn pg_condition_kinds(&self, c: &SyntaxNode) -> Vec<ErrorKind> {
        let toks: Vec<SyntaxToken> = ast::sig_tokens(c).collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < toks.len() {
            let u = upper(&toks[i]);
            let kinds = if u == "OR" {
                Vec::new()
            } else if u == "SQLSTATE" {
                i += 1;
                toks.get(i)
                    .map(|s| self.prof.condition_kinds(s.text()))
                    .unwrap_or_default()
            } else {
                self.prof.condition_kinds(&u)
            };
            for k in kinds {
                if !out.contains(&k) {
                    out.push(k);
                }
            }
            i += 1;
        }
        out
    }

    fn try_catch(&mut self, n: &SyntaxNode, next: NodeId, ctx: &Ctx) -> NodeId {
        let scope = self.new_scope(ScopeKind::Try, ctx.scope);
        let catch_units: Vec<SyntaxNode> = n
            .children()
            .filter(|c| c.kind() == SyntaxKind::CATCH_BLOCK)
            .flat_map(|c| ast::structural_children(&c))
            .collect();
        let catch = self.seq(&catch_units, next, ctx);
        let leave = self.add(Op::Leave(scope), None);
        self.edge(leave, Edge::Next, next);
        let mut inner = ctx.clone();
        inner.scope = Some(scope);
        inner.frames.push(Frame::Try { scope, catch });
        let try_units: Vec<SyntaxNode> = n
            .children()
            .filter(|c| c.kind() == SyntaxKind::TRY_BLOCK)
            .flat_map(|c| ast::structural_children(&c))
            .collect();
        let body = self.seq(&try_units, leave, &inner);
        let enter = self.add(Op::Enter(scope), Some(n.clone()));
        self.edge(enter, Edge::Next, body);
        enter
    }

    fn test_of(&self, cond: &SyntaxNode) -> Test {
        let toks: Vec<String> = ast::sig_tokens(cond).map(|t| upper(&t)).collect();
        let text = toks.join(" ");
        let cmp = |i: usize| -> Option<(String, String)> {
            Some((toks.get(i + 1)?.clone(), toks.get(i + 2)?.clone()))
        };
        match self.dialect {
            Dialect::Postgres => match toks.as_slice() {
                [f] if f == "FOUND" => return Test::NoRows(false),
                [n, f] if n == "NOT" && f == "FOUND" => return Test::NoRows(true),
                _ => {}
            },
            Dialect::Db2i => {
                if let Some(i) = toks.iter().position(|t| t == "SQLCODE" || t == "SQLSTATE") {
                    if let Some((op, v)) = cmp(i) {
                        match (toks[i].as_str(), op.as_str(), v.as_str()) {
                            ("SQLCODE", "=", "100") | ("SQLSTATE", "=", "'02000'") => {
                                return Test::NoRows(true)
                            }
                            ("SQLCODE", "<>", "100") | ("SQLSTATE", "<>", "'02000'") => {
                                return Test::NoRows(false)
                            }
                            ("SQLCODE", "<", "0") => return Test::ErrorStatus(true),
                            ("SQLCODE", ">=", "0") => return Test::ErrorStatus(false),
                            _ => {}
                        }
                    }
                }
            }
            Dialect::Tsql => {}
        }
        if self.dialect == Dialect::Tsql {
            if let Some(i) = toks.iter().position(|t| t == "@@ROWCOUNT") {
                if let Some((op, v)) = cmp(i) {
                    match (op.as_str(), v.as_str()) {
                        ("=", "0") => return Test::NoRows(true),
                        ("<>" | "!=" | ">", "0") => return Test::NoRows(false),
                        _ => {}
                    }
                }
            }
            if let Some(i) = toks.iter().position(|t| t == "@@ERROR") {
                if let Some((op, v)) = cmp(i) {
                    match (op.as_str(), v.as_str()) {
                        ("<>" | "!=" | ">", "0") => return Test::ErrorStatus(true),
                        ("=", "0") => return Test::ErrorStatus(false),
                        _ => {}
                    }
                }
            }
            if let Some(i) = toks.iter().position(|t| t == "@@TRANCOUNT") {
                if let Some((op, v)) = cmp(i) {
                    match (op.as_str(), v.as_str()) {
                        ("<>" | "!=" | ">", "0") | (">=", "1") => return Test::TranCount(true),
                        ("=", "0") => return Test::TranCount(false),
                        _ => {}
                    }
                }
            }
            if let Some(i) = toks.iter().position(|t| t == "XACT_STATE") {
                // XACT_STATE ( ) op value
                let op = toks.get(i + 3).cloned().unwrap_or_default();
                let v: String = toks[(i + 4).min(toks.len())..].concat();
                match (op.as_str(), v.as_str()) {
                    ("<>" | "!=", "0") => return Test::XactStateNonZero(true),
                    ("=", "0") => return Test::XactStateNonZero(false),
                    ("=", "-1") | ("<", "0") => return Test::XactStateDoomed,
                    ("=", "1") | (">", "0") => return Test::XactStateCommittable,
                    _ => {}
                }
            }
        }
        Test::Unknown(text)
    }

    fn branches(&mut self, n: &SyntaxNode, next: NodeId, ctx: &Ctx) -> NodeId {
        // IF c THEN b [ELSIF c THEN b]* [ELSE b]; CASE [sel] WHEN c THEN b ... [ELSE b].
        let mut arms: Vec<(Option<SyntaxNode>, SyntaxNode)> = Vec::new();
        let mut pending: Option<SyntaxNode> = None;
        for c in n.children() {
            match c.kind() {
                SyntaxKind::CONDITION => pending = Some(c),
                SyntaxKind::BRANCH => arms.push((pending.take(), c)),
                _ => {}
            }
        }
        let mut cur = next;
        let mut has_else = false;
        if let Some((None, _)) = arms.last() {
            has_else = true;
        }
        for (i, (cond, br)) in arms.iter().enumerate().rev() {
            let body = self.seq(&ast::structural_children(br), next, ctx);
            match cond {
                None if i + 1 == arms.len() && has_else => cur = body,
                None => cur = body,
                Some(c) => {
                    let t = self.add(
                        Op::Test {
                            test: self.test_of(c),
                            is_loop: false,
                        },
                        Some(c.clone()),
                    );
                    self.edge(t, Edge::True, body);
                    self.edge(t, Edge::False, cur);
                    cur = t;
                }
            }
        }
        cur
    }

    fn loop_(&mut self, n: &SyntaxNode, next: NodeId, ctx: &Ctx) -> NodeId {
        let cond = n.children().find(|c| c.kind() == SyntaxKind::CONDITION);
        let first = ast::header_tokens(n)
            .iter()
            .find(|t| {
                t.kind() == SyntaxKind::IDENT
                    && n.children().all(|l| {
                        l.kind() != SyntaxKind::LABEL
                            || !l.text_range().contains_range(t.text_range())
                    })
            })
            .map(upper)
            .unwrap_or_default();
        let text = match &cond {
            Some(c) => format!("{first} {}", ast::first_words(c, 12).join(" ")),
            None => format!("{first} (until LEAVE)"),
        };
        let test = self.add(
            Op::Test {
                test: Test::Unknown(text),
                is_loop: true,
            },
            cond.clone().or_else(|| Some(n.clone())),
        );
        let mut inner = ctx.clone();
        inner.loop_ = Some((test, next));
        if let Some(l) = Self::label_of(n) {
            inner.labels.push((l, test, next));
        }
        let body_units: Vec<SyntaxNode> = n
            .children()
            .filter(|c| c.kind() == SyntaxKind::BRANCH)
            .collect();
        let body = self.seq(&body_units, test, &inner);
        self.edge(test, Edge::True, body);
        self.edge(test, Edge::False, next);
        if first == "REPEAT" {
            body
        } else {
            test
        }
    }

    fn resolve(&self, ctx: &Ctx, kind: ErrorKind, resume: NodeId) -> Route {
        let mut unwind = Vec::new();
        for f in ctx.frames.iter().rev() {
            match f {
                Frame::Scope(s) => unwind.push(*s),
                Frame::Try { scope, catch } => {
                    if self.prof.condition_kinds("CATCH").contains(&kind) {
                        return Route {
                            handler: Some(HandlerRef {
                                construct: "try_catch",
                                entry: *catch,
                                scope: *scope,
                            }),
                            unwind,
                            resume,
                        };
                    }
                    unwind.push(*scope);
                }
                Frame::Handlers { scope, handlers } => {
                    if let Some((_, c, e)) = handlers.iter().find(|(k, _, _)| k.contains(&kind)) {
                        return Route {
                            handler: Some(HandlerRef {
                                construct: c,
                                entry: *e,
                                scope: *scope,
                            }),
                            unwind,
                            resume,
                        };
                    }
                    unwind.push(*scope);
                }
            }
        }
        Route {
            handler: None,
            unwind,
            resume,
        }
    }

    fn statement(&mut self, n: &SyntaxNode, next: NodeId, ctx: &Ctx) -> NodeId {
        let toks: Vec<SyntaxToken> = ast::sig_tokens(n).collect();
        let w = |i: usize| toks.get(i).map(upper).unwrap_or_default();
        // Loop and block exits.
        match w(0).as_str() {
            "LEAVE" | "BREAK" | "EXIT" | "CONTINUE" | "ITERATE" => {
                let label = toks
                    .get(1)
                    .filter(|t| {
                        t.kind() == SyntaxKind::IDENT && !t.text().eq_ignore_ascii_case("WHEN")
                    })
                    .map(upper);
                let target = match &label {
                    Some(l) => ctx
                        .labels
                        .iter()
                        .rev()
                        .find(|(x, _, _)| x == l)
                        .map(|(_, h, e)| (*h, *e)),
                    None => ctx.loop_,
                };
                if let Some((head, exit)) = target {
                    let to = if matches!(w(0).as_str(), "CONTINUE" | "ITERATE") {
                        head
                    } else {
                        exit
                    };
                    let when = toks
                        .iter()
                        .position(|t| t.text().eq_ignore_ascii_case("WHEN"));
                    if let Some(i) = when {
                        let text: Vec<String> = toks[i + 1..].iter().map(upper).collect();
                        let t = self.add(
                            Op::Test {
                                test: Test::Unknown(text.join(" ")),
                                is_loop: false,
                            },
                            Some(n.clone()),
                        );
                        self.edge(t, Edge::True, to);
                        self.edge(t, Edge::False, next);
                        return t;
                    }
                    let id = self.add(Op::Nop, Some(n.clone()));
                    self.edge(id, Edge::Next, to);
                    return id;
                }
            }
            _ => {}
        }
        let s = self.stmt(n, &toks);
        let is_return = matches!(s.action, Action::Return(_));
        let raises: Vec<ErrorKind> = match &s.action {
            Action::Raise { kind, .. } => vec![*kind],
            Action::Reraise { .. } => ErrorKind::ALL.to_vec(),
            _ => s.raises.clone(),
        };
        let always_raises = matches!(s.action, Action::Raise { .. } | Action::Reraise { .. });
        let id = self.add(Op::Stmt(s), Some(n.clone()));
        if !always_raises {
            self.edge(id, Edge::Next, if is_return { EXIT_OK } else { next });
        }
        for k in ErrorKind::ALL {
            let r = self.resolve(ctx, k, next);
            if raises.contains(&k) {
                let to = r.handler.as_ref().map(|h| h.entry).unwrap_or(EXIT_ERR);
                self.edge(id, Edge::Error(k), to);
            }
            self.nodes[id].routes.push((k, r));
        }
        id
    }

    fn stmt(&mut self, n: &SyntaxNode, toks: &[SyntaxToken]) -> Stmt {
        let d = self.dialect;
        let kind = classify::stmt_kind(n, d);
        let w = |i: usize| toks.get(i).map(upper).unwrap_or_default();
        let name_after = |i: usize| -> String {
            let mut j = i;
            while matches!(
                w(j).as_str(),
                "TRAN" | "TRANSACTION" | "TO" | "SAVEPOINT" | "WORK"
            ) {
                j += 1;
            }
            toks.get(j)
                .filter(|t| t.kind() != SyntaxKind::SEMICOLON)
                .map(upper)
                .unwrap_or_default()
        };
        let action = match kind {
            StmtKind::TxnBegin => Action::Begin,
            StmtKind::TxnCommit => Action::Commit,
            StmtKind::TxnRollback => {
                let has_to = toks.iter().any(|t| t.text().eq_ignore_ascii_case("TO"));
                let name = name_after(1);
                if has_to || (d == Dialect::Tsql && self.savepoint_names.contains(&name)) {
                    Action::RollbackTo(name)
                } else {
                    Action::Rollback
                }
            }
            StmtKind::Savepoint => match w(0).as_str() {
                "RELEASE" => Action::Release(name_after(1)),
                _ => Action::Savepoint(name_after(1)),
            },
            StmtKind::TxnSetting => match (w(1).as_str(), w(2).as_str()) {
                ("XACT_ABORT", v) => Action::XactAbort(v == "ON"),
                ("IMPLICIT_TRANSACTIONS", v) => Action::ImplicitTransactions(v == "ON"),
                _ => Action::None,
            },
            StmtKind::Return => {
                // A function's RETURN gives its value, not a status code.
                let code = crate::facts::return_code(n);
                Action::Return(match self.routine {
                    RoutineKind::Function => None,
                    _ => code,
                })
            }
            StmtKind::Raise => self.raise_action(toks),
            StmtKind::Call => Action::Call(call_name(n).unwrap_or_else(|| "dynamic SQL".into())),
            _ => match w(0).as_str() {
                "GOTO" => Action::Opaque("GOTO".into()),
                "RAISERROR" => self.raise_action(toks),
                _ => Action::None,
            },
        };
        let mut effects = Vec::new();
        let r = n.text_range();
        if let Some(key) = self.keys.get(&(r.start().into(), r.end().into())).cloned() {
            if let Some((_, target)) = self.durable_write(n) {
                let k = if kind == StmtKind::Select {
                    classify::nested_write(n).map(|x| x.0).unwrap_or(kind)
                } else {
                    kind
                };
                effects.push(Effect {
                    key,
                    kind: k,
                    target,
                    temp: None,
                });
            }
        }
        // Temporary objects written and read.
        let mut temp_writes: Vec<String> = Vec::new();
        if kind.is_write() {
            if let Some(parts) = classify::write_target(n) {
                if let Some(t) = self.temp_of(&parts) {
                    temp_writes.push(t);
                }
            }
        }
        let words: Vec<String> = toks.iter().map(upper).collect();
        // T-SQL OUTPUT ... INTO @t / #t, SELECT ... INTO #t.
        if d == Dialect::Tsql {
            let out_pos = words.iter().position(|x| x == "OUTPUT");
            for (i, x) in words.iter().enumerate() {
                if x == "INTO" && (out_pos.is_some_and(|o| i > o) || kind == StmtKind::Select) {
                    let parts = ast::qualified_name_at(toks, i + 1);
                    if let Some(t) = self.temp_of(&parts) {
                        if !temp_writes.contains(&t) {
                            temp_writes.push(t);
                        }
                    }
                }
            }
        }
        for t in &temp_writes {
            effects.push(Effect {
                key: format!("Temp:{t}"),
                kind,
                target: t.clone(),
                temp: Some(t.clone()),
            });
        }
        let mut mentions: HashMap<String, usize> = HashMap::new();
        for (i, t) in toks.iter().enumerate() {
            if !matches!(
                t.kind(),
                SyntaxKind::IDENT | SyntaxKind::VARIABLE | SyntaxKind::QUOTED_IDENT
            ) {
                continue;
            }
            if i > 0 && toks[i - 1].kind() == SyntaxKind::DOT {
                continue;
            }
            let parts = ast::qualified_name_at(toks, i);
            let is_temp = match d {
                Dialect::Tsql => {
                    t.text().starts_with('#')
                        || (t.text().starts_with('@')
                            && self.temps.contains_key(&temp_key(t.text())))
                }
                _ => {
                    self.temps
                        .contains_key(&temp_key(parts.last().map(String::as_str).unwrap_or("")))
                        && (parts.len() == 1
                            || parts[0].eq_ignore_ascii_case("SESSION")
                            || parts[0].eq_ignore_ascii_case("pg_temp"))
                }
            };
            if is_temp {
                *mentions
                    .entry(temp_key(parts.last().unwrap_or(&t.text().to_string())))
                    .or_insert(0) += 1;
            }
        }
        let declares = matches!(kind, StmtKind::Declare | StmtKind::Ddl);
        let temp_reads: Vec<String> = if declares {
            Vec::new()
        } else {
            let mut v: Vec<String> = mentions
                .into_iter()
                .filter(|(t, c)| !temp_writes.contains(t) || *c > 1)
                .map(|(t, _)| t)
                .collect();
            v.sort();
            v
        };
        let category: Option<&'static str> = match kind {
            StmtKind::Insert if has_on_conflict(n) => Some("merge"),
            StmtKind::Insert => Some("insert"),
            StmtKind::Update => Some("update"),
            StmtKind::Delete => Some("delete"),
            StmtKind::Merge => Some("merge"),
            StmtKind::TxnCommit => Some("commit"),
            StmtKind::Call => Some("call"),
            StmtKind::Select => {
                if let Some((k, _)) = classify::nested_write(n) {
                    match k {
                        StmtKind::Insert => Some("insert"),
                        StmtKind::Update => Some("update"),
                        StmtKind::Delete => Some("delete"),
                        _ => Some("merge"),
                    }
                } else if self.is_select_into(&words, toks) {
                    if words.iter().any(|x| x == "STRICT") && d == Dialect::Postgres {
                        Some("select_into_strict")
                    } else {
                        Some("select_into")
                    }
                } else {
                    None
                }
            }
            _ => None,
        };
        let raises = category
            .map(|c| self.prof.raises(c).to_vec())
            .unwrap_or_default();
        let multi_row = match kind {
            StmtKind::Update | StmtKind::Delete | StmtKind::Merge => true,
            StmtKind::Insert => !words.iter().any(|x| x == "VALUES"),
            StmtKind::Select => classify::nested_write(n).is_some(),
            _ => false,
        };
        let not_atomic = d == Dialect::Db2i
            && kind == StmtKind::Merge
            && words.windows(2).any(|x| x[0] == "NOT" && x[1] == "ATOMIC");
        Stmt {
            kind,
            action,
            effects,
            temp_reads,
            raises,
            category,
            multi_row,
            not_atomic,
        }
    }

    fn is_select_into(&self, words: &[String], toks: &[SyntaxToken]) -> bool {
        match self.dialect {
            Dialect::Tsql => {
                // SELECT @v = ...
                toks.get(1)
                    .is_some_and(|t| t.kind() == SyntaxKind::VARIABLE)
                    && toks.get(2).is_some_and(|t| t.text() == "=")
            }
            _ => {
                let mut depth = 0i32;
                for (i, t) in toks.iter().enumerate() {
                    match t.kind() {
                        SyntaxKind::L_PAREN => depth += 1,
                        SyntaxKind::R_PAREN => depth -= 1,
                        _ if depth == 0 && words[i] == "INTO" => return true,
                        _ if depth == 0 && words[i] == "FROM" => return false,
                        _ => {}
                    }
                }
                false
            }
        }
    }

    fn raise_action(&self, toks: &[SyntaxToken]) -> Action {
        let w = |i: usize| toks.get(i).map(upper).unwrap_or_default();
        let bare = toks
            .iter()
            .skip(1)
            .all(|t| t.kind() == SyntaxKind::SEMICOLON);
        match w(0).as_str() {
            "THROW" if bare => Action::Reraise { form: "throw" },
            "THROW" => Action::Raise {
                kind: ErrorKind::UserSignal,
                form: "throw",
            },
            "RAISERROR" => {
                // RAISERROR (msg, severity, state): severity <= 10 is informational.
                let args: Vec<&SyntaxToken> = toks
                    .iter()
                    .filter(|t| t.kind() == SyntaxKind::NUMBER)
                    .collect();
                let sev = args.first().and_then(|t| t.text().parse::<u32>().ok());
                if sev.is_some_and(|s| s <= 10) {
                    Action::None
                } else {
                    Action::Raise {
                        kind: ErrorKind::UserSignal,
                        form: "raiserror",
                    }
                }
            }
            "RESIGNAL" if bare => Action::Reraise { form: "signal" },
            "SIGNAL" | "RESIGNAL" => {
                let code = toks.iter().find(|t| t.kind() == SyntaxKind::STRING);
                let kind = code
                    .and_then(|c| self.prof.kind_of_code(c.text().trim_matches('\'')))
                    .unwrap_or(ErrorKind::UserSignal);
                Action::Raise {
                    kind,
                    form: "signal",
                }
            }
            _ => {
                // PL/pgSQL RAISE [EXCEPTION] ... [USING ERRCODE = '...'] | RAISE condition | RAISE;
                if bare {
                    return Action::Reraise { form: "raise" };
                }
                let mut kind = ErrorKind::UserSignal;
                if let Some(i) = toks
                    .iter()
                    .position(|t| t.text().eq_ignore_ascii_case("ERRCODE"))
                {
                    if let Some(c) = toks[i..].iter().find(|t| t.kind() == SyntaxKind::STRING) {
                        let code = c.text().trim_matches('\'');
                        kind = self
                            .prof
                            .kind_of_code(code)
                            .or_else(|| self.prof.condition_kinds(code).first().copied())
                            .unwrap_or(kind);
                    }
                } else if toks.get(1).is_some_and(|t| t.kind() == SyntaxKind::IDENT)
                    && !matches!(w(1).as_str(), "EXCEPTION")
                {
                    if let Some(k) = self.prof.condition_kinds(&w(1)).first() {
                        kind = *k;
                    }
                }
                Action::Raise {
                    kind,
                    form: "raise",
                }
            }
        }
    }
}

fn has_on_conflict(node: &SyntaxNode) -> bool {
    let words: Vec<String> = ast::sig_tokens(node)
        .map(|t| t.text().to_ascii_uppercase())
        .collect();
    words.windows(2).any(|w| w[0] == "ON" && w[1] == "CONFLICT")
}

/// Routine name of `EXEC p`, `EXEC @rc = p`, `CALL p(...)`, `PERFORM f(...)`.
fn call_name(n: &SyntaxNode) -> Option<String> {
    let toks: Vec<SyntaxToken> = ast::sig_tokens(n).collect();
    let mut i = 1;
    if toks
        .get(1)
        .is_some_and(|t| t.kind() == SyntaxKind::VARIABLE)
        && toks.get(2).is_some_and(|t| t.text() == "=")
    {
        i = 3;
    }
    if toks.get(i).is_some_and(|t| {
        t.kind() == SyntaxKind::L_PAREN
            || t.kind() == SyntaxKind::STRING
            || t.text().eq_ignore_ascii_case("IMMEDIATE")
    }) {
        return None;
    }
    let parts = ast::qualified_name_at(&toks, i);
    parts.last().cloned()
}
