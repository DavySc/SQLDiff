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
use sqldiff_syntax::abstract_syntax::{tokens, Stmt, K};
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
    /// The checker's statement slice for a confirmation: `c3a` when the
    /// routine's data statements were in it, `c1` when it had none.
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
    "ASM-TSQL-STRING-ARGS",
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
}

/// A T-SQL procedure that returns NULL returns 0 with a warning message;
/// the converted procedure returns 0 without it.
pub const RETURN_NULL_WARNING: &str = "ASM-TSQL-RETURN-NULL-WARNING";

fn returns_value(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match n {
        Node::Stmt(Stmt::Return(Some(_))) => true,
        Node::Block(b) => returns_value(&b.body),
        Node::If { then, els, .. } => returns_value(then) || returns_value(els),
        Node::While { body, .. } => returns_value(body),
        Node::Try { body, handlers } => {
            returns_value(body) || handlers.iter().any(|h| returns_value(&h.body))
        }
        _ => false,
    })
}

/// A PostgreSQL name as PostgreSQL resolves it: a quoted name whose case
/// matters arrives with a leading `"` and is kept; any other is folded to
/// lower case (`Sqldiff.Mirror.pgName`).
pub fn pg_name(n: &str) -> String {
    if n.starts_with('"') {
        n.to_string()
    } else {
        n.to_lowercase()
    }
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
            Node::While { body, .. } => tsql_locals(body, out),
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

fn callees(nodes: &[Node], out: &mut Vec<Vec<String>>) {
    for n in nodes {
        match n {
            Node::Call { name, .. } => out.push(name.clone()),
            Node::Block(b) => callees(&b.body, out),
            Node::If { then, els, .. } => {
                callees(then, out);
                callees(els, out);
            }
            Node::While { body, .. } => callees(body, out),
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
            Node::While { body, .. } => written_tables(body, out),
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
    by_target: Vec<MirrorTable>,
}

/// Words of a node as names compare: lower case, quotes removed.
fn words(n: &SyntaxNode, d: Dialect, out: &mut Vec<String>) {
    for t in tokens(n) {
        let w = match t.kind {
            K::Word => t.text.to_lowercase(),
            K::Quoted => sqldiff_syntax::abstract_syntax::quoted_name(&t.text, d)
                .trim_start_matches('"')
                .to_lowercase(),
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
                by_target.push(MirrorTable { name: key, cols });
            }
        }
        Tables { by_target }
    }

    pub fn get(&self, target: &str) -> Option<&MirrorTable> {
        self.by_target.iter().find(|t| t.name == target)
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
        let (to, dbo) = target_routine(&c);
        if dbo && !assumptions.iter().any(|x| x == "ASM-DBO-SCHEMA") {
            assumptions.push("ASM-DBO-SCHEMA".to_string());
        }
        let pair = (c.join("."), to);
        if !routines.contains(&pair) {
            routines.push(pair);
        }
    }
    let mut written = Vec::new();
    written_tables(&a.body.body, &mut written);
    let (mut table_pairs, mut table_catalog) = (Vec::new(), Vec::<MirrorTable>::new());
    for w in &written {
        let (to, dbo) = target_routine(w);
        let Some(t) = tables.get(&to) else { continue };
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
    if !written.is_empty() && !row_counts {
        assumptions.push("ASM-OBSERVER-ROWCOUNT".to_string());
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
    let readable: Vec<Routine> = out.iter().filter_map(|(_, _, r)| r.clone().ok()).collect();
    let mut certs = Vec::new();
    for (i, (name, span, a)) in src.into_iter().enumerate() {
        let mut c = Certificate {
            routine: name,
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
        let b = out.get(i).map(|(_, _, b)| b.clone());
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
                    Ok((req, assumptions)) => {
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
}
