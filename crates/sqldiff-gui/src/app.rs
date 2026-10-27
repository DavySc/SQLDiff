//! The app: one local server for every view, with a rail to switch between them.
//!
//! - `GET /`: home, what is open, and the forms that open more
//! - `GET /api/fs?path=`: a folder listing for the file picker (see `browse`)
//! - `POST /api/open/compare` `{"a", "b", "dialect_a"?, "dialect_b"?}`: open
//!   (or replace) the compare session on two files or two folders
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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use sqldiff_core::Dialect;
use sqldiff_source::config::{self, Sources};
use sqldiff_source::Resolved;

use crate::convert;
use crate::convert_review::{self, Review};
use crate::review::Store;
use crate::server::{
    allowed_hosts, bad, json, not_found, param, read_request, respond, Request, Response,
};
use crate::session::{Origin, Session};
use crate::{browse, server};

/// The home page; the payload is injected at `/*__SQLDIFF_PAYLOAD__*/null`.
pub const HOME_HTML: &str = include_str!("../assets/home.html");

/// What the app serves: convert mode always, a compare session and a review
/// when they were opened.
pub struct App {
    pub convert: convert::Setup,
    pub compare: Option<Session>,
    pub review: Option<Review>,
    /// Where the list of recent comparisons is kept (paths only); `None`
    /// keeps none.
    pub data_dir: Option<PathBuf>,
    /// Sessions opened from the app ignore project and user config files.
    pub isolated: bool,
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
    /// Replaced when a comparison is opened from home.
    compare: Mutex<Option<Arc<Mutex<Session>>>>,
    review: Option<Mutex<Review>>,
    data_dir: Option<PathBuf>,
    isolated: bool,
}

impl Shared {
    fn compare(&self) -> Option<Arc<Mutex<Session>>> {
        self.compare
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn is_open(&self, v: View) -> bool {
        match v {
            View::Home | View::Convert => true,
            View::Compare => self.compare().is_some(),
            View::Review => self.review.is_some(),
        }
    }
}

pub fn serve(listener: TcpListener, app: App, extra_hosts: &[String]) -> std::io::Result<()> {
    let shared = Arc::new(Shared {
        convert: app.convert,
        compare: Mutex::new(app.compare.map(|s| Arc::new(Mutex::new(s)))),
        review: app.review.map(Mutex::new),
        data_dir: app.data_dir,
        isolated: app.isolated,
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
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => return page(app, View::Home, home(app)),
        ("GET", "/api/fs") => {
            return match browse::list(&param(&req.query, "path").unwrap_or_default()) {
                Ok(l) => json(&l),
                Err(e) => bad(e),
            }
        }
        ("POST", "/api/open/compare") => {
            return match serde_json::from_slice::<OpenCompare>(&req.body)
                .map_err(|e| e.to_string())
                .and_then(|o| open_from_home(app, o))
            {
                Ok(()) => json(&serde_json::json!({ "open": View::Compare.path() })),
                Err(e) => bad(e),
            }
        }
        _ => {}
    }
    if let Some(res) = convert::route(&app.convert, &req, View::Convert.path()) {
        return framed(app, View::Convert, res);
    }
    for view in [View::Compare, View::Review] {
        let Some(req) = mount(&req, view.path()) else {
            continue;
        };
        let res = match view {
            View::Compare => app.compare().map(|s| {
                let mut s = s.lock().unwrap_or_else(|e| e.into_inner());
                server::route(&mut s, &req)
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
.standalone-only { display: none !important; }
</style>
";

#[derive(Serialize)]
struct Home {
    version: &'static str,
    convert: ConvertCard,
    compare: Option<CompareCard>,
    review: Option<ReviewCard>,
    /// Comparisons opened before, newest first.
    recent: Vec<Recent>,
    /// Every dialect, for the compare form: `(id, name)`.
    dialects: Vec<(&'static str, &'static str)>,
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
        compare: app.compare().map(|s| {
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
        recent: app.data_dir.as_deref().map(recent).unwrap_or_default(),
        dialects: Dialect::ALL
            .iter()
            .map(|d| (d.id(), d.display_name()))
            .collect(),
    };
    let json = serde_json::to_string(&payload)
        .unwrap()
        .replace("</", "<\\/");
    HOME_HTML.replace(
        "/*__SQLDIFF_PAYLOAD__*/null",
        &format!("/*__SQLDIFF_PAYLOAD__*/{json}"),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenCompare {
    a: String,
    b: String,
    /// Dialect ids; absent: from `dialect_map` or the path convention.
    #[serde(default)]
    dialect_a: Option<String>,
    #[serde(default)]
    dialect_b: Option<String>,
}

fn open_from_home(app: &Shared, o: OpenCompare) -> Result<(), String> {
    let dialect = |id: &Option<String>| -> Result<Option<Dialect>, String> {
        id.as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| s.parse().map_err(|_| format!("unknown dialect `{s}`")))
            .transpose()
    };
    let (da, db) = (dialect(&o.dialect_a)?, dialect(&o.dialect_b)?);
    let session = open_compare(Path::new(&o.a), Path::new(&o.b), da, db, app.isolated)?;
    let (a, b) = match session.origin() {
        Origin::Files { a, b } | Origin::Trees { a, b } => (a.clone(), b.clone()),
    };
    *app.compare.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(Mutex::new(session)));
    if let Some(dir) = &app.data_dir {
        remember(
            dir,
            Recent {
                a: a.display().to_string(),
                b: b.display().to_string(),
                dialect_a: da.map(|d| d.id().to_string()),
                dialect_b: db.map(|d| d.id().to_string()),
            },
        );
    }
    Ok(())
}

/// A compare session on two files or two folders, with the config a command
/// run in A's folder loads: the project's `sqldiff.toml` there or above, the
/// user config and the private config beside the app (unless `isolated`).
pub fn open_compare(
    a: &Path,
    b: &Path,
    dialect_a: Option<Dialect>,
    dialect_b: Option<Dialect>,
    isolated: bool,
) -> Result<Session, String> {
    let (a, b) = (browse::absolute(a), browse::absolute(b));
    for p in [&a, &b] {
        if !p.exists() {
            return Err(format!("{}: not found", p.display()));
        }
    }
    if a.is_dir() != b.is_dir() {
        return Err("give two files or two folders, not one of each".into());
    }
    let cwd = match a.is_dir() {
        true => a.clone(),
        false => a.parent().map(Path::to_path_buf).unwrap_or_default(),
    };
    let cfg = Resolved::load(&Sources {
        project: None,
        user: None,
        isolated,
        cwd: Some(cwd),
        packs: Vec::new(),
        cli: config::cli_layer(&[], &[], None).map_err(|e| e.0)?,
    })
    .map_err(|e| e.0)?;
    let store = Store::from_config(&cfg).map_err(|e| e.0)?;
    Session::open_with(Origin::detect(a, b), dialect_a, dialect_b, cfg, None, store)
}

/// One comparison on the home page's recent list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recent {
    pub a: String,
    pub b: String,
    #[serde(default)]
    pub dialect_a: Option<String>,
    #[serde(default)]
    pub dialect_b: Option<String>,
}

/// Most comparisons kept on the recent list.
pub const RECENT_MAX: usize = 8;

fn recent_file(data_dir: &Path) -> PathBuf {
    data_dir.join("gui").join("recent.json")
}

/// The recent comparisons kept in `data_dir`, newest first; none when the
/// file is missing or unreadable.
pub fn recent(data_dir: &Path) -> Vec<Recent> {
    std::fs::read_to_string(recent_file(data_dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Puts `r` first on the recent list. The list is a convenience: a failure
/// to write it is not an error.
fn remember(data_dir: &Path, r: Recent) {
    let mut list = recent(data_dir);
    list.retain(|x| x != &r);
    list.insert(0, r);
    list.truncate(RECENT_MAX);
    let file = recent_file(data_dir);
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(file, serde_json::to_string_pretty(&list).unwrap() + "\n");
}
