//! The app: one local server for every view, with a rail to switch between them.
//!
//! - `GET /`: home, what is open
//! - `GET /convert`, `POST /api/convert`, `POST /api/compare`: convert mode
//!   (see `convert`)
//! - `GET /compare`, `/compare/api/...`: the pair view or tree queue of a
//!   session (see `server`)
//! - `GET /review`, `/review/api/...`: a converted tree's review queue (see
//!   `convert_review`)
//!
//! Each view keeps its own page and API. The app mounts the API under the
//! view's path, tells the page where (`BASE` in the page) and puts the rail in
//! front of it. Requests pass the same checks as the single-view servers
//! ([`crate::server::read_request`]): a loopback `Host`, and JSON POSTs from
//! this server's own origin.

use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::convert;
use crate::convert_review::{self, Review};
use crate::server::{allowed_hosts, not_found, read_request, respond, Request, Response};
use crate::session::{Origin, Session};

/// The home page; the payload is injected at `/*__SQLDIFF_PAYLOAD__*/null`.
pub const HOME_HTML: &str = include_str!("../assets/home.html");

/// What the app serves: convert mode always, a compare session and a review
/// when they were opened.
pub struct App {
    pub convert: convert::Setup,
    pub compare: Option<Session>,
    pub review: Option<Review>,
}

/// A view of the app, in rail order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Home,
    Compare,
    Convert,
    Review,
}

impl View {
    /// The page's path; also the prefix of its API, except for convert mode,
    /// whose API is at the root.
    pub fn path(self) -> &'static str {
        match self {
            View::Home => "/",
            View::Compare => "/compare",
            View::Convert => "/convert",
            View::Review => "/review",
        }
    }
}

struct Shared {
    convert: convert::Setup,
    compare: Option<Mutex<Session>>,
    review: Option<Mutex<Review>>,
}

impl Shared {
    fn is_open(&self, v: View) -> bool {
        match v {
            View::Home | View::Convert => true,
            View::Compare => self.compare.is_some(),
            View::Review => self.review.is_some(),
        }
    }
}

pub fn serve(listener: TcpListener, app: App, extra_hosts: &[String]) -> std::io::Result<()> {
    let shared = Arc::new(Shared {
        convert: app.convert,
        compare: app.compare.map(Mutex::new),
        review: app.review.map(Mutex::new),
    });
    let hosts: Arc<Vec<String>> = Arc::new(allowed_hosts(&listener, extra_hosts)?);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let shared = Arc::clone(&shared);
        let hosts = Arc::clone(&hosts);
        std::thread::Builder::new()
            .stack_size(sqldiff_core::ENGINE_STACK)
            .spawn(move || {
                let _ = handle(stream, &shared, &hosts);
            })?;
    }
    Ok(())
}

fn handle(mut stream: TcpStream, app: &Shared, hosts: &[String]) -> std::io::Result<()> {
    let req = match read_request(&stream, hosts)? {
        Ok(r) => r,
        Err(res) => return respond(&mut stream, res),
    };
    let res = route(app, req);
    respond(&mut stream, res)
}

fn route(app: &Shared, req: Request) -> Response {
    if req.method == "GET" && req.path == "/" {
        return page(app, View::Home, home(app));
    }
    if let Some(res) = convert::route(&app.convert, &req, View::Convert.path()) {
        return framed(app, View::Convert, res);
    }
    for view in [View::Compare, View::Review] {
        let Some(req) = mount(&req, view.path()) else {
            continue;
        };
        let res = match view {
            View::Compare => app.compare.as_ref().map(|s| {
                let mut s = s.lock().unwrap_or_else(|e| e.into_inner());
                crate::server::route(&mut s, &req)
            }),
            _ => app.review.as_ref().map(|r| convert_review::route(r, &req)),
        };
        return match res {
            Some(res) if req.path == "/" => framed(app, view, res),
            Some(res) => res,
            // Nothing open there: the page shows home, which says how to open it.
            None if req.path == "/" && req.method == "GET" => page(app, View::Home, home(app)),
            None => not_found(),
        };
    }
    not_found()
}

/// `req` as the view mounted at `prefix` sees it: its page at `/` and its
/// API at `/api/...`; `None` when the path is not under `prefix`.
fn mount(req: &Request, prefix: &str) -> Option<Request> {
    let rest = req.path.strip_prefix(prefix)?;
    let path = match rest {
        "" | "/" => "/",
        r if r.starts_with("/api/") => r,
        _ => return None,
    };
    Some(Request {
        method: req.method.clone(),
        path: path.to_string(),
        query: req.query.clone(),
        body: req.body.clone(),
    })
}

/// A view's page response with the rail in front and `BASE` set; other
/// responses (API, errors) pass unchanged.
fn framed(app: &Shared, view: View, res: Response) -> Response {
    match res {
        ("200 OK", ctype, html) if ctype.starts_with("text/html") => page(app, view, html),
        other => other,
    }
}

fn page(app: &Shared, view: View, html: String) -> Response {
    let base = match view {
        View::Compare | View::Review => view.path(),
        View::Home | View::Convert => "",
    };
    let html = html
        .replacen(
            "/*__SQLDIFF_BASE__*/\"\"",
            &format!("/*__SQLDIFF_BASE__*/\"{base}\""),
            1,
        )
        .replacen("<!--__SQLDIFF_SHELL__-->", &rail(app, view), 1);
    ("200 OK", "text/html; charset=utf-8", html)
}

/// The rail: one link per view, the current one marked; a view with nothing
/// open is shown but not a link.
fn rail(app: &Shared, current: View) -> String {
    const ITEMS: [(View, &str, &str, &str); 4] = [
        (
            View::Home,
            "Home",
            "What is open",
            r#"<path d="M4 10.5 12 4l8 6.5V19a1 1 0 0 1-1 1h-4.5v-5.5h-5V20H5a1 1 0 0 1-1-1z"/>"#,
        ),
        (
            View::Compare,
            "Compare",
            "Compare two files or folders",
            r#"<rect x="3.5" y="4.5" width="7" height="15" rx="1.5"/><rect x="13.5" y="4.5" width="7" height="15" rx="1.5"/><path d="M6 9h2M6 12h2M16 12h2M16 15h2"/>"#,
        ),
        (
            View::Convert,
            "Convert",
            "Convert SQL live into another dialect",
            r#"<path d="M4 8h14m-3-3 3 3-3 3M20 16H6m3-3-3 3 3 3"/>"#,
        ),
        (
            View::Review,
            "Review",
            "Review a converted tree",
            r#"<path d="M10 7h10M10 12h10M10 17h10M4 7l1.2 1.2L7.5 6M4 12l1.2 1.2 2.3-2.2M4 17l1.2 1.2 2.3-2.2"/>"#,
        ),
    ];
    let mut s = String::from(RAIL_STYLE);
    s.push_str(r#"<nav id="app-rail" aria-label="Views">"#);
    for (view, label, title, icon) in ITEMS {
        let svg = format!(
            r#"<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">{icon}</svg><span>{label}</span>"#
        );
        if view == current {
            s.push_str(&format!(
                r#"<a class="on" href="{}" aria-current="page" title="{title}">{svg}</a>"#,
                view.path()
            ));
        } else if app.is_open(view) {
            s.push_str(&format!(
                r#"<a href="{}" title="{title}">{svg}</a>"#,
                view.path()
            ));
        } else {
            s.push_str(&format!(
                r#"<span class="off" title="{title}: nothing open, see Home" aria-disabled="true">{svg}</span>"#
            ));
        }
    }
    s.push_str("</nav>");
    s
}

const RAIL_STYLE: &str = "<style>
body { padding-left: 60px; box-sizing: border-box; }
#app-rail { position: fixed; left: 0; top: 0; bottom: 0; width: 60px; z-index: 5; display: flex; flex-direction: column; gap: 2px; padding: 8px 6px; background: var(--soft); border-right: 1px solid var(--line); font-family: system-ui, -apple-system, \"Segoe UI\", Roboto, sans-serif; }
#app-rail a, #app-rail span.off { display: flex; flex-direction: column; align-items: center; gap: 2px; padding: 7px 0 5px; border-radius: 8px; color: var(--muted); text-decoration: none; font-size: 10.5px; line-height: 1.2; }
#app-rail a:hover { color: var(--fg); background: var(--panel); }
#app-rail a.on { color: var(--accent); background: var(--panel); box-shadow: 0 1px 2px rgba(16,24,40,.08); font-weight: 600; }
#app-rail span.off { opacity: .45; cursor: default; }
#app-rail a:focus-visible { outline: 2px solid var(--focus, var(--accent)); outline-offset: -2px; }
/* The rail replaces the views' own links to each other. */
#to-convert, #files { display: none !important; }
</style>
";

#[derive(Serialize)]
struct Home {
    version: &'static str,
    convert: ConvertCard,
    compare: Option<CompareCard>,
    review: Option<ReviewCard>,
}

#[derive(Serialize)]
struct ConvertCard {
    from: &'static str,
    to: &'static str,
    name: String,
    template: bool,
}

#[derive(Serialize)]
struct CompareCard {
    /// `files` or `folders`.
    kind: &'static str,
    a: String,
    b: String,
    pairs: usize,
}

#[derive(Serialize)]
struct ReviewCard {
    src: String,
    out: String,
    from: &'static str,
    to: &'static str,
}

fn home(app: &Shared) -> String {
    let c = &app.convert;
    let payload = Home {
        version: env!("CARGO_PKG_VERSION"),
        convert: ConvertCard {
            from: c.from.display_name(),
            to: c.to.display_name(),
            name: c.name.clone(),
            template: c.template.is_some(),
        },
        compare: app.compare.as_ref().map(|s| {
            let s = s.lock().unwrap_or_else(|e| e.into_inner());
            let (kind, a, b) = match s.origin() {
                Origin::Files { a, b } => ("files", a, b),
                Origin::Trees { a, b } => ("folders", a, b),
            };
            CompareCard {
                kind,
                a: a.display().to_string(),
                b: b.display().to_string(),
                pairs: s.len(),
            }
        }),
        review: app.review.as_ref().map(|r| {
            let r = r.lock().unwrap_or_else(|e| e.into_inner());
            let s = r.setup();
            ReviewCard {
                src: s.tree.src.display().to_string(),
                out: s.out.display().to_string(),
                from: s.tree.from.display_name(),
                to: s.tree.to.display_name(),
            }
        }),
    };
    let json = serde_json::to_string(&payload)
        .unwrap()
        .replace("</", "<\\/");
    HOME_HTML.replace(
        "/*__SQLDIFF_PAYLOAD__*/null",
        &format!("/*__SQLDIFF_PAYLOAD__*/{json}"),
    )
}
