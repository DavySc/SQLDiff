use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use sqldiff_core::{Artifact, Dialect, Options};
use sqldiff_gui::review::{State, Store};
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
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    );
    let json = serde_json::from_str(res.split("\r\n\r\n").nth(1).unwrap()).unwrap_or_default();
    (res, json)
}

/// Built-in config with the data directory (review store, redaction salt) in `dir`.
fn cfg(dir: &std::path::Path) -> Resolved {
    let mut c = Resolved::builtin();
    c.data_dir = dir.join("data");
    c
}

fn pair_session() -> Session {
    let dir = std::env::temp_dir().join(format!("sqldiff-gui-pair-{}", std::process::id()));
    Session::open(
        Origin::Files {
            a: fixture_path("movestock.tsql.sql").into(),
            b: fixture_path("movestock.db2i.sql").into(),
        },
        Some(Dialect::Tsql),
        Some(Dialect::Db2i),
        cfg(&dir),
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
        cfg(&dir),
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
    assert!(r.verdict() >= sqldiff_core::Verdict::Unknown);
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
        Some(State::Accepted),
        Some("deliberate".into()),
    )
    .unwrap();
    let e = &s.queue()[i];
    assert_eq!((e.reviewed, e.accepted), (1, 1));
    // A new store over the same directory sees the decision.
    let store = Store::open(&dir.join("data")).unwrap();
    let d = store.get(&fp, &p.snapshot).unwrap();
    assert_eq!(d.comment.as_deref(), Some("deliberate"));
    assert!(store.get(&fp, "other:snapshot").is_none());
    // Decisions of fingerprints outside the session stay out of the page.
    let mut other = Store::open(&dir.join("data")).unwrap();
    other
        .put(
            "0123456789abcdef",
            "x:y",
            sqldiff_gui::review::Decision::new(State::Rejected),
        )
        .unwrap();
    s.refresh().unwrap();
    let pl = s.payload(i, true).unwrap();
    assert_eq!(pl.review.len(), pl.report.findings.len());
    assert_eq!(pl.review[0].state, sqldiff_review::ReviewState::Accepted);
    assert!(!serde_json::to_string(&s.page(Some(i), true))
        .unwrap()
        .contains("0123456789abcdef"));
    s.review(&fp, &p.snapshot, None, None).unwrap();
    assert_eq!(s.queue()[i].reviewed, 0);
}

#[test]
fn redacted_report_hides_paths_and_identifiers() {
    let (s, dir) = tree_session("report");
    let plain = s.session_report(false).unwrap();
    assert!(plain.contains("move_stock.tsql.sql"));
    let red = s.session_report(true).unwrap();
    assert!(red.contains("redacted"));
    for leak in ["move_stock", "StockLog", dir.to_str().unwrap(), "lonely"] {
        assert!(
            !red.to_lowercase().contains(&leak.to_lowercase()),
            "{leak} leaked:\n{red}"
        );
    }
    assert!(red.contains("PATH_") || red.contains("OBJ_"));
}

#[test]
fn redaction_covers_unicode_and_hyphenated_names() {
    let dir = std::env::temp_dir().join(format!("sqldiff-gui-uni-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join("order-sync.tsql.sql");
    let b = dir.join("order-sync.pg.sql");
    std::fs::write(
        &a,
        "CREATE PROCEDURE [order-sync] AS\nBEGIN\n  UPDATE Lager SET Größe = 1;\n  COMMIT;\nEND\n",
    )
    .unwrap();
    std::fs::write(&b, "CREATE PROCEDURE \"order-sync\"()\nLANGUAGE plpgsql AS $$\nBEGIN\n  UPDATE Lager SET Größe = 2;\nEND\n$$;\n").unwrap();
    let s = Session::open(
        Origin::detect(a, b),
        None,
        None,
        cfg(&dir),
        Store::in_memory(),
    )
    .unwrap();
    let red = s.session_report(true).unwrap();
    for leak in ["order-sync", "Größe", "Lager"] {
        assert!(!red.contains(leak), "{leak} leaked:\n{red}");
    }
}

#[test]
fn redacted_report_does_not_trip_on_its_own_template_words() {
    let dir = std::env::temp_dir().join(format!("sqldiff-gui-tpl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join("config.tsql.sql");
    let b = dir.join("config.pg.sql");
    std::fs::write(
        &a,
        "CREATE PROCEDURE Session AS\nBEGIN\n  UPDATE Verdict SET Pair = 1;\n  COMMIT;\nEND\n",
    )
    .unwrap();
    std::fs::write(&b, "CREATE PROCEDURE session()\nLANGUAGE plpgsql AS $$\nBEGIN\n  UPDATE verdict SET pair = 2;\nEND\n$$;\n").unwrap();
    let s = Session::open(
        Origin::detect(a, b),
        None,
        None,
        cfg(&dir),
        Store::in_memory(),
    )
    .unwrap();
    let red = s
        .session_report(true)
        .expect("template words are not leaks");
    assert!(red.contains("- config:"));
    assert!(!red.contains("config.tsql.sql"));
}

#[test]
fn edited_panes_keep_disk_decisions_and_foreign_snapshots_are_refused() {
    let (mut s, _dir) = tree_session("scratch");
    let i = s.queue().iter().position(|e| e.findings > 0).unwrap();
    let p = s.payload(i, true).unwrap();
    let fp = p.report.findings[0].fingerprint.clone();
    s.review(&fp, &p.snapshot, Some(State::Accepted), None)
        .unwrap();
    // Edit side A a little: the finding survives, and so does its decision.
    let edited = format!("-- note\n{}", p.sources[0]);
    let e = s.scratch(i, edited, p.sources[1].clone()).unwrap();
    assert_ne!(e.snapshot, p.snapshot);
    let k = e
        .report
        .findings
        .iter()
        .position(|f| f.fingerprint == fp)
        .unwrap();
    assert_eq!(e.review[k].state, sqldiff_review::ReviewState::Accepted);
    // Decisions only go on the files' own snapshots (or `*`).
    assert!(s
        .review(&fp, &e.snapshot, Some(State::Rejected), None)
        .is_err());
    assert!(s.review(&fp, "*", Some(State::Rejected), None).is_ok());
}

#[test]
fn wildcard_bind_answers_its_own_address() {
    let listener = TcpListener::bind("0.0.0.0:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || serve(listener, pair_session()));
    for host in ["0.0.0.0", "127.0.0.1", "localhost"] {
        let res = request(
            port,
            &format!("GET /api/session HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n"),
        );
        assert!(res.starts_with("HTTP/1.1 200"), "{host}: {res}");
    }
    let res = request(
        port,
        &format!("GET /api/session HTTP/1.1\r\nHost: rebind.example:{port}\r\n\r\n"),
    );
    assert!(res.starts_with("HTTP/1.1 403"));
}

#[test]
fn deep_links_match_whole_path_components() {
    let (s, _dir) = tree_session("find");
    assert!(s
        .find("move_stock.tsql.sql", "move_stock.db2i.sql")
        .is_some());
    assert!(s.find("stock.tsql.sql", "move_stock.db2i.sql").is_none());
    assert!(s.find("e.tsql.sql", "e.tsql.sql").is_none());
    assert!(s.find("inv/same.tsql.sql", "same.tsql.sql").is_some());
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

    let host = format!("Host: 127.0.0.1:{port}\r\n");
    let get = |path: &str| request(port, &format!("GET {path} HTTP/1.1\r\n{host}\r\n"));
    let res = get("/");
    assert!(res.starts_with("HTTP/1.1 200"));
    assert!(res.contains("txn.atomicity"));

    let (res, json) = post(
        port,
        "/api/diff",
        // Side B is Db2 for i, where a subselect needs FROM.
        &serde_json::json!({ "pair": 0, "a": "SELECT 1;", "b": "SELECT 1 FROM SYSIBM.SYSDUMMY1;" }),
    );
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    assert_eq!(json["report"]["findings"].as_array().unwrap().len(), 0);
    assert_eq!(json["live"], true);

    let res = get("/api/pair/0");
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

    let res = get("/api/report?redact=1");
    assert!(res.starts_with("HTTP/1.1 200") && res.contains("redacted"));
    assert!(get("/?a=movestock.tsql.sql&b=movestock.db2i.sql").contains("\"open\":0"));
    assert!(get("/nope").starts_with("HTTP/1.1 404"));

    // DNS rebinding: a foreign Host is refused, even for reads.
    assert!(request(
        port,
        "GET /api/pair/0 HTTP/1.1\r\nHost: evil.example\r\n\r\n"
    )
    .starts_with("HTTP/1.1 403"));
    assert!(request(port, "GET / HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 403"));
    // CSRF: a cross-origin or non-JSON POST does not write.
    let body =
        serde_json::json!({ "fingerprint": fp, "snapshot": snap, "state": "accepted" }).to_string();
    let raw = |extra: &str, ctype: &str| {
        request(
            port,
            &format!(
                "POST /api/review HTTP/1.1\r\n{host}{extra}Content-Type: {ctype}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
    };
    assert!(raw("Origin: https://evil.example\r\n", "application/json").starts_with("HTTP/1.1 403"));
    assert!(raw("Sec-Fetch-Site: cross-site\r\n", "application/json").starts_with("HTTP/1.1 403"));
    assert!(raw("", "text/plain").starts_with("HTTP/1.1 415"));
    assert!(raw(
        &format!("Origin: http://127.0.0.1:{port}\r\n"),
        "application/json"
    )
    .starts_with("HTTP/1.1 200"));
}

#[test]
fn convert_mode_is_served_beside_the_pair_view() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || serve(listener, pair_session()));
    let page = request(
        port,
        &format!("GET /convert HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    );
    assert!(page.starts_with("HTTP/1.1 200"), "{page}");
    assert!(page.contains(r#""pair_view":true"#));
    let (res, v) = post(
        port,
        "/api/convert",
        &serde_json::json!({ "text": "SELECT TOP 1 a FROM t;", "to": "postgres" }),
    );
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    assert_eq!(v["conversion"]["output"], "SELECT a FROM t\nLIMIT 1;\n");
}

const TPL_A: &str = "update [TAB] set [qty] = 0;\n";
const TPL_B: &str = "update [TAB] set qty = 0;\n";

/// A pack with the `[template]` syntax and two template files: `[TAB]` is a
/// parameter; in T-SQL `[qty]` is a quoted column.
fn template_dir(name: &str) -> (Resolved, std::path::PathBuf) {
    template_pack(
        name,
        ("[", "]"),
        [("upd.tsql.sql", TPL_A), ("upd.db2i.sql", TPL_B)],
    )
}

/// A pack with these placeholder delimiters and two template files.
fn template_pack(
    name: &str,
    (open, close): (&str, &str),
    files: [(&str, &str); 2],
) -> (Resolved, std::path::PathBuf) {
    use sqldiff_source::config::Sources;
    let dir = std::env::temp_dir().join(format!("sqldiff-gui-tpl-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("pack.toml"),
        format!(
            "[pack]\nname = \"t\"\nversion = \"1\"\n\n[template]\nopen = {open:?}\nclose = {close:?}\n\
             name_pattern = \"[A-Z][A-Z0-9_]*\"\n"
        ),
    )
    .unwrap();
    for (f, text) in files {
        std::fs::write(dir.join(f), text).unwrap();
    }
    let mut c = Resolved::load(&Sources {
        isolated: true,
        packs: vec![dir.clone()],
        ..Sources::default()
    })
    .unwrap();
    c.data_dir = dir.join("data");
    (c, dir)
}

/// A template-mode session over the two files of [`template_pack`].
fn templates_session(
    name: &str,
    delimiters: (&str, &str),
    files: [(&str, &str); 2],
    known: &[&str],
) -> Session {
    use sqldiff_gui::session::TemplateMode;
    let (c, dir) = template_pack(name, delimiters, files);
    let template = TemplateMode {
        placeholders: sqldiff_source::tree::placeholders(&c).unwrap(),
        known: Some(known.iter().map(|s| s.to_string()).collect()),
    };
    Session::open_with(
        Origin::detect(dir.join(files[0].0), dir.join(files[1].0)),
        None,
        None,
        c,
        Some(template),
        Store::in_memory(),
    )
    .unwrap()
}

fn template_session(name: &str, known: Option<&[&str]>) -> Session {
    use sqldiff_gui::session::TemplateMode;
    let (c, dir) = template_dir(name);
    let template = TemplateMode {
        placeholders: sqldiff_source::tree::placeholders(&c).unwrap(),
        known: known.map(|k| k.iter().map(|s| s.to_string()).collect()),
    };
    Session::open_with(
        Origin::detect(dir.join("upd.tsql.sql"), dir.join("upd.db2i.sql")),
        None,
        None,
        c,
        Some(template),
        Store::in_memory(),
    )
    .unwrap()
}

fn rules(r: &sqldiff_core::Report) -> Vec<String> {
    let mut v: Vec<String> = r.findings.iter().map(|f| f.rule.clone()).collect();
    v.sort();
    v
}

#[test]
fn template_mode_diffs_fragments_everywhere_in_the_session() {
    let s = template_session("mode", Some(&["TAB"]));
    let r = s.report(0).unwrap();
    // As the CLI's template diff: the same SQL once `[TAB]` is a placeholder,
    // and the T-SQL `[qty]` a quoted name, not a placeholder.
    let (c, _) = template_dir("mode-cli");
    let (a, b) = (
        Artifact::new("a", Dialect::Tsql, TPL_A),
        Artifact::new("b", Dialect::Db2i, TPL_B),
    );
    let tab = ["TAB".to_string()].into();
    let cli = sqldiff_source::tree::template_diff(
        &a,
        &b,
        [Some(&tab), Some(&tab)],
        &c,
        &Options::default(),
    )
    .unwrap();
    assert_eq!(r.verdict(), cli.verdict());
    assert_eq!(rules(&r), rules(&cli));
    // Read as plain SQL, the same pair reports otherwise.
    let plain = sqldiff_source::tree::diff(&a, &b, &c);
    assert!(
        rules(&plain) != rules(&cli) || plain.verdict() != cli.verdict(),
        "{:?}",
        rules(&plain)
    );
    // Scratch edits re-diff as templates too.
    let p = s.scratch(0, TPL_A.into(), TPL_B.into()).unwrap();
    assert_eq!(rules(&p.report), rules(&cli));
    // The page and the session report say which mode this is.
    assert_eq!(s.info().template, Some(vec!["TAB".to_string()]));
    let md = s.session_report(false).unwrap();
    assert!(md.contains("- mode: templates, parameters TAB"), "{md}");
    let md = s.session_report(true).unwrap();
    assert!(
        md.contains("- mode: templates, 1 given parameter(s)"),
        "{md}"
    );
}

#[test]
fn template_mode_without_parameters_uses_the_pack_rules() {
    let s = template_session("rules", None);
    assert_eq!(s.info().template, Some(Vec::new()));
    // Without a parameter list the pattern decides, and the report says so.
    let r = s.report(0).unwrap();
    assert!(
        r.unknown_reasons
            .iter()
            .any(|x| x.contains("taken as placeholders by the name pattern")),
        "{:?}",
        r.unknown_reasons
    );
    assert!(s.overlay(0).is_some());
    assert_eq!(pair_session().info().template, None);
}

/// p01 with its tables as `${…}` placeholders: each is a byte shorter than
/// its stand-in (`TPL_LOG`), so fragment offsets drift from the template's.
fn p01_templates() -> [String; 2] {
    let corpus = format!(
        "{}/../../corpus/pairs/p01-move-stock",
        env!("CARGO_MANIFEST_DIR")
    );
    ["tsql.sql", "pg.sql"].map(|f| {
        std::fs::read_to_string(format!("{corpus}/{f}"))
            .unwrap()
            .replace("stock_log", "${LOG}")
            .replace("stock SET", "${STOCK} SET")
    })
}

/// A location's excerpt is what the template has at its offsets.
fn in_template(loc: &sqldiff_core::model::Location, template: &str) {
    let at = &template[loc.byte_start..loc.byte_end];
    assert_eq!(
        loc.excerpt.first().map(String::as_str),
        at.lines().next().map(str::trim_end),
        "{loc:?}"
    );
    let line_start = template[..loc.byte_start].rfind('\n').map_or(0, |i| i + 1);
    assert_eq!(loc.start_col, loc.byte_start - line_start + 1, "{loc:?}");
    assert_eq!(
        loc.start_line,
        template[..loc.byte_start].matches('\n').count() + 1
    );
}

#[test]
fn template_what_if_answers_in_template_lines_and_names() {
    use sqldiff_core::ir::ErrorKind;
    use sqldiff_core::model::Side;
    let [ta, tb] = p01_templates();
    let s = templates_session(
        "whatif",
        ("${", "}"),
        [("move.tsql.sql", &ta), ("move.pg.sql", &tb)],
        &["LOG", "STOCK"],
    );
    // Line 7 of the T-SQL template is the INSERT into the log table.
    let w = s.what_if(0, Side::A, 7, ErrorKind::Constraint).unwrap();
    assert_eq!(
        w.scenario.statement.as_deref(),
        Some("the INSERT into ${LOG}")
    );
    assert_eq!(w.scenario.line_a, Some(7));
    assert!(!w.findings.is_empty());
    let json = serde_json::to_string(&w).unwrap();
    assert!(!json.contains("TPL_"), "{json}");
    for f in &w.findings {
        for loc in [&f.a, &f.b].into_iter().flatten() {
            in_template(loc, if loc.side == Side::A { &ta } else { &tb });
        }
    }
    // The same finding in the pair view sits at the same place.
    let p = s.payload(0, true).unwrap();
    for f in &w.findings {
        if let Some(g) = p
            .report
            .findings
            .iter()
            .find(|g| g.fingerprint == f.fingerprint)
        {
            assert_eq!(
                f.a.as_ref().map(|l| (l.byte_start, l.byte_end)),
                g.a.as_ref().map(|l| (l.byte_start, l.byte_end))
            );
        }
    }
    // Brackets are in template lines too: BEGIN TRAN on line 4.
    let o = s.overlay(0).unwrap();
    assert!(o.a.iter().any(|b| b.start_line == 4), "{:?}", o.a);
    assert!(!serde_json::to_string(&o).unwrap().contains("TPL_"));
}

#[test]
fn redacted_template_report_hides_names_between_dollar_placeholders() {
    // As SQL, `$TAB$ … $ID$` is a dollar-quoted string: the names inside it
    // must still be known to the redactor.
    let a = "CREATE FUNCTION f() RETURNS void LANGUAGE sql AS 'UPDATE x SET y = 1';\n\
             UPDATE $TAB$ SET secretcol = 0 WHERE $ID$ = 1;\n";
    let b = "CREATE FUNCTION f() RETURNS void LANGUAGE sql AS 'UPDATE x SET y = 1';\n\
             UPDATE $TAB$ SET secretcol = 1, hiddencol = 2 WHERE $ID$ = 1;\n";
    let s = templates_session(
        "redact",
        ("$", "$"),
        [("upd.pg.sql", a), ("upd2.pg.sql", b)],
        &["TAB", "ID"],
    );
    let plain = s.session_report(false).unwrap();
    assert!(plain.contains("hiddencol"), "{plain}");
    let red = s.session_report(true).unwrap();
    for leak in ["secretcol", "hiddencol", "TAB$", "$ID"] {
        assert!(!red.contains(leak), "{leak} leaked:\n{red}");
    }
}

#[test]
fn template_pair_view_blocks_follow_the_fragment() {
    // Parsed raw, `$T$ set a = 1;\nupdate $T$` is one dollar-quoted string:
    // the first two statements would run together into one block.
    let a = "update $T$ set a = 1;\nupdate $T$ set b = 2;\nupdate $U$ set c = 3;\n";
    let b = "update $T$ set a = 1;\nupdate $T$ set b = 5;\nupdate $U$ set c = 3;\n";
    let s = templates_session(
        "blocks",
        ("$", "$"),
        [("v.pg.sql", a), ("w.pg.sql", b)],
        &["T", "U"],
    );
    let p = s.payload(0, true).unwrap();
    // One block per statement, shown in the template's own text.
    assert_eq!(p.view.rows.len(), 3, "{:#?}", p.view.rows);
    let all: String = p.view.rows.iter().map(|r| text(r, true)).collect();
    assert_eq!(all, a);
    let all: String = p.view.rows.iter().map(|r| text(r, false)).collect();
    assert_eq!(all, b);
    assert_eq!(p.sources, [a.to_string(), b.to_string()]);
    let kinds: Vec<RowKind> = p.view.rows.iter().map(|r| r.kind).collect();
    assert_eq!(kinds, [RowKind::Same, RowKind::Changed, RowKind::Same]);
    // Every finding sits in the block of its statement: the second.
    assert!(!p.report.findings.is_empty());
    for (i, r) in p.view.rows.iter().enumerate() {
        for blk in [&r.a, &r.b].into_iter().flatten() {
            if i == 1 {
                assert!(!blk.findings.is_empty(), "{blk:?}");
            } else {
                assert!(blk.findings.is_empty(), "{blk:?}");
            }
        }
    }
    // Edits re-diff the same way.
    let e = s.scratch(0, a.into(), b.into()).unwrap();
    assert_eq!(e.view.rows.len(), 3);
}
