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
    }
}

fn start(app: App) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || serve(listener, app, &[]));
    addr
}

/// Status line and body.
fn request(addr: SocketAddr, method: &str, path: &str, body: &str) -> (String, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
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
        ("/convert", None),
        ("/compare", Some("/compare")),
        ("/review", Some("/review")),
    ] {
        let (status, html) = get(addr, path);
        assert_eq!(status, "HTTP/1.1 200 OK", "{path}");
        let (links, on) = rail(&html);
        assert_eq!(links, ["/", "/compare", "/convert", "/review"], "{path}");
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
    assert_eq!(links, ["/", "/convert"]);
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
    assert_eq!(rail(&get(addr, "/").1).0, ["/", "/convert"]);

    let (status, body) =
        open(serde_json::json!({ "a": a, "b": b, "dialect_a": "tsql", "dialect_b": "tsql" }));
    assert_eq!(status, "HTTP/1.1 200 OK", "{body}");
    assert_eq!(body, r#"{"open":"/compare"}"#);
    assert_eq!(rail(&get(addr, "/").1).0, ["/", "/compare", "/convert"]);
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
    assert_eq!(rail(&get(addr, "/").1).0, ["/", "/convert"]);
    let (_, home) = get(addr, "/");
    assert!(
        home.contains(r#""no_picking":"picking files works only"#),
        "{home}"
    );
    // On loopback the same requests are answered.
    let addr = start(app(None, None));
    assert_eq!(
        get(addr, &format!("/api/fs?path={}", d.0.display())).0,
        "HTTP/1.1 200 OK"
    );
    assert!(get(addr, "/").1.contains(r#""no_picking":null"#));
}
