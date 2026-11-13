//! Region-comparison design, stage 0a (report only): relational variable
//! coupling (`equiv::coupling`). Each never-coupled case reads the tested
//! variable in a condition after the statement under test, so the test
//! observes what it claims.
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
    format!(
        "CREATE PROCEDURE public.p({params})\nLANGUAGE plpgsql AS $$\n{decl}\nBEGIN\n{body}\nEND $$;\n"
    )
}

/// (a), (b) classes under kappa_ff of the last paired condition.
fn last(a: &str, b: &str) -> (CClass, CClass) {
    let r = report(a, b);
    let u = r.first().expect("a routine pair");
    assert!(u.refused.is_none(), "refused: {:?}", u.refused);
    let row = u.rows.last().expect("a paired condition");
    (row.ff_a.clone(), row.ff_b.clone())
}

fn not_coupled(a: &str, b: &str) -> (CClass, CClass) {
    let (x, y) = last(a, b);
    assert_ne!(x, CClass::Coupled, "(a) coupled");
    assert_ne!(y, CClass::Coupled, "(b) coupled");
    (x, y)
}

const T: &str = "UPDATE t SET c = 1";

#[test]
fn a_parameter_only_condition_is_coupled() {
    let (x, y) = last(
        &tsql("@x int", &format!("IF @x > 5 {T};")),
        &pg("p_x integer", "", &format!("IF p_x > 5 THEN {T}; END IF;")),
    );
    assert_eq!((x, y), (CClass::Coupled, CClass::Coupled));
}

#[test]
fn a_variable_assigned_differently_is_not_coupled() {
    not_coupled(
        &tsql(
            "@x int",
            &format!("DECLARE @n int = 0;\nSET @n = @x + 1;\nIF @n > 5 {T};"),
        ),
        &pg(
            "p_x integer",
            "n integer := 0;",
            &format!("n := p_x + 2;\nIF n > 5 THEN {T}; END IF;"),
        ),
    );
}

#[test]
fn a_value_derived_from_a_select_from_a_table_is_not_coupled() {
    let (x, _) = not_coupled(
        &tsql(
            "@x int",
            &format!(
                "DECLARE @x2 int;\nDECLARE @y int;\nSELECT @x2 = c FROM t;\nSET @y = @x2 + 1;\nIF @y > 5 {T};"
            ),
        ),
        &pg(
            "p_x integer",
            "x2 integer; y integer;",
            &format!("SELECT c INTO x2 FROM t;\ny := x2 + 1;\nIF y > 5 THEN {T}; END IF;"),
        ),
    );
    assert_eq!(x, CClass::ReadsTable);
}

#[test]
fn an_assignment_in_one_if_arm_is_not_coupled() {
    not_coupled(
        &tsql(
            "@x int",
            &format!("DECLARE @n int = 0;\nIF @x > 1 SET @n = 5;\nIF @n > 3 {T};"),
        ),
        &pg(
            "p_x integer",
            "n integer := 0;",
            &format!("IF p_x > 1 THEN NULL; END IF;\nIF n > 3 THEN {T}; END IF;"),
        ),
    );
}

#[test]
fn a_counter_step_of_one_against_two_is_not_coupled() {
    not_coupled(
        &tsql(
            "@x int",
            "DECLARE @i int = 0;\nWHILE @i < 10 BEGIN\nSET @i = @i + 1;\nEND",
        ),
        &pg(
            "p_x integer",
            "i integer := 0;",
            "WHILE i < 10 LOOP\ni := i + 2;\nEND LOOP;",
        ),
    );
}

#[test]
fn a_select_assignment_from_a_table_is_not_coupled() {
    not_coupled(
        &tsql(
            "@x int",
            &format!("DECLARE @v int;\nSELECT @v = c FROM t;\nIF @v > 1 {T};"),
        ),
        &pg(
            "p_x integer",
            "v integer;",
            &format!("SELECT c INTO v FROM t;\nIF v > 1 THEN {T}; END IF;"),
        ),
    );
}

#[test]
fn an_output_argument_is_not_coupled() {
    let (x, _) = not_coupled(
        &tsql(
            "@x int",
            &format!("DECLARE @v int = 0;\nEXEC q @v OUTPUT;\nIF @v > 1 {T};"),
        ),
        &pg(
            "p_x integer",
            "v integer := 0;",
            &format!("CALL q(v);\nIF v > 1 THEN {T}; END IF;"),
        ),
    );
    assert_ne!(x, CClass::Coupled);
}

#[test]
fn a_fetch_into_variable_is_not_coupled() {
    not_coupled(
        &tsql(
            "@x int",
            &format!(
                "DECLARE @v int;\nDECLARE c CURSOR FOR SELECT c FROM t;\nOPEN c;\nFETCH NEXT FROM c INTO @v;\nIF @v > 1 {T};"
            ),
        ),
        &pg(
            "p_x integer",
            "v integer; c CURSOR FOR SELECT c FROM t;",
            &format!("OPEN c;\nFETCH c INTO v;\nIF v > 1 THEN {T}; END IF;"),
        ),
    );
}

#[test]
fn random_and_clock_values_are_not_coupled() {
    let (x, _) = not_coupled(
        &tsql(
            "@x int",
            &format!("DECLARE @u uniqueidentifier = NEWID();\nIF @u IS NULL {T};"),
        ),
        &pg(
            "p_x integer",
            "u uuid := gen_random_uuid();",
            &format!("IF u IS NULL THEN {T}; END IF;"),
        ),
    );
    assert_ne!(x, CClass::Coupled);
    not_coupled(
        &tsql(
            "@x int",
            &format!("DECLARE @d datetime2 = SYSDATETIME();\nIF @d IS NULL {T};"),
        ),
        &pg(
            "p_x integer",
            "d timestamp := clock_timestamp();",
            &format!("IF d IS NULL THEN {T}; END IF;"),
        ),
    );
}

#[test]
fn different_parameter_defaults_are_not_coupled() {
    let (x, y) = not_coupled(
        &tsql("@x int = 1", &format!("IF @x > 5 {T};")),
        &pg(
            "p_x integer DEFAULT 2",
            "",
            &format!("IF p_x > 5 THEN {T}; END IF;"),
        ),
    );
    assert_eq!((x, y), (CClass::ParamType, CClass::ParamType));
}

#[test]
fn a_varchar_parameter_against_nvarchar_is_not_coupled() {
    not_coupled(
        &tsql("@x nvarchar(10)", &format!("IF @x = 'a' {T};")),
        &pg(
            "p_x varchar(10)",
            "",
            &format!("IF p_x = 'a' THEN {T}; END IF;"),
        ),
    );
    // A length the PostgreSQL parameter ignores.
    let (x, _) = not_coupled(
        &tsql("@x varchar(10)", &format!("IF @x IS NULL {T};")),
        &pg(
            "p_x varchar(10)",
            "",
            &format!("IF p_x IS NULL THEN {T}; END IF;"),
        ),
    );
    assert_eq!(x, CClass::ParamType);
}

#[test]
fn a_declare_in_an_untaken_arm_against_a_block_default_is_not_coupled() {
    not_coupled(
        &tsql(
            "@x int",
            &format!("IF @x > 100 BEGIN DECLARE @n int = 5; END\nIF @n = 5 {T};"),
        ),
        &pg(
            "p_x integer",
            "n integer := 5;",
            &format!("IF p_x > 100 THEN NULL; END IF;\nIF n = 5 THEN {T}; END IF;"),
        ),
    );
}

#[test]
fn a_shadowed_variable_is_not_coupled() {
    not_coupled(
        &tsql("@x int", &format!("DECLARE @n int = 1;\nIF @n = 1 {T};")),
        &pg(
            "p_x integer",
            "n integer := 1;",
            &format!("DECLARE n integer := 2;\nBEGIN\nIF n = 1 THEN {T}; END IF;\nEND;"),
        ),
    );
}

#[test]
fn a_multi_target_select_is_not_coupled() {
    not_coupled(
        &tsql(
            "@x int",
            &format!("DECLARE @a int;\nDECLARE @b int;\nSELECT @a = 1, @b = 2;\nIF @a = 1 {T};"),
        ),
        &pg(
            "p_x integer",
            "a integer; b integer;",
            &format!("a := 1;\nb := 2;\nIF a = 1 THEN {T}; END IF;"),
        ),
    );
}

#[test]
fn a_loop_counter_is_coupled_with_the_fixpoint_and_not_without() {
    let (x, y) = last(
        &tsql(
            "@x int",
            &format!("DECLARE @i int = 0;\nWHILE @i < 10 BEGIN\n{T};\nSET @i = @i + 1;\nEND"),
        ),
        &pg(
            "p_x integer",
            "i integer := 0;",
            &format!("WHILE i < 10 LOOP\n{T};\ni := i + 1;\nEND LOOP;"),
        ),
    );
    assert_eq!(x, CClass::LoopCarried);
    assert_eq!(y, CClass::Coupled);
}

#[test]
fn both_arms_assigning_the_same_term_couple_after_the_join() {
    let (x, y) = last(
        &tsql(
            "@x int",
            &format!(
                "DECLARE @n int = 0;\nIF @x > 1 SET @n = @x + 1; ELSE SET @n = @x + 1;\nIF @n > 3 {T};"
            ),
        ),
        &pg(
            "p_x integer",
            "n integer := 0;",
            &format!(
                "IF p_x > 1 THEN n := p_x + 1; ELSE n := p_x + 1; END IF;\nIF n > 3 THEN {T}; END IF;"
            ),
        ),
    );
    assert_eq!((x, y), (CClass::Coupled, CClass::Coupled));
}

#[test]
fn the_report_is_the_same_twice_and_counts_work() {
    let a = tsql(
        "@x int",
        &format!("DECLARE @i int = 0;\nWHILE @i < 10 BEGIN\nSET @i = @i + 1;\nEND\nIF @x > 1 {T};"),
    );
    let b = pg(
        "p_x integer",
        "i integer := 0;",
        &format!("WHILE i < 10 LOOP\ni := i + 1;\nEND LOOP;\nIF p_x > 1 THEN {T}; END IF;"),
    );
    let run = || {
        let (x, y) = (a.clone(), b.clone());
        sqldiff_core::with_engine_stack(move || {
            let aa = Artifact::new("a.sql", Dialect::Tsql, x);
            let bb = Artifact::new("b.sql", Dialect::Postgres, y);
            sqldiff_core::coupling(&aa, &bb, &Options::default())
        })
    };
    let (r1, w1) = run();
    let (r2, w2) = run();
    assert_eq!(format!("{r1:?}"), format!("{r2:?}"));
    assert_eq!(w1, w2);
    assert!(w1.get(sqldiff_model::work::Site::Coupling) > 0);
    assert!(r1[0].ff_b.rounds >= 1, "{:?}", r1[0]);
}

/// The diff path never computes the report: `diff` leaves the work meter's
/// coupling site at zero and its report is the same with and without a
/// `coupling` run before it.
#[test]
fn the_diff_path_does_not_run_the_analysis() {
    let a = tsql("@x int", &format!("IF @x > 5 {T};"));
    let b = pg("p_x integer", "", &format!("IF p_x > 5 THEN {T}; END IF;"));
    let diff = |a: String, b: String| {
        sqldiff_core::with_engine_stack(move || {
            let aa = Artifact::new("a.sql", Dialect::Tsql, a);
            let bb = Artifact::new("b.sql", Dialect::Postgres, b);
            let (r, w) = sqldiff_model::work::scope("t", || {
                sqldiff_core::diff(&aa, &bb, &Options::default())
            });
            (format!("{r:?}"), w)
        })
    };
    let (before, w) = diff(a.clone(), b.clone());
    assert_eq!(w.get(sqldiff_model::work::Site::Coupling), 0);
    report(&a, &b);
    let (after, _) = diff(a, b);
    assert_eq!(before, after);
}
