//! `geography::STGeomFromWKB(wkb, 4326)` and `geography::Point(lat, lon,
//! 4326)` against PostGIS. Run on SQL Server 2022 and PostGIS 3.5 on
//! 2026-11-02 (`eval/geography/2026-11-02/`): both keep the doubles of a
//! little-endian two-dimensional WKB unchanged and write the same bytes
//! back; SQL Server raises on a latitude beyond ±90, NaN and trailing
//! bytes, where PostGIS moves the value into range or accepts it. The
//! conversion is shown equal only for a constant both read alike, whether
//! written inline or built up in a `varbinary(max)` variable.

use sqldiff_convert::{check, rewrite_in, Conversion, Status};
use sqldiff_core::catalog::{ddl, Catalog};
use sqldiff_core::{Artifact, Dialect, Options};

const TABLE: &str = "CREATE TABLE dbo.Cities (
    CityID int NOT NULL PRIMARY KEY,
    Location [sys].[geography] NULL
);
";

fn convert(src: &str, to: Dialect) -> Conversion {
    let mut cat = Catalog::default();
    for f in [TABLE, src] {
        let root = sqldiff_syntax::parse(f, Dialect::Tsql).syntax();
        ddl::load_tree(&mut cat, &root, Dialect::Tsql);
    }
    let target = rewrite_in(TABLE, Dialect::Tsql, to, None)
        .output()
        .to_string();
    let opts = Options {
        catalog_a: vec![Artifact::new("t.sql", Dialect::Tsql, TABLE)],
        catalog_b: vec![Artifact::new("t.sql", to, &target)],
        ..Options::default()
    };
    check(src, rewrite_in(src, Dialect::Tsql, to, Some(&cat)), &opts)
}

fn pg(src: &str) -> Conversion {
    convert(src, Dialect::Postgres)
}

fn status(c: &Conversion) -> Status {
    c.units.iter().map(|u| u.status).max().unwrap()
}

fn d(v: f64) -> String {
    v.to_le_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

fn le32(n: u32) -> String {
    n.to_le_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

fn poly(ring: &[(f64, f64)]) -> String {
    let pts: String = ring.iter().map(|&(x, y)| d(x) + &d(y)).collect();
    format!("0103000000{}{}{pts}", le32(1), le32(ring.len() as u32))
}

/// A polygon of several rings, the first one the exterior.
fn poly_rings(rings: &[Vec<(f64, f64)>]) -> String {
    let body: String = rings
        .iter()
        .map(|r| {
            let pts: String = r.iter().map(|&(x, y)| d(x) + &d(y)).collect();
            format!("{}{pts}", le32(r.len() as u32))
        })
        .collect();
    format!("0103000000{}{body}", le32(rings.len() as u32))
}

/// A square of side `s` at (`x`, `y`), counter-clockwise or clockwise.
fn square(x: f64, y: f64, s: f64, ccw: bool) -> Vec<(f64, f64)> {
    let mut r = vec![(x, y), (x + s, y), (x + s, y + s), (x, y + s), (x, y)];
    if !ccw {
        r.reverse();
    }
    r
}

const SQUARE: [(f64, f64); 5] = [
    (-88.5, 30.2),
    (-87.5, 30.2),
    (-87.5, 31.2),
    (-88.5, 31.2),
    (-88.5, 30.2),
];

fn ok(c: &Conversion) {
    assert!(status(c) < Status::Unproven, "{c:#?}");
}

fn not_proven(c: &Conversion) {
    assert!(status(c) >= Status::Unproven, "{c:#?}");
}

#[test]
fn inline_polygon_is_converted_and_proven() {
    let src = format!(
        "SET XACT_ABORT ON;\nUPDATE dbo.Cities SET Location = geography::STGeomFromWKB(0x{}, 4326) WHERE CityID = 1;\n",
        poly(&SQUARE)
    );
    let c = pg(&src);
    assert!(c.output.contains("ST_GeomFromWKB("), "{}", c.output);
    assert!(c.output.contains(", 4326)::geography"), "{}", c.output);
    ok(&c);
}

#[test]
fn a_binary_built_in_pieces_is_read_as_a_whole() {
    let h = poly(&SQUARE);
    let (a, b) = h.split_at(40);
    let src = format!(
        "SET XACT_ABORT ON;\nDECLARE @B varbinary(max);\nDECLARE @G geography;\nSET @B = 0x{a};\nSET @B += 0x{b};\nSET @G = geography::STGeomFromWKB(@B, 4326);\nUPDATE dbo.Cities SET Location = @G WHERE CityID = 1;\n"
    );
    ok(&pg(&src));
    // The same value, assigned again before the call, is the new value.
    let src2 = format!(
        "SET XACT_ABORT ON;\nDECLARE @B varbinary(max) = 0x{h};\nSET @B = 0x{h};\nUPDATE dbo.Cities SET Location = geography::STGeomFromWKB(@B, 4326) WHERE CityID = 1;\n"
    );
    ok(&pg(&src2));
}

#[test]
fn other_shapes_and_a_point_in_wkb() {
    // Point, line string, multi polygon, empty collection.
    let point = format!("0101000000{}{}", d(-87.0655564), d(36.5822667));
    let line = format!(
        "0102000000{}{}{}{}{}",
        le32(2),
        d(1.0),
        d(2.0),
        d(3.0),
        d(4.0)
    );
    let apart = poly(&square(10.0, 10.0, 1.0, true));
    let multi = format!("0106000000{}{}{}", le32(2), poly(&SQUARE), apart);
    // A counter-clockwise shell with a clockwise hole.
    let holed = poly_rings(&[square(0.0, 0.0, 10.0, true), square(2.0, 2.0, 2.0, false)]);
    for h in [point, line, multi, holed, format!("0107000000{}", le32(0))] {
        let src = format!(
            "SET XACT_ABORT ON;\nUPDATE dbo.Cities SET Location = geography::STGeomFromWKB(0x{h}, 4326) WHERE CityID = 1;\n"
        );
        ok(&pg(&src));
    }
}

#[test]
fn point_from_decimal_coordinates() {
    for (lat, lon) in [
        ("37.765786", "-122.504086"),
        ("0", "0"),
        ("-90", "180"),
        ("36.5822667", "-87.0655564"),
    ] {
        let src = format!(
            "SET XACT_ABORT ON;\nUPDATE dbo.Cities SET Location = geography::Point({lat}, {lon}, 4326) WHERE CityID = 1;\n"
        );
        let c = pg(&src);
        assert!(
            c.output.contains(&format!(
                "ST_SetSRID(ST_MakePoint({lon}, {lat}), 4326)::geography"
            )),
            "{}",
            c.output
        );
        ok(&c);
    }
}

#[test]
fn coordinates_beyond_what_a_double_keeps_are_not_proven() {
    // 15 significant digits are read back exactly; 16 or more may not be.
    let src = |lat: &str| {
        format!("SET XACT_ABORT ON;\nUPDATE dbo.Cities SET Location = geography::Point({lat}, 1, 4326) WHERE CityID = 1;\n")
    };
    ok(&pg(&src("37.7657860000000")));
    not_proven(&pg(&src("37.76578600000001")));
    not_proven(&pg(&src("37.765786000000012345678")));
}

#[test]
fn input_the_engines_treat_differently_is_not_proven() {
    let mut bad: Vec<(&str, String)> = Vec::new();
    // Latitude 91: SQL Server raises, PostGIS moves it into range.
    let lat91 = poly(&[(0.0, 0.0), (1.0, 0.0), (1.0, 91.0), (0.0, 1.0), (0.0, 0.0)]);
    bad.push(("latitude 91", format!("0x{lat91}")));
    // Trailing byte: SQL Server raises, PostGIS reads it.
    bad.push(("trailing byte", format!("0x{}00", poly(&SQUARE))));
    // Ring not closed: both raise, but not alike (not a constant either).
    let open = poly(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]);
    bad.push(("open ring", format!("0x{open}")));
    // Big-endian and EWKB-flagged types.
    bad.push((
        "big endian",
        "0x00000000010000000000000000000000000000000000000000".into(),
    ));
    bad.push((
        "ewkb srid",
        format!("0x0101000020{}{}{}", le32(4326), d(1.0), d(2.0)),
    ));
    // NaN longitude.
    bad.push(("nan", format!("0x0101000000{}{}", d(f64::NAN), d(1.0))));
    // Ring direction: SQL Server takes the area on the left of the ring (a
    // clockwise exterior is the rest of the globe), PostGIS the smaller side.
    // Each shape has a counter-clockwise control that is proven below.
    let cw: Vec<_> = SQUARE.iter().rev().copied().collect();
    bad.push(("clockwise exterior", format!("0x{}", poly(&cw))));
    let shell = square(0.0, 0.0, 10.0, true);
    bad.push((
        "counter-clockwise hole",
        format!("0x{}", poly_rings(&[shell.clone(), square(2.0, 2.0, 2.0, true)])),
    ));
    bad.push((
        "clockwise shell with a clockwise hole",
        format!("0x{}", poly_rings(&[square(0.0, 0.0, 10.0, false), square(2.0, 2.0, 2.0, false)])),
    ));
    bad.push((
        "hole outside the shell",
        format!("0x{}", poly_rings(&[shell.clone(), square(20.0, 2.0, 2.0, false)])),
    ));
    let bow = vec![(0.0, 0.0), (1.0, 1.0), (1.0, 0.0), (0.0, 1.0), (0.0, 0.0)];
    bad.push(("bow tie", format!("0x{}", poly(&bow))));
    // A hemisphere and a pole: too large for the smaller-side reading.
    bad.push(("180 degrees wide", format!("0x{}", poly(&square(-90.0, 0.0, 180.0, true)))));
    bad.push(("around the pole", format!("0x{}", poly(&square(0.0, 80.0, 20.0, true)))));
    // A variable the code never gives a value: NULL, or a parameter's value.
    bad.push(("no value", "@p".into()));
    for (what, arg) in bad {
        let src = format!(
            "SET XACT_ABORT ON;\nDECLARE @p varbinary(max);\nUPDATE dbo.Cities SET Location = geography::STGeomFromWKB({arg}, 4326) WHERE CityID = 1;\n"
        );
        let c = pg(&src);
        assert!(status(&c) >= Status::Unproven, "{what}: {c:#?}");
    }
    // The same script with a valid constant is proven, so the cases above
    // fail for their argument and not for the script.
    let src = format!(
        "SET XACT_ABORT ON;\nDECLARE @p varbinary(max);\nUPDATE dbo.Cities SET Location = geography::STGeomFromWKB(0x{}, 4326) WHERE CityID = 1;\n",
        poly(&SQUARE)
    );
    ok(&pg(&src));
}

#[test]
fn a_variable_the_code_does_not_fix_is_not_proven() {
    let h = poly(&SQUARE);
    // varbinary(n) cuts a longer value off silently; the loop and the IF may
    // have changed the value; a SELECT writes it.
    for pre in [
        format!("DECLARE @B varbinary(100) = 0x{h};"),
        format!("DECLARE @B varbinary(max) = 0x{h}; WHILE @@ROWCOUNT > 5 SET @B = 0x00;"),
        format!("DECLARE @B varbinary(max) = 0x{h}; IF @@ROWCOUNT > 5 SET @B = 0x00;"),
        format!("DECLARE @B varbinary(max) = 0x{h}; SELECT @B = NULL;"),
        format!("DECLARE @B varbinary(max) = 0x{h}; EXEC dbo.Q @B OUTPUT;"),
    ] {
        let src = format!(
            "SET XACT_ABORT ON;\n{pre}\nUPDATE dbo.Cities SET Location = geography::STGeomFromWKB(@B, 4326) WHERE CityID = 1;\n"
        );
        not_proven(&pg(&src));
    }
}

#[test]
fn other_srids_and_variable_coordinates_are_not_proven() {
    for call in [
        format!("geography::STGeomFromWKB(0x{}, 4269)", poly(&SQUARE)),
        "geography::Point(91, 0, 4326)".to_string(),
        "geography::Point(0, 181, 4326)".to_string(),
        "geography::Point(1e0, 2e0, 4326)".to_string(),
    ] {
        let src = format!(
            "SET XACT_ABORT ON;\nUPDATE dbo.Cities SET Location = {call} WHERE CityID = 1;\n"
        );
        not_proven(&pg(&src));
    }
    let src = "SET XACT_ABORT ON;\nDECLARE @lat float = 1.5;\nDECLARE @lon float = 2.5;\nUPDATE dbo.Cities SET Location = geography::Point(@lat, @lon, 4326) WHERE CityID = 1;\n";
    not_proven(&pg(src));
}

#[test]
fn other_static_methods_are_refused_and_leave_no_residue() {
    for call in [
        "geography::STGeomFromText('POINT(1 2)', 4326)",
        "geography::Parse('POINT(1 2)')",
        "geometry::STGeomFromText('POINT(1 2)', 0)",
        "hierarchyid::GetRoot()",
    ] {
        let src = format!(
            "SET XACT_ABORT ON;\nUPDATE dbo.Cities SET Location = {call} WHERE CityID = 1;\n"
        );
        for to in [Dialect::Postgres, Dialect::Db2i] {
            let c = convert(&src, to);
            assert!(
                status(&c) == Status::NotConverted,
                "{call} to {to:?}: {c:#?}"
            );
            let live: String = c
                .output
                .lines()
                .filter(|l| !l.trim_start().starts_with("--"))
                .collect();
            assert!(
                !live.contains("::") || live.contains("RAISE"),
                "{call}: {}",
                c.output
            );
        }
    }
}

#[test]
fn db2_for_i_refuses_the_two_converted_methods() {
    let src = format!(
        "SET XACT_ABORT ON;\nUPDATE dbo.Cities SET Location = geography::STGeomFromWKB(0x{}, 4326) WHERE CityID = 1;\nUPDATE dbo.Cities SET Location = geography::Point(1, 2, 4326) WHERE CityID = 2;\n",
        poly(&SQUARE)
    );
    let c = convert(&src, Dialect::Db2i);
    assert!(!c.output.contains("ST_GeomFromWKB"), "{}", c.output);
    assert!(status(&c) == Status::NotConverted, "{c:#?}");
}
