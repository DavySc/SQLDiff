//! Proposed by the #507 review: one test per drop rule that the PR's tests
//! do not observe.
use sqldiff_core::equiv::coupling::{CClass, UnitCoupling};
use sqldiff_core::{Artifact, Dialect, Options};

fn report(a: &str, b: &str) -> Vec<UnitCoupling> {
    let (a, b) = (a.to_string(), b.to_string());
    sqldiff_core::with_engine_stack(move || {
        let aa = Artifact::new("a.sql", Dialect::Tsql, a);
        let bb = Artifact::new("b.sql", Dialect::Postgres, b);
        sqldiff_core::coupling(&aa, &bb, &Options::default()).0
    })
}
fn tsql(params: &str, body: &str) -> String {
    format!("CREATE PROCEDURE dbo.p {params}\nAS\nBEGIN\n{body}\nEND\n")
}
fn pg(params: &str, decl: &str, body: &str) -> String {
    let decl = if decl.is_empty() {
        String::new()
    } else {
        format!("DECLARE {decl}")
    };
    format!("CREATE PROCEDURE public.p({params})\nLANGUAGE plpgsql AS $$\n{decl}\nBEGIN\n{body}\nEND $$;\n")
}
/// Classes of the last paired condition: (ff a, ff b, all a, all b).
fn last(a: &str, b: &str) -> Vec<CClass> {
    let r = report(a, b);
    let u = r.first().expect("a routine pair");
    assert!(u.refused.is_none(), "refused: {:?}", u.refused);
    let row = u.rows.last().expect("a paired condition");
    eprintln!("{row:?}");
    vec![
        row.ff_a.clone(),
        row.ff_b.clone(),
        row.all_a.clone(),
        row.all_b.clone(),
    ]
}
fn none_coupled(v: Vec<CClass>) {
    assert!(v.iter().all(|c| *c != CClass::Coupled), "{v:?}");
}
const T: &str = "UPDATE t SET c = 1";

#[test]
fn x_different_terms_in_the_arms_of_an_uncoupled_if() {
    none_coupled(last(
        &tsql("@a int", &format!("DECLARE @c int; DECLARE @x int = 0;\nSELECT @c = c FROM t;\nIF @c = 1 SET @x = 1; ELSE SET @x = 2;\nIF @x = 1 {T};")),
        &pg("a integer", "c integer; x integer := 0;", &format!("SELECT c INTO c FROM t;\nIF c = 1 THEN x := 1; ELSE x := 2; END IF;\nIF x = 1 THEN {T}; END IF;")),
    ));
}

#[test]
fn x_catch_entry_is_not_the_end_of_the_try() {
    none_coupled(last(
        &tsql("@a int", &format!("DECLARE @x int = 0;\nBEGIN TRY\nSET @x = @a + 1;\n{T};\nSET @x = 0;\nEND TRY\nBEGIN CATCH\nIF @x = 0 {T};\nEND CATCH")),
        &pg("a integer", "x integer := 0;", &format!("BEGIN\nx := a + 2;\n{T};\nx := 0;\nEXCEPTION WHEN OTHERS THEN\nIF x = 0 THEN {T}; END IF;\nEND;")),
    ));
}

#[test]
fn x_a_write_a_goto_can_skip() {
    none_coupled(last(
        &tsql("@a int", &format!("DECLARE @x int = 0;\nIF @a = 1 GOTO l;\nSET @x = 1;\nl:\nIF @x = 1 {T};")),
        &pg("a integer", "x integer := 0;", &format!("<<goto_l>>\nBEGIN\nIF a = 1 THEN EXIT goto_l; END IF;\nx := 1;\nEND;\nIF x = 1 THEN {T}; END IF;")),
    ));
}

#[test]
fn x_a_loop_left_by_break_keeps_only_unwritten_pairs() {
    // At the head and at the end of the body `@x` is 0 on both sides; at the
    // BREAK it holds values that differ. The loop exit is also reached from
    // the BREAK, so the head's kappa does not hold after the loop.
    none_coupled(last(
        &tsql("@a int", &format!("DECLARE @i int = 0; DECLARE @x int = 0;\nWHILE @i < 10 BEGIN SET @i = @i + 1; SET @x = @a + 1; IF @i = 5 BREAK; SET @x = 0; END\nIF @x = 0 {T};")),
        &pg("a integer", "i integer := 0; x integer := 0;", &format!("WHILE i < 10 LOOP i := i + 1; x := a + 2; IF i = 5 THEN EXIT; END IF; x := 0; END LOOP;\nIF x = 0 THEN {T}; END IF;")),
    ));
}

#[test]
fn x_a_declare_without_value_in_a_loop() {
    // T-SQL keeps @y from the previous iteration; PL/pgSQL resets y to NULL
    // at each entry to the block.
    none_coupled(last(
        &tsql("@a int", &format!("DECLARE @i int = 0;\nWHILE @i < 3 BEGIN\nDECLARE @y int;\nIF @y IS NULL {T};\nSET @y = 1;\nSET @i = @i + 1;\nEND")),
        &pg("a integer", "i integer := 0;", &format!("WHILE i < 3 LOOP\nDECLARE y integer;\nBEGIN\nIF y IS NULL THEN {T}; END IF;\ny := 1;\ni := i + 1;\nEND;\nEND LOOP;")),
    ));
}
