//! The app (`app::serve`): every view behind one server, each page framed
//! by the rail with its API mounted under the view's path.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;

use sqldiff_convert::tree;
use sqldiff_core::Dialect;
use sqldiff_gui::app::{serve, App};
use sqldiff_gui::convert::{Setup, SAMPLE};
use sqldiff_gui::convert_review::{self, Review};
use sqldiff_gui::review::Store;
use sqldiff_gui::session::{Origin, Session};
use sqldiff_source::Resolved;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let d = std::env::temp_dir().join(format!("sqldiff-gui-app-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let p = self.0.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        p
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn sample() -> Setup {
    Setup {
        from: Dialect::Tsql,
        to: Dialect::Postgres,
        template: None,
        text: SAMPLE.to_string(),
        name: String::new(),
    }
}

fn session(d: &Dir) -> Session {
    let a = d.write("a.sql", "SELECT a FROM t WHERE b = 1;\n");
    let b = d.write("b.sql", "SELECT a FROM t WHERE b = 2;\n");
    let mut cfg = Resolved::builtin();
    cfg.data_dir = d.0.join("data");
    Session::open(
        Origin::Files { a, b },
        Some(Dialect::Tsql),
        Some(Dialect::Tsql),
        cfg,
        Store::in_memory(),
    )
    .unwrap()
}

/// A tree converted as `sqldiff convert` does, opened for review.
fn review(d: &Dir) -> Review {
    d.write("src/p.sql", "CREATE TABLE t (a int NOT NULL);\n");
    let args = tree::Args {
        src: d.0.join("src"),
        from: Dialect::Tsql,
        to: Dialect::Postgres,
        jobs: Some(1),
        no_catalog: false,
        template: None,
        extensions: tree::extensions(Vec::new()),
        helpers: Vec::new(),
        deployment: Default::default(),
        catalogs: Vec::new(),
        catalogs_b: Vec::new(),
    };
    let out = d.0.join("out");
    let (inputs, unreadable) = tree::read(&args).unwrap();
    tree::prepare_out(&args.src, &out, false).unwrap();
    let t = tree::convert(&args, &inputs, unreadable, Some(&out)).unwrap();
    tree::write(&t, &out).unwrap();
    Review::open(convert_review::Setup {
        tree: args,
        out,
        instances: None,
        packs: Vec::new(),
    })
    .unwrap()
}

fn app(compare: Option<Session>, review: Option<Review>) -> App {
    App {
        convert: sample(),
        compare,
        review,
        data_dir: None,
        isolated: true,
        packs: Vec::new(),
    }
}

fn start(app: App) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || serve(listener, app, &[]));
    addr
}

/// Status line and body; a POST carries this launch's token, as the pages
/// send it.
fn request(addr: SocketAddr, method: &str, path: &str, body: &str) -> (String, String) {
    let token = (method == "POST").then(|| token(addr));
    request_with(addr, method, path, body, token.as_deref())
}

/// The token the pages of the app at `addr` carry.
fn token(addr: SocketAddr) -> String {
    let (status, page) = request_with(addr, "GET", "/settings", "", None);
    assert!(status.ends_with("200 OK"), "{status}");
    let at = page
        .find(r#"<meta name="sqldiff-token" content=""#)
        .expect("token in the page")
        + 36;
    page[at..at + page[at..].find('"').unwrap()].to_string()
}

/// [`request`] with `token`, if any, as the `X-SQLDiff-Token` header.
fn request_with(
    addr: SocketAddr,
    method: &str,
    path: &str,
    body: &str,
    token: Option<&str>,
) -> (String, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    let token = token.map_or(String::new(), |t| format!("X-SQLDiff-Token: {t}\r\n"));
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n{token}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    let (head, body) = resp.split_once("\r\n\r\n").unwrap();
    (head.lines().next().unwrap().to_string(), body.to_string())
}

fn get(addr: SocketAddr, path: &str) -> (String, String) {
    request(addr, "GET", path, "")
}

/// The rail's links (views that are open) and the marked current view.
fn rail(html: &str) -> (Vec<String>, String) {
    let nav = &html[html.find(r#"<nav id="app-rail""#).expect("rail")..];
    let nav = &nav[..nav.find("</nav>").unwrap()];
    let links = nav
        .split("<a ")
        .skip(1)
        .map(|a| {
            a.split('"')
                .nth(if a.starts_with("class") { 3 } else { 1 })
                .unwrap()
                .to_string()
        })
        .collect();
    let on = nav.split(r#"<a class="on" href=""#).nth(1).unwrap();
    (links, on[..on.find('"').unwrap()].to_string())
}

#[test]
fn every_view_is_served_by_one_server_behind_the_rail() {
    let d = Dir::new("all");
    let addr = start(app(Some(session(&d)), Some(review(&d))));
    for (path, base) in [
        ("/", None),
        ("/runs", None),
        ("/convert", None),
        ("/compare", Some("/compare")),
        ("/review", Some("/review")),
        ("/settings", None),
    ] {
        let (status, html) = get(addr, path);
        assert_eq!(status, "HTTP/1.1 200 OK", "{path}");
        let (links, on) = rail(&html);
        assert_eq!(
            links,
            ["/", "/runs", "/compare", "/convert", "/review", "/settings"],
            "{path}"
        );
        assert_eq!(on, path);
        assert!(!html.contains("<!--__SQLDIFF_SHELL__-->"), "{path}");
        if let Some(base) = base {
            assert!(
                html.contains(&format!(r#"const BASE = /*__SQLDIFF_BASE__*/"{base}";"#)),
                "{path}"
            );
        }
    }
    // Home lists what is open.
    let (_, home) = get(addr, "/");
    assert!(home.contains(r#""kind":"files""#), "{home}");
    assert!(home.contains(r#""to":"PostgreSQL""#), "{home}");
}

#[test]
fn each_view_answers_its_api_under_its_own_path() {
    let d = Dir::new("api");
    let addr = start(app(Some(session(&d)), Some(review(&d))));
    // The pair view and the review both have /api/refresh; each answers its own.
    let (status, body) = request(addr, "POST", "/compare/api/refresh", "{}");
    assert_eq!(status, "HTTP/1.1 200 OK", "{body}");
    assert!(body.contains(r#""queue""#), "{body}");
    let (status, body) = request(addr, "POST", "/review/api/refresh", "{}");
    assert_eq!(status, "HTTP/1.1 200 OK", "{body}");
    assert!(body.contains(r#""ledger_path""#), "{body}");
    let (status, body) = get(addr, "/compare/api/pair/0");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(body.contains(r#""report""#));
    let (status, body) = get(addr, "/review/api/state");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(body.starts_with('{'));
    // Convert mode keeps its API at the root.
    let (status, body) = request(addr, "POST", "/api/convert", r#"{"text":"SELECT 1;"}"#);
    assert_eq!(status, "HTTP/1.1 200 OK", "{body}");
    // Nothing is served from the views' unmounted paths.
    assert_eq!(get(addr, "/api/session").0, "HTTP/1.1 404 Not Found");
    assert_eq!(get(addr, "/api/state").0, "HTTP/1.1 404 Not Found");
    assert_eq!(get(addr, "/compare/elsewhere").0, "HTTP/1.1 404 Not Found");
}

#[test]
fn a_view_with_nothing_open_is_not_a_link_and_shows_home() {
    let addr = start(app(None, None));
    let (_, html) = get(addr, "/convert");
    let (links, on) = rail(&html);
    assert_eq!(links, ["/", "/runs", "/convert", "/settings"]);
    assert_eq!(on, "/convert");
    assert!(html.contains(r#"<span class="off" title="Compare two files or folders: nothing open"#));
    let (status, html) = get(addr, "/review");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(rail(&html).1, "/");
    assert!(html.contains(r#""review":null"#));
    assert_eq!(
        request(addr, "POST", "/review/api/refresh", "{}").0,
        "HTTP/1.1 404 Not Found"
    );
}

#[test]
fn cross_origin_posts_are_refused_on_every_view() {
    let d = Dir::new("csrf");
    let addr = start(app(Some(session(&d)), Some(review(&d))));
    for path in [
        "/api/convert",
        "/compare/api/refresh",
        "/review/api/refresh",
    ] {
        let mut s = TcpStream::connect(addr).unwrap();
        write!(
            s,
            "POST {path} HTTP/1.1\r\nHost: {addr}\r\nOrigin: https://evil.example\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}"
        )
        .unwrap();
        let mut resp = String::new();
        s.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 403"), "{path}: {resp}");
    }
}

/// Every file below `d` with its contents, to tell that nothing was written.
fn snapshot(d: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut todo = vec![d.to_path_buf()];
    while let Some(p) = todo.pop() {
        for e in std::fs::read_dir(&p).unwrap() {
            let e = e.unwrap().path();
            if e.is_dir() {
                todo.push(e.clone());
                out.push((e, Vec::new()));
            } else {
                out.push((e.clone(), std::fs::read(&e).unwrap()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn every_post_needs_this_launchs_token_and_writes_nothing_without_it() {
    let d = Dir::new("token");
    d.write("ws/sqldiff.toml", "[format]\nindent = 2\n");
    let addr = start(app(Some(session(&d)), Some(review(&d))));
    // A run that exists, so its cancel and review routes are reached.
    let spec = serde_json::json!({
        "src": d.0.join("src"), "out": d.0.join("run1"), "from": "tsql", "to": "postgres",
    });
    let (status, body) = request(addr, "POST", "/api/runs", &spec.to_string());
    assert_eq!(status, "HTTP/1.1 200 OK", "{body}");
    let run = finished(addr, 1);
    assert_eq!(run["state"], "done", "{run}");
    let before = snapshot(&d.0);
    let (_, runs_before) = get(addr, "/api/runs");
    let src = d.0.join("src");
    let posts = [
        (
            "/api/open/compare",
            serde_json::json!({"a": d.0.join("a.sql"), "b": d.0.join("b.sql")}),
        ),
        (
            "/api/runs",
            serde_json::json!({"src": src, "out": d.0.join("o1"), "from": "tsql", "to": "postgres"}),
        ),
        (
            "/api/runs/instances",
            serde_json::json!({"src": src, "out": d.0.join("o2")}),
        ),
        (
            "/api/runs/drift",
            serde_json::json!({"src": src, "out": d.0.join("o3")}),
        ),
        (
            "/api/runs/changes",
            serde_json::json!({"src": src, "out": d.0.join("o4")}),
        ),
        ("/runs/1/cancel", serde_json::json!({})),
        ("/runs/1/review/api/record", serde_json::json!({})),
        ("/runs/1/review/api/refresh", serde_json::json!({})),
        ("/review/api/record", serde_json::json!({})),
        ("/review/api/refresh", serde_json::json!({})),
        ("/compare/api/review", serde_json::json!({})),
        ("/compare/api/assumption", serde_json::json!({})),
        ("/compare/api/refresh", serde_json::json!({})),
        ("/compare/api/diff", serde_json::json!({})),
        (
            "/api/convert",
            serde_json::json!({"text": "SELECT 1;", "from": "tsql", "to": "postgres"}),
        ),
        (
            "/api/compare",
            serde_json::json!({"a": "SELECT 1;", "b": "SELECT 2;", "from": "tsql", "to": "postgres"}),
        ),
        (
            "/api/settings/preview",
            serde_json::json!({"dir": d.0.join("ws"), "draft": {}, "dialect": "tsql", "text": "SELECT 1;"}),
        ),
        (
            "/api/settings/save",
            serde_json::json!({"dir": d.0.join("ws"), "draft": {"indent": 8}, "hash": null}),
        ),
    ];
    let other = token(start(app(None, None)));
    for (path, body) in &posts {
        for t in [
            None,
            Some(String::new()),
            Some("0".repeat(32)),
            Some(other.clone()),
        ] {
            let (status, text) = request_with(addr, "POST", path, &body.to_string(), t.as_deref());
            assert_eq!(status, "HTTP/1.1 403 Forbidden", "{path} {t:?}: {text}");
            assert!(text.contains("token"), "{path}: {text}");
        }
    }
    assert!(snapshot(&d.0) == before, "a refused POST wrote to disk");
    assert_eq!(get(addr, "/api/runs").1, runs_before);
    // The token is on every page the app serves, and only there.
    for view in [
        "/",
        "/runs",
        "/compare",
        "/convert",
        "/review",
        "/runs/1/review",
        "/settings",
    ] {
        let (status, html) = get(addr, view);
        assert_eq!(status, "HTTP/1.1 200 OK", "{view}");
        assert!(
            html.contains(&format!(
                r#"<meta name="sqldiff-token" content="{}">"#,
                token(addr)
            )),
            "{view}"
        );
    }
    assert!(!sqldiff_gui::page(&serde_json::Value::Null).contains(r#"<meta name="sqldiff-token""#));
}

#[test]
fn the_static_pages_keep_their_own_api_paths() {
    // Outside the app (an export, the single-view servers) nothing is mounted.
    let html = sqldiff_gui::page(&serde_json::Value::Null);
    assert!(html.contains(r#"const BASE = /*__SQLDIFF_BASE__*/"";"#));
    assert!(!html.contains("app-rail"));
}

#[test]
fn the_picker_lists_folders_first_without_hidden_names() {
    let d = Dir::new("fs");
    d.write("b.sql", "");
    d.write("A.txt", "");
    d.write(".hidden", "");
    d.write("zeta/x.sql", "");
    d.write("Alpha/y.sql", "");
    let addr = start(app(None, None));
    let q = |p: &std::path::Path| {
        format!(
            "/api/fs?path={}",
            p.display().to_string().replace('%', "%25")
        )
    };
    let (status, body) = get(addr, &q(&d.0));
    assert_eq!(status, "HTTP/1.1 200 OK", "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let names: Vec<(&str, bool)> = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["name"].as_str().unwrap(), e["dir"].as_bool().unwrap()))
        .collect();
    assert_eq!(
        names,
        [
            ("Alpha", true),
            ("zeta", true),
            ("A.txt", false),
            ("b.sql", false)
        ]
    );
    assert_eq!(v["path"], d.0.display().to_string());
    assert_eq!(v["parent"], d.0.parent().unwrap().display().to_string());
    // A file lists the folder holding it.
    let (_, body) = get(addr, &q(&d.0.join("zeta").join("x.sql")));
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["path"], d.0.join("zeta").display().to_string());
    assert_eq!(
        get(addr, &q(&d.0.join("missing"))).0,
        "HTTP/1.1 400 Bad Request"
    );
}

#[test]
fn a_comparison_opened_from_home_replaces_the_session_and_is_remembered() {
    let d = Dir::new("open");
    let a = d.write("a.sql", "SELECT a FROM t WHERE b = 1;\n");
    let b = d.write("b.sql", "SELECT a FROM t WHERE b = 2;\n");
    d.write("ta/x.sql", "SELECT 1;\n");
    d.write("tb/x.sql", "SELECT 2;\n");
    let mut with_dir = app(None, None);
    with_dir.data_dir = Some(d.0.join("data"));
    let addr = start(with_dir);
    let open =
        |body: serde_json::Value| request(addr, "POST", "/api/open/compare", &body.to_string());

    // Refused, and nothing opened: one file and one folder, a missing path,
    // an unknown dialect, an unknown field.
    for body in [
        serde_json::json!({ "a": a, "b": d.0.join("tb") }),
        serde_json::json!({ "a": a, "b": d.0.join("nope.sql") }),
        serde_json::json!({ "a": a, "b": b, "dialect_a": "oracle" }),
        serde_json::json!({ "a": a, "b": b, "by": "agent" }),
    ] {
        assert_eq!(open(body.clone()).0, "HTTP/1.1 400 Bad Request", "{body}");
    }
    assert_eq!(
        rail(&get(addr, "/").1).0,
        ["/", "/runs", "/convert", "/settings"]
    );

    let (status, body) =
        open(serde_json::json!({ "a": a, "b": b, "dialect_a": "tsql", "dialect_b": "tsql" }));
    assert_eq!(status, "HTTP/1.1 200 OK", "{body}");
    assert_eq!(body, r#"{"open":"/compare"}"#);
    assert_eq!(
        rail(&get(addr, "/").1).0,
        ["/", "/runs", "/compare", "/convert", "/settings"]
    );
    let (_, pair) = get(addr, "/compare/api/pair/0");
    assert!(pair.contains(r#""verdict":"different""#), "{pair}");

    // Two folders replace the two files.
    let (status, _) = open(
        serde_json::json!({ "a": d.0.join("ta"), "b": d.0.join("tb"), "dialect_a": "", "dialect_b": null }),
    );
    assert_eq!(status, "HTTP/1.1 200 OK");
    let (_, home) = get(addr, "/");
    assert!(home.contains(r#""kind":"folders""#), "{home}");

    // Newest first, paths and dialects only.
    let recent = sqldiff_gui::app::recent(&d.0.join("data"));
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].a, d.0.join("ta").display().to_string());
    assert_eq!(recent[0].dialect_a, None);
    assert_eq!(recent[1].a, a.display().to_string());
    assert_eq!(recent[1].dialect_b.as_deref(), Some("tsql"));
    assert!(home.contains(r#""recent":[{"a":"#), "{home}");
}

#[test]
fn files_cannot_be_picked_or_opened_on_a_network_address() {
    let d = Dir::new("lan");
    let a = d.write("a.sql", "SELECT 1;\n");
    // Bound to every interface: reachable from other machines.
    let listener = TcpListener::bind("0.0.0.0:0").unwrap();
    let addr: SocketAddr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port())
        .parse()
        .unwrap();
    std::thread::spawn(move || serve(listener, app(None, None), &[]));
    let (status, body) = get(addr, &format!("/api/fs?path={}", d.0.display()));
    assert_eq!(status, "HTTP/1.1 403 Forbidden", "{body}");
    assert!(!body.contains("a.sql"));
    let open = serde_json::json!({ "a": a, "b": a }).to_string();
    assert_eq!(
        request(addr, "POST", "/api/open/compare", &open).0,
        "HTTP/1.1 403 Forbidden"
    );
    assert_eq!(
        rail(&get(addr, "/").1).0,
        ["/", "/runs", "/convert", "/settings"]
    );
    let (_, home) = get(addr, "/");
    assert!(
        home.contains(r#""no_picking":"picking files and starting runs work only"#),
        "{home}"
    );
    let run =
        serde_json::json!({ "src": d.0, "out": d.0.join("out"), "from": "tsql", "to": "postgres" });
    assert_eq!(
        request(addr, "POST", "/api/runs", &run.to_string()).0,
        "HTTP/1.1 403 Forbidden"
    );
    assert!(!d.0.join("out").exists());
    assert_eq!(
        request(addr, "POST", "/api/runs/instances", &run.to_string()).0,
        "HTTP/1.1 403 Forbidden"
    );
    let drift = serde_json::json!({
        "trees": [{"dialect": "tsql", "dir": d.0}, {"dialect": "postgres", "dir": d.0}],
        "out": d.0.join("out"),
    });
    assert_eq!(
        request(addr, "POST", "/api/runs/drift", &drift.to_string()).0,
        "HTTP/1.1 403 Forbidden"
    );
    assert!(!d.0.join("out").exists());
    let changes = serde_json::json!({ "repo": d.0, "base": "main", "out": d.0.join("out") });
    assert_eq!(
        request(addr, "POST", "/api/runs/changes", &changes.to_string()).0,
        "HTTP/1.1 403 Forbidden"
    );
    assert!(!d.0.join("out").exists());
    let batch = serde_json::json!({
        "sources": [{"src": d.0, "from": "tsql"}], "targets": ["postgres"], "out": d.0.join("out"),
    });
    assert_eq!(
        request(addr, "POST", "/api/runs/batch", &batch.to_string()).0,
        "HTTP/1.1 403 Forbidden"
    );
    assert!(!d.0.join("out").exists());
    // On loopback the same requests are answered.
    let addr = start(app(None, None));
    assert_eq!(
        get(addr, &format!("/api/fs?path={}", d.0.display())).0,
        "HTTP/1.1 200 OK"
    );
    assert!(get(addr, "/").1.contains(r#""no_picking":null"#));
}

/// Run `id` as `/api/runs` lists it, once it has stopped running.
fn finished(addr: SocketAddr, id: u64) -> serde_json::Value {
    for _ in 0..600 {
        let runs: serde_json::Value = serde_json::from_str(&get(addr, "/api/runs").1).unwrap();
        let run = runs
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .cloned()
            .expect("listed");
        if run["state"] != "running" && run["state"] != "queued" {
            return run;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("run {id} still running after a minute");
}

#[test]
fn a_batch_is_listed_with_its_matrix_script_and_runs() {
    let d = Dir::new("batch");
    d.write("in/orders/a.sql", "CREATE TABLE t (a int NOT NULL);\n");
    d.write("in/billing/b.sql", "SELECT 1 AS one;\n");
    let addr = start(app(None, None));
    let (_, html) = get(addr, "/runs");
    assert!(html.contains(r#""batches":[]"#), "{html}");
    let spec = serde_json::json!({
        "sources": [
            {"src": d.0.join("in").join("orders"), "from": "tsql"},
            {"src": d.0.join("in").join("billing"), "from": "tsql"},
        ],
        "targets": ["postgres", "db2i"],
        "out": d.0.join("out"),
    });
    let (status, body) = request(addr, "POST", "/api/runs/batch", &spec.to_string());
    assert_eq!(status, "HTTP/1.1 200 OK", "{body}");
    let b: serde_json::Value = serde_json::from_str(&body).unwrap();
    let ids: Vec<u64> = b["cells"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["run"].as_u64().unwrap())
        .collect();
    assert_eq!(ids.len(), 4);
    for id in &ids {
        let run = finished(addr, *id);
        assert_eq!(run["state"], "done", "{run}");
        assert_eq!(run["kind"], "convert");
        assert_eq!(run["batch"]["batch"], b["id"]);
        // Each cell is an ordinary run with its own review.
        assert_eq!(
            get(addr, &format!("/runs/{id}/review")).0,
            "HTTP/1.1 200 OK"
        );
    }
    let batches: serde_json::Value = serde_json::from_str(&get(addr, "/api/batches").1).unwrap();
    assert_eq!(batches[0]["id"], b["id"]);
    assert_eq!(batches[0]["busy"], false);
    assert!(batches[0]["script"]["sh"]
        .as_str()
        .unwrap()
        .contains(" convert "));
    let out = d.0.join("out");
    for (src, to) in [
        ("orders", "postgres"),
        ("orders", "db2i"),
        ("billing", "postgres"),
        ("billing", "db2i"),
    ] {
        assert!(
            out.join(src)
                .join(to)
                .join("conversion-report.json")
                .is_file(),
            "{src} {to}"
        );
    }
    // Cancel answers with the batch; nothing is left to stop.
    let path = format!("/api/batches/{}/cancel", b["id"]);
    let (status, body) = request(addr, "POST", &path, "{}");
    assert_eq!(status, "HTTP/1.1 200 OK", "{body}");
    assert_eq!(get(addr, &path).0, "HTTP/1.1 404 Not Found");
    assert_eq!(
        request(addr, "POST", "/api/batches/999/cancel", "{}").0,
        "HTTP/1.1 404 Not Found"
    );
    // The same folders again, without replacing: refused whole.
    let (status, body) = request(addr, "POST", "/api/runs/batch", &spec.to_string());
    assert_eq!(status, "HTTP/1.1 400 Bad Request", "{body}");
    assert!(body.contains("not empty"), "{body}");
    let runs: serde_json::Value = serde_json::from_str(&get(addr, "/api/runs").1).unwrap();
    assert_eq!(runs.as_array().unwrap().len(), 4);
    // Replacing, it runs again.
    let mut again = spec.clone();
    again["overwrite"] = true.into();
    let (status, body) = request(addr, "POST", "/api/runs/batch", &again.to_string());
    assert_eq!(status, "HTTP/1.1 200 OK", "{body}");
    let b: serde_json::Value = serde_json::from_str(&body).unwrap();
    for c in b["cells"].as_array().unwrap() {
        assert_eq!(finished(addr, c["run"].as_u64().unwrap())["state"], "done");
    }
}

#[test]
fn a_run_converts_a_tree_and_opens_its_own_review() {
    let d = Dir::new("run");
    d.write("src/a.sql", "CREATE TABLE t (a int NOT NULL);\n");
    d.write(
        "src/sub/p.sql",
        "CREATE PROCEDURE dbo.p AS\nBEGIN\n  UPDATE dbo.t SET a = 1;\nEND\n",
    );
    let addr = start(app(None, None));
    let (status, html) = get(addr, "/runs");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(html.contains(r#""runs":[]"#), "{html}");
    assert!(!html.contains("<!--__SQLDIFF_PICKER__-->"));
    let spec = serde_json::json!({
        "src": d.0.join("src"),
        "out": d.0.join("out"),
        "from": "tsql",
        "to": "postgres",
    });
    let (status, body) = request(addr, "POST", "/api/runs", &spec.to_string());
    assert_eq!(status, "HTTP/1.1 200 OK", "{body}");
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_u64()
        .unwrap();
    let run = finished(addr, id);
    assert_eq!(run["state"], "done", "{run}");
    assert_eq!(run["kind"], "convert");
    assert_eq!(
        (run["checked"].as_u64(), run["total"].as_u64()),
        (Some(2), Some(2)),
        "{run}"
    );
    assert_eq!(run["totals"]["files"], 2, "{run}");
    // The run sheet: the command that writes the same, config ignored as the
    // app (started isolated) ignores it.
    let c = &run["command"];
    assert_eq!(c["args"][0], "convert");
    assert_eq!(c["args"][1], d.0.join("src").display().to_string());
    assert!(
        c["args"].as_array().unwrap().contains(&"--isolated".into()),
        "{c}"
    );
    assert_eq!(
        (&c["user_config"], &c["project_config"]),
        (&serde_json::Value::Null, &serde_json::Value::Null)
    );
    assert!(
        c["powershell"]
            .as_str()
            .unwrap()
            .ends_with("--from tsql --to postgres --isolated"),
        "{c}"
    );
    // What was written is what the command writes: the files and the reports.
    for f in [
        "a.sql",
        "sub/p.sql",
        "conversion-report.json",
        "review-queue.json",
    ] {
        assert!(d.0.join("out").join(f).is_file(), "{f}");
    }

    // The run's review is mounted under the run and is where Review leads.
    let review = format!("/runs/{id}/review");
    let (status, html) = get(addr, &review);
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(rail(&html).1, review);
    assert!(html.contains(&format!(r#"const BASE = /*__SQLDIFF_BASE__*/"{review}";"#)));
    let (status, state) = get(addr, &format!("{review}/api/state"));
    assert_eq!(status, "HTTP/1.1 200 OK", "{state}");
    assert!(state.contains(r#""ledger_path""#), "{state}");
    assert!(rail(&get(addr, "/").1).0.contains(&review));

    // A second run into the folder that is no longer empty fails and changes nothing.
    let before = std::fs::read(d.0.join("out/a.sql")).unwrap();
    let (status, body) = request(addr, "POST", "/api/runs", &spec.to_string());
    assert_eq!(status, "HTTP/1.1 200 OK", "{body}");
    let id2 = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_u64()
        .unwrap();
    let run = finished(addr, id2);
    assert_eq!(run["state"], "failed", "{run}");
    assert!(
        run["error"].as_str().unwrap().contains("not empty"),
        "{run}"
    );
    assert_eq!(std::fs::read(d.0.join("out/a.sql")).unwrap(), before);
    assert_eq!(
        get(addr, &format!("/runs/{id2}/review")).0,
        "HTTP/1.1 404 Not Found"
    );
    // Newest first.
    let runs: serde_json::Value = serde_json::from_str(&get(addr, "/api/runs").1).unwrap();
    assert_eq!(runs[0]["id"], id2);
    assert_eq!(runs[1]["id"], id);
}

#[test]
fn runs_that_cannot_start_are_refused() {
    let d = Dir::new("refused");
    d.write("src/a.sql", "SELECT 1;\n");
    let addr = start(app(None, None));
    let out = d.0.join("out");
    let spec = |src: serde_json::Value, from: &str, to: &str| {
        serde_json::json!({ "src": src, "out": out, "from": from, "to": to }).to_string()
    };
    let src = serde_json::json!(d.0.join("src"));
    for (body, says) in [
        (spec(src.clone(), "oracle", "postgres"), "unknown dialect"),
        (spec(src.clone(), "postgres", "tsql"), "no conversion rules"),
        (spec(serde_json::json!(d.0.join("gone")), "tsql", "postgres"), "not found"),
        (spec(serde_json::json!(" "), "tsql", "postgres"), "give a source"),
        (
            serde_json::json!({ "src": src, "out": out, "from": "tsql", "to": "postgres", "template": true })
                .to_string(),
            "[template] syntax",
        ),
        (
            serde_json::json!({ "src": src, "out": out, "from": "tsql", "to": "postgres", "parameters": ["A"] })
                .to_string(),
            "template runs",
        ),
        (
            serde_json::json!({ "src": src, "out": out, "from": "tsql", "to": "postgres", "packs": [d.0.join("no-pack")] })
                .to_string(),
            "no-pack: not found",
        ),
        (
            serde_json::json!({ "src": src, "out": out, "from": "tsql", "to": "postgres", "instances": d.0.join("no-report") })
                .to_string(),
            "no-report: not found",
        ),
        (
            serde_json::json!({ "src": src, "out": out, "from": "tsql", "to": "postgres", "jobs": 2 })
                .to_string(),
            "jobs",
        ),
    ] {
        let (status, text) = request(addr, "POST", "/api/runs", &body);
        assert_eq!(status, "HTTP/1.1 400 Bad Request", "{body}: {text}");
        assert!(text.contains(says), "{body}: {text}");
    }
    // Instance checks: the manifest, and the templates or the generator's
    // output, must be given and exist.
    let manifest = d.write("instances.toml", "");
    let check = |extra: serde_json::Value| {
        let mut b = serde_json::json!({ "manifest": manifest, "out": out, "from": "tsql", "to": "postgres" });
        b.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        b.to_string()
    };
    for (body, says) in [
        (
            check(serde_json::json!({ "manifest": d.0.join("none.toml") })),
            "none.toml: not found",
        ),
        (check(serde_json::json!({})), "give the source templates"),
        (
            check(serde_json::json!({ "source": d.0.join("src") })),
            "give the source templates",
        ),
        (
            check(serde_json::json!({ "generator_only": true })),
            "needs both of its outputs",
        ),
        (
            check(
                serde_json::json!({ "generator_only": true, "woven": d.0.join("src"), "woven_target": d.0.join("src"), "source": d.0.join("src") }),
            ),
            "takes no templates",
        ),
        (
            check(serde_json::json!({ "target": d.0.join("gone"), "source": d.0.join("src") })),
            "gone: not found",
        ),
        (
            check(serde_json::json!({ "from": "oracle" })),
            "unknown dialect",
        ),
        (check(serde_json::json!({ "since": "x" })), "since"),
    ] {
        let (status, text) = request(addr, "POST", "/api/runs/instances", &body);
        assert_eq!(status, "HTTP/1.1 400 Bad Request", "{body}: {text}");
        assert!(text.contains(says), "{body}: {text}");
    }
    assert_eq!(get(addr, "/api/runs").1, "[]");
    assert!(!out.exists());
    assert_eq!(
        request(addr, "POST", "/runs/7/cancel", "{}").0,
        "HTTP/1.1 404 Not Found"
    );
}

#[test]
fn a_run_never_reads_or_writes_where_a_running_run_writes() {
    let d = Dir::new("overlap");
    // Enough files that the first run is still converting.
    for k in 0..300 {
        d.write(
            &format!("src/p{k}.sql"),
            &format!("CREATE PROCEDURE dbo.p{k} AS\nBEGIN\n  UPDATE dbo.t SET a = {k};\nEND\n"),
        );
    }
    let manifest = d.write("instances.toml", "");
    let addr = start(app(None, None));
    let out = d.0.join("out");
    let spec =
        serde_json::json!({ "src": d.0.join("src"), "out": out, "from": "tsql", "to": "postgres" });
    let (status, body) = request(addr, "POST", "/api/runs", &spec.to_string());
    assert_eq!(status, "HTTP/1.1 200 OK", "{body}");
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_u64()
        .unwrap();
    let refused = [
        // Into the same folder, or one inside it.
        (
            "/api/runs",
            serde_json::json!({ "src": d.0.join("src"), "out": out.join("sub"), "from": "tsql", "to": "postgres" }),
            "another run is writing there",
        ),
        // Reading the folder being written.
        (
            "/api/runs/instances",
            serde_json::json!({ "manifest": manifest, "source": d.0.join("src"), "target": out, "from": "tsql", "to": "postgres", "out": d.0.join("inst") }),
            "half-written",
        ),
        // Writing into the folder being read.
        (
            "/api/runs",
            serde_json::json!({ "src": d.0.join("src"), "out": d.0.join("src/x"), "from": "tsql", "to": "postgres" }),
            "another run is reading there",
        ),
    ];
    let still = |addr| {
        let runs: serde_json::Value = serde_json::from_str(&get(addr, "/api/runs").1).unwrap();
        runs.as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == id && r["state"] == "running")
    };
    for (path, body, says) in refused {
        // `out` exists once the first run prepared it: the instance check's
        // target must exist to get as far as the overlap.
        std::fs::create_dir_all(&out).unwrap();
        let (status, text) = request(addr, "POST", path, &body.to_string());
        assert!(still(addr), "the first run finished too soon for this test");
        assert_eq!(status, "HTTP/1.1 400 Bad Request", "{body}: {text}");
        assert!(text.contains(says), "{body}: {text}");
    }
    // Another spelling of the same folder is the same folder.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&d.0, d.0.join("link")).unwrap();
        let spec = serde_json::json!({ "src": d.0.join("src"), "out": d.0.join("link/out"), "from": "tsql", "to": "postgres" });
        let (status, text) = request(addr, "POST", "/api/runs", &spec.to_string());
        assert!(still(addr), "the first run finished too soon for this test");
        assert_eq!(status, "HTTP/1.1 400 Bad Request", "{text}");
        assert!(text.contains("another run is writing there"), "{text}");
    }
    request(addr, "POST", &format!("/runs/{id}/cancel"), "{}");
    assert_eq!(finished(addr, id)["state"], "cancelled");
}

#[test]
fn runs_are_kept_in_the_data_dir_and_their_reviews_open_again() {
    let d = Dir::new("history");
    d.write("src/a.sql", "CREATE TABLE t (a int NOT NULL);\n");
    let data = d.0.join("data");
    let kept = || App {
        data_dir: Some(data.clone()),
        isolated: false,
        ..app(None, None)
    };
    let addr = start(kept());
    let spec = serde_json::json!({
        "src": d.0.join("src"),
        "out": d.0.join("out"),
        "from": "tsql",
        "to": "postgres",
    });
    let (status, body) = request(addr, "POST", "/api/runs", &spec.to_string());
    assert_eq!(status, "HTTP/1.1 200 OK", "{body}");
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_u64()
        .unwrap();
    let run = finished(addr, id);
    assert_eq!(run["state"], "done", "{run}");
    assert_eq!(run["past"], false);
    // Paths, settings and totals: never the SQL.
    let file = data.join("gui").join("runs.json");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.contains("CREATE TABLE"), "{text}");
    assert!(text.contains(r#""state": "done""#), "{text}");

    // After a restart: listed as it finished, and its review opens again
    // from the tree on disk, with the done gate judged there.
    let addr = start(kept());
    let runs: serde_json::Value = serde_json::from_str(&get(addr, "/api/runs").1).unwrap();
    let past = &runs[0];
    assert_eq!((&past["id"], &past["past"]), (&id.into(), &true.into()));
    assert_eq!(past["state"], "done");
    assert_eq!(past["command"], run["command"]);
    assert_eq!(past["totals"], run["totals"]);
    assert!(past["gate"].is_null(), "judged once the review opens");
    let review = format!("/runs/{id}/review");
    let (status, _) = get(addr, &review);
    assert_eq!(status, "HTTP/1.1 200 OK");
    let state: serde_json::Value =
        serde_json::from_str(&get(addr, &format!("{review}/api/state")).1).unwrap();
    assert_eq!(state["gate"]["complete"], true, "{state}");
    let runs: serde_json::Value = serde_json::from_str(&get(addr, "/api/runs").1).unwrap();
    assert_eq!(runs[0]["gate"]["complete"], true, "{runs}");
    // New runs go on from the last number.
    let spec2 = serde_json::json!({ "src": d.0.join("src"), "out": d.0.join("out2"), "from": "tsql", "to": "postgres" });
    let (_, body) = request(addr, "POST", "/api/runs", &spec2.to_string());
    let id2 = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_u64()
        .unwrap();
    assert_eq!(id2, id + 1);
    finished(addr, id2);

    // Config found from the source changed since: the review is not opened
    // on other config than the run's.
    d.write("src/sqldiff.toml", "");
    let addr = start(kept());
    let (status, text) = get(addr, &review);
    assert_eq!(status, "HTTP/1.1 409 Conflict", "{text}");
    assert!(text.contains("other config"), "{text}");
    std::fs::remove_file(d.0.join("src/sqldiff.toml")).unwrap();
    // The source gone: refused, not opened on nothing.
    std::fs::rename(d.0.join("src"), d.0.join("gone")).unwrap();
    let (status, text) = get(addr, &review);
    assert_eq!(status, "HTTP/1.1 409 Conflict", "{text}");
    std::fs::rename(d.0.join("gone"), d.0.join("src")).unwrap();
    // Opened again on a network address only from this computer.
    let listener = TcpListener::bind("0.0.0.0:0").unwrap();
    let lan = SocketAddr::from(([127, 0, 0, 1], listener.local_addr().unwrap().port()));
    let lan_app = kept();
    std::thread::spawn(move || serve(listener, lan_app, &[]));
    assert_eq!(get(lan, &review).0, "HTTP/1.1 403 Forbidden");
}

#[test]
fn a_run_the_app_stopped_during_is_listed_as_failed() {
    let d = Dir::new("history-stopped");
    let spec = serde_json::json!({ "kind": "convert", "src": d.0.join("src"), "out": d.0.join("out"), "from": "tsql", "to": "postgres" });
    let record = serde_json::json!([{
        "id": 7,
        "spec": spec,
        "command": { "args": ["convert"], "project_config": null, "user_config": null, "powershell": "sqldiff convert", "sh": "sqldiff convert" },
        "started": "2026-10-27T10:00:00Z",
        "checked": 3,
        "total": 9,
        "state": { "state": "running" },
    }]);
    d.write("data/gui/runs.json", &record.to_string());
    let addr = start(App {
        data_dir: Some(d.0.join("data")),
        ..app(None, None)
    });
    let runs: serde_json::Value = serde_json::from_str(&get(addr, "/api/runs").1).unwrap();
    assert_eq!(runs[0]["id"], 7, "{runs}");
    assert_eq!(runs[0]["state"], "failed");
    assert!(runs[0]["error"]
        .as_str()
        .unwrap()
        .contains("may be incomplete"));
    assert_eq!(get(addr, "/runs/7/review").0, "HTTP/1.1 404 Not Found");
}

const DRIFT_PACK: &str = "[pack]\nname = \"drift\"\nversion = \"0.1.0\"\nschema = 1\n\n[template]\nopen = \"<%\"\nclose = \"%>\"\nname_pattern = \"[A-Z_][A-Z0-9_]*\"\n";

fn drift_spec(d: &Dir, out: &str) -> serde_json::Value {
    serde_json::json!({
        "trees": [
            { "dialect": "tsql", "dir": d.0.join("ms") },
            { "dialect": "postgres", "dir": d.0.join("pg") },
        ],
        "out": d.0.join(out),
        "packs": [d.0.join("pack.toml")],
    })
}

fn start_drift(addr: SocketAddr, body: &serde_json::Value) -> u64 {
    let (status, text) = request(addr, "POST", "/api/runs/drift", &body.to_string());
    assert_eq!(status, "HTTP/1.1 200 OK", "{text}");
    serde_json::from_str::<serde_json::Value>(&text).unwrap()["id"]
        .as_u64()
        .unwrap()
}

#[test]
fn a_drift_run_is_listed_with_its_command_and_is_not_a_review() {
    let d = Dir::new("drift");
    d.write("pack.toml", DRIFT_PACK);
    d.write("ms/a.sql", "CREATE TABLE <%S%>.<%T%> (Id int);\n");
    d.write("pg/a.sql", "CREATE TABLE <%S%>.<%U%> (Id integer);\n");
    let addr = start(app(None, None));
    // Refused before a thread starts.
    let bad = |body: serde_json::Value, says: &str| {
        let (status, text) = request(addr, "POST", "/api/runs/drift", &body.to_string());
        assert_eq!(status, "HTTP/1.1 400 Bad Request", "{body}: {text}");
        assert!(text.contains(says), "{body}: {text}");
    };
    let mut one = drift_spec(&d, "out");
    one["trees"].as_array_mut().unwrap().pop();
    bad(one, "at least two");
    let mut twice = drift_spec(&d, "out");
    twice["trees"][1]["dialect"] = "tsql".into();
    bad(twice, "given twice");
    let mut dialect = drift_spec(&d, "out");
    dialect["trees"][1]["dialect"] = "oracle".into();
    bad(dialect, "unknown dialect");
    let mut reference = drift_spec(&d, "out");
    reference["reference"] = "db2i".into();
    bad(reference, "no tree to check against");
    let mut nopack = drift_spec(&d, "out");
    nopack["packs"] = serde_json::json!([]);
    bad(nopack, "[template]");
    assert!(!d.0.join("out").exists());

    let id = start_drift(addr, &drift_spec(&d, "out"));
    let run = finished(addr, id);
    assert_eq!(run["kind"], "drift");
    assert_eq!(run["state"], "done", "{run}");
    // The verdict is the command's, not a gate: drift was found.
    assert_eq!(run["complete"], false);
    assert_eq!(run["totals"]["drifting"], 1);
    assert!(run["gate"].is_null(), "{run}");
    let args: Vec<_> = run["command"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert_eq!(args[0], "drift");
    assert!(
        args.contains(&"--isolated") && args.contains(&"--pack"),
        "{args:?}"
    );
    assert!(d.0.join("out/drift-report.json").is_file());
    // A drift run has no review to open.
    assert_eq!(
        get(addr, &format!("/runs/{id}/review")).0,
        "HTTP/1.1 404 Not Found"
    );
    // A second check into the same folder fails unless it may replace files.
    let again = start_drift(addr, &drift_spec(&d, "out"));
    let run = finished(addr, again);
    assert_eq!(run["state"], "failed", "{run}");
    assert!(
        run["error"].as_str().unwrap().contains("not empty"),
        "{run}"
    );
}

#[test]
fn a_cancelled_drift_run_writes_no_report() {
    let d = Dir::new("drift-cancel");
    d.write("pack.toml", DRIFT_PACK);
    // Enough templates that the check is still reading the first tree when
    // the cancel arrives.
    for n in 0..4000 {
        d.write(
            &format!("ms/t{n}.sql"),
            &format!("CREATE TABLE <%S%>.t{n} (Id int);\n"),
        );
        d.write(
            &format!("pg/t{n}.sql"),
            &format!("CREATE TABLE <%S%>.t{n} (Id integer);\n"),
        );
    }
    let addr = start(app(None, None));
    let id = start_drift(addr, &drift_spec(&d, "out"));
    request(addr, "POST", &format!("/runs/{id}/cancel"), "{}");
    let run = finished(addr, id);
    assert_eq!(run["state"], "cancelled", "{run}");
    let written: Vec<_> = std::fs::read_dir(d.0.join("out"))
        .map(|r| r.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(written.is_empty(), "{written:?}");
}

fn git(repo: &std::path::Path, args: &[&str]) {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

/// A repository whose `topic` branch changes `n` SQL files of `main`.
fn branch(d: &Dir, n: usize) -> PathBuf {
    let repo = d.0.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    for i in 0..n {
        d.write(
            &format!("repo/f{i}.tsql.sql"),
            "SELECT a FROM t WHERE b = 1;\n",
        );
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["checkout", "-q", "-b", "topic"]);
    for i in 0..n {
        d.write(
            &format!("repo/f{i}.tsql.sql"),
            "SELECT a FROM t WHERE b = 2;\n",
        );
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "change"]);
    repo
}

fn start_changes(addr: SocketAddr, body: &serde_json::Value) -> u64 {
    let (status, text) = request(addr, "POST", "/api/runs/changes", &body.to_string());
    assert_eq!(status, "HTTP/1.1 200 OK", "{text}");
    serde_json::from_str::<serde_json::Value>(&text).unwrap()["id"]
        .as_u64()
        .unwrap()
}

#[test]
fn a_branch_review_run_writes_its_files_and_is_not_a_review() {
    let d = Dir::new("changes");
    let repo = branch(&d, 2);
    let addr = start(app(None, None));
    let spec =
        |out: &str| serde_json::json!({ "repo": repo, "base": "main", "out": d.0.join(out) });
    let bad = |body: serde_json::Value, says: &str| {
        let (status, text) = request(addr, "POST", "/api/runs/changes", &body.to_string());
        assert_eq!(status, "HTTP/1.1 400 Bad Request", "{body}: {text}");
        assert!(text.contains(says), "{body}: {text}");
    };
    let mut missing = spec("out");
    missing["repo"] = d.0.join("nowhere").to_str().unwrap().into();
    bad(missing, "not found");
    let mut blank = spec("out");
    blank["base"] = " ".into();
    bad(blank, "base revision");
    let mut dialect = spec("out");
    dialect["dialect"] = "oracle".into();
    bad(dialect, "unknown dialect");
    d.write("full/x.txt", "in the way\n");
    bad(spec("full"), "not empty");
    assert!(!d.0.join("out").exists());

    let id = start_changes(addr, &spec("out"));
    let run = finished(addr, id);
    assert_eq!(run["kind"], "changes");
    assert_eq!(run["state"], "done", "{run}");
    assert_eq!(run["totals"]["verdict"], "different");
    assert_eq!(run["totals"]["files"], 2);
    assert_eq!(run["complete"], false);
    assert!(run["gate"].is_null(), "{run}");
    for f in ["changes.json", "changes.md", "changes.sarif"] {
        assert!(d.0.join("out").join(f).is_file(), "{f}");
    }
    let args: Vec<_> = run["command"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert_eq!(args[0], "changes");
    assert!(
        args.contains(&"--no-cache") && args.contains(&"--isolated"),
        "{args:?}"
    );
    assert_eq!(
        get(addr, &format!("/runs/{id}/review")).0,
        "HTTP/1.1 404 Not Found"
    );
}

#[test]
fn a_cancelled_branch_review_writes_nothing() {
    let d = Dir::new("changes-cancel");
    let repo = branch(&d, 1500);
    let addr = start(app(None, None));
    let id = start_changes(
        addr,
        &serde_json::json!({ "repo": repo, "base": "main", "out": d.0.join("out") }),
    );
    request(addr, "POST", &format!("/runs/{id}/cancel"), "{}");
    let run = finished(addr, id);
    assert_eq!(run["state"], "cancelled", "{run}");
    // Not even the folder: the files and the folder are made after the last cancel point.
    assert!(!d.0.join("out").exists());
}

/// What every page says about a comparison's or run's packs: a shared pack
/// (named in a config file) by name, version and path, a private pack (the
/// app's `--pack`, a run's pack field) by path only; its name never reaches
/// a page or an API answer.
#[test]
fn pages_confirm_the_packs_loaded_and_name_no_private_pack() {
    let d = Dir::new("packs");
    // Expected paths are joined part by part: on Windows the app reports
    // `\` throughout, and a joined "a/b" would keep its `/`.
    let shared = d.0.join("shared").join("pack.toml");
    d.write(
        "shared/pack.toml",
        "[pack]\nname = \"shared-rules\"\nversion = \"1.2.0\"\nschema = 1\n",
    );
    let secret = d.0.join("secret").join("pack.toml");
    d.write(
        "secret/pack.toml",
        "[pack]\nname = \"secretname\"\nversion = \"9.9.9\"\nschema = 1\n\n[template]\nopen = \"<%\"\nclose = \"%>\"\nname_pattern = \"[A-Z_][A-Z0-9_]*\"\n",
    );
    d.write("ws/sqldiff.toml", "packs = [\"../shared\"]\n");
    let a = d.write("ws/a.sql", "SELECT a FROM t WHERE b = 1;\n");
    let b = d.write("ws/b.sql", "SELECT a FROM t WHERE b = 2;\n");
    d.write("ws/src/p.sql", "CREATE TABLE t (a int NOT NULL);\n");
    let addr = start(App {
        isolated: false,
        packs: vec![d.0.join("secret")],
        ..app(None, None)
    });
    let path = |p: &std::path::Path| p.display().to_string();
    let payload = |html: &str| -> serde_json::Value {
        let at = html.find("/*__SQLDIFF_PAYLOAD__*/").expect("payload") + 23;
        let mut de = serde_json::Deserializer::from_str(&html[at..]);
        serde::Deserialize::deserialize(&mut de).unwrap()
    };

    // Home says which packs a comparison opened there loads besides its
    // folder's config: the ones the app was started with.
    let (_, home) = get(addr, "/");
    assert_eq!(
        payload(&home)["launch_packs"],
        serde_json::json!([path(&d.0.join("secret"))])
    );

    let (status, body) = request(
        addr,
        "POST",
        "/api/open/compare",
        &serde_json::json!({ "a": a, "b": b, "dialect_a": "tsql", "dialect_b": "tsql" })
            .to_string(),
    );
    assert_eq!(status, "HTTP/1.1 200 OK", "{body}");
    let want = serde_json::json!([
        { "private": false, "name": "shared-rules", "version": "1.2.0", "path": path(&shared) },
        { "private": true, "name": null, "version": null, "path": path(&secret) },
    ]);
    let (_, home) = get(addr, "/");
    assert_eq!(payload(&home)["compare"]["packs"], want, "{home}");
    let (_, page) = get(addr, "/compare");
    assert_eq!(payload(&page)["session"]["packs"], want);
    let (_, info) = get(addr, "/compare/api/session");
    assert!(info.contains(r#""name":"private pack""#), "{info}");

    // A run's packs: the config's and its own field's.
    let (status, run) = request(
        addr,
        "POST",
        "/api/runs",
        &serde_json::json!({
            "src": d.0.join("ws/src"), "out": d.0.join("out"), "from": "tsql", "to": "postgres",
            "packs": [d.0.join("secret")],
        })
        .to_string(),
    );
    assert_eq!(status, "HTTP/1.1 200 OK", "{run}");
    let id = serde_json::from_str::<serde_json::Value>(&run).unwrap()["id"]
        .as_u64()
        .unwrap();
    let run = finished(addr, id);
    assert_eq!(run["packs"], want, "{run}");
    let (_, review) = get(addr, &format!("/runs/{id}/review/api/state"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&review).unwrap()["packs"],
        want
    );

    for page in [
        "/",
        "/compare",
        "/compare/api/session",
        "/runs",
        "/api/runs",
    ] {
        let (_, text) = get(addr, page);
        assert!(
            !text.contains("secretname") && !text.contains("9.9.9"),
            "{page}: {text}"
        );
    }
}

/// A process started while a connection is open (a branch review runs `git`)
/// must not hold that connection: the response arrives when it is written,
/// not when the process exits, and is not reset when it does (Windows, where
/// a socket from `try_clone` is inheritable).
#[cfg(windows)]
#[test]
fn a_process_started_meanwhile_does_not_hold_the_connection() {
    let addr = start(app(None, None));
    let mut s = TcpStream::connect(addr).unwrap();
    // Accepted by now (the server waits on the request line); if not, the
    // child cannot hold it either, so this can pass wrongly but not fail wrongly.
    std::thread::sleep(std::time::Duration::from_millis(300));
    let mut child = std::process::Command::new("cmd")
        .args(["/C", "ping -n 6 127.0.0.1 >NUL"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let t = std::time::Instant::now();
    write!(s, "GET /api/runs HTTP/1.1\r\nHost: {addr}\r\n\r\n").unwrap();
    let mut resp = String::new();
    let read = s.read_to_string(&mut resp);
    let took = t.elapsed();
    let _ = child.kill();
    let _ = child.wait();
    read.unwrap();
    assert!(resp.starts_with("HTTP/1.1 200 OK"), "{resp}");
    assert!(took < std::time::Duration::from_secs(3), "{took:?}");
}
