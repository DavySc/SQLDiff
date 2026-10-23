use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use sqldiff_core::{Artifact, Dialect, Options};
use sqldiff_gui::review::{ReviewState, Store};
use sqldiff_gui::server::serve;
use sqldiff_gui::session::{Flip, Origin, Session};
use sqldiff_gui::{build, page, Payload, Row, RowKind};
use sqldiff_source::Resolved;

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

fn post(port: u16, path: &str, body: &serde_json::Value) -> (String, serde_json::Value) {
    let body = body.to_string();
    let res = request(
        port,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    );
    let json = serde_json::from_str(res.split("\r\n\r\n").nth(1).unwrap()).unwrap_or_default();
    (res, json)
}

fn pair_session() -> Session {
    Session::open(
        Origin::Files {
            a: fixture_path("movestock.tsql.sql").into(),
            b: fixture_path("movestock.db2i.sql").into(),
        },
        Some(Dialect::Tsql),
        Some(Dialect::Db2i),
        Resolved::builtin(),
        Store::in_memory(),
    )
    .unwrap()
}

/// Two trees: movestock in T-SQL vs Db2 (findings), and a reformat (clean).
fn tree_session(name: &str) -> (Session, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("sqldiff-gui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (a, b) = (dir.join("a/inv"), dir.join("b/inv"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(a.join("move_stock.tsql.sql"), fixture("movestock.tsql.sql")).unwrap();
    std::fs::write(b.join("move_stock.db2i.sql"), fixture("movestock.db2i.sql")).unwrap();
    std::fs::write(a.join("same.tsql.sql"), fixture("movestock.tsql.sql")).unwrap();
    std::fs::write(
        b.join("same.tsql.sql"),
        fixture("movestock_reformatted.tsql.sql"),
    )
    .unwrap();
    std::fs::write(a.join("lonely.tsql.sql"), "SELECT 1;").unwrap();
    let s = Session::open(
        Origin::detect(dir.join("a"), dir.join("b")),
        None,
        None,
        Resolved::builtin(),
        Store::open(&dir.join("data")).unwrap(),
    )
    .unwrap();
    (s, dir)
}

#[test]
fn queue_lists_tree_pairs_with_folder_and_review_state() {
    let (s, _dir) = tree_session("queue");
    let q = s.queue();
    assert_eq!(q.len(), 2, "{q:?}");
    assert!(q.iter().all(|e| e.folder == "inv"));
    let info = s.info();
    assert_eq!(info.only_a.len(), 1);
    assert!(info.only_a[0].ends_with("lonely.tsql.sql"));
    assert!(s
        .find("a/inv/move_stock.tsql.sql", "inv/move_stock.db2i.sql")
        .is_some());
}

#[test]
fn denying_an_assumption_drops_dependent_verdicts_to_unknown() {
    let mut s = pair_session();
    let base = s.report(0).unwrap();
    let info = s.info();
    let a = info
        .assumptions
        .iter()
        .find(|a| !a.pairs.is_empty())
        .expect("movestock relies on an assumption");
    let dependent: Vec<_> = base
        .findings
        .iter()
        .filter(|f| f.assumptions.contains(&a.id))
        .map(|f| f.fingerprint.clone())
        .collect();
    s.set_flip(&a.id, Flip::Denied);
    let r = s.report(0).unwrap();
    assert!(r.unknown_reasons.iter().any(|u| u.contains(&a.id)) || !dependent.is_empty());
    for f in &r.findings {
        if dependent.contains(&f.fingerprint) {
            assert_eq!(f.verdict, sqldiff_core::Verdict::Unknown);
        }
    }
    assert!(r.verdict >= sqldiff_core::Verdict::Unknown);
    assert_eq!(
        s.info()
            .assumptions
            .iter()
            .find(|x| x.id == a.id)
            .unwrap()
            .flip,
        Flip::Denied
    );
    s.set_flip(&a.id, Flip::Open);
    assert_eq!(s.report(0).unwrap(), base);
}

#[test]
fn review_decisions_persist_and_count_in_the_queue() {
    let (mut s, dir) = tree_session("review");
    let i = s.queue().iter().position(|e| e.findings > 0).unwrap();
    let p = s.payload(i, true).unwrap();
    let fp = p.report.findings[0].fingerprint.clone();
    s.review(
        &fp,
        &p.snapshot,
        Some(ReviewState::Accepted),
        Some("deliberate".into()),
    )
    .unwrap();
    let e = &s.queue()[i];
    assert_eq!((e.reviewed, e.accepted), (1, 1));
    // A new store over the same directory sees the decision.
    let store = Store::open(&dir.join("data")).unwrap();
    let (d, current) = store.lookup(&fp, &p.snapshot).unwrap();
    assert!(current);
    assert_eq!(d.comment.as_deref(), Some("deliberate"));
    assert!(store.lookup(&fp, "other:snapshot").is_some_and(|(_, c)| !c));
    s.review(&fp, &p.snapshot, None, None).unwrap();
    assert_eq!(s.queue()[i].reviewed, 0);
}

#[test]
fn redacted_report_hides_paths_and_identifiers() {
    let (s, dir) = tree_session("report");
    let plain = s.session_report(false);
    assert!(plain.contains("move_stock.tsql.sql"));
    let red = s.session_report(true);
    assert!(red.contains("redacted"));
    for leak in ["move_stock", "StockLog", dir.to_str().unwrap(), "lonely"] {
        assert!(!red.contains(leak), "{leak} leaked:\n{red}");
    }
    assert!(red.contains("object-"));
}

#[test]
fn page_embeds_session_and_static_export_carries_every_pair() {
    let (s, _dir) = tree_session("page");
    let p = s.page(None, false);
    assert!(p.pairs.iter().all(|x| x.is_some()));
    assert_eq!(p.open, None);
    let live = s.page(Some(1), true);
    assert!(live.pairs[1].is_some() && live.pairs[0].is_none());
    let html = page(&p);
    assert_eq!(html.matches("</script>").count(), 1);
}

#[test]
fn server_serves_page_and_rediffs_edits() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || serve(listener, pair_session()));

    let res = request(port, "GET / HTTP/1.1\r\nHost: x\r\n\r\n");
    assert!(res.starts_with("HTTP/1.1 200"));
    assert!(res.contains("txn.atomicity"));

    let (res, json) = post(
        port,
        "/api/diff",
        &serde_json::json!({ "pair": 0, "a": "SELECT 1;", "b": "SELECT 1;" }),
    );
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    assert_eq!(json["report"]["findings"].as_array().unwrap().len(), 0);
    assert_eq!(json["live"], true);

    let res = request(port, "GET /api/pair/0 HTTP/1.1\r\n\r\n");
    let pair: serde_json::Value =
        serde_json::from_str(res.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let fp = pair["report"]["findings"][0]["fingerprint"]
        .as_str()
        .unwrap();
    let snap = pair["snapshot"].as_str().unwrap();
    let (res, json) = post(
        port,
        "/api/review",
        &serde_json::json!({ "fingerprint": fp, "snapshot": snap, "state": "rejected", "comment": "fix" }),
    );
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    assert_eq!(json["queue"][0]["reviewed"], 1);

    let id = json["session"]["assumptions"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, json) = post(
        port,
        "/api/assumption",
        &serde_json::json!({ "id": id, "flip": "denied" }),
    );
    assert_eq!(json["session"]["assumptions"][0]["flip"], "denied");

    let res = request(port, "GET /api/report?redact=1 HTTP/1.1\r\n\r\n");
    assert!(res.starts_with("HTTP/1.1 200") && res.contains("redacted"));
    assert!(request(
        port,
        "GET /?a=movestock.tsql.sql&b=movestock.db2i.sql HTTP/1.1\r\n\r\n"
    )
    .contains("\"open\":0"));
    assert!(request(port, "GET /nope HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 404"));
}
