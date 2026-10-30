//! SQL Server geography constants stored in a geography column become
//! PostGIS EWKB. SQL Server 2022 and PostGIS 3.6 were run on the 400 points
//! of pds150-ins-app-cities-a.sql: SQL Server's Long/Lat and PostGIS's
//! ST_X/ST_Y of the EWKB written are the same doubles. PostGIS rejects
//! SQL Server's own form ("Invalid endian flag value encountered").

use sqldiff_convert::{check, rewrite_in, Conversion, Status};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{Artifact, Dialect, Options};

const TABLE: &str = "CREATE TABLE dbo.Cities (
    CityID int NOT NULL PRIMARY KEY,
    Location [sys].[geography] NULL
);
";

fn pg(src: &str) -> Conversion {
    let mut cat = Catalog::default();
    for f in [TABLE, src] {
        let root = sqldiff_syntax::parse(f, Dialect::Tsql).syntax();
        ddl::load_tree(&mut cat, &root, Dialect::Tsql);
    }
    let target = rewrite_in(TABLE, Dialect::Tsql, Dialect::Postgres, None)
        .output()
        .to_string();
    let opts = Options {
        catalog_a: vec![Artifact::new("t.sql", Dialect::Tsql, TABLE)],
        catalog_b: vec![Artifact::new("t.sql", Dialect::Postgres, &target)],
        ..Options::default()
    };
    check(
        src,
        rewrite_in(src, Dialect::Tsql, Dialect::Postgres, Some(&cat)),
        &opts,
    )
}

fn status(c: &Conversion) -> Status {
    c.units.iter().map(|u| u.status).max().unwrap()
}

#[test]
fn point_constants_become_ewkb_and_are_proven() {
    let src = "SET XACT_ABORT ON;\nINSERT dbo.Cities (CityID, Location) VALUES (101, 0xe6100000010c610619b7874a4240c982781332c455c0), (102, 0xE6100000010F00000000000042400000000000C055C000000000000014400000000000001C40);\n";
    let c = pg(src);
    for text in [
        "'0101000020E6100000C982781332C455C0610619B7874A4240'::geography",
        "'01010000E0E61000000000000000C055C0000000000000424000000000000014400000000000001C40'::geography",
    ] {
        assert!(c.output.contains(text), "{text}\n{}", c.output);
    }
    assert!(status(&c) < Status::Unproven, "{c:?}");
}

#[test]
fn points_postgis_would_change_stay_binary_and_unproven() {
    // Longitude -187: SQL Server keeps it, PostGIS moves it to 173.
    let src = "SET XACT_ABORT ON;\nINSERT dbo.Cities (CityID, Location) VALUES (1, 0xE6100000010C000000000000424000000000006067C0);\n";
    let c = pg(src);
    assert!(c.output.contains("::bytea"), "{}", c.output);
    assert!(status(&c) >= Status::Unproven, "{c:?}");
}
