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
    let addr = start(App {
        convert: sample(),
        compare: Some(session(&d)),
        review: Some(review(&d)),
    });
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
    let addr = start(App {
        convert: sample(),
        compare: Some(session(&d)),
        review: Some(review(&d)),
    });
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
    let addr = start(App {
        convert: sample(),
        compare: None,
        review: None,
    });
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
    let addr = start(App {
        convert: sample(),
        compare: Some(session(&d)),
        review: Some(review(&d)),
    });
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
