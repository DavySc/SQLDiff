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

/// The reason text of an `other` class.
fn why(c: &CClass) -> String {
    c.why().unwrap_or("").to_string()
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
    let (x, y) = not_coupled(
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
    assert_eq!(
        (x, y),
        (
            CClass::FeedingAssignmentNotEqual,
            CClass::FeedingAssignmentNotEqual
        )
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
    let (x, y) = not_coupled(
        &tsql(
            "@x int",
            &format!("DECLARE @n int = 0;\nIF @x > 1 SET @n = @x + 1;\nIF @n > 3 {T};"),
        ),
        &pg(
            "p_x integer",
            "n integer := 0;",
            &format!("IF p_x > 1 THEN NULL; END IF;\nIF n > 3 THEN {T}; END IF;"),
        ),
    );
    assert!(why(&x).contains("no partner"), "{x:?}");
    assert!(why(&y).contains("no partner"), "{y:?}");
}

#[test]
fn a_counter_step_of_one_against_two_is_not_coupled() {
    let (x, y) = not_coupled(
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
    assert_eq!(y, CClass::FeedingAssignmentNotEqual, "{y:?}");
    assert!(why(&x).contains("written in a loop"), "{x:?}");
}

#[test]
fn a_select_assignment_from_a_table_is_not_coupled() {
    let (x, y) = not_coupled(
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
    assert_eq!((x, y), (CClass::ReadsTable, CClass::ReadsTable));
}

#[test]
fn an_output_argument_is_not_coupled() {
    let (x, y) = not_coupled(
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
    assert_eq!((x, y), (CClass::AfterOutputCall, CClass::AfterOutputCall));
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
    assert_eq!(x, CClass::Volatile);
    let (x, y) = not_coupled(
        &tsql(
            "@x int",
            &format!("DECLARE @d datetime2(6) = SYSDATETIME();\nIF @d IS NULL {T};"),
        ),
        &pg(
            "p_x integer",
            "d timestamp := clock_timestamp();",
            &format!("IF d IS NULL THEN {T}; END IF;"),
        ),
    );
    // SYSDATETIME and clock_timestamp are different time sources, so the
    // statement level refuses the pair before volatility is reached: the
    // volatile reason cannot be isolated for the clock.
    assert_eq!(
        (x, y),
        (
            CClass::FeedingAssignmentNotEqual,
            CClass::FeedingAssignmentNotEqual
        )
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
    let (_x, _y) = not_coupled(
        &tsql("@x nvarchar(10)", &format!("IF @x IS NULL {T};")),
        &pg(
            "p_x varchar(10)",
            "",
            &format!("IF p_x IS NULL THEN {T}; END IF;"),
        ),
    );
    assert_eq!(
        (_x, _y),
        (CClass::ParamType, CClass::ParamType),
        "only the type rule may block it"
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
    let (x, y) = not_coupled(
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
    assert!(why(&x).contains("no partner"), "{x:?}");
    assert!(why(&y).contains("no partner"), "{y:?}");
}

#[test]
fn a_shadowed_variable_is_not_coupled() {
    let (x, y) = not_coupled(
        &tsql("@x int", &format!("DECLARE @n int = 1;\nIF @n = 1 {T};")),
        &pg(
            "p_x integer",
            "n integer := 1;",
            &format!("DECLARE n integer := 2;\nBEGIN\nIF n = 1 THEN {T}; END IF;\nEND;"),
        ),
    );
    assert!(why(&x).contains("declared twice"), "{x:?}");
    assert!(why(&y).contains("declared twice"), "{y:?}");
}

#[test]
fn a_multi_target_select_is_not_coupled() {
    // Values are parameters, so the locals pair and only the multi-target
    // rule can keep @a1 out of kappa.
    let (x, y) = not_coupled(
        &tsql(
            "@x int",
            &format!(
                "DECLARE @a1 int;\nDECLARE @b int;\nSELECT @a1 = @x, @b = @x + 1;\nIF @a1 = 1 {T};"
            ),
        ),
        &pg(
            "p_x integer",
            "a1 integer; b integer;",
            &format!("a1 := p_x;\nb := p_x + 1;\nIF a1 = 1 THEN {T}; END IF;"),
        ),
    );
    for c in [x, y] {
        assert_eq!(
            why(&c),
            sqldiff_core::equiv::coupling::MULTI_TARGET,
            "{c:?}"
        );
    }
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

#[test]
fn cx2_a_declare_in_an_untaken_arm_against_a_block_default_is_not_coupled() {
    let (x, y) = not_coupled(
        &tsql(
            "@a int",
            "IF @a = 1 BEGIN DECLARE @x int = 5; END;\nIF @x IS NULL DELETE FROM big;",
        ),
        &pg(
            "a integer",
            "x integer := 5;",
            "IF a = 1 THEN NULL; END IF;\nIF x IS NULL THEN DELETE FROM big; END IF;",
        ),
    );
    // The T-SQL initializer is an assignment at its own point (inside the
    // arm); the PostgreSQL default is one at block entry. Unpaired.
    assert!(why(&x).contains("no partner"), "{x:?}");
    assert!(why(&y).contains("no partner"), "{y:?}");
}

#[test]
fn cx2_a_goto_over_the_declare_against_a_block_default_is_not_coupled() {
    let (x, y) = not_coupled(
        &tsql(
            "@a int",
            "IF @a = 1 GOTO l;\nDECLARE @x int = 5;\nl:\nIF @x IS NULL DELETE FROM big;",
        ),
        &pg(
            "a integer",
            "x integer := 5;",
            "<<goto_l>>\nBEGIN\nIF a = 1 THEN EXIT goto_l; END IF;\nEND;\nIF x IS NULL THEN DELETE FROM big; END IF;",
        ),
    );
    assert!(
        why(&x).contains("no partner") || why(&x).contains("jump leaves"),
        "{x:?}"
    );
    assert!(
        why(&y).contains("no partner") || why(&y).contains("jump leaves"),
        "{y:?}"
    );
}

#[test]
fn a_declare_with_a_value_at_the_same_aligned_point_is_coupled() {
    // T-SQL `DECLARE @v = e` is an assignment at its own point and the
    // PostgreSQL default one at block entry: aligned at the top they couple.
    let (x, y) = last(
        &tsql(
            "@a int",
            "DECLARE @v int = 5;\nIF @v IS NULL DELETE FROM big;",
        ),
        &pg(
            "a integer",
            "v integer := 5;",
            "IF v IS NULL THEN DELETE FROM big; END IF;",
        ),
    );
    assert_eq!((x, y), (CClass::Coupled, CClass::Coupled));
}

/// A deep nest of loops finishes at once: the rows are recorded in the
/// converged round, so a nest that converges in one round costs one walk per
/// level (the review's repro was exponential in the depth).
#[test]
fn a_deep_loop_nest_finishes_and_is_deterministic() {
    for depth in [30usize, 64] {
        let a = tsql(
            "@a int",
            &format!("{}PRINT 1;", "WHILE 1 = 1 ".repeat(depth)),
        );
        let b = pg(
            "a integer",
            "",
            &format!(
                "{}RAISE NOTICE 'x';{}",
                "WHILE 1 = 1 LOOP ".repeat(depth),
                " END LOOP;".repeat(depth)
            ),
        );
        let start = std::time::Instant::now();
        let r = report(&a, &b);
        assert!(
            start.elapsed().as_secs() < 20,
            "depth {depth} took too long"
        );
        assert_eq!(format!("{r:?}"), format!("{:?}", report(&a, &b)));
        let u = &r[0];
        assert!(
            u.refused.is_none() || u.refused.as_deref().unwrap().starts_with("over-cap"),
            "{:?}",
            u.refused
        );
        // Linear in the depth, far below the budget.
        assert!(u.ff_b.steps < 100 * depth as u64, "{:?}", u.ff_b);
    }
}

/// Past the step budget the pair is reported over-cap, deterministically,
/// with no row and no pair counted as coupled.
#[test]
fn a_pair_that_runs_out_of_budget_is_reported_over_cap() {
    let a = tsql(
        "@a int",
        &format!(
            "{}PRINT 1;{}",
            "WHILE @a > 0 BEGIN\n".repeat(8),
            "\nEND".repeat(8)
        ),
    );
    let b = pg(
        "a integer",
        "",
        &format!(
            "{}RAISE NOTICE 'x';{}",
            "WHILE a > 0 LOOP ".repeat(8),
            " END LOOP;".repeat(8)
        ),
    );
    let run = |cap: u64| {
        let (a, b) = (a.clone(), b.clone());
        sqldiff_core::with_engine_stack(move || {
            let aa = Artifact::new("a.sql", Dialect::Tsql, a);
            let bb = Artifact::new("b.sql", Dialect::Postgres, b);
            sqldiff_core::coupling_capped(&aa, &bb, &Options::default(), cap)
        })
    };
    let ok = run(1 << 20);
    assert!(
        ok[0].refused.is_none() && !ok[0].rows.is_empty(),
        "{:?}",
        ok[0]
    );
    let steps = ok[0].ff_a.steps + ok[0].ff_b.steps + ok[0].all_a.steps + ok[0].all_b.steps;
    let r1 = run(steps / 2);
    let r2 = run(steps / 2);
    assert_eq!(format!("{r1:?}"), format!("{r2:?}"));
    let u = &r1[0];
    assert!(
        u.refused.as_deref().unwrap().starts_with("over-cap"),
        "{u:?}"
    );
    assert!(u.rows.is_empty());
    assert_eq!(u.ff_b.exit + u.all_b.exit, 0);
    // The budget is exactly the steps: one more is enough.
    assert!(run(steps)[0].refused.is_none());
}

/// A condition the pairing left out is a row of class `unpaired`, so the
/// counts say how many conditions were not classed.
#[test]
fn an_unpaired_condition_is_a_row() {
    let r = report(
        &tsql("@a int", &format!("IF @a > 1 {T};\nIF @a > 2 {T};")),
        &pg("a integer", "", &format!("IF a > 1 THEN {T}; END IF;")),
    );
    let rows = &r[0].rows;
    // The pairing may leave out either side's conditions; none is silent.
    assert_eq!(rows.iter().filter(|x| x.a_line > 0).count(), 2, "{rows:?}");
    assert_eq!(rows.iter().filter(|x| x.b_line > 0).count(), 1, "{rows:?}");
    assert!(
        rows.iter().any(|x| x.ff_b.label() == "unpaired"),
        "{rows:?}"
    );
}
