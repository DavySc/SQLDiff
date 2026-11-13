//! A witness must be a value the operand can take. CEILING/FLOOR give whole
//! numbers, so "ceil(x) = 1.5" shows nothing; a datetime2(3) value has no
//! fourth digit. Neither pair may be reported as different for that reason.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

fn run(a: &str, b: &str) -> Report {
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Postgres, b),
        &Options::default(),
    )
}

fn proc(params_t: &str, params_p: &str, body_t: &str, body_p: &str) -> (String, String) {
    (
        format!("CREATE PROCEDURE dbo.p {params_t}\nAS\nBEGIN\n  SET NOCOUNT ON;\n  SET XACT_ABORT ON;\n  DECLARE @d datetime2(7);\n  {body_t}\n  INSERT INTO dbo.t (id, w) VALUES (1, @d);\nEND\n"),
        format!("CREATE OR REPLACE PROCEDURE p({params_p})\nLANGUAGE plpgsql\nAS $$\nDECLARE\n  v_d timestamp;\nBEGIN\n  {body_p}\n  INSERT INTO t (id, w) VALUES (1, v_d);\nEND;\n$$;\n"),
    )
}

fn not_different(a: &str, b: &str) {
    let r = run(a, b);
    assert!(
        !matches!(r.verdict(), Verdict::Different),
        "{a}\n{b}\n{:?}\n{:#?}",
        r.verdict(),
        r.findings
    );
}

#[test]
fn ceiling_of_a_float_has_no_fraction_witness() {
    let (a, b) = proc(
        "@c datetime2(7), @f float",
        "p_c timestamp, p_f double precision",
        "SET @d = DATEADD(hour, CEILING(@f), @c);",
        "v_d := p_c + make_interval(hours => ceil(p_f)::integer);",
    );
    not_different(&a, &b);
    let (a, b) = proc(
        "@f float",
        "p_f double precision",
        "SET @d = CAST(CEILING(@f) AS int);",
        "v_d := ceil(p_f)::integer;",
    );
    not_different(&a, &b);
}

#[test]
fn a_three_digit_source_has_no_four_digit_witness() {
    let (a, b) = proc(
        "@o datetime2(3)",
        "p_o timestamp(3)",
        "SET @d = CAST(CAST(@o AS datetime2) AS datetime2(3));",
        "v_d := (p_o::timestamp)::timestamp(3);",
    );
    not_different(&a, &b);
}

#[test]
fn a_bigint_dateadd_amount_is_not_proven() {
    let (a, b) = proc(
        "@c datetime2(7), @n bigint",
        "p_c timestamp, p_n bigint",
        "SET @d = DATEADD(hour, @n, @c);",
        "v_d := p_c + make_interval(hours => p_n::integer);",
    );
    let r = run(&a, &b);
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.verdict()
    );
}
