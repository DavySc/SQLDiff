//! T-SQL bit against PostgreSQL boolean and Db2 for i SMALLINT. PostgreSQL
//! rejects a boolean meeting a number (`b = 1`, `b IN (0, 1)`, `b + 0`,
//! `COALESCE(b, 0)`, a CASE mixing the two, an integer stored in a boolean
//! column), so a conversion that keeps those is never equal. Each engine's
//! behaviour was checked on SQL Server 2022 and PostgreSQL 18; see
//! eval/bit-boolean/.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const MS_DDL: &str = "CREATE TABLE dbo.Flags (Id int NOT NULL PRIMARY KEY, IsActive bit NOT NULL,
  IsDeleted bit NULL, Score int NULL, Name varchar(20) NULL);";
const PG_DDL: &str =
    "CREATE TABLE Flags (Id integer NOT NULL PRIMARY KEY, IsActive boolean NOT NULL,
  IsDeleted boolean NULL, Score integer NULL, Name varchar(20) NULL);";
const DB2_DDL: &str =
    "CREATE TABLE Flags (Id INTEGER NOT NULL PRIMARY KEY, IsActive SMALLINT NOT NULL,
  IsDeleted SMALLINT NULL, Score INTEGER NULL, Name VARCHAR(20) NULL);";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Want {
    /// Equal, possibly under assumptions.
    Eq,
    /// Different.
    Diff,
    /// Anything but equal.
    NotEq,
    /// Unknown: neither proven nor refuted.
    Unknown,
}
use Want::*;

/// A case is a select list (wrapped in a view) or, starting with `!`, a whole
/// statement.
fn wrap(d: Dialect, s: &str) -> String {
    match (s.strip_prefix('!'), d) {
        (Some(stmt), _) => stmt.to_string(),
        (None, Dialect::Tsql) => format!("CREATE VIEW dbo.V AS SELECT {s};"),
        (None, _) => format!("CREATE OR REPLACE VIEW V AS SELECT {s};"),
    }
}

fn run(a: &str, b: &str, db: Dialect) -> Report {
    let ddl = if db == Dialect::Postgres {
        PG_DDL
    } else {
        DB2_DDL
    };
    let opts = Options {
        catalog_a: vec![Artifact::new("catalog.sql", Dialect::Tsql, MS_DDL)],
        catalog_b: vec![Artifact::new("catalog.sql", db, ddl)],
        ..Options::default()
    };
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, wrap(Dialect::Tsql, a)),
        &Artifact::new("b.sql", db, wrap(db, b)),
        &opts,
    )
}

fn check(cases: &[(&str, &str, Want)], db: Dialect) {
    let mut bad = Vec::new();
    for (a, b, want) in cases {
        let r = run(a, b, db);
        let equal = matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions);
        let ok = match want {
            Eq => equal,
            Diff => r.verdict() == Verdict::Different,
            NotEq => !equal,
            Unknown => r.verdict() == Verdict::Unknown,
        };
        if !ok {
            let rules: Vec<_> = r.findings.iter().map(|f| f.rule.as_str()).collect();
            bad.push(format!(
                "{want:?}: got {:?} {rules:?}\n  A: {a}\n  B: {b}",
                r.verdict()
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Views: what a converter must write, and what it must not keep as written.
const PG_VIEWS: &[(&str, &str, Want)] = &[
    (
        r#"Id FROM dbo.Flags WHERE IsActive = 1"#,
        r#"Id FROM Flags WHERE IsActive = 1"#,
        Diff,
    ),
    (
        r#"Id FROM dbo.Flags WHERE IsActive = 1"#,
        r#"Id FROM Flags WHERE IsActive = TRUE"#,
        Eq,
    ),
    (
        r#"Id FROM dbo.Flags WHERE IsActive = 1"#,
        r#"Id FROM Flags WHERE IsActive"#,
        Eq,
    ),
    (
        r#"Id FROM dbo.Flags WHERE IsActive = 0"#,
        r#"Id FROM Flags WHERE NOT IsActive"#,
        Eq,
    ),
    (
        r#"Id FROM dbo.Flags WHERE IsActive <> 0"#,
        r#"Id FROM Flags WHERE IsActive <> FALSE"#,
        Eq,
    ),
    (
        r#"Id FROM dbo.Flags WHERE IsActive = 'true'"#,
        r#"Id FROM Flags WHERE IsActive = 'true'"#,
        Eq,
    ),
    (
        r#"Id FROM dbo.Flags WHERE IsActive = '2'"#,
        r#"Id FROM Flags WHERE IsActive = '2'"#,
        NotEq,
    ),
    (
        r#"Id, CAST(IsActive AS varchar(5)) AS s FROM dbo.Flags"#,
        r#"Id, CAST(IsActive AS varchar(5)) AS s FROM Flags"#,
        Diff,
    ),
    (
        r#"Id, CAST(IsActive AS int) AS s FROM dbo.Flags"#,
        r#"Id, CAST(IsActive AS integer) AS s FROM Flags"#,
        Eq,
    ),
    (
        r#"Id, IsActive + 0 AS s FROM dbo.Flags"#,
        r#"Id, IsActive + 0 AS s FROM Flags"#,
        Diff,
    ),
    (
        r#"Id, COALESCE(IsDeleted, 0) AS s FROM dbo.Flags"#,
        r#"Id, COALESCE(IsDeleted, 0) AS s FROM Flags"#,
        Diff,
    ),
    (
        r#"Id, ISNULL(IsDeleted, 0) AS s FROM dbo.Flags"#,
        r#"Id, COALESCE(IsDeleted, FALSE) AS s FROM Flags"#,
        Eq,
    ),
    (
        r#"Id, CASE WHEN Score > 1 THEN IsActive ELSE 0 END AS s FROM dbo.Flags"#,
        r#"Id, CASE WHEN Score > 1 THEN IsActive ELSE 0 END AS s FROM Flags"#,
        Diff,
    ),
    (
        r#"Id FROM dbo.Flags WHERE IsActive IN (0, 1)"#,
        r#"Id FROM Flags WHERE IsActive IN (0, 1)"#,
        Diff,
    ),
    (
        r#"Id, CONCAT('x', IsActive) AS s FROM dbo.Flags"#,
        r#"Id, CONCAT('x', IsActive) AS s FROM Flags"#,
        NotEq,
    ),
    (
        r#"a.Id FROM dbo.Flags a JOIN dbo.Flags b ON a.IsActive = b.IsDeleted"#,
        r#"a.Id FROM Flags a JOIN Flags b ON a.IsActive = b.IsDeleted"#,
        Eq,
    ),
    (
        r#"Id FROM dbo.Flags WHERE IsActive = Score"#,
        r#"Id FROM Flags WHERE IsActive = Score"#,
        Diff,
    ),
    (
        r#"Id, IsActive FROM dbo.Flags ORDER BY IsActive"#,
        r#"Id, IsActive FROM Flags ORDER BY IsActive"#,
        Eq,
    ),
    (
        r#"Id FROM dbo.Flags WHERE IsActive = 1 AND Score = 1"#,
        r#"Id FROM Flags WHERE IsActive AND Score = 1"#,
        Eq,
    ),
    (
        r#"Id, CASE WHEN IsActive = 1 THEN 'y' ELSE 'n' END AS s FROM dbo.Flags"#,
        r#"Id, CASE WHEN IsActive THEN 'y' ELSE 'n' END AS s FROM Flags"#,
        Eq,
    ),
    (
        r#"Id, CAST(Score AS bit) AS s FROM dbo.Flags"#,
        r#"Id, CAST(Score AS boolean) AS s FROM Flags"#,
        Eq,
    ),
    (
        r#"Id, CAST(IsActive AS bit) AS s FROM dbo.Flags"#,
        r#"Id, IsActive AS s FROM Flags"#,
        Eq,
    ),
    (
        r#"Id, MAX(Id) AS m FROM dbo.Flags GROUP BY Id, IsActive"#,
        r#"Id, MAX(Id) AS m FROM Flags GROUP BY Id, IsActive"#,
        Eq,
    ),
];

/// Variants and routines: NULLs, strings, defaults, stores, returns.
const PG_MORE: &[(&str, &str, Want)] = &[
    (
        r#"Id FROM dbo.Flags WHERE IsDeleted = 1"#,
        r#"Id FROM Flags WHERE IsDeleted IS TRUE"#,
        Unknown,
    ),
    (
        r#"Id FROM dbo.Flags WHERE IsActive = 0"#,
        r#"Id FROM Flags WHERE IsActive"#,
        NotEq,
    ),
    (
        r#"Id FROM dbo.Flags WHERE IsActive = 1"#,
        r#"Id FROM Flags WHERE NOT IsActive"#,
        NotEq,
    ),
    (
        r#"Id FROM dbo.Flags WHERE Score = 1"#,
        r#"Id FROM Flags WHERE Score = TRUE"#,
        Diff,
    ),
    (
        r#"Id, 1 AS x FROM dbo.Flags"#,
        r#"Id, TRUE AS x FROM Flags"#,
        NotEq,
    ),
    (
        r#"Id, CAST(Score AS bit) AS s FROM dbo.Flags"#,
        r#"Id, Score <> 0 AS s FROM Flags"#,
        Eq,
    ),
    (
        r#"Id, CAST(Name AS bit) AS s FROM dbo.Flags"#,
        r#"Id, CAST(Name AS boolean) AS s FROM Flags"#,
        NotEq,
    ),
    (
        r#"Id, ISNULL(IsDeleted, 5) AS s FROM dbo.Flags"#,
        r#"Id, COALESCE(IsDeleted, TRUE) AS s FROM Flags"#,
        Eq,
    ),
    (
        r#"Id, COALESCE(IsDeleted, 5) AS s FROM dbo.Flags"#,
        r#"Id, COALESCE(IsDeleted, TRUE) AS s FROM Flags"#,
        NotEq,
    ),
    (
        r#"Id, CAST(IsActive AS varchar(5)) AS s FROM dbo.Flags"#,
        r#"Id, CAST(CAST(IsActive AS integer) AS varchar(5)) AS s FROM Flags"#,
        Eq,
    ),
    (
        r#"Id FROM dbo.Flags WHERE IsActive = 1 AND IsDeleted = 0"#,
        r#"Id FROM Flags WHERE IsActive AND NOT IsDeleted"#,
        Eq,
    ),
    (
        r#"Id, CASE WHEN IsActive = 1 THEN 1 ELSE 0 END AS s FROM dbo.Flags"#,
        r#"Id, CASE WHEN IsActive THEN 1 ELSE 0 END AS s FROM Flags"#,
        Eq,
    ),
    (
        r#"Id, CAST(IsActive AS smallint) AS s FROM dbo.Flags"#,
        r#"Id, CAST(IsActive AS smallint) AS s FROM Flags"#,
        Diff,
    ),
    (
        r#"Id, CAST(Score AS bit) AS s FROM dbo.Flags"#,
        r#"Id, CAST(Score AS boolean) AS s FROM Flags"#,
        Eq,
    ),
    (
        r#"Id, IsActive FROM dbo.Flags ORDER BY IsDeleted"#,
        r#"Id, IsActive FROM Flags ORDER BY IsDeleted"#,
        NotEq,
    ),
    (
        r#"Id FROM dbo.Flags WHERE IsActive = 'false'"#,
        r#"Id FROM Flags WHERE IsActive = FALSE"#,
        Eq,
    ),
    (
        r#"Id FROM dbo.Flags WHERE IsActive = 'yes'"#,
        r#"Id FROM Flags WHERE IsActive = 'yes'"#,
        NotEq,
    ),
    (
        r#"Id FROM dbo.Flags WHERE IsActive IN (0, 1)"#,
        r#"Id FROM Flags WHERE IsActive IN (FALSE, TRUE)"#,
        Eq,
    ),
    (
        r#"Id FROM dbo.Flags WHERE IsActive <> IsDeleted"#,
        r#"Id FROM Flags WHERE IsActive <> IsDeleted"#,
        Eq,
    ),
    (
        r#"Id FROM dbo.Flags WHERE IsActive = 2"#,
        r#"Id FROM Flags WHERE IsActive = 2"#,
        Diff,
    ),
    (
        r#"!CREATE PROCEDURE dbo.P @Id int AS UPDATE dbo.Flags SET IsDeleted = 1 WHERE Id = @Id;"#,
        r#"!CREATE PROCEDURE P(p_id integer) LANGUAGE plpgsql AS $$ BEGIN UPDATE Flags SET IsDeleted = TRUE WHERE Id = p_id; END; $$;"#,
        Eq,
    ),
    (
        r#"!CREATE PROCEDURE dbo.P @Id int AS UPDATE dbo.Flags SET IsDeleted = 1 WHERE Id = @Id;"#,
        r#"!CREATE PROCEDURE P(p_id integer) LANGUAGE plpgsql AS $$ BEGIN UPDATE Flags SET IsDeleted = 1 WHERE Id = p_id; END; $$;"#,
        Diff,
    ),
    (
        r#"!CREATE PROCEDURE dbo.P @Id int AS UPDATE dbo.Flags SET IsDeleted = 5 WHERE Id = @Id;"#,
        r#"!CREATE PROCEDURE P(p_id integer) LANGUAGE plpgsql AS $$ BEGIN UPDATE Flags SET IsDeleted = TRUE WHERE Id = p_id; END; $$;"#,
        Eq,
    ),
    (
        r#"!CREATE PROCEDURE dbo.P @Id int AS UPDATE dbo.Flags SET IsDeleted = 0 WHERE Id = @Id;"#,
        r#"!CREATE PROCEDURE P(p_id integer) LANGUAGE plpgsql AS $$ BEGIN UPDATE Flags SET IsDeleted = TRUE WHERE Id = p_id; END; $$;"#,
        NotEq,
    ),
    (
        r#"!CREATE PROCEDURE dbo.P @Id int, @S int AS UPDATE dbo.Flags SET IsDeleted = @S WHERE Id = @Id;"#,
        r#"!CREATE PROCEDURE P(p_id integer, p_s integer) LANGUAGE plpgsql AS $$ BEGIN UPDATE Flags SET IsDeleted = (p_s <> 0) WHERE Id = p_id; END; $$;"#,
        Eq,
    ),
    (
        r#"!CREATE PROCEDURE dbo.P @Id int AS INSERT INTO dbo.Flags (Id, IsActive, IsDeleted) VALUES (@Id, 0, 1);"#,
        r#"!CREATE PROCEDURE P(p_id integer) LANGUAGE plpgsql AS $$ BEGIN INSERT INTO Flags (Id, IsActive, IsDeleted) VALUES (p_id, FALSE, TRUE); END; $$;"#,
        Eq,
    ),
    (
        r#"!CREATE PROCEDURE dbo.P @Id int AS INSERT INTO dbo.Flags (Id, IsActive, IsDeleted) VALUES (@Id, 'true', 'false');"#,
        r#"!CREATE PROCEDURE P(p_id integer) LANGUAGE plpgsql AS $$ BEGIN INSERT INTO Flags (Id, IsActive, IsDeleted) VALUES (p_id, 'true', 'false'); END; $$;"#,
        Eq,
    ),
    (
        r#"!CREATE PROCEDURE dbo.P @Id int, @B bit AS UPDATE dbo.Flags SET IsDeleted = @B WHERE Id = @Id;"#,
        r#"!CREATE PROCEDURE P(p_id integer, p_b boolean) LANGUAGE plpgsql AS $$ BEGIN UPDATE Flags SET IsDeleted = p_b WHERE Id = p_id; END; $$;"#,
        Eq,
    ),
    (
        r#"!CREATE PROCEDURE dbo.P @Id int, @B bit AS UPDATE dbo.Flags SET Score = 1 WHERE Id = @Id AND IsActive = @B;"#,
        r#"!CREATE PROCEDURE P(p_id integer, p_b boolean) LANGUAGE plpgsql AS $$ BEGIN UPDATE Flags SET Score = 1 WHERE Id = p_id AND IsActive = p_b; END; $$;"#,
        Eq,
    ),
    (
        r#"!CREATE PROCEDURE dbo.P @Id int AS BEGIN SET XACT_ABORT ON; DECLARE @b bit = 1; UPDATE dbo.Flags SET IsDeleted = @b WHERE Id = @Id; END"#,
        r#"!CREATE PROCEDURE P(p_id integer) LANGUAGE plpgsql AS $$ DECLARE v_b boolean := TRUE; BEGIN UPDATE Flags SET IsDeleted = v_b WHERE Id = p_id; END; $$;"#,
        Eq,
    ),
    (
        r#"!CREATE PROCEDURE dbo.P @Id int AS BEGIN SET XACT_ABORT ON; DECLARE @b bit = 2; UPDATE dbo.Flags SET IsDeleted = @b WHERE Id = @Id; END"#,
        r#"!CREATE PROCEDURE P(p_id integer) LANGUAGE plpgsql AS $$ DECLARE v_b boolean := 2; BEGIN UPDATE Flags SET IsDeleted = v_b WHERE Id = p_id; END; $$;"#,
        NotEq,
    ),
    (
        r#"!CREATE PROCEDURE dbo.P @Id int AS BEGIN SET XACT_ABORT ON; DECLARE @b bit = 1; UPDATE dbo.Flags SET IsDeleted = @b WHERE Id = @Id; END"#,
        r#"!CREATE PROCEDURE P(p_id integer) LANGUAGE plpgsql AS $$ DECLARE v_b boolean := 1; BEGIN UPDATE Flags SET IsDeleted = v_b WHERE Id = p_id; END; $$;"#,
        Eq,
    ),
    (
        r#"!CREATE FUNCTION dbo.F (@x int) RETURNS bit AS BEGIN RETURN @x; END"#,
        r#"!CREATE FUNCTION F(p_x integer) RETURNS boolean LANGUAGE plpgsql AS $$ BEGIN RETURN p_x <> 0; END; $$;"#,
        Eq,
    ),
    (
        r#"!CREATE FUNCTION dbo.F (@x int) RETURNS bit AS BEGIN RETURN @x; END"#,
        r#"!CREATE FUNCTION F(p_x integer) RETURNS boolean LANGUAGE plpgsql AS $$ BEGIN RETURN p_x; END; $$;"#,
        NotEq,
    ),
    (
        r#"!CREATE FUNCTION dbo.F (@b bit) RETURNS int AS BEGIN RETURN CASE WHEN @b = 1 THEN 10 ELSE 20 END; END"#,
        r#"!CREATE FUNCTION F(p_b boolean) RETURNS integer LANGUAGE plpgsql AS $$ BEGIN RETURN CASE WHEN p_b = TRUE THEN 10 ELSE 20 END; END; $$;"#,
        Eq,
    ),
    (
        r#"!CREATE FUNCTION dbo.F (@b bit) RETURNS int AS BEGIN RETURN CASE WHEN @b = 1 THEN 10 ELSE 20 END; END"#,
        r#"!CREATE FUNCTION F(p_b boolean) RETURNS integer LANGUAGE plpgsql AS $$ BEGIN RETURN CASE WHEN p_b = 1 THEN 10 ELSE 20 END; END; $$;"#,
        Diff,
    ),
    (
        r#"!CREATE FUNCTION dbo.F (@b bit) RETURNS int AS BEGIN RETURN CASE WHEN @b = 1 THEN 10 ELSE 20 END; END"#,
        r#"!CREATE FUNCTION F(p_b boolean) RETURNS integer LANGUAGE plpgsql AS $$ BEGIN RETURN CASE WHEN p_b THEN 10 ELSE 20 END; END; $$;"#,
        Eq,
    ),
    (
        r#"!CREATE PROCEDURE dbo.P @Id int, @B bit = 0 AS UPDATE dbo.Flags SET IsDeleted = @B WHERE Id = @Id;"#,
        r#"!CREATE PROCEDURE P(p_id integer, p_b boolean DEFAULT FALSE) LANGUAGE plpgsql AS $$ BEGIN UPDATE Flags SET IsDeleted = p_b WHERE Id = p_id; END; $$;"#,
        Eq,
    ),
    (
        r#"!CREATE PROCEDURE dbo.P @Id int, @B bit = 1 AS UPDATE dbo.Flags SET IsDeleted = @B WHERE Id = @Id;"#,
        r#"!CREATE PROCEDURE P(p_id integer, p_b boolean DEFAULT FALSE) LANGUAGE plpgsql AS $$ BEGIN UPDATE Flags SET IsDeleted = p_b WHERE Id = p_id; END; $$;"#,
        NotEq,
    ),
];

/// Db2 for i: bit is SMALLINT (ASM-BIT-SMALLINT); a stored non-zero becomes 1.
const DB2: &[(&str, &str, Want)] = &[
    (
        r#"Id FROM dbo.Flags WHERE IsActive = 1"#,
        r#"Id FROM Flags WHERE IsActive = 1"#,
        Eq,
    ),
    (
        r#"Id FROM dbo.Flags WHERE IsActive = 'true'"#,
        r#"Id FROM Flags WHERE IsActive = 1"#,
        Eq,
    ),
    (
        r#"Id, ISNULL(IsDeleted, 5) AS s FROM dbo.Flags"#,
        r#"Id, COALESCE(IsDeleted, 1) AS s FROM Flags"#,
        Eq,
    ),
    (
        r#"Id, ISNULL(IsDeleted, 5) AS s FROM dbo.Flags"#,
        r#"Id, COALESCE(IsDeleted, 5) AS s FROM Flags"#,
        NotEq,
    ),
    (
        r#"Id, CAST(Score AS bit) AS s FROM dbo.Flags"#,
        r#"Id, CAST(Score AS SMALLINT) AS s FROM Flags"#,
        NotEq,
    ),
    (
        r#"!CREATE PROCEDURE dbo.P @Id int AS UPDATE dbo.Flags SET IsDeleted = 5 WHERE Id = @Id;"#,
        r#"!CREATE PROCEDURE P(IN p_id INTEGER) LANGUAGE SQL BEGIN UPDATE Flags SET IsDeleted = 1 WHERE Id = p_id; END"#,
        Eq,
    ),
    (
        r#"!CREATE PROCEDURE dbo.P @Id int AS UPDATE dbo.Flags SET IsDeleted = 5 WHERE Id = @Id;"#,
        r#"!CREATE PROCEDURE P(IN p_id INTEGER) LANGUAGE SQL BEGIN UPDATE Flags SET IsDeleted = 5 WHERE Id = p_id; END"#,
        NotEq,
    ),
];

#[test]
fn postgres_views() {
    check(PG_VIEWS, Dialect::Postgres);
}

#[test]
fn postgres_routines_and_variants() {
    check(PG_MORE, Dialect::Postgres);
}

#[test]
fn db2i_smallint() {
    check(DB2, Dialect::Db2i);
}

/// A comparison kept as written is reported with the reason PostgreSQL
/// rejects it, not as an unexplained difference.
#[test]
fn kept_comparison_names_the_postgres_error() {
    let r = run(
        "Id FROM dbo.Flags WHERE IsActive = 1",
        "Id FROM Flags WHERE IsActive = 1",
        Dialect::Postgres,
    );
    assert_eq!(r.verdict(), Verdict::Different);
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "equiv.bool-type-error")
        .expect("bool-type-error finding");
    assert!(f.witness.contains("fails"), "{}", f.witness);
}

/// IIF is CASE shorthand, also with a bit next to 0 / 1 results.
#[test]
fn iif_is_a_case() {
    check(
        &[
            (
                "Id, IIF(Score > 2, 1, 0) AS h FROM dbo.Flags",
                "Id, CASE WHEN Score > 2 THEN 1 ELSE 0 END AS h FROM Flags",
                Eq,
            ),
            (
                "Id, IIF(Score > 2, 1, 0) AS h FROM dbo.Flags",
                "Id, CASE WHEN Score >= 2 THEN 1 ELSE 0 END AS h FROM Flags",
                NotEq,
            ),
            (
                "Id, IIF(Score > 2, 1, IsDeleted) AS h FROM dbo.Flags",
                "Id, CASE WHEN Score > 2 THEN TRUE ELSE IsDeleted END AS h FROM Flags",
                Eq,
            ),
            (
                "Id, IIF(Score > 2, 2, IsDeleted) AS h FROM dbo.Flags",
                "Id, CASE WHEN Score > 2 THEN 2 ELSE IsDeleted END AS h FROM Flags",
                Diff,
            ),
        ],
        Dialect::Postgres,
    );
}

/// From the cross-review of the first version.
#[test]
fn review_cases() {
    check(
        &[
            // A widening integer cast stays: bigint arithmetic overflows elsewhere.
            (
                "Id, CAST(Score AS bigint) * 2147483647 AS s FROM dbo.Flags",
                "Id, Score * 2147483647 AS s FROM Flags",
                NotEq,
            ),
            (
                "Id, CAST(Score AS bigint) + 2147483647 AS s FROM dbo.Flags",
                "Id, Score + 2147483647 AS s FROM Flags",
                NotEq,
            ),
            // BETWEEN on a boolean.
            (
                "Id FROM dbo.Flags WHERE IsActive BETWEEN 0 AND 1",
                "Id FROM Flags WHERE IsActive BETWEEN 0 AND 1",
                Diff,
            ),
            (
                "Id FROM dbo.Flags WHERE IsActive BETWEEN 0 AND 1",
                "Id FROM Flags WHERE IsActive BETWEEN FALSE AND TRUE",
                Eq,
            ),
            // A bit read through a derived table.
            (
                "d.Id FROM (SELECT Id, IsActive AS a FROM dbo.Flags) d WHERE d.a = 1",
                "d.Id FROM (SELECT Id, IsActive AS a FROM Flags) d WHERE d.a = 1",
                Diff,
            ),
            (
                "d.Id FROM (SELECT Id, IsActive AS a FROM dbo.Flags) d WHERE d.a = 1",
                "d.Id FROM (SELECT Id, IsActive AS a FROM Flags) d WHERE d.a = TRUE",
                Eq,
            ),
            (
                "d.Id FROM (SELECT Id, IsActive FROM dbo.Flags) d WHERE IsActive = 1",
                "d.Id FROM (SELECT Id, IsActive FROM Flags) d WHERE IsActive = 1",
                Diff,
            ),
            // SQL Server rejects arithmetic between two bits.
            (
                "Id, IsActive + IsDeleted AS s FROM dbo.Flags",
                "Id, IsActive::integer + IsDeleted::integer AS s FROM Flags",
                Diff,
            ),
        ],
        Dialect::Postgres,
    );
}
