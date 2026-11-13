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
GO
CREATE TABLE Sales.Things (Id int NOT NULL PRIMARY KEY, Name nvarchar(50) NOT NULL);
GO
CREATE VIEW Sales.ThingNames AS SELECT Name FROM Sales.Things;
";

const CAT_D: &str = "CREATE SCHEMA SALES;
CREATE PROCEDURE SALES.NOOP () LANGUAGE SQL BEGIN END;
CREATE PROCEDURE SALES.OTHER () LANGUAGE SQL BEGIN END;
CREATE PROCEDURE SALES.ONE (IN P_N INTEGER) LANGUAGE SQL BEGIN END;
CREATE TABLE SALES.THINGS (ID INTEGER NOT NULL PRIMARY KEY, NAME VARCHAR(50) NOT NULL);
CREATE VIEW SALES.THINGNAMES AS SELECT NAME FROM SALES.THINGS;
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

const CAT_T_REP: &str = "CREATE SCHEMA Sales;
GO
CREATE PROCEDURE Sales.Rep AS BEGIN SELECT 1 AS One; END;
";

fn run_rep(a: &str, b: &str, cat_d: &str) -> Report {
    let o = Options {
        catalog_a: vec![Artifact::new("cat.sql", T, CAT_T_REP)],
        catalog_b: vec![Artifact::new("cat.db2.sql", D, cat_d)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", T, a),
        &Artifact::new("b.sql", D, b),
        &o,
    )
}

#[test]
fn a_callee_that_returns_result_sets_to_its_caller_is_not_compared() {
    // T-SQL sends the rows to the client; this Db2 for i callee leaves its
    // cursor open for the caller of the CALL.
    let cat = "CREATE SCHEMA SALES;
CREATE PROCEDURE SALES.REP () LANGUAGE SQL DYNAMIC RESULT SETS 1
BEGIN
  DECLARE C CURSOR WITH RETURN TO CALLER FOR SELECT 1 FROM SYSIBM.SYSDUMMY1;
  OPEN C;
END;
";
    let r = run_rep("EXEC Sales.Rep;", "CALL Sales.Rep();", cat);
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.contains("DYNAMIC RESULT SETS")),
        "{:?}",
        r.unknown_reasons
    );
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
}

#[test]
fn result_sets_without_dynamic_is_read_too() {
    for decl in ["RESULT SETS 1", "RESULT SET 1", "DYNAMIC RESULT SETS 2"] {
        let cat = format!(
            "CREATE SCHEMA SALES;\nCREATE PROCEDURE SALES.REP () {decl} LANGUAGE SQL\nBEGIN\n  DECLARE C CURSOR WITH RETURN TO CALLER FOR SELECT 1 FROM SYSIBM.SYSDUMMY1;\n  OPEN C;\nEND;\n"
        );
        let r = run_rep("EXEC Sales.Rep;", "CALL Sales.Rep();", &cat);
        assert!(
            r.unknown_reasons.iter().any(|x| x.contains("RESULT SETS")),
            "{decl}: {:?}",
            r.unknown_reasons
        );
        assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions, "{decl}");
    }
}

#[test]
fn a_callee_with_zero_result_sets_is_compared() {
    let cat = "CREATE SCHEMA SALES;
CREATE PROCEDURE SALES.REP () LANGUAGE SQL DYNAMIC RESULT SETS 0
BEGIN
END;
";
    let r = run_rep("EXEC Sales.Rep;", "CALL Sales.Rep();", cat);
    assert!(r.unknown_reasons.is_empty(), "{:?}", r.unknown_reasons);
    assert_eq!(r.verdict(), Verdict::EqualUnderAssumptions);
}

#[test]
fn an_argument_on_one_side_only_is_never_equal() {
    let r = run("EXEC Sales.Noop 3;", "CALL Sales.Noop();", CAT_D);
    assert_ne!(r.verdict(), Verdict::Equal);
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
}

#[test]
fn a_callee_whose_result_sets_the_catalog_did_not_read_is_not_compared() {
    use sqldiff_core::catalog::{ddl, Catalog};
    use sqldiff_core::Facts;
    use std::sync::Arc;

    // A catalog an adapter sent (no DDL read) does not say whether the
    // procedure declares result sets: the call must not count as compared.
    let mut cat = Catalog::default();
    let root = sqldiff_syntax::parse(CAT_D, D).syntax();
    ddl::load_tree(&mut cat, &root, D);
    for r in &mut cat.routines {
        r.dynamic_result_sets = None;
    }
    let o = Options {
        catalog_a: vec![Artifact::new("cat.sql", T, CAT_T)],
        facts_b: vec![Facts::new(Arc::new(cat))],
        ..Options::default()
    };
    let r = diff(
        &Artifact::new("a.sql", T, "EXEC Sales.Noop;"),
        &Artifact::new("b.sql", D, "CALL Sales.Noop();"),
        &o,
    );
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.contains("does not say whether it declares DYNAMIC RESULT SETS")),
        "{:?}",
        r.unknown_reasons
    );
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
}

fn ep(level1: &str, name: &str, col: Option<&str>, text: &str) -> String {
    let c = col.map_or(String::new(), |c| {
        format!(", @level2type = N'COLUMN', @level2name = N'{c}'")
    });
    format!(
        "EXEC sp_addextendedproperty @name = N'MS_Description', @value = N'{text}', @level0type = N'SCHEMA', @level0name = N'Sales', @level1type = N'{level1}', @level1name = N'{name}'{c};"
    )
}

fn comment_equal(a: &str, b: &str) {
    let r = run(a, b, CAT_D);
    assert!(r.unknown_reasons.is_empty(), "{:?}", r.unknown_reasons);
    assert!(
        !r.findings.iter().any(|f| f.verdict == Verdict::Different),
        "{:#?}",
        r.findings
    );
    assert_eq!(r.verdict(), Verdict::EqualUnderAssumptions);
    let ids: Vec<&str> = r.assumptions.iter().map(|a| a.id.as_str()).collect();
    for id in ["ASM-EXTPROP-COMMENT", "ASM-FACT-V35"] {
        assert!(ids.contains(&id), "{id}: {ids:?}");
    }
}

#[test]
fn a_table_comment_is_equal_under_the_comment_assumptions() {
    comment_equal(
        &ep("TABLE", "Things", None, "Things sold"),
        "COMMENT ON TABLE Sales.Things IS 'Things sold';",
    );
}

#[test]
fn a_column_comment_is_equal() {
    comment_equal(
        &ep("TABLE", "Things", Some("Name"), "it''s the name"),
        "COMMENT ON COLUMN Sales.Things.Name IS 'it''s the name';",
    );
}

#[test]
fn a_schema_comment_is_equal() {
    comment_equal(
        "EXEC sp_addextendedproperty @name = N'MS_Description', @value = N'Sales data', @level0type = N'SCHEMA', @level0name = N'Sales';",
        "COMMENT ON SCHEMA Sales IS 'Sales data';",
    );
}

#[test]
fn a_different_comment_text_differs() {
    let r = run(
        &ep("TABLE", "Things", None, "Things sold"),
        "COMMENT ON TABLE Sales.Things IS 'Things bought';",
        CAT_D,
    );
    assert_eq!(r.verdict(), Verdict::Different, "{:?}", r.unknown_reasons);
    assert!(r.findings.iter().any(|f| f.rule == "equiv.comment-text"));
}

#[test]
fn a_comment_longer_than_the_checked_limit_is_not_compared() {
    let long = "x".repeat(255);
    not_compared(
        &ep("TABLE", "Things", None, &long),
        &format!("COMMENT ON TABLE Sales.Things IS '{long}';"),
        CAT_D,
        "fact V35",
    );
}

#[test]
fn the_limit_itself_is_compared() {
    let at = "x".repeat(254);
    comment_equal(
        &ep("TABLE", "Things", None, &at),
        &format!("COMMENT ON TABLE Sales.Things IS '{at}';"),
    );
}

#[test]
fn a_comment_that_is_not_printable_ascii_is_not_compared() {
    not_compared(
        &ep("TABLE", "Things", None, "caf\u{e9}"),
        "COMMENT ON TABLE Sales.Things IS 'caf\u{e9}';",
        CAT_D,
        "printable ASCII",
    );
}

#[test]
fn a_view_comment_is_not_compared() {
    not_compared(
        &ep("VIEW", "ThingNames", None, "Names"),
        "COMMENT ON TABLE Sales.ThingNames IS 'Names';",
        CAT_D,
        "no counterpart",
    );
}

#[test]
fn a_view_column_comment_is_not_compared() {
    not_compared(
        &ep("VIEW", "ThingNames", Some("Name"), "Name"),
        "COMMENT ON COLUMN Sales.ThingNames.Name IS 'Name';",
        CAT_D,
        "view's column",
    );
}

#[test]
fn a_column_missing_from_the_db2_catalog_is_not_compared() {
    let cat =
        "CREATE SCHEMA SALES;\nCREATE TABLE SALES.THINGS (ID INTEGER NOT NULL PRIMARY KEY);\n";
    not_compared(
        &ep("TABLE", "Things", Some("Name"), "n"),
        "COMMENT ON COLUMN Sales.Things.Name IS 'n';",
        cat,
        "catalog has no column",
    );
}

#[test]
fn a_table_missing_from_the_db2_catalog_is_not_compared() {
    let cat = "CREATE SCHEMA SALES;\n";
    not_compared(
        &ep("TABLE", "Things", None, "n"),
        "COMMENT ON TABLE Sales.Things IS 'n';",
        cat,
        "catalog has no",
    );
}

#[test]
fn a_comment_on_other_objects_is_not_compared() {
    not_compared(
        &ep("TABLE", "Things", None, "n"),
        "COMMENT ON VIEW Sales.Things IS 'n';",
        CAT_D,
        "COMMENT ON VIEW",
    );
}

#[test]
fn an_unqualified_table_comment_is_not_compared() {
    not_compared(
        &ep("TABLE", "Things", None, "n"),
        "COMMENT ON TABLE Things IS 'n';",
        CAT_D,
        "full qualified name",
    );
}
