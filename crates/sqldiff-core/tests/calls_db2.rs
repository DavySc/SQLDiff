//! Procedure calls compared between T-SQL `EXEC` and Db2 for i `CALL`.
//! Only a call without arguments is compared: an argument takes the
//! parameter's type, and the Db2 for i parameter types are not verified
//! facts. Every other shape stays not compared, with a reason.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;
const D: Dialect = Dialect::Db2i;

const CAT_T: &str = "CREATE SCHEMA Sales;
GO
CREATE PROCEDURE Sales.Noop AS BEGIN SET NOCOUNT ON; END;
GO
CREATE PROCEDURE Sales.Other AS BEGIN SET NOCOUNT ON; END;
GO
CREATE PROCEDURE Sales.One @n int AS BEGIN SET NOCOUNT ON; END;
GO
CREATE PROCEDURE Sales.Two @n int AS BEGIN SET NOCOUNT ON; END;
";

const CAT_D: &str = "CREATE SCHEMA SALES;
CREATE PROCEDURE SALES.NOOP () LANGUAGE SQL BEGIN END;
CREATE PROCEDURE SALES.OTHER () LANGUAGE SQL BEGIN END;
CREATE PROCEDURE SALES.ONE (IN P_N INTEGER) LANGUAGE SQL BEGIN END;
";

fn run(a: &str, b: &str, cat_d: &str) -> Report {
    let o = Options {
        catalog_a: vec![Artifact::new("cat.sql", T, CAT_T)],
        catalog_b: vec![Artifact::new("cat.db2.sql", D, cat_d)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", T, a),
        &Artifact::new("b.sql", D, b),
        &o,
    )
}

fn not_compared(a: &str, b: &str, cat_d: &str, needle: &str) {
    let r = run(a, b, cat_d);
    assert!(
        r.unknown_reasons.iter().any(|x| x.contains(needle)),
        "{needle}: {:?}",
        r.unknown_reasons
    );
    assert_ne!(r.verdict(), Verdict::Equal);
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
}

#[test]
fn a_call_without_arguments_is_equal_under_the_callee_assumption() {
    let r = run("EXEC Sales.Noop;", "CALL Sales.Noop();", CAT_D);
    assert!(r.unknown_reasons.is_empty(), "{:?}", r.unknown_reasons);
    assert_eq!(r.verdict(), Verdict::EqualUnderAssumptions);
    let ids: Vec<&str> = r.assumptions.iter().map(|a| a.id.as_str()).collect();
    assert!(ids.contains(&"ASM-CALLEE-CONVERTED"), "{ids:?}");
}

#[test]
fn letter_case_of_the_names_does_not_matter() {
    let r = run("exec sales.noop;", "CALL SALES.NOOP();", CAT_D);
    assert_eq!(r.verdict(), Verdict::EqualUnderAssumptions);
}

#[test]
fn a_call_with_arguments_is_not_compared() {
    not_compared(
        "EXEC Sales.One @n = 3;",
        "CALL Sales.One(3);",
        CAT_D,
        "call arguments are not compared",
    );
}

#[test]
fn a_call_naming_another_procedure_is_not_paired() {
    not_compared(
        "EXEC Sales.Noop;",
        "CALL Sales.Other();",
        CAT_D,
        "no counterpart",
    );
}

#[test]
fn a_name_without_a_schema_is_not_compared() {
    not_compared(
        "EXEC Sales.Noop;",
        "CALL Noop();",
        CAT_D,
        "without a schema",
    );
}

#[test]
fn a_procedure_missing_from_the_db2_catalog_is_not_compared() {
    let cat = "CREATE SCHEMA SALES;\nCREATE PROCEDURE SALES.OTHER () LANGUAGE SQL BEGIN END;\n";
    not_compared(
        "EXEC Sales.Noop;",
        "CALL Sales.Noop();",
        cat,
        "has no procedure",
    );
}

#[test]
fn a_procedure_that_takes_parameters_on_one_side_is_not_compared() {
    // Sales.Two has a parameter on the T-SQL side only.
    let cat = format!("{CAT_D}CREATE PROCEDURE SALES.TWO () LANGUAGE SQL BEGIN END;\n");
    not_compared("EXEC Sales.Two;", "CALL Sales.Two();", &cat, "parameter(s)");
}

#[test]
fn a_quoted_lower_case_db2_name_is_not_the_folded_one() {
    not_compared(
        "EXEC Sales.Noop;",
        "CALL \"Sales\".Noop();",
        CAT_D,
        "lower-case",
    );
}
