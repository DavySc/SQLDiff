use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use sqldiff_core::{Artifact, Dialect, Options};
use sqldiff_gui::server::{serve, Pair};
use sqldiff_gui::{build, page, Payload, Row, RowKind};

fn fixture_path(name: &str) -> String {
    format!("{}/../../tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(fixture_path(name)).unwrap()
}

fn view(a: &str, da: Dialect, b: &str, db: Dialect) -> Payload {
    build(
        &Artifact::new("a.sql", da, fixture(a)),
        &Artifact::new("b.sql", db, fixture(b)),
        &Options::default(),
        false,
    )
}

fn text(row: &Row, side_a: bool) -> String {
    let blk = if side_a { &row.a } else { &row.b };
    blk.as_ref()
        .map(|b| b.segs.iter().map(|s| s.0.as_str()).collect())
        .unwrap_or_default()
}

fn find<'a>(p: &'a Payload, needle: &str) -> &'a Row {
    p.view
        .rows
        .iter()
        .find(|r| text(r, true).contains(needle))
        .unwrap_or_else(|| panic!("no row with {needle}"))
}

#[test]
fn rows_reproduce_both_sources_exactly() {
    for (a, da, b, db) in [
        (
            "movestock.tsql.sql",
            Dialect::Tsql,
            "movestock.db2i.sql",
            Dialect::Db2i,
        ),
        (
            "movestock.tsql.sql",
            Dialect::Tsql,
            "movestock.pg.sql",
            Dialect::Postgres,
        ),
        (
            "catch_rethrow.tsql.sql",
            Dialect::Tsql,
            "movestock_reformatted.tsql.sql",
            Dialect::Tsql,
        ),
    ] {
        let p = view(a, da, b, db);
        let ta: String = p.view.rows.iter().map(|r| text(r, true)).collect();
        let tb: String = p.view.rows.iter().map(|r| text(r, false)).collect();
        assert_eq!(ta, fixture(a), "{a}");
        assert_eq!(tb, fixture(b), "{b}");
    }
}

#[test]
fn conversion_rows_align_by_statement() {
    let p = view(
        "movestock.tsql.sql",
        Dialect::Tsql,
        "movestock.db2i.sql",
        Dialect::Db2i,
    );
    let update = find(&p, "UPDATE Stock");
    assert_eq!(update.kind, RowKind::Same);
    assert!(text(update, false).contains("p_Qty"));
    let insert = find(&p, "INSERT INTO StockLog");
    assert_eq!(insert.kind, RowKind::Same);
    assert!(text(insert, false).contains("INSERT INTO StockLog"));
    assert_eq!(find(&p, "RETURN 1").kind, RowKind::OnlyA);
    assert_eq!(find(&p, "COMMIT;").kind, RowKind::OnlyA);
    // Each finding with a location is attached to the row holding it.
    for (i, f) in p.report.findings.iter().enumerate() {
        let attached = p.view.rows.iter().any(|r| {
            r.a.as_ref().is_some_and(|b| b.findings.contains(&i))
                || r.b.as_ref().is_some_and(|b| b.findings.contains(&i))
        });
        assert_eq!(attached, f.a.is_some() || f.b.is_some(), "{}", f.rule);
    }
}

#[test]
fn reformat_is_all_same_rows() {
    let p = view(
        "movestock.tsql.sql",
        Dialect::Tsql,
        "movestock_reformatted.tsql.sql",
        Dialect::Tsql,
    );
    assert!(p.report.findings.is_empty());
    for r in &p.view.rows {
        assert_eq!(
            r.kind,
            RowKind::Same,
            "{:?} / {:?}",
            text(r, true),
            text(r, false)
        );
    }
}

#[test]
fn page_embeds_payload_safely() {
    let p = build(
        &Artifact::new("a.sql", Dialect::Tsql, "SELECT '</script><b>';"),
        &Artifact::new("b.sql", Dialect::Tsql, "SELECT 1;"),
        &Options::default(),
        false,
    );
    let html = page(&p);
    assert!(!html.contains("/*__SQLDIFF_PAYLOAD__*/null"));
    assert_eq!(html.matches("</script>").count(), 1);
}

fn request(port: u16, raw: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(raw.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

#[test]
fn server_serves_page_and_rediffs_edits() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let pair = Pair {
        a: fixture_path("movestock.tsql.sql").into(),
        b: fixture_path("movestock.db2i.sql").into(),
        dialect_a: Dialect::Tsql,
        dialect_b: Dialect::Db2i,
        excerpt_lines: 3,
    };
    std::thread::spawn(move || serve(listener, pair));

    let res = request(port, "GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(res.starts_with("HTTP/1.1 200"));
    assert!(res.contains("txn.atomicity"));

    let body = serde_json::json!({ "a": "SELECT 1;", "b": "SELECT 1;" }).to_string();
    let res = request(
        port,
        &format!(
            "POST /api/diff HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    );
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    let json: serde_json::Value =
        serde_json::from_str(res.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(json["report"]["findings"].as_array().unwrap().len(), 0);
    assert_eq!(json["live"], true);

    assert!(request(port, "GET /nope HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 404"));
}
