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
use sqldiff_core::proof::{self, Checker, MirrorColumn, MirrorRequest, MirrorTable};
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
pub const C2A_RESIDUALS: [&str; 10] = [
    "ASM-TSQL-ARITH-ERRORS",
    "ASM-TSQL-ANSI-NULLS",
    "ASM-STATE-AFTER-ERROR",
    "ASM-CONCAT-NULL",
    BINARY_COLLATION,
    "ASM-TSQL-LEN-BMP",
    STRING_ARGS,
    // C4: only errors the core models reach a handler, no caller
    // transaction around a handled error or transaction control, and
    // callers ignore the number of an error the routine raises.
    "ASM-CATCH-CORE-ERRORS",
    "ASM-NO-CALLER-TXN",
    "ASM-THROW-ERRCODE",
];

/// Residual ids the checker names whenever a routine needs them, so a
/// `c2a` confirmation without one proves the converter's note of it.
/// Not every residual: `ASM-NO-CALLER-TXN` is named only around handlers
/// and transaction control, while an error that ends a plain routine also
/// meets the caller's transaction.
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

type Read = (String, (usize, usize), Result<Routine, String>);

fn routines(text: &str, d: Dialect) -> Vec<Read> {
    parse(text, d)
        .syntax()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ROUTINE)
        .map(|n| {
            let name = sqldiff_syntax::ast::routine_name(&n).unwrap_or_default();
            let span = sqldiff_syntax::ast::sig_range(&n);
            (name, span, routine::read(&n, d))
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

/// The tables in a query's `FROM`, as written (C3b: the checker reads only
/// a one-table `FROM`; anything else is refused there).
fn from_tables(q: &Select, out: &mut Vec<Vec<String>>) {
    for f in &q.from {
        if let From::Table { name, .. } = f {
            if !out.contains(name) {
                out.push(name.clone());
            }
        }
    }
}

/// The tables a routine's queries read, as written, in the shapes the
/// checker's C3b slice reads: `SELECT @x = ...`, `SET @x = (SELECT ...)`,
/// `IF [NOT] EXISTS (SELECT ...)` (formal/lean/Sqldiff/Mirror/Lower.lean,
/// "The C3b slice: queries").
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

/// Plain table definitions of a tree, and every word named elsewhere.
fn definitions(texts: &[&str], d: Dialect) -> (Vec<plain_table::PlainTable>, Vec<String>) {
    let (mut defs, mut named) = (Vec::new(), Vec::new());
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
                        // The body names its tables; only DDL in it counts.
                        for s in n
                            .descendants()
                            .filter(|s| s.kind() == SyntaxKind::STATEMENT)
                        {
                            if is_ddl(&s) {
                                words(&s, d, &mut named);
                            }
                        }
                    } else {
                        words(&n, d, &mut named);
                    }
                    it.skip_subtree();
                }
                SyntaxKind::STATEMENT => {
                    match plain_table::read(&n, d) {
                        Ok(t) => defs.push(t),
                        Err(_) => words(&n, d, &mut named),
                    }
                    it.skip_subtree();
                }
                _ => {}
            }
        }
    }
    (defs, named)
}

impl Tables {
    /// The plain tables of a T-SQL tree converted to PostgreSQL; none for
    /// any other pair of dialects.
    pub fn read(inputs: &[&str], outputs: &[&str], from: Dialect, to: Dialect) -> Tables {
        if (from, to) != (Dialect::Tsql, Dialect::Postgres) {
            return Tables::default();
        }
        let (src, src_named) = definitions(inputs, from);
        let (out, out_named) = definitions(outputs, to);
        let pg = |parts: &[String]| {
            parts
                .iter()
                .map(|p| pg_name(p))
                .collect::<Vec<_>>()
                .join(".")
        };
        let mut by_target = Vec::new();
        for a in &src {
            let key = target_routine(&a.name).0;
            let last = key
                .rsplit('.')
                .next()
                .unwrap_or_default()
                .trim_start_matches('"')
                .to_lowercase();
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
            let cols: Option<Vec<MirrorColumn>> = a
                .cols
                .iter()
                .zip(&b.cols)
                .map(|(x, y)| {
                    (x.name.is_ascii()
                        && pg_name(&x.name) == pg_name(&y.name)
                        && x.not_null == y.not_null
                        && x.key == y.key)
                        .then(|| MirrorColumn {
                            name: pg_name(&y.name),
                            tsql: x.name.clone(),
                            not_null: y.not_null,
                            key: y.key,
                        })
                })
                .collect();
            if let Some(cols) = cols {
                by_target.push((MirrorTable { name: key, cols }, a.name.clone()));
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
    let (tables, row_counts) = (&trusted.tables, trusted.row_counts);
    let renaming = propose_renaming(a, b)?;
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
        if trusted.binary_collation && tables.declared(&to) != Some(w.as_slice()) {
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
    let stringy = |t: &str| {
        let t = t.to_ascii_lowercase();
        t.contains("char") || t == "sysname" || t.contains("text")
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
    Ok((
        MirrorRequest {
            row_counts,
            routines,
            catalog,
            tables: table_pairs,
            table_catalog,
            binary_collation: trusted.binary_collation,
            renaming,
            source: encode::routine(a),
            target: encode::routine(b),
        },
        assumptions,
    ))
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
    let mut readable: Vec<Routine> = out.iter().filter_map(|(_, _, r)| r.clone().ok()).collect();
    for r in &trusted.output_routines {
        if !readable.iter().any(|x| x.name == r.name) {
            readable.push(r.clone());
        }
    }
    let mut certs = Vec::new();
    for (name, span, a) in src {
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
        };
        // The output routine of the same name, if exactly one.
        let key = pair_key(&name, from);
        let same: Vec<_> = out
            .iter()
            .filter(|(n, _, _)| pair_key(n, to) == key)
            .collect();
        let b = match same.as_slice() {
            [(_, _, b)] => Some(b.clone()),
            _ => None,
        };
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
                                        Answer::Unconfirmed { reason }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        };
        certs.push(c);
    }
    certs
}

/// The routines of a converted tree that read back, for the target catalog.
pub fn output_routines(texts: &[&str], d: Dialect) -> Vec<Routine> {
    texts
        .iter()
        .flat_map(|t| routines(t, d))
        .filter_map(|(_, _, r)| r.ok())
        .collect()
}

/// The routines a tree declares, names as written (quotes removed).
pub fn declared_routines(texts: &[&str], d: Dialect) -> Vec<Vec<String>> {
    texts
        .iter()
        .flat_map(|t| routines(t, d))
        .map(|(name, _, _)| name.split('.').map(str::to_string).collect())
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
/// to mean the same, runs without errors); then the routine's own.
fn confirmed_rests_on(scalars: Option<&str>, residual: &[String], own: Vec<String>) -> Vec<String> {
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
        let readable: Vec<Routine> = b.iter().filter_map(|(_, _, r)| r.clone().ok()).collect();
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
        // Anything else that names the table keeps it out.
        for extra in [
            "CREATE INDEX ix ON dbo.Acct (Qty);\n",
            "GO\nCREATE TRIGGER tr ON dbo.Acct AFTER INSERT AS BEGIN SET NOCOUNT ON; END\n",
            "ALTER TABLE dbo.Acct ADD CONSTRAINT ck CHECK (Qty > 0);\n",
            "CREATE TABLE dbo.Line (Id int NOT NULL REFERENCES dbo.Acct (Id));\n",
            "CREATE TABLE dbo.Acct (Id int NOT NULL PRIMARY KEY);\n",
            "GO\nCREATE PROCEDURE dbo.q AS BEGIN ALTER TABLE dbo.Acct ADD z int NULL; END\n",
        ] {
            assert!(tables(extra).get("acct").is_none(), "{extra}");
        }
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
    }
}
