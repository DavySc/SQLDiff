//! System-versioned (temporal) tables. The PostgreSQL output was run on
//! PostgreSQL 18 against the same DML on SQL Server 2022 (2026-10-26): the
//! same history rows, and the same statements fail (Msg 13535, 13536, 13537,
//! 13545).

use sqldiff_convert::{check, convert, rewrite_in, Conversion, Status};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{Dialect, Options};

const MAIN: &str = "CREATE TABLE [Sales].[Groups] (
    [GroupID]   INT           NOT NULL,
    [Name]      NVARCHAR (50) NOT NULL,
    [ValidFrom] DATETIME2 (7) GENERATED ALWAYS AS ROW START NOT NULL,
    [ValidTo]   DATETIME2 (7) GENERATED ALWAYS AS ROW END   NOT NULL,
    CONSTRAINT [PK_Groups] PRIMARY KEY CLUSTERED ([GroupID] ASC),
    PERIOD FOR SYSTEM_TIME ([ValidFrom], [ValidTo])
)
WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE=[Sales].[Groups_Archive], DATA_CONSISTENCY_CHECK=ON));
";

const ARCHIVE: &str = "CREATE TABLE [Sales].[Groups_Archive] (
    [GroupID]   INT           NOT NULL,
    [Name]      NVARCHAR (50) NOT NULL,
    [ValidFrom] DATETIME2 (7) NOT NULL,
    [ValidTo]   DATETIME2 (7) NOT NULL
);
";

fn pg(text: &str) -> Conversion {
    convert(text, Dialect::Tsql, Dialect::Postgres).unwrap()
}

fn in_tree(text: &str, files: &[&str]) -> Conversion {
    let mut cat = Catalog::default();
    for f in files {
        let root = sqldiff_syntax::parse(f, Dialect::Tsql).syntax();
        ddl::load_tree(&mut cat, &root, Dialect::Tsql);
    }
    let d = rewrite_in(text, Dialect::Tsql, Dialect::Postgres, Some(&cat));
    check(text, d, &Options::default())
}

/// The rules' own verdict on a unit the engine did not compare: the unit is
/// not proven, and this is the worst of its other notes.
fn rules_only(u: &sqldiff_convert::Unit) -> Status {
    assert_eq!(u.status, Status::Unproven, "{:?}", u.notes);
    assert!(
        u.notes
            .iter()
            .any(|n| n.rule == sqldiff_convert::NOT_COMPARED),
        "{:?}",
        u.notes
    );
    u.notes
        .iter()
        .filter(|n| n.rule != sqldiff_convert::NOT_COMPARED)
        .map(|n| n.status)
        .max()
        .unwrap_or(Status::Converted)
}

fn rules(c: &Conversion) -> Vec<&str> {
    c.units[0].notes.iter().map(|n| n.rule.as_str()).collect()
}

// The engine does not compare a system-versioned table and its history
// triggers with the original yet: these units are not proven, and the tests
// check the rules' own verdict.

#[test]
fn a_system_versioned_table_gets_a_history_trigger() {
    let c = pg(MAIN);
    let o = &c.output;
    assert_eq!(c.units.len(), 1, "{o}");
    assert_eq!(
        rules_only(&c.units[0]),
        Status::Assumptions,
        "{:?}",
        c.units[0].notes
    );
    assert!(
        rules(&c).contains(&"ASM-TEMPORAL-TRIGGER"),
        "{:?}",
        rules(&c)
    );
    // Without the tree's catalog the history table is an assumption.
    assert!(
        rules(&c).contains(&"ASM-TEMPORAL-HISTORY"),
        "{:?}",
        rules(&c)
    );
    for want in [
        "ValidFrom timestamp DEFAULT '-infinity' NOT NULL,",
        "ValidTo timestamp DEFAULT '-infinity' NOT NULL,",
        "CONSTRAINT PK_Groups PRIMARY KEY (GroupID)\n);",
        "CREATE FUNCTION Sales.Groups_versioning() RETURNS trigger",
        "now_utc timestamp(6) := CAST(transaction_timestamp() AT TIME ZONE 'UTC' AS timestamp(6));",
        "IF OLD.ValidFrom > now_utc THEN",
        "INSERT INTO Sales.Groups_Archive (GroupID, Name, ValidFrom, ValidTo)\n        VALUES (OLD.GroupID, OLD.Name, OLD.ValidFrom, now_utc);",
        "NEW.ValidTo := '9999-12-31 23:59:59.999999';",
        "CREATE TRIGGER Groups_period_update BEFORE UPDATE OF ValidFrom, ValidTo ON Sales.Groups",
        "CREATE TRIGGER Groups_versioning BEFORE INSERT OR UPDATE OR DELETE ON Sales.Groups",
        "CREATE TRIGGER Groups_versioning_truncate BEFORE TRUNCATE ON Sales.Groups",
    ] {
        assert!(o.contains(want), "{want}\n{o}");
    }
    assert!(
        !o.contains("SYSTEM_VERSIONING") && !o.contains("PERIOD"),
        "{o}"
    );
}

#[test]
fn the_end_of_time_follows_the_precision() {
    let src = "CREATE TABLE s.t (k INT NOT NULL PRIMARY KEY, f DATETIME2(0) GENERATED ALWAYS AS ROW START, e DATETIME2(0) GENERATED ALWAYS AS ROW END, PERIOD FOR SYSTEM_TIME (f, e)) WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE = s.h));\n";
    let o = pg(src).output;
    // SQL Server makes period columns NOT NULL when nothing is said.
    assert!(
        o.contains("f timestamp(0) NOT NULL DEFAULT '-infinity'"),
        "{o}"
    );
    assert!(o.contains("NEW.e := '9999-12-31 23:59:59';"), "{o}");
    assert!(o.contains("AS timestamp(0));"), "{o}");
}

#[test]
fn the_history_table_is_checked_against_the_tree() {
    // Same columns, same order: nothing to assume.
    let c = in_tree(MAIN, &[MAIN, ARCHIVE]);
    assert_eq!(
        rules_only(&c.units[0]),
        Status::Assumptions,
        "{:?}",
        c.units[0].notes
    );
    assert!(
        !rules(&c).contains(&"ASM-TEMPORAL-HISTORY"),
        "{:?}",
        rules(&c)
    );
    // Another column order: SQL Server refuses it (Msg 13524).
    let swapped = ARCHIVE.replace(
        "[Name]      NVARCHAR (50) NOT NULL,\n    [ValidFrom]",
        "[ValidFrom] DATETIME2 (7) NOT NULL,\n    [Name]     ",
    );
    let swapped = swapped.replacen(
        "[Name]      DATETIME2 (7) NOT NULL",
        "[Name] NVARCHAR (50) NOT NULL",
        1,
    );
    let c = in_tree(MAIN, &[MAIN, &swapped]);
    assert_eq!(
        c.units[0].status,
        Status::NotConverted,
        "{swapped}\n{}",
        c.output
    );
    assert!(
        c.units[0].notes[0].text.contains("13524"),
        "{:?}",
        c.units[0].notes
    );
    // A missing column: Msg 13523.
    let short = ARCHIVE.replace("    [Name]      NVARCHAR (50) NOT NULL,\n", "");
    let c = in_tree(MAIN, &[MAIN, &short]);
    assert!(
        c.units[0].notes[0].text.contains("13523"),
        "{:?}",
        c.units[0].notes
    );
    // No history table in the tree: SQL Server creates it, so does the output.
    let c = in_tree(MAIN, &[MAIN]);
    assert!(
        c.output.starts_with("CREATE TABLE Sales.Groups ("),
        "{}",
        c.output
    );
    assert!(
        c.output.contains(
            ");\n\nCREATE TABLE Sales.Groups_Archive (LIKE Sales.Groups);\n\nCREATE FUNCTION"
        ),
        "{}",
        c.output
    );
    assert!(
        c.units[0].status < Status::Differs,
        "{:?}",
        c.units[0].notes
    );
}

#[test]
fn what_has_no_exact_equivalent_is_refused() {
    let base = |cols: &str, with: &str| {
        format!("CREATE TABLE s.t (k INT NOT NULL PRIMARY KEY, {cols}, PERIOD FOR SYSTEM_TIME (f, e)) WITH ({with});\n")
    };
    let cols = "f DATETIME2 GENERATED ALWAYS AS ROW START NOT NULL, e DATETIME2 GENERATED ALWAYS AS ROW END NOT NULL";
    let on = "SYSTEM_VERSIONING = ON (HISTORY_TABLE = s.h)";
    for (src, why) in [
        (base(cols, "SYSTEM_VERSIONING = ON"), "generated name"),
        (
            base(
                cols,
                "SYSTEM_VERSIONING = ON (HISTORY_TABLE = s.h, HISTORY_RETENTION_PERIOD = 6 MONTHS)",
            ),
            "HISTORY_RETENTION_PERIOD",
        ),
        (base(cols, "SYSTEM_VERSIONING = OFF"), "without history"),
        (
            base(&cols.replace("ROW START", "ROW START HIDDEN"), on),
            "HIDDEN",
        ),
        (
            base(&cols.replace("ROW START NOT NULL", "ROW START NULL"), on),
            "13587",
        ),
        (
            base(cols, &format!("MEMORY_OPTIMIZED = ON, {on}")),
            "MEMORY_OPTIMIZED",
        ),
    ] {
        let c = pg(&src);
        assert_eq!(
            c.units[0].status,
            Status::NotConverted,
            "{src}\n{}",
            c.output
        );
        assert!(
            c.units[0].notes[0].text.contains(why),
            "{why}: {:?}",
            c.units[0].notes
        );
    }
    // HISTORY_RETENTION_PERIOD = INFINITE deletes nothing.
    let c = pg(&base(
        cols,
        "SYSTEM_VERSIONING = ON (HISTORY_TABLE = s.h, HISTORY_RETENTION_PERIOD = INFINITE)",
    ));
    assert_eq!(
        rules_only(&c.units[0]),
        Status::Assumptions,
        "{:?}",
        c.units[0].notes
    );
}

#[test]
fn db2_for_i_refuses_until_the_facts_are_verified() {
    let c = convert(MAIN, Dialect::Tsql, Dialect::Db2i).unwrap();
    assert_eq!(c.units[0].status, Status::NotConverted, "{}", c.output);
    assert!(
        c.units[0].notes[0].text.contains("not verified on IBM i"),
        "{:?}",
        c.units[0].notes
    );
}

#[test]
fn key_columns_lose_their_sort_order() {
    let src = "CREATE TABLE s.t (a INT NOT NULL, b INT NOT NULL, CONSTRAINT pk PRIMARY KEY CLUSTERED (a ASC, b DESC), CONSTRAINT uq UNIQUE NONCLUSTERED (b ASC));\n";
    for to in [Dialect::Postgres, Dialect::Db2i] {
        let c = convert(src, Dialect::Tsql, to).unwrap();
        assert!(
            c.output.contains("CONSTRAINT pk PRIMARY KEY (a, b)"),
            "{}",
            c.output
        );
        assert!(
            c.output.contains("CONSTRAINT uq UNIQUE (b)"),
            "{}",
            c.output
        );
        assert!(rules(&c).contains(&"tsql.key-order"), "{:?}", rules(&c));
    }
}

const SWITCHES: &str = "ALTER TABLE [Sales].[Groups] SET (SYSTEM_VERSIONING = OFF);
ALTER TABLE [Sales].[Groups] DROP PERIOD FOR SYSTEM_TIME;
ALTER TABLE [Sales].[Groups] ADD PERIOD FOR SYSTEM_TIME([ValidFrom], [ValidTo]);
ALTER TABLE [Sales].[Groups] SET (SYSTEM_VERSIONING = ON (HISTORY_TABLE = [Sales].[Groups_Archive], DATA_CONSISTENCY_CHECK = ON));
";

#[test]
fn switching_versioning_switches_the_history_triggers() {
    let c = in_tree(SWITCHES, &[MAIN, ARCHIVE]);
    let o = &c.output;
    assert_eq!(c.units.len(), 4, "{o}");
    for u in &c.units {
        assert_eq!(rules_only(u), Status::Assumptions, "{:?}", u.notes);
        assert!(
            u.notes.iter().any(|n| n.rule == "ASM-TEMPORAL-SWITCH"),
            "{:?}",
            u.notes
        );
    }
    for want in [
        "ALTER TABLE Sales.Groups DISABLE TRIGGER Groups_versioning, DISABLE TRIGGER Groups_versioning_truncate;",
        "ALTER TABLE Sales.Groups DISABLE TRIGGER Groups_period_update;",
        "ALTER TABLE Sales.Groups ENABLE TRIGGER Groups_period_update;",
        "ALTER TABLE Sales.Groups ENABLE TRIGGER Groups_versioning, ENABLE TRIGGER Groups_versioning_truncate;",
    ] {
        assert!(o.contains(want), "{want}\n{o}");
    }
}

#[test]
fn switching_versioning_needs_the_tables_definition() {
    // Without the tree's catalog, or for a table it does not show
    // system-versioned, which triggers the table has is not known.
    for c in [pg(SWITCHES), in_tree(SWITCHES, &[ARCHIVE])] {
        assert_eq!(c.counts.not_converted, 4, "{}", c.output);
    }
    // Db2 for i refuses system versioning altogether.
    let c = convert(SWITCHES, Dialect::Tsql, Dialect::Db2i).unwrap();
    assert_eq!(c.counts.not_converted, 4, "{}", c.output);
    for (src, why) in [
        (
            "ALTER TABLE Sales.Groups ADD PERIOD FOR SYSTEM_TIME (ValidTo, ValidFrom);\n",
            "the CREATE TABLE's period",
        ),
        (
            "ALTER TABLE Sales.Groups SET (SYSTEM_VERSIONING = ON (HISTORY_TABLE = Sales.Other));\n",
            "trigger writes the history table",
        ),
        (
            "ALTER TABLE Sales.Groups SET (SYSTEM_VERSIONING = ON);\n",
            "generated name",
        ),
        (
            "ALTER TABLE Sales.Groups SET (SYSTEM_VERSIONING = ON (HISTORY_TABLE = Sales.Groups_Archive, HISTORY_RETENTION_PERIOD = 6 MONTHS));\n",
            "HISTORY_RETENTION_PERIOD",
        ),
        (
            "ALTER TABLE Sales.Groups SET (SYSTEM_VERSIONING = OFF, DATA_DELETION = ON);\n",
            "only SET (SYSTEM_VERSIONING = OFF)",
        ),
    ] {
        let c = in_tree(src, &[MAIN, ARCHIVE]);
        assert_eq!(c.units[0].status, Status::NotConverted, "{src}: {}", c.output);
        assert!(
            c.units[0].notes.iter().any(|n| n.text.contains(why)),
            "{src}: {:?}",
            c.units[0].notes
        );
    }
}

/// Compared with a value, T-SQL's end of time is written with PostgreSQL's
/// 6 digits under ASM-END-OF-TIME (Davy, 2026-10-29), in any spelling T-SQL
/// reads; elsewhere a 7-digit end of time still differs.
#[test]
fn the_end_of_time_literal_differs_on_postgresql() {
    for lit in ["9999-12-31 23:59:59.9999999", "99991231 23:59:59.9999999"] {
        let src = format!("SELECT GroupID FROM Sales.Groups WHERE ValidTo = '{lit}';\n");
        let c = in_tree(&src, &[MAIN, ARCHIVE]);
        assert!(
            c.output.contains("ValidTo = '9999-12-31 23:59:59.999999'"),
            "{}",
            c.output
        );
        // The engine's comparison needs the catalogs (tests/user_functions.rs).
        assert!(
            c.units[0].status < Status::Differs,
            "{:?}",
            c.units[0].notes
        );
        assert!(rules(&c).contains(&"ASM-END-OF-TIME"), "{:?}", rules(&c));
    }
    let src = "SELECT GroupID, '9999-12-31 23:59:59.9999999' AS e FROM Sales.Groups;\n";
    let c = pg(src);
    assert_eq!(c.units[0].status, Status::Differs, "{:?}", c.units[0].notes);
    assert!(
        rules(&c).contains(&"tsql.end-of-time-literal"),
        "{:?}",
        rules(&c)
    );
    // Six digits are PostgreSQL's own end of time, and Db2 keeps seven.
    let six = src.replace(".9999999", ".999999");
    assert!(!rules(&pg(&six)).contains(&"tsql.end-of-time-literal"));
    let c = convert(src, Dialect::Tsql, Dialect::Db2i).unwrap();
    assert!(
        !rules(&c).contains(&"tsql.end-of-time-literal"),
        "{:?}",
        rules(&c)
    );
}

#[test]
fn temporal_queries_are_refused() {
    let src =
        "SELECT GroupID FROM Sales.Groups FOR SYSTEM_TIME AS OF '2020-01-01' WHERE GroupID = 1;\n";
    for to in [Dialect::Postgres, Dialect::Db2i] {
        let c = convert(src, Dialect::Tsql, to).unwrap();
        assert_eq!(c.units[0].status, Status::NotConverted, "{}", c.output);
        assert!(
            c.units[0].notes[0].text.contains("FOR SYSTEM_TIME"),
            "{:?}",
            c.units[0].notes
        );
    }
}
