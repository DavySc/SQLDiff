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
use sqldiff_core::proof::{self, Checker, MirrorRequest};
use sqldiff_syntax::abstract_syntax::Stmt;
use sqldiff_syntax::routine::{self, Mode, Node, Routine, RoutineKind};
use sqldiff_syntax::{encode, parse, Dialect, SyntaxKind};

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
/// OFF), and compares with NULL as UNKNOWN (ANSI_NULLS ON).
pub const C2A_RESIDUALS: [&str; 2] = ["ASM-TSQL-ARITH-ERRORS", "ASM-TSQL-ANSI-NULLS"];

/// A T-SQL procedure that returns NULL returns 0 with a warning message;
/// the converted procedure returns 0 without it.
pub const RETURN_NULL_WARNING: &str = "ASM-TSQL-RETURN-NULL-WARNING";

fn returns_value(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match n {
        Node::Stmt(Stmt::Return(Some(_))) => true,
        Node::Block(b) => returns_value(&b.body),
        Node::If { then, els, .. } => returns_value(then) || returns_value(els),
        Node::While { body, .. } => returns_value(body),
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
            _ => {}
        }
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
    row_counts: bool,
) -> Result<(MirrorRequest, Vec<String>), String> {
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
    row_counts: bool,
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
                match mirror_request(&a, &b, &readable, row_counts) {
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
                                        c.scalars = m.scalars.clone();
                                        c.assumptions = confirmed_rests_on(
                                            m.scalars.as_deref(),
                                            &m.residual,
                                            assumptions,
                                        );
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
        let certs = certify(TSQL, &out, Dialect::Tsql, Dialect::Postgres, false, None);
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
        let src = "CREATE PROCEDURE dbo.p AS BEGIN BEGIN TRAN; COMMIT; END\n";
        let d = crate::rewrite(src, Dialect::Tsql, Dialect::Postgres);
        let certs = certify(
            src,
            d.output(),
            Dialect::Tsql,
            Dialect::Postgres,
            false,
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
            false,
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
}
