//! The first argument of DATEADD, DATEDIFF, DATEPART and DATENAME is a date
//! part, a word, even where a table in scope has a column with that name
//! (WWI MakeTemporalChanges read `minute` as a column of the FROM table).
//! Whole-unit DATEADD keeps datetime on its 1/300 s ticks (SQL Server 2022:
//! DATEADD(minute, 5, '…10:00:00.003') is '…10:05:00.003', as PostgreSQL's
//! timestamp(3) + make_interval(mins => 5)); smalldatetime rounds seconds.

use sqldiff_core::{diff, Artifact, Dialect, Options, Verdict};

const MS_DDL: &str = "CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, [minute] int NULL, [day] int NULL, dt datetime NULL, sd smalldatetime NULL, a date NULL, b date NULL, n int NULL);";
const PG_DDL: &str = "CREATE TABLE t (id integer NOT NULL PRIMARY KEY, minute integer NULL, day integer NULL, dt timestamp(3) NULL, sd timestamp(0) NULL, a date NULL, b date NULL, n integer NULL);";

fn equal(a: &str, b: &str) -> bool {
    let opts = Options {
        catalog_a: vec![Artifact::new("catalog.sql", Dialect::Tsql, MS_DDL)],
        catalog_b: vec![Artifact::new("catalog.sql", Dialect::Postgres, PG_DDL)],
        ..Options::default()
    };
    let r = diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Postgres, b),
        &opts,
    );
    matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions)
}

#[test]
fn a_date_part_is_not_the_column_of_that_name() {
    // DATEDIFF(day, …) with a column named `day` in scope.
    assert!(equal(
        "UPDATE dbo.t SET n = DATEDIFF(day, a, b) WHERE id = 1;",
        "UPDATE t SET n = ((b)::date - (a)::date) WHERE id = 1;"
    ));
    for (part, field) in [("minute", "mins"), ("hour", "hours"), ("day", "days")] {
        let a = format!("UPDATE dbo.t SET dt = DATEADD({part}, 5, dt) WHERE id = 1;");
        let b = format!("UPDATE t SET dt = (dt + make_interval({field} => 5)) WHERE id = 1;");
        assert!(equal(&a, &b), "{a}");
    }
    // A synonym is the same part: DATEADD(mi, …) as DATEADD(minute, …).
    assert!(equal(
        "UPDATE dbo.t SET dt = DATEADD(mi, 5, dt) WHERE id = 1;",
        "UPDATE t SET dt = (dt + make_interval(mins => 5)) WHERE id = 1;"
    ));
    assert!(equal(
        "UPDATE dbo.t SET n = DATEDIFF(dd, a, b) WHERE id = 1;",
        "UPDATE t SET n = ((b)::date - (a)::date) WHERE id = 1;"
    ));
    // The column itself is still a column.
    assert!(!equal(
        "UPDATE dbo.t SET dt = DATEADD(minute, 5, dt) WHERE id = 1;",
        "UPDATE t SET dt = (dt + make_interval(mins => minute)) WHERE id = 1;"
    ));
    assert!(equal(
        "UPDATE dbo.t SET dt = DATEADD(minute, [minute], dt) WHERE id = 1;",
        "UPDATE t SET dt = (dt + make_interval(mins => minute)) WHERE id = 1;"
    ));
}

#[test]
fn whole_units_on_datetime_and_smalldatetime() {
    assert!(equal(
        "UPDATE dbo.t SET dt = DATEADD(second, 7, dt) WHERE id = 1;",
        "UPDATE t SET dt = (dt + make_interval(secs => 7)) WHERE id = 1;"
    ));
    assert!(equal(
        "UPDATE dbo.t SET sd = DATEADD(minute, 5, sd) WHERE id = 1;",
        "UPDATE t SET sd = (sd + make_interval(mins => 5)) WHERE id = 1;"
    ));
    // DATEADD(second, 29, smalldatetime) rounds back to the minute.
    assert!(!equal(
        "UPDATE dbo.t SET sd = DATEADD(second, 29, sd) WHERE id = 1;",
        "UPDATE t SET sd = (sd + make_interval(secs => 29)) WHERE id = 1;"
    ));
}
