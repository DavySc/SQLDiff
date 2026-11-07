//! Perf plan W6c step 1 (report only): which paired conditions are the same
//! fact about the input (`equiv::identity`, design `aligned-merge-design.md`
//! E0 and E1). Every adversarial case of the design review must come out not
//! identified; the positives are what the analysis is for.
use sqldiff_core::equiv::identity::Class;
use sqldiff_core::{Artifact, Dialect, Options};

/// Classes of the paired conditions of `a` (T-SQL) against `b` (PostgreSQL),
/// in the order of side A.
fn classes(a: &str, b: &str) -> Vec<Class> {
    let a = a.to_string();
    let b = b.to_string();
    sqldiff_core::with_engine_stack(move || {
        let aa = Artifact::new("a.sql", Dialect::Tsql, a);
        let bb = Artifact::new("b.sql", Dialect::Postgres, b);
        sqldiff_core::identity(&aa, &bb, &Options::default())
            .into_iter()
            .flat_map(|r| r.rows.into_iter().map(|c| c.class))
            .collect()
    })
}

fn tsql(params: &str, body: &str) -> String {
    format!("CREATE PROCEDURE dbo.p {params}\nAS\nBEGIN\n{body}\nEND\n")
}

fn pg(params: &str, decl: &str, body: &str) -> String {
    format!(
        "CREATE PROCEDURE public.p({params})\nLANGUAGE plpgsql AS $$\n{decl}\nBEGIN\n{body}\nEND $$;\n"
    )
}

fn first(c: &[Class]) -> &Class {
    c.first().expect("a paired condition")
}

fn not_identified(c: &[Class]) {
    assert!(!c.is_empty(), "no paired condition at all");
    assert!(
        c.iter().all(|x| !x.identified()),
        "expected no identification, got {c:?}"
    );
}

#[test]
fn a_parameter_only_condition_is_identified() {
    let c = classes(
        &tsql("@x int", "IF @x > 5 UPDATE t SET c = 1;"),
        &pg(
            "p_x integer",
            "",
            "IF p_x > 5 THEN UPDATE t SET c = 1; END IF;",
        ),
    );
    assert_eq!(first(&c), &Class::Value, "{c:?}");
}

#[test]
fn a_local_assigned_once_from_a_parameter_is_identified() {
    let c = classes(
        &tsql(
            "@x int",
            "DECLARE @n int;\nSET @n = @x + 1;\nIF @n > 5 UPDATE t SET c = 1;",
        ),
        &pg(
            "p_x integer",
            "DECLARE v_n integer;",
            "v_n := p_x + 1;\nIF v_n > 5 THEN UPDATE t SET c = 1; END IF;",
        ),
    );
    assert_eq!(first(&c), &Class::Value, "{c:?}");
}

#[test]
fn a_loop_invariant_definition_is_identified() {
    let c = classes(
        &tsql(
            "@x int",
            "DECLARE @n int = 3;\nDECLARE @i int = 0;\nWHILE @i < 10 BEGIN\nIF @n > @x UPDATE t SET c = 1;\nSET @i = @i + 1;\nEND",
        ),
        &pg(
            "p_x integer",
            "DECLARE v_n integer := 3; v_i integer := 0;",
            "WHILE v_i < 10 LOOP\nIF v_n > p_x THEN UPDATE t SET c = 1; END IF;\nv_i := v_i + 1;\nEND LOOP;",
        ),
    );
    // The IF inside the loop reads `n` (invariant) and `x`; the WHILE reads
    // the loop counter and is not identified.
    let (w, i): (Vec<_>, Vec<_>) = c.iter().partition(|x| matches!(x, Class::No(_)));
    assert_eq!(w.len(), 1, "{c:?}");
    assert_eq!(i, vec![&Class::Value], "{c:?}");
}

#[test]
fn a_loop_counter_is_not_identified() {
    let c = classes(
        &tsql(
            "@x int",
            "DECLARE @i int = 0;\nWHILE @i < 10 BEGIN\nSET @i = @i + 1;\nEND",
        ),
        &pg(
            "p_x integer",
            "DECLARE v_i integer := 0;",
            "WHILE v_i < 10 LOOP\nv_i := v_i + 1;\nEND LOOP;",
        ),
    );
    not_identified(&c);
}

#[test]
fn isnull_against_coalesce_is_not_identified() {
    let c = classes(
        &tsql(
            "@y varchar(10)",
            "IF ISNULL(@y, 'longer than ten') = 'a' UPDATE t SET c = 1;",
        ),
        &pg(
            "p_y varchar",
            "",
            "IF COALESCE(p_y, 'longer than ten') = 'a' THEN UPDATE t SET c = 1; END IF;",
        ),
    );
    not_identified(&c);
}

#[test]
fn a_cast_that_can_overflow_is_not_identified() {
    let c = classes(
        &tsql(
            "@x varchar(20)",
            "IF CAST(@x AS int) > 5 UPDATE t SET c = 1;",
        ),
        &pg(
            "p_x varchar",
            "",
            "IF p_x::integer > 5 THEN UPDATE t SET c = 1; END IF;",
        ),
    );
    not_identified(&c);
}

#[test]
fn a_variable_assigned_differently_is_not_identified() {
    let c = classes(
        &tsql(
            "@x int",
            "DECLARE @n int;\nSET @n = @x + 1;\nIF @n > 5 UPDATE t SET c = 1;",
        ),
        &pg(
            "p_x integer",
            "DECLARE v_n integer;",
            "v_n := p_x + 2;\nIF v_n > 5 THEN UPDATE t SET c = 1; END IF;",
        ),
    );
    not_identified(&c);
}

#[test]
fn a_value_derived_from_an_unproven_query_is_not_identified() {
    let c = classes(
        &tsql(
            "@id int",
            "DECLARE @x int, @y int;\nSELECT @x = c FROM t WHERE id = @id;\nSET @y = @x + 1;\nIF @y > 1 UPDATE t SET c = 1;",
        ),
        &pg(
            "p_id integer",
            "DECLARE v_x integer; v_y integer;",
            "SELECT c INTO v_x FROM t WHERE id = p_id;\nv_y := v_x + 1;\nIF v_y > 1 THEN UPDATE t SET c = 1; END IF;",
        ),
    );
    not_identified(&c);
}

#[test]
fn an_assignment_in_one_branch_is_not_identified() {
    let c = classes(
        &tsql(
            "@x int",
            "DECLARE @n int = 0;\nIF @x > 0 SET @n = 1;\nIF @n = 1 UPDATE t SET c = 1;",
        ),
        &pg(
            "p_x integer",
            "DECLARE v_n integer := 0;",
            "IF p_x > 0 THEN v_n := 1; END IF;\nIF v_n = 1 THEN UPDATE t SET c = 1; END IF;",
        ),
    );
    // The first IF reads a parameter; the second reads `n`, which has two
    // reaching definitions.
    assert_eq!(c.len(), 2, "{c:?}");
    assert_eq!(c[0], Class::Value, "{c:?}");
    assert!(!c[1].identified(), "{c:?}");
}

#[test]
fn a_select_assignment_from_a_table_is_not_identified() {
    let c = classes(
        &tsql(
            "@id int",
            "DECLARE @n int = 0;\nSELECT @n = c FROM t WHERE id = @id;\nIF @n = 1 UPDATE t SET c = 1;",
        ),
        &pg(
            "p_id integer",
            "DECLARE v_n integer := 0;",
            "SELECT c INTO v_n FROM t WHERE id = p_id;\nIF v_n = 1 THEN UPDATE t SET c = 1; END IF;",
        ),
    );
    not_identified(&c);
}

#[test]
fn an_output_argument_is_not_identified() {
    let c = classes(
        &tsql(
            "@x int",
            "DECLARE @n int = 0;\nEXEC dbo.q @n OUTPUT;\nIF @n = 1 UPDATE t SET c = 1;",
        ),
        &pg(
            "p_x integer",
            "DECLARE v_n integer := 0;",
            "CALL public.q(v_n);\nIF v_n = 1 THEN UPDATE t SET c = 1; END IF;",
        ),
    );
    not_identified(&c);
}

#[test]
fn an_assignment_in_a_handler_is_not_identified() {
    let c = classes(
        &tsql(
            "@x int",
            "DECLARE @n int = 0;\nBEGIN TRY\nSET @n = 1;\nEND TRY\nBEGIN CATCH\nSET @n = 2;\nEND CATCH\nIF @n = 1 UPDATE t SET c = 1;",
        ),
        &pg(
            "p_x integer",
            "DECLARE v_n integer := 0;",
            "BEGIN\nv_n := 1;\nEXCEPTION WHEN OTHERS THEN v_n := 2;\nEND;\nIF v_n = 1 THEN UPDATE t SET c = 1; END IF;",
        ),
    );
    not_identified(&c);
}

#[test]
fn volatile_definitions_are_not_identified() {
    let c = classes(
        &tsql(
            "@x uniqueidentifier",
            "DECLARE @g uniqueidentifier;\nSET @g = NEWID();\nIF @g = @x UPDATE t SET c = 1;",
        ),
        &pg(
            "p_x uuid",
            "DECLARE v_g uuid;",
            "v_g := gen_random_uuid();\nIF v_g = p_x THEN UPDATE t SET c = 1; END IF;",
        ),
    );
    not_identified(&c);
    let c = classes(
        &tsql(
            "@x datetime2",
            "DECLARE @t datetime2;\nSET @t = SYSDATETIME();\nIF @t > @x UPDATE t SET c = 1;",
        ),
        &pg(
            "p_x timestamp",
            "DECLARE v_t timestamp;",
            "v_t := statement_timestamp();\nIF v_t > p_x THEN UPDATE t SET c = 1; END IF;",
        ),
    );
    not_identified(&c);
}

#[test]
fn a_condition_that_reads_the_database_is_at_best_state_identified() {
    let c = classes(
        &tsql(
            "@x int",
            "UPDATE t SET c = 0;\nIF EXISTS (SELECT 1 FROM t WHERE c = @x) UPDATE t SET c = 1;",
        ),
        &pg(
            "p_x integer",
            "",
            "UPDATE t SET c = 0;\nIF EXISTS (SELECT 1 FROM t WHERE c = p_x) THEN UPDATE t SET c = 1; END IF;",
        ),
    );
    assert!(
        c.iter().all(|x| !matches!(x, Class::Value)),
        "a condition that reads a table is never value-identified: {c:?}"
    );
}

#[test]
fn different_parameter_defaults_are_not_identified() {
    let c = classes(
        &tsql("@x int = 1", "IF @x > 5 UPDATE t SET c = 1;"),
        &pg(
            "p_x integer DEFAULT 2",
            "",
            "IF p_x > 5 THEN UPDATE t SET c = 1; END IF;",
        ),
    );
    not_identified(&c);
}

#[test]
fn parameter_types_that_store_values_differently_are_not_identified() {
    let c = classes(
        &tsql("@y nvarchar(10)", "IF @y = 'a' UPDATE t SET c = 1;"),
        &pg(
            "p_y varchar(10)",
            "",
            "IF p_y = 'a' THEN UPDATE t SET c = 1; END IF;",
        ),
    );
    not_identified(&c);
}

#[test]
fn the_report_is_the_same_twice() {
    let (a, b) = (
        tsql("@x int", "IF @x > 5 UPDATE t SET c = 1;"),
        pg(
            "p_x integer",
            "",
            "IF p_x > 5 THEN UPDATE t SET c = 1; END IF;",
        ),
    );
    assert_eq!(classes(&a, &b), classes(&a, &b));
}
