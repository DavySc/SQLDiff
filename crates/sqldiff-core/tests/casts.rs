//! Casts, implicit conversions and literal types across T-SQL and
//! PostgreSQL. A conversion is equal only where SQL Server 2022 and
//! PostgreSQL 18 give the same value or the same error; the runs behind each
//! case are in eval/casts/. Db2 for i conversions are compared only between
//! integers and to wider decimals.

use sqldiff_core::{diff, Artifact, Dialect, Options, Report, Verdict};

const MS_DDL: &str = "CREATE TABLE dbo.M (Id int NOT NULL PRIMARY KEY, D decimal(10,2) NULL,
  D0 decimal(10,0) NULL, D1 decimal(1,1) NULL, F float NULL, R real NULL, S varchar(20) NULL,
  C char(5) NULL, T datetime2(3) NULL, Dt datetime NULL, Tm time(3) NULL, Q int NULL, B bigint NULL,
  Ti tinyint NULL, Big decimal(38,10) NULL, Day date NULL);";
const PG_DDL: &str = "CREATE TABLE M (Id integer NOT NULL PRIMARY KEY, D decimal(10,2) NULL,
  D0 decimal(10,0) NULL, D1 decimal(1,1) NULL, F double precision NULL, R real NULL,
  S varchar(20) NULL, C char(5) NULL, T timestamp(3) NULL, Dt timestamp(3) NULL, Tm time(3) NULL,
  Q integer NULL, B bigint NULL, Ti smallint NULL, Big decimal(38,10) NULL, Day date NULL);";
const DB2_DDL: &str = "CREATE TABLE M (Id INTEGER NOT NULL PRIMARY KEY, D DECIMAL(10,2),
  D0 DECIMAL(10,0), D1 DECIMAL(1,1), F DOUBLE, R REAL, S VARCHAR(20), C CHAR(5),
  T TIMESTAMP(3), Dt TIMESTAMP(3), Tm TIME, Q INTEGER, B BIGINT, Ti SMALLINT,
  Big DECIMAL(38,10), Day DATE);";

#[derive(Clone, Copy, Debug, PartialEq)]
enum Want {
    /// Equal, possibly under assumptions.
    Eq,
    /// Different, with the given rule.
    Diff(&'static str),
    /// Unknown: neither proven nor refuted.
    Unknown,
}
use Want::*;

/// A case is a select list over M (wrapped in a view) or, starting with `!`,
/// a whole statement.
fn wrap(d: Dialect, s: &str) -> String {
    match (s.strip_prefix('!'), d) {
        (Some(stmt), _) => stmt.to_string(),
        (None, Dialect::Tsql) => format!("CREATE VIEW dbo.V AS SELECT Id, {s} AS x FROM dbo.M;"),
        (None, _) => format!("CREATE OR REPLACE VIEW V AS SELECT Id, {s} AS x FROM M;"),
    }
}

fn run(a: &str, b: &str, db: Dialect, catalog: bool) -> Report {
    let ddl = if db == Dialect::Postgres {
        PG_DDL
    } else {
        DB2_DDL
    };
    let opts = if catalog {
        Options {
            catalog_a: vec![Artifact::new("catalog.sql", Dialect::Tsql, MS_DDL)],
            catalog_b: vec![Artifact::new("catalog.sql", db, ddl)],
            ..Options::default()
        }
    } else {
        Options::default()
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
        let r = run(a, b, db, true);
        let equal = matches!(r.verdict, Verdict::Equal | Verdict::EqualUnderAssumptions);
        let rules: Vec<_> = r.findings.iter().map(|f| f.rule.as_str()).collect();
        let ok = match want {
            Eq => equal,
            Diff(rule) => r.verdict == Verdict::Different && rules.contains(rule),
            Unknown => r.verdict == Verdict::Unknown,
        };
        if !ok {
            bad.push(format!(
                "{want:?}: got {:?} {rules:?}\n  A: {a}\n  B: {b}\n  {:?}",
                r.verdict,
                r.findings.iter().map(|f| &f.witness).collect::<Vec<_>>()
            ));
        }
        // Without catalogs a case is never equal where the catalog shows it
        // is not, unless an assumption about the unknown types says so.
        if !matches!(want, Eq) {
            let r = run(a, b, db, false);
            let untyped = r
                .assumptions
                .iter()
                .any(|x| x.id.starts_with("ASM-UNTYPED"));
            let equal = r.verdict == Verdict::Equal
                || (r.verdict == Verdict::EqualUnderAssumptions && !untyped);
            if equal {
                bad.push(format!("equal without catalogs\n  A: {a}\n  B: {b}"));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// The two reported false equals, as whole statements.
#[test]
fn reported_false_equals() {
    let cases = [
        (
            "!SELECT CAST(1.5 AS int);",
            "!SELECT CAST(1.5 AS integer);",
            Diff("equiv.cast-rounding"),
        ),
        (
            "!SELECT CAST(1234567e0 AS varchar(30));",
            "!SELECT CAST(1234567e0 AS varchar(30));",
            Diff("equiv.float-text"),
        ),
    ];
    check(&cases, Dialect::Postgres);
}

/// Decimal and float to integer: T-SQL truncates, PostgreSQL rounds.
const TO_INT: &[(&str, &str, Want)] = &[
    (
        "CAST(D AS int)",
        "CAST(D AS integer)",
        Diff("equiv.cast-rounding"),
    ),
    ("CAST(D AS int)", "CAST(trunc(D) AS integer)", Eq),
    ("CONVERT(int, D)", "D::integer", Diff("equiv.cast-rounding")),
    ("CONVERT(varchar(30), D)", "CAST(D AS varchar(30))", Eq),
    (
        "CAST(F AS bigint)",
        "CAST(F AS bigint)",
        Diff("equiv.cast-rounding"),
    ),
    ("CAST(F AS bigint)", "CAST(trunc(F) AS bigint)", Eq),
    (
        "CAST(D1 AS int)",
        "CAST(D1 AS integer)",
        Diff("equiv.cast-rounding"),
    ),
    // No fraction: the same on both sides.
    ("CAST(D0 AS int)", "CAST(D0 AS integer)", Eq),
    ("CAST(Q AS bigint)", "CAST(Q AS bigint)", Eq),
    ("CAST(2.0 AS int)", "CAST(2.0 AS integer)", Eq),
    ("CAST(1.2 AS int)", "CAST(1.2 AS integer)", Unknown),
    (
        "CAST(-2.5 AS int)",
        "CAST(-2.5 AS integer)",
        Diff("equiv.cast-rounding"),
    ),
    // trunc of an integer is trunc(double precision) in PostgreSQL.
    ("CAST(B AS bigint)", "CAST(trunc(B) AS bigint)", Unknown),
];

#[test]
fn decimal_and_float_to_integer() {
    check(TO_INT, Dialect::Postgres);
}

/// Stored in an integer column or variable: the same rounding.
#[test]
fn implicit_conversion_to_integer() {
    let cases = [
        (
            "!UPDATE dbo.M SET Q = D WHERE Id = 1;",
            "!UPDATE M SET Q = D WHERE Id = 1;",
            Diff("equiv.cast-rounding"),
        ),
        (
            "!UPDATE dbo.M SET Q = D WHERE Id = 1;",
            "!UPDATE M SET Q = trunc(D) WHERE Id = 1;",
            Eq,
        ),
        (
            "!INSERT INTO dbo.M (Id, Q) VALUES (1, 2.5);",
            "!INSERT INTO M (Id, Q) VALUES (1, 2.5);",
            Diff("equiv.cast-rounding"),
        ),
        (
            "!UPDATE dbo.M SET D = D0 WHERE Id = 1;",
            "!UPDATE M SET D = D0 WHERE Id = 1;",
            Eq,
        ),
        (
            "!UPDATE dbo.M SET S = C WHERE Id = 1;",
            "!UPDATE M SET S = C WHERE Id = 1;",
            Eq,
        ),
    ];
    check(&cases, Dialect::Postgres);
}

/// Literal types: float exponents, large integers, hex, N-strings.
const LITERALS: &[(&str, &str, Want)] = &[
    (
        "CAST(1.5e0 AS varchar(30))",
        "CAST(1.50e0 AS varchar(30))",
        Unknown,
    ),
    (
        "CAST(1.50e0 AS varchar(30))",
        "CAST(1.50e0 AS varchar(30))",
        Diff("equiv.float-text"),
    ),
    (
        "CAST(1.5e0 AS varchar(30))",
        "CAST(1.5e0 AS varchar(30))",
        Unknown,
    ),
    ("Q + 1e0", "Q + 1e0", Unknown),
    ("Q * 1.5", "Q * 1.5", Unknown),
    ("Q + 1.5", "Q + 1.5", Eq),
    ("Q + 1", "Q + 1", Eq),
    // int in T-SQL range only; bigint in PostgreSQL, numeric(10,0) in T-SQL.
    ("Q / 7", "Q / 7", Eq),
    ("2147483648 / 7", "2147483648 / 7", Unknown),
    ("Q / 2147483648", "Q / 2147483648", Unknown),
    ("0x1F", "0x1F", Unknown),
    ("0x1F", "31", Unknown),
    ("Q + 1000", "Q + 1_000", Eq),
];

#[test]
fn literal_types() {
    check(LITERALS, Dialect::Postgres);
    // PostgreSQL N'..' is character(n), blank-padded.
    check(
        &[(
            "CASE WHEN S = N'a ' THEN 1 ELSE 0 END",
            "CASE WHEN S = N'a ' THEN 1 ELSE 0 END",
            Unknown,
        )],
        Dialect::Postgres,
    );
}

/// Numbers and times as text, and text as numbers.
const TEXT: &[(&str, &str, Want)] = &[
    ("CAST(Q AS varchar(11))", "CAST(Q AS varchar(11))", Eq),
    ("CAST(Q AS varchar(5))", "CAST(Q AS varchar(5))", Unknown),
    ("CAST(D AS varchar(30))", "CAST(D AS varchar(30))", Eq),
    ("CAST(D AS varchar(12))", "CAST(D AS varchar(12))", Eq),
    ("CAST(D AS varchar(5))", "CAST(D AS varchar(5))", Unknown),
    ("CAST(1.50 AS varchar(30))", "CAST(1.50 AS varchar(30))", Eq),
    (
        "CAST(F AS varchar(30))",
        "CAST(F AS varchar(30))",
        Diff("equiv.float-text"),
    ),
    (
        "CAST(S AS int)",
        "CAST(S AS integer)",
        Diff("equiv.cast-string-number"),
    ),
    (
        "CAST(S AS decimal(10,2))",
        "CAST(S AS decimal(10,2))",
        Diff("equiv.cast-string-number"),
    ),
    (
        "CAST(S AS float)",
        "CAST(S AS double precision)",
        Diff("equiv.cast-string-number"),
    ),
    // A string literal converts by its value: equal where both engines read
    // the same number, not compared where one may round, fail or pad.
    ("CAST('12' AS int)", "CAST('12' AS integer)", Eq),
    ("CAST('-007' AS bigint)", "CAST('-007' AS bigint)", Eq),
    (
        "CAST('1.5' AS decimal(5,1))",
        "CAST('1.5' AS numeric(5,1))",
        Eq,
    ),
    (
        "CAST('12' AS int)",
        "CAST('13' AS integer)",
        Diff("equiv.witness"),
    ),
    ("CAST('' AS int)", "CAST('' AS integer)", Unknown),
    ("CAST(' 12 ' AS int)", "CAST(' 12 ' AS integer)", Unknown),
    (
        "CAST('1.55' AS decimal(5,1))",
        "CAST('1.55' AS numeric(5,1))",
        Unknown,
    ),
    ("CAST('1.5' AS int)", "CAST('1.5' AS integer)", Unknown),
    (
        "CAST(T AS varchar(40))",
        "CAST(T AS varchar(40))",
        Diff("equiv.time-text"),
    ),
    (
        "CAST(Dt AS varchar(40))",
        "CAST(Dt AS varchar(40))",
        Diff("equiv.time-text"),
    ),
    (
        "CAST(Tm AS varchar(40))",
        "CAST(Tm AS varchar(40))",
        Diff("equiv.time-text"),
    ),
    (
        "CAST(Day AS varchar(40))",
        "CAST(Day AS varchar(40))",
        Unknown,
    ),
    ("CAST(S AS varchar(5))", "CAST(S AS varchar(5))", Eq),
    ("CAST(S AS char(5))", "CAST(S AS char(5))", Eq),
];

#[test]
fn text_conversions() {
    check(TEXT, Dialect::Postgres);
}

/// Decimal precision and scale, float and real.
const NUMBERS: &[(&str, &str, Want)] = &[
    ("CAST(D AS decimal(10,1))", "CAST(D AS decimal(10,1))", Eq),
    ("CAST(Q AS decimal(12,2))", "CAST(Q AS decimal(12,2))", Eq),
    (
        "CAST(F AS decimal(10,2))",
        "CAST(F AS decimal(10,2))",
        Diff("equiv.cast-rounding"),
    ),
    ("CAST(D AS float)", "CAST(D AS double precision)", Unknown),
    ("CAST(Q AS float)", "CAST(Q AS double precision)", Eq),
    ("CAST(F AS real)", "CAST(F AS double precision)", Unknown),
    ("CAST(R AS float)", "CAST(R AS double precision)", Eq),
    // T-SQL DECIMAL without precision is (18,0); PostgreSQL's is unbounded.
    ("CAST(D AS decimal)", "CAST(D AS decimal)", Unknown),
    (
        "CASE WHEN R = 0.1 THEN 1 ELSE 0 END",
        "CASE WHEN R = 0.1 THEN 1 ELSE 0 END",
        Unknown,
    ),
    (
        "CASE WHEN R = 1 THEN 1 ELSE 0 END",
        "CASE WHEN R = 1 THEN 1 ELSE 0 END",
        Eq,
    ),
    ("R + D", "R + D", Unknown),
    ("F + D", "F + D", Eq),
    ("D + D0", "D + D0", Eq),
    // A T-SQL decimal sum past 38 digits overflows; numeric does not.
    ("Big + Big", "Big + Big", Unknown),
    // tinyint + tinyint overflows at 255 in T-SQL.
    ("Ti + Ti", "Ti + Ti", Unknown),
    ("Ti + 1", "Ti + 1", Eq),
];

#[test]
fn numeric_conversions() {
    check(NUMBERS, Dialect::Postgres);
}

/// Times: fewer fractional digits round differently.
const TIMES: &[(&str, &str, Want)] = &[
    ("CAST(T AS date)", "CAST(T AS date)", Eq),
    ("CAST(Day AS datetime2(3))", "CAST(Day AS timestamp(3))", Eq),
    (
        "CAST(T AS datetime2(0))",
        "CAST(T AS timestamp(0))",
        Diff("equiv.time-rounding"),
    ),
    (
        "CAST(T AS datetime2(1))",
        "CAST(T AS timestamp(1))",
        Diff("equiv.time-rounding"),
    ),
    (
        "CAST(Tm AS time(0))",
        "CAST(Tm AS time(0))",
        Diff("equiv.time-rounding"),
    ),
    ("CAST(T AS datetime2(3))", "CAST(T AS timestamp(3))", Eq),
    // datetime counts 1/300 s.
    ("CAST(T AS datetime)", "CAST(T AS timestamp(3))", Unknown),
    ("CAST(T AS datetime2)", "CAST(T AS timestamp)", Unknown),
];

#[test]
fn time_conversions() {
    check(TIMES, Dialect::Postgres);
}

/// Db2 for i: only integer and widening decimal conversions are compared.
#[test]
fn db2_conversions() {
    let cases = [
        ("CAST(Q AS bigint)", "CAST(Q AS BIGINT)", Eq),
        ("CAST(Q AS decimal(12,2))", "CAST(Q AS DECIMAL(12,2))", Eq),
        ("CAST(D AS decimal(12,3))", "CAST(D AS DECIMAL(12,3))", Eq),
        ("CAST(D AS int)", "CAST(D AS INTEGER)", Unknown),
        (
            "CAST(D AS decimal(10,1))",
            "CAST(D AS DECIMAL(10,1))",
            Unknown,
        ),
        ("CAST(F AS varchar(30))", "CAST(F AS VARCHAR(30))", Unknown),
        ("CAST(S AS int)", "CAST(S AS INTEGER)", Unknown),
    ];
    check(&cases, Dialect::Db2i);
}
