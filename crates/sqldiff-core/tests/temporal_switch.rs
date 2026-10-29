//! System-versioned tables while a data load writes them: T-SQL turning
//! versioning off and on and dropping and adding the period against the
//! PostgreSQL triggers the converted CREATE TABLE made, and writes to the
//! period columns while the period is dropped.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;

const CAT_T: &str = "CREATE TABLE Sales.Groups (
    GroupID int NOT NULL CONSTRAINT PK_Groups PRIMARY KEY,
    Name nvarchar(50) NOT NULL,
    ValidFrom datetime2(6) GENERATED ALWAYS AS ROW START NOT NULL,
    ValidTo datetime2(6) GENERATED ALWAYS AS ROW END NOT NULL,
    PERIOD FOR SYSTEM_TIME (ValidFrom, ValidTo)
)
WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE = Sales.Groups_Archive, DATA_CONSISTENCY_CHECK = ON));
GO
CREATE TABLE Sales.Groups_Archive (
    GroupID int NOT NULL,
    Name nvarchar(50) NOT NULL,
    ValidFrom datetime2(6) NOT NULL,
    ValidTo datetime2(6) NOT NULL
);
GO
CREATE TABLE Sales.Plain (
    Id int NOT NULL CONSTRAINT PK_Plain PRIMARY KEY,
    Total AS Id * 2
);
";

const CAT_P: &str = "CREATE TABLE Sales.Groups (
    GroupID integer NOT NULL CONSTRAINT PK_Groups PRIMARY KEY,
    Name varchar(50) NOT NULL,
    ValidFrom timestamp(6) NOT NULL DEFAULT '-infinity',
    ValidTo timestamp(6) NOT NULL DEFAULT '-infinity'
);
CREATE TABLE Sales.Groups_Archive (
    GroupID integer NOT NULL,
    Name varchar(50) NOT NULL,
    ValidFrom timestamp(6) NOT NULL,
    ValidTo timestamp(6) NOT NULL
);
CREATE TABLE Sales.Plain (
    Id integer NOT NULL CONSTRAINT PK_Plain PRIMARY KEY,
    Total integer GENERATED ALWAYS AS (Id * 2) STORED
);
";

fn run(a: &str, b: &str) -> Report {
    let o = Options {
        catalog_a: vec![Artifact::new("cat.sql", T, CAT_T)],
        catalog_b: vec![Artifact::new("cat.pg.sql", P, CAT_P)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", T, a),
        &Artifact::new("b.sql", P, b),
        &o,
    )
}

fn proc_t(body: &str) -> String {
    format!("CREATE PROCEDURE dbo.Load\nAS\nBEGIN\n    SET XACT_ABORT ON;\n{body}\nEND;")
}

fn proc_p(body: &str) -> String {
    format!("CREATE OR REPLACE PROCEDURE Load()\nLANGUAGE plpgsql\nAS $$\nBEGIN\n{body}\nEND;\n$$;")
}

fn asms(r: &Report) -> Vec<&str> {
    r.assumptions.iter().map(|a| a.id.as_str()).collect()
}

fn equal(a: &str, b: &str) -> Report {
    let r = run(&proc_t(a), &proc_p(b));
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:?} {:#?}",
        r.unknown_reasons,
        r.findings
    );
    r
}

fn unknown(a: &str, b: &str, needle: &str) {
    let r = run(&proc_t(a), &proc_p(b));
    assert!(
        r.unknown_reasons.iter().any(|x| x.contains(needle)),
        "{needle}: {:?}",
        r.unknown_reasons
    );
    assert_ne!(r.verdict(), Verdict::Equal);
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
}

const OFF_T: &str = "    ALTER TABLE Sales.Groups SET (SYSTEM_VERSIONING = OFF);
    ALTER TABLE Sales.Groups DROP PERIOD FOR SYSTEM_TIME;";
const OFF_P: &str = "    ALTER TABLE Sales.Groups DISABLE TRIGGER Groups_versioning, DISABLE TRIGGER Groups_versioning_truncate;
    ALTER TABLE Sales.Groups DISABLE TRIGGER Groups_period_update;";
const ON_T: &str = "    ALTER TABLE Sales.Groups ADD PERIOD FOR SYSTEM_TIME (ValidFrom, ValidTo);
    ALTER TABLE Sales.Groups SET (SYSTEM_VERSIONING = ON (HISTORY_TABLE = Sales.Groups_Archive, DATA_CONSISTENCY_CHECK = ON));";
const ON_P: &str = "    ALTER TABLE Sales.Groups ENABLE TRIGGER Groups_period_update;
    ALTER TABLE Sales.Groups ENABLE TRIGGER Groups_versioning, ENABLE TRIGGER Groups_versioning_truncate;";

#[test]
fn switching_versioning_matches_switching_the_triggers() {
    for (a, b) in [(OFF_T, OFF_P), (ON_T, ON_P)] {
        let r = equal(a, b);
        assert!(asms(&r).contains(&"ASM-TEMPORAL-SWITCH"), "{:?}", asms(&r));
    }
}

#[test]
fn other_triggers_or_another_period_or_history_are_not_compared() {
    // Off against enabling.
    unknown(
        OFF_T,
        &OFF_P.replace("DISABLE", "ENABLE"),
        "switches other triggers",
    );
    // Only one of the two versioning triggers.
    unknown(
        "    ALTER TABLE Sales.Groups SET (SYSTEM_VERSIONING = OFF);",
        "    ALTER TABLE Sales.Groups DISABLE TRIGGER Groups_versioning;",
        "switches other triggers",
    );
    unknown(
        "    ALTER TABLE Sales.Groups ADD PERIOD FOR SYSTEM_TIME (ValidTo, ValidFrom);",
        "    ALTER TABLE Sales.Groups ENABLE TRIGGER Groups_period_update;",
        "is not the one",
    );
    unknown(
        "    ALTER TABLE Sales.Groups SET (SYSTEM_VERSIONING = ON (HISTORY_TABLE = Sales.Other));",
        "    ALTER TABLE Sales.Groups ENABLE TRIGGER Groups_versioning, ENABLE TRIGGER Groups_versioning_truncate;",
        "is not the history table",
    );
    // A table the catalog does not show system-versioned.
    unknown(
        "    ALTER TABLE Sales.Plain SET (SYSTEM_VERSIONING = OFF);",
        "    ALTER TABLE Sales.Plain DISABLE TRIGGER Plain_versioning, DISABLE TRIGGER Plain_versioning_truncate;",
        "does not show",
    );
    // ALL switches every trigger, foreign keys' too.
    unknown(
        "    ALTER TABLE Sales.Groups DROP PERIOD FOR SYSTEM_TIME;",
        "    ALTER TABLE Sales.Groups DISABLE TRIGGER ALL;",
        "ALL",
    );
}

#[test]
fn a_period_column_write_is_compared_under_an_assumption() {
    for (a, b) in [
        (
            "INSERT Sales.Groups (GroupID, Name, ValidFrom, ValidTo) VALUES (1, N'a', '2020-01-01', '9999-12-31 23:59:59.999999');",
            "INSERT INTO Sales.Groups (GroupID, Name, ValidFrom, ValidTo) VALUES (1, 'a', '2020-01-01', '9999-12-31 23:59:59.999999');",
        ),
        (
            "UPDATE Sales.Groups SET ValidFrom = '2020-01-02' WHERE GroupID = 1;",
            "UPDATE Sales.Groups SET ValidFrom = '2020-01-02' WHERE GroupID = 1;",
        ),
    ] {
        let r = equal(&format!("{OFF_T}\n    {a}"), &format!("{OFF_P}\n    {b}"));
        assert!(
            asms(&r).contains(&"ASM-TEMPORAL-PERIOD-WRITE"),
            "{:?}",
            asms(&r)
        );
    }
}

#[test]
fn a_computed_column_write_stays_refused() {
    let r = run(
        &proc_t("    UPDATE Sales.Plain SET Total = 3 WHERE Id = 1;"),
        &proc_p("    UPDATE Sales.Plain SET Total = 3 WHERE Id = 1;"),
    );
    assert!(
        r.findings.iter().any(|f| f.rule == "equiv.unsupported"
            && f.witness.contains("writes the generated column total")),
        "{:#?}",
        r.findings
    );
    assert!(!asms(&r).contains(&"ASM-TEMPORAL-PERIOD-WRITE"));
}
