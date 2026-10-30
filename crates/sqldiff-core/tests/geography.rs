//! T-SQL geography against PostGIS geography: a value copied as it is
//! agrees; anything that builds, compares or orders one is left to the
//! dialects. Checked on PostGIS 3.6 (PostgreSQL 18): SQL Server's binary
//! form 0xE6100000010C… stored in a geography column fails with "Invalid
//! endian flag value encountered".

use sqldiff_core::{diff, Artifact, Dialect, Options, Verdict};

const MS_DDL: &str = "CREATE TABLE dbo.C (Id int NOT NULL PRIMARY KEY, L1 [sys].[geography] NULL, L2 geography NULL);";
const PG_DDL: &str =
    "CREATE TABLE C (Id integer NOT NULL PRIMARY KEY, L1 geography NULL, L2 geography NULL);";

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
fn a_geography_copied_as_it_is() {
    assert!(equal(
        "UPDATE dbo.C SET L2 = L1 WHERE Id = 1;",
        "UPDATE C SET L2 = L1 WHERE Id = 1;"
    ));
    assert!(equal(
        "CREATE FUNCTION dbo.Loc (@Id int) RETURNS geography AS BEGIN DECLARE @g geography; SET @g = (SELECT L1 FROM dbo.C WHERE Id = @Id); RETURN @g; END;",
        "CREATE OR REPLACE FUNCTION Loc(p_id integer) RETURNS geography LANGUAGE plpgsql AS $$ DECLARE v_g geography; BEGIN v_g := (SELECT L1 FROM C WHERE Id = p_id); RETURN v_g; END; $$;",
    ));
}

#[test]
fn geography_built_compared_or_ordered_is_not_shown_equal() {
    let mut bad = Vec::new();
    for (a, b) in [
        (
            "UPDATE dbo.C SET L1 = 0xE6100000010C610619B7874A4240C982781332C455C0 WHERE Id = 1;",
            "UPDATE C SET L1 = '\\xE6100000010C610619B7874A4240C982781332C455C0'::bytea WHERE Id = 1;",
        ),
        (
            "SELECT Id FROM dbo.C WHERE L1 = L2;",
            "SELECT Id FROM C WHERE L1 = L2;",
        ),
        ("SELECT Id FROM dbo.C ORDER BY L1;", "SELECT Id FROM C ORDER BY L1;"),
        ("SELECT MIN(L1) AS m FROM dbo.C;", "SELECT MIN(L1) AS m FROM C;"),
        (
            "SELECT L1, COUNT(*) AS n FROM dbo.C GROUP BY L1;",
            "SELECT L1, COUNT(*) AS n FROM C GROUP BY L1;",
        ),
        ("SELECT DISTINCT L1 FROM dbo.C;", "SELECT DISTINCT L1 FROM C;"),
        (
            "SELECT COUNT(DISTINCT L1) AS n FROM dbo.C;",
            "SELECT COUNT(DISTINCT L1) AS n FROM C;",
        ),
        (
            "SELECT L1 FROM dbo.C UNION SELECT L2 FROM dbo.C;",
            "SELECT L1 FROM C UNION SELECT L2 FROM C;",
        ),
        (
            "SELECT L1 FROM dbo.C INTERSECT SELECT L2 FROM dbo.C;",
            "SELECT L1 FROM C INTERSECT SELECT L2 FROM C;",
        ),
        (
            "SELECT L1 FROM dbo.C EXCEPT SELECT L2 FROM dbo.C;",
            "SELECT L1 FROM C EXCEPT SELECT L2 FROM C;",
        ),
        (
            "SELECT Id FROM dbo.C WHERE L1 IN (L2);",
            "SELECT Id FROM C WHERE L1 IN (L2);",
        ),
        (
            "SELECT Id FROM dbo.C WHERE L1 IN (SELECT L2 FROM dbo.C);",
            "SELECT Id FROM C WHERE L1 IN (SELECT L2 FROM C);",
        ),
        (
            "SELECT CASE L1 WHEN L2 THEN 1 ELSE 0 END AS s FROM dbo.C;",
            "SELECT CASE L1 WHEN L2 THEN 1 ELSE 0 END AS s FROM C;",
        ),
    ] {
        if equal(a, b) {
            bad.push(format!("shown equal:\n  {a}\n  {b}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn point_constants_read_to_the_same_point() {
    // geography::Point(36.5822667, -87.0655564, 4326) and its EWKB.
    let t = "UPDATE dbo.C SET L1 = 0xE6100000010C610619B7874A4240C982781332C455C0 WHERE Id = 1;";
    assert!(equal(
        t,
        "UPDATE C SET L1 = '0101000020E6100000C982781332C455C0610619B7874A4240'::geography WHERE Id = 1;"
    ));
    // The latitude's last mantissa bit, latitude and longitude swapped,
    // another SRID, Z dropped.
    for b in [
        "UPDATE C SET L1 = '0101000020E6100000C982781332C455C0600619B7874A4240'::geography WHERE Id = 1;",
        "UPDATE C SET L1 = '0101000020E6100000610619B7874A4240C982781332C455C0'::geography WHERE Id = 1;",
        "UPDATE C SET L1 = '0101000020AD100000C982781332C455C0610619B7874A4240'::geography WHERE Id = 1;",
    ] {
        assert!(!equal(t, b), "{b}");
    }
    assert!(!equal(
        "UPDATE dbo.C SET L1 = 0xE6100000010D00000000000042400000000000C055C00000000000001440 WHERE Id = 1;",
        "UPDATE C SET L1 = '0101000020E61000000000000000C055C00000000000004240'::geography WHERE Id = 1;"
    ));
}
