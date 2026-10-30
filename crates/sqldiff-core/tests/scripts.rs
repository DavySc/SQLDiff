//! Scripts compared across dialects: a T-SQL batch against the DO block it
//! became, plain statements against plain statements, and the declared types
//! of variables. Each rule has an equal pair and a near miss.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const T: Dialect = Dialect::Tsql;
const P: Dialect = Dialect::Postgres;
const D: Dialect = Dialect::Db2i;

const CATALOG_T: &str = "CREATE TABLE dbo.Methods (MethodID int NOT NULL PRIMARY KEY, MethodName nvarchar(50) NOT NULL, ValidFrom datetime2(7) NULL);
CREATE TABLE dbo.Hist (ID int NOT NULL PRIMARY KEY, Name nvarchar(50) NOT NULL,
  ValidFrom datetime2(7) GENERATED ALWAYS AS ROW START NOT NULL,
  ValidTo datetime2(7) GENERATED ALWAYS AS ROW END NOT NULL,
  PERIOD FOR SYSTEM_TIME (ValidFrom, ValidTo));";
const CATALOG_P: &str = "CREATE TABLE Methods (MethodID integer NOT NULL PRIMARY KEY, MethodName varchar(50) NOT NULL, ValidFrom timestamp NULL);
CREATE TABLE Hist (ID integer NOT NULL PRIMARY KEY, Name varchar(50) NOT NULL, ValidFrom timestamp NOT NULL, ValidTo timestamp NOT NULL);";

fn run_in(da: Dialect, db: Dialect, a: &str, b: &str) -> Report {
    let opts = Options {
        catalog_a: vec![Artifact::new("ca.sql", da, CATALOG_T)],
        catalog_b: vec![Artifact::new("cb.sql", db, CATALOG_P)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", da, a),
        &Artifact::new("b.sql", db, b),
        &opts,
    )
}

fn run(a: &str, b: &str) -> Report {
    run_in(T, P, a, b)
}

fn rules(r: &Report) -> Vec<&str> {
    r.findings.iter().map(|f| f.rule.as_str()).collect()
}

/// Equal under assumptions, nothing undecided.
fn equal(a: &str, b: &str) -> Report {
    let r = run(a, b);
    assert_eq!(
        r.verdict(),
        Verdict::EqualUnderAssumptions,
        "{:#?}\n{:#?}",
        r.findings,
        r.unknown_reasons
    );
    assert!(r.unknown_reasons.is_empty(), "{:?}", r.unknown_reasons);
    r
}

/// Not equal, and a reason or finding mentions `needle`.
fn not_equal(r: Report, needle: &str) {
    assert!(
        r.unknown_reasons.iter().any(|x| x.contains(needle))
            || r.findings
                .iter()
                .any(|f| f.witness.contains(needle) || f.rule.contains(needle)),
        "{needle}: {:?} {:?}",
        r.unknown_reasons,
        rules(&r)
    );
    assert_ne!(r.verdict(), Verdict::Equal);
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
}

const SCRIPT_T: &str = "PRINT 'Inserting Methods'
GO

DECLARE @CurrentDateTime datetime2(7) = '20200101';

INSERT [dbo].Methods (MethodID, MethodName, ValidFrom)
VALUES (1, 'Cash', @CurrentDateTime), (2, 'Check', @CurrentDateTime);
GO
";

const SCRIPT_P: &str = "DO $$
BEGIN
  RAISE NOTICE '%', 'Inserting Methods';
END
$$;

DO $$
DECLARE
  v_current_date_time timestamp;
BEGIN
  v_current_date_time := '20200101';
  INSERT INTO Methods (MethodID, MethodName, ValidFrom)
  VALUES (1, 'Cash', v_current_date_time), (2, 'Check', v_current_date_time);
END
$$;
";

#[test]
fn batches_against_do_blocks() {
    let r = equal(SCRIPT_T, SCRIPT_P);
    assert!(r.findings.is_empty(), "{:?}", rules(&r));
}

#[test]
fn a_changed_row_is_not_equal() {
    not_equal(
        run(SCRIPT_T, &SCRIPT_P.replace("'Check'", "'Card'")),
        "Check",
    );
    not_equal(
        run(
            SCRIPT_T,
            &SCRIPT_P.replace(
                "(2, 'Check', v_current_date_time)",
                "(2, 'Check', v_current_date_time), (3, 'EFT', v_current_date_time)",
            ),
        ),
        "",
    );
}

#[test]
fn a_changed_message_is_not_equal() {
    not_equal(
        run(
            SCRIPT_T,
            &SCRIPT_P.replace("'Inserting Methods'", "'Inserted Methods'"),
        ),
        "",
    );
    // NULL prints as an empty line in T-SQL and as <NULL> in PostgreSQL.
    not_equal(
        run(
            "DECLARE @m varchar(10) = NULL;\nPRINT @m;",
            "DO $$\nDECLARE v_m varchar(10);\nBEGIN\n  v_m := NULL;\n  RAISE NOTICE '%', v_m;\nEND\n$$;",
        ),
        "<NULL>",
    );
}

#[test]
fn blocks_must_line_up() {
    // Two batches against one block: not compared batch by batch.
    let one_block = "DO $$
DECLARE
  v_current_date_time timestamp;
BEGIN
  RAISE NOTICE '%', 'Inserting Methods';
  v_current_date_time := '20200101';
  INSERT INTO Methods (MethodID, MethodName, ValidFrom)
  VALUES (1, 'Cash', v_current_date_time), (2, 'Check', v_current_date_time);
END
$$;";
    let r = run(SCRIPT_T, one_block);
    assert_ne!(r.verdict(), Verdict::Equal);
    assert_ne!(r.verdict(), Verdict::EqualUnderAssumptions);
}

#[test]
fn a_rounded_variable_is_not_compared() {
    // timestamp keeps 6 digits: PostgreSQL rounds this to 10000-01-01 00:00:00.
    not_equal(
        run(
            "DECLARE @EndOfTime datetime2(7) = '99991231 23:59:59.9999999';\nINSERT dbo.Methods (MethodID, MethodName, ValidFrom) VALUES (1, 'x', @EndOfTime);",
            "DO $$\nDECLARE\n  v_end_of_time timestamp;\nBEGIN\n  v_end_of_time := '99991231 23:59:59.9999999';\n  INSERT INTO Methods (MethodID, MethodName, ValidFrom) VALUES (1, 'x', v_end_of_time);\nEND\n$$;",
        ),
        "store values differently",
    );
    // Assigned from something other than a literal: the type decides.
    not_equal(
        run(
            "DECLARE @d datetime2(7);\nSELECT @d = ValidFrom FROM dbo.Methods WHERE MethodID = 1;\nINSERT dbo.Methods (MethodID, MethodName, ValidFrom) VALUES (2, 'x', @d);",
            "DO $$\nDECLARE\n  v_d timestamp;\nBEGIN\n  SELECT ValidFrom INTO v_d FROM Methods WHERE MethodID = 1;\n  INSERT INTO Methods (MethodID, MethodName, ValidFrom) VALUES (2, 'x', v_d);\nEND\n$$;",
        ),
        "store values differently",
    );
}

#[test]
fn variable_types_that_hold_the_same_values() {
    equal(
        "DECLARE @n int = 7;\nDECLARE @d datetime2(6) = '2020-01-01 10:00:00.5';\nINSERT dbo.Methods (MethodID, MethodName, ValidFrom) VALUES (@n, 'x', @d);",
        "DO $$\nDECLARE\n  v_n integer;\n  v_d timestamp(6);\nBEGIN\n  v_n := 7;\n  v_d := '2020-01-01 10:00:00.5';\n  INSERT INTO Methods (MethodID, MethodName, ValidFrom) VALUES (v_n, 'x', v_d);\nEND\n$$;",
    );
    // A string variable: T-SQL cuts a longer value off, PostgreSQL raises.
    let r = run(
        "DECLARE @s varchar(10);\nSELECT @s = MethodName FROM dbo.Methods WHERE MethodID = 1;\nINSERT dbo.Methods (MethodID, MethodName, ValidFrom) VALUES (2, @s, NULL);",
        "DO $$\nDECLARE\n  v_s varchar(10);\nBEGIN\n  SELECT MethodName INTO v_s FROM Methods WHERE MethodID = 1;\n  INSERT INTO Methods (MethodID, MethodName, ValidFrom) VALUES (2, v_s, NULL);\nEND\n$$;",
    );
    assert!(
        r.assumptions.iter().any(|a| a.id == "ASM-VARIABLE-LENGTH"),
        "{:?}",
        r.assumptions
    );
}

#[test]
fn literals_assigned_to_same_types() {
    // Checked on SQL Server 2022 and PostgreSQL 18: decimal to int truncates
    // in T-SQL (1) and rounds in PostgreSQL (2).
    for (t, p) in [
        ("DECLARE @x int = 1.5;", "v_x := 1.5;"),
        ("DECLARE @x int;\nSET @x = 2.5;", "v_x := 2.5;"),
    ] {
        not_equal(
            run(
                &format!("SET XACT_ABORT ON;\n{t}\nINSERT dbo.Methods (MethodID, MethodName, ValidFrom) VALUES (@x, 'a', NULL);"),
                &format!("DO $$\nDECLARE\n  v_x integer;\nBEGIN\n  {p}\n  INSERT INTO Methods (MethodID, MethodName, ValidFrom) VALUES (v_x, 'a', NULL);\nEND\n$$;"),
            ),
            "cut off, rounded or rejected",
        );
    }
    // T-SQL stores 'abc', PostgreSQL raises "value too long".
    not_equal(
        run(
            "SET XACT_ABORT ON;\nDECLARE @s varchar(3) = 'abcdef';\nINSERT dbo.Methods (MethodID, MethodName, ValidFrom) VALUES (1, @s, NULL);",
            "DO $$\nDECLARE\n  v_s varchar(3);\nBEGIN\n  v_s := 'abcdef';\n  INSERT INTO Methods (MethodID, MethodName, ValidFrom) VALUES (1, v_s, NULL);\nEND\n$$;",
        ),
        "cut off, rounded or rejected",
    );
    // Also when the variable is written by a query too.
    not_equal(
        run(
            "SET XACT_ABORT ON;\nDECLARE @s varchar(3) = 'abcdef';\nSELECT @s = MethodName FROM dbo.Methods WHERE MethodID = 1;\nINSERT dbo.Methods (MethodID, MethodName, ValidFrom) VALUES (1, @s, NULL);",
            "DO $$\nDECLARE\n  v_s varchar(3);\nBEGIN\n  v_s := 'abcdef';\n  SELECT MethodName INTO v_s FROM Methods WHERE MethodID = 1;\n  INSERT INTO Methods (MethodID, MethodName, ValidFrom) VALUES (1, v_s, NULL);\nEND\n$$;",
        ),
        "cut off, rounded or rejected",
    );
    // Literals both types hold unchanged.
    equal(
        "SET XACT_ABORT ON;\nDECLARE @x int = 2;\nDECLARE @s varchar(3) = 'abc';\nINSERT dbo.Methods (MethodID, MethodName, ValidFrom) VALUES (@x, @s, NULL);",
        "DO $$\nDECLARE\n  v_x integer;\n  v_s varchar(3);\nBEGIN\n  v_x := 2;\n  v_s := 'abc';\n  INSERT INTO Methods (MethodID, MethodName, ValidFrom) VALUES (v_x, v_s, NULL);\nEND\n$$;",
    );
}

#[test]
fn strings_assigned_to_shorter_variables() {
    // 15 + 50 characters into nvarchar(50): SQL Server cuts a longer value
    // off, PostgreSQL raises; the same under ASM-VARIABLE-LENGTH.
    let r = equal(
        "SET XACT_ABORT ON;\nDECLARE @city nvarchar(50);\nDECLARE @bank nvarchar(50);\nSET @city = (SELECT MethodName FROM dbo.Methods WHERE MethodID = 1);\nSET @bank = 'Woodgrove Bank ' + @city;\nINSERT dbo.Methods (MethodID, MethodName, ValidFrom) VALUES (2, @bank, NULL);",
        "DO $$\nDECLARE\n  v_city varchar(50);\n  v_bank varchar(50);\nBEGIN\n  v_city := (SELECT MethodName FROM Methods WHERE MethodID = 1);\n  v_bank := 'Woodgrove Bank ' || v_city;\n  INSERT INTO Methods (MethodID, MethodName, ValidFrom) VALUES (2, v_bank, NULL);\nEND\n$$;",
    );
    assert!(
        r.assumptions.iter().any(|a| a.id == "ASM-VARIABLE-LENGTH"),
        "{:?}",
        r.assumptions
    );
    // Its literal alone does not fit: always cut off in T-SQL.
    let r = run(
        "SET XACT_ABORT ON;\nDECLARE @city nvarchar(50);\nDECLARE @bank nvarchar(10);\nSET @city = (SELECT MethodName FROM dbo.Methods WHERE MethodID = 1);\nSET @bank = 'Woodgrove Bank ' + @city;\nINSERT dbo.Methods (MethodID, MethodName, ValidFrom) VALUES (2, @bank, NULL);",
        "DO $$\nDECLARE\n  v_city varchar(50);\n  v_bank varchar(10);\nBEGIN\n  v_city := (SELECT MethodName FROM Methods WHERE MethodID = 1);\n  v_bank := 'Woodgrove Bank ' || v_city;\n  INSERT INTO Methods (MethodID, MethodName, ValidFrom) VALUES (2, v_bank, NULL);\nEND\n$$;",
    );
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.verdict()
    );
}

#[test]
fn a_uuid_copied_to_a_uuid_variable() {
    equal(
        "SET XACT_ABORT ON;\nDECLARE @a uniqueidentifier;\nDECLARE @b uniqueidentifier;\nSET @b = @a;\nINSERT dbo.Methods (MethodID, MethodName, ValidFrom) VALUES (1, CASE WHEN @b = @a THEN 'same' ELSE 'other' END, NULL);",
        "DO $$\nDECLARE\n  v_a uuid;\n  v_b uuid;\nBEGIN\n  v_b := v_a;\n  INSERT INTO Methods (MethodID, MethodName, ValidFrom) VALUES (1, CASE WHEN v_b = v_a THEN 'same' ELSE 'other' END, NULL);\nEND\n$$;",
    );
    // As text: upper case in T-SQL, lower case in PostgreSQL.
    let r = run(
        "SET XACT_ABORT ON;\nDECLARE @a uniqueidentifier;\nDECLARE @s varchar(36);\nSET @s = @a;\nINSERT dbo.Methods (MethodID, MethodName, ValidFrom) VALUES (1, @s, NULL);",
        "DO $$\nDECLARE\n  v_a uuid;\n  v_s varchar(36);\nBEGIN\n  v_s := v_a;\n  INSERT INTO Methods (MethodID, MethodName, ValidFrom) VALUES (1, v_s, NULL);\nEND\n$$;",
    );
    assert!(
        !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions),
        "{:?}",
        r.verdict()
    );
}

#[test]
fn variables_match_by_name_when_a_declaration_is_dropped() {
    equal(
        "DECLARE @Unused int;\nDECLARE @Id int = 1;\nINSERT dbo.Methods (MethodID, MethodName, ValidFrom) VALUES (@Id, 'x', NULL);",
        "DO $$\nDECLARE\n  v_id integer;\nBEGIN\n  v_id := 1;\n  INSERT INTO Methods (MethodID, MethodName, ValidFrom) VALUES (v_id, 'x', NULL);\nEND\n$$;",
    );
}

#[test]
fn binary_constants_and_plus() {
    equal(
        "DECLARE @b varbinary(max) = 0xAB01;\nPRINT 'x';",
        "DO $$\nDECLARE\n  v_b bytea;\nBEGIN\n  v_b := '\\xAB01'::bytea;\n  RAISE NOTICE '%', 'x';\nEND\n$$;",
    );
    // T-SQL + concatenates binary strings; PostgreSQL has no bytea +.
    not_equal(
        run(
            "DECLARE @b varbinary(max) = 0xAB01;\nSET @b += 0xCD;\nPRINT 'x';",
            "DO $$\nDECLARE\n  v_b bytea;\nBEGIN\n  v_b := '\\xAB01'::bytea;\n  v_b := v_b + '\\xCD'::bytea;\n  RAISE NOTICE '%', 'x';\nEND\n$$;",
        ),
        "",
    );
}

#[test]
fn writes_to_period_columns_are_not_compared() {
    not_equal(
        run(
            "DECLARE @t datetime2(7) = '20200101';\nINSERT dbo.Hist (ID, Name, ValidFrom, ValidTo) VALUES (1, 'x', @t, @t);",
            "DO $$\nDECLARE\n  v_t timestamp;\nBEGIN\n  v_t := '20200101';\n  INSERT INTO Hist (ID, Name, ValidFrom, ValidTo) VALUES (1, 'x', v_t, v_t);\nEND\n$$;",
        ),
        "generated column validfrom",
    );
}

#[test]
fn error_continuation() {
    // In a block, T-SQL runs the second INSERT after the first fails; the
    // DO block stops (and undoes the first).
    let r = run(
        "DECLARE @x int = 1;\nINSERT dbo.Methods (MethodID, MethodName, ValidFrom) VALUES (@x, 'a', NULL);\nINSERT dbo.Methods (MethodID, MethodName, ValidFrom) VALUES (2, 'b', NULL);",
        "DO $$\nDECLARE\n  v_x integer;\nBEGIN\n  v_x := 1;\n  INSERT INTO Methods (MethodID, MethodName, ValidFrom) VALUES (v_x, 'a', NULL);\n  INSERT INTO Methods (MethodID, MethodName, ValidFrom) VALUES (2, 'b', NULL);\nEND\n$$;",
    );
    assert_eq!(r.verdict(), Verdict::Different, "{:?}", rules(&r));
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "equiv.error-continues")
        .expect("error-continues");
    // The literal assignment cannot fail: the first INSERT is the one.
    assert!(f.witness.contains("line 2 fails"), "{}", f.witness);
    // Plain statements run one by one on both sides: no such claim.
    let r = run(
        "INSERT dbo.Methods (MethodID, MethodName, ValidFrom) VALUES (1, 'a', NULL);\nINSERT dbo.Methods (MethodID, MethodName, ValidFrom) VALUES (2, 'b', NULL);",
        "INSERT INTO Methods (MethodID, MethodName, ValidFrom) VALUES (1, 'a', NULL);\nINSERT INTO Methods (MethodID, MethodName, ValidFrom) VALUES (2, 'b', NULL);",
    );
    assert!(
        !rules(&r).contains(&"equiv.error-continues"),
        "{:?}",
        rules(&r)
    );
}

#[test]
fn db2_variables_are_not_compared_yet() {
    let r = run_in(
        T,
        D,
        "DECLARE @n int = 7;\nPRINT 'x';",
        "BEGIN\n  DECLARE v_n INTEGER;\n  SET v_n = 7;\nEND;",
    );
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.contains("no verified Db2 for i type facts")),
        "{:?}",
        r.unknown_reasons
    );
}
