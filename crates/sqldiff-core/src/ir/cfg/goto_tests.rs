//! Which `GOTO`s the lowering follows: each case lowers a routine and looks
//! at the node of every `GOTO` statement. A followed jump is an `Op::Nop`;
//! one left alone is a statement whose action is `Opaque("GOTO")`, which
//! the interpreter reports as "GOTO is not modelled" (never a wrong equal).

use super::*;
use crate::artifact::Artifact;
use crate::model::Side;

/// For each `GOTO` statement of the first routine, in source order:
/// `true` when the jump is followed.
fn followed(d: Dialect, text: &str) -> Vec<bool> {
    let art = Artifact::new("p.sql", d, text.to_string());
    let a = Analyzed::new(&art, Side::A, 0);
    let cfg = lower(&a);
    let mut out: Vec<(usize, bool)> = cfg
        .nodes
        .iter()
        .filter_map(|n| {
            let s = n.syntax.as_ref()?;
            let first = ast::sig_tokens(s).next()?;
            if s.kind() != SyntaxKind::STATEMENT || !first.text().eq_ignore_ascii_case("GOTO") {
                return None;
            }
            let pos = usize::from(s.text_range().start());
            match &n.op {
                Op::Nop => Some((pos, true)),
                Op::Stmt(st) if matches!(&st.action, Action::Opaque(w) if w == "GOTO") => {
                    Some((pos, false))
                }
                other => panic!("unexpected op for GOTO: {other:?}"),
            }
        })
        .collect();
    out.sort();
    out.into_iter().map(|(_, f)| f).collect()
}

fn tsql(body: &str) -> String {
    format!("CREATE PROCEDURE dbo.p @x int\nAS\nBEGIN\n{body}\nEND\n")
}

#[test]
fn forward_goto_in_the_same_or_an_enclosing_list_is_followed() {
    let src = tsql("IF @x = 1\nBEGIN\nBEGIN\nGOTO l;\nEND\nEND\nIF @x = 2 GOTO l;\nUPDATE dbo.t SET c = 1;\nl:\nINSERT dbo.t (c) VALUES (1);");
    assert_eq!(followed(Dialect::Tsql, &src), vec![true, true]);
}

#[test]
fn goto_out_of_a_while_or_a_catch_is_followed() {
    let src = tsql("WHILE @x < 9\nBEGIN\nSET @x = @x + 1;\nIF @x = 5 GOTO l;\nEND\nBEGIN TRY\nINSERT dbo.t (c) VALUES (1);\nEND TRY\nBEGIN CATCH\nGOTO l;\nEND CATCH\nUPDATE dbo.t SET c = 1;\nl:\nINSERT dbo.t (c) VALUES (1);");
    assert_eq!(followed(Dialect::Tsql, &src), vec![true, true]);
}

#[test]
fn backward_goto_is_not_followed() {
    let src = tsql("again:\nSET @x = @x + 1;\nIF @x < 3 GOTO again;");
    assert_eq!(followed(Dialect::Tsql, &src), vec![false]);
}

#[test]
fn goto_to_a_label_named_twice_is_not_followed() {
    let src = tsql("IF @x = 1 GOTO l;\nUPDATE dbo.t SET c = 1;\nl:\nINSERT dbo.t (c) VALUES (1);\nl:\nINSERT dbo.t (c) VALUES (2);");
    assert_eq!(followed(Dialect::Tsql, &src), vec![false]);
}

#[test]
fn goto_into_a_block_or_a_sibling_branch_is_not_followed() {
    let src = tsql("IF @x = 1 GOTO l;\nIF @x = 2\nBEGIN\nUPDATE dbo.t SET c = 1;\nl:\nINSERT dbo.t (c) VALUES (1);\nEND");
    assert_eq!(followed(Dialect::Tsql, &src), vec![false]);
    let src = tsql("IF @x = 1\nBEGIN\nGOTO m;\nEND\nELSE\nBEGIN\nUPDATE dbo.t SET c = 1;\nm:\nINSERT dbo.t (c) VALUES (1);\nEND");
    assert_eq!(followed(Dialect::Tsql, &src), vec![false]);
}

#[test]
fn goto_out_of_a_try_block_is_not_followed() {
    let src = tsql("BEGIN TRY\nINSERT dbo.t (c) VALUES (1);\nGOTO l;\nEND TRY\nBEGIN CATCH\nINSERT dbo.e (c) VALUES (1);\nEND CATCH\nUPDATE dbo.t SET c = 1;\nl:\nINSERT dbo.t (c) VALUES (1);");
    assert_eq!(followed(Dialect::Tsql, &src), vec![false]);
}

#[test]
fn db2_goto_out_of_a_handler_or_a_block_with_handlers_is_not_followed() {
    let src = "CREATE PROCEDURE p (IN x INT)\nLANGUAGE SQL\nBEGIN\nBEGIN\nDECLARE CONTINUE HANDLER FOR SQLEXCEPTION\nBEGIN\nINSERT INTO e VALUES (1);\nGOTO l;\nEND;\nIF x = 1 THEN GOTO l; END IF;\nINSERT INTO t VALUES (2);\nEND;\nINSERT INTO t VALUES (4);\nl:\nINSERT INTO t VALUES (3);\nEND\n";
    assert_eq!(followed(Dialect::Db2i, src), vec![false, false]);
}

/// For each `EXIT` statement of the first routine, in source order: `true`
/// when it is followed (an `Op::Nop` that continues elsewhere), `false` when
/// it stays an opaque statement.
fn exits_followed(d: Dialect, text: &str) -> Vec<bool> {
    let art = Artifact::new("p.sql", d, text.to_string());
    let a = Analyzed::new(&art, Side::A, 0);
    let cfg = lower(&a);
    let mut out: Vec<(usize, bool)> = cfg
        .nodes
        .iter()
        .filter_map(|n| {
            let s = n.syntax.as_ref()?;
            let first = ast::sig_tokens(s).next()?;
            if s.kind() != SyntaxKind::STATEMENT || !first.text().eq_ignore_ascii_case("EXIT") {
                return None;
            }
            let pos = usize::from(s.text_range().start());
            Some((pos, matches!(n.op, Op::Nop)))
        })
        .collect();
    out.sort();
    out.into_iter().map(|(_, f)| f).collect()
}

fn pg(body: &str) -> String {
    format!("CREATE PROCEDURE p(p_x integer)\nLANGUAGE plpgsql\nAS $$\nBEGIN\n{body}\nEND;\n$$;\n")
}

#[test]
fn a_postgres_exit_names_the_labelled_block_or_loop_it_leaves() {
    // The label of `<<name>> BEGIN` and `<<name>> LOOP` is the name, not `<<`.
    let block = pg("<<g>>\nBEGIN\nIF p_x = 1 THEN EXIT g; END IF;\nUPDATE t SET c = 1;\nEND;");
    assert_eq!(exits_followed(Dialect::Postgres, &block), vec![true]);
    let lp = pg("<<outer>>\nLOOP\nLOOP\nEXIT outer;\nEND LOOP;\nEND LOOP;");
    assert_eq!(exits_followed(Dialect::Postgres, &lp), vec![true]);
    // A name that labels nothing in scope is left alone.
    let none = pg("<<g>>\nBEGIN\nIF p_x = 1 THEN EXIT h; END IF;\nEND;");
    assert_eq!(exits_followed(Dialect::Postgres, &none), vec![false]);
}
