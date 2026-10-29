//! Binding (design §3 layer 2): resolves every name in an artifact to what it
//! refers to — local declarations, catalog objects, built-ins or pack
//! helpers — and runs the deterministic early checks of layer 5 that need
//! only binding: referenced columns exist, call signatures match, built-ins
//! and types are available, no shadowing or unresolved names, declaration
//! order.
//!
//! Invariant: every reference either resolves, fails with an issue (a proven
//! binding error), or becomes an assumption.

mod compare;
mod doubt;
mod query;

use std::collections::HashMap;

use sqldiff_syntax::{ast, Dialect, SyntaxKind, SyntaxNode};

use crate::artifact::Analyzed;
use crate::catalog::builtins::{self, Helper};
use crate::catalog::ddl::{self, OutputCol};
use crate::catalog::{Catalog, CatalogView, DataType, Ident, RoutineKind, TableKind};
use crate::model::{Assumption, ChangeClass, Severity};
use crate::tokens::{self, close_paren, split_commas, Tok};

pub use compare::{binding_findings, Bindings};

/// What a binder needs besides the artifact.
pub struct BindInput<'a> {
    pub view: &'a CatalogView,
    pub helpers: &'a [Helper],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefKind {
    Variable,
    Column,
    Table,
    Routine,
    Function,
    Label,
    Cursor,
    Condition,
    Type,
}

impl RefKind {
    pub fn label(self) -> &'static str {
        match self {
            RefKind::Variable => "variable",
            RefKind::Column => "column",
            RefKind::Table => "table",
            RefKind::Routine => "routine",
            RefKind::Function => "function",
            RefKind::Label => "label",
            RefKind::Cursor => "cursor",
            RefKind::Condition => "condition",
            RefKind::Type => "type",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A declaration in the artifact (index into `Bound::decls`).
    Decl(usize),
    Column {
        table: String,
        column: String,
        data_type: DataType,
        nullable: bool,
    },
    Table(String),
    Routine(String),
    Builtin,
    Helper,
    /// Implicit names: `FOUND`, `SQLSTATE`, trigger rows, cursor rows.
    Implicit,
    /// A select-list alias (`ORDER BY total`).
    OutputAlias,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Resolved(Target),
    /// A proven binding error; index into `Bound::issues`.
    Failed(usize),
    /// Could not be checked; id of the assumption it rests on.
    Assumed(String),
}

#[derive(Debug, Clone)]
pub struct Ref {
    pub kind: RefKind,
    /// The name as written, parts joined with `.`.
    pub name: String,
    pub start: usize,
    pub end: usize,
    /// Significant range of the enclosing statement.
    pub stmt: (usize, usize),
    pub resolution: Resolution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclKind {
    Variable,
    Parameter,
    Cursor,
    Condition,
    /// Loop variable or row of a `FOR` loop.
    LoopVar,
}

#[derive(Debug, Clone)]
pub struct Decl {
    pub kind: DeclKind,
    pub name: String,
    pub key: String,
    pub start: usize,
    pub end: usize,
    pub scope: usize,
    /// Position among declarations with the same key in the enclosing routine
    /// (the artifact outside routines), so unrelated routines do not shift it.
    pub ordinal: usize,
    /// Key of the enclosing routine, if any.
    pub routine: Option<String>,
    pub data_type: Option<DataType>,
    /// Field names of a row (Db2 `FOR l AS SELECT`, PL/pgSQL `%ROWTYPE`, records).
    pub row: Option<Vec<OutputCol>>,
}

/// A binding finding before it becomes a report finding.
#[derive(Debug, Clone)]
pub struct Issue {
    pub rule: &'static str,
    pub class: ChangeClass,
    pub severity: Severity,
    /// The construct fails (or behaves as stated) on every execution that
    /// reaches it, according to the catalog and dialect rules.
    pub proven: bool,
    pub message: String,
    pub start: usize,
    pub end: usize,
    /// Location-independent key for old-vs-new comparison and fingerprints.
    pub key: String,
    pub assumptions: Vec<String>,
}

/// Result of binding one artifact.
#[derive(Debug, Clone, Default)]
pub struct Bound {
    pub refs: Vec<Ref>,
    pub decls: Vec<Decl>,
    pub issues: Vec<Issue>,
    pub assumptions: Vec<Assumption>,
}

impl Bound {
    pub fn assumption(&self, id: &str) -> Option<&Assumption> {
        self.assumptions.iter().find(|a| a.id == id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
    File,
    Batch,
    Routine,
    Block,
    Loop,
    Handler,
}

#[derive(Debug, Clone)]
struct Scope {
    id: usize,
    kind: ScopeKind,
    label: Option<String>,
    /// The label as written, for messages.
    label_text: Option<String>,
    decls: Vec<usize>,
}

/// Trigger context: pseudo tables and rows.
#[derive(Debug, Clone, Default)]
struct TriggerCtx {
    table: Vec<Ident>,
    /// (alias key, is a table) for Db2 REFERENCING.
    referencing: Vec<(String, bool)>,
    /// PostgreSQL trigger function (`RETURNS trigger`).
    pg_function: bool,
}

pub(crate) struct Binder<'a> {
    d: Dialect,
    version: String,
    text: &'a str,
    lines: &'a sqldiff_syntax::LineIndex,
    input: &'a BindInput<'a>,
    out: Bound,
    scopes: Vec<Scope>,
    next_scope: usize,
    /// Objects the artifact creates: temp tables, table variables, routines.
    local: Catalog,
    ordinals: HashMap<String, usize>,
    trigger: Option<TriggerCtx>,
    /// `#variable_conflict` directive in a PL/pgSQL body.
    pg_conflict_directive: bool,
    /// T-SQL GOTO labels of the current batch.
    goto_labels: Vec<String>,
    /// Every routine the artifact creates, wherever it is: a call inside a
    /// routine body is resolved when the body runs, possibly after a routine
    /// defined further down was created.
    later: Catalog,
}

/// Binds one parsed artifact.
pub fn bind(a: &Analyzed, input: &BindInput) -> Bound {
    let mut b = Binder {
        d: a.dialect(),
        version: crate::target::engine_version(a.dialect(), &a.artifact.version),
        text: &a.artifact.text,
        lines: &a.lines,
        input,
        out: Bound::default(),
        scopes: Vec::new(),
        next_scope: 0,
        local: Catalog::default(),
        ordinals: HashMap::new(),
        trigger: None,
        pg_conflict_directive: a.artifact.text.contains("#variable_conflict"),
        goto_labels: Vec::new(),
        later: Catalog::default(),
    };
    for r in a
        .root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ROUTINE)
    {
        if let Some(h) = ddl::routine_header(&r, b.d) {
            if h.view.is_none() && h.routine.kind != RoutineKind::Trigger {
                b.later.put_routine(b.d, h.routine);
            }
        }
    }
    b.push_scope(ScopeKind::File, None);
    b.walk_children(&a.root);
    b.degrade(&doubt::doubts(a));
    b.out
}

/// Whether an issue was demoted because the binder could not read its input.
pub(crate) fn is_degraded(i: &Issue) -> bool {
    i.assumptions.iter().any(|a| doubt::is_doubt(a))
}

impl<'a> Binder<'a> {
    /// Demotes proven issues inside doubtful regions to assumptions, and the
    /// references that failed with them.
    fn degrade(&mut self, doubts: &[doubt::Doubt]) {
        let mut demoted: HashMap<usize, String> = HashMap::new();
        for (idx, i) in self.out.issues.iter_mut().enumerate() {
            if !i.proven {
                continue;
            }
            let Some(dt) = doubts
                .iter()
                .find(|dt| dt.start <= i.start && i.start < dt.end.max(dt.start + 1))
            else {
                continue;
            };
            i.proven = false;
            i.message.push_str(&dt.note);
            i.assumptions.push(dt.id.clone());
            demoted.insert(idx, dt.id.clone());
        }
        for dt in doubts {
            if demoted.values().any(|id| *id == dt.id) {
                self.assume(dt.id.clone(), dt.text.clone());
            }
        }
        for r in &mut self.out.refs {
            if let Resolution::Failed(i) = r.resolution {
                if let Some(id) = demoted.get(&i) {
                    r.resolution = Resolution::Assumed(id.clone());
                }
            }
        }
    }

    /// Whether the walk is inside a routine body.
    fn in_routine(&self) -> bool {
        self.scopes.iter().any(|s| s.kind == ScopeKind::Routine)
    }

    // ---- scopes and declarations -------------------------------------------

    fn push_scope(&mut self, kind: ScopeKind, label: Option<String>) {
        self.push_scope_text(kind, label, None);
    }

    fn push_scope_text(
        &mut self,
        kind: ScopeKind,
        label: Option<String>,
        label_text: Option<String>,
    ) {
        self.scopes.push(Scope {
            id: self.next_scope,
            kind,
            label,
            label_text,
            decls: Vec::new(),
        });
        self.next_scope += 1;
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn key_of(&self, name: &str, quoted: bool) -> String {
        Ident::new(name, quoted).key(self.d)
    }

    fn line(&self, offset: usize) -> usize {
        self.lines.line_col(offset.min(self.text.len())).0
    }

    /// Declares a name in the innermost scope; reports duplicates and shadowing.
    #[allow(clippy::too_many_arguments)]
    fn declare(
        &mut self,
        kind: DeclKind,
        name: &str,
        quoted: bool,
        start: usize,
        end: usize,
        data_type: Option<DataType>,
        row: Option<Vec<OutputCol>>,
    ) -> usize {
        let key = self.key_of(name, quoted);
        let variable_like = |k: DeclKind| {
            matches!(
                k,
                DeclKind::Variable | DeclKind::Parameter | DeclKind::LoopVar
            )
        };
        let scope = self.scopes.last().expect("scope").clone();
        let same =
            scope.decls.iter().copied().find(|&i| {
                self.out.decls[i].key == key && same_space(self.out.decls[i].kind, kind)
            });
        if let Some(prev) = same {
            let p = &self.out.decls[prev];
            let msg = format!(
                "`{name}` is declared twice in the same scope (line {} and line {}); the {} rejects the second declaration.",
                self.line(p.start),
                self.line(start),
                self.d.display_name()
            );
            self.issue(Issue {
                rule: "bind.duplicate-declaration",
                class: ChangeClass::Binding,
                severity: Severity::High,
                proven: true,
                message: msg,
                start,
                end,
                key: format!("duplicate {key}"),
                assumptions: Vec::new(),
            });
        } else if variable_like(kind) {
            let outer = self.scopes[..self.scopes.len() - 1]
                .iter()
                .rev()
                .flat_map(|s| s.decls.iter().rev().copied())
                .find(|&i| self.out.decls[i].key == key && variable_like(self.out.decls[i].kind));
            if let Some(o) = outer {
                let od = &self.out.decls[o];
                let where_ = self
                    .scopes
                    .last()
                    .and_then(|s| s.label_text.clone().or_else(|| s.label.clone()))
                    .map(|l| format!("block `{l}`"))
                    .unwrap_or_else(|| "the inner scope".to_string());
                let msg = format!(
                    "`{name}` declared on line {} shadows the {} `{}` declared on line {}; inside {where_} every reference reaches the inner one.",
                    self.line(start),
                    if od.kind == DeclKind::Parameter { "parameter" } else { "variable" },
                    od.name,
                    self.line(od.start),
                );
                self.issue(Issue {
                    rule: "bind.shadowed-variable",
                    class: ChangeClass::Binding,
                    severity: Severity::Medium,
                    proven: false,
                    message: msg,
                    start,
                    end,
                    key: format!("shadow {key}"),
                    assumptions: Vec::new(),
                });
            }
        }
        let routine = self
            .scopes
            .iter()
            .rev()
            .find(|s| s.kind == ScopeKind::Routine)
            .and_then(|s| s.label.clone());
        let ordinal = {
            let n = self.ordinals.entry(key.clone()).or_insert(0);
            *n += 1;
            *n - 1
        };
        let idx = self.out.decls.len();
        self.out.decls.push(Decl {
            kind,
            name: name.to_string(),
            key,
            start,
            end,
            scope: scope.id,
            ordinal,
            routine,
            data_type,
            row,
        });
        self.scopes.last_mut().expect("scope").decls.push(idx);
        idx
    }

    /// Innermost visible declaration of `key` among the given kinds.
    fn lookup(&self, key: &str, kinds: &[DeclKind]) -> Option<usize> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|s| s.decls.iter().rev().copied())
            .find(|&i| self.out.decls[i].key == key && kinds.contains(&self.out.decls[i].kind))
    }

    /// Variable `key` declared in the scope labelled `label` (PL/pgSQL
    /// `fn.param`, `lbl.var`; Db2 `lbl.var`).
    fn lookup_qualified(&self, label: &str, key: &str) -> Option<usize> {
        let s = self
            .scopes
            .iter()
            .rev()
            .find(|s| s.label.as_deref() == Some(label))?;
        s.decls.iter().copied().find(|&i| {
            self.out.decls[i].key == key
                && matches!(
                    self.out.decls[i].kind,
                    DeclKind::Variable | DeclKind::Parameter | DeclKind::LoopVar
                )
        })
    }

    fn has_label(&self, key: &str) -> bool {
        self.scopes.iter().any(|s| s.label.as_deref() == Some(key))
    }

    fn issue(&mut self, i: Issue) -> usize {
        self.out.issues.push(i);
        self.out.issues.len() - 1
    }

    fn assume(&mut self, id: String, text: String) -> String {
        if self.out.assumption(&id).is_none() {
            self.out.assumptions.push(Assumption::default(&id, &text));
        }
        id
    }

    fn add_ref(
        &mut self,
        kind: RefKind,
        name: String,
        span: (usize, usize),
        stmt: (usize, usize),
        resolution: Resolution,
    ) {
        self.out.refs.push(Ref {
            kind,
            name,
            start: span.0,
            end: span.1,
            stmt,
            resolution,
        });
    }

    /// Records a proven failure as an issue plus a failed reference.
    #[allow(clippy::too_many_arguments)]
    fn fail(
        &mut self,
        kind: RefKind,
        name: String,
        span: (usize, usize),
        stmt: (usize, usize),
        rule: &'static str,
        message: String,
        key: String,
        assumptions: Vec<String>,
    ) {
        let i = self.issue(Issue {
            rule,
            class: ChangeClass::Binding,
            severity: Severity::High,
            proven: true,
            message,
            start: span.0,
            end: span.1,
            key,
            assumptions,
        });
        self.add_ref(kind, name, span, stmt, Resolution::Failed(i));
    }

    fn builtins(&self) -> &'static builtins::Builtins {
        builtins::of(self.d)
    }

    fn is_keyword(&self, w: &str) -> bool {
        self.builtins().is_keyword(w)
    }

    // ---- tree walk ---------------------------------------------------------

    fn walk_children(&mut self, node: &SyntaxNode) {
        for c in node.children() {
            self.walk(&c);
        }
    }

    fn walk(&mut self, node: &SyntaxNode) {
        match node.kind() {
            SyntaxKind::BATCH => {
                self.push_scope(ScopeKind::Batch, None);
                self.goto_labels = goto_labels(node, self.d);
                self.walk_children(node);
                self.pop_scope();
            }
            SyntaxKind::ROUTINE => self.routine(node),
            SyntaxKind::BLOCK => self.block(node),
            SyntaxKind::DECLARE_SECTION => {
                for c in node.children() {
                    if c.kind() == SyntaxKind::STATEMENT {
                        self.pg_declaration(&c);
                    } else {
                        self.walk(&c);
                    }
                }
            }
            SyntaxKind::LOOP_STMT => self.loop_stmt(node),
            SyntaxKind::HANDLER => self.db2_handler(node),
            SyntaxKind::EXCEPTION_HANDLER => {
                let scoped = self.d != Dialect::Tsql;
                if scoped {
                    self.push_scope(ScopeKind::Handler, None);
                }
                for c in node.children() {
                    if c.kind() != SyntaxKind::CONDITION {
                        self.walk(&c);
                    }
                }
                if scoped {
                    self.pop_scope();
                }
            }
            SyntaxKind::CONDITION => {
                let stmt = ast::sig_range(&node.parent().unwrap_or_else(|| node.clone()));
                let t = tokens::of_node(node);
                self.analyze(&t, stmt);
            }
            SyntaxKind::STATEMENT => self.statement(node),
            SyntaxKind::LABEL => {}
            _ => self.walk_children(node),
        }
    }

    fn routine(&mut self, node: &SyntaxNode) {
        let header = ddl::routine_header(node, self.d);
        let label = header
            .as_ref()
            .and_then(|h| h.name.last())
            .map(|n| n.key(self.d));
        self.push_scope(ScopeKind::Routine, label);
        let saved_ordinals = std::mem::take(&mut self.ordinals);
        let saved_trigger = self.trigger.take();
        if let Some(h) = &header {
            if h.view.is_none() {
                let stmt = ast::sig_range(node);
                for (p, span) in h.routine.params.iter().zip(&h.param_spans) {
                    self.check_type(&p.data_type, *span, stmt);
                    if !p.name.is_empty() {
                        let quoted = self.text[span.0..span.1].starts_with('"');
                        self.declare(
                            DeclKind::Parameter,
                            &p.name,
                            quoted,
                            span.0,
                            span.1,
                            Some(p.data_type.clone()),
                            None,
                        );
                    }
                }
                if h.routine.kind != RoutineKind::Trigger {
                    self.local.put_routine(self.d, h.routine.clone());
                }
            }
            if let Some(t) = &h.trigger {
                self.trigger = Some(TriggerCtx {
                    table: t.table.clone(),
                    referencing: t
                        .referencing
                        .iter()
                        .map(|(w, a)| (a.key(self.d), w.ends_with("TABLE")))
                        .collect(),
                    pg_function: false,
                });
            }
            let pg_trigger_fn = self.d == Dialect::Postgres
                && h.routine
                    .returns
                    .as_ref()
                    .is_some_and(|r| r.base == "TRIGGER");
            if pg_trigger_fn {
                // The transition tables of the triggers that run this
                // function, when they all name the same table and tables.
                let key = |v: &[Ident]| v.iter().map(|p| p.key(self.d)).collect::<Vec<_>>();
                let fname = key(&h.name);
                let root = node.ancestors().last().unwrap_or_else(|| node.clone());
                let mut seen: Vec<ddl::TriggerInfo> = Vec::new();
                for st in root
                    .descendants()
                    .filter(|n| n.kind() == SyntaxKind::STATEMENT)
                {
                    let t = tokens::of_node(&st);
                    if let Some((f, info)) = ddl::pg_trigger_statement(&t) {
                        let f = key(&f);
                        if f.last() == fname.last()
                            && (f.len() == 1 || fname.len() == 1 || f == fname)
                        {
                            seen.push(info);
                        }
                    }
                }
                let agreed = seen.first().filter(|first| {
                    seen.iter().all(|x| {
                        key(&x.table) == key(&first.table)
                            && x.referencing.len() == first.referencing.len()
                            && x.referencing
                                .iter()
                                .zip(&first.referencing)
                                .all(|(a, b)| a.0 == b.0 && a.1.key(self.d) == b.1.key(self.d))
                    })
                });
                self.trigger = Some(match agreed {
                    Some(info) => TriggerCtx {
                        table: info.table.clone(),
                        referencing: info
                            .referencing
                            .iter()
                            .map(|(w, a)| (a.key(self.d), w.ends_with("TABLE")))
                            .collect(),
                        pg_function: true,
                    },
                    None => TriggerCtx {
                        pg_function: true,
                        ..TriggerCtx::default()
                    },
                });
            }
        }
        // Temporary tables are resolved when the statement runs, so a table
        // created later in the routine (or in a handler declared earlier) counts.
        for st in node
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::STATEMENT)
        {
            let w = ast::first_words(&st, 4);
            let creates = (w.first().map(String::as_str) == Some("CREATE")
                && w.iter().any(|x| x == "TABLE"))
                || (w.first().map(String::as_str) == Some("DECLARE")
                    && w.get(1).map(String::as_str) == Some("GLOBAL"));
            if creates {
                let t = tokens::of_node(&st);
                if let Some(mut tb) = ddl::create_table(&t, self.d) {
                    if self.d == Dialect::Db2i
                        && w.get(1).map(String::as_str) == Some("GLOBAL")
                        && tb.schema.is_none()
                    {
                        tb.schema = Some("SESSION".to_string());
                    }
                    self.local.put_table(self.d, tb);
                }
            }
        }
        let saved_labels = std::mem::take(&mut self.goto_labels);
        if self.d == Dialect::Tsql {
            self.goto_labels = goto_labels(node, self.d);
        }
        for c in node.children() {
            if c.kind() == SyntaxKind::ROUTINE_BODY {
                self.walk_children(&c);
            }
        }
        self.goto_labels = saved_labels;
        self.trigger = saved_trigger;
        self.ordinals = saved_ordinals;
        self.pop_scope();
    }

    fn block(&mut self, node: &SyntaxNode) {
        // T-SQL variables live for the whole batch; BEGIN ... END is not a scope.
        if self.d == Dialect::Tsql {
            self.walk_children(node);
            return;
        }
        let text = node
            .children()
            .find(|c| c.kind() == SyntaxKind::LABEL)
            .and_then(|l| label_name(&l));
        let label = text.as_ref().map(|n| self.key_of(n, false));
        self.push_scope_text(ScopeKind::Block, label, text);
        // Db2 declaration order: variables and conditions, cursors, handlers, statements.
        let mut rank_seen = 0u8;
        for c in node.children() {
            if self.d == Dialect::Db2i {
                let rank = db2_rank(&c);
                if let Some(r) = rank {
                    if r < rank_seen {
                        let (s, e) = ast::sig_range(&c);
                        let what = match r {
                            0 => "variable or condition declaration",
                            1 => "cursor declaration",
                            _ => "handler declaration",
                        };
                        let msg = format!(
                            "The {what} on line {} follows a later kind of declaration or a statement; Db2 for i requires variables and conditions, then cursors, then handlers, then statements.",
                            self.line(s)
                        );
                        let key = format!("declaration-order {}", snippet(&tokens::of_node(&c), 6));
                        self.issue(Issue {
                            rule: "bind.declaration-order",
                            class: ChangeClass::Binding,
                            severity: Severity::High,
                            proven: true,
                            message: msg,
                            start: s,
                            end: e,
                            key,
                            assumptions: Vec::new(),
                        });
                    }
                    rank_seen = rank_seen.max(r);
                }
            }
            self.walk(&c);
        }
        self.pop_scope();
    }

    fn db2_handler(&mut self, node: &SyntaxNode) {
        let stmt = ast::sig_range(node);
        if let Some(cond) = node.children().find(|c| c.kind() == SyntaxKind::CONDITION) {
            let t = tokens::of_node(&cond);
            let mut i = 0;
            while i < t.len() {
                let x = &t[i];
                if x.is_kw("SQLSTATE") {
                    i += 1;
                    while t
                        .get(i)
                        .is_some_and(|y| y.is_kw("VALUE") || y.is(SyntaxKind::STRING))
                    {
                        i += 1;
                    }
                    continue;
                }
                if x.is_any_kw(&["SQLEXCEPTION", "SQLWARNING", "NOT", "FOUND"]) || !x.is_name() {
                    i += 1;
                    continue;
                }
                self.condition_ref(x, stmt);
                i += 1;
            }
        }
        self.push_scope(ScopeKind::Handler, None);
        for c in node.children() {
            if c.kind() != SyntaxKind::CONDITION {
                self.walk(&c);
            }
        }
        self.pop_scope();
    }

    fn condition_ref(&mut self, x: &Tok, stmt: (usize, usize)) {
        let key = x.ident().key(self.d);
        match self.lookup(&key, &[DeclKind::Condition]) {
            Some(i) => self.add_ref(
                RefKind::Condition,
                x.text.clone(),
                (x.start, x.end),
                stmt,
                Resolution::Resolved(Target::Decl(i)),
            ),
            None => self.fail(
                RefKind::Condition,
                x.text.clone(),
                (x.start, x.end),
                stmt,
                "bind.unknown-condition",
                format!(
                    "Condition `{}` is not declared in an enclosing compound statement.",
                    x.text
                ),
                format!("condition {key}"),
                Vec::new(),
            ),
        }
    }

    fn loop_stmt(&mut self, node: &SyntaxNode) {
        let label = node
            .children()
            .find(|c| c.kind() == SyntaxKind::LABEL)
            .and_then(|l| label_name(&l))
            .map(|n| self.key_of(&n, false));
        let stmt = ast::sig_range(node);
        let head = tokens::header(node);
        let is_for = head
            .iter()
            .find(|t| !t.is(SyntaxKind::COLON) && !t.is_op("<<") && !t.is_op(">>"))
            .is_some_and(|t| t.is_any_kw(&["FOR", "FOREACH"]))
            || head.get(2).is_some_and(|t| t.is_kw("FOR"));
        let foreach = head.iter().any(|t| t.is_kw("FOREACH"));
        // T-SQL variables live for the whole batch: a loop is not a scope.
        let scoped = self.d != Dialect::Tsql;
        if scoped {
            self.push_scope(ScopeKind::Loop, label);
        }
        let cond = node.children().find(|c| c.kind() == SyntaxKind::CONDITION);
        match (is_for, cond) {
            (true, Some(c)) => {
                let t = tokens::of_node(&c);
                match self.d {
                    Dialect::Db2i => self.db2_for(&t, stmt),
                    Dialect::Postgres => self.pg_for(&t, stmt, foreach),
                    Dialect::Tsql => self.analyze(&t, stmt),
                }
            }
            (false, Some(c)) => {
                let t = tokens::of_node(&c);
                self.analyze(&t, stmt);
            }
            _ => {}
        }
        for c in node.children() {
            if c.kind() == SyntaxKind::BRANCH {
                self.walk(&c);
            }
        }
        if scoped {
            self.pop_scope();
        }
    }

    /// Db2 `FOR l AS [c CURSOR [WITH HOLD] FOR] SELECT ...`: `l` names the row,
    /// and its columns are also visible unqualified in the body.
    fn db2_for(&mut self, t: &[Tok], stmt: (usize, usize)) {
        let Some(name) = t.first().filter(|x| x.is_name()) else {
            self.analyze(t, stmt);
            return;
        };
        let q = t
            .iter()
            .position(|x| x.is_any_kw(&["SELECT", "WITH", "VALUES"]))
            .unwrap_or(t.len());
        let query = &t[q..];
        self.analyze(query, stmt);
        let outs = ddl::select_outputs(query);
        if let Some(c) = t
            .iter()
            .position(|x| x.is_kw("CURSOR"))
            .and_then(|i| i.checked_sub(1))
            .map(|i| &t[i])
        {
            self.declare(
                DeclKind::Cursor,
                &c.ident().text,
                c.kind == SyntaxKind::QUOTED_IDENT,
                c.start,
                c.end,
                None,
                None,
            );
        }
        let row = self.typed_outputs(outs, query);
        self.declare(
            DeclKind::LoopVar,
            &name.ident().text,
            name.kind == SyntaxKind::QUOTED_IDENT,
            name.start,
            name.end,
            None,
            row,
        );
    }

    /// PL/pgSQL `FOR i IN [REVERSE] a..b`, `FOR r IN query`, `FOREACH x IN ARRAY e`.
    fn pg_for(&mut self, t: &[Tok], stmt: (usize, usize), foreach: bool) {
        let Some(in_at) = t.iter().position(|x| x.is_kw("IN")) else {
            self.analyze(t, stmt);
            return;
        };
        let targets = &t[..in_at];
        let rest = &t[in_at + 1..];
        if foreach || t.first().is_some_and(|x| x.is_kw("FOREACH")) {
            // FOREACH assigns to declared variables: `FOREACH x [SLICE n] IN ARRAY e`.
            self.analyze(rest, stmt);
            let end = targets
                .iter()
                .position(|x| x.is_kw("SLICE"))
                .unwrap_or(targets.len());
            for tg in targets[..end]
                .iter()
                .filter(|x| x.is_name() && !x.is_kw("FOREACH"))
            {
                self.loop_target(tg, stmt, None, false);
            }
            return;
        }
        let is_query = rest
            .iter()
            .find(|x| !x.is_kw("REVERSE"))
            .is_some_and(|x| x.is_any_kw(&["SELECT", "WITH", "VALUES", "EXECUTE"]));
        if is_query {
            self.analyze(rest, stmt);
            let outs = ddl::select_outputs(rest);
            let row = self.typed_outputs(outs, rest);
            // The targets must be declared; a record takes the query's shape.
            let single = targets.len() == 1;
            for tg in targets.iter().filter(|x| x.is_name()) {
                self.loop_target(tg, stmt, row.clone(), single);
            }
        } else {
            self.analyze(rest, stmt);
            // Integer loop: the variable is declared by the loop.
            if let Some(tg) = targets.iter().find(|x| x.is_name()) {
                self.declare(
                    DeclKind::LoopVar,
                    &tg.ident().text,
                    tg.kind == SyntaxKind::QUOTED_IDENT,
                    tg.start,
                    tg.end,
                    Some(DataType::parse_str("integer")),
                    None,
                );
            }
        }
    }

    /// A loop target that must already be declared; a lone record target takes the row shape.
    fn loop_target(
        &mut self,
        tg: &Tok,
        stmt: (usize, usize),
        row: Option<Vec<OutputCol>>,
        single: bool,
    ) {
        let key = tg.ident().key(self.d);
        match self.lookup(
            &key,
            &[DeclKind::Variable, DeclKind::Parameter, DeclKind::LoopVar],
        ) {
            Some(i) => {
                if single && self.out.decls[i].row.is_none() {
                    self.out.decls[i].row = row;
                }
                self.add_ref(
                    RefKind::Variable,
                    tg.text.clone(),
                    (tg.start, tg.end),
                    stmt,
                    Resolution::Resolved(Target::Decl(i)),
                );
            }
            None => self.unknown_variable(tg, stmt),
        }
    }

    // ---- statements --------------------------------------------------------

    fn statement(&mut self, node: &SyntaxNode) {
        let mut t = tokens::of_node(node);
        while t.last().is_some_and(|x| x.is(SyntaxKind::SEMICOLON)) {
            t.pop();
        }
        let stmt = ast::sig_range(node);
        let Some(first) = t.first() else { return };
        let head = first.upper();
        let second = t.get(1).map(Tok::upper).unwrap_or_default();
        match (self.d, head.as_str()) {
            (_, "CREATE") => self.create_statement(&t, stmt),
            // ALTER TABLE on a table the artifact created (temp tables) changes its columns;
            // persistent tables come from the catalog, which reflects the ALTER already.
            (_, "ALTER") => {
                if second == "TABLE" {
                    ddl::apply_statement(&mut self.local, &t, self.d);
                }
            }
            (_, "DROP" | "GRANT" | "REVOKE" | "COMMENT" | "USE" | "DENY") => {}
            (_, "BEGIN" | "COMMIT" | "SAVE" | "SAVEPOINT" | "RELEASE") => {
                // Transaction names are not references; T-SQL may name them with a variable.
                self.analyze_vars_only(&t, stmt);
            }
            (_, "ROLLBACK") => self.analyze_vars_only(&t, stmt),
            (Dialect::Tsql, "DECLARE") => self.tsql_declare(&t, stmt),
            (Dialect::Db2i, "DECLARE") => self.db2_declare(&t, stmt),
            (Dialect::Postgres, "DECLARE") => {}
            // `SET [SESSION | LOCAL] name {= | TO} value`, `SET ROLE`, `SET TIME ZONE`, ...:
            // configuration names and literal values, never variables or columns
            // (PL/pgSQL does not substitute variables into utility commands).
            (Dialect::Postgres, "SET") => {}
            (Dialect::Tsql, "SET") if !t.get(1).is_some_and(|x| x.is(SyntaxKind::VARIABLE)) => {}
            (Dialect::Db2i, "SET") if !self.db2_set_targets_variables(&t) => {}
            (_, "EXEC" | "EXECUTE") if self.d == Dialect::Tsql => self.tsql_exec(&t, stmt),
            // `INSERT INTO t (cols) EXEC proc ...`: the rows come from a procedure call.
            (Dialect::Tsql, "INSERT")
                if tokens::find_kw(&t, 1, "EXEC")
                    .or_else(|| tokens::find_kw(&t, 1, "EXECUTE"))
                    .is_some() =>
            {
                let e = tokens::find_kw(&t, 1, "EXEC")
                    .into_iter()
                    .chain(tokens::find_kw(&t, 1, "EXECUTE"))
                    .min()
                    .unwrap_or(t.len());
                self.analyze(&t[..e], stmt);
                self.tsql_exec(&t[e..], stmt);
            }
            (_, "CALL") => self.call_statement(&t, stmt),
            (Dialect::Tsql, "GOTO") => {
                if let Some(l) = t.get(1) {
                    let key = self.key_of(&l.text, false);
                    if self.goto_labels.contains(&key) {
                        self.add_ref(
                            RefKind::Label,
                            l.text.clone(),
                            (l.start, l.end),
                            stmt,
                            Resolution::Resolved(Target::Implicit),
                        );
                    } else {
                        self.fail(
                            RefKind::Label,
                            l.text.clone(),
                            (l.start, l.end),
                            stmt,
                            "bind.unknown-label",
                            format!("GOTO target `{}` is not a label in this batch.", l.text),
                            format!("label {key}"),
                            Vec::new(),
                        );
                    }
                }
            }
            (Dialect::Db2i, "LEAVE" | "ITERATE") | (Dialect::Postgres, "EXIT" | "CONTINUE") => {
                let mut rest = 1;
                if let Some(l) = t.get(1).filter(|x| x.is_name() && !x.is_kw("WHEN")) {
                    rest = 2;
                    let key = self.key_of(&l.ident().text, l.kind == SyntaxKind::QUOTED_IDENT);
                    if self.has_label(&key) {
                        self.add_ref(
                            RefKind::Label,
                            l.text.clone(),
                            (l.start, l.end),
                            stmt,
                            Resolution::Resolved(Target::Implicit),
                        );
                    } else {
                        self.fail(
                            RefKind::Label,
                            l.text.clone(),
                            (l.start, l.end),
                            stmt,
                            "bind.unknown-label",
                            format!("`{head} {}` names no enclosing labelled statement.", l.text),
                            format!("label {key}"),
                            Vec::new(),
                        );
                    }
                }
                if let Some(w) = t.iter().position(|x| x.is_kw("WHEN")) {
                    self.analyze(&t[w + 1..], stmt);
                }
                let _ = rest;
            }
            (_, "OPEN" | "CLOSE" | "DEALLOCATE" | "FETCH" | "MOVE") => {
                self.cursor_statement(&t, stmt)
            }
            (Dialect::Db2i, "SIGNAL" | "RESIGNAL") => {
                if let Some(c) = t.get(1).filter(|x| x.is_name() && !x.is_kw("SQLSTATE")) {
                    let c = c.clone();
                    self.condition_ref(&c, stmt);
                }
                if let Some(s) = t.iter().position(|x| x.is_kw("SET")) {
                    self.analyze_assignments(&t[s + 1..], stmt, true);
                }
            }
            (Dialect::Postgres, "RAISE") => {
                // RAISE [level] 'format', args [USING option = expr, ...]
                let fmt = t.iter().position(|x| x.is(SyntaxKind::STRING));
                let using = t.iter().position(|x| x.is_kw("USING"));
                if let Some(f) = fmt {
                    let end = using.unwrap_or(t.len());
                    if f + 1 < end {
                        self.analyze(&t[f + 1..end], stmt);
                    }
                }
                if let Some(u) = using {
                    self.analyze_assignments(&t[u + 1..], stmt, true);
                }
            }
            (_, "GET") => {
                // GET [CURRENT|STACKED] DIAGNOSTICS [EXCEPTION n] target = item, ...
                if let Some(dg) = t.iter().position(|x| x.is_kw("DIAGNOSTICS")) {
                    let mut k = dg + 1;
                    if t.get(k).is_some_and(|x| x.is_kw("EXCEPTION")) {
                        k += 2;
                    }
                    for item in split_commas(&t[k..]) {
                        if let Some(eq) = item
                            .iter()
                            .position(|x| x.is_op("=") || x.is(SyntaxKind::COLON))
                        {
                            self.analyze(&item[..eq], stmt);
                        }
                    }
                }
            }
            (Dialect::Postgres, _) if is_pg_assignment(&t) => {
                let eq = t
                    .iter()
                    .position(|x| x.is_op("=") || x.is_op(":="))
                    .unwrap_or(0);
                self.analyze(&t[..eq], stmt);
                self.analyze(&t[eq + 1..], stmt);
            }
            (Dialect::Postgres, "PERFORM") => self.analyze(&t[1..], stmt),
            (Dialect::Postgres, "EXECUTE") | (Dialect::Db2i, "EXECUTE" | "PREPARE") => {
                // Dynamic SQL: the string is opaque; INTO targets and USING arguments are references.
                let from = if head == "PREPARE" { 2 } else { 1 };
                if head == "EXECUTE" && second == "IMMEDIATE" {
                    self.analyze(&t[2..], stmt);
                } else if from < t.len() {
                    let skip_name = self.d == Dialect::Db2i && head == "EXECUTE";
                    self.analyze(&t[from + usize::from(skip_name)..], stmt);
                }
            }
            (Dialect::Postgres, "RETURN") if second == "QUERY" || second == "NEXT" => {
                self.analyze(&t[2..], stmt)
            }
            (Dialect::Postgres, "NULL" | "ASSERT") => {
                if head == "ASSERT" {
                    self.analyze(&t[1..], stmt);
                }
            }
            (
                Dialect::Tsql,
                "THROW" | "RAISERROR" | "PRINT" | "RETURN" | "WAITFOR" | "BREAK" | "CONTINUE",
            ) => self.analyze(&t[1..], stmt),
            (
                Dialect::Postgres,
                "LISTEN" | "NOTIFY" | "SHOW" | "RESET" | "DISCARD" | "ANALYZE" | "VACUUM" | "LOCK"
                | "TRUNCATE",
            ) => {
                if head == "TRUNCATE" || head == "LOCK" {
                    self.analyze(&t, stmt);
                }
            }
            // Statements that carry expressions. Anything else (`LABEL ON`, `RENAME`,
            // vendor commands the binder does not model) names objects or is
            // unclassified, and reading its words as references would guess a
            // `different`; it gets no binding findings.
            (
                _,
                "SELECT" | "WITH" | "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "VALUES" | "SET"
                | "RETURN" | "(",
            ) => self.analyze(&t, stmt),
            _ => {}
        }
    }

    fn create_statement(&mut self, t: &[Tok], stmt: (usize, usize)) {
        let table_at = t.iter().take(6).position(|x| x.is_kw("TABLE"));
        let view_at = t.iter().take(5).position(|x| x.is_kw("VIEW"));
        if let Some(_at) = table_at {
            if let Some(tb) = ddl::create_table(t, self.d) {
                for c in &tb.columns {
                    if c.data_type.is_known() {
                        let span = t
                            .iter()
                            .find(|x| x.is_name() && x.ident().stored(self.d) == c.name)
                            .map(|x| (x.start, x.end))
                            .unwrap_or(stmt);
                        self.check_type(&c.data_type, span, stmt);
                    }
                }
                // `CREATE TABLE t AS SELECT ...` (Db2: `AS (SELECT ...) WITH DATA`)
                // reads its query; `AS` inside the column list (`GENERATED
                // ALWAYS AS IDENTITY (START WITH 1)`) does not start one.
                if let Some(q) = create_table_query(t) {
                    let (from, to) = q;
                    self.analyze(&t[from..to], stmt);
                }
                self.local.put_table(self.d, tb);
            }
        } else if let Some(v) = view_at {
            if let Some(as_at) = tokens::find_kw(t, v + 1, "AS") {
                self.analyze(&t[as_at + 1..], stmt);
            }
            ddl::apply_statement(&mut self.local, t, self.d);
        }
    }

    /// Only T-SQL variables in a statement whose other names are not references.
    fn analyze_vars_only(&mut self, t: &[Tok], stmt: (usize, usize)) {
        for x in t.iter().filter(|x| x.is(SyntaxKind::VARIABLE)) {
            let x = x.clone();
            self.variable_token(&x, stmt);
        }
    }

    /// `name = expr, ...` where `name` is an option word, not a reference.
    fn analyze_assignments(&mut self, t: &[Tok], stmt: (usize, usize), skip_targets: bool) {
        for item in split_commas(t) {
            match item.iter().position(|x| x.is_op("=")) {
                Some(eq) => {
                    if !skip_targets {
                        self.analyze(&item[..eq], stmt);
                    }
                    self.analyze(&item[eq + 1..], stmt);
                }
                None => self.analyze(item, stmt),
            }
        }
    }

    fn tsql_declare(&mut self, t: &[Tok], stmt: (usize, usize)) {
        // DECLARE c CURSOR ... FOR select
        if t.get(1).is_some_and(|x| x.is_name()) && t.iter().any(|x| x.is_kw("CURSOR")) {
            if let Some(f) = tokens::find_kw(t, 2, "FOR") {
                self.analyze(&t[f + 1..], stmt);
            }
            let c = &t[1];
            self.declare(
                DeclKind::Cursor,
                &c.ident().text,
                false,
                c.start,
                c.end,
                None,
                None,
            );
            return;
        }
        for item in split_commas(&t[1..]) {
            let Some(v) = item.first().filter(|x| x.is(SyntaxKind::VARIABLE)) else {
                continue;
            };
            let mut k = 1;
            if item.get(k).is_some_and(|x| x.is_kw("AS")) {
                k += 1;
            }
            if item.get(k).is_some_and(|x| x.is_kw("TABLE")) {
                // Table variable.
                let mut tb = crate::catalog::Table::new(None, v.text.clone(), TableKind::Variable);
                if let Some(open) = item.iter().position(|x| x.is(SyntaxKind::L_PAREN)) {
                    let c = close_paren(item, open);
                    ddl::column_defs(&mut tb, &item[open + 1..c], self.d);
                }
                self.local.put_table(self.d, tb);
                self.declare(
                    DeclKind::Variable,
                    &v.text,
                    false,
                    v.start,
                    v.end,
                    None,
                    None,
                );
                continue;
            }
            if item.get(k).is_some_and(|x| x.is_kw("CURSOR")) {
                self.declare(DeclKind::Cursor, &v.text, false, v.start, v.end, None, None);
                continue;
            }
            let eq = item.iter().position(|x| x.is_op("="));
            let ty_end = eq.unwrap_or(item.len());
            let ty = DataType::parse(&tokens::texts(&item[k..ty_end]));
            self.check_type(&ty, (v.start, v.end), stmt);
            if let Some(e) = eq {
                self.analyze(&item[e + 1..], stmt);
            }
            self.declare(
                DeclKind::Variable,
                &v.text,
                false,
                v.start,
                v.end,
                Some(ty),
                None,
            );
        }
    }

    fn db2_declare(&mut self, t: &[Tok], stmt: (usize, usize)) {
        if t.get(1).is_some_and(|x| x.is_kw("GLOBAL")) {
            if let Some(tb) = ddl::create_table(t, self.d) {
                let mut tb = tb;
                if tb.schema.is_none() {
                    tb.schema = Some("SESSION".to_string());
                }
                self.local.put_table(self.d, tb);
            }
            // `AS (fullselect) WITH DATA`: the query is the one after a
            // top-level AS, not `START WITH` in a column or `WITH REPLACE`.
            let mut depth = 0usize;
            let mut as_at = None;
            for (i, x) in t.iter().enumerate() {
                match x.kind {
                    SyntaxKind::L_PAREN => depth += 1,
                    SyntaxKind::R_PAREN => depth = depth.saturating_sub(1),
                    _ if depth == 0
                        && x.is_kw("AS")
                        && t.get(i + 1).is_some_and(|y| y.kind == SyntaxKind::L_PAREN) =>
                    {
                        as_at = Some(i);
                        break;
                    }
                    _ => {}
                }
            }
            if let Some(q) =
                as_at.and_then(|a| (a + 2..t.len()).find(|&j| t[j].is_any_kw(&["SELECT", "WITH"])))
            {
                self.analyze(&t[q..], stmt);
            }
            return;
        }
        // Names up to the type.
        let mut names = Vec::new();
        let mut k = 1;
        while let Some(x) = t.get(k) {
            if !x.is_name() {
                break;
            }
            names.push(x.clone());
            k += 1;
            if t.get(k).is_some_and(|y| y.is(SyntaxKind::COMMA)) {
                k += 1;
            } else {
                break;
            }
        }
        let rest = &t[k..];
        if rest.first().is_some_and(|x| x.is_kw("CONDITION")) {
            for n in &names {
                self.declare(
                    DeclKind::Condition,
                    &n.ident().text,
                    n.kind == SyntaxKind::QUOTED_IDENT,
                    n.start,
                    n.end,
                    None,
                    None,
                );
            }
            return;
        }
        let cursor = rest.iter().take(4).any(|x| x.is_kw("CURSOR"));
        if cursor {
            if let Some(f) = tokens::find_kw(rest, 0, "FOR") {
                let q = &rest[f + 1..];
                if q.first()
                    .is_some_and(|x| x.is_any_kw(&["SELECT", "WITH", "VALUES"]))
                {
                    self.analyze(q, stmt);
                }
            }
            for n in &names {
                self.declare(
                    DeclKind::Cursor,
                    &n.ident().text,
                    n.kind == SyntaxKind::QUOTED_IDENT,
                    n.start,
                    n.end,
                    None,
                    None,
                );
            }
            return;
        }
        if rest.first().is_some_and(|x| x.is_kw("STATEMENT")) {
            return;
        }
        let def = rest.iter().position(|x| x.is_kw("DEFAULT"));
        let ty_end = ddl::type_end(rest, 0).min(def.unwrap_or(rest.len()));
        let ty = DataType::parse(&tokens::texts(&rest[..ty_end]));
        if let Some(n) = names.first() {
            self.check_type(&ty, (n.start, n.end), stmt);
        }
        if let Some(dp) = def {
            self.analyze(&rest[dp + 1..], stmt);
        }
        for n in &names {
            self.declare(
                DeclKind::Variable,
                &n.ident().text,
                n.kind == SyntaxKind::QUOTED_IDENT,
                n.start,
                n.end,
                Some(ty.clone()),
                None,
            );
        }
    }

    /// One declaration of a PL/pgSQL DECLARE section.
    fn pg_declaration(&mut self, node: &SyntaxNode) {
        let t = tokens::of_node(node);
        let stmt = ast::sig_range(node);
        let Some(n) = t.first().filter(|x| x.is_name()) else {
            return;
        };
        let n = n.clone();
        let quoted = n.kind == SyntaxKind::QUOTED_IDENT;
        let name = n.ident().text;
        let mut t = t;
        while t.last().is_some_and(|x| x.is(SyntaxKind::SEMICOLON)) {
            t.pop();
        }
        let mut k = 1;
        if t.get(k).is_some_and(|x| x.is_kw("CONSTANT")) {
            k += 1;
        }
        let rest = &t[k..];
        // name ALIAS FOR $1 | param
        if rest.first().is_some_and(|x| x.is_kw("ALIAS")) {
            self.analyze(rest.get(2..).unwrap_or(&[]), stmt);
            self.declare(
                DeclKind::Variable,
                &name,
                quoted,
                n.start,
                n.end,
                None,
                None,
            );
            return;
        }
        // name [[NO] SCROLL] CURSOR [(args)] FOR|IS query
        if rest.iter().take(3).any(|x| x.is_kw("CURSOR")) {
            if let Some(f) = rest.iter().position(|x| x.is_any_kw(&["FOR", "IS"])) {
                self.analyze(&rest[f + 1..], stmt);
            }
            self.declare(DeclKind::Cursor, &name, quoted, n.start, n.end, None, None);
            // A bound cursor is also usable as a refcursor variable.
            return;
        }
        let init = rest.iter().position(|x| {
            x.is_kw("DEFAULT") || x.is_op("=") || x.is_op(":=") || x.is(SyntaxKind::COLON)
        });
        let ty_end = init.unwrap_or(rest.len());
        let ty_toks: Vec<&Tok> = rest[..ty_end]
            .iter()
            .take_while(|x| !x.is_any_kw(&["COLLATE", "NOT", "NULL", "DEFAULT"]))
            .collect();
        let (ty, row) = self.pg_decl_type(&ty_toks, stmt);
        if let Some(i) = init {
            let mut e = i + 1;
            if rest.get(e).is_some_and(|x| x.is_op("=")) {
                e += 1;
            }
            self.analyze(&rest[e.min(rest.len())..], stmt);
        }
        self.declare(DeclKind::Variable, &name, quoted, n.start, n.end, ty, row);
    }

    /// Type of a PL/pgSQL declaration, resolving `%TYPE` and `%ROWTYPE`.
    fn pg_decl_type(
        &mut self,
        ty: &[&Tok],
        stmt: (usize, usize),
    ) -> (Option<DataType>, Option<Vec<OutputCol>>) {
        let pct = ty.iter().position(|x| x.is_op("%"));
        if let Some(p) = pct {
            let owned: Vec<Tok> = ty[..p].iter().map(|x| (*x).clone()).collect();
            let (parts, _) = tokens::qualified(&owned, 0);
            let rowtype = ty.get(p + 1).is_some_and(|x| x.is_kw("ROWTYPE"));
            let span = (
                owned.first().map(|x| x.start).unwrap_or(stmt.0),
                owned.last().map(|x| x.end).unwrap_or(stmt.1),
            );
            if rowtype {
                let row = self.table_ref(&parts, span, stmt).map(|tb| {
                    tb.columns
                        .iter()
                        .map(|c| OutputCol {
                            name: Some(Ident::new(c.name.clone(), true)),
                            source: None,
                        })
                        .collect()
                });
                return (None, row);
            }
            if parts.len() >= 2 {
                let (tparts, col) = parts.split_at(parts.len() - 1);
                if let Some(tb) = self.table_ref(tparts, span, stmt) {
                    let found = tb.column(self.d, &col[0]).cloned();
                    let display = tb.display();
                    return match found {
                        Some(c) => (Some(c.data_type), None),
                        None => {
                            self.unknown_column_in(&col[0].text, &display, span, stmt);
                            (None, None)
                        }
                    };
                }
                return (None, None);
            }
            // var%TYPE
            if let Some(v) = parts.first() {
                let k = v.key(self.d);
                if let Some(i) = self.lookup(&k, &[DeclKind::Variable, DeclKind::Parameter]) {
                    return (self.out.decls[i].data_type.clone(), None);
                }
            }
            return (None, None);
        }
        let words: Vec<&str> = ty.iter().map(|x| x.text.as_str()).collect();
        let dt = DataType::parse(&words);
        if dt.base == "RECORD" {
            return (Some(dt), None);
        }
        // A table name used as a type is its row type.
        if dt.is_known()
            && self.builtins().data_type(&dt.base, &self.version) == builtins::Availability::Unknown
        {
            let owned: Vec<Tok> = ty.iter().map(|x| (*x).clone()).collect();
            let (parts, after) = tokens::qualified(&owned, 0);
            if after == owned.len() {
                let view = self.input.view;
                let lk = self.lookup_table_quiet(&parts, view);
                if let Some(cols) = lk {
                    return (None, Some(cols));
                }
            }
        }
        let span = (
            ty.first().map(|x| x.start).unwrap_or(stmt.0),
            ty.last().map(|x| x.end).unwrap_or(stmt.1),
        );
        self.check_type(&dt, span, stmt);
        (Some(dt), None)
    }

    fn lookup_table_quiet(&self, parts: &[Ident], view: &CatalogView) -> Option<Vec<OutputCol>> {
        let local = CatalogView::new(self.d, &[&self.local]);
        let found = match local.table(parts) {
            crate::catalog::Lookup::Found(t) => Some(t.clone()),
            _ => match view.table(parts) {
                crate::catalog::Lookup::Found(t) => Some(t.clone()),
                _ => None,
            },
        }?;
        Some(
            found
                .columns
                .iter()
                .map(|c| OutputCol {
                    name: Some(Ident::new(c.name.clone(), true)),
                    source: None,
                })
                .collect(),
        )
    }

    /// Db2 `SET target = expr`: true when the statement assigns variables
    /// (not `SET SCHEMA`, `SET PATH`, `SET TRANSACTION`, ...).
    fn db2_set_targets_variables(&self, t: &[Tok]) -> bool {
        match t.get(1) {
            Some(x) if x.is(SyntaxKind::L_PAREN) => true,
            Some(x) if x.is_name() => {
                if x.is_any_kw(&[
                    "CURRENT",
                    "SCHEMA",
                    "PATH",
                    "TRANSACTION",
                    "SESSION",
                    "ENCRYPTION",
                    "OPTION",
                    "CONNECTION",
                    "SQLID",
                ]) {
                    let k = x.ident().key(self.d);
                    return self
                        .lookup(&k, &[DeclKind::Variable, DeclKind::Parameter])
                        .is_some();
                }
                true
            }
            _ => false,
        }
    }

    /// T-SQL `EXEC[UTE] [@rc =] proc [args]`.
    fn tsql_exec(&mut self, t: &[Tok], stmt: (usize, usize)) {
        let mut k = 1;
        if t.get(k).is_some_and(|x| x.is(SyntaxKind::L_PAREN)) {
            // EXEC (@sql): dynamic.
            self.analyze(&t[k..], stmt);
            return;
        }
        if t.get(k).is_some_and(|x| x.is(SyntaxKind::VARIABLE))
            && t.get(k + 1).is_some_and(|x| x.is_op("="))
        {
            let v = t[k].clone();
            self.variable_token(&v, stmt);
            k += 2;
        }
        // `EXEC @proc ...`: a variable holding the procedure name is dynamic.
        if let Some(v) = t.get(k).filter(|x| x.is(SyntaxKind::VARIABLE)).cloned() {
            self.variable_token(&v, stmt);
            self.exec_args(&t[k + 1..], stmt);
            return;
        }
        let (parts, after) = tokens::qualified(t, k);
        if parts.is_empty() {
            self.analyze(t.get(k..).unwrap_or(&[]), stmt);
            return;
        }
        let name_span = (t[k].start, t[after - 1].end);
        let args = self.exec_args(&t[after..], stmt);
        self.resolve_call(&parts, name_span, stmt, &args, CallStyle::Exec);
    }

    /// T-SQL EXEC arguments: `@p = value [OUTPUT]` or `value [OUTPUT]`;
    /// a parameter name is not a variable of the batch.
    fn exec_args(&mut self, t: &[Tok], stmt: (usize, usize)) -> Vec<CallArg> {
        let mut args: Vec<CallArg> = Vec::new();
        // `WITH RECOMPILE`, `WITH RESULT SETS (...)`: options, not arguments.
        let end = t.iter().position(|x| x.is_kw("WITH")).unwrap_or(t.len());
        for item in split_commas(&t[..end]) {
            let item: Vec<Tok> = item
                .iter()
                .filter(|x| !x.is_any_kw(&["OUTPUT", "OUT", "READONLY"]))
                .cloned()
                .collect();
            if item.is_empty() {
                continue;
            }
            if item.len() >= 2 && item[0].is(SyntaxKind::VARIABLE) && item[1].is_op("=") {
                args.push(CallArg::Named(item[0].text.clone()));
                self.analyze(&item[2..], stmt);
            } else {
                args.push(CallArg::Positional);
                self.analyze(&item, stmt);
            }
        }
        args
    }

    /// `CALL proc(args)` in all dialects.
    fn call_statement(&mut self, t: &[Tok], stmt: (usize, usize)) {
        let (parts, after) = tokens::qualified(t, 1);
        if parts.is_empty() {
            return;
        }
        let name_span = (t[1].start, t[after - 1].end);
        let mut args = Vec::new();
        if t.get(after).is_some_and(|x| x.is(SyntaxKind::L_PAREN)) {
            let c = close_paren(t, after);
            args = self.call_args(&t[after + 1..c], stmt);
        }
        self.resolve_call(&parts, name_span, stmt, &args, CallStyle::Call);
    }

    /// Splits and binds parenthesised call arguments.
    fn call_args(&mut self, inner: &[Tok], stmt: (usize, usize)) -> Vec<CallArg> {
        let mut args = Vec::new();
        for item in split_commas(inner) {
            if item.is_empty() {
                continue;
            }
            let named = item.len() >= 2
                && item[0].is_name()
                && (item[1].is_op("=>")
                    || item[1].is_op(":=")
                    || (item[1].is(SyntaxKind::COLON)
                        && item.get(2).is_some_and(|x| x.is_op("="))));
            if named {
                args.push(CallArg::Named(item[0].ident().text));
                let skip = if item[1].is(SyntaxKind::COLON) { 3 } else { 2 };
                self.analyze(&item[skip.min(item.len())..], stmt);
            } else {
                args.push(CallArg::Positional);
                self.analyze(item, stmt);
            }
        }
        args
    }

    fn cursor_statement(&mut self, t: &[Tok], stmt: (usize, usize)) {
        let head = t[0].upper();
        let mut k = 1;
        // FETCH [NEXT|PRIOR|FIRST|LAST|ABSOLUTE n|RELATIVE n|FORWARD|BACKWARD] [FROM|IN] c
        while let Some(x) = t.get(k) {
            if x.is_any_kw(&[
                "NEXT", "PRIOR", "FIRST", "LAST", "FORWARD", "BACKWARD", "ALL", "FROM", "IN",
                "GLOBAL",
            ]) {
                k += 1;
            } else if x.is_any_kw(&["ABSOLUTE", "RELATIVE"]) {
                k += 2;
            } else if x.is(SyntaxKind::NUMBER) {
                k += 1;
            } else {
                break;
            }
        }
        let Some(c) = t.get(k).cloned() else { return };
        if c.is(SyntaxKind::VARIABLE) {
            self.variable_token(&c, stmt);
        } else if c.is_name() {
            let key = c.ident().key(self.d);
            match self.lookup(
                &key,
                &[DeclKind::Cursor, DeclKind::Variable, DeclKind::Parameter],
            ) {
                Some(i) => self.add_ref(
                    RefKind::Cursor,
                    c.text.clone(),
                    (c.start, c.end),
                    stmt,
                    Resolution::Resolved(Target::Decl(i)),
                ),
                None if self.d == Dialect::Tsql => {
                    let id = self.assume(
                        format!("ASM-BIND-CURSOR:{key}"),
                        format!(
                            "Cursor `{}` is a global cursor opened by the caller",
                            c.text
                        ),
                    );
                    self.add_ref(
                        RefKind::Cursor,
                        c.text.clone(),
                        (c.start, c.end),
                        stmt,
                        Resolution::Assumed(id),
                    );
                }
                None => self.fail(
                    RefKind::Cursor,
                    c.text.clone(),
                    (c.start, c.end),
                    stmt,
                    "bind.unknown-cursor",
                    format!(
                        "`{head} {}`: cursor `{}` is not declared in scope.",
                        c.text, c.text
                    ),
                    format!("cursor {key}"),
                    Vec::new(),
                ),
            }
        }
        let rest = &t[(k + 1).min(t.len())..];
        // OPEN c FOR query | USING args; FETCH c INTO targets.
        if !rest.is_empty() {
            let from = rest
                .iter()
                .position(|x| x.is_any_kw(&["INTO", "FOR", "USING"]))
                .map(|p| p + 1)
                .unwrap_or(rest.len());
            if from < rest.len() {
                self.analyze(&rest[from..], stmt);
            }
        }
    }

    fn unknown_variable(&mut self, x: &Tok, stmt: (usize, usize)) {
        let key = x.ident().key(self.d);
        let msg = match self.d {
            Dialect::Tsql => format!("`{}` is not declared in this batch before its use; SQL Server fails with \"Must declare the scalar variable\".", x.text),
            Dialect::Postgres => format!("`{}` is not a variable, parameter or column in scope; the statement fails with undefined_column (42703) when it runs.", x.text),
            Dialect::Db2i => format!("`{}` is not a variable, parameter or column in scope; Db2 for i rejects the statement (SQL0206).", x.text),
        };
        self.fail(
            RefKind::Variable,
            x.text.clone(),
            (x.start, x.end),
            stmt,
            "bind.unknown-variable",
            msg,
            format!("variable {key}"),
            Vec::new(),
        );
    }

    fn variable_token(&mut self, x: &Tok, stmt: (usize, usize)) {
        if x.text.starts_with("@@") {
            return;
        }
        let key = self.key_of(&x.text, false);
        match self.lookup(
            &key,
            &[DeclKind::Variable, DeclKind::Parameter, DeclKind::Cursor],
        ) {
            Some(i) => self.add_ref(
                RefKind::Variable,
                x.text.clone(),
                (x.start, x.end),
                stmt,
                Resolution::Resolved(Target::Decl(i)),
            ),
            None => self.unknown_variable(x, stmt),
        }
    }

    /// Checks that a declared type exists in the dialect at the target version.
    fn check_type(&mut self, ty: &DataType, span: (usize, usize), stmt: (usize, usize)) {
        if !ty.is_known() || ty.text.contains('%') {
            return;
        }
        let base = ty.base.clone();
        match self.builtins().data_type(&base, &self.version) {
            builtins::Availability::Available => self.add_ref(
                RefKind::Type,
                ty.text.clone(),
                span,
                stmt,
                Resolution::Resolved(Target::Builtin),
            ),
            builtins::Availability::Since(v) => {
                let msg = format!(
                    "Type `{}` needs {} {v} or later; the target is {}.",
                    ty.text,
                    self.d.display_name(),
                    self.version
                );
                self.fail(
                    RefKind::Type,
                    ty.text.clone(),
                    span,
                    stmt,
                    "bind.builtin-version",
                    msg,
                    format!("type {base}"),
                    Vec::new(),
                );
            }
            builtins::Availability::Unknown => {
                let elsewhere = builtins::type_elsewhere(&base, self.d);
                if !elsewhere.is_empty() {
                    let names: Vec<&str> = elsewhere.iter().map(|d| d.display_name()).collect();
                    let msg = format!(
                        "Type `{}` is a {} type; {} has no type of that name.",
                        ty.text,
                        names.join("/"),
                        self.d.display_name()
                    );
                    self.fail(
                        RefKind::Type,
                        ty.text.clone(),
                        span,
                        stmt,
                        "bind.unknown-type",
                        msg,
                        format!("type {base}"),
                        Vec::new(),
                    );
                } else {
                    let id = self.assume(
                        format!("ASM-BIND-TYPE:{}", base.to_lowercase()),
                        format!("Type `{}` is a user-defined type or domain that exists in the target database", ty.text),
                    );
                    self.add_ref(
                        RefKind::Type,
                        ty.text.clone(),
                        span,
                        stmt,
                        Resolution::Assumed(id),
                    );
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CallArg {
    Positional,
    Named(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallStyle {
    /// T-SQL EXEC: named arguments use `@p = v`; parameters with defaults may be omitted.
    Exec,
    /// `CALL p(args)`.
    Call,
    /// Function call in an expression.
    Function,
}

fn same_space(a: DeclKind, b: DeclKind) -> bool {
    let var = |k: DeclKind| {
        matches!(
            k,
            DeclKind::Variable | DeclKind::Parameter | DeclKind::LoopVar
        )
    };
    (var(a) && var(b)) || a == b
}

fn label_name(label: &SyntaxNode) -> Option<String> {
    tokens::of_node(label)
        .into_iter()
        .find(|t| t.kind == SyntaxKind::IDENT)
        .map(|t| t.text)
}

fn goto_labels(node: &SyntaxNode, d: Dialect) -> Vec<String> {
    node.descendants()
        .filter(|n| {
            // T-SQL has no labelled blocks or loops: every label is a GOTO target,
            // also the ones the parser attaches to the BEGIN ... END that follows.
            n.kind() == SyntaxKind::LABEL
                && (d == Dialect::Tsql
                    || n.parent().is_some_and(|p| {
                        p.kind() != SyntaxKind::BLOCK && p.kind() != SyntaxKind::LOOP_STMT
                    }))
        })
        .filter_map(|l| label_name(&l))
        .map(|n| Ident::plain(n).key(d))
        .collect()
}

/// Db2 compound-statement item rank for the declaration-order check.
fn db2_rank(n: &SyntaxNode) -> Option<u8> {
    match n.kind() {
        SyntaxKind::HANDLER => Some(2),
        SyntaxKind::LABEL => None,
        SyntaxKind::STATEMENT => {
            let w = ast::first_words(n, 4);
            if w.first().map(String::as_str) != Some("DECLARE") {
                return Some(3);
            }
            if w.iter().any(|x| x == "CURSOR") {
                Some(1)
            } else if w.get(1).map(String::as_str) == Some("GLOBAL") {
                Some(3)
            } else {
                Some(0)
            }
        }
        k if k.is_unit() => Some(3),
        _ => None,
    }
}

fn is_pg_assignment(t: &[Tok]) -> bool {
    // target [. field | [i]] := expr   (`:=` lexes as `:` `=`, or `=` alone)
    let (parts, after) = tokens::qualified(t, 0);
    if parts.is_empty() {
        return false;
    }
    let mut k = after;
    if t.get(k).is_some_and(|x| x.is(SyntaxKind::L_BRACKET)) {
        while k < t.len() && !t[k].is(SyntaxKind::R_BRACKET) {
            k += 1;
        }
        k += 1;
    }
    match t.get(k) {
        Some(x) if x.is_op(":=") || x.is_op("=") => true,
        Some(x) if x.is(SyntaxKind::COLON) => t.get(k + 1).is_some_and(|y| y.is_op("=")),
        _ => false,
    }
}

pub(crate) fn snippet(t: &[Tok], max: usize) -> String {
    let words: Vec<String> = t.iter().take(max).map(|x| x.text.clone()).collect();
    crate::artifact::snippet_of(&words, 80)
}

/// Token range of the query in `CREATE TABLE ... AS query`: a top-level `AS`
/// followed by `SELECT` / `WITH`, or by a parenthesized query.
fn create_table_query(t: &[Tok]) -> Option<(usize, usize)> {
    let mut depth = 0usize;
    for (i, x) in t.iter().enumerate() {
        if x.is(SyntaxKind::L_PAREN) {
            depth += 1;
        } else if x.is(SyntaxKind::R_PAREN) {
            depth = depth.saturating_sub(1);
        } else if depth == 0 && x.is_kw("AS") {
            match t.get(i + 1) {
                Some(n) if n.is_any_kw(&["SELECT", "WITH"]) => return Some((i + 1, t.len())),
                Some(n)
                    if n.is(SyntaxKind::L_PAREN)
                        && t.get(i + 2)
                            .is_some_and(|y| y.is_any_kw(&["SELECT", "WITH"])) =>
                {
                    return Some((i + 2, close_paren(t, i + 1)));
                }
                _ => {}
            }
        }
    }
    None
}
