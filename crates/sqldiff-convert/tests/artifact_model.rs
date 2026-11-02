//! A file that holds several routines, one of them refused by the
//! conversion (a DDL trigger has no PostgreSQL form): the refused routine is
//! reported and the routines around it still meet their own conversion, with
//! the conversion's trace. A same-length edit of the output must then show up
//! as a difference in exactly the routine it was made in.

use sqldiff_convert::convert;
use sqldiff_core::equiv::Anchor;
use sqldiff_core::{diff, Artifact, Dialect, Options, Verdict};

const INPUT: &str = "CREATE TRIGGER dbo.ddl1 ON DATABASE FOR DDL_DATABASE_LEVEL_EVENTS AS
BEGIN
  SET NOCOUNT ON;
END;
GO
CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, n int NULL);
GO
CREATE PROCEDURE dbo.p1 @x int AS
BEGIN
  SET NOCOUNT ON;
  UPDATE dbo.t SET n = @x WHERE id = 1;
END;
GO
CREATE TRIGGER dbo.tr1 ON dbo.t AFTER INSERT AS
BEGIN
  UPDATE dbo.t SET n = 0 WHERE id IN (SELECT id FROM inserted);
END;
GO
CREATE PROCEDURE dbo.p2 @y int AS
BEGIN
  SET NOCOUNT ON;
  UPDATE dbo.t SET n = @y + 1 WHERE id = 2;
END;
GO
";

/// The engine's verdict for the input against `output`, with the
/// conversion's own trace as anchors (edits keep every length).
fn check(output: &str, anchors: &[Anchor]) -> sqldiff_core::Report {
    let opts = Options {
        catalog_a: vec![Artifact::new(
            "catalog.sql",
            Dialect::Tsql,
            "CREATE TABLE dbo.t (id int NOT NULL PRIMARY KEY, n int NULL);",
        )],
        catalog_b: vec![Artifact::new(
            "catalog.sql",
            Dialect::Postgres,
            "CREATE TABLE t (id integer NOT NULL PRIMARY KEY, n integer NULL);",
        )],
        anchors: anchors.to_vec(),
        ..Options::default()
    };
    diff(
        &Artifact::new("input.sql", Dialect::Tsql, INPUT),
        &Artifact::new("converted.sql", Dialect::Postgres, output),
        &opts,
    )
}

fn not_covered(r: &sqldiff_core::Report) -> Vec<&String> {
    r.unknown_reasons
        .iter()
        .filter(|x| x.contains("several routines") || x.contains("outside the routines"))
        .collect()
}

#[test]
fn a_refused_routine_does_not_shift_the_routines_around_it() {
    let c = convert(INPUT, Dialect::Tsql, Dialect::Postgres).unwrap();
    let anchors: Vec<Anchor> = c
        .units
        .iter()
        .map(|u| Anchor {
            a: (u.src.start, u.src.end),
            b: (u.out.start, u.out.end),
        })
        .collect();
    assert!(!anchors.is_empty());
    let same = check(&c.output, &anchors);
    assert_eq!(not_covered(&same), Vec::<&String>::new(), "{}", c.output);
    assert!(same.findings.is_empty(), "{:#?}", same.findings);
    // Only the refused DDL trigger (line 1) is left unknown.
    assert!(
        same.unknown_reasons
            .iter()
            .all(|x| x.contains("line 1:") || x.contains("line 1 ")),
        "{:?}",
        same.unknown_reasons
    );

    // Same-length edits of the output, one per routine after the refused one.
    for (from, to) in [
        ("SET n = p_x WHERE", "SET n = 1+2 WHERE"),
        ("SET n = 0 WHERE", "SET n = 1 WHERE"),
        ("p_y + 1", "p_y + 2"),
    ] {
        let edited = c.output.replacen(from, to, 1);
        assert_ne!(edited, c.output, "{from}: {}", c.output);
        assert_eq!(edited.len(), c.output.len(), "{from}");
        let r = check(&edited, &anchors);
        // Found as a difference, or at least no longer shown equal in that
        // routine: a reason or finding beyond the refused DDL trigger.
        let beyond = !r.findings.is_empty()
            || r.unknown_reasons
                .iter()
                .any(|x| !x.contains("line 1:") && !x.contains("line 1 "));
        assert!(beyond, "{from}: {:?}", r.unknown_reasons);
        assert_eq!(not_covered(&r), Vec::<&String>::new());
    }
}
