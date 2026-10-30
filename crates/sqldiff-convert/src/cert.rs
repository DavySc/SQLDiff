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
    /// The T-SQL routines the source tree declares, names as written: under
    /// a binary collation names are case-sensitive, so a call reaches a
    /// declared routine only when spelled exactly as declared.
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

/// Dynamic SQL: a string run as a statement, whose text no scan can see
/// when it is built at run time. T-SQL `EXEC (...)`, `EXEC @s`,
/// `sp_executesql`; PL/pgSQL `EXECUTE` (in PostgreSQL always dynamic).
fn dynamic_sql(n: &SyntaxNode, d: Dialect) -> bool {
    let t = tokens(n);
    let runs_text = |j: usize| {
        t.get(j)
            .is_some_and(|y| matches!(y.kind, K::LParen | K::Var | K::Str))
    };
    t.iter().enumerate().any(|(i, x)| {
        let exec = x.up == "EXEC" || x.up == "EXECUTE";
        // `EXEC @rc = dbo.p` captures a return code: what runs is the
        // named procedure, unless it too is a variable (`EXEC @rc = @p`).
        let rc = t.get(i + 1).is_some_and(|y| y.kind == K::Var)
            && t.get(i + 2)
                .is_some_and(|y| y.kind == K::Op && y.text == "=");
        (exec && d == Dialect::Postgres)
            || (exec
                && if rc {
                    runs_text(i + 3)
                } else {
                    runs_text(i + 1)
                })
            || x.up == "SP_EXECUTESQL"
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
    let (mut defs, mut named, mut dynamic) = (Vec::new(), Vec::new(), false);
    let (mut indexes, mut padding, mut collation) = (Vec::new(), false, false);
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
                    if matches!(kind.as_deref(), Some("PROCEDURE" | "PROC" | "FUNCTION")) {
                        // The body names its tables; only DDL in it counts, and
                        // any table named in a string (DDL kept as text).
                        for s in n
                            .descendants()
                            .filter(|s| s.kind() == SyntaxKind::STATEMENT)
                        {
                            if is_ddl(&s) {
                                words(&s, d, &mut named);
                            } else {
                                string_words(&s, &mut named);
                            }
                        }
                        dynamic |= dynamic_sql(&n, d);
                    } else {
                        words(&n, d, &mut named);
                    }
                    it.skip_subtree();
                }
                SyntaxKind::STATEMENT => {
                    dynamic |= dynamic_sql(&n, d);
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
        let mut by_target = Vec::new();
        for a in src {
            let key = target_routine(&a.name).0;
            let last = last_of(&key);
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
                let t = MirrorTable { name: key, cols };
                // A `varchar` column created under `ANSI_PADDING OFF` trims
                // trailing spaces on writes (eval/tables T3).
                if src_ddl.padding_off && has_varchar(&t) {
                    continue;
                }
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
        if !mappable(&c) || (trusted.binary_collation && !trusted.declared_routines.contains(&c)) {
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
const LOWERING_FAMILIES: [&str; 6] = ["routine:", "stmt:", "node:", "expr:", "store:", "type:"];

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
            &Trusted::default(),
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
        // Anything else that names the table keeps it out.
        for extra in [
            "CREATE UNIQUE INDEX ix ON dbo.Acct (Qty);\n",
            "CREATE INDEX ix ON dbo.Acct (Qty) WHERE Qty IS NOT NULL;\n",
            "GO\nCREATE TRIGGER tr ON dbo.Acct AFTER INSERT AS BEGIN SET NOCOUNT ON; END\n",
            "ALTER TABLE dbo.Acct ADD CONSTRAINT ck CHECK (Qty > 0);\n",
            "CREATE TABLE dbo.Line (Id int NOT NULL REFERENCES dbo.Acct (Id));\n",
            "CREATE TABLE dbo.Acct (Id int NOT NULL PRIMARY KEY);\n",
            "GO\nCREATE PROCEDURE dbo.q AS BEGIN ALTER TABLE dbo.Acct ADD z int NULL; END\n",
        ] {
            assert!(tables(extra).get("acct").is_none(), "{extra}");
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
            assert!(tables(extra).get("cust").is_some(), "{extra}");
        }
        // Anything else keeps it out.
        for extra in [
            "CREATE UNIQUE INDEX UX_Name ON dbo.Cust (Name);\n",
            "CREATE INDEX IX_Name ON dbo.Cust (Name) WHERE Name > 'a';\n",
            "CREATE INDEX IX_Name ON dbo.Cust (Name) WITH (FILLFACTOR = 80);\n",
            "CREATE INDEX IX_Nope ON dbo.Cust (Nope);\n",
            "SET ANSI_PADDING OFF;\n",
        ] {
            assert!(tables(extra).get("cust").is_none(), "{extra}");
        }
        // An index whose PostgreSQL entry may exceed the btree limit: 700
        // characters may take 2800 bytes (eval/tables T8), though 700 bytes
        // fit SQL Server's 1700.
        let wide = "CREATE TABLE dbo.W (Id int NOT NULL PRIMARY KEY, V varchar(700) NULL);\n\
                    CREATE INDEX IX_V ON dbo.W (V);\n";
        assert!(tables_of(wide, &convert(wide)).get("w").is_none());
        let wide_ok = wide.replace("700", "600");
        assert!(tables_of(&wide_ok, &convert(&wide_ok)).get("w").is_some());
        // Types and defaults must pair: an output edited to another length,
        // to text, without the default, with another default.
        let out = convert(ddl);
        assert!(tables_of(ddl, &out).get("cust").is_some());
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
            assert!(tables_of(&src, &out).get("cust").is_none(), "{extra}");
        }
        for extra in [
            "CREATE INDEX ix_name ON public.cust (name);\n",
            "CREATE INDEX ix_name ON \"public\".\"cust\" (name);\n",
            "CREATE INDEX ix_name ON mydb.public.cust (name);\n",
            "CREATE INDEX ix_name ON dbo.cust (name);\n",
        ] {
            let edited = format!("{out}{extra}");
            assert!(tables_of(ddl, &edited).get("cust").is_none(), "{extra}");
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
            assert!(tables_of(ddl, &edited).get("cust").is_none(), "{to}");
        }
        // A default only one side shows, the other way round.
        let src = ddl.replace(" DEFAULT 0", "");
        assert!(tables_of(&src, &out).get("cust").is_none());
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
        let (req, asm) = mirror_request(&a, &b, &[], &Trusted::default()).unwrap();
        // `q` resolves through sales first in T-SQL; Ä folds differently.
        assert_eq!(req.routines, [("dbo.r".to_string(), "r".to_string())]);
        assert!(
            asm.iter().any(|x| x == "ASM-CALLEE-CONFIRMED:dbo.r"),
            "{asm:?}"
        );
        // Under a binary collation only the declared spelling reaches it.
        let bin = Trusted {
            binary_collation: true,
            declared_routines: vec![vec!["dbo".into(), "R".into()]],
            ..Default::default()
        };
        let (req, _) = mirror_request(&a, &b, &[], &bin).unwrap();
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
        // named by a variable is.
        let rc = format!(
            "{ddl}GO\nCREATE PROCEDURE dbo.q AS\nBEGIN\n  DECLARE @rc int;\n  EXEC @rc = dbo.r;\nEND\n"
        );
        assert!(read(&rc).get("acct").is_some());
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
