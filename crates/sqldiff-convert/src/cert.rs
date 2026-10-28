//! Certificates for converted routines (docs/verified-conversion.md, "The
//! certificate", C0 item 5).
//!
//! A certificate is about the text on disk: the output file is parsed with
//! the target dialect's parser and read into the abstract syntax, so what
//! the checker is shown is what will run, whatever the rules or the printer
//! did. The source is read the same way. Everything Rust adds (the renaming
//! that pairs the two sides' names) is an untrusted hint the checker
//! verifies.

use serde::Serialize;
use sha2::{Digest, Sha256};
use sqldiff_syntax::abstract_syntax::Stmt;
use sqldiff_syntax::routine::{self, Mode, Node, Routine};
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
    /// variables (`@x` to `v_x`), and the T-SQL return code to the
    /// parameter that carries it.
    pub renaming: Renaming,
    #[serde(flatten)]
    pub answer: Answer,
}

fn sha(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Name of the T-SQL return code in a renaming (`RETURN @x` in a procedure).
pub const RETURN_CODE: &str = "@@RETURN";

fn routines(text: &str, d: Dialect) -> Vec<(String, Result<Routine, String>)> {
    parse(text, d)
        .syntax()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ROUTINE)
        .map(|n| {
            let name = sqldiff_syntax::ast::routine_name(&n).unwrap_or_default();
            (name, routine::read(&n, d))
        })
        .collect()
}

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

fn returns_value(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match n {
        Node::Stmt(Stmt::Return(Some(_))) => true,
        Node::Block(b) => returns_value(&b.body),
        Node::If { then, els, .. } => returns_value(then) || returns_value(els),
        Node::While { body, .. } => returns_value(body),
        _ => false,
    })
}

/// Proposes which target name stands for each source name: parameters by
/// position, T-SQL locals in declaration order against the PL/pgSQL
/// declaration section, and the return code against the extra trailing
/// `INOUT` parameter. The checker verifies the result; a wrong proposal can
/// only make it refuse.
pub fn propose_renaming(a: &Routine, b: &Routine) -> Result<Renaming, String> {
    let mut out = Vec::new();
    let code = a.kind == routine::RoutineKind::Procedure && returns_value(&a.body.body);
    let extra = usize::from(code);
    if b.params.len() != a.params.len() + extra {
        return Err(format!(
            "{} parameters in the source, {} in the output",
            a.params.len(),
            b.params.len()
        ));
    }
    for (x, y) in a.params.iter().zip(&b.params) {
        out.push((x.name.clone(), y.name.clone()));
    }
    if code {
        let last = b.params.last().expect("checked above");
        if last.mode != Mode::InOut {
            return Err("the return code's parameter is not INOUT".into());
        }
        out.push((RETURN_CODE.to_string(), last.name.clone()));
    }
    let mut locals = Vec::new();
    tsql_locals(&a.body.body, &mut locals);
    let decls: Vec<String> = b
        .body
        .decls
        .iter()
        .filter_map(|s| match s {
            Stmt::Declare { name, .. } => Some(name.clone()),
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
        if !seen.insert(y.to_lowercase()) {
            return Err(format!("two source names map to `{y}`"));
        }
    }
    Ok(out)
}

/// A source to target name pairing.
pub type Renaming = Vec<(String, String)>;

/// Answers a routine pair with its renaming.
pub type Check<'a> = &'a dyn Fn(&Routine, &Routine, &[(String, String)]) -> Answer;

/// Certificates for the routines of one converted file. `check` answers a
/// pair with its renaming; without a checker for routines every answer is
/// unconfirmed, with the reason.
pub fn certify(
    input: &str,
    output: &str,
    from: Dialect,
    to: Dialect,
    check: Check,
) -> Vec<Certificate> {
    let src = routines(input, from);
    let out = routines(output, to);
    let mut certs = Vec::new();
    for (i, (name, a)) in src.into_iter().enumerate() {
        let mut c = Certificate {
            routine: name,
            from: from.id().to_string(),
            to: to.id().to_string(),
            source: None,
            target: None,
            renaming: Vec::new(),
            answer: Answer::Unconfirmed {
                reason: String::new(),
            },
        };
        let b = out.get(i).map(|(_, b)| b.clone());
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
                match propose_renaming(&a, &b) {
                    Err(e) => Answer::Unconfirmed {
                        reason: format!("no renaming pairs the two routines: {e}"),
                    },
                    Ok(r) => {
                        let ans = check(&a, &b, &r);
                        c.renaming = r;
                        ans
                    }
                }
            }
        };
        certs.push(c);
    }
    certs
}

/// The answer while the checker has no routine semantics (phase C1).
pub fn no_checker(_: &Routine, _: &Routine, _: &[(String, String)]) -> Answer {
    Answer::Unconfirmed {
        reason: "the checker does not check routines yet (phase C1)".into(),
    }
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
        let certs = certify(TSQL, &out, Dialect::Tsql, Dialect::Postgres, &no_checker);
        assert_eq!(certs.len(), 1, "{out}");
        let c = &certs[0];
        assert!(c.source.is_some() && c.target.is_some(), "{c:?}\n{out}");
        assert_eq!(
            c.renaming,
            [
                ("@a", "p_a"),
                ("@b", "p_b"),
                (RETURN_CODE, "return_code"),
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
            &no_checker,
        );
        let Answer::Unconfirmed { reason } = &certs[0].answer else {
            panic!();
        };
        assert!(reason.contains("outside the verified shape"), "{reason}");
    }
}
