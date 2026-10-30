//! The abstract syntax shared by the diff engine and the converter: typed
//! statement and expression trees parsed from the flat statement tokens of
//! the CST (docs/verified-conversion.md, C0 item 1). The converter emits it
//! and `print` writes it back as text; a proof is about these trees.
//!
//! The CST stops at statements, so this module parses the significant tokens
//! of one statement into a small typed tree: expressions (with precedence),
//! SELECT, INSERT, UPDATE, DELETE, MERGE, assignments, RETURN and DECLARE.
//! Anything outside the supported subset is an error, and the caller keeps
//! the statement as opaque tokens (which can only compare equal to the same
//! tokens in the same dialect).

use crate::{ast as cst, Dialect, SyntaxKind, SyntaxNode, SyntaxToken};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum K {
    Word,
    Quoted,
    Var,
    Param,
    Num,
    Str,
    Op,
    Comma,
    Dot,
    LParen,
    RParen,
    Semi,
    Other,
}

/// A significant token: kind, text, upper-cased text and byte offset.
#[derive(Debug, Clone)]
pub struct Tok {
    pub kind: K,
    pub text: String,
    pub up: String,
    pub off: usize,
}

impl Tok {
    pub fn from_syntax(t: &SyntaxToken) -> Tok {
        let kind = match t.kind() {
            SyntaxKind::IDENT => K::Word,
            SyntaxKind::QUOTED_IDENT => K::Quoted,
            SyntaxKind::VARIABLE => K::Var,
            SyntaxKind::PARAM => K::Param,
            SyntaxKind::NUMBER => K::Num,
            SyntaxKind::STRING => K::Str,
            SyntaxKind::OP => K::Op,
            SyntaxKind::COMMA => K::Comma,
            SyntaxKind::DOT => K::Dot,
            SyntaxKind::L_PAREN => K::LParen,
            SyntaxKind::R_PAREN => K::RParen,
            SyntaxKind::SEMICOLON => K::Semi,
            _ => K::Other,
        };
        let text = t.text().to_string();
        Tok {
            kind,
            up: text.to_uppercase(),
            text,
            off: t.text_range().start().into(),
        }
    }
}

/// Significant tokens of a node, terminators dropped.
pub fn tokens(node: &SyntaxNode) -> Vec<Tok> {
    cst::sig_tokens(node)
        .map(|t| Tok::from_syntax(&t))
        .filter(|t| t.kind != K::Semi)
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// Column or unresolved name: qualified parts as written.
    Name(Vec<String>),
    /// T-SQL `@x` / `@@x`.
    Var(String),
    Num(String),
    /// String literal value (quotes removed, `''` unescaped).
    Str(String),
    Null,
    Star(Option<String>),
    Call(Box<Call>),
    Bin(String, Box<Expr>, Box<Expr>),
    Unary(String, Box<Expr>),
    IsNull(Box<Expr>, bool),
    /// `e [NOT] IN (list)`.
    InList(Box<Expr>, Vec<Expr>, bool),
    InQuery(Box<Expr>, Box<Select>, bool),
    Between(Box<Expr>, Box<Expr>, Box<Expr>, bool),
    Like(Box<Expr>, Box<Expr>, bool, bool),
    Cast(Box<Expr>, Type),
    Case(Option<Box<Expr>>, Vec<(Expr, Expr)>, Option<Box<Expr>>),
    Exists(Box<Select>),
    Subquery(Box<Select>),
    Tuple(Vec<Expr>),
    /// Db2 `CURRENT TIMESTAMP` and friends, T-SQL `CURRENT_TIMESTAMP` without parens.
    Special(String),
    /// `EXTRACT(field FROM e)`.
    Extract(String, Box<Expr>),
}

/// The parameters, in order, and their defaults of the PostgreSQL built-ins
/// whose named arguments are read: `make_interval` (PostgreSQL 18 manual,
/// 9.9 Date/Time Functions: `make_interval ( [ years int [, months int
/// [, weeks int [, days int [, hours int [, mins int [, secs double
/// precision ]]]]]]] )`, each defaulting to zero).
fn named_params(name: &[String]) -> Option<&'static [(&'static str, &'static str)]> {
    let builtin = match name {
        [f] => f,
        [s, f] if s.eq_ignore_ascii_case("pg_catalog") => f,
        _ => return None,
    };
    builtin.eq_ignore_ascii_case("make_interval").then_some(&[
        ("years", "0"),
        ("months", "0"),
        ("weeks", "0"),
        ("days", "0"),
        ("hours", "0"),
        ("mins", "0"),
        ("secs", "0.0"),
    ])
}

#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub name: Vec<String>,
    pub args: Vec<Expr>,
    pub distinct: bool,
    /// `ORDER BY` inside the argument list or in `WITHIN GROUP (...)`.
    pub order: Vec<OrderItem>,
    pub within_group: bool,
    pub over: bool,
    pub star: bool,
}

/// A T-SQL `OPENJSON ... WITH` column's `'path'` and `AS JSON`.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonCol {
    pub path: Option<String>,
    pub as_json: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Type {
    pub name: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OrderItem {
    pub expr: Expr,
    pub desc: bool,
    /// Explicit `NULLS FIRST` (true) or `NULLS LAST` (false).
    pub nulls_first: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelItem {
    pub expr: Expr,
    pub alias: Option<String>,
    /// T-SQL `SELECT @v = expr`.
    pub assign: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum From {
    Table {
        name: Vec<String>,
        alias: Option<String>,
        hints: Vec<String>,
    },
    Sub {
        query: Box<Select>,
        alias: Option<String>,
        cols: Vec<String>,
    },
    Values {
        rows: Vec<Vec<Expr>>,
        alias: Option<String>,
        cols: Vec<String>,
    },
    /// A table-valued function call (`OPENJSON(@j) WITH (...)`); parsed
    /// only by the extended parser.
    Func {
        name: Vec<String>,
        args: Vec<Expr>,
        /// T-SQL `WITH (col type ['path'], ...)` result schema.
        schema: Vec<(String, Type)>,
        /// The path and `AS JSON` of each `schema` column. Not encoded.
        paths: Vec<JsonCol>,
        alias: Option<String>,
    },
    /// Db2 `FINAL TABLE (INSERT ...)` and friends.
    DataChange {
        kind: String,
        stmt: Box<Stmt>,
        alias: Option<String>,
    },
    Join {
        kind: String,
        left: Box<From>,
        right: Box<From>,
        on: Option<Expr>,
    },
}

/// A common table expression (extended parser only).
#[derive(Debug, Clone, PartialEq)]
pub struct Cte {
    pub name: String,
    pub cols: Vec<String>,
    pub query: Select,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Select {
    /// `WITH name AS (...)` (extended parser only).
    pub ctes: Vec<Cte>,
    /// T-SQL `FOR JSON ...` (extended parser only): the rows are returned as JSON text.
    pub for_json: bool,
    /// `UNION [ALL]` / `EXCEPT` / `INTERSECT` operands that follow (extended
    /// parser only); ORDER BY and paging of the select apply to the whole.
    pub set_ops: Vec<(String, Select)>,
    pub distinct: bool,
    pub top: Option<Expr>,
    pub items: Vec<SelItem>,
    /// PL/pgSQL / Db2 `SELECT ... INTO v`.
    pub into: Vec<Expr>,
    pub into_strict: bool,
    pub from: Vec<From>,
    pub where_: Option<Expr>,
    pub group_by: Vec<Expr>,
    pub having: Option<Expr>,
    pub order_by: Vec<OrderItem>,
    pub offset: Option<Expr>,
    pub limit: Option<Expr>,
    /// `FOR UPDATE`, `WITH RS USE AND KEEP EXCLUSIVE LOCKS`, ...
    pub locking: Vec<String>,
    /// A bare `VALUES (...)` query.
    pub values: Option<Vec<Vec<Expr>>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InsertSource {
    Values(Vec<Vec<Expr>>),
    Query(Box<Select>),
    Default,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConflictAction {
    Nothing,
    Update(Vec<(String, Expr)>, Option<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum MergeAction {
    Update(Vec<(String, Expr)>),
    Delete,
    Insert(Vec<String>, Vec<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct When {
    pub matched: bool,
    pub by_source: bool,
    pub cond: Option<Expr>,
    pub action: MergeAction,
}

impl Select {
    /// T-SQL `SELECT @v = expr` with no FROM, WHERE, grouping or paging: the
    /// query returns exactly one row, so the statement is `SET @v = expr`
    /// (SQL Server 2022: a scalar subquery in it gives NULL on no row, as in
    /// SET). The target and the value.
    pub fn bare_assign(&self) -> Option<(Expr, &Expr)> {
        let [item] = self.items.as_slice() else {
            return None;
        };
        let var = item.assign.as_ref()?;
        let bare = self.from.is_empty()
            && self.where_.is_none()
            && self.group_by.is_empty()
            && self.having.is_none()
            && self.top.is_none()
            && self.limit.is_none()
            && self.offset.is_none()
            && self.set_ops.is_empty()
            && self.ctes.is_empty()
            && self.values.is_none()
            && self.into.is_empty()
            && !self.for_json
            && !self.distinct;
        bare.then(|| (Expr::Var(var.clone()), &item.expr))
    }
}

impl Stmt {
    /// The target and value of an assignment: `SET`, `:=`, or a bare T-SQL
    /// `SELECT @v = expr` (see [`Select::bare_assign`]).
    pub fn assignment(&self) -> Option<(std::borrow::Cow<'_, Expr>, &Expr)> {
        match self {
            Stmt::Assign(t, v) => Some((std::borrow::Cow::Borrowed(t), v)),
            Stmt::Select(q) => q
                .bare_assign()
                .map(|(t, v)| (std::borrow::Cow::Owned(t), v)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Select(Box<Select>),
    Insert {
        target: Vec<String>,
        cols: Vec<String>,
        source: InsertSource,
        returning: Vec<SelItem>,
        returning_into: Vec<Expr>,
        on_conflict: Option<(Vec<String>, ConflictAction)>,
    },
    Update {
        target: Vec<String>,
        alias: Option<String>,
        /// Column lists with one value each (`SET (a, b) = (subquery)` keeps its list).
        sets: Vec<(Vec<String>, Expr)>,
        from: Vec<From>,
        where_: Option<Expr>,
    },
    Delete {
        target: Vec<String>,
        alias: Option<String>,
        from: Vec<From>,
        where_: Option<Expr>,
    },
    Merge {
        target: Vec<String>,
        alias: Option<String>,
        hints: Vec<String>,
        source: From,
        on: Expr,
        whens: Vec<When>,
    },
    /// `SET v = e`, `v := e`, `SET @v = e`.
    Assign(Expr, Expr),
    Return(Option<Expr>),
    /// `RETURN SELECT ...` / `RETURN (SELECT ...)` of a table function, or a
    /// query that is the body of a view or SQL function.
    Declare {
        name: String,
        ty: Option<Type>,
        default: Option<Expr>,
    },
    /// An informational message to the client: T-SQL `PRINT e`, PostgreSQL
    /// `RAISE NOTICE '%', e` (or a format string without placeholders).
    /// Only built by `lower` for scripts' statements.
    Message(Expr),
}

/// Words that end an expression or a name (clause keywords).
const RESERVED: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "GROUP",
    "HAVING",
    "ORDER",
    "BY",
    "UNION",
    "EXCEPT",
    "INTERSECT",
    "INTO",
    "ON",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "FULL",
    "OUTER",
    "CROSS",
    "AS",
    "AND",
    "OR",
    "NOT",
    "IS",
    "IN",
    "LIKE",
    "ILIKE",
    "BETWEEN",
    "THEN",
    "WHEN",
    "ELSE",
    "END",
    "SET",
    "VALUES",
    "OFFSET",
    "FETCH",
    "LIMIT",
    "FOR",
    "WITH",
    "USING",
    "RETURNING",
    "OUTPUT",
    "ASC",
    "DESC",
    "NULLS",
    "ROWS",
    "ROW",
    "ONLY",
    "MATCHED",
    "WITHIN",
    "OVER",
    "DISTINCT",
    "ALL",
    "TOP",
    "WINDOW",
    "ESCAPE",
    "CONFLICT",
    "DO",
    "COLLATE",
];

pub(crate) fn is_reserved(up: &str) -> bool {
    RESERVED.contains(&up)
}

pub type PResult<T> = Result<T, String>;

/// The rows a data change statement returns: T-SQL `OUTPUT inserted.x,
/// deleted.y [INTO t (cols)]`, PostgreSQL `UPDATE/DELETE … RETURNING …`
/// (no INTO), and PostgreSQL `WITH o AS (<dml> RETURNING …) INSERT INTO t
/// (cols) SELECT <every returned column> FROM o`, which is OUTPUT … INTO.
/// Read only by [`parse_stmt_output`]; the statement tree leaves it out.
/// Items are as written: which row image a name means (`inserted` or
/// `deleted`, `new` or `old`, a bare or table-qualified column) is the
/// reader's to resolve.
#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    pub items: Vec<SelItem>,
    /// The table the rows go to and its column list (empty when not given).
    pub into: Option<(Vec<String>, Vec<String>)>,
}

pub struct Parser<'a> {
    t: &'a [Tok],
    pub i: usize,
    pub d: Dialect,
    /// Extended syntax for the evaluator: CTEs, table functions, OUTPUT
    /// clauses, FOR JSON and FOR SYSTEM_TIME. The canonical lowering never
    /// sees these.
    pub ext: bool,
    /// What the extended parser read but left out of the tree (an `OUTPUT`
    /// clause, table hints on an `INSERT` target, the qualifier of a `SET`
    /// target, `FOR JSON` options). The evaluator compares the statement's
    /// tokens as well; [`parse_stmt_exact`] refuses any of these.
    pub dropped: Vec<&'static str>,
    /// Constructs only readers that lower them take (`parse_stmt_lowered`,
    /// [`parse_stmt_output`], [`Parser::lowering`]):
    /// UNION, EXCEPT and INTERSECT, and aggregates `OVER ()`.
    pub lowered: bool,
    /// Read OUTPUT / RETURNING into [`Parser::output`] instead of refusing
    /// them ([`parse_stmt_output`]).
    pub keep_output: bool,
    pub output: Option<Output>,
}

impl<'a> Parser<'a> {
    pub fn new(t: &'a [Tok], d: Dialect) -> Self {
        Parser {
            t,
            i: 0,
            d,
            ext: false,
            dropped: Vec::new(),
            lowered: false,
            keep_output: false,
            output: None,
        }
    }

    /// A parser for the equivalence lowering (see [`Parser::lowered`]).
    pub fn lowering(t: &'a [Tok], d: Dialect) -> Self {
        Parser {
            lowered: true,
            ..Parser::new(t, d)
        }
    }

    pub fn ext(t: &'a [Tok], d: Dialect) -> Self {
        Parser {
            t,
            i: 0,
            d,
            ext: true,
            dropped: Vec::new(),
            lowered: true,
            keep_output: false,
            output: None,
        }
    }

    pub fn at_end(&self) -> bool {
        self.i >= self.t.len()
    }

    /// How many tokens have been read.
    pub fn pos(&self) -> usize {
        self.i
    }

    pub(crate) fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }

    pub(crate) fn peek_at(&self, n: usize) -> Option<&Tok> {
        self.t.get(self.i + n)
    }

    pub(crate) fn is_kw(&self, kw: &str) -> bool {
        self.peek().is_some_and(|t| t.kind == K::Word && t.up == kw)
    }

    pub(crate) fn is_kw_at(&self, n: usize, kw: &str) -> bool {
        self.peek_at(n)
            .is_some_and(|t| t.kind == K::Word && t.up == kw)
    }

    pub(crate) fn eat_kw(&mut self, kw: &str) -> bool {
        if self.is_kw(kw) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    pub(crate) fn eat_kws(&mut self, kws: &[&str]) -> bool {
        if kws.iter().enumerate().all(|(n, k)| self.is_kw_at(n, k)) {
            self.i += kws.len();
            true
        } else {
            false
        }
    }

    pub(crate) fn expect_kw(&mut self, kw: &str) -> PResult<()> {
        if self.eat_kw(kw) {
            Ok(())
        } else {
            Err(self.err(&format!("expected {kw}")))
        }
    }

    pub(crate) fn is_kind(&self, k: K) -> bool {
        self.peek().is_some_and(|t| t.kind == k)
    }

    pub(crate) fn eat_kind(&mut self, k: K) -> bool {
        if self.is_kind(k) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    pub(crate) fn expect_kind(&mut self, k: K) -> PResult<()> {
        if self.eat_kind(k) {
            Ok(())
        } else {
            Err(self.err(&format!("expected {k:?}")))
        }
    }

    pub(crate) fn is_op(&self, op: &str) -> bool {
        self.peek().is_some_and(|t| t.kind == K::Op && t.text == op)
    }

    /// The operator of a T-SQL compound assignment (`+=` ...) at the cursor.
    fn compound_op(&self) -> Option<&'static str> {
        self.peek_compound(0)
    }

    fn peek_compound(&self, at: usize) -> Option<&'static str> {
        if self.d != Dialect::Tsql {
            return None;
        }
        let t = self.peek_at(at)?;
        if t.kind != K::Op {
            return None;
        }
        Some(match t.text.as_str() {
            "+=" => "+",
            "-=" => "-",
            "*=" => "*",
            "/=" => "/",
            "%=" => "%",
            _ => return None,
        })
    }

    pub(crate) fn eat_op(&mut self, op: &str) -> bool {
        if self.is_op(op) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    pub fn err(&self, msg: &str) -> String {
        match self.peek() {
            Some(t) => format!("{msg} at `{}`", t.text),
            None => format!("{msg} at end of statement"),
        }
    }

    /// An identifier that is not a clause keyword.
    pub(crate) fn ident(&mut self) -> PResult<String> {
        match self.peek() {
            Some(t) if t.kind == K::Word && !is_reserved(&t.up) => {
                let s = t.text.clone();
                self.i += 1;
                Ok(s)
            }
            Some(t) if t.kind == K::Quoted => {
                let s = quoted_name(&t.text, self.d);
                self.i += 1;
                Ok(s)
            }
            _ => Err(self.err("expected an identifier")),
        }
    }

    /// Any word or quoted identifier (also keywords), e.g. after a `.`.
    pub(crate) fn any_ident(&mut self) -> PResult<String> {
        match self.peek() {
            Some(t) if t.kind == K::Word => {
                let s = t.text.clone();
                self.i += 1;
                Ok(s)
            }
            Some(t) if t.kind == K::Quoted => {
                let s = quoted_name(&t.text, self.d);
                self.i += 1;
                Ok(s)
            }
            _ => Err(self.err("expected a name")),
        }
    }

    /// `a.b.c` or `lib/file`; T-SQL `#temp` / `@tablevar` are one part.
    pub fn qname(&mut self) -> PResult<Vec<String>> {
        let mut parts = Vec::new();
        if self.is_kind(K::Var) {
            parts.push(self.peek().unwrap().text.clone());
            self.i += 1;
            return Ok(parts);
        }
        if self.is_op("#") {
            self.i += 1;
            let n = self.any_ident()?;
            parts.push(format!("#{n}"));
            return Ok(parts);
        }
        // Template placeholders such as `@extschema@` stand for a schema name.
        if self.ext
            && self.is_op("@")
            && self.is_kind_at(1, K::Word)
            && self
                .peek_at(2)
                .is_some_and(|t| t.kind == K::Op && t.text == "@")
        {
            parts.push(format!("@{}@", self.peek_at(1).unwrap().text));
            self.i += 3;
        } else {
            parts.push(self.ident()?);
        }
        while self.is_kind(K::Dot) || (self.d == Dialect::Db2i && self.is_op("/")) {
            if self.is_kind(K::Dot)
                && self
                    .peek_at(1)
                    .is_some_and(|t| t.kind == K::Op && t.text == "*")
            {
                break;
            }
            self.i += 1;
            parts.push(self.any_ident()?);
        }
        Ok(parts)
    }

    fn alias(&mut self) -> PResult<Option<String>> {
        if self.eat_kw("AS") {
            return Ok(Some(self.any_ident()?));
        }
        // T-SQL's reserved OPTION starts the query hints, `OPTION (…)`.
        if self.d == Dialect::Tsql
            && self.is_kw("OPTION")
            && self.peek_at(1).is_some_and(|t| t.kind == K::LParen)
        {
            return Ok(None);
        }
        match self.peek() {
            Some(t)
                if (t.kind == K::Word && !is_reserved(&t.up) && !is_clause_start(&t.up))
                    || t.kind == K::Quoted =>
            {
                Ok(Some(self.ident()?))
            }
            _ => Ok(None),
        }
    }

    fn col_list(&mut self) -> PResult<Vec<String>> {
        self.expect_kind(K::LParen)?;
        let mut cols = Vec::new();
        loop {
            cols.push(self.any_ident()?);
            if !self.eat_kind(K::Comma) {
                break;
            }
        }
        self.expect_kind(K::RParen)?;
        Ok(cols)
    }

    // ---- types ---------------------------------------------------------

    pub fn ty(&mut self) -> PResult<Type> {
        let mut name = self.any_ident()?.to_uppercase();
        // Multi-word type names.
        loop {
            let next = self.peek().map(|t| t.up.clone()).unwrap_or_default();
            let joined = match (name.as_str(), next.as_str()) {
                ("DOUBLE", "PRECISION") | ("CHARACTER", "VARYING") | ("CHAR", "VARYING") => true,
                ("TIMESTAMP" | "TIME", "WITH" | "WITHOUT") => {
                    if self.is_kw_at(1, "TIME") && self.is_kw_at(2, "ZONE") {
                        name = format!("{name} {next} TIME ZONE");
                        self.i += 3;
                        continue;
                    }
                    false
                }
                _ => false,
            };
            if joined {
                name = format!("{name} {next}");
                self.i += 1;
            } else {
                break;
            }
        }
        let mut args = Vec::new();
        if self.eat_kind(K::LParen) {
            loop {
                match self.peek() {
                    Some(t) if matches!(t.kind, K::Num | K::Word) => {
                        let mut arg = t.up.clone();
                        let end = t.off + t.text.len();
                        let num = t.kind == K::Num;
                        self.i += 1;
                        // Db2 for i LOB lengths take a multiplier written
                        // against the number: `CLOB(1G)`, `BLOB(64K)`.
                        if self.d == Dialect::Db2i && num {
                            if let Some(m) = self.peek().filter(|m| {
                                m.kind == K::Word
                                    && m.off == end
                                    && matches!(m.up.as_str(), "K" | "M" | "G")
                            }) {
                                arg.push_str(&m.up);
                                self.i += 1;
                            }
                        }
                        args.push(arg);
                    }
                    _ => return Err(self.err("bad type argument")),
                }
                if !self.eat_kind(K::Comma) {
                    break;
                }
            }
            self.expect_kind(K::RParen)?;
        }
        Ok(Type { name, args })
    }

    // ---- expressions ---------------------------------------------------

    pub fn expr(&mut self) -> PResult<Expr> {
        self.or_expr()
    }

    fn or_expr(&mut self) -> PResult<Expr> {
        let mut l = self.and_expr()?;
        while self.eat_kw("OR") {
            let r = self.and_expr()?;
            l = Expr::Bin("OR".into(), Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn and_expr(&mut self) -> PResult<Expr> {
        let mut l = self.not_expr()?;
        while self.eat_kw("AND") {
            let r = self.not_expr()?;
            l = Expr::Bin("AND".into(), Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn not_expr(&mut self) -> PResult<Expr> {
        if self.is_kw("NOT") && !self.is_kw_at(1, "EXISTS") {
            self.i += 1;
            let e = self.not_expr()?;
            return Ok(Expr::Unary("NOT".into(), Box::new(e)));
        }
        self.pred()
    }

    fn pred(&mut self) -> PResult<Expr> {
        let l = self.other()?;
        if let Some(t) = self.peek() {
            if t.kind == K::Op
                && matches!(t.text.as_str(), "=" | "<>" | "!=" | "<" | ">" | "<=" | ">=")
            {
                let op = if t.text == "!=" {
                    "<>".to_string()
                } else {
                    t.text.clone()
                };
                self.i += 1;
                let r = self.other()?;
                return Ok(Expr::Bin(op, Box::new(l), Box::new(r)));
            }
        }
        if self.eat_kw("IS") {
            let neg = self.eat_kw("NOT");
            // x IS [NOT] TRUE / FALSE / UNKNOWN (extended parser): a call.
            if self.ext && (self.is_kw("TRUE") || self.is_kw("FALSE") || self.is_kw("UNKNOWN")) {
                let what = self.peek().unwrap().up.clone();
                self.i += 1;
                let name = format!("IS {}{what}", if neg { "NOT " } else { "" });
                return Ok(Expr::Call(Box::new(Call {
                    name: vec![name],
                    args: vec![l],
                    distinct: false,
                    order: Vec::new(),
                    within_group: false,
                    over: false,
                    star: false,
                })));
            }
            self.expect_kw("NULL")?;
            return Ok(Expr::IsNull(Box::new(l), neg));
        }
        let neg = if self.is_kw("NOT")
            && (self.is_kw_at(1, "IN")
                || self.is_kw_at(1, "LIKE")
                || self.is_kw_at(1, "ILIKE")
                || self.is_kw_at(1, "BETWEEN"))
        {
            self.i += 1;
            true
        } else {
            false
        };
        if self.eat_kw("IN") {
            self.expect_kind(K::LParen)?;
            if self.is_kw("SELECT") || self.is_kw("WITH") {
                let q = self.select()?;
                self.expect_kind(K::RParen)?;
                return Ok(Expr::InQuery(Box::new(l), Box::new(q), neg));
            }
            let list = self.expr_list()?;
            self.expect_kind(K::RParen)?;
            return Ok(Expr::InList(Box::new(l), list, neg));
        }
        if self.eat_kw("BETWEEN") {
            let lo = self.other()?;
            self.expect_kw("AND")?;
            let hi = self.other()?;
            return Ok(Expr::Between(Box::new(l), Box::new(lo), Box::new(hi), neg));
        }
        let ci = self.is_kw("ILIKE");
        if self.eat_kw("LIKE") || self.eat_kw("ILIKE") {
            let r = self.other()?;
            if self.eat_kw("ESCAPE") {
                return Err("LIKE ... ESCAPE is not supported".into());
            }
            return Ok(Expr::Like(Box::new(l), Box::new(r), neg, ci));
        }
        if neg {
            return Err(self.err("unexpected NOT"));
        }
        Ok(l)
    }

    /// PostgreSQL ranks `||` with "any other operator", below `+` and `-`:
    /// `'a' || 1 + 2` is `'a' || (1 + 2)`. The other dialects keep it with
    /// `+` and `-` here.
    fn other(&mut self) -> PResult<Expr> {
        let mut l = self.add()?;
        if self.d != Dialect::Postgres {
            return Ok(l);
        }
        // The jsonb operators `->`, `->>`, `#>` and `#>>` share the rank.
        while let Some(op) = ["||", "->>", "->", "#>>", "#>"]
            .into_iter()
            .find(|op| self.is_op(op))
        {
            self.i += 1;
            let r = self.add()?;
            l = Expr::Bin(op.into(), Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn add(&mut self) -> PResult<Expr> {
        let mut l = self.add_operand()?;
        loop {
            let op = match self.peek() {
                Some(t)
                    if t.kind == K::Op
                        && (matches!(t.text.as_str(), "+" | "-")
                            || t.text == "||" && self.d != Dialect::Postgres) =>
                {
                    t.text.clone()
                }
                // Db2 `CONCAT` operator form is rare; not supported.
                _ => break,
            };
            self.i += 1;
            let r = self.add_operand()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    /// An operand of `+` and `-`. T-SQL ranks unary `+` and `-` with them,
    /// below `*`, `/` and `%` ("Operator precedence (Transact-SQL)"), so
    /// `-@a * @b` is `-(@a * @b)`: it differs from `(-@a) * @b` where the
    /// product overflows (on SQL Server 2022, `-@a * @b` with @a =
    /// -2147483648 and @b = 0 is 0). PostgreSQL and Db2 rank unary minus
    /// above `*` (`unary`). A literal keeps its sign: `-5` is a number.
    fn add_operand(&mut self) -> PResult<Expr> {
        if self.d == Dialect::Tsql && (self.is_op("-") || self.is_op("+")) {
            let minus = self.is_op("-");
            self.i += 1;
            let e = self.add_operand()?;
            return Ok(match e {
                _ if !minus => e,
                Expr::Num(n) if !n.starts_with('-') => Expr::Num(format!("-{n}")),
                e => Expr::Unary("-".into(), Box::new(e)),
            });
        }
        self.mul()
    }

    fn mul(&mut self) -> PResult<Expr> {
        let mut l = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(t) if t.kind == K::Op && matches!(t.text.as_str(), "*" | "/" | "%") => {
                    t.text.clone()
                }
                _ => break,
            };
            self.i += 1;
            let r = self.unary()?;
            l = Expr::Bin(op, Box::new(l), Box::new(r));
        }
        Ok(l)
    }

    fn unary(&mut self) -> PResult<Expr> {
        if self.is_op("-") || self.is_op("+") {
            let op = self.peek().unwrap().text.clone();
            self.i += 1;
            let e = self.unary()?;
            if op == "-" {
                if let Expr::Num(n) = &e {
                    return Ok(Expr::Num(format!("-{n}")));
                }
                return Ok(Expr::Unary("-".into(), Box::new(e)));
            }
            return Ok(e);
        }
        self.postfix()
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        loop {
            if self.eat_op("::") {
                let ty = self.ty()?;
                e = Expr::Cast(Box::new(e), ty);
            } else if self.is_kw("COLLATE") {
                return Err("COLLATE is not supported".into());
            } else {
                break;
            }
        }
        Ok(e)
    }

    pub fn expr_list(&mut self) -> PResult<Vec<Expr>> {
        let mut v = vec![self.expr()?];
        while self.eat_kind(K::Comma) {
            v.push(self.expr()?);
        }
        Ok(v)
    }

    fn primary(&mut self) -> PResult<Expr> {
        let Some(t) = self.peek().cloned() else {
            return Err(self.err("expected an expression"));
        };
        match t.kind {
            K::Num => {
                self.i += 1;
                Ok(Expr::Num(t.text.to_lowercase()))
            }
            K::Str => {
                // PostgreSQL reads N'..' as character(n): blank-padded, so it
                // compares and concatenates without trailing blanks
                // (eval/casts: N'ab ' = 'ab' is true). Not modelled.
                if self.d == Dialect::Postgres && t.text.starts_with(['N', 'n']) {
                    return Err(
                        "PostgreSQL N'..' literals (type character) are not supported".into(),
                    );
                }
                self.i += 1;
                Ok(Expr::Str(string_value(&t.text)?))
            }
            K::Var => {
                self.i += 1;
                Ok(Expr::Var(t.text.clone()))
            }
            K::Param => Err("positional parameters are not supported".into()),
            K::Op if t.text == "*" => {
                self.i += 1;
                Ok(Expr::Star(None))
            }
            K::LParen => {
                self.i += 1;
                if self.is_kw("SELECT") || self.is_kw("WITH") || self.is_kw("VALUES") {
                    let q = self.select()?;
                    self.expect_kind(K::RParen)?;
                    return Ok(Expr::Subquery(Box::new(q)));
                }
                let list = self.expr_list()?;
                self.expect_kind(K::RParen)?;
                if list.len() == 1 {
                    Ok(list.into_iter().next().unwrap())
                } else {
                    Ok(Expr::Tuple(list))
                }
            }
            K::Word => self.word_primary(&t),
            K::Quoted => self.name_or_call(),
            _ => Err(self.err("unexpected token in expression")),
        }
    }

    fn word_primary(&mut self, t: &Tok) -> PResult<Expr> {
        match t.up.as_str() {
            // T-SQL `NEXT VALUE FOR seq` (extended parser): a call.
            "NEXT" if self.ext && self.is_kw_at(1, "VALUE") && self.is_kw_at(2, "FOR") => {
                self.i += 3;
                let seq = self.qname()?;
                Ok(Expr::Call(Box::new(Call {
                    name: vec!["NEXT VALUE FOR".into()],
                    args: vec![Expr::Name(seq)],
                    distinct: false,
                    order: Vec::new(),
                    within_group: false,
                    over: false,
                    star: false,
                })))
            }
            "NULL" => {
                self.i += 1;
                Ok(Expr::Null)
            }
            "CASE" => self.case(),
            "CAST" | "TRY_CAST" if self.is_kind_at(1, K::LParen) => {
                if t.up == "TRY_CAST" {
                    return Err("TRY_CAST is not supported".into());
                }
                self.i += 2;
                let e = self.expr()?;
                self.expect_kw("AS")?;
                let ty = self.ty()?;
                self.expect_kind(K::RParen)?;
                Ok(Expr::Cast(Box::new(e), ty))
            }
            "EXISTS" => {
                self.i += 1;
                self.expect_kind(K::LParen)?;
                let q = self.select()?;
                self.expect_kind(K::RParen)?;
                Ok(Expr::Exists(Box::new(q)))
            }
            "NOT" if self.is_kw_at(1, "EXISTS") => {
                self.i += 1;
                let e = self.primary()?;
                Ok(Expr::Unary("NOT".into(), Box::new(e)))
            }
            "EXTRACT" if self.is_kind_at(1, K::LParen) => {
                self.i += 2;
                let field = self.any_ident()?.to_uppercase();
                self.expect_kw("FROM")?;
                let e = self.expr()?;
                self.expect_kind(K::RParen)?;
                Ok(Expr::Extract(field, Box::new(e)))
            }
            "CURRENT"
                if self.d == Dialect::Db2i
                    && self.peek_at(1).is_some_and(|n| {
                        matches!(n.up.as_str(), "TIMESTAMP" | "DATE" | "TIME")
                    }) =>
            {
                let s = format!("CURRENT_{}", self.peek_at(1).unwrap().up);
                self.i += 2;
                Ok(Expr::Special(s))
            }
            // Keywords that are functions, not names: unquoted, they never
            // read a column of that name (`WHERE current_role IS NULL` is
            // the role in PostgreSQL even with a column `"current_role"`).
            "CURRENT_TIMESTAMP" | "CURRENT_DATE" | "CURRENT_TIME" | "LOCALTIMESTAMP"
            | "CURRENT_USER" | "SESSION_USER" | "USER" | "SYSTEM_USER"
                if !self.is_kind_at(1, K::LParen) =>
            {
                self.i += 1;
                Ok(Expr::Special(t.up.clone()))
            }
            "CURRENT_ROLE" | "CURRENT_CATALOG" | "CURRENT_SCHEMA" | "LOCALTIME"
                if self.d == Dialect::Postgres && !self.is_kind_at(1, K::LParen) =>
            {
                self.i += 1;
                Ok(Expr::Special(t.up.clone()))
            }
            "TRUE" | "FALSE" => {
                self.i += 1;
                Ok(Expr::Special(t.up.clone()))
            }
            // PostgreSQL typed literal `interval '1 day'`: a cast of the string.
            "INTERVAL" | "DATE" | "TIMESTAMP" | "TIMESTAMPTZ"
                if self.d == Dialect::Postgres && self.is_kind_at(1, K::Str) =>
            {
                let ty = self.ty()?;
                let s = self.primary()?;
                // `interval '1' day` restricts the fields: not supported.
                if self.peek().is_some_and(|n| {
                    matches!(
                        n.up.as_str(),
                        "YEAR" | "MONTH" | "DAY" | "HOUR" | "MINUTE" | "SECOND"
                    )
                }) {
                    return Err("interval literal with a field restriction".into());
                }
                Ok(Expr::Cast(Box::new(s), ty))
            }
            // LEFT(s, n) and RIGHT(s, n) are functions, not joins.
            "LEFT" | "RIGHT" if self.is_kind_at(1, K::LParen) => self.name_or_call(),
            u if is_reserved(u) => Err(self.err("unexpected keyword in expression")),
            _ => self.name_or_call(),
        }
    }

    pub(crate) fn is_kind_at(&self, n: usize, k: K) -> bool {
        self.peek_at(n).is_some_and(|t| t.kind == k)
    }

    fn name_or_call(&mut self) -> PResult<Expr> {
        let mut parts = vec![self.any_ident()?];
        loop {
            if self.is_kind(K::Dot) {
                if self.is_kind_at(1, K::Op) && self.peek_at(1).unwrap().text == "*" {
                    self.i += 2;
                    return Ok(Expr::Star(Some(parts.join("."))));
                }
                self.i += 1;
                parts.push(self.any_ident()?);
            } else {
                break;
            }
        }
        if self.is_kind(K::LParen) {
            return self.call(parts);
        }
        Ok(Expr::Name(parts))
    }

    fn call(&mut self, name: Vec<String>) -> PResult<Expr> {
        self.expect_kind(K::LParen)?;
        let mut c = Call {
            name,
            args: Vec::new(),
            distinct: false,
            order: Vec::new(),
            within_group: false,
            over: false,
            star: false,
        };
        if self.is_op("*") && self.is_kind_at(1, K::RParen) {
            self.i += 2;
            c.star = true;
        } else if self.eat_kind(K::RParen) {
        } else {
            c.distinct = self.eat_kw("DISTINCT");
            c.args = self.call_args(&c.name)?;
            if self.eat_kws(&["ORDER", "BY"]) {
                c.order = self.order_list()?;
            }
            self.expect_kind(K::RParen)?;
        }
        if self.eat_kws(&["WITHIN", "GROUP"]) {
            self.expect_kind(K::LParen)?;
            self.expect_kw("ORDER")?;
            self.expect_kw("BY")?;
            c.order = self.order_list()?;
            self.expect_kind(K::RParen)?;
            c.within_group = true;
        }
        // An aggregate over all rows of the query: `OVER ()` only.
        if self.lowered
            && self.is_kw("OVER")
            && self.is_kind_at(1, K::LParen)
            && self.is_kind_at(2, K::RParen)
            && !c.distinct
            && c.order.is_empty()
        {
            self.i += 3;
            c.over = true;
        }
        if self.is_kw("OVER") || self.is_kw("FILTER") {
            return Err("window functions and FILTER are not supported".into());
        }
        Ok(Expr::Call(Box::new(c)))
    }

    /// The arguments of a call. PostgreSQL named arguments (`name => v`)
    /// are read only for built-ins whose parameters are known here, and are
    /// placed by name, with each left-out parameter's default written out;
    /// an unknown or repeated name, or a positional argument after a named
    /// one, is refused as PostgreSQL refuses it.
    fn call_args(&mut self, name: &[String]) -> PResult<Vec<Expr>> {
        let named_at = |p: &Self| {
            p.d == Dialect::Postgres
                && p.peek().is_some_and(|t| t.kind == K::Word)
                && p.peek_at(1)
                    .is_some_and(|t| t.kind == K::Op && t.text == "=>")
        };
        let mut args = Vec::new();
        let mut named: Vec<(String, Expr)> = Vec::new();
        loop {
            if named_at(self) {
                let n = self.any_ident()?.to_lowercase();
                self.i += 1;
                named.push((n, self.expr()?));
            } else if !named.is_empty() {
                return Err(self.err("positional argument after a named one"));
            } else {
                args.push(self.expr()?);
            }
            if !self.eat_kind(K::Comma) {
                break;
            }
        }
        if named.is_empty() {
            return Ok(args);
        }
        let shown = name.join(".");
        let params = named_params(name)
            .ok_or_else(|| format!("named arguments to {shown}() are not supported"))?;
        if args.len() > params.len() {
            return Err(format!(
                "{shown}() takes at most {} arguments",
                params.len()
            ));
        }
        let mut slots: Vec<Option<Expr>> = args.into_iter().map(Some).collect();
        slots.resize(params.len(), None);
        for (n, v) in named {
            let k = params
                .iter()
                .position(|(p, _)| *p == n)
                .ok_or_else(|| format!("{shown}() has no parameter {n}"))?;
            if slots[k].is_some() {
                return Err(format!("{shown}() is given parameter {n} twice"));
            }
            slots[k] = Some(v);
        }
        Ok(slots
            .into_iter()
            .zip(params)
            .map(|(v, (_, default))| v.unwrap_or_else(|| Expr::Num(default.to_string())))
            .collect())
    }

    fn case(&mut self) -> PResult<Expr> {
        self.expect_kw("CASE")?;
        let operand = if self.is_kw("WHEN") {
            None
        } else {
            Some(Box::new(self.expr()?))
        };
        let mut whens = Vec::new();
        while self.eat_kw("WHEN") {
            let w = self.expr()?;
            self.expect_kw("THEN")?;
            let th = self.expr()?;
            whens.push((w, th));
        }
        let els = if self.eat_kw("ELSE") {
            Some(Box::new(self.expr()?))
        } else {
            None
        };
        self.expect_kw("END")?;
        Ok(Expr::Case(operand, whens, els))
    }

    pub fn order_list(&mut self) -> PResult<Vec<OrderItem>> {
        let mut v = Vec::new();
        loop {
            let expr = self.expr()?;
            let desc = if self.eat_kw("DESC") {
                true
            } else {
                self.eat_kw("ASC");
                false
            };
            let nulls_first = if self.eat_kws(&["NULLS", "FIRST"]) {
                Some(true)
            } else if self.eat_kws(&["NULLS", "LAST"]) {
                Some(false)
            } else {
                None
            };
            v.push(OrderItem {
                expr,
                desc,
                nulls_first,
            });
            if !self.eat_kind(K::Comma) {
                break;
            }
        }
        Ok(v)
    }

    // ---- queries -------------------------------------------------------

    pub fn select(&mut self) -> PResult<Select> {
        if self.is_kw("WITH") && self.lowered && !self.ext {
            let ctes = self.with_list()?;
            let mut s = self.select()?;
            if !s.ctes.is_empty() {
                return Err("nested WITH".into());
            }
            s.ctes = ctes;
            return Ok(s);
        }
        if self.is_kw("WITH") {
            if !self.ext {
                return Err("common table expressions are not supported".into());
            }
            self.i += 1;
            if self.is_kw("RECURSIVE") {
                return Err("recursive common table expressions are not supported".into());
            }
            let mut ctes = Vec::new();
            loop {
                let name = self.ident()?;
                let cols = if self.is_kind(K::LParen) {
                    self.col_list()?
                } else {
                    Vec::new()
                };
                self.expect_kw("AS")?;
                if self.is_kw("MATERIALIZED") || self.is_kw("NOT") {
                    return Err("MATERIALIZED common table expressions are not supported".into());
                }
                self.expect_kind(K::LParen)?;
                let query = self.select()?;
                self.expect_kind(K::RParen)?;
                ctes.push(Cte { name, cols, query });
                if !self.eat_kind(K::Comma) {
                    break;
                }
            }
            let mut s = self.select()?;
            if !s.ctes.is_empty() {
                return Err("nested WITH".into());
            }
            s.ctes = ctes;
            return Ok(s);
        }
        if self.eat_kw("VALUES") {
            let rows = self.values_rows()?;
            return Ok(Select {
                values: Some(rows),
                ..Select::default()
            });
        }
        self.expect_kw("SELECT")?;
        let mut s = Select {
            distinct: self.eat_kw("DISTINCT"),
            ..Select::default()
        };
        if !s.distinct {
            self.eat_kw("ALL");
        }
        if self.eat_kw("TOP") {
            let e = self.primary()?;
            // `TOP 100 PERCENT` keeps every row.
            if matches!(&e, Expr::Num(n) if n.parse::<f64>() == Ok(100.0))
                && self.is_kw("PERCENT")
                && !self.is_kw_at(1, "WITH")
            {
                self.i += 1;
            } else if self.is_kw("PERCENT") || self.is_kws_with_ties() {
                return Err("TOP PERCENT / WITH TIES is not supported".into());
            } else {
                s.top = Some(e);
            }
        }
        loop {
            s.items.push(self.sel_item()?);
            if !self.eat_kind(K::Comma) {
                break;
            }
        }
        if self.eat_kw("INTO") {
            s.into_strict = self.d == Dialect::Postgres && self.eat_kw("STRICT");
            loop {
                let n = self.qname()?;
                s.into.push(if n.len() == 1 && n[0].starts_with('@') {
                    Expr::Var(n[0].clone())
                } else {
                    Expr::Name(n)
                });
                if !self.eat_kind(K::Comma) {
                    break;
                }
            }
        }
        if self.eat_kw("FROM") {
            s.from = self.table_list()?;
            // Db2 for i: `FROM SYSIBM.SYSDUMMY1` is the one-row FROM that
            // T-SQL and PostgreSQL leave out.
            if self.d == Dialect::Db2i
                && matches!(&s.from[..], [From::Table { name, alias: None, hints }]
                    if hints.is_empty()
                        && name.len() == 2
                        && name[0].eq_ignore_ascii_case("SYSIBM")
                        && name[1].eq_ignore_ascii_case("SYSDUMMY1"))
            {
                s.from.clear();
            }
        }
        if self.eat_kw("WHERE") {
            s.where_ = Some(self.expr()?);
        }
        if self.eat_kws(&["GROUP", "BY"]) {
            s.group_by = self.expr_list()?;
        }
        if self.eat_kw("HAVING") {
            s.having = Some(self.expr()?);
        }
        if self.is_kw("UNION") || self.is_kw("EXCEPT") || self.is_kw("INTERSECT") {
            if !self.lowered {
                return Err("set operations are not supported".into());
            }
            let mut op = self.peek().unwrap().up.clone();
            self.i += 1;
            if self.eat_kw("ALL") {
                op.push_str(" ALL");
            } else {
                self.eat_kw("DISTINCT");
            }
            let paren = self.eat_kind(K::LParen);
            let mut operand = self.select()?;
            if paren {
                self.expect_kind(K::RParen)?;
                if self.is_kw("UNION")
                    || self.is_kw("EXCEPT")
                    || self.is_kw("INTERSECT")
                    || self.is_kw("ORDER")
                {
                    return Err("set operation after a parenthesised operand".into());
                }
            } else {
                // The operand parsed the rest of the chain and its ORDER BY
                // and paging, which belong to the whole.
                s.order_by = std::mem::take(&mut operand.order_by);
                s.offset = operand.offset.take();
                s.limit = operand.limit.take();
                s.locking = std::mem::take(&mut operand.locking);
                s.for_json = std::mem::take(&mut operand.for_json);
            }
            let rest = std::mem::take(&mut operand.set_ops);
            s.set_ops.push((op, operand));
            s.set_ops.extend(rest);
            let ops: Vec<&str> = s.set_ops.iter().map(|(o, _)| o.as_str()).collect();
            if ops.iter().any(|o| o.starts_with("INTERSECT"))
                && ops.iter().any(|o| !o.starts_with("INTERSECT"))
            {
                return Err("INTERSECT mixed with other set operations".into());
            }
            return Ok(s);
        }
        if self.eat_kws(&["ORDER", "BY"]) {
            s.order_by = self.order_list()?;
        }
        // Paging: OFFSET n ROWS FETCH {FIRST|NEXT} m ROWS ONLY | LIMIT m OFFSET n | FETCH FIRST m ROWS ONLY.
        loop {
            if self.eat_kw("OFFSET") {
                s.offset = Some(self.add()?);
                if !(self.eat_kw("ROWS") || self.eat_kw("ROW")) && self.d != Dialect::Postgres {
                    return Err(self.err("expected ROWS"));
                }
            } else if self.eat_kw("LIMIT") {
                if self.eat_kw("ALL") {
                    continue;
                }
                s.limit = Some(self.add()?);
            } else if self.is_kw("FETCH") {
                self.i += 1;
                if !(self.eat_kw("FIRST") || self.eat_kw("NEXT")) {
                    return Err(self.err("expected FIRST or NEXT"));
                }
                let n = if self.is_kw("ROWS") || self.is_kw("ROW") {
                    Expr::Num("1".into())
                } else {
                    self.add()?
                };
                if !(self.eat_kw("ROWS") || self.eat_kw("ROW")) {
                    return Err(self.err("expected ROWS"));
                }
                if !self.eat_kw("ONLY") {
                    return Err("FETCH ... WITH TIES is not supported".into());
                }
                s.limit = Some(n);
            } else {
                break;
            }
        }
        // T-SQL FOR JSON PATH / AUTO [, options].
        if self.ext && self.d == Dialect::Tsql && self.is_kw("FOR") && self.is_kw_at(1, "JSON") {
            self.i += 2;
            self.dropped.push("FOR JSON options");
            if !(self.eat_kw("PATH") || self.eat_kw("AUTO")) {
                return Err(self.err("expected PATH or AUTO"));
            }
            while self.eat_kind(K::Comma) {
                if self.eat_kw("ROOT") {
                    if self.eat_kind(K::LParen) {
                        self.i += 1;
                        self.expect_kind(K::RParen)?;
                    }
                } else if self.eat_kw("INCLUDE_NULL_VALUES") || self.eat_kw("WITHOUT_ARRAY_WRAPPER")
                {
                } else {
                    return Err(self.err("unsupported FOR JSON option"));
                }
            }
            s.for_json = true;
            return Ok(s);
        }
        // Locking and isolation clauses are kept as words; anything else
        // (FOR XML, FOR JSON, FOR BROWSE, ...) is refused.
        if self.is_kw("FOR") || self.is_kw("WITH") {
            const WORDS: &[&str] = &[
                "FOR",
                "UPDATE",
                "SHARE",
                "NO",
                "KEY",
                "NOWAIT",
                "SKIP",
                "LOCKED",
                "READ",
                "ONLY",
                "FETCH",
                "WITH",
                "RS",
                "RR",
                "CS",
                "UR",
                "NC",
                "USE",
                "AND",
                "KEEP",
                "EXCLUSIVE",
                "LOCKS",
                "DATA",
            ];
            let mut of = false;
            while let Some(t) = self.peek() {
                if t.kind == K::RParen {
                    break;
                }
                let word = t.kind == K::Word && WORDS.contains(&t.up.as_str());
                if t.kind == K::Word && t.up == "OF" {
                    of = true;
                } else if word {
                    of = false;
                } else if !(of && matches!(t.kind, K::Word | K::Quoted | K::Dot | K::Comma)) {
                    return Err(self.err("unsupported FOR/WITH clause"));
                }
                s.locking.push(t.up.clone());
                self.i += 1;
            }
            if s.locking.len() < 2 {
                return Err(self.err("unsupported FOR/WITH clause"));
            }
        }
        Ok(s)
    }

    fn is_kws_with_ties(&self) -> bool {
        self.is_kw("WITH") && self.is_kw_at(1, "TIES")
    }

    fn sel_item(&mut self) -> PResult<SelItem> {
        // T-SQL `@v = expr`.
        if self.is_kind(K::Var) && self.is_kind_at(1, K::Op) && self.peek_at(1).unwrap().text == "="
        {
            let v = self.peek().unwrap().text.clone();
            self.i += 2;
            let expr = self.expr()?;
            return Ok(SelItem {
                expr,
                alias: None,
                assign: Some(v),
            });
        }
        if self.is_kind(K::Var) && self.d == Dialect::Tsql {
            if let Some(op) = self.peek_compound(1) {
                let v = self.peek().unwrap().text.clone();
                self.i += 2;
                let e = self.expr()?;
                return Ok(SelItem {
                    expr: Expr::Bin(op.into(), Box::new(Expr::Var(v.clone())), Box::new(e)),
                    alias: None,
                    assign: Some(v),
                });
            }
        }
        let expr = self.expr()?;
        let alias = self.alias()?;
        Ok(SelItem {
            expr,
            alias,
            assign: None,
        })
    }

    fn table_list(&mut self) -> PResult<Vec<From>> {
        let mut v = vec![self.join_tree()?];
        while self.eat_kind(K::Comma) {
            v.push(self.join_tree()?);
        }
        Ok(v)
    }

    fn join_tree(&mut self) -> PResult<From> {
        let mut left = self.table_ref()?;
        loop {
            let kind = if self.eat_kw("JOIN") || self.eat_kws(&["INNER", "JOIN"]) {
                "INNER"
            } else if self.eat_kws(&["LEFT", "JOIN"]) || self.eat_kws(&["LEFT", "OUTER", "JOIN"]) {
                "LEFT"
            } else if self.eat_kws(&["RIGHT", "JOIN"]) || self.eat_kws(&["RIGHT", "OUTER", "JOIN"])
            {
                "RIGHT"
            } else if self.eat_kws(&["FULL", "JOIN"]) || self.eat_kws(&["FULL", "OUTER", "JOIN"]) {
                "FULL"
            } else if self.eat_kws(&["CROSS", "JOIN"]) {
                "CROSS"
            } else {
                break;
            };
            let right = self.table_ref()?;
            let on = if kind == "CROSS" {
                None
            } else {
                self.expect_kw("ON")?;
                Some(self.expr()?)
            };
            left = From::Join {
                kind: kind.into(),
                left: Box::new(left),
                right: Box::new(right),
                on,
            };
        }
        Ok(left)
    }

    fn table_ref(&mut self) -> PResult<From> {
        if self.is_kind(K::LParen) {
            self.i += 1;
            if self.eat_kw("VALUES") {
                let rows = self.values_rows()?;
                self.expect_kind(K::RParen)?;
                let alias = self.alias()?;
                let cols = if self.is_kind(K::LParen) {
                    self.col_list()?
                } else {
                    Vec::new()
                };
                return Ok(From::Values { rows, alias, cols });
            }
            let q = self.select()?;
            self.expect_kind(K::RParen)?;
            let alias = self.alias()?;
            let cols = if self.is_kind(K::LParen) {
                self.col_list()?
            } else {
                Vec::new()
            };
            return Ok(From::Sub {
                query: Box::new(q),
                alias,
                cols,
            });
        }
        if self.d == Dialect::Db2i
            && self
                .peek()
                .is_some_and(|t| matches!(t.up.as_str(), "FINAL" | "NEW" | "OLD"))
            && self.is_kw_at(1, "TABLE")
        {
            let kind = self.peek().unwrap().up.clone();
            self.i += 2;
            self.expect_kind(K::LParen)?;
            let stmt = self.stmt()?;
            self.expect_kind(K::RParen)?;
            let alias = self.alias()?;
            return Ok(From::DataChange {
                kind,
                stmt: Box::new(stmt),
                alias,
            });
        }
        if self.eat_kw("ONLY") {
            // Leaves inheritance children out; not kept in the tree.
            self.dropped.push("ONLY");
        }
        let name = self.qname()?;
        if self.is_kind(K::LParen) {
            // The engine reads only the JSON row sources it models.
            let json = match (self.d, name.as_slice()) {
                (Dialect::Tsql, [f]) => f.eq_ignore_ascii_case("OPENJSON"),
                (Dialect::Postgres, [f]) => f.eq_ignore_ascii_case("jsonb_array_elements"),
                _ => false,
            };
            if !self.ext && !json {
                return Err("table functions in FROM are not supported".into());
            }
            self.i += 1;
            let args = if self.is_kind(K::RParen) {
                Vec::new()
            } else {
                self.expr_list()?
            };
            self.expect_kind(K::RParen)?;
            let mut schema = Vec::new();
            let mut paths = Vec::new();
            if self.d == Dialect::Tsql && self.is_kw("WITH") && self.is_kind_at(1, K::LParen) {
                self.i += 2;
                loop {
                    let col = self.ident()?;
                    let ty = self.ty()?;
                    let path = if self.is_kind(K::Str) {
                        let t = self.peek().unwrap().text.clone();
                        self.i += 1;
                        Some(string_value(&t)?)
                    } else {
                        None
                    };
                    let as_json = self.eat_kw("AS");
                    if as_json {
                        self.expect_kw("JSON")?;
                    }
                    if path.is_some() || as_json {
                        // Not in the encoded tree (see `From::Func`).
                        self.dropped.push("an OPENJSON column path");
                    }
                    schema.push((col, ty));
                    paths.push(JsonCol { path, as_json });
                    if !self.eat_kind(K::Comma) {
                        break;
                    }
                }
                self.expect_kind(K::RParen)?;
            }
            let alias = self.alias()?;
            return Ok(From::Func {
                name,
                args,
                schema,
                paths,
                alias,
            });
        }
        // T-SQL temporal tables: FOR SYSTEM_TIME AS OF x reads another
        // (arbitrary) state of the table, kept as a hint naming the point.
        let mut asof = None;
        if self.ext && self.is_kw("FOR") && self.is_kw_at(1, "SYSTEM_TIME") {
            self.i += 2;
            self.expect_kw("AS")?;
            self.expect_kw("OF")?;
            let start = self.i;
            self.primary()?;
            let text: Vec<String> = self.t[start..self.i].iter().map(|t| t.up.clone()).collect();
            asof = Some(format!("SYSTEM_TIME AS OF {}", text.join(" ")));
        }
        let mut hints = self.table_hints()?;
        hints.extend(asof);
        let alias = self.alias()?;
        hints.extend(self.table_hints()?);
        Ok(From::Table { name, alias, hints })
    }

    /// T-SQL `WITH (HOLDLOCK, ...)` table hints.
    fn table_hints(&mut self) -> PResult<Vec<String>> {
        let mut v = Vec::new();
        if self.d == Dialect::Tsql && self.is_kw("WITH") && self.is_kind_at(1, K::LParen) {
            self.i += 2;
            loop {
                v.push(self.any_ident()?.to_uppercase());
                if !self.eat_kind(K::Comma) {
                    break;
                }
            }
            self.expect_kind(K::RParen)?;
        }
        Ok(v)
    }

    fn values_rows(&mut self) -> PResult<Vec<Vec<Expr>>> {
        let mut rows = Vec::new();
        loop {
            self.expect_kind(K::LParen)?;
            rows.push(self.expr_list()?);
            self.expect_kind(K::RParen)?;
            if !self.eat_kind(K::Comma) {
                break;
            }
        }
        Ok(rows)
    }

    fn set_list(&mut self) -> PResult<Vec<(Vec<String>, Expr)>> {
        let mut sets = Vec::new();
        loop {
            let mut target = None;
            let cols = if self.is_kind(K::LParen) {
                self.col_list()?
            } else {
                let n = self.qname()?;
                if n.len() > 1 {
                    self.dropped.push("the qualifier of a SET target");
                }
                let c = vec![n.last().cloned().unwrap_or_default()];
                target = Some(n);
                c
            };
            // T-SQL `SET col += e` is `SET col = col + e`.
            let e = match (self.compound_op(), target) {
                (Some(op), Some(n)) => {
                    self.i += 1;
                    let e = self.expr()?;
                    Expr::Bin(op.into(), Box::new(Expr::Name(n)), Box::new(e))
                }
                _ => {
                    if !self.eat_op("=") {
                        return Err(self.err("expected ="));
                    }
                    self.expr()?
                }
            };
            sets.push((cols, e));
            if !self.eat_kind(K::Comma) {
                break;
            }
        }
        Ok(sets)
    }

    // ---- statements ----------------------------------------------------

    /// T-SQL `OUTPUT inserted.x, ... [INTO target [(cols)]]`: skipped by
    /// the extended parser (the evaluator compares the write itself).
    /// With [`Parser::keep_output`] it is kept in [`Parser::output`].
    fn output(&mut self) -> PResult<()> {
        if !self.is_kw("OUTPUT") {
            return Ok(());
        }
        if !self.ext && !self.keep_output {
            return Err("OUTPUT clauses are not supported".into());
        }
        self.i += 1;
        let mut items = Vec::new();
        loop {
            items.push(self.sel_item()?);
            if !self.eat_kind(K::Comma) {
                break;
            }
        }
        let mut into = None;
        if self.eat_kw("INTO") {
            let target = self.qname_or_var()?;
            let cols = if self.is_kind(K::LParen) {
                self.col_list()?
            } else {
                Vec::new()
            };
            into = Some((target, cols));
        }
        if self.ext {
            self.dropped.push("an OUTPUT clause");
            return Ok(());
        }
        self.keep(Output { items, into })
    }

    fn keep(&mut self, out: Output) -> PResult<()> {
        if self.output.is_some() {
            return Err("two OUTPUT or RETURNING clauses".into());
        }
        self.output = Some(out);
        Ok(())
    }

    /// PostgreSQL `RETURNING items` of an UPDATE or DELETE, kept with
    /// [`Parser::keep_output`] and refused otherwise; `INTO` after it
    /// (PL/pgSQL) is refused.
    fn returning_items(&mut self, what: &str) -> PResult<()> {
        if !self.is_kw("RETURNING") {
            return Ok(());
        }
        if !self.keep_output || self.d != Dialect::Postgres {
            return Err(format!("{what} RETURNING is not supported"));
        }
        self.i += 1;
        let mut items = Vec::new();
        loop {
            items.push(self.sel_item()?);
            if !self.eat_kind(K::Comma) {
                break;
            }
        }
        if self.is_kw("INTO") {
            return Err(format!("{what} RETURNING INTO is not supported"));
        }
        self.keep(Output { items, into: None })
    }

    /// Whether the statement is `WITH name AS (INSERT|UPDATE|DELETE …`.
    fn dml_cte_ahead(&self) -> bool {
        self.keep_output
            && self.d == Dialect::Postgres
            && self.is_kw("WITH")
            && self
                .peek_at(1)
                .is_some_and(|t| t.kind == K::Word || t.kind == K::Quoted)
            && self.is_kw_at(2, "AS")
            && self.peek_at(3).is_some_and(|t| t.kind == K::LParen)
            && ["INSERT", "UPDATE", "DELETE"]
                .iter()
                .any(|w| self.is_kw_at(4, w))
    }

    /// PostgreSQL `WITH o AS (<dml> RETURNING items) INSERT INTO t [(cols)]
    /// SELECT c1, …, cn FROM o`, where c1 … cn are the names of the returned
    /// columns, each once and in order: the rows the statement changed go to
    /// `t`, as T-SQL `OUTPUT … INTO t` sends them. Returns the inner
    /// statement; the rest is kept in [`Parser::output`].
    fn output_cte(&mut self) -> PResult<Stmt> {
        self.expect_kw("WITH")?;
        let name = self.ident()?;
        self.expect_kw("AS")?;
        self.expect_kind(K::LParen)?;
        let dml = match self.peek().map(|t| t.up.clone()).as_deref() {
            Some("INSERT") => match self.insert()? {
                Stmt::Insert {
                    target,
                    cols,
                    source,
                    returning,
                    returning_into,
                    on_conflict,
                } if !returning.is_empty() && returning_into.is_empty() => {
                    self.keep(Output {
                        items: returning,
                        into: None,
                    })?;
                    Stmt::Insert {
                        target,
                        cols,
                        source,
                        returning: Vec::new(),
                        returning_into,
                        on_conflict,
                    }
                }
                _ => return Err("a data-modifying CTE without RETURNING".into()),
            },
            Some("UPDATE") => self.update()?,
            Some("DELETE") => self.delete()?,
            _ => return Err("expected INSERT, UPDATE or DELETE".into()),
        };
        let Some(Output { items, into: None }) = self.output.take() else {
            return Err("a data-modifying CTE without RETURNING".into());
        };
        self.expect_kind(K::RParen)?;
        self.expect_kw("INSERT")?;
        self.expect_kw("INTO")?;
        let target = self.qname()?;
        let cols = if self.is_kind(K::LParen) {
            self.col_list()?
        } else {
            Vec::new()
        };
        self.expect_kw("SELECT")?;
        let mut names = Vec::new();
        loop {
            names.push(self.ident()?);
            if !self.eat_kind(K::Comma) {
                break;
            }
        }
        self.expect_kw("FROM")?;
        if self.qname()? != [name] {
            return Err("the INSERT does not read the data-modifying CTE".into());
        }
        // Each returned column once, in order, by the name it is returned as.
        let returned: Vec<Option<String>> = items
            .iter()
            .map(|i| match (&i.alias, &i.expr) {
                (Some(a), _) => Some(a.to_lowercase()),
                (None, Expr::Name(p)) => p.last().map(|x| x.to_lowercase()),
                _ => None,
            })
            .collect();
        let names: Vec<String> = names.iter().map(|x| x.to_lowercase()).collect();
        let distinct = (1..names.len()).all(|i| !names[..i].contains(&names[i]));
        let same = names.len() == returned.len()
            && names
                .iter()
                .zip(&returned)
                .all(|(n, r)| r.as_ref() == Some(n));
        if !same || !distinct {
            return Err("the INSERT does not take every returned column once, in order".into());
        }
        self.keep(Output {
            items,
            into: Some((target, cols)),
        })?;
        Ok(dml)
    }

    /// `WITH [RECURSIVE] name [(cols)] AS (query), …` for the equivalence
    /// lowering, which reads every CTE the T-SQL way: a CTE's query sees
    /// the CTEs before it, and its own name is the CTE itself (recursion).
    /// PostgreSQL agrees when its list has RECURSIVE and no query names a
    /// later CTE (RECURSIVE makes that name the later CTE, T-SQL a table),
    /// or has no RECURSIVE and no query names its own CTE (that name is
    /// then a table). Any other list is refused. A name counts wherever it
    /// stands outside a qualifier (`cte.col`), so a column or alias of that
    /// name refuses too. MATERIALIZED and NOT MATERIALIZED are refused, and
    /// so is Db2 for i, whose rules are not checked here.
    fn with_list(&mut self) -> PResult<Vec<Cte>> {
        if self.d == Dialect::Db2i {
            return Err("common table expressions are not compared for Db2 for i".into());
        }
        self.expect_kw("WITH")?;
        let recursive = self.eat_kw("RECURSIVE");
        if recursive && self.d != Dialect::Postgres {
            return Err("recursive common table expressions are not supported".into());
        }
        let mut ctes: Vec<Cte> = Vec::new();
        let mut bodies = Vec::new();
        loop {
            let name = self.ident()?;
            if ctes.iter().any(|c| same_name(&c.name, &name)) {
                return Err(format!("two common table expressions named `{name}`"));
            }
            let cols = if self.is_kind(K::LParen) {
                self.col_list()?
            } else {
                Vec::new()
            };
            self.expect_kw("AS")?;
            if self.is_kw("MATERIALIZED") || self.is_kw("NOT") {
                return Err("MATERIALIZED common table expressions are not supported".into());
            }
            self.expect_kind(K::LParen)?;
            let start = self.i;
            let query = self.select()?;
            bodies.push(start..self.i);
            self.expect_kind(K::RParen)?;
            ctes.push(Cte { name, cols, query });
            if !self.eat_kind(K::Comma) {
                break;
            }
        }
        if self.d == Dialect::Postgres {
            for (i, body) in bodies.iter().enumerate() {
                let names = |c: &Cte| {
                    self.t[body.clone()].iter().enumerate().any(|(k, t)| {
                        let text = match t.kind {
                            K::Word => t.text.clone(),
                            K::Quoted => quoted_name(&t.text, self.d),
                            _ => return false,
                        };
                        same_name(&text, &c.name)
                            && !self
                                .t
                                .get(body.start + k + 1)
                                .is_some_and(|n| n.kind == K::Dot)
                    })
                };
                if recursive && ctes[i + 1..].iter().any(names) {
                    return Err("a recursive CTE list where a query names a CTE defined after it is not supported".into());
                }
                if !recursive && names(&ctes[i]) {
                    return Err(
                        "a CTE whose query names itself without RECURSIVE is not supported".into(),
                    );
                }
            }
        }
        Ok(ctes)
    }

    /// `WITH … SELECT` or `WITH … INSERT … SELECT` for the equivalence
    /// lowering: the CTEs go to the query, the only place the INSERT may
    /// name them (not as its target, nor in OUTPUT, RETURNING or ON
    /// CONFLICT). A T-SQL `OPTION (MAXRECURSION n)` at the end is read and
    /// left out: it only stops a deeper recursion with an error, which the
    /// lowering states as an assumption for every recursive CTE.
    fn with_stmt(&mut self) -> PResult<Stmt> {
        let ctes = self.with_list()?;
        let st = if self.is_kw("INSERT") {
            let had_output = self.output.is_some();
            let mut st = self.insert()?;
            let Stmt::Insert {
                target,
                source,
                returning,
                on_conflict,
                ..
            } = &mut st
            else {
                unreachable!("insert() returns an INSERT")
            };
            if let [t] = &target[..] {
                if ctes.iter().any(|c| same_name(&c.name, t)) {
                    return Err("INSERT into a common table expression is not supported".into());
                }
            }
            if !returning.is_empty() || on_conflict.is_some() || self.output.is_some() != had_output
            {
                return Err(
                    "WITH … INSERT with OUTPUT, RETURNING or ON CONFLICT is not supported".into(),
                );
            }
            let InsertSource::Query(q) = source else {
                return Err("WITH … INSERT … VALUES is not supported".into());
            };
            if !q.ctes.is_empty() {
                return Err("nested WITH".into());
            }
            q.ctes = ctes;
            st
        } else {
            let mut s = self.select()?;
            if !s.ctes.is_empty() {
                return Err("nested WITH".into());
            }
            s.ctes = ctes;
            Stmt::Select(Box::new(s))
        };
        if self.d == Dialect::Tsql
            && self.is_kw("OPTION")
            && self.peek_at(1).is_some_and(|t| t.kind == K::LParen)
            && self.is_kw_at(2, "MAXRECURSION")
            && self.peek_at(3).is_some_and(|t| t.kind == K::Num)
            && self.peek_at(4).is_some_and(|t| t.kind == K::RParen)
        {
            self.i += 5;
        }
        Ok(st)
    }

    fn qname_or_var(&mut self) -> PResult<Vec<String>> {
        match self.peek() {
            Some(t) if t.kind == K::Var => {
                let v = t.text.clone();
                self.i += 1;
                Ok(vec![v])
            }
            _ => self.qname(),
        }
    }

    pub fn stmt(&mut self) -> PResult<Stmt> {
        let Some(t) = self.peek().cloned() else {
            return Err("empty statement".into());
        };
        let s = match t.up.as_str() {
            "SELECT" | "VALUES" => Stmt::Select(Box::new(self.select()?)),
            "WITH" if self.dml_cte_ahead() => self.output_cte()?,
            "WITH" => match self.with_update()? {
                Some(u) => u,
                None if self.ext => Stmt::Select(Box::new(self.select()?)),
                None if self.lowered => self.with_stmt()?,
                None => return Err("common table expressions are not supported".into()),
            },
            "INSERT" => self.insert()?,
            "UPDATE" => self.update()?,
            "DELETE" => self.delete()?,
            "MERGE" => self.merge()?,
            "RETURN" => {
                self.i += 1;
                if self.at_end() {
                    Stmt::Return(None)
                } else if self.is_kw("SELECT") || self.is_kw("VALUES") {
                    Stmt::Return(Some(Expr::Subquery(Box::new(self.select()?))))
                } else {
                    Stmt::Return(Some(self.expr()?))
                }
            }
            "SET" => {
                self.i += 1;
                let target = match self.peek() {
                    Some(t) if t.kind == K::Var => {
                        let v = t.text.clone();
                        self.i += 1;
                        Expr::Var(v)
                    }
                    _ => Expr::Name(self.qname()?),
                };
                if let Some(op) = self.compound_op() {
                    // T-SQL `SET @v += e` is `SET @v = @v + e`.
                    self.i += 1;
                    let e = self.expr()?;
                    Stmt::Assign(
                        target.clone(),
                        Expr::Bin(op.into(), Box::new(target), Box::new(e)),
                    )
                } else {
                    if !self.eat_op("=") {
                        return Err(self.err("expected ="));
                    }
                    Stmt::Assign(target, self.expr()?)
                }
            }
            "DECLARE" => self.declare()?,
            _ if self.is_kind_at(1, K::Op) && self.peek_at(1).unwrap().text == ":=" => {
                let n = self.qname()?;
                self.i += 1;
                Stmt::Assign(Expr::Name(n), self.expr()?)
            }
            _ => return Err(format!("unsupported statement `{}`", t.text)),
        };
        if !self.at_end() && !self.is_kind(K::RParen) {
            return Err(self.err("unexpected trailing tokens"));
        }
        Ok(s)
    }

    fn declare(&mut self) -> PResult<Stmt> {
        self.expect_kw("DECLARE")?;
        let name = match self.peek() {
            Some(t) if t.kind == K::Var => {
                let v = t.text.clone();
                self.i += 1;
                v
            }
            _ => self.ident()?,
        };
        self.eat_kw("AS");
        if self.is_kw("TABLE")
            || self.is_kw("CURSOR")
            || self.is_kw("CONDITION")
            || self.is_kw("CONTINUE")
            || self.is_kw("EXIT")
            || self.is_kw("UNDO")
            || self.is_kw("GLOBAL")
        {
            return Err("unsupported declaration".into());
        }
        let ty = self.ty()?;
        let default = if self.eat_kw("DEFAULT") || self.eat_op("=") || self.eat_op(":=") {
            Some(self.expr()?)
        } else {
            None
        };
        if !self.at_end() {
            return Err(self.err("unsupported declaration"));
        }
        Ok(Stmt::Declare {
            name,
            ty: Some(ty),
            default,
        })
    }

    fn insert(&mut self) -> PResult<Stmt> {
        self.expect_kw("INSERT")?;
        self.eat_kw("INTO");
        let target = self.qname()?;
        // Table hints on the target do not change what is written.
        if self.ext && !self.table_hints()?.is_empty() {
            self.dropped.push("table hints on an INSERT target");
        }
        if self.is_kw("AS")
            || (self.is_kind(K::Word)
                && !self.is_kw("VALUES")
                && !self.is_kw("SELECT")
                && !self.is_kw("DEFAULT")
                && !self.is_kw("OUTPUT"))
        {
            return Err("INSERT target alias is not supported".into());
        }
        let cols = if self.is_kind(K::LParen) && !self.is_kw_at(1, "SELECT") {
            self.col_list()?
        } else {
            Vec::new()
        };
        self.output()?;
        let source = if self.eat_kw("VALUES") {
            InsertSource::Values(self.values_rows()?)
        } else if self.eat_kws(&["DEFAULT", "VALUES"]) {
            InsertSource::Default
        } else {
            let q = if self.eat_kind(K::LParen) {
                let q = self.select()?;
                self.expect_kind(K::RParen)?;
                q
            } else {
                self.select()?
            };
            InsertSource::Query(Box::new(q))
        };
        let mut on_conflict = None;
        if self.eat_kws(&["ON", "CONFLICT"]) {
            let cols = if self.is_kind(K::LParen) {
                self.col_list()?
            } else {
                return Err("ON CONFLICT without a column list is not supported".into());
            };
            self.expect_kw("DO")?;
            let action = if self.eat_kw("NOTHING") {
                ConflictAction::Nothing
            } else {
                self.expect_kw("UPDATE")?;
                self.expect_kw("SET")?;
                let sets = self
                    .set_list()?
                    .into_iter()
                    .map(|(c, e)| (c.into_iter().next().unwrap_or_default(), e))
                    .collect();
                let w = if self.eat_kw("WHERE") {
                    Some(self.expr()?)
                } else {
                    None
                };
                ConflictAction::Update(sets, w)
            };
            on_conflict = Some((cols, action));
        }
        let mut returning = Vec::new();
        let mut returning_into = Vec::new();
        if self.eat_kw("RETURNING") {
            loop {
                returning.push(self.sel_item()?);
                if !self.eat_kind(K::Comma) {
                    break;
                }
            }
            if self.eat_kw("INTO") {
                if self.eat_kw("STRICT") {
                    return Err("RETURNING INTO STRICT is not supported".into());
                }
                loop {
                    returning_into.push(Expr::Name(self.qname()?));
                    if !self.eat_kind(K::Comma) {
                        break;
                    }
                }
            }
        }
        Ok(Stmt::Insert {
            target,
            cols,
            source,
            returning,
            returning_into,
            on_conflict,
        })
    }

    /// `WITH ctes UPDATE …`: each CTE becomes a derived table where the
    /// UPDATE's FROM list names it, which is what both T-SQL and PostgreSQL
    /// do with a non-recursive CTE that only reads. `None` (nothing read)
    /// when no UPDATE follows the list. An error when a CTE is named anywhere
    /// else (the target, a subquery, another CTE's query, its own), so the
    /// tree never holds a CTE name that would bind to a table.
    fn with_update(&mut self) -> PResult<Option<Stmt>> {
        let start = self.i;
        self.expect_kw("WITH")?;
        if self.is_kw("RECURSIVE") {
            self.i = start;
            return Ok(None);
        }
        let mut ctes = Vec::new();
        loop {
            let name = self.ident()?;
            let cols = if self.is_kind(K::LParen) {
                self.col_list()?
            } else {
                Vec::new()
            };
            self.expect_kw("AS")?;
            if self.is_kw("MATERIALIZED") || self.is_kw("NOT") {
                return Err("MATERIALIZED common table expressions are not supported".into());
            }
            self.expect_kind(K::LParen)?;
            let query = self.select()?;
            self.expect_kind(K::RParen)?;
            ctes.push(Cte { name, cols, query });
            if !self.eat_kind(K::Comma) {
                break;
            }
        }
        if !self.is_kw("UPDATE") {
            self.i = start;
            return Ok(None);
        }
        let mut st = self.update()?;
        let Stmt::Update { target, from, .. } = &mut st else {
            unreachable!("update() returns an UPDATE")
        };
        let same = |x: &str, y: &str| {
            if x.starts_with('"') || y.starts_with('"') {
                x == y
            } else {
                x.eq_ignore_ascii_case(y)
            }
        };
        if let [t] = &target[..] {
            if ctes.iter().any(|c| same(&c.name, t)) {
                return Err("UPDATE through a common table expression is not supported".into());
            }
        }
        fn inline(
            f: &mut From,
            ctes: &[Cte],
            same: &dyn Fn(&str, &str) -> bool,
            hits: &mut [usize],
        ) -> PResult<()> {
            match f {
                From::Join { left, right, .. } => {
                    inline(left, ctes, same, hits)?;
                    inline(right, ctes, same, hits)
                }
                From::Table { name, alias, hints } if name.len() == 1 => {
                    let Some(j) = ctes.iter().position(|c| same(&c.name, &name[0])) else {
                        return Ok(());
                    };
                    if !hints.is_empty() {
                        return Err("table hints on a common table expression".into());
                    }
                    hits[j] += 1;
                    let c = &ctes[j];
                    *f = From::Sub {
                        query: Box::new(c.query.clone()),
                        alias: Some(alias.clone().unwrap_or_else(|| c.name.clone())),
                        cols: c.cols.clone(),
                    };
                    Ok(())
                }
                _ => Ok(()),
            }
        }
        let mut hits = vec![0; ctes.len()];
        for f in from.iter_mut() {
            inline(f, &ctes, &same, &mut hits)?;
        }
        // Every name of a CTE outside a qualifier (`cte.col`): its
        // definition and the FROM items just inlined, nothing else.
        for (j, c) in ctes.iter().enumerate() {
            let named = self.t[start..self.i]
                .iter()
                .enumerate()
                .filter(|(k, t)| {
                    let text = match t.kind {
                        K::Word => t.text.clone(),
                        K::Quoted => quoted_name(&t.text, self.d),
                        _ => return false,
                    };
                    same(&text, &c.name)
                        && !self.t.get(start + k + 1).is_some_and(|n| n.kind == K::Dot)
                })
                .count();
            if named != 1 + hits[j] {
                return Err(
                    "a common table expression named outside the UPDATE's FROM list is not supported"
                        .into(),
                );
            }
        }
        Ok(Some(st))
    }

    fn update(&mut self) -> PResult<Stmt> {
        self.expect_kw("UPDATE")?;
        if self.is_kw("TOP") {
            return Err("UPDATE TOP is not supported".into());
        }
        if self.eat_kw("ONLY") {
            // Leaves inheritance children out; not kept in the tree.
            self.dropped.push("ONLY");
        }
        let target = self.qname()?;
        let hints = self.table_hints()?;
        if !hints.is_empty() {
            return Err("table hints on UPDATE are not supported".into());
        }
        let alias = if self.is_kw("SET") {
            None
        } else {
            self.alias()?
        };
        self.expect_kw("SET")?;
        let sets = self.set_list()?;
        self.output()?;
        let from = if self.eat_kw("FROM") {
            self.table_list()?
        } else {
            Vec::new()
        };
        let where_ = if self.eat_kw("WHERE") {
            Some(self.expr()?)
        } else {
            None
        };
        self.returning_items("UPDATE")?;
        Ok(Stmt::Update {
            target,
            alias,
            sets,
            from,
            where_,
        })
    }

    fn delete(&mut self) -> PResult<Stmt> {
        self.expect_kw("DELETE")?;
        if self.is_kw("TOP") {
            return Err("DELETE TOP is not supported".into());
        }
        let has_from = self.eat_kw("FROM");
        if self.eat_kw("ONLY") {
            // Leaves inheritance children out; not kept in the tree.
            self.dropped.push("ONLY");
        }
        let target = self.qname()?;
        let alias = if self.is_kw("FROM")
            || self.is_kw("WHERE")
            || self.is_kw("USING")
            || self.is_kw("OUTPUT")
            || self.at_end()
        {
            None
        } else {
            self.alias()?
        };
        if self.d == Dialect::Tsql && self.keep_output {
            self.output()?;
        }
        let mut from = Vec::new();
        if (self.d == Dialect::Tsql && self.eat_kw("FROM")) || self.eat_kw("USING") {
            from = self.table_list()?;
        }
        let _ = has_from;
        let where_ = if self.eat_kw("WHERE") {
            Some(self.expr()?)
        } else {
            None
        };
        if self.is_kw("OUTPUT") {
            return Err("DELETE OUTPUT is not supported here".into());
        }
        self.returning_items("DELETE")?;
        Ok(Stmt::Delete {
            target,
            alias,
            from,
            where_,
        })
    }

    fn merge(&mut self) -> PResult<Stmt> {
        self.expect_kw("MERGE")?;
        if self.is_kw("TOP") {
            return Err("MERGE TOP is not supported".into());
        }
        self.eat_kw("INTO");
        let target = self.qname()?;
        let mut hints = self.table_hints()?;
        let alias = if self.is_kw("USING") {
            None
        } else {
            self.alias()?
        };
        hints.extend(self.table_hints()?);
        self.expect_kw("USING")?;
        let source = self.table_ref()?;
        self.expect_kw("ON")?;
        let on = self.expr()?;
        let mut whens = Vec::new();
        while self.eat_kw("WHEN") {
            let matched = !self.eat_kw("NOT");
            self.expect_kw("MATCHED")?;
            let by_source = if self.eat_kws(&["BY", "SOURCE"]) {
                true
            } else {
                self.eat_kws(&["BY", "TARGET"]);
                false
            };
            let cond = if self.eat_kw("AND") {
                Some(self.expr()?)
            } else {
                None
            };
            self.expect_kw("THEN")?;
            let action = if self.eat_kw("UPDATE") {
                self.expect_kw("SET")?;
                MergeAction::Update(
                    self.set_list()?
                        .into_iter()
                        .map(|(c, e)| (c.into_iter().next().unwrap_or_default(), e))
                        .collect(),
                )
            } else if self.eat_kw("DELETE") {
                MergeAction::Delete
            } else {
                self.expect_kw("INSERT")?;
                let cols = if self.is_kind(K::LParen) {
                    self.col_list()?
                } else {
                    Vec::new()
                };
                self.expect_kw("VALUES")?;
                self.expect_kind(K::LParen)?;
                let vals = self.expr_list()?;
                self.expect_kind(K::RParen)?;
                MergeAction::Insert(cols, vals)
            };
            whens.push(When {
                matched,
                by_source,
                cond,
                action,
            });
        }
        if self.is_kw("OUTPUT") || self.is_kw("NOT") || self.is_kw("ELSE") {
            return Err("unsupported MERGE clause".into());
        }
        Ok(Stmt::Merge {
            target,
            alias,
            hints,
            source,
            on,
            whens,
        })
    }
}

/// Words that start a clause after a table reference (never an alias).
pub(crate) fn is_clause_start(up: &str) -> bool {
    matches!(
        up,
        "WHERE"
            | "GROUP"
            | "ORDER"
            | "HAVING"
            | "LIMIT"
            | "OFFSET"
            | "FETCH"
            | "JOIN"
            | "INNER"
            | "LEFT"
            | "RIGHT"
            | "FULL"
            | "CROSS"
            | "ON"
            | "USING"
            | "SET"
            | "WHEN"
            | "UNION"
            | "WITH"
            | "FOR"
            | "OUTPUT"
            | "RETURNING"
            | "VALUES"
            | "SELECT"
            | "LATERAL"
            | "WINDOW"
    )
}

/// A quoted identifier. When quoting changes its meaning (PostgreSQL folds
/// unquoted names to lower case, Db2 for i to upper case), the name keeps a
/// leading `"` so it never matches the unquoted spelling.
/// Output column names (as written) that hold distinct values on every row
/// of query `q`: the items of a GROUP BY whose keys are all plain columns
/// the query also returns. NULLs group together, and the grouping compares
/// values as the dialect's own `=` does, so a row of another table meets at
/// most one row of `q` through equalities on those columns.
pub fn group_key(q: &Select) -> Option<Vec<String>> {
    if q.group_by.is_empty() || !q.set_ops.is_empty() || q.values.is_some() || q.for_json {
        return None;
    }
    // A `"`-marked name keeps its case (see `quoted_name`).
    let norm = |p: &[String]| -> Vec<String> {
        p.iter()
            .map(|x| {
                if x.starts_with('"') {
                    x.clone()
                } else {
                    x.to_lowercase()
                }
            })
            .collect()
    };
    let mut key = Vec::new();
    for g in &q.group_by {
        let Expr::Name(gp) = g else { return None };
        let out = q.items.iter().find_map(|it| match &it.expr {
            Expr::Name(ip) if it.assign.is_none() && norm(ip) == norm(gp) => Some(
                it.alias
                    .clone()
                    .unwrap_or_else(|| ip.last().cloned().unwrap_or_default()),
            ),
            _ => None,
        })?;
        key.push(out);
    }
    Some(key)
}

/// Two names as [`quoted_name`] gives them: a `"`-marked name only as
/// written, any other up to letter case.
fn same_name(x: &str, y: &str) -> bool {
    if x.starts_with('"') || y.starts_with('"') {
        x == y
    } else {
        x.eq_ignore_ascii_case(y)
    }
}

pub fn quoted_name(raw: &str, d: Dialect) -> String {
    let s = cst::unquote(raw);
    let significant = match d {
        Dialect::Postgres => s != s.to_lowercase(),
        Dialect::Db2i => s != s.to_uppercase(),
        Dialect::Tsql => false,
    };
    if significant {
        format!("\"{s}")
    } else {
        s
    }
}

/// Value of a string literal token; rejects prefixes other than N.
pub fn string_value(raw: &str) -> PResult<String> {
    let q = raw.find('\'').ok_or("bad string literal")?;
    let prefix = raw[..q].to_ascii_uppercase();
    if !(prefix.is_empty() || prefix == "N") {
        return Err(format!("string literal prefix `{prefix}` is not supported"));
    }
    // The token must close: an unterminated literal (`'abc` at the end of
    // the text) has no value to read.
    let mut value = String::new();
    let mut chars = raw[q + 1..].chars();
    loop {
        match chars.next() {
            None => return Err("unterminated string literal".into()),
            Some('\'') => match chars.clone().next() {
                Some('\'') => {
                    chars.next();
                    value.push('\'');
                }
                None => return Ok(value),
                Some(_) => return Err("bad string literal".into()),
            },
            Some(c) => value.push(c),
        }
    }
}

/// Parses a whole statement token list.
/// Parses a statement with the extended syntax of the evaluator.
pub fn parse_stmt_ext(toks: &[Tok], d: Dialect) -> PResult<Stmt> {
    let mut p = Parser::ext(toks, d);
    let s = p.stmt()?;
    if !p.at_end() {
        return Err(p.err("unexpected trailing tokens"));
    }
    Ok(s)
}

/// [`parse_stmt_ext`], refusing a statement the tree would not show whole
/// (see [`Parser::dropped`]): for readers whose consumers see only the tree.
pub fn parse_stmt_exact(toks: &[Tok], d: Dialect) -> PResult<Stmt> {
    let mut p = Parser::ext(toks, d);
    let s = p.stmt()?;
    if !p.at_end() {
        return Err(p.err("unexpected trailing tokens"));
    }
    if let Some(what) = p.dropped.first() {
        return Err(format!("{what} is not kept in the syntax tree"));
    }
    Ok(s)
}

/// [`parse_stmt`], also reading the rows the statement returns (see
/// [`Output`]); `None` when it returns none.
pub fn parse_stmt_output(toks: &[Tok], d: Dialect) -> PResult<(Stmt, Option<Output>)> {
    let mut p = Parser::lowering(toks, d);
    p.keep_output = true;
    let s = p.stmt()?;
    if !p.at_end() {
        return Err(p.err("unexpected trailing tokens"));
    }
    if p.output.is_some()
        && !matches!(
            s,
            Stmt::Insert { .. } | Stmt::Update { .. } | Stmt::Delete { .. }
        )
    {
        return Err("OUTPUT or RETURNING inside another statement".into());
    }
    Ok((s, p.output))
}

pub fn parse_stmt(toks: &[Tok], d: Dialect) -> PResult<Stmt> {
    let mut p = Parser::new(toks, d);
    let s = p.stmt()?;
    if !p.at_end() {
        return Err(p.err("unexpected trailing tokens"));
    }
    Ok(s)
}

/// [`parse_stmt`] reading UNION, EXCEPT and INTERSECT and aggregates
/// `OVER ()` too, for the equivalence lowering, which compares them
/// (`equiv::canon`).
pub fn parse_stmt_lowered(toks: &[Tok], d: Dialect) -> PResult<Stmt> {
    let mut p = Parser::lowering(toks, d);
    let s = p.stmt()?;
    if !p.at_end() {
        return Err(p.err("unexpected trailing tokens"));
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(sql: &str, d: Dialect) -> Vec<Tok> {
        let p = crate::parse(sql, d);
        tokens(&p.syntax())
    }

    #[test]
    fn json_row_sources() {
        // The engine's parser reads OPENJSON and its paths...
        let t = toks(
            "SELECT a FROM OPENJSON(@j) WITH (a int N'strict $.A', b date, c nvarchar(max) '$.c' AS JSON) AS o",
            Dialect::Tsql,
        );
        let Stmt::Select(q) = parse_stmt(&t, Dialect::Tsql).unwrap() else {
            panic!()
        };
        let From::Func { paths, .. } = &q.from[0] else {
            panic!()
        };
        let p = |path: Option<&str>, as_json| JsonCol {
            path: path.map(Into::into),
            as_json,
        };
        assert_eq!(
            paths,
            &[
                p(Some("strict $.A"), false),
                p(None, false),
                p(Some("$.c"), true)
            ]
        );
        // ... which the encoded tree does not show.
        assert!(parse_stmt_exact(&t, Dialect::Tsql).is_err());
        // Other table functions stay refused.
        let t = toks("SELECT a FROM STRING_SPLIT(@j, ',')", Dialect::Tsql);
        assert!(parse_stmt(&t, Dialect::Tsql).is_err());
        // PostgreSQL's jsonb operators rank with `||`, left to right.
        let t = toks(
            "SELECT CAST(e ->> 'k' AS int), x #>> '{}' || 'a' FROM jsonb_array_elements(CAST(p_j AS jsonb)) AS e",
            Dialect::Postgres,
        );
        let Stmt::Select(q) = parse_stmt(&t, Dialect::Postgres).unwrap() else {
            panic!()
        };
        let Expr::Cast(x, _) = &q.items[0].expr else {
            panic!()
        };
        assert!(matches!(&**x, Expr::Bin(op, _, _) if op == "->>"));
        let Expr::Bin(op, l, _) = &q.items[1].expr else {
            panic!()
        };
        assert_eq!(op, "||");
        assert!(matches!(&**l, Expr::Bin(op, _, _) if op == "#>>"));
    }

    #[test]
    fn keywords_that_are_functions_are_not_names() {
        let e = |src: &str, d: Dialect| {
            let toks = tokens(&crate::parse(&format!("SELECT {src}"), d).syntax());
            let mut p = Parser::new(&toks[1..], d);
            p.expr().unwrap()
        };
        for k in [
            "current_role",
            "current_catalog",
            "current_schema",
            "localtime",
            "system_user",
        ] {
            assert_eq!(
                e(k, Dialect::Postgres),
                Expr::Special(k.to_uppercase()),
                "{k}"
            );
        }
        assert_eq!(
            e("SYSTEM_USER", Dialect::Tsql),
            Expr::Special("SYSTEM_USER".into())
        );
        // Quoted, they are names; in T-SQL current_role is a name anyway.
        assert!(matches!(
            e("\"current_role\"", Dialect::Postgres),
            Expr::Name(_)
        ));
        assert!(matches!(e("[SYSTEM_USER]", Dialect::Tsql), Expr::Name(_)));
        assert!(matches!(e("current_role", Dialect::Tsql), Expr::Name(_)));
    }

    #[test]
    fn tsql_unary_minus_ranks_below_multiplication() {
        let e = |src: &str, d: Dialect| {
            let toks = tokens(&crate::parse(&format!("SELECT {src}"), d).syntax());
            let mut p = Parser::new(&toks[1..], d);
            p.expr().unwrap()
        };
        let neg = |x: Expr| Expr::Unary("-".into(), Box::new(x));
        let mul = |x: Expr, y: Expr| Expr::Bin("*".into(), Box::new(x), Box::new(y));
        let (a, b) = (Expr::Var("@a".into()), Expr::Var("@b".into()));
        assert_eq!(e("-@a * @b", Dialect::Tsql), neg(mul(a.clone(), b.clone())));
        let (pa, pb) = (Expr::Name(vec!["a".into()]), Expr::Name(vec!["b".into()]));
        assert_eq!(e("-a * b", Dialect::Postgres), mul(neg(pa), pb));
        // A literal keeps its sign; `-` then `+` binds as before.
        assert_eq!(
            e("-2147483648", Dialect::Tsql),
            Expr::Num("-2147483648".into())
        );
        assert!(matches!(e("-@a + @b", Dialect::Tsql), Expr::Bin(op, _, _) if op == "+"));
    }

    #[test]
    fn parses_corpus_shapes() {
        for (sql, d) in [
            ("SELECT @name = last_name FROM dbo.customer WHERE customer_id = @Id;", Dialect::Tsql),
            ("SELECT last_name INTO v_name FROM customer WHERE customer_id = p_id;", Dialect::Postgres),
            ("SELECT o.order_id FROM orders o WHERE o.customer_id = p ORDER BY o.created_at DESC, o.order_id DESC LIMIT p_take OFFSET p_skip;", Dialect::Postgres),
            ("MERGE dbo.item_price WITH (HOLDLOCK) AS t USING (SELECT @Item AS item_id) AS s ON t.item_id = s.item_id WHEN MATCHED THEN UPDATE SET unit_price = s.unit_price WHEN NOT MATCHED THEN INSERT (item_id) VALUES (s.item_id);", Dialect::Tsql),
            ("INSERT INTO item_price (item_id, unit_price) VALUES (p_item, p_price) ON CONFLICT (item_id) DO UPDATE SET unit_price = EXCLUDED.unit_price;", Dialect::Postgres),
            ("SELECT order_id INTO p_order_id FROM FINAL TABLE (INSERT INTO orders (customer_id) VALUES (p_customer));", Dialect::Db2i),
            ("DELETE pf FROM dbo.price_feed AS pf LEFT JOIN dbo.item AS i ON i.item_id = pf.item_id WHERE i.item_id IS NULL;", Dialect::Tsql),
            ("SELECT order_id, DAYS(shipped_at) - DAYS(created_at) AS ship_days FROM orders;", Dialect::Db2i),
            ("SELECT order_id, LISTAGG(VARCHAR(item_id), ',') WITHIN GROUP (ORDER BY line_no) AS item_ids FROM order_line GROUP BY order_id;", Dialect::Db2i),
            ("SELECT order_id, string_agg(item_id::text, ',' ORDER BY line_no) AS item_ids FROM order_line GROUP BY order_id;", Dialect::Postgres),
            ("SELECT EXTRACT(DAY FROM shipped_at - created_at)::integer AS d FROM orders;", Dialect::Postgres),
        ] {
            let t = toks(sql, d);
            if let Err(e) = parse_stmt(&t, d) {
                panic!("{sql}: {e}");
            }
        }
    }

    #[test]
    fn locking_trailers_only() {
        for (sql, d) in [
            (
                "SELECT qty FROM stock WHERE item_id = p FOR UPDATE OF stock SKIP LOCKED;",
                Dialect::Postgres,
            ),
            (
                "SELECT qty FROM stock WHERE item_id = p WITH RS USE AND KEEP EXCLUSIVE LOCKS;",
                Dialect::Db2i,
            ),
        ] {
            let t = toks(sql, d);
            if let Err(e) = parse_stmt(&t, d) {
                panic!("{sql}: {e}");
            }
        }
        let sql =
            "SELECT STUFF((SELECT ',' + name FROM dbo.item FOR XML PATH('')), 1, 1, '') AS n;";
        assert!(parse_stmt(&toks(sql, Dialect::Tsql), Dialect::Tsql).is_err());
    }
}
