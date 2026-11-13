//! `EXIT`/`BREAK`/`CONTINUE` out of a loop from inside an exception block
//! (PostgreSQL) or a TRY block (T-SQL) leaves the scopes opened since the
//! loop, as the end of those blocks does. Without the `Leave` nodes the
//! scope stayed on the stack and a later COMMIT was refused as "in an
//! exception block".

use super::*;
use crate::artifact::Artifact;
use crate::model::Side;

/// The scopes (by kind) left on the way from the first jump statement
/// (`EXIT`/`BREAK`/`CONTINUE`) of the routine to the first node that is not
/// a `Leave`, less what the loop's own exit leaves anyway.
fn left_by_the_jump(d: Dialect, text: &str) -> Vec<ScopeKind> {
    let art = Artifact::new("p.sql", d, text.to_string());
    let a = Analyzed::new(&art, Side::A, 0);
    let cfg = lower(&a);
    let jump = cfg
        .nodes
        .iter()
        .position(|n| {
            n.syntax.as_ref().is_some_and(|s| {
                s.kind() == SyntaxKind::STATEMENT
                    && ast::sig_tokens(s).next().is_some_and(|t| {
                        ["EXIT", "BREAK", "CONTINUE"]
                            .iter()
                            .any(|w| t.text().eq_ignore_ascii_case(w))
                    })
            })
        })
        .expect("a jump statement");
    let chain = |mut at: NodeId| {
        let mut kinds = Vec::new();
        while let Op::Leave(s) = &cfg.nodes[at].op {
            kinds.push(cfg.scopes[*s].kind);
            at = cfg.nodes[at].succ[0].1;
        }
        kinds
    };
    let to = cfg.nodes[jump]
        .succ
        .iter()
        .find(|(e, _)| matches!(e, Edge::Next | Edge::True))
        .map(|(_, to)| *to)
        .expect("the jump continues");
    let mut kinds = chain(to);
    // What the loop's own exit leaves anyway (the enclosing block's end).
    let loop_exit = cfg
        .nodes
        .iter()
        .find(|n| matches!(n.op, Op::Test { is_loop: true, .. }))
        .and_then(|n| n.succ.iter().find(|(e, _)| matches!(e, Edge::False)))
        .map(|(_, to)| chain(*to))
        .unwrap_or_default();
    if !loop_exit.is_empty() && kinds.ends_with(&loop_exit) {
        kinds.truncate(kinds.len() - loop_exit.len());
    }
    kinds
}

const LOOP_BODY: &str = "WHILE i < n LOOP\nBEGIN\ni := i + 1;\nEXIT WHEN i > 3;\nEXCEPTION WHEN OTHERS THEN\ni := 0;\nEND;\nEND LOOP;\nCOMMIT;";

fn pg(body: &str) -> String {
    format!("CREATE PROCEDURE p(n integer) LANGUAGE plpgsql AS $$\nDECLARE i integer := 0;\nBEGIN\n{body}\nEND $$;\n")
}

#[test]
fn exit_out_of_an_exception_block_leaves_its_scope() {
    let kinds = left_by_the_jump(Dialect::Postgres, &pg(LOOP_BODY));
    assert_eq!(kinds, vec![ScopeKind::ExceptionBlock]);
    // CONTINUE goes to the loop head the same way.
    let kinds = left_by_the_jump(
        Dialect::Postgres,
        &pg(&LOOP_BODY.replace("EXIT WHEN", "CONTINUE WHEN")),
    );
    assert_eq!(kinds, vec![ScopeKind::ExceptionBlock]);
}

#[test]
fn exit_in_a_plain_loop_leaves_nothing() {
    let kinds = left_by_the_jump(
        Dialect::Postgres,
        &pg("WHILE i < n LOOP\ni := i + 1;\nEXIT WHEN i > 3;\nEND LOOP;"),
    );
    assert!(kinds.is_empty(), "{kinds:?}");
}

#[test]
fn break_out_of_a_try_block_leaves_the_try_scope() {
    let src = "CREATE PROCEDURE dbo.p @x int\nAS\nBEGIN\nWHILE @x < 9\nBEGIN\nBEGIN TRY\nSET @x = @x + 1;\nBREAK;\nEND TRY\nBEGIN CATCH\nSET @x = 0;\nEND CATCH\nEND\nEND\n";
    // The loop's BEGIN..END body is a plain block, left too.
    assert_eq!(
        left_by_the_jump(Dialect::Tsql, src),
        vec![ScopeKind::Try, ScopeKind::Block]
    );
}
