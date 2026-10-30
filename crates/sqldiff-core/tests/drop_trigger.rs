//! T-SQL `DROP TRIGGER` against the PostgreSQL `DROP TRIGGER … ON table`
//! and `DROP FUNCTION IF EXISTS f()` it became. The table and the function
//! come from the catalogs: side A's CREATE TRIGGER says which table, side
//! B's which function.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const TABLE_T: &str = "CREATE TABLE Sales.Orders (OrderID int NOT NULL PRIMARY KEY, Total int NULL);\nGO\nCREATE TABLE Sales.Lines (LineID int NOT NULL PRIMARY KEY);\nGO\n";
const TRIG_T: &str = "CREATE TRIGGER Sales.TR_Orders ON Sales.Orders AFTER INSERT AS\nBEGIN\n    SET NOCOUNT ON;\nEND;\nGO\n";
const TABLE_P: &str = "CREATE TABLE Sales.Orders (OrderID integer NOT NULL PRIMARY KEY, Total integer NULL);\nCREATE TABLE Sales.Lines (LineID integer NOT NULL PRIMARY KEY);\n";
const TRIG_P: &str = "CREATE OR REPLACE FUNCTION Sales.TR_Orders() RETURNS trigger\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  RETURN NULL;\nEND;\n$$;\n\nCREATE OR REPLACE TRIGGER TR_Orders\nAFTER INSERT ON Sales.Orders\nREFERENCING NEW TABLE AS inserted\nFOR EACH STATEMENT\nEXECUTE FUNCTION Sales.TR_Orders();\n";

fn proc_t(drop: &str) -> String {
    format!("CREATE PROCEDURE Sales.DropIt\nAS BEGIN\n    SET XACT_ABORT ON;\n    DELETE FROM Sales.Lines WHERE LineID < 0;\n    {drop}\nEND;\n")
}

fn proc_p(drop: &str) -> String {
    format!("CREATE OR REPLACE PROCEDURE Sales.DropIt()\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  DELETE FROM Sales.Lines WHERE LineID < 0;\n  {drop}\nEND;\n$$;\n")
}

const DROP_P: &str =
    "DROP TRIGGER IF EXISTS TR_Orders ON Sales.Orders;\n  DROP FUNCTION IF EXISTS Sales.TR_Orders();";

fn run_with(a: &str, b: &str, cat_t: &str, cat_p: &str) -> Report {
    let opts = Options {
        catalog_a: vec![Artifact::new("ca.sql", Dialect::Tsql, cat_t)],
        catalog_b: vec![Artifact::new("cb.sql", Dialect::Postgres, cat_p)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Postgres, b),
        &opts,
    )
}

fn run(a: &str, b: &str) -> Report {
    run_with(
        a,
        b,
        &format!("{TABLE_T}{TRIG_T}"),
        &format!("{TABLE_P}{TRIG_P}"),
    )
}

fn equal(r: Report) {
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:#?}\n{:#?}",
        r.findings,
        r.unknown_reasons
    );
    assert!(r.unknown_reasons.is_empty(), "{:?}", r.unknown_reasons);
    assert!(
        r.assumptions.iter().any(|a| a.id == "ASM-TRIGGER-DROP"),
        "{:?}",
        r.assumptions
    );
}

fn unknown(r: Report, needle: &str) {
    assert!(
        r.unknown_reasons.iter().any(|x| x.contains(needle)),
        "{needle}: {:?}",
        r.unknown_reasons
    );
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
    assert_ne!(r.verdict(), Verdict::Equal);
}

#[test]
fn a_drop_trigger_in_a_procedure_is_compared() {
    equal(run(
        &proc_t("DROP TRIGGER IF EXISTS [Sales].[TR_Orders];"),
        &proc_p(DROP_P),
    ));
    equal(run(
        &proc_t("DROP TRIGGER Sales.TR_Orders;"),
        &proc_p(&DROP_P.replace("IF EXISTS TR_Orders", "TR_Orders")),
    ));
}

#[test]
fn a_drop_trigger_in_a_script_is_compared() {
    equal(run(
        "DROP TRIGGER Sales.TR_Orders;\n",
        "DROP TRIGGER TR_Orders ON Sales.Orders;\nDROP FUNCTION IF EXISTS Sales.TR_Orders();\n",
    ));
}

#[test]
fn if_exists_on_one_side_only_differs() {
    let r = run(&proc_t("DROP TRIGGER Sales.TR_Orders;"), &proc_p(DROP_P));
    assert_eq!(r.verdict(), Verdict::Different, "{:?}", r.unknown_reasons);
    assert!(r.findings.iter().any(|f| f.rule == "equiv.drop-if-exists"));
}

#[test]
fn the_table_and_function_must_be_the_triggers() {
    let t = proc_t("DROP TRIGGER IF EXISTS Sales.TR_Orders;");
    // Another table.
    unknown(
        run(
            &t,
            &proc_p(&DROP_P.replace("ON Sales.Orders", "ON Sales.Lines")),
        ),
        "the T-SQL trigger is on",
    );
    // The function stays behind.
    unknown(
        run(
            &t,
            &proc_p("DROP TRIGGER IF EXISTS TR_Orders ON Sales.Orders;"),
        ),
        "does not drop the trigger's function",
    );
    // Another function.
    unknown(
        run(
            &t,
            &proc_p(&DROP_P.replace("Sales.TR_Orders()", "Sales.Other()")),
        ),
        "drops the function",
    );
    // Another trigger name.
    let r = run(
        &t,
        &proc_p(&DROP_P.replace("EXISTS TR_Orders", "EXISTS TR_Other")),
    );
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
}

#[test]
fn the_catalogs_must_know_the_trigger() {
    let t = proc_t("DROP TRIGGER IF EXISTS Sales.TR_Orders;");
    let p = proc_p(DROP_P);
    // No CREATE TRIGGER on the T-SQL side.
    unknown(
        run_with(&t, &p, TABLE_T, &format!("{TABLE_P}{TRIG_P}")),
        "no CREATE TRIGGER of it",
    );
    // Nor on the PostgreSQL side (its CREATE TRIGGER was not converted).
    unknown(
        run_with(&t, &p, &format!("{TABLE_T}{TRIG_T}"), TABLE_P),
        "no CREATE TRIGGER of it",
    );
    // Two different definitions of the name.
    let twice = format!(
        "{TABLE_T}{TRIG_T}{}",
        TRIG_T.replace("ON Sales.Orders", "ON Sales.Lines")
    );
    unknown(
        run_with(&t, &p, &twice, &format!("{TABLE_P}{TRIG_P}")),
        "more than once",
    );
    // Several events: one PostgreSQL trigger per event.
    let multi = TRIG_T.replace("AFTER INSERT", "AFTER INSERT, UPDATE");
    unknown(
        run_with(
            &t,
            &p,
            &format!("{TABLE_T}{multi}"),
            &format!("{TABLE_P}{TRIG_P}"),
        ),
        "several events",
    );
    // Another event on the PostgreSQL side.
    let upd = TRIG_P.replace("AFTER INSERT", "AFTER UPDATE");
    unknown(
        run_with(
            &t,
            &p,
            &format!("{TABLE_T}{TRIG_T}"),
            &format!("{TABLE_P}{upd}"),
        ),
        "fires on UPDATE",
    );
    // Another PostgreSQL trigger runs the same function.
    let shared = format!(
        "{TABLE_P}{TRIG_P}CREATE TRIGGER TR_Lines AFTER INSERT ON Sales.Lines FOR EACH STATEMENT EXECUTE FUNCTION Sales.TR_Orders();\n"
    );
    unknown(
        run_with(&t, &p, &format!("{TABLE_T}{TRIG_T}"), &shared),
        "another PostgreSQL trigger",
    );
}

#[test]
fn a_file_that_names_the_trigger_elsewhere_is_not_compared() {
    // The catalog's definition may not be the one in force where the DROP
    // runs when the file itself creates or names the trigger.
    unknown(
        run(
            "DROP TRIGGER Sales.TR_Orders;\nGO\nCREATE TABLE Sales.TR_Orders (x int);\n",
            "DROP TRIGGER TR_Orders ON Sales.Orders;\nDROP FUNCTION IF EXISTS Sales.TR_Orders();\nCREATE TABLE Sales.TR_Orders (x integer);\n",
        ),
        "elsewhere",
    );
}

#[test]
fn a_ddl_trigger_is_not_compared() {
    unknown(
        run(
            "DROP TRIGGER IF EXISTS TR_Orders ON DATABASE;\n",
            "DROP TRIGGER IF EXISTS TR_Orders ON Sales.Orders;\nDROP FUNCTION IF EXISTS Sales.TR_Orders();\n",
        ),
        "DDL trigger",
    );
}
