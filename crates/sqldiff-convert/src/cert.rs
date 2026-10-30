//! Certificates for converted routines (docs/verified-conversion.md, "The
//! certificate", C0 item 5).
//!
//! A certificate is about the text on disk: the output file is parsed with
//! the target dialect's parser and read into the abstract syntax, so what
//! the checker is shown is what will run, whatever the rules or the printer
//! did. The source is read the same way. Rust adds the renaming that pairs
//! the two sides' names, an untrusted hint the checker verifies, and the
//! trusted inputs the checker cannot see, each from a stated source: the
//! callee names through the converter's schema rule (`ASM-DBO-SCHEMA`),
//! the target routines' parameter modes from the converted output itself,
//! and whether row counts are observed from the observer profile.

use serde::Serialize;
use sha2::{Digest, Sha256};
use sqldiff_core::assumptions;
use sqldiff_core::proof::{
    self, Checker, MirrorColumn, MirrorDefault, MirrorRequest, MirrorTable, MirrorType,
};
use sqldiff_syntax::abstract_syntax::{tokens, Expr, From, Select, Stmt, K};
use sqldiff_syntax::routine::{self, Mode, Node, Routine, RoutineKind};
use sqldiff_syntax::{encode, parse, plain_table, Dialect, SyntaxKind, SyntaxNode};

/// Where a certificate stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "answer")]
pub enum Answer {
    Confirmed { theorems: Vec<String> },
    Unconfirmed { reason: String },
}

/// One routine's certificate.
#[derive(Debug, Clone, Serialize)]
pub struct Certificate {
    /// The routine's name in the source, parts joined with `.`.
    pub routine: String,
    pub from: String,
    pub to: String,
    /// SHA-256 of the source routine's abstract syntax (`encode::routine`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// SHA-256 of the output routine's abstract syntax, read back from the
    /// output text with the target parser.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Untrusted hint: source name to target name, for parameters and
    /// variables (`@x` to `v_x`), and the T-SQL return status
    /// ([`STATUS`]) to the parameter that carries it.
    pub renaming: Renaming,
    /// What a confirmation still rests on.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<String>,
    /// The checker's scalar slice for a confirmation: `c2a` when every
    /// expression has its meaning in the spec.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scalars: Option<String>,
    /// The checker's statement slice for a confirmation: `c3b` when the
    /// routine reads a table (with or without data statements), `c3a` when
    /// its data statements were in it, `c1` when it had none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statements: Option<String>,
    /// `c4` when the routine's error handling and transactions were in the
    /// checker's C4 slice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<String>,
    /// The routine's byte range in the source.
    #[serde(skip)]
    pub span: (usize, usize),
    #[serde(flatten)]
    pub answer: Answer,
    /// For an unconfirmed answer, the refusal's codes (`family:detail`,
    /// see [`reason_code`]): the checker's own when it gave them, else one
    /// code read from the reason. Empty for a confirmation.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub why: Vec<String>,
}

/// Where a certificate's refusal comes from, as one code `family:detail`
/// in ASCII lower case, from the reason text a certificate carries. The
/// families: `read` (the routine reader refused the source or the output
/// routine), `pair` (the two routines could not be paired or renamed),
/// `check` (the checker was missing, failed, or refused the pair). The
/// checker's own codes (`MirrorAnswer::why`) take precedence; this is the
/// fallback for reasons produced on the Rust side and for a checker
/// without codes. A reason outside every known family is
/// `<family>:other`, never dropped.
pub fn reason_code(reason: &str) -> String {
    const SHAPE: [&str; 2] = [
        "the source routine is outside the verified shape: ",
        "the output routine is outside the verified shape: ",
    ];
    if let Some(rest) = SHAPE.iter().find_map(|p| reason.strip_prefix(p)) {
        return read_code(rest);
    }
    if reason == "the output has no routine for it" {
        return "pair:no-output".into();
    }
    if let Some(rest) = reason.strip_prefix("no renaming pairs the two routines: ") {
        return format!(
            "pair:renaming:{}",
            if rest.contains("parameters") {
                "params"
            } else if rest.contains("local variables") {
                "locals"
            } else if rest.contains("source names map") {
                "names"
            } else {
                "other"
            }
        );
    }
    if reason == "no checker" {
        return "check:none".into();
    }
    if reason.starts_with("checker failed") || reason.contains("does not speak") {
        return "check:transport".into();
    }
    check_code(reason)
}

/// The code of a reason the routine reader gave (`sqldiff_syntax::routine`).
fn read_code(e: &str) -> String {
    fn lower(s: &str) -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect()
    }
    // The backticked word of "unsupported statement `X`" and the token of
    // "expected PROCEDURE or FUNCTION at `TRIGGER`".
    let quoted = |e: &str| e.split('`').nth(1).map(lower);
    if e.starts_with("dynamic SQL") {
        "read:dynamic-sql".into()
    } else if e.contains("FOR JSON") {
        "read:for-json".into()
    } else if e.starts_with("unsupported routine option") {
        "read:routine-option".into()
    } else if e.starts_with("unsupported parameter option") {
        "read:parameter-option".into()
    } else if e.starts_with("expected PROCEDURE or FUNCTION") {
        format!("read:kind:{}", quoted(e).unwrap_or_else(|| "other".into()))
    } else if let Some(k) = e.strip_suffix(" is not a routine") {
        format!("read:kind:{}", lower(k))
    } else if e.starts_with("table-valued results") {
        "read:table-valued".into()
    } else if e.starts_with("unsupported declaration") {
        // A cursor or a table variable.
        "read:declaration".into()
    } else if e.starts_with("unsupported statement `") {
        format!("read:stmt:{}", quoted(e).unwrap_or_else(|| "other".into()))
    } else if let Some(k) = e
        .strip_suffix(" is not read yet")
        .filter(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_uppercase() || b == b'_'))
    {
        // A statement kind the reader has no rule for (`CURSOR_STMT`).
        format!("read:stmt:{}", lower(k))
    } else {
        "read:other".into()
    }
}

/// The code of a reason the checker gave without codes of its own.
fn check_code(r: &str) -> String {
    let detail = if r == "a procedure and a function" {
        "kind"
    } else if r.contains("privileges") {
        "privileges"
    } else if r.contains("parameter or return types") {
        "signature"
    } else if r.starts_with("certificate:") {
        "certificate"
    } else if r.contains("differ beyond the renaming") {
        "differ"
    } else if r.contains("schema") {
        "schema"
    } else if r.contains("does not decode") {
        "decode"
    } else if r.contains("outside the C1 spec") {
        "spec"
    } else {
        "other"
    };
    format!("check:{detail}")
}

/// A source to target name pairing.
pub type Renaming = Vec<(String, String)>;

fn sha(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The T-SQL return status in a renaming (`Sqldiff.Mirror.statusVar`).
pub const STATUS: &str = "%status";

/// The parameter a converted procedure carries the status in
/// (`Sqldiff.Mirror.statusName`).
pub const STATUS_PARAM: &str = "return_code";

/// C1 operators are uninterpreted: a confirmation holds if each operator
/// means the same function in both dialects, which C2 proves.
pub const C1_OPERATORS: &str = "ASM-C1-OPERATORS";

/// Outside the C2a slice the core stops at an error, where T-SQL without
/// `XACT_ABORT ON` carries on with the next statement: a confirmation holds
/// for runs without errors.
pub const C1_NO_ERRORS: &str = "ASM-C1-NO-ERRORS";

/// The assumptions the C2a slice may still rest on, as the checker names
/// them (`formal/lean/Sqldiff/Mirror/Lower.lean`): T-SQL raises on integer
/// overflow and division by zero (ANSI_WARNINGS and ARITHABORT not both
/// OFF), and compares with NULL as UNKNOWN (ANSI_NULLS ON); and what the
/// caller sees of the database after an error is not compared (T-SQL keeps
/// earlier writes that PostgreSQL rolls back with the failed `CALL`).
/// With strings (C2b): sessions concatenate NULL to NULL, collations are
/// binary (only sent when the deployment accepts it), `LEN` sees no
/// character outside the BMP, and string arguments fit their parameters.
/// With dates (C2c): the two DATEADD facts the converter notes too.
/// With data statements or queries (C3a, C3b): callers run the routine
/// with `SET ROWCOUNT 0`.
pub const C2A_RESIDUALS: [&str; 13] = [
    "ASM-TSQL-ARITH-ERRORS",
    "ASM-TSQL-ANSI-NULLS",
    "ASM-STATE-AFTER-ERROR",
    "ASM-CONCAT-NULL",
    BINARY_COLLATION,
    "ASM-TSQL-LEN-BMP",
    STRING_ARGS,
    // C2c: a DATEADD result stays within 0001-01-01 and 9999-12-31 (T-SQL
    // raises 517 past them, PostgreSQL returns the value), and years clamp
    // February 29 to February 28 as months clamp (eval/values V6).
    "ASM-DATEADD-RANGE",
    "ASM-DATEADD-YEAR-END",
    // C4: only errors the core models reach a handler, no caller
    // transaction around a handled error or transaction control, and
    // callers ignore the number of an error the routine raises.
    "ASM-CATCH-CORE-ERRORS",
    "ASM-NO-CALLER-TXN",
    "ASM-THROW-ERRCODE",
    // C3a, C3b: a caller's SET ROWCOUNT n limits the rows the routine's
    // INSERT, UPDATE, DELETE and SELECT statements touch (eval/session).
    ROWCOUNT_0,
];

/// A routine with a data statement or a query (`statements` `c3a` or
/// `c3b`) runs as converted only under `SET ROWCOUNT 0`, the default: a
/// caller's `SET ROWCOUNT n` is inherited by the T-SQL routine and limits
/// the rows its INSERT, UPDATE, DELETE and SELECT statements touch or read,
/// and PostgreSQL has no such setting (eval/session, R1 to R4).
pub const ROWCOUNT_0: &str = "ASM-TSQL-ROWCOUNT-0";

/// Residual ids the checker names whenever a routine needs them, so a
/// `c2a` confirmation without one proves the converter's note of it.
/// Not every residual: `ASM-NO-CALLER-TXN` is named around handlers,
/// transaction control and (checker 0.16.0) whenever an error can leave the
/// routine, but the converter notes it for more than these (a `COMMIT`
/// after an autocommitted statement), so its absence discharges nothing.
pub const NAMED_WHEN_NEEDED: [&str; 1] = ["ASM-CONCAT-NULL"];

/// Converter assumptions a `c2a` confirmation in the C4 slice (`errors`
/// is `c4`) establishes: the checker settles the PL/pgSQL block's rollback
/// against T-SQL's TRY instead of assuming it away.
pub const DISCHARGED_BY_C4: [&str; 1] = ["ASM-TRY-SUBTRANSACTION"];

/// The collation fact the checker takes as given when the deployment
/// accepts it: string comparisons are then in the slice.
pub const BINARY_COLLATION: &str = "ASM-BINARY-COLLATION";

/// What Rust tells the checker that it cannot see, each from a stated
/// source.
#[derive(Debug, Clone, Default)]
pub struct Trusted {
    /// Plain tables of the tree ([`Tables`]).
    pub tables: Tables,
    /// The observer reads rows-affected counts (observer profile).
    pub row_counts: bool,
    /// The deployment profile accepts [`BINARY_COLLATION`].
    pub binary_collation: bool,
    /// The T-SQL routines the source tree declares, names as written. A call
    /// is mapped only when spelt exactly as declared, whatever the collation
    /// the deployment states: in a case-sensitive SQL Server database
    /// `EXEC dbo.LOOK` does not reach `dbo.look` (2812), while PostgreSQL
    /// folds both (eval/queries/callee-case-*).
    pub declared_routines: Vec<Vec<String>>,
    /// The routines of the whole converted tree, read back from the output:
    /// the target catalog a call is checked against (a callee may be in
    /// another file).
    pub output_routines: Vec<Routine>,
}

/// A converted routine calls another and the confirmation took the callee
/// as its own conversion: it holds when the callee's conversion is
/// confirmed too (the release gate checks this in the same run).
pub const CALLEE_CONFIRMED: &str = "ASM-CALLEE-CONFIRMED";

/// String arguments fit their T-SQL parameters (see `C2A_RESIDUALS`).
pub const STRING_ARGS: &str = "ASM-TSQL-STRING-ARGS";

/// A double-quoted T-SQL identifier is a name only under
/// `QUOTED_IDENTIFIER ON` (the setting stored with the routine when it was
/// created); OFF makes it a string.
pub const QUOTED_IDENTIFIER: &str = "ASM-TSQL-QUOTED-IDENTIFIER";

/// A T-SQL procedure that returns NULL returns 0 with a warning message;
/// the converted procedure returns 0 without it.
pub const RETURN_NULL_WARNING: &str = "ASM-TSQL-RETURN-NULL-WARNING";

fn returns_value(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match n {
        Node::Stmt(Stmt::Return(Some(_))) => true,
        Node::Block(b) => returns_value(&b.body),
        Node::If { then, els, .. } => returns_value(then) || returns_value(els),
        Node::While { body, .. } | Node::ForQuery { body, .. } => returns_value(body),
        Node::Try { body, handlers } => {
            returns_value(body) || handlers.iter().any(|h| returns_value(&h.body))
        }
        _ => false,
    })
}

/// A PostgreSQL name as PostgreSQL resolves it: a quoted name whose case
/// matters arrives with a leading `"` and is kept; any other has its ASCII
/// letters folded to lower case, and only those: PostgreSQL leaves other
/// letters as written in a UTF-8 database (`CREATE PROCEDURE Ärger` makes
/// `"Ärger"`), and so does `Sqldiff.Mirror.pgName`.
pub fn pg_name(n: &str) -> String {
    if n.starts_with('"') {
        n.to_string()
    } else {
        n.to_ascii_lowercase()
    }
}

/// The key a routine's name pairs on: T-SQL `dbo.` dropped, quotes gone,
/// ASCII letters folded.
pub(crate) fn pair_key(name: &str, d: Dialect) -> String {
    let parts: Vec<String> = name.split('.').map(|p| p.to_ascii_lowercase()).collect();
    match parts.as_slice() {
        [s, n] if d == Dialect::Tsql && s == "dbo" => n.clone(),
        _ => parts.join("."),
    }
}

/// A name the trusted mappings may carry: two parts or more (a one-part
/// T-SQL name resolves through the calling routine's schema and then
/// `dbo`, which the mapping does not model), and ASCII, where the two
/// dialects agree on what case folding does.
fn mappable(parts: &[String]) -> bool {
    parts.len() >= 2 && parts.iter().all(|p| p.is_ascii())
}

/// A routine read past the statements the reader does not read, with the
/// message for each one left out (`routine::read_collecting`).
type Collected = Result<(Routine, Vec<String>), String>;

/// A routine's name, span, strict read and, only when the strict read
/// failed, its collecting read.
type Read = (
    String,
    (usize, usize),
    Result<Routine, String>,
    Option<Collected>,
);

fn routines(text: &str, d: Dialect) -> Vec<Read> {
    parse(text, d)
        .syntax()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ROUTINE)
        .map(|n| {
            let name = sqldiff_syntax::ast::routine_name(&n).unwrap_or_default();
            let span = sqldiff_syntax::ast::sig_range(&n);
            let strict = routine::read(&n, d);
            let collected = strict.is_err().then(|| routine::read_collecting(&n, d));
            (name, span, strict, collected)
        })
        .collect()
}

/// Assumptions of the converter's notes that a `c2a` confirmation of the
/// whole routine establishes: ISNULL against COALESCE is proven for the
/// int arguments the slice admits. Nothing else is discharged by a proof.
pub const DISCHARGED_BY_C2A: [&str; 1] = ["ASM-ISNULL-TYPE"];

/// T-SQL local variables in the order their DECLAREs appear.
fn tsql_locals(nodes: &[Node], out: &mut Vec<String>) {
    for n in nodes {
        match n {
            Node::Stmt(Stmt::Declare { name, .. }) => out.push(name.clone()),
            Node::Block(b) => tsql_locals(&b.body, out),
            Node::If { then, els, .. } => {
                tsql_locals(then, out);
                tsql_locals(els, out);
            }
            Node::While { body, .. } | Node::ForQuery { body, .. } => tsql_locals(body, out),
            Node::Try { body, handlers } => {
                tsql_locals(body, out);
                for h in handlers {
                    tsql_locals(&h.body, out);
                }
            }
            _ => {}
        }
    }
}

/// A T-SQL caller that catches an error raised inside a transaction the
/// routine opened gets that transaction back, doomed (`XACT_STATE() = -1`,
/// its writes fail with 3930 until it rolls back), where PostgreSQL leaves
/// none open after a failed `CALL` (eval/errors-txn P9d). The checker does
/// not observe it; the rollback-and-rethrow idiom inside a TRY does not
/// leave it (P11), so only a `BEGIN TRAN` outside every TRY counts.
pub const DOOMED_TXN: &str = "ASM-TSQL-DOOMED-TXN";

fn begins_tran_outside_try(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match n {
        Node::Txn { op, .. } => op.eq_ignore_ascii_case("BEGIN"),
        Node::Block(b) => begins_tran_outside_try(&b.body),
        Node::If { then, els, .. } => begins_tran_outside_try(then) || begins_tran_outside_try(els),
        Node::While { body, .. } | Node::ForQuery { body, .. } => begins_tran_outside_try(body),
        _ => false,
    })
}

fn callees(nodes: &[Node], out: &mut Vec<Vec<String>>) {
    for n in nodes {
        match n {
            Node::Call { name, .. } => out.push(name.clone()),
            Node::Block(b) => callees(&b.body, out),
            Node::If { then, els, .. } => {
                callees(then, out);
                callees(els, out);
            }
            Node::While { body, .. } | Node::ForQuery { body, .. } => callees(body, out),
            Node::Try { body, handlers } => {
                callees(body, out);
                for h in handlers {
                    callees(&h.body, out);
                }
            }
            _ => {}
        }
    }
}

/// The tables a routine's data statements write, as written.
fn written_tables(nodes: &[Node], out: &mut Vec<Vec<String>>) {
    for n in nodes {
        match n {
            Node::Stmt(
                Stmt::Insert { target, .. }
                | Stmt::Update { target, .. }
                | Stmt::Delete { target, .. },
            ) => {
                if !out.contains(target) {
                    out.push(target.clone());
                }
            }
            Node::Block(b) => written_tables(&b.body, out),
            Node::If { then, els, .. } => {
                written_tables(then, out);
                written_tables(els, out);
            }
            Node::While { body, .. } | Node::ForQuery { body, .. } => written_tables(body, out),
            Node::Try { body, handlers } => {
                written_tables(body, out);
                for h in handlers {
                    written_tables(&h.body, out);
                }
            }
            _ => {}
        }
    }
}

/// The tables in a query's `FROM`, as written: each item of the list and,
/// (C3d) inside a join, both sides (the checker reads inner and cross joins
/// of catalog tables; other joins and sources are refused there).
fn from_tables(q: &Select, out: &mut Vec<Vec<String>>) {
    fn walk(f: &From, out: &mut Vec<Vec<String>>) {
        match f {
            From::Table { name, .. } => {
                if !out.contains(name) {
                    out.push(name.clone());
                }
            }
            From::Join { left, right, .. } => {
                walk(left, out);
                walk(right, out);
            }
            _ => {}
        }
    }
    for f in &q.from {
        walk(f, out);
    }
}

/// The tables a routine's queries read, as written, in the shapes the
/// checker's C3b slice reads: `SELECT @x = ...`, `SET @x = (SELECT ...)`,
/// `IF [NOT] EXISTS (SELECT ...)` (formal/lean/Sqldiff/Mirror/Lower.lean,
/// "The C3b slice: queries"), (C3d) over every table of a join.
fn read_tables(nodes: &[Node], out: &mut Vec<Vec<String>>) {
    for n in nodes {
        match n {
            Node::Stmt(Stmt::Select(q)) | Node::Stmt(Stmt::Assign(_, Expr::Subquery(q))) => {
                from_tables(q, out)
            }
            Node::Block(b) => read_tables(&b.body, out),
            Node::If { cond, then, els } => {
                match cond {
                    Expr::Exists(q) => from_tables(q, out),
                    Expr::Unary(op, x) if op.eq_ignore_ascii_case("NOT") => {
                        if let Expr::Exists(q) = x.as_ref() {
                            from_tables(q, out);
                        }
                    }
                    _ => {}
                }
                read_tables(then, out);
                read_tables(els, out);
            }
            Node::While { body, .. } => read_tables(body, out),
            Node::ForQuery { query, body, .. } => {
                from_tables(query, out);
                read_tables(body, out);
            }
            Node::Try { body, handlers } => {
                read_tables(body, out);
                for h in handlers {
                    read_tables(&h.body, out);
                }
            }
            _ => {}
        }
    }
}

/// The tables a converted routine's tables are taken to be: in both
/// databases exactly as the tree's DDL declares them
/// (`ASM-TABLES-AS-DECLARED`).
pub const TABLES_AS_DECLARED: &str = "ASM-TABLES-AS-DECLARED";

/// (C3c) The T-SQL `varchar` columns a routine writes were created with
/// `ANSI_PADDING ON`, so they keep trailing spaces as PostgreSQL does
/// (`ASM-TSQL-ANSI-PADDING`; eval/tables T3).
pub const ANSI_PADDING: &str = "ASM-TSQL-ANSI-PADDING";

/// The tables of a converted tree that the C3a slice may take as given
/// (`proof::MirrorTable`), by target name. A table is here only when the
/// source and the output tree each define it exactly once with a plain
/// `CREATE TABLE` (`plain_table`), the two definitions pair column by
/// column under the converter's naming (the T-SQL name folded as
/// PostgreSQL folds it), and nothing else in either tree names it outside
/// a procedure or function body (an index, trigger, rule, policy,
/// `ALTER TABLE`, a table that inherits from it, a view), nor does any
/// DDL statement inside one. Anything the DDL reader cannot show as plain
/// leaves the table out, so its data statements stay unconfirmed.
///
/// (C3c) Columns pair with their types (`int` with `integer`, `varchar(n)`
/// and `nvarchar(n)` with `varchar(n)`) and their constant defaults, the
/// same on both sides; a plain index (`plain_table::index`: non-unique, no
/// filter or option) does not keep the table out when no entry of it can
/// exceed the engine's size limit (`PlainIndex::fits`); a T-SQL tree that
/// turns `ANSI_PADDING` off keeps out every table with a `varchar` column.
///
/// (C3d part 2) Every other table each side defines once with a `CREATE
/// TABLE` the read-only reader reads (`plain_table::read_only`) is sent
/// read-only ([`MirrorTable::read_only`], `read_only_table`) when the two
/// definitions pair by column name and nothing else in either tree names it
/// but statements that cannot change what a read returns
/// (`plain_table::leaves_reads`, another table's definition, a T-SQL view or
/// trigger); the checker refuses every write to it. The trees include the
/// base catalogs' DDL (`--catalog`, `--catalog-b` or its conversion,
/// `base::Base::ddl`), so their tables reach the checker on the same terms.
/// Dynamic SQL anywhere (`dynamic_sql`: since the review an allow-list, so
/// any `EXEC` of a procedure the tree does not declare, a PostgreSQL `DO`
/// block, a routine body no scan reads, a top-level call of an undeclared
/// routine), and a T-SQL change of the database's collation, still keep
/// every table out. (Review) A table is sent neither plain nor read-only
/// when its last name starts with `pg_` (PostgreSQL resolves it in
/// `pg_catalog` first) or another schema defines a table of the same last
/// name, which may hide it on PostgreSQL's `search_path`.
#[derive(Debug, Clone, Default)]
pub struct Tables {
    /// Each table with its T-SQL name as declared.
    by_target: Vec<(MirrorTable, Vec<String>)>,
}

/// Words of a node as names compare: lower case, quotes removed.
fn words(n: &SyntaxNode, d: Dialect, out: &mut Vec<String>) {
    for t in tokens(n) {
        let w = match t.kind {
            K::Word => t.text.to_lowercase(),
            K::Quoted => sqldiff_syntax::abstract_syntax::quoted_name(&t.text, d)
                .trim_start_matches('"')
                .to_lowercase(),
            // A name inside a string still names the table:
            // `sp_bindefault 'df', 'dbo.Acct.Qty'`, `EXEC('ALTER TABLE ...')`.
            K::Str => {
                out.extend(
                    t.text
                        .split(|c: char| {
                            !(c.is_alphanumeric() || matches!(c, '_' | '@' | '#' | '$'))
                        })
                        .filter(|w| !w.is_empty())
                        .map(str::to_lowercase),
                );
                continue;
            }
            _ => continue,
        };
        out.push(w);
    }
}

/// (C3d part 2) The words of a `CREATE` statement's head, before its first
/// `ON`, `AS` or `(`: the name of the index, view or trigger it defines
/// (and a few keywords). An index or a view is a relation of that name,
/// which may hide a table of the same name in another schema on
/// PostgreSQL's `search_path`, so it counts for that name.
fn head_words(n: &SyntaxNode, d: Dialect, out: &mut Vec<String>) {
    let t = tokens(n);
    if t.first().map(|x| x.up.as_str()) != Some("CREATE") {
        return;
    }
    for t in t {
        if t.kind == K::LParen || (t.kind == K::Word && (t.up == "ON" || t.up == "AS")) {
            break;
        }
        match t.kind {
            K::Word => out.push(t.text.to_lowercase()),
            K::Quoted => out.push(
                sqldiff_syntax::abstract_syntax::quoted_name(&t.text, d)
                    .trim_start_matches('"')
                    .to_lowercase(),
            ),
            _ => {}
        }
    }
}

fn is_ddl(n: &SyntaxNode) -> bool {
    tokens(n).first().is_some_and(|t| {
        matches!(
            t.up.as_str(),
            "CREATE" | "ALTER" | "DROP" | "ENABLE" | "DISABLE" | "EXEC" | "EXECUTE"
        )
    })
}

/// Words inside the string literals of a node: a statement that is not DDL
/// may still carry DDL as text (`DECLARE @s nvarchar(200) = N'ALTER TABLE
/// ...'`, then `EXEC (@s)`).
fn string_words(n: &SyntaxNode, out: &mut Vec<String>) {
    for t in tokens(n).iter().filter(|t| t.kind == K::Str) {
        out.extend(
            t.text
                .split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '@' | '#' | '$')))
                .filter(|w| !w.is_empty())
                .map(str::to_lowercase),
        );
    }
}

/// (C3d part 2, review) The T-SQL procedures that run text or change
/// names in ways no scan of the tree can see, also when called without
/// `EXEC` as the first statement of a batch, and the rowset functions that
/// run a statement on a server, possibly this one. Any mention of one is
/// dynamic SQL, whatever the tree declares.
const TSQL_TEXT_RUNNERS: [&str; 22] = [
    "sp_executesql",
    "sp_sqlexec",
    "sp_prepexec",
    "sp_prepexecrpc",
    "sp_prepare",
    "sp_execute",
    "sp_cursoropen",
    "sp_cursorprepare",
    "sp_cursorprepexec",
    "sp_cursorexecute",
    "sp_msforeachtable",
    "sp_msforeachdb",
    "sp_msforeach_worker",
    "sp_rename",
    "xp_cmdshell",
    "sp_send_dbmail",
    "sp_start_job",
    "sp_add_jobstep",
    "sp_update_jobstep",
    "openquery",
    "openrowset",
    "opendatasource",
];

/// (C3d part 2, review) The system procedures an `EXEC` may call without
/// counting as dynamic SQL: they set or drop an extended property, a
/// description no read sees. Unqualified or in `sys` or `dbo`.
const TSQL_HARMLESS_PROCS: [&str; 3] = [
    "sp_addextendedproperty",
    "sp_updateextendedproperty",
    "sp_dropextendedproperty",
];

/// The words that start a T-SQL statement. A top-level statement that
/// starts with any other name is a procedure call without `EXEC` (allowed
/// as the first statement of a batch, "EXECUTE (Transact-SQL)").
const TSQL_STATEMENT_WORDS: [&str; 62] = [
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "MERGE",
    "CREATE",
    "ALTER",
    "DROP",
    "DECLARE",
    "SET",
    "IF",
    "ELSE",
    "WHILE",
    "BEGIN",
    "END",
    "RETURN",
    "PRINT",
    "RAISERROR",
    "THROW",
    "EXEC",
    "EXECUTE",
    "USE",
    "GRANT",
    "REVOKE",
    "DENY",
    "TRUNCATE",
    "WITH",
    "COMMIT",
    "ROLLBACK",
    "SAVE",
    "BREAK",
    "CONTINUE",
    "GOTO",
    "WAITFOR",
    "OPEN",
    "CLOSE",
    "FETCH",
    "DEALLOCATE",
    "ENABLE",
    "DISABLE",
    "DBCC",
    "BACKUP",
    "RESTORE",
    "CHECKPOINT",
    "KILL",
    "BULK",
    "READTEXT",
    "WRITETEXT",
    "UPDATETEXT",
    "SETUSER",
    "SHUTDOWN",
    "RECONFIGURE",
    "REVERT",
    "MOVE",
    "RECEIVE",
    "SEND",
    "GET",
    "COMMENT",
    "ADD",
    "TRY",
    "CATCH",
    "GO",
];

/// The words that may stand before `(` in a PostgreSQL statement without
/// calling a routine: clauses, operators, the special forms the grammar
/// spells like calls (`COALESCE`, `EXTRACT`, ...) and type names with a
/// modifier.
const PG_PAREN_WORDS: [&str; 71] = [
    "IN",
    "EXISTS",
    "VALUES",
    "AS",
    "OVER",
    "FILTER",
    "CAST",
    "ANY",
    "ALL",
    "SOME",
    "ROW",
    "ARRAY",
    "USING",
    "ON",
    "WHERE",
    "AND",
    "OR",
    "NOT",
    "SELECT",
    "THEN",
    "ELSE",
    "WHEN",
    "CASE",
    "FROM",
    "JOIN",
    "INTO",
    "BY",
    "HAVING",
    "RETURNING",
    "SET",
    "LATERAL",
    "TABLE",
    "WITH",
    "MATERIALIZED",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "IS",
    "LIKE",
    "ILIKE",
    "BETWEEN",
    "DISTINCT",
    "LIMIT",
    "OFFSET",
    "CONFLICT",
    "DO",
    "CUBE",
    "ROLLUP",
    "SETS",
    "GROUPING",
    "ROWS",
    "RANGE",
    "GROUPS",
    "WITHIN",
    "COALESCE",
    "NULLIF",
    "GREATEST",
    "LEAST",
    "EXTRACT",
    "POSITION",
    "SUBSTRING",
    "OVERLAY",
    "TRIM",
    "INTERVAL",
    "NUMERIC",
    "DECIMAL",
    "VARCHAR",
    "CHAR",
    "CHARACTER",
    "TIMESTAMP",
    "TIME",
];

/// (C3d part 2, review) PostgreSQL built-ins a top-level statement may
/// call without counting as dynamic SQL: they read their arguments and
/// change nothing (no sequence, setting, lock or file). Unqualified or in
/// `pg_catalog`. Anything else a top-level statement calls that the tree
/// does not declare may run any statement.
const PG_PURE_BUILTINS: [&str; 64] = [
    "count",
    "sum",
    "min",
    "max",
    "avg",
    "lower",
    "upper",
    "length",
    "char_length",
    "character_length",
    "octet_length",
    "abs",
    "round",
    "trunc",
    "floor",
    "ceil",
    "ceiling",
    "mod",
    "power",
    "sqrt",
    "sign",
    "now",
    "clock_timestamp",
    "statement_timestamp",
    "transaction_timestamp",
    "concat",
    "concat_ws",
    "substr",
    "left",
    "right",
    "btrim",
    "ltrim",
    "rtrim",
    "lpad",
    "rpad",
    "replace",
    "reverse",
    "initcap",
    "repeat",
    "chr",
    "ascii",
    "strpos",
    "split_part",
    "to_char",
    "to_date",
    "to_timestamp",
    "to_number",
    "date_trunc",
    "date_part",
    "make_date",
    "make_time",
    "make_timestamp",
    "make_interval",
    "age",
    "format",
    "md5",
    "string_agg",
    "array_agg",
    "bool_and",
    "bool_or",
    "row_number",
    "rank",
    "dense_rank",
    "lag",
];

/// A part of a name as the dialect compares it: quotes and brackets
/// removed; T-SQL in lower case (its names compare without case under the
/// collations the slice admits); PostgreSQL folded (`"Foo` for a quoted
/// name whose case matters, as `quoted_name` keeps it).
fn name_part(t: &sqldiff_syntax::abstract_syntax::Tok, d: Dialect) -> Option<String> {
    match t.kind {
        K::Word => Some(t.text.to_lowercase()),
        K::Quoted => {
            let q = sqldiff_syntax::abstract_syntax::quoted_name(&t.text, d);
            Some(match d {
                Dialect::Tsql => q.to_lowercase(),
                _ => q,
            })
        }
        _ => None,
    }
}

/// The dotted name starting at `t[i]`, its parts as `name_part` gives
/// them, and the position after it; `None` when no name starts there or a
/// part is missing (`db..p`).
fn name_at(
    t: &[sqldiff_syntax::abstract_syntax::Tok],
    i: usize,
    d: Dialect,
) -> Option<(Vec<String>, usize)> {
    let mut parts = vec![name_part(t.get(i)?, d)?];
    let mut j = i + 1;
    while t.get(j).is_some_and(|x| x.kind == K::Dot) {
        parts.push(name_part(t.get(j + 1)?, d)?);
        j += 2;
    }
    Some((parts, j))
}

/// (C3d part 2, review) A PostgreSQL routine whose body no scan reads: a
/// body given as a string (`AS '...'`, also a C or internal function's
/// object file and symbol), one the reader keeps as one dollar-quoted
/// token (a language it does not parse), or a `LANGUAGE` other than
/// `plpgsql` and `sql`.
fn pg_body_unread(n: &SyntaxNode) -> bool {
    let toks: Vec<sqldiff_syntax::SyntaxToken> = n
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| !t.kind().is_trivia())
        .collect();
    let quoted = toks.windows(2).any(|w| {
        w[0].kind() == SyntaxKind::IDENT
            && w[0].text().eq_ignore_ascii_case("AS")
            && matches!(w[1].kind(), SyntaxKind::STRING | SyntaxKind::DOLLAR_STRING)
    });
    let language = toks.windows(2).any(|w| {
        w[0].kind() == SyntaxKind::IDENT
            && w[0].text().eq_ignore_ascii_case("LANGUAGE")
            && !matches!(
                w[1].text()
                    .trim_matches(|c| c == '\'' || c == '"')
                    .to_lowercase()
                    .as_str(),
                "plpgsql" | "sql"
            )
    });
    quoted || language || toks.iter().any(|t| t.kind() == SyntaxKind::DOLLAR_STRING)
}

/// (C3d part 2, review) The routines a tree declares and whose bodies its
/// scan reads, each name's parts as `name_part` gives them: the targets an
/// `EXEC` or a top-level call may name without counting as dynamic SQL.
/// Not a T-SQL CLR routine (`EXTERNAL NAME`), nor a PostgreSQL routine
/// whose body is not read (`pg_body_unread`), which run what no scan sees.
fn declared_bodies(texts: &[&str], d: Dialect) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for text in texts {
        let root = parse(text, d).syntax();
        for n in root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::ROUTINE)
        {
            let t = tokens(&n);
            let Some(k) = t
                .iter()
                .take(4)
                .position(|x| !matches!(x.up.as_str(), "CREATE" | "OR" | "ALTER" | "REPLACE"))
            else {
                continue;
            };
            if !matches!(t[k].up.as_str(), "PROCEDURE" | "PROC" | "FUNCTION") {
                continue;
            }
            let clr = d == Dialect::Tsql
                && t.windows(2)
                    .any(|w| w[0].up == "EXTERNAL" && w[1].up == "NAME");
            if clr || (d == Dialect::Postgres && pg_body_unread(&n)) {
                continue;
            }
            if let Some((parts, _)) = name_at(&t, k + 1, d) {
                out.push(parts);
            }
        }
    }
    out
}

/// (C3d part 2, review) A T-SQL call of the procedure named at `t[j]` that
/// runs nothing a scan of the tree cannot see: one the tree declares
/// (`declared_bodies`), compared with brackets and case removed, or an
/// extended-property procedure. A one-part `sp_` or `xp_` name resolves to
/// the system procedure first and is never the declared one.
fn tsql_call_seen(
    t: &[sqldiff_syntax::abstract_syntax::Tok],
    j: usize,
    declared: &[Vec<String>],
) -> bool {
    let Some((parts, _)) = name_at(t, j, Dialect::Tsql) else {
        return false;
    };
    let last = parts.last().map(String::as_str).unwrap_or_default();
    if TSQL_HARMLESS_PROCS.contains(&last)
        && (parts.len() == 1 || (parts.len() == 2 && matches!(parts[0].as_str(), "sys" | "dbo")))
    {
        return true;
    }
    if last.starts_with("sp_") || last.starts_with("xp_") {
        return false;
    }
    declared.contains(&parts)
}

/// Dynamic SQL: a statement whose effect no scan of the tree can see, so
/// that it may change any table (C3d part 2: an allow-list since the
/// review).
///
/// * T-SQL: every `EXEC`/`EXECUTE`, unless it calls a procedure the tree
///   declares (`declared_bodies`) or an extended-property procedure
///   (`tsql_call_seen`), by name (not `EXEC (...)`, `EXEC @s`, `EXEC
///   @rc = @p`); `EXEC @rc = p` is the call of `p`. A routine's `WITH
///   EXECUTE AS` option runs nothing. A top-level statement that starts
///   with a name (a call without `EXEC`) is held to the same. Any mention
///   of a procedure that runs text or renames (`TSQL_TEXT_RUNNERS`) or of
///   a rowset function that runs a statement on a server.
/// * PostgreSQL: every `EXECUTE` (in PL/pgSQL always text), but a table
///   trigger's `EXECUTE FUNCTION f()`; a `DO` block; `COPY ... PROGRAM`; a
///   top-level statement (`n` a `STATEMENT`) that calls a routine the tree
///   does not declare, other than a built-in that changes nothing
///   (`PG_PURE_BUILTINS`). A routine whose body is not read counts in
///   `definitions` (`pg_body_unread`).
fn dynamic_sql(n: &SyntaxNode, d: Dialect, declared: &[Vec<String>]) -> bool {
    let t = tokens(n);
    let up = |i: usize| {
        t.get(i)
            .filter(|x| x.kind == K::Word)
            .map(|x| x.up.as_str())
    };
    match d {
        Dialect::Tsql => {
            let runs_text = |j: usize| {
                t.get(j).map_or(true, |y| {
                    matches!(y.kind, K::LParen | K::Var | K::Str | K::Param)
                })
            };
            let mention = t.iter().any(|x| {
                matches!(x.kind, K::Word | K::Quoted)
                    && name_part(x, d).is_some_and(|w| TSQL_TEXT_RUNNERS.contains(&w.as_str()))
            });
            let exec = t.iter().enumerate().any(|(i, x)| {
                if !(x.kind == K::Word && (x.up == "EXEC" || x.up == "EXECUTE")) {
                    return false;
                }
                // `WITH [option, ] EXECUTE AS {CALLER | SELF | OWNER | 'user'}`
                // in a routine's head: an option, no call.
                let option = up(i + 1) == Some("AS")
                    && i > 0
                    && (t[i - 1].kind == K::Comma || up(i - 1) == Some("WITH"));
                if option {
                    return false;
                }
                // `EXEC @rc = p` captures p's return code.
                let rc = t.get(i + 1).is_some_and(|y| y.kind == K::Var)
                    && t.get(i + 2)
                        .is_some_and(|y| y.kind == K::Op && y.text == "=");
                let j = if rc { i + 3 } else { i + 1 };
                runs_text(j) || !tsql_call_seen(&t, j, declared)
            });
            // A procedure called without `EXEC`: the first statement of a batch.
            let bare = n.kind() == SyntaxKind::STATEMENT
                && t.first().is_some_and(|x| {
                    x.kind == K::Quoted
                        || (x.kind == K::Word && !TSQL_STATEMENT_WORDS.contains(&x.up.as_str()))
                })
                && !tsql_call_seen(&t, 0, declared);
            mention || exec || bare
        }
        _ => {
            // A PostgreSQL table trigger names the function it runs, `CREATE
            // [OR REPLACE] [CONSTRAINT] TRIGGER ... EXECUTE FUNCTION|PROCEDURE
            // f()`: no text is run, and the function's body is read as a
            // routine of its own. Only in that statement: in PL/pgSQL
            // `EXECUTE function` runs the text of a variable named `function`.
            let trigger = {
                let head: Vec<&str> = t.iter().take(5).map(|x| x.up.as_str()).collect();
                let rest = match head.as_slice() {
                    ["CREATE", "OR", "REPLACE", rest @ ..] => rest,
                    ["CREATE", rest @ ..] => rest,
                    _ => &[][..],
                };
                matches!(rest, ["TRIGGER", ..] | ["CONSTRAINT", "TRIGGER", ..])
            };
            let execute = t.iter().enumerate().any(|(i, x)| {
                x.up == "EXECUTE"
                    && !(trigger
                        && t.get(i + 1)
                            .is_some_and(|y| y.up == "FUNCTION" || y.up == "PROCEDURE"))
            });
            let block = up(0) == Some("DO");
            let program = up(0) == Some("COPY") && t.iter().any(|x| x.up == "PROGRAM");
            let calls = n.kind() == SyntaxKind::STATEMENT
                && matches!(
                    up(0),
                    Some(
                        "SELECT"
                            | "CALL"
                            | "PERFORM"
                            | "VALUES"
                            | "WITH"
                            | "INSERT"
                            | "UPDATE"
                            | "DELETE"
                            | "MERGE"
                            | "EXPLAIN"
                            | "COPY"
                            | "TABLE"
                    )
                )
                && pg_calls_unseen(&t, declared);
            execute || block || program || calls
        }
    }
}

/// (C3d part 2, review) A PostgreSQL statement calls a routine that is
/// neither declared in the tree with a body the scan reads
/// (`declared_bodies`; a `pg_` name resolves in `pg_catalog` first) nor a
/// built-in that changes nothing (`PG_PURE_BUILTINS`). A call is a name
/// before `(` that is no clause word (`PG_PAREN_WORDS`), no type after
/// `::` or `AS`, no table after `INTO` and no `WITH` query's name; anything
/// unsure counts as a call.
fn pg_calls_unseen(t: &[sqldiff_syntax::abstract_syntax::Tok], declared: &[Vec<String>]) -> bool {
    let d = Dialect::Postgres;
    (0..t.len()).any(|i| {
        if !(matches!(t[i].kind, K::Word | K::Quoted)
            && t.get(i + 1).is_some_and(|y| y.kind == K::LParen))
        {
            return false;
        }
        // The start of the dotted name ending at `i`.
        let mut s = i;
        while s >= 2 && t[s - 1].kind == K::Dot && matches!(t[s - 2].kind, K::Word | K::Quoted) {
            s -= 2;
        }
        let one = s == i;
        if one && t[i].kind == K::Word && PG_PAREN_WORDS.contains(&t[i].up.as_str()) {
            return false;
        }
        let prev = s.checked_sub(1).map(|k| &t[k]);
        if prev.is_some_and(|p| {
            (p.kind == K::Op && p.text == "::")
                || (p.kind == K::Word
                    && matches!(p.up.as_str(), "AS" | "INTO" | "WITH" | "RECURSIVE"))
        }) {
            return false;
        }
        let Some((parts, _)) = name_at(t, s, d) else {
            return true;
        };
        let last = parts.last().map(String::as_str).unwrap_or_default();
        let builtin = PG_PURE_BUILTINS.contains(&last)
            && (parts.len() == 1 || (parts.len() == 2 && parts[0] == "pg_catalog"));
        let own = !last.starts_with("pg_") && declared.contains(&parts);
        !(builtin || own)
    })
}

/// What a tree's DDL says about its tables ([`definitions`]).
#[derive(Default)]
struct Ddl {
    /// The plain table definitions.
    defs: Vec<plain_table::PlainTable>,
    /// (C3c) The plain indexes (non-unique, no filter or option).
    indexes: Vec<plain_table::PlainIndex>,
    /// Every word named elsewhere.
    named: Vec<String>,
    /// The tree runs dynamic SQL somewhere.
    dynamic: bool,
    /// (C3c) A T-SQL `SET ANSI_PADDING OFF` (or `SET ANSI_DEFAULTS OFF`) at
    /// the top level: a `varchar` column created after it trims trailing
    /// spaces on every write (eval/tables T3).
    padding_off: bool,
    /// A T-SQL `ALTER DATABASE ... COLLATE`: columns created before it keep
    /// the old collation, so no column is known to have the database's.
    collation_changed: bool,
    /// (C3d part 2) Every `CREATE TABLE` the read-only reader reads
    /// (`plain_table::read_only`), plain ones included.
    ro_defs: Vec<plain_table::ReadTable>,
    /// (C3d part 2) Every word named by a statement that may change what a
    /// read of a table returns: all but the read-only definitions and the
    /// statements of `plain_table::leaves_reads`; T-SQL views; T-SQL
    /// triggers but for the DDL and strings in their bodies (as routines).
    named_ro: Vec<String>,
    /// (C3d part 2) A T-SQL trigger or view runs dynamic SQL: it may change
    /// any table when it fires, so no table is read-only.
    ro_blocked: bool,
}

/// A statement that may turn `ANSI_PADDING` off: `SET ANSI_PADDING OFF`,
/// `SET ANSI_DEFAULTS OFF` (which turns it off too), `ALTER DATABASE ...
/// SET ANSI_PADDING OFF`, or `sp_configure 'user options'` (the server's
/// default for connections). Any such statement counts, wherever it stands.
fn padding_off(n: &SyntaxNode) -> bool {
    let t = tokens(n);
    let named = |w: &str| t.iter().any(|x| x.up == w);
    ((named("ANSI_PADDING") || named("ANSI_DEFAULTS")) && named("OFF"))
        || (named("SP_CONFIGURE")
            && t.iter()
                .any(|x| x.kind == K::Str && x.text.to_lowercase().contains("user options")))
}

/// `ALTER DATABASE ... COLLATE ...`.
fn collation_changed(n: &SyntaxNode) -> bool {
    let t = tokens(n);
    t.len() >= 2
        && t[0].up == "ALTER"
        && t[1].up == "DATABASE"
        && t.iter().any(|x| x.up == "COLLATE")
}

/// Plain table definitions and (C3c) plain indexes of a tree, every word
/// named elsewhere, whether the tree runs dynamic SQL anywhere, and
/// whether it turns `ANSI_PADDING` off.
fn definitions(texts: &[&str], d: Dialect) -> Ddl {
    let declared = declared_bodies(texts, d);
    let (mut defs, mut named, mut dynamic) = (Vec::new(), Vec::new(), false);
    let (mut indexes, mut padding, mut collation) = (Vec::new(), false, false);
    let (mut ro_defs, mut named_ro, mut ro_blocked) = (Vec::new(), Vec::new(), false);
    for text in texts {
        let root = parse(text, d).syntax();
        let mut it = root.preorder();
        while let Some(ev) = it.next() {
            let rowan::WalkEvent::Enter(n) = ev else {
                continue;
            };
            match n.kind() {
                SyntaxKind::ROUTINE => {
                    let kind = tokens(&n)
                        .iter()
                        .take(4)
                        .find(|t| !matches!(t.up.as_str(), "CREATE" | "OR" | "ALTER" | "REPLACE"))
                        .map(|t| t.up.clone());
                    // The DDL and the strings of a routine body (a table named
                    // in a string: DDL kept as text).
                    let body_words = |out: &mut Vec<String>| {
                        for s in n
                            .descendants()
                            .filter(|s| s.kind() == SyntaxKind::STATEMENT)
                        {
                            if is_ddl(&s) {
                                words(&s, d, out);
                            } else {
                                string_words(&s, out);
                            }
                        }
                    };
                    if matches!(kind.as_deref(), Some("PROCEDURE" | "PROC" | "FUNCTION")) {
                        // The body names its tables; only DDL in it counts, and
                        // any table named in a string (DDL kept as text).
                        body_words(&mut named);
                        body_words(&mut named_ro);
                        dynamic |= dynamic_sql(&n, d, &declared);
                        // C3d part 2, review: a PostgreSQL body no scan reads (a
                        // string, another language) may run anything.
                        dynamic |= d == Dialect::Postgres && pg_body_unread(&n);
                    } else if d == Dialect::Postgres && kind.as_deref() == Some("DO") {
                        // C3d part 2, review: a `DO` block runs at deployment.
                        words(&n, d, &mut named);
                        dynamic = true;
                    } else {
                        words(&n, d, &mut named);
                        // C3d part 2: a T-SQL view changes no table, and a
                        // trigger fires on writes only: a read fires none. What
                        // its body may run when it fires counts as for a
                        // procedure, dynamic SQL for every table.
                        match (d, kind.as_deref()) {
                            (Dialect::Tsql, Some("VIEW")) => head_words(&n, d, &mut named_ro),
                            (Dialect::Tsql, Some("TRIGGER")) => {
                                head_words(&n, d, &mut named_ro);
                                body_words(&mut named_ro);
                                ro_blocked |= dynamic_sql(&n, d, &declared);
                            }
                            _ => {
                                words(&n, d, &mut named_ro);
                                dynamic |= dynamic_sql(&n, d, &declared);
                            }
                        }
                    }
                    it.skip_subtree();
                }
                SyntaxKind::STATEMENT => {
                    dynamic |= dynamic_sql(&n, d, &declared);
                    padding |= d == Dialect::Tsql && padding_off(&n);
                    collation |= d == Dialect::Tsql && collation_changed(&n);
                    match plain_table::read(&n, d) {
                        Ok(t) => defs.push(t),
                        // C3c: a plain index is checked against its table's
                        // sizes in `Tables::read`; any other statement that
                        // names the table keeps it out.
                        Err(_) => match plain_table::index(&n, d) {
                            Ok(ix) => indexes.push(ix),
                            Err(_) => words(&n, d, &mut named),
                        },
                    }
                    // C3d part 2: a table definition, or a statement that
                    // cannot change what a read returns, does not keep a
                    // read-only table out; anything else naming it does.
                    match plain_table::read_only(&n, d) {
                        Ok(t) => ro_defs.push(t),
                        Err(_) if plain_table::leaves_reads(&n, d) => {
                            head_words(&n, d, &mut named_ro)
                        }
                        // Another table (`LIKE t`, `AS SELECT ... FROM t`, a
                        // temporary one): only the name it defines counts.
                        Err(_) => match plain_table::creates_other_table(&n, d) {
                            Some(name) => named_ro.extend(
                                name.iter()
                                    .map(|x| x.trim_start_matches('"').to_lowercase()),
                            ),
                            None => words(&n, d, &mut named_ro),
                        },
                    }
                    it.skip_subtree();
                }
                _ => {}
            }
        }
    }
    Ddl {
        defs,
        indexes,
        named,
        dynamic,
        padding_off: padding,
        collation_changed: collation,
        ro_defs,
        named_ro,
        ro_blocked,
    }
}

/// (C3c) The column type of the checker's catalog for a T-SQL column and
/// the PostgreSQL column converted from it: `int` against `integer`,
/// `varchar(n)` or `nvarchar(n)` against `varchar(n)` with the same `n`,
/// (C2c) `bigint`, `smallint` and `date` against themselves, `bit` against
/// `boolean`, `datetime2(p)` against `timestamp(p)` (the converter's
/// mapping); anything else does not pair.
fn pair_type(a: plain_table::ColType, b: plain_table::ColType) -> Option<MirrorType> {
    use plain_table::ColType::{BigInt, Bool, Date, Int, NVarchar, SmallInt, Timestamp, Varchar};
    match (a, b) {
        (Int, Int) => Some(MirrorType::Int),
        (BigInt, BigInt) => Some(MirrorType::BigInt),
        (SmallInt, SmallInt) => Some(MirrorType::SmallInt),
        (Varchar(n), Varchar(m)) if n == m => Some(MirrorType::Str {
            len: n,
            wide: false,
        }),
        (NVarchar(n), Varchar(m)) if n == m => Some(MirrorType::Str { len: n, wide: true }),
        (Bool, Bool) => Some(MirrorType::Bool),
        (Date, Date) => Some(MirrorType::Date),
        (Timestamp(p), Timestamp(q)) if p == q => Some(MirrorType::Timestamp(p)),
        _ => None,
    }
}

/// (C3c) The default both columns declare, the same constant (`None` for
/// none on both sides), or why they do not pair. (C2c) For a bit column
/// T-SQL `0`/`1` pairs with PostgreSQL `FALSE`/`TRUE`; for a date or
/// timestamp column the same ISO literal on both sides.
fn pair_default(
    ty: MirrorType,
    a: &Option<plain_table::ColDefault>,
    b: &Option<plain_table::ColDefault>,
) -> Option<Option<MirrorDefault>> {
    use plain_table::ColDefault;
    match (ty, a, b) {
        (_, None, None) => Some(None),
        (MirrorType::Bool, Some(ColDefault::Int(x)), Some(ColDefault::Bool(y)))
            if (*x == 1) == *y =>
        {
            Some(Some(MirrorDefault::Bool(*y)))
        }
        (MirrorType::Bool, Some(ColDefault::Bool(x)), Some(ColDefault::Bool(y))) if x == y => {
            Some(Some(MirrorDefault::Bool(*y)))
        }
        (MirrorType::Date, Some(ColDefault::Str(x)), Some(ColDefault::Str(y))) if x == y => {
            Some(Some(MirrorDefault::Date(x.clone())))
        }
        (MirrorType::Timestamp(_), Some(ColDefault::Str(x)), Some(ColDefault::Str(y)))
            if x == y =>
        {
            Some(Some(MirrorDefault::Timestamp(x.clone())))
        }
        (
            MirrorType::Int | MirrorType::BigInt | MirrorType::SmallInt,
            Some(ColDefault::Int(x)),
            Some(ColDefault::Int(y)),
        ) if x == y => Some(Some(MirrorDefault::Int(*x))),
        (MirrorType::Str { .. }, Some(ColDefault::Str(x)), Some(ColDefault::Str(y))) if x == y => {
            Some(Some(MirrorDefault::Str(x.clone())))
        }
        _ => None,
    }
}

/// (C3d part 2) The read-only table of the checker's catalog for a T-SQL
/// table and the PostgreSQL table converted from it, or `None` when they do
/// not pair: the same number of columns, the same names in the same order
/// under the converter's naming (ASCII), and at least one column of the
/// slice. A column is of the slice when both sides' types are and pair
/// (`pair_type`), else opaque (a name only: no read takes its value). It is
/// `NOT NULL` only when both sides say so (or it is a key column); key
/// flags are sent only when both declare the same primary key in the
/// `CREATE TABLE`, not deferrable (`plain_table::ReadCol::key`), and they
/// are sent as declared, also on an opaque or string column: the checker
/// then reads the table without a key (`CatTable.keyVisible`). No default
/// is sent: no read sees one.
fn read_only_table(
    key: String,
    a: &plain_table::ReadTable,
    b: &plain_table::ReadTable,
) -> Option<MirrorTable> {
    if a.cols.len() != b.cols.len()
        || !a
            .cols
            .iter()
            .zip(&b.cols)
            .all(|(x, y)| x.name.is_ascii() && pg_name(&x.name) == pg_name(&y.name))
    {
        return None;
    }
    let keyed =
        a.cols.iter().any(|x| x.key) && a.cols.iter().zip(&b.cols).all(|(x, y)| x.key == y.key);
    let cols: Vec<MirrorColumn> = a
        .cols
        .iter()
        .zip(&b.cols)
        .map(|(x, y)| {
            let ty = match (x.ty, y.ty) {
                (Some(p), Some(q)) => pair_type(p, q).unwrap_or(MirrorType::Opaque),
                _ => MirrorType::Opaque,
            };
            let key = keyed && x.key;
            MirrorColumn {
                name: pg_name(&y.name),
                tsql: x.name.clone(),
                not_null: ty != MirrorType::Opaque && (key || (x.not_null && y.not_null)),
                key,
                ty,
                default: None,
            }
        })
        .collect();
    if cols.iter().all(|c| c.ty == MirrorType::Opaque) {
        return None;
    }
    Some(MirrorTable {
        name: key,
        cols,
        read_only: true,
    })
}

/// (C3c) The T-SQL table has a `varchar` column (the one `ANSI_PADDING`
/// applies to; eval/tables T3).
pub fn has_varchar(t: &MirrorTable) -> bool {
    t.cols
        .iter()
        .any(|c| matches!(c.ty, MirrorType::Str { wide: false, .. }))
}

impl Tables {
    /// The plain tables of a T-SQL tree converted to PostgreSQL; none for
    /// any other pair of dialects.
    pub fn read(inputs: &[&str], outputs: &[&str], from: Dialect, to: Dialect) -> Tables {
        if (from, to) != (Dialect::Tsql, Dialect::Postgres) {
            return Tables::default();
        }
        let src_ddl = definitions(inputs, from);
        let out_ddl = definitions(outputs, to);
        // A statement built at run time may change any table.
        // After a change of the database's collation no column is known to
        // have it (ASM-TABLES-AS-DECLARED); every table is kept out.
        if src_ddl.dynamic || out_ddl.dynamic || src_ddl.collation_changed {
            return Tables::default();
        }
        let (src, src_named) = (&src_ddl.defs, &src_ddl.named);
        let (out, out_named) = (&out_ddl.defs, &out_ddl.named);
        let pg = |parts: &[String]| {
            parts
                .iter()
                .map(|p| pg_name(p))
                .collect::<Vec<_>>()
                .join(".")
        };
        let last_of = |key: &str| {
            key.rsplit('.')
                .next()
                .unwrap_or_default()
                .trim_start_matches('"')
                .to_lowercase()
        };
        // C3d part 2, review: every table either side defines, by its
        // target name. Another schema's table of the same last name may hide
        // this one on PostgreSQL's `search_path` (or a T-SQL user's default
        // schema), whether or not a reader reads it.
        let defined: Vec<String> = src_ddl
            .defs
            .iter()
            .map(|x| target_routine(&x.name).0)
            .chain(src_ddl.ro_defs.iter().map(|x| target_routine(&x.name).0))
            .chain(out_ddl.defs.iter().map(|x| pg(&x.name)))
            .chain(out_ddl.ro_defs.iter().map(|x| pg(&x.name)))
            .collect();
        // A table out of reach: a `pg_` name, which PostgreSQL resolves in
        // `pg_catalog` first (`pg_user` is the catalog's view whatever the
        // routine's schema), or one another schema also defines.
        let out_of_reach = |key: &str| {
            let last = last_of(key);
            last.starts_with("pg_") || defined.iter().any(|k| k != key && last_of(k) == last)
        };
        let mut by_target = Vec::new();
        for a in src {
            let key = target_routine(&a.name).0;
            let last = last_of(&key);
            if out_of_reach(&key) {
                continue;
            }
            let once = src
                .iter()
                .filter(|x| target_routine(&x.name).0 == key)
                .count()
                == 1;
            let bs: Vec<_> = out.iter().filter(|x| pg(&x.name) == key).collect();
            let [b] = bs.as_slice() else { continue };
            if !once
                || src_named.contains(&last)
                || out_named.contains(&last)
                || a.cols.len() != b.cols.len()
            {
                continue;
            }
            // C3c: an index naming the table under another spelling
            // (`MyDb.dbo.W`, `public.w`) may be on it; it keeps it out, as
            // any other statement naming it does.
            let elsewhere = src_ddl.indexes.iter().any(|ix| {
                let k = target_routine(&ix.table).0;
                k != key && last_of(&k) == last
            }) || out_ddl.indexes.iter().any(|ix| {
                let k = pg(&ix.table);
                k != key && last_of(&k) == last
            });
            if elsewhere {
                continue;
            }
            // C3c: every index on the table, on either side, is plain and
            // its entries fit the engine's limit (eval/tables T8).
            let indexes_fit = src_ddl
                .indexes
                .iter()
                .filter(|ix| target_routine(&ix.table).0 == key)
                .all(|ix| ix.fits(a, from).is_ok())
                && out_ddl
                    .indexes
                    .iter()
                    .filter(|ix| pg(&ix.table) == key)
                    .all(|ix| ix.fits(b, to).is_ok());
            if !indexes_fit {
                continue;
            }
            let cols: Option<Vec<MirrorColumn>> = a
                .cols
                .iter()
                .zip(&b.cols)
                .map(|(x, y)| {
                    if !(x.name.is_ascii()
                        && pg_name(&x.name) == pg_name(&y.name)
                        && x.not_null == y.not_null
                        && x.key == y.key)
                    {
                        return None;
                    }
                    // C3c: the type and the default as both DDLs show them.
                    let ty = pair_type(x.ty, y.ty)?;
                    let default = pair_default(ty, &x.default, &y.default)?;
                    Some(MirrorColumn {
                        name: pg_name(&y.name),
                        tsql: x.name.clone(),
                        not_null: y.not_null,
                        key: y.key,
                        ty,
                        default,
                    })
                })
                .collect();
            if let Some(cols) = cols {
                let t = MirrorTable {
                    name: key,
                    cols,
                    read_only: false,
                };
                // A `varchar` column created under `ANSI_PADDING OFF` trims
                // trailing spaces on writes (eval/tables T3).
                if src_ddl.padding_off && has_varchar(&t) {
                    continue;
                }
                by_target.push((t, a.name.clone()));
            }
        }
        // C3d part 2: every other table defined once on each side, named by
        // nothing that can change a read, whose two definitions pair, is a
        // read-only table.
        if src_ddl.ro_blocked || out_ddl.ro_blocked {
            return Tables { by_target };
        }
        for a in &src_ddl.ro_defs {
            let key = target_routine(&a.name).0;
            if by_target.iter().any(|(t, _)| t.name == key) {
                continue;
            }
            let last = last_of(&key);
            let once = src_ddl
                .ro_defs
                .iter()
                .filter(|x| target_routine(&x.name).0 == key)
                .count()
                == 1;
            let bs: Vec<_> = out_ddl
                .ro_defs
                .iter()
                .filter(|x| pg(&x.name) == key)
                .collect();
            let [b] = bs.as_slice() else { continue };
            if !once
                || out_of_reach(&key)
                || src_ddl.named_ro.contains(&last)
                || out_ddl.named_ro.contains(&last)
            {
                continue;
            }
            if let Some(t) = read_only_table(key, a, b) {
                by_target.push((t, a.name.clone()));
            }
        }
        Tables { by_target }
    }

    pub fn get(&self, target: &str) -> Option<&MirrorTable> {
        self.by_target
            .iter()
            .find(|t| t.0.name == target)
            .map(|t| &t.0)
    }

    /// The table, when it is sent as one the routines may write (not
    /// read-only, C3d part 2).
    #[cfg(test)]
    fn writable(&self, target: &str) -> Option<&MirrorTable> {
        self.get(target).filter(|t| !t.read_only)
    }

    /// The T-SQL name the table was declared with.
    fn declared(&self, target: &str) -> Option<&[String]> {
        self.by_target
            .iter()
            .find(|t| t.0.name == target)
            .map(|t| t.1.as_slice())
    }
}

fn has_status(b: &Routine) -> bool {
    b.params
        .last()
        .is_some_and(|p| pg_name(&p.name) == STATUS_PARAM && p.mode == Mode::InOut)
}

/// Proposes which target name stands for each source name: parameters by
/// position, T-SQL locals in declaration order against the PL/pgSQL
/// declaration section, and the return status against the trailing
/// `return_code` parameter (or itself, when the output has none). Target
/// names are folded as PostgreSQL folds them. The checker verifies the
/// result; a wrong proposal can only make it refuse.
pub fn propose_renaming(a: &Routine, b: &Routine) -> Result<Renaming, String> {
    let mut out = Vec::new();
    let status = a.kind == RoutineKind::Procedure && has_status(b);
    if b.params.len() != a.params.len() + usize::from(status) {
        return Err(format!(
            "{} parameters in the source, {} in the output",
            a.params.len(),
            b.params.len()
        ));
    }
    for (x, y) in a.params.iter().zip(&b.params) {
        out.push((x.name.clone(), pg_name(&y.name)));
    }
    if a.kind == RoutineKind::Procedure {
        let to = if status { STATUS_PARAM } else { STATUS };
        out.push((STATUS.to_string(), to.to_string()));
    }
    let mut locals = Vec::new();
    tsql_locals(&a.body.body, &mut locals);
    let decls: Vec<String> = b
        .body
        .decls
        .iter()
        .filter_map(|s| match s {
            Stmt::Declare { name, .. } => Some(pg_name(name)),
            _ => None,
        })
        .collect();
    if locals.len() != decls.len() {
        return Err(format!(
            "{} local variables in the source, {} declared in the output",
            locals.len(),
            decls.len()
        ));
    }
    out.extend(locals.into_iter().zip(decls));
    let mut seen = std::collections::HashSet::new();
    for (_, y) in &out {
        if !seen.insert(y.clone()) {
            return Err(format!("two source names map to `{y}`"));
        }
    }
    Ok(out)
}

/// The target routine a T-SQL routine name reaches under the converter's
/// schema rule: `dbo.` is dropped (`ASM-DBO-SCHEMA`), and the parts are
/// folded as PostgreSQL folds unquoted names. The second value says whether
/// the rule dropped `dbo`.
pub fn target_routine(parts: &[String]) -> (String, bool) {
    let dbo = parts.len() == 2 && parts[0].eq_ignore_ascii_case("dbo");
    let rest = if dbo { &parts[1..] } else { parts };
    let name = rest
        .iter()
        .map(|p| pg_name(p))
        .collect::<Vec<_>>()
        .join(".");
    (name, dbo)
}

fn mode_letter(m: Mode) -> char {
    match m {
        Mode::In => 'i',
        Mode::Out => 'o',
        Mode::InOut => 'b',
    }
}

/// What the checker is asked about one routine pair, and the assumptions
/// a confirmation rests on.
pub fn mirror_request(
    a: &Routine,
    b: &Routine,
    output_routines: &[Routine],
    trusted: &Trusted,
) -> Result<(MirrorRequest, Vec<String>), String> {
    let renaming = propose_renaming(a, b)?;
    Ok(request_with(a, b, renaming, output_routines, trusted))
}

/// [`mirror_request`] with the renaming given.
fn request_with(
    a: &Routine,
    b: &Routine,
    renaming: Renaming,
    output_routines: &[Routine],
    trusted: &Trusted,
) -> (MirrorRequest, Vec<String>) {
    let (tables, row_counts) = (&trusted.tables, trusted.row_counts);
    let mut assumptions = Vec::new();
    if a.kind == RoutineKind::Procedure {
        assumptions.push("ASM-RETURN-CODE".to_string());
        if returns_value(&a.body.body) {
            assumptions.push(RETURN_NULL_WARNING.to_string());
        }
    }
    let mut calls = Vec::new();
    callees(&a.body.body, &mut calls);
    let mut routines = Vec::new();
    for c in calls {
        // Not mapped, the call stays unknown to the checker, which refuses.
        // Spelt exactly as declared, as tables are below: a case-sensitive
        // database need not have a binary collation, so the deployment's
        // collation fact cannot vouch for another spelling.
        if !mappable(&c) || !trusted.declared_routines.contains(&c) {
            continue;
        }
        let (to, dbo) = target_routine(&c);
        if dbo && !assumptions.iter().any(|x| x == "ASM-DBO-SCHEMA") {
            assumptions.push("ASM-DBO-SCHEMA".to_string());
        }
        let pair = (c.join("."), to);
        if !routines.contains(&pair) {
            let callee = format!("{CALLEE_CONFIRMED}:{}", c.join("."));
            if !assumptions.contains(&callee) {
                assumptions.push(callee);
            }
            routines.push(pair);
        }
    }
    let mut written = Vec::new();
    written_tables(&a.body.body, &mut written);
    // C3b: the tables the routine reads are sent on the same conditions.
    let mut used = written.clone();
    read_tables(&a.body.body, &mut used);
    let (mut table_pairs, mut table_catalog) = (Vec::new(), Vec::<MirrorTable>::new());
    for w in &used {
        if !mappable(w) {
            continue;
        }
        let (to, dbo) = target_routine(w);
        let Some(t) = tables.get(&to) else { continue };
        // Spelt exactly as the DDL declares it, whatever the collation the
        // deployment states: in a case-sensitive SQL Server database
        // `dbo.ORD` does not reach `dbo.Ord` (208, Invalid object name),
        // while PostgreSQL folds both to `ord`. A table named any other way
        // is not sent, so the checker refuses what reads or writes it.
        if tables.declared(&to) != Some(w.as_slice()) {
            continue;
        }
        if dbo && !assumptions.iter().any(|x| x == "ASM-DBO-SCHEMA") {
            assumptions.push("ASM-DBO-SCHEMA".to_string());
        }
        table_pairs.push((w.join("."), to));
        if !table_catalog.contains(t) {
            table_catalog.push(t.clone());
        }
    }
    if !table_catalog.is_empty() {
        assumptions.push(TABLES_AS_DECLARED.to_string());
    }
    // C3c: a write to a `varchar` column keeps trailing spaces only if the
    // column was created under `ANSI_PADDING ON` (eval/tables T3), which
    // the DDL does not show.
    let writes_varchar = written.iter().any(|w| {
        mappable(w)
            && table_catalog
                .iter()
                .any(|t| t.name == target_routine(w).0 && has_varchar(t))
    });
    if writes_varchar {
        assumptions.push(ANSI_PADDING.to_string());
    }
    // Only writes send a rows-affected count: `SELECT @x = ...`, `IF EXISTS`
    // and `SET @x = (subquery)` do not (eval/queries Q3).
    if !written.is_empty() && !row_counts {
        assumptions.push("ASM-OBSERVER-ROWCOUNT".to_string());
    }
    if begins_tran_outside_try(&a.body.body) {
        assumptions.push(DOOMED_TXN.to_string());
    }
    // T-SQL truncates a string argument to its parameter's length and
    // PostgreSQL does not; the C2a residual names it, a C1 answer does not.
    // C2c: so do the parameters of every other type of the slice (a
    // `datetime2(7)` value is rounded on entry, an `int` narrowed or made
    // 0/1, a `1.5` passed to an `int` or `bigint` truncated to 1 where
    // PostgreSQL finds no procedure): callers pass values of the
    // parameter's declared type.
    let stringy = |t: &str| {
        let t = t.to_ascii_lowercase();
        t.contains("char")
            || t == "sysname"
            || t.contains("text")
            || matches!(
                t.as_str(),
                "int" | "integer" | "bigint" | "smallint" | "bit" | "date" | "datetime2"
            )
    };
    if a.params.iter().any(|p| stringy(&p.ty.name)) {
        assumptions.push(STRING_ARGS.to_string());
    }
    // Answers on the C1 theorem carry no residual: the record keeps the
    // fact visible whatever the answer.
    if trusted.binary_collation {
        assumptions.push(BINARY_COLLATION.to_string());
    }
    let catalog = output_routines
        .iter()
        .map(|r| {
            let name = r
                .name
                .iter()
                .map(|p| pg_name(p))
                .collect::<Vec<_>>()
                .join(".");
            (name, r.params.iter().map(|p| mode_letter(p.mode)).collect())
        })
        .collect();
    // C2c: the callees' parameter types, as their PostgreSQL DDL declares
    // them; the checker holds each argument to its parameter's type.
    let catalog_types = output_routines
        .iter()
        .map(|r| {
            let name = r
                .name
                .iter()
                .map(|p| pg_name(p))
                .collect::<Vec<_>>()
                .join(".");
            let types = r
                .params
                .iter()
                .map(|p| (p.ty.name.clone(), p.ty.args.clone()))
                .collect();
            (name, types)
        })
        .collect();
    (
        MirrorRequest {
            row_counts,
            routines,
            catalog,
            catalog_types,
            tables: table_pairs,
            table_catalog,
            binary_collation: trusted.binary_collation,
            renaming,
            source: encode::routine(a),
            target: encode::routine(b),
        },
        assumptions,
    )
}

/// Certificates for the routines of one converted file, asked of `checker`
/// (none: every answer is unconfirmed, with the reason).
pub fn certify(
    input: &str,
    output: &str,
    from: Dialect,
    to: Dialect,
    trusted: &Trusted,
    checker: Option<&dyn Checker>,
) -> Vec<Certificate> {
    let src = routines(input, from);
    let out = routines(output, to);
    let mut readable: Vec<Routine> = out
        .iter()
        .filter_map(|(_, _, r, _)| r.clone().ok())
        .collect();
    for r in &trusted.output_routines {
        if !readable.iter().any(|x| x.name == r.name) {
            readable.push(r.clone());
        }
    }
    let mut certs = Vec::new();
    for (name, span, a, a_collected) in src {
        let mut c = Certificate {
            routine: name.clone(),
            scalars: None,
            statements: None,
            errors: None,
            span,
            from: from.id().to_string(),
            to: to.id().to_string(),
            source: None,
            target: None,
            renaming: Vec::new(),
            assumptions: Vec::new(),
            answer: Answer::Unconfirmed {
                reason: String::new(),
            },
            why: Vec::new(),
        };
        // The output routine of the same name, if exactly one.
        let key = pair_key(&name, from);
        let same: Vec<_> = out
            .iter()
            .filter(|(n, _, _, _)| pair_key(n, to) == key)
            .collect();
        let (b, b_collected) = match same.as_slice() {
            [(_, _, b, bc)] => (Some(b.clone()), bc.as_ref()),
            _ => (None, None),
        };
        // Computed before the strict reads are matched on below.
        let unread = left_out(&a, a_collected.as_ref(), b.as_ref(), b_collected);
        let quoted = from == Dialect::Tsql
            && input
                .get(span.0..span.1)
                .is_some_and(|t| has_double_quoted_name(t, from));
        c.answer = match (a, b) {
            (Err(e), _) => Answer::Unconfirmed {
                reason: format!("the source routine is outside the verified shape: {e}"),
            },
            (_, None) => Answer::Unconfirmed {
                reason: "the output has no routine for it".into(),
            },
            (_, Some(Err(e))) => Answer::Unconfirmed {
                reason: format!("the output routine is outside the verified shape: {e}"),
            },
            (Ok(a), Some(Ok(b))) => {
                c.source = Some(sha(&encode::routine(&a)));
                c.target = Some(sha(&encode::routine(&b)));
                match mirror_request(&a, &b, &readable, trusted) {
                    Err(e) => Answer::Unconfirmed {
                        reason: format!("no renaming pairs the two routines: {e}"),
                    },
                    Ok((req, mut assumptions)) => {
                        if quoted {
                            assumptions.push(QUOTED_IDENTIFIER.to_string());
                        }
                        c.renaming = req.renaming.clone();
                        match checker {
                            None => Answer::Unconfirmed {
                                reason: "no checker".into(),
                            },
                            Some(k) => {
                                let m = k.check_mirror(&req);
                                match m.answer {
                                    proof::Answer::Confirmed { theorem } => {
                                        c.assumptions = confirmed_rests_on(
                                            m.scalars.as_deref(),
                                            m.statements.as_deref(),
                                            &m.residual,
                                            assumptions,
                                        );
                                        // `c2a` only where its residual was taken.
                                        c.scalars =
                                            if c.assumptions.iter().any(|a| a == C1_OPERATORS) {
                                                Some("uninterpreted".to_string())
                                            } else {
                                                m.scalars.clone()
                                            };
                                        c.statements = m.statements.clone();
                                        c.errors = m.errors.clone();
                                        Answer::Confirmed {
                                            theorems: vec![theorem],
                                        }
                                    }
                                    proof::Answer::Unconfirmed { reason } => {
                                        c.why = m.why.clone();
                                        Answer::Unconfirmed { reason }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        };
        if let Answer::Unconfirmed { reason } = &c.answer {
            if c.why.is_empty() {
                c.why = vec![reason_code(reason)];
            }
        }
        if let Some(u) = unread {
            // The checker diagnoses the trees without the unread
            // statements; its answer only adds codes.
            let checked = match (&u.trees, checker) {
                (Some((a, b)), Some(k)) => {
                    let renaming =
                        propose_renaming(a, b).unwrap_or_else(|_| partial_renaming(a, b));
                    Some(k.check_mirror(&request_with(a, b, renaming, &readable, trusted).0))
                }
                _ => None,
            };
            never_confirmed(&mut c, u.codes, checked);
        }
        certs.push(c);
    }
    certs
}

/// What the reader left out of a routine pair whose strict read stopped
/// at an unread statement (C0.10).
#[derive(Debug)]
struct Unread {
    /// The `read:*` code of every statement left out, the source's first,
    /// each once.
    codes: Vec<String>,
    /// The two trees without those statements, when both sides read that
    /// far (the output routine exists and is not refused as a whole).
    trees: Option<(Routine, Routine)>,
}

/// The pair read past its unread statements, when the strict read that
/// gives the certificate its reason stopped at one: the source's, else
/// the output's. `None` (today's answer and codes) when that read failed
/// on the routine itself (`routine::read_collecting` fails too) or when
/// both strict reads succeed.
fn left_out(
    a: &Result<Routine, String>,
    a_collected: Option<&Collected>,
    b: Option<&Result<Routine, String>>,
    b_collected: Option<&Collected>,
) -> Option<Unread> {
    // A side's tree and its left-out messages; `None` when it does not
    // read even past its unread statements.
    fn side(
        strict: &Result<Routine, String>,
        collected: Option<&Collected>,
    ) -> Option<(Routine, Vec<String>)> {
        match (strict, collected) {
            (Ok(r), _) => Some((r.clone(), Vec::new())),
            (Err(_), Some(Ok((r, msgs)))) if !msgs.is_empty() => Some((r.clone(), msgs.clone())),
            _ => None,
        }
    }
    let (ra, ma) = side(a, a_collected)?;
    let sb = b.and_then(|b| side(b, b_collected));
    // The read that gives the reason: the source's when it failed, else
    // the output's; it must have left statements out.
    let reason_side = if a.is_err() { &ma } else { &sb.as_ref()?.1 };
    if reason_side.is_empty() {
        return None;
    }
    let mut codes = Vec::new();
    let prefixed = ma
        .iter()
        .map(|m| format!("the source routine is outside the verified shape: {m}"))
        .chain(sb.iter().flat_map(|(_, mb)| {
            mb.iter()
                .map(|m| format!("the output routine is outside the verified shape: {m}"))
        }));
    for reason in prefixed {
        let code = reason_code(&reason);
        if !codes.contains(&code) {
            codes.push(code);
        }
    }
    Some(Unread {
        codes,
        trees: sb.map(|(rb, _)| (ra, rb)),
    })
}

/// A renaming for a diagnostic request when [`propose_renaming`] has none
/// (a left-out statement declared a variable, so the locals no longer
/// pair): parameters by position as far as both sides have them, and the
/// return status. The checker lowers both routines before it looks at the
/// renaming, so the codes of an outside-spec refusal do not depend on it.
fn partial_renaming(a: &Routine, b: &Routine) -> Renaming {
    let mut out: Renaming = a
        .params
        .iter()
        .zip(&b.params)
        .map(|(x, y)| (x.name.clone(), pg_name(&y.name)))
        .collect();
    if a.kind == RoutineKind::Procedure {
        let to = if has_status(b) { STATUS_PARAM } else { STATUS };
        out.push((STATUS.to_string(), to.to_string()));
    }
    out
}

/// The checker's code families that name a construct its lowering refused
/// (formal/lean/README.md, "Refusal diagnostics"). Other codes of an
/// answer about trees with statements left out (`check:differ`,
/// `check:certificate`, `check:signature`, `pair:*`, ...) say only that
/// the trees differ, which leaving statements out makes them do.
const LOWERING_FAMILIES: [&str; 7] = [
    "routine:", "stmt:", "node:", "expr:", "store:", "type:", "name:",
];

/// A routine with a left-out statement is never confirmed (C0.10).
///
/// `c` is the certificate of a routine whose reader left statements out
/// (`read_codes`, their `read:*` codes), and `checked` the checker's
/// answer about the trees without them, if it was asked. Whatever that
/// answer is, a confirmation included, `c` comes out unconfirmed: with
/// its reason unchanged (the strict reader's refusal), nothing a
/// confirmation carries, and `why` = `read_codes`, then the codes of the
/// checker's refusal in [`LOWERING_FAMILIES`], each once. The checker's
/// answer adds codes and nothing else.
fn never_confirmed(
    c: &mut Certificate,
    read_codes: Vec<String>,
    checked: Option<proof::MirrorAnswer>,
) {
    if let Answer::Confirmed { .. } = c.answer {
        // Not reached: a left-out statement means a strict read failed,
        // whose answer is a refusal. Kept so the invariant holds here.
        c.answer = Answer::Unconfirmed {
            reason: "the routine reader left statements out".into(),
        };
    }
    c.assumptions.clear();
    c.scalars = None;
    c.statements = None;
    c.errors = None;
    let mut why = read_codes;
    if let Some(m) = checked {
        if let proof::Answer::Unconfirmed { .. } = m.answer {
            for code in m.why {
                if LOWERING_FAMILIES.iter().any(|f| code.starts_with(f)) && !why.contains(&code) {
                    why.push(code);
                }
            }
        }
    }
    if why.is_empty() {
        if let Answer::Unconfirmed { reason } = &c.answer {
            why.push(reason_code(reason));
        }
    }
    c.why = why;
}

/// The routines of a converted tree that read back, for the target catalog.
pub fn output_routines(texts: &[&str], d: Dialect) -> Vec<Routine> {
    texts
        .iter()
        .flat_map(|t| routines(t, d))
        .filter_map(|(_, _, r, _)| r.ok())
        .collect()
}

/// The routines a tree declares, names as written (quotes removed).
pub fn declared_routines(texts: &[&str], d: Dialect) -> Vec<Vec<String>> {
    texts
        .iter()
        .flat_map(|t| routines(t, d))
        .map(|(name, _, _, _)| name.split('.').map(str::to_string).collect())
        .collect()
}

fn has_double_quoted_name(text: &str, d: Dialect) -> bool {
    parse(text, d)
        .syntax()
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .any(|t| t.kind() == SyntaxKind::QUOTED_IDENT && t.text().starts_with('"'))
}

/// What a confirmation rests on: the C2a slice's residual assumptions
/// when every expression was in it, else the C1 caveats (operators taken
/// to mean the same, runs without errors); then the routine's own. A
/// routine with data statements or queries also rests on `ROWCOUNT_0`,
/// which the C1 caveats do not cover and a checker before 0.15.0 does not
/// name.
fn confirmed_rests_on(
    scalars: Option<&str>,
    statements: Option<&str>,
    residual: &[String],
    own: Vec<String>,
) -> Vec<String> {
    // A residual the registry does not know (a checker newer than this
    // build) is not trusted to mean anything: fall back to the C1 caveats.
    let known = residual
        .iter()
        .all(|r| C2A_RESIDUALS.contains(&r.as_str()) && assumptions::lookup(r).is_some());
    let mut out: Vec<String> = if scalars == Some("c2a") && known {
        residual.to_vec()
    } else {
        vec![C1_OPERATORS.to_string(), C1_NO_ERRORS.to_string()]
    };
    if matches!(statements, Some("c3a" | "c3b")) && !out.iter().any(|a| a == ROWCOUNT_0) {
        out.push(ROWCOUNT_0.to_string());
    }
    for a in own {
        if !out.contains(&a) {
            out.push(a);
        }
    }
    out
}

/// How many top-level statements of `input` are not routines: no
/// certificate covers them.
pub fn outside_routines(input: &str, from: Dialect) -> usize {
    let root = parse(input, from).syntax();
    let mut units = Vec::new();
    for n in root.children() {
        if n.kind() == SyntaxKind::BATCH {
            units.extend(n.children().filter(|c| c.kind().is_unit()));
        } else if n.kind().is_unit() {
            units.push(n);
        }
    }
    units
        .iter()
        .filter(|u| u.kind() != SyntaxKind::ROUTINE)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TSQL: &str = "CREATE PROCEDURE dbo.p @a int, @b int = 2 OUTPUT
AS
BEGIN
  DECLARE @x int = 1;
  IF @a > 1
    SET @x = 2;
  ELSE IF @a < 0
  BEGIN
    SET @x = 3;
  END
  ELSE
    SET @x = 4;
  WHILE @x < 10
    SET @x = @x + 1;
  RETURN @x;
END
";

    #[test]
    fn a_converted_procedure_gets_a_certificate_with_its_renaming() {
        let d = crate::rewrite(TSQL, Dialect::Tsql, Dialect::Postgres);
        let out = d.output().to_string();
        let certs = certify(
            TSQL,
            &out,
            Dialect::Tsql,
            Dialect::Postgres,
            &Trusted::default(),
            None,
        );
        assert_eq!(certs.len(), 1, "{out}");
        let c = &certs[0];
        assert!(c.source.is_some() && c.target.is_some(), "{c:?}\n{out}");
        assert_eq!(
            c.renaming,
            [
                ("@a", "p_a"),
                ("@b", "p_b"),
                (STATUS, "return_code"),
                ("@x", "v_x")
            ]
            .map(|(x, y)| (x.to_string(), y.to_string()))
        );
        assert!(matches!(c.answer, Answer::Unconfirmed { .. }));
    }

    #[test]
    fn a_routine_outside_the_shape_says_why() {
        let src = "CREATE PROCEDURE dbo.p AS BEGIN RAISERROR('x', 16, 1); END\n";
        let d = crate::rewrite(src, Dialect::Tsql, Dialect::Postgres);
        let certs = certify(
            src,
            d.output(),
            Dialect::Tsql,
            Dialect::Postgres,
            &Trusted::default(),
            None,
        );
        let Answer::Unconfirmed { reason } = &certs[0].answer else {
            panic!();
        };
        assert!(reason.contains("outside the verified shape"), "{reason}");
    }

    #[test]
    fn callees_follow_the_schema_rule_and_the_catalog_is_the_output() {
        let src = "CREATE PROCEDURE dbo.q @n int AS BEGIN SET @n = @n + 1; END\nGO\nCREATE PROCEDURE dbo.p AS BEGIN DECLARE @v int = 1; EXEC dbo.q @v; END\n";
        let out = crate::rewrite(src, Dialect::Tsql, Dialect::Postgres)
            .output()
            .to_string();
        let a = routines(src, Dialect::Tsql);
        let b = routines(&out, Dialect::Postgres);
        let readable: Vec<Routine> = b.iter().filter_map(|(_, _, r, _)| r.clone().ok()).collect();
        let (req, asm) = mirror_request(
            a[1].2.as_ref().unwrap(),
            b[1].2.as_ref().unwrap_or_else(|e| panic!("{e}\n{out}")),
            &readable,
            &Trusted {
                declared_routines: declared_routines(&[src], Dialect::Tsql),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(req.routines, [("dbo.q".to_string(), "q".to_string())]);
        assert!(
            req.catalog.iter().any(|(n, _)| n == "q"),
            "{:?}",
            req.catalog
        );
        assert!(asm.contains(&"ASM-DBO-SCHEMA".to_string()));
        let line = req.line(9).unwrap();
        assert!(line.starts_with("sqldiff-check/3\t9\t"), "{line}");
        assert_eq!(line.split('\t').count(), 10, "{line}");
    }

    #[test]
    fn a_callee_is_mapped_only_as_declared_brackets_aside() {
        let mapped = |decl: &str, call: &str| {
            let src = format!(
                "CREATE PROCEDURE {decl} @n int AS BEGIN SET @n = @n + 1; END\nGO\n\
                 CREATE PROCEDURE dbo.p AS BEGIN DECLARE @v int = 1; EXEC {call} @v; END\n"
            );
            let out = crate::rewrite(&src, Dialect::Tsql, Dialect::Postgres)
                .output()
                .to_string();
            let (a, b) = (
                routines(&src, Dialect::Tsql),
                routines(&out, Dialect::Postgres),
            );
            let trusted = Trusted {
                declared_routines: declared_routines(&[&src], Dialect::Tsql),
                ..Default::default()
            };
            let (req, _) = mirror_request(
                a[1].2.as_ref().unwrap(),
                b[1].2.as_ref().unwrap_or_else(|e| panic!("{e}\n{out}")),
                &[],
                &trusted,
            )
            .unwrap();
            !req.routines.is_empty()
        };
        assert!(mapped("dbo.Look", "dbo.Look"));
        assert!(mapped("[dbo].[Look]", "dbo.Look"));
        assert!(mapped("dbo.Look", "[dbo].[Look]"));
        // 2812 in a case-sensitive database (eval/queries/callee-case-mssql.out).
        assert!(!mapped("dbo.Look", "dbo.LOOK"));
        assert!(!mapped("dbo.Look", "DBO.Look"));
    }

    #[test]
    fn only_plain_tables_named_nowhere_else_reach_the_checker() {
        let ddl = "CREATE TABLE dbo.Acct (Id int NOT NULL, Qty int NULL, \
                   CONSTRAINT PK_Acct PRIMARY KEY (Id));\n";
        let proc = "CREATE PROCEDURE dbo.p @id int AS\nBEGIN\n  \
                    UPDATE dbo.Acct SET Qty = 1 WHERE Id = @id;\nEND\n";
        let tables = |extra: &str| {
            let src = format!("{ddl}{extra}");
            let out = crate::rewrite(&src, Dialect::Tsql, Dialect::Postgres)
                .output()
                .to_string();
            let p = crate::rewrite(proc, Dialect::Tsql, Dialect::Postgres)
                .output()
                .to_string();
            Tables::read(&[&src, proc], &[&out, &p], Dialect::Tsql, Dialect::Postgres)
        };
        let t = tables("");
        let acct = t.get("acct").expect("a plain table");
        let cols: Vec<(&str, &str, bool, bool)> = acct
            .cols
            .iter()
            .map(|c| (c.name.as_str(), c.tsql.as_str(), c.not_null, c.key))
            .collect();
        assert_eq!(
            cols,
            [("id", "Id", true, true), ("qty", "Qty", false, false)]
        );
        // The routine's request names the table it writes, as written.
        let a = routines(proc, Dialect::Tsql).remove(0).2.unwrap();
        let p = crate::rewrite(proc, Dialect::Tsql, Dialect::Postgres)
            .output()
            .to_string();
        let b = routines(&p, Dialect::Postgres).remove(0).2.unwrap();
        let trusted = Trusted {
            tables: t.clone(),
            ..Default::default()
        };
        let (req, asm) = mirror_request(&a, &b, &[], &trusted).unwrap();
        assert_eq!(req.tables, [("dbo.Acct".to_string(), "acct".to_string())]);
        assert_eq!(req.table_catalog, std::slice::from_ref(acct));
        for x in [
            TABLES_AS_DECLARED,
            "ASM-DBO-SCHEMA",
            "ASM-OBSERVER-ROWCOUNT",
        ] {
            assert!(asm.iter().any(|y| y == x), "{x}: {asm:?}");
        }
        // A plain index does not keep it out (C3c).
        assert_eq!(
            tables("CREATE INDEX ix ON dbo.Acct (Qty);\n").get("acct"),
            Some(acct)
        );
        // Anything else that names the table keeps it out as a table the
        // routines may write; (C3d part 2) what cannot change a read leaves it
        // a read-only table, the rest keeps it out altogether.
        for (extra, read_only) in [
            ("CREATE UNIQUE INDEX ix ON dbo.Acct (Qty);\n", true),
            (
                "CREATE INDEX ix ON dbo.Acct (Qty) WHERE Qty IS NOT NULL;\n",
                true,
            ),
            // A trigger fires on writes only; its conversion's `CREATE
            // TRIGGER ... EXECUTE FUNCTION` runs no text (`dynamic_sql`).
            (
                "GO\nCREATE TRIGGER tr ON dbo.Acct AFTER INSERT AS BEGIN SET NOCOUNT ON; END\n",
                true,
            ),
            (
                "ALTER TABLE dbo.Acct ADD CONSTRAINT ck CHECK (Qty > 0);\n",
                true,
            ),
            (
                "CREATE TABLE dbo.Line (Id int NOT NULL REFERENCES dbo.Acct (Id));\n",
                true,
            ),
            (
                "CREATE TABLE dbo.Acct (Id int NOT NULL PRIMARY KEY);\n",
                false,
            ),
            (
                "GO\nCREATE PROCEDURE dbo.q AS BEGIN ALTER TABLE dbo.Acct ADD z int NULL; END\n",
                false,
            ),
        ] {
            let t = tables(extra);
            assert!(t.writable("acct").is_none(), "{extra}");
            assert_eq!(t.get("acct").is_some(), read_only, "{extra}");
        }
    }

    /// C3d: every table of a join, on either side and in a comma list, is
    /// sent to the checker; a table the routine does not read is not.
    #[test]
    fn the_tables_of_a_join_reach_the_checker() {
        let ddl =
            "CREATE TABLE dbo.Ord (Id int NOT NULL PRIMARY KEY, Qty int NULL, CustId int NULL);\n\
                   CREATE TABLE dbo.Cust (Id int NOT NULL PRIMARY KEY, Score int NULL);\n\
                   CREATE TABLE dbo.Two (A int NOT NULL, B int NOT NULL, V int NULL, \
                   CONSTRAINT PK_Two PRIMARY KEY (A, B));\n\
                   CREATE TABLE dbo.Idle (Id int NOT NULL PRIMARY KEY);\n";
        let proc = "CREATE PROCEDURE dbo.p @id int, @q int OUTPUT AS\nBEGIN\n  \
                    SELECT @q = c.Score FROM dbo.Ord AS o JOIN dbo.Cust c ON c.Id = o.CustId \
                    WHERE o.Id = @id;\n  \
                    SELECT @q = COUNT(*) FROM dbo.Ord o, dbo.Two CROSS JOIN dbo.Cust;\nEND\n";
        let convert = |s: &str| {
            crate::rewrite(s, Dialect::Tsql, Dialect::Postgres)
                .output()
                .to_string()
        };
        let t = Tables::read(&[ddl], &[&convert(ddl)], Dialect::Tsql, Dialect::Postgres);
        let a = routines(proc, Dialect::Tsql).remove(0).2.unwrap();
        let b = routines(&convert(proc), Dialect::Postgres)
            .remove(0)
            .2
            .unwrap();
        let trusted = Trusted {
            tables: t.clone(),
            ..Default::default()
        };
        let (r, asm) = mirror_request(&a, &b, &[], &trusted).unwrap();
        let names: Vec<&str> = r.table_catalog.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["ord", "cust", "two"], "{r:?}");
        assert_eq!(
            r.tables,
            [("dbo.Ord", "ord"), ("dbo.Cust", "cust"), ("dbo.Two", "two")]
                .map(|(x, y)| (x.to_string(), y.to_string()))
        );
        assert!(asm.iter().any(|x| x == TABLES_AS_DECLARED), "{asm:?}");
        // Reads send no rows-affected count.
        assert!(!asm.iter().any(|x| x == "ASM-OBSERVER-ROWCOUNT"), "{asm:?}");
        // A table named in another case than its DDL, or without the schema
        // its DDL gives, is not sent, whatever the collation the deployment
        // states: a case-sensitive SQL Server database raises 208 for
        // `dbo.ORD` where PostgreSQL folds it to `ord` (review of C3d).
        for (spelt, binary) in [
            ("dbo.ORD", false),
            ("dbo.ord", false),
            ("ORD", false),
            ("Ord", false),
            ("DBO.Ord", false),
            ("dbo.ORD", true),
        ] {
            let p = format!(
                "CREATE PROCEDURE dbo.p @id int, @q int OUTPUT AS\nBEGIN\n  \
                 SELECT @q = c.Score FROM {spelt} AS o JOIN dbo.Cust c ON c.Id = o.CustId \
                 WHERE o.Id = @id;\nEND\n"
            );
            let a = routines(&p, Dialect::Tsql).remove(0).2.unwrap();
            let b = routines(&convert(&p), Dialect::Postgres)
                .remove(0)
                .2
                .unwrap();
            let trusted = Trusted {
                tables: t.clone(),
                binary_collation: binary,
                ..Default::default()
            };
            let (r, _) = mirror_request(&a, &b, &[], &trusted).unwrap();
            let names: Vec<&str> = r.table_catalog.iter().map(|t| t.name.as_str()).collect();
            assert_eq!(names, ["cust"], "{spelt}: {r:?}");
        }
    }

    #[test]
    fn string_columns_defaults_and_plain_indexes_reach_the_checker() {
        let ddl = "CREATE TABLE dbo.Cust (Id int NOT NULL PRIMARY KEY, Name varchar(20) NOT NULL, \
                   City nvarchar(30) NULL DEFAULT 'Paris', Qty int NOT NULL DEFAULT 0);\n";
        let proc = "CREATE PROCEDURE dbo.p @id int AS\nBEGIN\n  \
                    INSERT INTO dbo.Cust (Id, Name) VALUES (@id, 'a');\nEND\n";
        let read_proc = "CREATE PROCEDURE dbo.r @id int, @c nvarchar(30) OUTPUT AS\nBEGIN\n  \
                         SELECT @c = City FROM dbo.Cust WHERE Id = @id;\nEND\n";
        let convert = |s: &str| {
            crate::rewrite(s, Dialect::Tsql, Dialect::Postgres)
                .output()
                .to_string()
        };
        // Both trees as given (the output edited by hand where a case needs it).
        let tables_of =
            |src: &str, out: &str| Tables::read(&[src], &[out], Dialect::Tsql, Dialect::Postgres);
        let tables = |extra: &str| {
            let src = format!("{ddl}{extra}");
            tables_of(&src, &convert(&src))
        };
        let t = tables("");
        let cust = t.get("cust").expect("a plain table");
        type Col<'a> = (&'a str, MirrorType, Option<MirrorDefault>);
        let cols: Vec<Col> = cust
            .cols
            .iter()
            .map(|c| (c.name.as_str(), c.ty, c.default.clone()))
            .collect();
        assert_eq!(
            cols,
            [
                ("id", MirrorType::Int, None),
                (
                    "name",
                    MirrorType::Str {
                        len: 20,
                        wide: false
                    },
                    None
                ),
                (
                    "city",
                    MirrorType::Str {
                        len: 30,
                        wide: true
                    },
                    Some(MirrorDefault::Str("Paris".into()))
                ),
                ("qty", MirrorType::Int, Some(MirrorDefault::Int(0))),
            ]
        );
        // A write to a table with a varchar column names ANSI_PADDING; a read
        // does not.
        let req = |p: &str| {
            let a = routines(p, Dialect::Tsql).remove(0).2.unwrap();
            let b = routines(&convert(p), Dialect::Postgres)
                .remove(0)
                .2
                .unwrap();
            let trusted = Trusted {
                tables: t.clone(),
                ..Default::default()
            };
            mirror_request(&a, &b, &[], &trusted).unwrap()
        };
        let (r, asm) = req(proc);
        assert_eq!(r.table_catalog, std::slice::from_ref(cust));
        assert!(asm.iter().any(|x| x == ANSI_PADDING), "{asm:?}");
        let (r, asm) = req(read_proc);
        assert_eq!(r.table_catalog, std::slice::from_ref(cust));
        assert!(!asm.iter().any(|x| x == ANSI_PADDING), "{asm:?}");
        // Plain indexes whose entries fit both engines keep the table in.
        for extra in [
            "CREATE INDEX IX_Name ON dbo.Cust (Name);\n",
            "CREATE NONCLUSTERED INDEX IX_City ON dbo.Cust (City DESC) INCLUDE (Qty);\n",
        ] {
            assert!(tables(extra).writable("cust").is_some(), "{extra}");
        }
        // Anything else keeps it out.
        for extra in [
            "CREATE UNIQUE INDEX UX_Name ON dbo.Cust (Name);\n",
            "CREATE INDEX IX_Name ON dbo.Cust (Name) WHERE Name > 'a';\n",
            "CREATE INDEX IX_Name ON dbo.Cust (Name) WITH (FILLFACTOR = 80);\n",
            "CREATE INDEX IX_Nope ON dbo.Cust (Nope);\n",
            "SET ANSI_PADDING OFF;\n",
        ] {
            assert!(tables(extra).writable("cust").is_none(), "{extra}");
        }
        // An index whose PostgreSQL entry may exceed the btree limit: 700
        // characters may take 2800 bytes (eval/tables T8), though 700 bytes
        // fit SQL Server's 1700.
        let wide = "CREATE TABLE dbo.W (Id int NOT NULL PRIMARY KEY, V varchar(700) NULL);\n\
                    CREATE INDEX IX_V ON dbo.W (V);\n";
        assert!(tables_of(wide, &convert(wide)).writable("w").is_none());
        let wide_ok = wide.replace("700", "600");
        assert!(tables_of(&wide_ok, &convert(&wide_ok))
            .writable("w")
            .is_some());
        // Types and defaults must pair: an output edited to another length,
        // to text, without the default, with another default.
        let out = convert(ddl);
        assert!(tables_of(ddl, &out).writable("cust").is_some());
        // An index on another spelling of the table's name, on one side
        // only (the other side's tree as converted from the bare DDL), and
        // padding turned off for the database or the server.
        for extra in [
            "CREATE INDEX IX_Name ON MyDb.dbo.Cust (Name);\n",
            "CREATE INDEX IX_Name ON srv.MyDb.dbo.Cust (Name);\n",
            "ALTER DATABASE CURRENT SET ANSI_PADDING OFF;\n",
            "EXEC sp_configure 'user options', 16;\n",
            "ALTER DATABASE CURRENT COLLATE Latin1_General_100_BIN2;\n",
        ] {
            let src = format!("{ddl}{extra}");
            assert!(tables_of(&src, &out).writable("cust").is_none(), "{extra}");
        }
        // After a change of the database's collation no table is sent, not
        // even read-only (C3d part 2).
        let src = format!("{ddl}ALTER DATABASE CURRENT COLLATE Latin1_General_100_BIN2;\n");
        assert!(tables_of(&src, &out).get("cust").is_none());
        for extra in [
            "CREATE INDEX ix_name ON public.cust (name);\n",
            "CREATE INDEX ix_name ON \"public\".\"cust\" (name);\n",
            "CREATE INDEX ix_name ON mydb.public.cust (name);\n",
            "CREATE INDEX ix_name ON dbo.cust (name);\n",
        ] {
            let edited = format!("{out}{extra}");
            assert!(
                tables_of(ddl, &edited).writable("cust").is_none(),
                "{extra}"
            );
        }
        for (from, to) in [
            ("Name varchar(20)", "Name varchar(21)"),
            ("Name varchar(20)", "Name text"),
            (" DEFAULT 'Paris'", ""),
            ("DEFAULT 'Paris'", "DEFAULT 'Lyon'"),
            ("DEFAULT 0", "DEFAULT 1"),
        ] {
            assert!(out.contains(from), "{from} in {out}");
            let edited = out.replace(from, to);
            assert!(tables_of(ddl, &edited).writable("cust").is_none(), "{to}");
        }
        // A default only one side shows, the other way round.
        let src = ddl.replace(" DEFAULT 0", "");
        assert!(tables_of(&src, &out).writable("cust").is_none());
    }

    /// C3d part 2: a table the routines only read may carry what no read
    /// sees; it is sent read-only, its out-of-slice columns opaque, its key
    /// only when both sides declare it whole. What can change a read keeps
    /// it out.
    #[test]
    fn read_only_tables_reach_the_checker() {
        let ddl = "CREATE TABLE dbo.Item (Id int IDENTITY(1,1) NOT NULL PRIMARY KEY, \
                   Name varchar(20) NOT NULL, Price decimal(12,2) NOT NULL, \
                   Added datetime2 NOT NULL DEFAULT SYSDATETIME());\n\
                   CREATE TABLE dbo.Line (OrdId int NOT NULL, LineNo int NOT NULL, \
                   ItemId int NOT NULL REFERENCES dbo.Item (Id), Qty int NOT NULL CHECK (Qty > 0), \
                   Code char(4) NOT NULL, CONSTRAINT PK_Line PRIMARY KEY (OrdId, LineNo));\n\
                   CREATE TABLE dbo.Tag (Code char(4) NOT NULL PRIMARY KEY, N int NULL);\n";
        let convert = |s: &str| {
            crate::rewrite(s, Dialect::Tsql, Dialect::Postgres)
                .output()
                .to_string()
        };
        let tables_of =
            |src: &str, out: &str| Tables::read(&[src], &[out], Dialect::Tsql, Dialect::Postgres);
        let tables = |extra: &str| {
            let src = format!("{ddl}{extra}");
            tables_of(&src, &convert(&src))
        };
        let t = tables("");
        let shape = |n: &str| {
            let m = t.get(n).unwrap_or_else(|| panic!("{n} is sent"));
            assert!(m.read_only, "{n}");
            m.cols
                .iter()
                .map(|c| (c.name.clone(), c.ty, c.not_null, c.key))
                .collect::<Vec<_>>()
        };
        let col = |n: &str, ty, nn, k| (n.to_string(), ty, nn, k);
        let vc20 = MirrorType::Str {
            len: 20,
            wide: false,
        };
        assert_eq!(
            shape("item"),
            [
                col("id", MirrorType::Int, true, true),
                col("name", vc20, true, false),
                col("price", MirrorType::Opaque, false, false),
                col("added", MirrorType::Opaque, false, false),
            ]
        );
        assert_eq!(
            shape("line"),
            [
                col("ordid", MirrorType::Int, true, true),
                col("lineno", MirrorType::Int, true, true),
                col("itemid", MirrorType::Int, true, false),
                col("qty", MirrorType::Int, true, false),
                col("code", MirrorType::Opaque, false, false),
            ]
        );
        // The key is the opaque column: sent as declared, the checker reads
        // the table without a key.
        assert_eq!(
            shape("tag"),
            [
                col("code", MirrorType::Opaque, false, true),
                col("n", MirrorType::Int, false, false),
            ]
        );
        // No default is sent, and a T-SQL column's spelling is kept.
        assert!(t
            .get("item")
            .unwrap()
            .cols
            .iter()
            .all(|c| c.default.is_none()));
        assert_eq!(t.get("line").unwrap().cols[2].tsql, "ItemId");
        // What cannot change a read leaves the tables read-only.
        for extra in [
            "CREATE UNIQUE INDEX UX_Name ON dbo.Item (Name) WHERE Name > 'a';\n",
            "ALTER TABLE dbo.Line ADD CONSTRAINT CK_Qty2 CHECK (Qty < 1000);\n",
            "ALTER TABLE dbo.Line NOCHECK CONSTRAINT ALL;\n",
            "GO\nCREATE VIEW dbo.V AS SELECT Id, Name FROM dbo.Item;\nGO\n",
            "EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'items', \
             @level0type = N'SCHEMA', @level0name = 'dbo', @level1type = N'TABLE', @level1name = 'Item';\n",
            "CREATE TABLE dbo.Other (Id int NOT NULL PRIMARY KEY, ItemId int NULL REFERENCES dbo.Item (Id));\n",
        ] {
            let t = tables(extra);
            for n in ["item", "line"] {
                assert!(t.get(n).is_some_and(|m| m.read_only), "{n}: {extra}");
            }
        }
        // What can, or what the reader cannot show harmless, keeps the table
        // out (and only that table). An index or a view named like the table
        // may hide it on PostgreSQL's search_path.
        for (extra, out) in [
            ("CREATE INDEX Item ON dbo.Line (Qty);\n", "item"),
            ("GO\nCREATE VIEW rpt.Item AS SELECT Id FROM dbo.Item;\nGO\n", "item"),
            ("GO\nCREATE FUNCTION dbo.f (@n varchar(20)) RETURNS TABLE WITH SCHEMABINDING AS \
              RETURN SELECT 1 AS ok WHERE @n = USER_NAME();\nGO\n\
              CREATE SECURITY POLICY dbo.P ADD FILTER PREDICATE dbo.f(Name) ON dbo.Item;\n", "item"),
            ("ALTER TABLE dbo.Item ADD Extra int NULL;\n", "item"),
            ("ALTER TABLE dbo.Item ALTER COLUMN Name varchar(30) NOT NULL;\n", "item"),
            ("ALTER TABLE dbo.Item DROP CONSTRAINT PK_Item;\n", "item"),
            ("GRANT SELECT ON dbo.Item TO someone;\n", "item"),
            ("INSERT INTO dbo.Item (Name, Price, Added) VALUES ('a', 1, '2020-01-01');\n", "item"),
            ("CREATE TABLE dbo.Item (Id int NOT NULL PRIMARY KEY);\n", "item"),
            ("GO\nCREATE PROCEDURE dbo.q AS BEGIN ALTER TABLE dbo.Line DROP COLUMN Code; END\n", "line"),
            ("GO\nCREATE PROCEDURE dbo.q AS BEGIN DECLARE @s nvarchar(100) = N'DROP TABLE dbo.Line'; END\n", "line"),
        ] {
            let t = tables(extra);
            assert!(t.get(out).is_none(), "{out}: {extra}");
            let other = if out == "item" { "line" } else { "item" };
            assert!(t.get(other).is_some_and(|m| m.read_only), "{other}: {extra}");
        }
        // Dynamic SQL anywhere, or a T-SQL trigger that runs it, keeps every
        // table out.
        for extra in [
            // C3d part 2, review: an undeclared procedure, `sp_rename` among
            // them, may do anything (`dynamic_sql`).
            "EXEC sp_rename 'dbo.Item.Name', 'Title', 'COLUMN';\n",
            "GO\nCREATE PROCEDURE dbo.q @s nvarchar(100) AS BEGIN EXEC (@s); END\n",
            "GO\nCREATE TRIGGER dbo.tr ON dbo.Other AFTER INSERT AS BEGIN EXEC sp_executesql N'SELECT 1'; END\n",
        ] {
            let src = format!("{ddl}{extra}");
            let t = tables_of(&src, &convert(ddl));
            assert!(t.get("item").is_none() && t.get("line").is_none(), "{extra}");
        }
        // The two definitions must pair: the same column names in the same
        // order; a column whose types do not pair is opaque; a key that the
        // other side does not declare is no key; nullability is claimed only
        // where both sides say NOT NULL.
        let out = convert(ddl);
        for (from, to) in [
            ("ItemId integer", "ItemNo integer"),
            ("Qty integer NOT NULL CHECK (Qty > 0),", ""),
        ] {
            assert!(out.contains(from), "{from} in {out}");
            assert!(
                tables_of(ddl, &out.replace(from, to)).get("line").is_none(),
                "{to}"
            );
        }
        let edited = out
            .replace("Name varchar(20) NOT NULL", "Name varchar(21) NULL")
            .replace(
                "CONSTRAINT PK_Line PRIMARY KEY (OrdId, LineNo)",
                "UNIQUE (OrdId, LineNo)",
            );
        assert!(edited != out);
        let t = tables_of(ddl, &edited);
        assert_eq!(t.get("item").unwrap().cols[1].ty, MirrorType::Opaque);
        assert!(t.get("line").unwrap().cols.iter().all(|c| !c.key));
        let edited = out.replace("Qty integer NOT NULL", "Qty integer NULL");
        assert!(edited != out);
        assert!(!tables_of(ddl, &edited).get("line").unwrap().cols[3].not_null);
        // A routine that writes a read-only table sends it as it is: the
        // checker refuses the write (`stmt:update:table:read-only`).
        let proc = "CREATE PROCEDURE dbo.p @id int AS\nBEGIN\n  \
                    UPDATE dbo.Line SET Qty = 1 WHERE OrdId = @id AND LineNo = 1;\nEND\n";
        let a = routines(proc, Dialect::Tsql).remove(0).2.unwrap();
        let b = routines(&convert(proc), Dialect::Postgres)
            .remove(0)
            .2
            .unwrap();
        let trusted = Trusted {
            tables: tables(""),
            ..Default::default()
        };
        let (req, _) = mirror_request(&a, &b, &[], &trusted).unwrap();
        assert_eq!(req.table_catalog.len(), 1);
        assert!(req.table_catalog[0].read_only);
        assert!(req.line(1).unwrap().contains("=r"));
    }

    #[test]
    fn a_transaction_outside_a_try_names_the_doomed_caller_transaction() {
        let bracket = "CREATE PROCEDURE dbo.p @a int AS\nBEGIN\n  SET XACT_ABORT ON;\n  \
                       BEGIN TRAN;\n  SET @a = @a + 1;\n  COMMIT;\nEND\n";
        let idiom = "CREATE PROCEDURE dbo.p @a int AS\nBEGIN\n  SET XACT_ABORT ON;\n  \
                     BEGIN TRY\n    BEGIN TRAN;\n    SET @a = @a + 1;\n    COMMIT;\n  END TRY\n  \
                     BEGIN CATCH\n    IF @@TRANCOUNT > 0 ROLLBACK;\n    THROW;\n  END CATCH\nEND\n";
        let read = |src: &str| routines(src, Dialect::Tsql).remove(0).2.unwrap();
        assert!(begins_tran_outside_try(&read(bracket).body.body));
        assert!(!begins_tran_outside_try(&read(idiom).body.body));
    }

    /// C3d part 2, review (F4): dynamic SQL is an allow-list. An `EXEC` keeps
    /// every table out unless it calls a procedure the tree declares, by
    /// name, or sets an extended property; on PostgreSQL a `DO` block, a body
    /// no scan reads and a top-level call of an undeclared routine do too.
    /// Each of these ran DDL at deployment that no name scan saw (a policy,
    /// row-level security, a rename).
    #[test]
    fn only_calls_the_tree_shows_are_not_dynamic() {
        let ddl = "CREATE TABLE dbo.Item (Id int NOT NULL PRIMARY KEY, Name varchar(20) NOT NULL, \
                   Price decimal(12,2) NOT NULL);\n\
                   CREATE TABLE dbo.Line (OrdId int NOT NULL PRIMARY KEY, ItemId int NOT NULL);\n";
        let convert = |s: &str| {
            crate::rewrite(s, Dialect::Tsql, Dialect::Postgres)
                .output()
                .to_string()
        };
        let both_in = |src: &str, out: &str| {
            let t = Tables::read(&[src], &[out], Dialect::Tsql, Dialect::Postgres);
            let (i, l) = (t.get("item").is_some(), t.get("line").is_some());
            assert_eq!(i, l, "{src}\n{out}");
            i
        };
        let out = convert(ddl);
        assert!(both_in(ddl, &out));
        // T-SQL, at the top level or in a routine.
        for extra in [
            "GO\nDECLARE @s nvarchar(100) = N'SELECT 1';\nEXEC sp_sqlexec @s;\n",
            "GO\nDECLARE @s nvarchar(100) = N'SELECT 1';\nEXEC [sp_executesql] @s;\n",
            "GO\nDECLARE @s nvarchar(100) = N'SELECT 1';\nEXEC sys.[sp_executesql] @s;\n",
            "GO\nDECLARE @s nvarchar(100) = N'SELECT 1';\nEXEC sp_MSforeachtable @s;\n",
            "GO\nDECLARE @h int, @s nvarchar(100) = N'SELECT 1';\nEXEC sp_prepexec @h OUTPUT, NULL, @s;\n",
            "GO\nDECLARE @c nvarchar(100) = N'dbo.' + NCHAR(84) + N'1.V';\nEXEC sp_rename @c, N'Z', 'COLUMN';\n",
            "GO\nEXEC dbo.Setup;\n",
            "GO\nEXECUTE AS USER = 'someone';\n",
            "GO\nsp_executesql N'SELECT 1';\n",
            "GO\nSetup;\n",
            "GO\nSELECT * FROM OPENQUERY(loopback, 'SELECT 1');\n",
            "GO\nCREATE PROCEDURE dbo.Clr AS EXTERNAL NAME A.B.C;\nGO\nEXEC dbo.Clr;\n",
            "GO\nCREATE PROCEDURE dbo.q AS BEGIN EXEC dbo.NotHere; END\n",
            "GO\nCREATE PROCEDURE dbo.q AS BEGIN EXEC dbo.Item_Name; END\n",
            "GO\nCREATE PROCEDURE sp_mine AS BEGIN RETURN 1; END\nGO\nEXEC sp_mine;\n",
        ] {
            assert!(!both_in(&format!("{ddl}{extra}"), &out), "{extra}");
        }
        for extra in [
            "GO\nEXEC sys.sp_addextendedproperty @name = N'd', @value = N'x', \
             @level0type = N'SCHEMA', @level0name = 'dbo';\n",
            "GO\nEXEC [sp_updateextendedproperty] @name = N'd', @value = N'y';\n",
            "GO\nCREATE PROCEDURE dbo.R AS BEGIN RETURN 1; END\nGO\n\
             CREATE PROCEDURE [dbo].[q] WITH EXECUTE AS OWNER AS BEGIN EXEC [DBO].[r]; END\n\
             GO\nEXEC dbo.q;\n",
        ] {
            assert!(both_in(&format!("{ddl}{extra}"), &out), "{extra}");
        }
        // PostgreSQL.
        for extra in [
            "DO $$ BEGIN EXECUTE 'CREATE POLICY p ON ' || chr(105) || 'tem USING (false)'; END $$;\n",
            "DO 'BEGIN NULL; END';\n",
            "DO LANGUAGE plpgsql $$ BEGIN NULL; END $$;\n",
            "CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS 'BEGIN NULL; END';\n",
            "CREATE FUNCTION f() RETURNS void LANGUAGE sql AS 'SELECT 1';\nSELECT f();\n",
            "CREATE FUNCTION f() RETURNS int LANGUAGE plpython3u AS $$ return 1 $$;\n",
            "CREATE FUNCTION f(int) RETURNS int AS 'MODULE_PATHNAME', 'f' LANGUAGE C;\n",
            "CREATE FUNCTION f(int) RETURNS int LANGUAGE internal AS 'int4abs';\n",
            "SELECT not_declared();\n",
            "SELECT set_config('search_path', 's', false);\n",
            "CALL elsewhere.p();\n",
            "INSERT INTO other (a) VALUES (g());\n",
            "CREATE FUNCTION pg_mine() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$;\nSELECT pg_mine();\n",
            "COPY other FROM PROGRAM 'cat x';\n",
        ] {
            assert!(!both_in(ddl, &format!("{out}{extra}")), "{extra}");
        }
        for extra in [
            "CREATE FUNCTION h() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$;\nSELECT h();\n",
            "CREATE PROCEDURE p() LANGUAGE plpgsql AS $$ BEGIN NULL; END $$;\nCALL p();\n",
            "SELECT lower('A'), pg_catalog.upper('b'), CAST(1 AS numeric(10, 2)), '1'::varchar(3);\n",
        ] {
            assert!(both_in(ddl, &format!("{out}{extra}")), "{extra}");
        }
    }

    /// C3d part 2, review: a table PostgreSQL may not resolve to its own
    /// definition is sent neither plain nor read-only: a `pg_` name
    /// (`pg_catalog` is searched first: `pg_user` is the catalog's view), or
    /// one another schema also defines, readable or not (F5, F7).
    #[test]
    fn tables_another_relation_may_hide_are_not_sent() {
        let plain = "CREATE TABLE dbo.Acct (Id int NOT NULL PRIMARY KEY, Qty int NULL);\n";
        let ro = "CREATE TABLE dbo.Item (Id int NOT NULL PRIMARY KEY, Price decimal(12,2) NULL);\n";
        let convert = |s: &str| {
            crate::rewrite(s, Dialect::Tsql, Dialect::Postgres)
                .output()
                .to_string()
        };
        let read =
            |src: &str, out: &str| Tables::read(&[src], &[out], Dialect::Tsql, Dialect::Postgres);
        let base = format!("{plain}{ro}");
        let t = read(&base, &convert(&base));
        assert!(t.get("acct").is_some_and(|m| !m.read_only));
        assert!(t.get("item").is_some_and(|m| m.read_only));
        let pg = base.replace("Acct", "pg_acct").replace("Item", "Pg_Item");
        let t = read(&pg, &convert(&pg));
        assert!(t.get("pg_acct").is_none() && t.get("pg_item").is_none());
        // Another schema's definition, plain, read-only or neither, on either
        // side, keeps the table of that last name out, and only it.
        for (src_extra, out_extra) in [
            (
                "",
                "CREATE TABLE s.acct (id integer NOT NULL PRIMARY KEY, qty integer);\n",
            ),
            (
                "",
                "CREATE TABLE s.item (id integer NOT NULL PRIMARY KEY, price numeric(12,2));\n",
            ),
            ("", "CREATE TABLE s.acct (id integer) INHERITS (u);\n"),
            ("CREATE TABLE rpt.Acct (Id int NOT NULL PRIMARY KEY);\n", ""),
            (
                "CREATE TABLE rpt.Item (Id int NOT NULL, X decimal(1,0) NULL);\n",
                "",
            ),
        ] {
            let src = format!("{base}{src_extra}");
            let t = read(&src, &format!("{}{out_extra}", convert(&base)));
            let (gone, kept) = if format!("{src_extra}{out_extra}")
                .to_lowercase()
                .contains("acct")
            {
                ("acct", "item")
            } else {
                ("item", "acct")
            };
            assert!(t.get(gone).is_none(), "{gone}: {src_extra}{out_extra}");
            assert!(t.get(kept).is_some(), "{kept}: {src_extra}{out_extra}");
        }
    }

    #[test]
    fn a_trigger_naming_its_function_runs_no_text() {
        let dynamic = |text: &str| {
            parse(text, Dialect::Postgres)
                .syntax()
                .descendants()
                .filter(|n| n.kind() == SyntaxKind::STATEMENT)
                .any(|n| dynamic_sql(&n, Dialect::Postgres, &[]))
        };
        for text in [
            "CREATE TRIGGER tr AFTER INSERT ON acct FOR EACH ROW EXECUTE FUNCTION f();\n",
            "CREATE OR REPLACE TRIGGER tr AFTER INSERT ON acct FOR EACH ROW EXECUTE PROCEDURE f();\n",
            "CREATE CONSTRAINT TRIGGER tr AFTER INSERT ON acct FOR EACH ROW EXECUTE FUNCTION f();\n",
        ] {
            assert!(!dynamic(text), "{text}");
        }
        // Elsewhere `EXECUTE function` runs the text of a variable so named,
        // and an event trigger fires on DDL.
        for text in [
            "DO $$ DECLARE function text := 'SELECT 1'; BEGIN EXECUTE function; END $$;\n",
            "EXECUTE function;\n",
            "CREATE EVENT TRIGGER et ON ddl_command_end EXECUTE FUNCTION f();\n",
        ] {
            assert!(dynamic(text), "{text}");
        }
    }

    #[test]
    fn names_fold_as_postgresql_folds_them() {
        assert_eq!(pg_name("Acct"), "acct");
        // PostgreSQL folds only ASCII letters in a UTF-8 database.
        assert_eq!(pg_name("Ärger"), "Ärger");
        assert_eq!(pg_name("\"Acct"), "\"Acct");
    }

    #[test]
    fn only_qualified_ascii_names_are_mapped() {
        let src = "CREATE PROCEDURE sales.p @x int OUTPUT AS\nBEGIN\n  EXEC q @x OUTPUT;\n  EXEC dbo.r @x OUTPUT;\n  EXEC dbo.Ärger @x OUTPUT;\nEND\n";
        let out = crate::rewrite(src, Dialect::Tsql, Dialect::Postgres)
            .output()
            .to_string();
        let a = routines(src, Dialect::Tsql).remove(0).2.unwrap();
        let b = routines(&out, Dialect::Postgres)
            .remove(0)
            .2
            .unwrap_or_else(|e| panic!("{e}\n{out}"));
        let declared = |names: &[&[&str]]| Trusted {
            declared_routines: names
                .iter()
                .map(|n| n.iter().map(|p| p.to_string()).collect())
                .collect(),
            ..Default::default()
        };
        let all = declared(&[&["q"], &["dbo", "r"], &["dbo", "Ärger"]]);
        let (req, asm) = mirror_request(&a, &b, &[], &all).unwrap();
        // `q` resolves through sales first in T-SQL; Ä folds differently.
        assert_eq!(req.routines, [("dbo.r".to_string(), "r".to_string())]);
        assert!(
            asm.iter().any(|x| x == "ASM-CALLEE-CONFIRMED:dbo.r"),
            "{asm:?}"
        );
        // A callee the tree does not declare stays unknown.
        let (req, _) = mirror_request(&a, &b, &[], &Trusted::default()).unwrap();
        assert!(req.routines.is_empty(), "{:?}", req.routines);
        // Only the declared spelling reaches it, whatever the collation: a
        // case-sensitive database raises 2812 for `dbo.r` against `dbo.R`
        // (eval/queries/callee-case-mssql.out), binary collation or not.
        for binary_collation in [false, true] {
            let other = Trusted {
                binary_collation,
                ..declared(&[&["dbo", "R"]])
            };
            let (req, _) = mirror_request(&a, &b, &[], &other).unwrap();
            assert!(req.routines.is_empty(), "{:?}", req.routines);
        }
        let schema = declared(&[&["DBO", "r"]]);
        let (req, _) = mirror_request(&a, &b, &[], &schema).unwrap();
        assert!(req.routines.is_empty(), "{:?}", req.routines);
    }

    #[test]
    fn routines_pair_by_name_and_quoted_names_are_named() {
        // The first routine has no output routine; the second is not paired
        // with the first one's place.
        let src = "CREATE PROCEDURE dbo.p1 @v sql_variant AS\nBEGIN\n  SET @v = NULL;\nEND\nGO\nCREATE PROCEDURE dbo.p2 @a int OUTPUT AS\nBEGIN\n  SET @a = \"a\";\nEND\n";
        let out = "CREATE OR REPLACE PROCEDURE p2(INOUT p_a integer, INOUT return_code integer) LANGUAGE plpgsql AS $$\nBEGIN\n  return_code := 0;\n  p_a := p_a;\nEND;\n$$;\n";
        let certs = certify(
            src,
            out,
            Dialect::Tsql,
            Dialect::Postgres,
            &Trusted::default(),
            None,
        );
        let Answer::Unconfirmed { reason } = &certs[0].answer else {
            panic!()
        };
        assert_eq!(reason, "the output has no routine for it");
        assert!(certs[1].target.is_some(), "{:?}", certs[1]);
        assert!(has_double_quoted_name(src, Dialect::Tsql));
        assert!(!has_double_quoted_name("SELECT [a], 'b'", Dialect::Tsql));
    }

    #[test]
    fn a_table_named_in_a_string_is_not_plain() {
        let ddl = "CREATE TABLE dbo.Acct (Id int NOT NULL, Qty int NULL, CONSTRAINT PK_Acct PRIMARY KEY (Id));\n";
        let bind = format!("{ddl}GO\nEXEC sp_bindefault 'df_qty', 'dbo.Acct.Qty';\n");
        let out = crate::rewrite(ddl, Dialect::Tsql, Dialect::Postgres)
            .output()
            .to_string();
        let read = |src: &str| Tables::read(&[src], &[&out], Dialect::Tsql, Dialect::Postgres);
        assert!(read(ddl).get("acct").is_some());
        assert!(read(&bind).get("acct").is_none());
        // DDL kept in a variable and run later, and dynamic SQL anywhere.
        let hidden = format!(
            "{ddl}GO\nCREATE PROCEDURE dbo.unkey AS\nBEGIN\n  DECLARE @s nvarchar(200) = N'ALTER TABLE dbo.Acct DROP CONSTRAINT PK_Acct';\n  EXEC (@s);\nEND\n"
        );
        assert!(read(&hidden).get("acct").is_none());
        let built = format!(
            "{ddl}GO\nCREATE PROCEDURE dbo.any @t sysname AS\nBEGIN\n  DECLARE @s nvarchar(200) = N'DROP TABLE ' + @t;\n  EXEC sp_executesql @s;\nEND\n"
        );
        assert!(read(&built).get("acct").is_none());
        // Capturing a return code is not dynamic SQL; running a procedure
        // named by a variable is. (C3d part 2, review) The procedure called
        // is one the tree declares; an undeclared one may run anything.
        let rc = format!(
            "{ddl}GO\nCREATE PROCEDURE dbo.r AS\nBEGIN\n  RETURN 1;\nEND\nGO\n\
             CREATE PROCEDURE dbo.q AS\nBEGIN\n  DECLARE @rc int;\n  EXEC @rc = dbo.r;\nEND\n"
        );
        assert!(read(&rc).get("acct").is_some());
        assert!(read(&rc.replace("EXEC @rc = dbo.r", "EXEC @rc = dbo.s"))
            .get("acct")
            .is_none());
        let by_name = format!(
            "{ddl}GO\nCREATE PROCEDURE dbo.q @p sysname AS\nBEGIN\n  DECLARE @rc int;\n  EXEC @rc = @p;\nEND\n"
        );
        assert!(read(&by_name).get("acct").is_none());
    }

    /// Every message family the routine reader (`sqldiff_syntax::routine`
    /// and the statement reader it uses), the pairing and the checker
    /// produce has its code; nothing falls through unnamed.
    #[test]
    fn every_refusal_has_a_code() {
        let shape = |e: &str| format!("the source routine is outside the verified shape: {e}");
        let out = |e: &str| format!("the output routine is outside the verified shape: {e}");
        for (reason, code) in [
            // The reader.
            (shape("dynamic SQL is not read"), "read:dynamic-sql"),
            (out("dynamic SQL is not read"), "read:dynamic-sql"),
            (
                shape("FOR JSON options is not kept in the syntax tree"),
                "read:for-json",
            ),
            (shape("unsupported FOR JSON option at `ROOT`"), "read:for-json"),
            (
                shape("unsupported routine option at `RECOMPILE`"),
                "read:routine-option",
            ),
            (
                shape("unsupported parameter option at `READONLY`"),
                "read:parameter-option",
            ),
            (
                shape("expected PROCEDURE or FUNCTION at `TRIGGER`"),
                "read:kind:trigger",
            ),
            (shape("expected PROCEDURE or FUNCTION at `VIEW`"), "read:kind:view"),
            (
                shape("expected PROCEDURE or FUNCTION at end of statement"),
                "read:kind:other",
            ),
            (shape("CREATE_VIEW is not a routine"), "read:kind:create-view"),
            (
                shape("table-valued results are not read yet"),
                "read:table-valued",
            ),
            (shape("unsupported statement `WAITFOR`"), "read:stmt:waitfor"),
            (shape("unsupported statement `RAISERROR`"), "read:stmt:raiserror"),
            (shape("CURSOR_STMT is not read yet"), "read:stmt:cursor-stmt"),
            (shape("unsupported declaration"), "read:declaration"),
            (shape("unsupported declaration at `CURSOR`"), "read:declaration"),
            // The reader's other refusals: shapes it does not read.
            (shape("exception handlers are not read yet"), "read:other"),
            (shape("Db2 for i routines are not read yet"), "read:other"),
            (shape("a block written `BEGIN X`"), "read:other"),
            (shape("only WHILE loops are read"), "read:other"),
            (shape("THROW takes no arguments or three"), "read:other"),
            (shape("unsupported MERGE clause"), "read:other"),
            (shape("the body contains its own quote"), "read:other"),
            (shape("a PL/pgSQL body that is not one block"), "read:other"),
            (shape("unexpected tokens in EXEC at `WITH`"), "read:other"),
            // Pairing.
            ("the output has no routine for it".to_string(), "pair:no-output"),
            (
                "no renaming pairs the two routines: 2 parameters in the source, 1 in the output"
                    .to_string(),
                "pair:renaming:params",
            ),
            (
                "no renaming pairs the two routines: 1 local variables in the source, 0 declared in the output".to_string(),
                "pair:renaming:locals",
            ),
            (
                "no renaming pairs the two routines: two source names map to `v_x`".to_string(),
                "pair:renaming:names",
            ),
            // The checker.
            ("no checker".to_string(), "check:none"),
            (
                "checker failed: no answer within 30s".to_string(),
                "check:transport",
            ),
            (
                "the checker does not speak sqldiff-check/3".to_string(),
                "check:transport",
            ),
            ("a procedure and a function".to_string(), "check:kind"),
            (
                "the routines run with different privileges".to_string(),
                "check:privileges",
            ),
            (
                "the parameter or return types do not correspond".to_string(),
                "check:signature",
            ),
            (
                "certificate: a T-SQL variable has no PL/pgSQL name".to_string(),
                "check:certificate",
            ),
            (
                "the lowered routines differ beyond the renaming".to_string(),
                "check:differ",
            ),
            (
                "the request's abstract syntax schema is not this checker's: generate formal/lean/Sqldiff/Ast.lean again and rebuild".to_string(),
                "check:schema",
            ),
            ("routine A does not decode".to_string(), "check:decode"),
            ("the catalog does not decode".to_string(), "check:decode"),
            (
                "the T-SQL routine is outside the C1 spec".to_string(),
                "check:spec",
            ),
            (
                "the PL/pgSQL routine is outside the C1 spec".to_string(),
                "check:spec",
            ),
            ("malformed request".to_string(), "check:other"),
            ("".to_string(), "check:other"),
        ] {
            assert_eq!(reason_code(&reason), code, "{reason:?}");
        }
    }

    /// A certificate's `why` is the code of its reason, or the checker's
    /// own codes when it gave them.
    #[test]
    fn a_refused_certificate_carries_its_codes() {
        let src = "CREATE PROCEDURE dbo.p @s nvarchar(100) AS BEGIN EXEC (@s); END\n";
        let d = crate::rewrite(src, Dialect::Tsql, Dialect::Postgres);
        let certs = certify(
            src,
            d.output(),
            Dialect::Tsql,
            Dialect::Postgres,
            &Trusted::default(),
            None,
        );
        assert_eq!(certs[0].why, ["read:dynamic-sql"], "{:?}", certs[0]);
        let json = serde_json::to_value(&certs[0]).unwrap();
        assert_eq!(json["why"][0], "read:dynamic-sql");
        assert_eq!(json["answer"], "unconfirmed");

        #[derive(Debug)]
        struct Codes;
        impl Checker for Codes {
            fn name(&self) -> String {
                "codes".into()
            }
            fn check(&self, _: &proof::Request) -> proof::Answer {
                unreachable!()
            }
            fn check_mirror(&self, _: &MirrorRequest) -> proof::MirrorAnswer {
                let mut m = proof::MirrorAnswer::from(proof::Answer::Unconfirmed {
                    reason: "the lowered routines differ beyond the renaming".into(),
                });
                m.why = vec!["check:differ:assign".into(), "check:differ:if".into()];
                m
            }
        }
        let d = crate::rewrite(TSQL, Dialect::Tsql, Dialect::Postgres);
        let certs = certify(
            TSQL,
            d.output(),
            Dialect::Tsql,
            Dialect::Postgres,
            &Trusted::default(),
            Some(&Codes),
        );
        assert_eq!(certs[0].why, ["check:differ:assign", "check:differ:if"]);
        let json = serde_json::to_value(&certs[0]).unwrap();
        assert_eq!(json["why"].as_array().unwrap().len(), 2);
    }

    /// A confirmation with data statements or queries rests on the
    /// caller's `SET ROWCOUNT 0` whichever path it took: the checker's
    /// residual names it from 0.15.0, the C1 caveats do not cover it, and
    /// an older checker's residual lacks it.
    #[test]
    fn data_statements_and_queries_rest_on_rowcount_0() {
        let own = vec!["ASM-TABLES-AS-DECLARED".to_string()];
        let named = vec![STRING_ARGS.to_string(), ROWCOUNT_0.to_string()];
        assert_eq!(
            confirmed_rests_on(Some("c2a"), Some("c3a"), &named, own.clone()),
            [STRING_ARGS, ROWCOUNT_0, "ASM-TABLES-AS-DECLARED"]
        );
        // A checker before 0.15.0.
        let old = vec![STRING_ARGS.to_string()];
        assert_eq!(
            confirmed_rests_on(Some("c2a"), Some("c3b"), &old, own.clone()),
            [STRING_ARGS, ROWCOUNT_0, "ASM-TABLES-AS-DECLARED"]
        );
        assert_eq!(
            confirmed_rests_on(Some("uninterpreted"), Some("c3a"), &[], own),
            [
                C1_OPERATORS,
                C1_NO_ERRORS,
                ROWCOUNT_0,
                "ASM-TABLES-AS-DECLARED"
            ]
        );
        // No table, no ROWCOUNT.
        assert_eq!(
            confirmed_rests_on(Some("c2a"), Some("c1"), &old, Vec::new()),
            [STRING_ARGS]
        );
        assert!(C2A_RESIDUALS.contains(&ROWCOUNT_0));
    }

    /// A checker that answers every mirror request with `answer` and
    /// counts the requests.
    #[derive(Debug)]
    struct Always {
        answer: fn() -> proof::MirrorAnswer,
        asked: std::sync::atomic::AtomicUsize,
    }

    impl Always {
        fn new(answer: fn() -> proof::MirrorAnswer) -> Always {
            Always {
                answer,
                asked: Default::default(),
            }
        }
        fn asked(&self) -> usize {
            self.asked.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl Checker for Always {
        fn name(&self) -> String {
            "always".into()
        }
        fn check(&self, _: &proof::Request) -> proof::Answer {
            unreachable!()
        }
        fn check_mirror(&self, _: &MirrorRequest) -> proof::MirrorAnswer {
            self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (self.answer)()
        }
    }

    /// Four unread statements between readable ones; the `DECLARE` of
    /// a table variable is one of them, so the locals no longer pair.
    const UNREAD: &str = "CREATE PROCEDURE dbo.p @s nvarchar(100), @a int
AS
BEGIN
  DECLARE @x int = 1;
  PRINT 'start';
  SET @x = @a;
  CREATE TABLE #t (a int);
  IF @a > 1
  BEGIN
    EXEC (@s);
    SET @x = 2;
  END
  SELECT @x AS x FOR JSON PATH;
  PRINT 'again';
  RETURN @x;
END
";

    fn certify_unread(k: &dyn Checker) -> Certificate {
        let d = crate::rewrite(UNREAD, Dialect::Tsql, Dialect::Postgres);
        let mut certs = certify(
            UNREAD,
            d.output(),
            Dialect::Tsql,
            Dialect::Postgres,
            &Trusted::default(),
            Some(k),
        );
        assert_eq!(certs.len(), 1, "{certs:?}");
        certs.remove(0)
    }

    const UNREAD_REASON: &str =
        "the source routine is outside the verified shape: unsupported statement `PRINT`";

    /// The invariant: a checker that confirms everything is asked about
    /// the trees without the unread statements, and the routine still
    /// comes out unconfirmed, with today's reason and the codes of every
    /// left-out statement.
    #[test]
    fn a_routine_with_a_left_out_statement_is_never_confirmed() {
        fn confirmed() -> proof::MirrorAnswer {
            let mut m = proof::MirrorAnswer::from(proof::Answer::Confirmed {
                theorem: "Sqldiff.Mirror.check_sound_c2a".into(),
            });
            m.scalars = Some("c2a".into());
            m.statements = Some("c1".into());
            // Codes on a confirmation are ignored, as is the confirmation.
            m.why = vec!["stmt:select:order".into()];
            m
        }
        let k = Always::new(confirmed);
        let c = certify_unread(&k);
        assert_eq!(k.asked(), 1, "the checker is asked about the rest");
        assert_eq!(
            c.answer,
            Answer::Unconfirmed {
                reason: UNREAD_REASON.into()
            }
        );
        assert_eq!(
            c.why,
            [
                "read:stmt:print",
                "read:stmt:create",
                "read:dynamic-sql",
                "read:for-json"
            ],
            "{c:?}"
        );
        assert!(c.assumptions.is_empty() && c.scalars.is_none() && c.statements.is_none());
        assert!(c.source.is_none() && c.target.is_none(), "{c:?}");
        // Without a checker: the same answer and the same codes.
        let d = crate::rewrite(UNREAD, Dialect::Tsql, Dialect::Postgres);
        let none = certify(
            UNREAD,
            d.output(),
            Dialect::Tsql,
            Dialect::Postgres,
            &Trusted::default(),
            None,
        );
        assert_eq!(none[0].answer, c.answer);
        assert_eq!(none[0].why, c.why);
    }

    /// The invariant holds in the one function that applies it, whatever
    /// the certificate it is handed.
    #[test]
    fn never_confirmed_refuses_even_a_confirmed_certificate() {
        let d = crate::rewrite(TSQL, Dialect::Tsql, Dialect::Postgres);
        let mut c = certify(
            TSQL,
            d.output(),
            Dialect::Tsql,
            Dialect::Postgres,
            &Trusted::default(),
            None,
        )
        .remove(0);
        c.answer = Answer::Confirmed {
            theorems: vec!["Sqldiff.Mirror.check_sound".into()],
        };
        c.assumptions = vec![C1_OPERATORS.into()];
        c.scalars = Some("c2a".into());
        let ok = proof::MirrorAnswer::from(proof::Answer::Confirmed {
            theorem: "Sqldiff.Mirror.check_sound".into(),
        });
        never_confirmed(&mut c, vec!["read:stmt:print".into()], Some(ok));
        assert!(matches!(c.answer, Answer::Unconfirmed { .. }), "{c:?}");
        assert_eq!(c.why, ["read:stmt:print"]);
        assert!(c.assumptions.is_empty() && c.scalars.is_none());
    }

    /// From the checker's refusal of the trees without the unread
    /// statements, only the lowering's codes are kept, each once, after
    /// the reader's.
    #[test]
    fn only_the_lowerings_codes_are_kept_past_an_unread_statement() {
        fn outside() -> proof::MirrorAnswer {
            let mut m = proof::MirrorAnswer::from(proof::Answer::Unconfirmed {
                reason: "the T-SQL routine is outside the C1 spec".into(),
            });
            m.why = [
                "stmt:select:from:join",
                "check:differ",
                "check:certificate",
                "pair:renaming:locals",
                "check:signature",
                "check:kind",
                "check:privileges",
                "routine:scope:undeclared",
                "node:call:type",
                "expr:fn:sysdatetime",
                "store:assign:int",
                "type:datetime",
                "stmt:select:from:join",
                "read:stmt:print",
            ]
            .map(String::from)
            .to_vec();
            m
        }
        let k = Always::new(outside);
        let c = certify_unread(&k);
        assert_eq!(k.asked(), 1);
        assert_eq!(
            c.answer,
            Answer::Unconfirmed {
                reason: UNREAD_REASON.into()
            }
        );
        assert_eq!(
            c.why,
            [
                "read:stmt:print",
                "read:stmt:create",
                "read:dynamic-sql",
                "read:for-json",
                "stmt:select:from:join",
                "routine:scope:undeclared",
                "node:call:type",
                "expr:fn:sysdatetime",
                "store:assign:int",
                "type:datetime",
            ]
        );
        // A refusal that is not about the lowering adds nothing.
        fn differ() -> proof::MirrorAnswer {
            let mut m = proof::MirrorAnswer::from(proof::Answer::Unconfirmed {
                reason: "the lowered routines differ beyond the renaming".into(),
            });
            m.why = vec!["check:differ:assign".into()];
            m
        }
        let c = certify_unread(&Always::new(differ));
        assert_eq!(c.why.len(), 4, "{c:?}");
    }

    /// A routine refused as a whole (here a routine option) is answered
    /// as before: one code, and the checker is not asked.
    #[test]
    fn a_routine_refused_as_a_whole_is_not_diagnosed_further() {
        let src = "CREATE PROCEDURE dbo.p WITH RECOMPILE AS BEGIN PRINT 'x'; SET NOCOUNT ON; END\n";
        let d = crate::rewrite(src, Dialect::Tsql, Dialect::Postgres);
        fn confirmed() -> proof::MirrorAnswer {
            proof::MirrorAnswer::from(proof::Answer::Confirmed {
                theorem: "Sqldiff.Mirror.check_sound".into(),
            })
        }
        let k = Always::new(confirmed);
        let certs = certify(
            src,
            d.output(),
            Dialect::Tsql,
            Dialect::Postgres,
            &Trusted::default(),
            Some(&k),
        );
        assert_eq!(k.asked(), 0);
        assert!(matches!(certs[0].answer, Answer::Unconfirmed { .. }));
        assert_eq!(certs[0].why, ["read:routine-option"]);
    }
}
