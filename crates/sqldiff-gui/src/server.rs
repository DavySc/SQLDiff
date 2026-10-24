//! A minimal local HTTP server for the GUI (std only, loopback by default).
//!
//! `GET /` serves the page: the review queue, or the pair view when the
//! session holds one pair or `?a=<path>&b=<path>` (or `?pair=N`) names one.
//! The API re-diffs scratchpad texts, flips assumptions, records review
//! decisions and re-reads the sources; nothing is ever written to a source.
//!
//! - `GET /api/pair/N`: pair view payload
//! - `GET /api/session`: session info and queue
//! - `GET /api/report[?redact=1]`: session report (Markdown)
//! - `POST /api/diff` `{"pair": N, "a": "...", "b": "..."}`: scratchpad re-diff
//! - `POST /api/assumption` `{"id": "...", "flip": "open|confirmed|denied"}`
//! - `POST /api/review` `{"fingerprint", "snapshot", "state": "accepted|rejected|commented|null", "comment"}`
//! - `POST /api/refresh`: re-read sources
//! - `GET /convert`, `POST /api/convert`, `POST /api/compare`: convert mode
//!   and its scratch compare (see `convert`)
//! - `GET /api/what_if?pair=N&line=L&side=a|b&error=<kind>`: "what if this
//!   statement fails?" (design §6.1)
//! - `GET /api/overlay?pair=N`: transaction brackets of both sides (design §6.2)
//!
//! Every request must name a loopback host (or the address the server is
//! bound to) in `Host`, which defeats DNS rebinding. POSTs must also be
//! `application/json` and, when the browser sends `Origin`, come from this
//! server's own origin, so another web page cannot write decisions (CSRF).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use sqldiff_core::ir::ErrorKind;
use sqldiff_core::model::Side;

use crate::page;
use crate::review::State;
use crate::session::{Flip, Session};

#[derive(Deserialize)]
struct Edit {
    #[serde(default)]
    pair: usize,
    a: String,
    b: String,
}

#[derive(Deserialize)]
struct SetFlip {
    id: String,
    flip: Flip,
}

#[derive(Deserialize)]
struct Review {
    fingerprint: String,
    snapshot: String,
    state: Option<State>,
    #[serde(default)]
    comment: Option<String>,
}

/// Largest request body accepted (scratchpad texts).
pub(crate) const MAX_BODY: usize = 16 * 1024 * 1024;

pub fn serve(listener: TcpListener, session: Session) -> std::io::Result<()> {
    serve_with_hosts(listener, session, &[])
}

/// [`serve`], also answering requests addressed to `extra` host names
/// (`--allow-host`, for a server bound to a non-loopback address).
pub fn serve_with_hosts(
    listener: TcpListener,
    session: Session,
    extra: &[String],
) -> std::io::Result<()> {
    let session = Arc::new(Mutex::new(session));
    let hosts: Arc<Vec<String>> = Arc::new(allowed_hosts(&listener, extra)?);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let session = Arc::clone(&session);
        let hosts = Arc::clone(&hosts);
        std::thread::spawn(move || {
            let _ = handle(stream, &session, &hosts);
        });
    }
    Ok(())
}

pub(crate) type Response = (&'static str, &'static str, String);

pub(crate) fn json<T: serde::Serialize>(v: &T) -> Response {
    (
        "200 OK",
        "application/json",
        serde_json::to_string(v).unwrap(),
    )
}

pub(crate) fn bad(e: impl ToString) -> Response {
    ("400 Bad Request", "text/plain", e.to_string())
}

pub(crate) fn not_found() -> Response {
    ("404 Not Found", "text/plain", "not found".to_string())
}

/// Query parameter, percent-decoded.
fn param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (k == key).then(|| decode(v))
    })
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(v) => {
                        out.push(v);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A parsed request.
pub(crate) struct Request {
    pub method: String,
    pub path: String,
    pub query: String,
    pub body: Vec<u8>,
}

/// Convert mode's start: T-SQL → PostgreSQL with the sample.
fn convert_setup() -> &'static crate::convert::Setup {
    static SETUP: std::sync::OnceLock<crate::convert::Setup> = std::sync::OnceLock::new();
    SETUP.get_or_init(|| crate::convert::Setup {
        from: sqldiff_core::Dialect::Tsql,
        to: sqldiff_core::Dialect::Postgres,
        template: None,
        text: crate::convert::SAMPLE.to_string(),
        name: String::new(),
    })
}

/// Host names (without port) a request may address: loopback names, the
/// address the listener is bound to (`0.0.0.0` included) and `extra`.
/// A DNS name only passes when it is listed, which defeats DNS rebinding.
pub(crate) fn allowed_hosts(
    listener: &TcpListener,
    extra: &[String],
) -> std::io::Result<Vec<String>> {
    let ip = listener.local_addr()?.ip();
    let mut v: Vec<String> = ["localhost", "127.0.0.1", "[::1]"]
        .map(String::from)
        .to_vec();
    v.push(match ip {
        std::net::IpAddr::V4(a) => a.to_string(),
        std::net::IpAddr::V6(a) => format!("[{a}]"),
    });
    v.extend(extra.iter().map(|h| host_name(h).to_string()));
    Ok(v)
}

/// `host[:port]` without the port.
fn host_name(h: &str) -> &str {
    let h = h.trim();
    if h.starts_with('[') {
        return h.find(']').map_or(h, |e| &h[..=e]);
    }
    h.rsplit_once(':').map_or(h, |(n, _)| n)
}

#[derive(Default)]
struct Headers {
    content_length: usize,
    host: Option<String>,
    origin: Option<String>,
    content_type: Option<String>,
    fetch_site: Option<String>,
}

/// Why a request is refused before it is routed, if it is.
fn refuse(method: &str, h: &Headers, hosts: &[String]) -> Option<Response> {
    let forbidden = |why: &str| Some(("403 Forbidden", "text/plain", why.to_string()));
    let host_ok = h
        .host
        .as_deref()
        .is_some_and(|x| hosts.iter().any(|a| a.eq_ignore_ascii_case(host_name(x))));
    if !host_ok {
        return forbidden("unexpected Host");
    }
    if method != "POST" {
        return None;
    }
    // Browsers send Origin on cross-site POSTs: only this server's own page may post.
    if let Some(o) = &h.origin {
        let authority = o.split_once("://").map_or(o.as_str(), |(_, a)| a);
        if !h
            .host
            .as_deref()
            .is_some_and(|x| authority.eq_ignore_ascii_case(x.trim()))
        {
            return forbidden("cross-origin request");
        }
    }
    if h.fetch_site.as_deref() == Some("cross-site") {
        return forbidden("cross-site request");
    }
    // Not a CORS-safelisted type, so a foreign page cannot send it without a preflight.
    let json = h.content_type.as_deref().is_some_and(|c| {
        c.trim()
            .to_ascii_lowercase()
            .starts_with("application/json")
    });
    if !json {
        return Some((
            "415 Unsupported Media Type",
            "text/plain",
            "POST bodies must be application/json".to_string(),
        ));
    }
    if h.content_length > MAX_BODY {
        return Some(("413 Payload Too Large", "text/plain", String::new()));
    }
    None
}

/// Reads one request. `Err(response)` when it must be refused (see
/// [`refuse`]); a refused body is drained first, so the client reads the
/// response instead of a reset connection.
pub(crate) fn read_request(
    stream: &TcpStream,
    hosts: &[String],
) -> std::io::Result<Result<Request, Response>> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let mut h = Headers::default();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim().to_string();
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => h.content_length = v.parse().unwrap_or(0),
                "host" => h.host = Some(v),
                "origin" => h.origin = Some(v),
                "content-type" => h.content_type = Some(v),
                "sec-fetch-site" => h.fetch_site = Some(v),
                _ => {}
            }
        }
    }
    if let Some(r) = refuse(&method, &h, hosts) {
        let drain = h.content_length.min(MAX_BODY) as u64;
        let _ = std::io::copy(&mut reader.by_ref().take(drain), &mut std::io::sink());
        return Ok(Err(r));
    }
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let mut body = Vec::new();
    if method == "POST" {
        body = vec![0; h.content_length];
        reader.read_exact(&mut body)?;
    }
    Ok(Ok(Request {
        method,
        path: path.to_string(),
        query: query.to_string(),
        body,
    }))
}

fn handle(
    mut stream: TcpStream,
    session: &Mutex<Session>,
    hosts: &[String],
) -> std::io::Result<()> {
    let req = match read_request(&stream, hosts)? {
        Ok(r) => r,
        Err(res) => return respond(&mut stream, res),
    };
    // Convert mode lives beside the pair view, at /convert.
    if let Some(res) = crate::convert::route(convert_setup(), &req, "/convert") {
        return respond(&mut stream, res);
    }
    let (method, path, query, body) = (req.method, req.path.as_str(), req.query.as_str(), req.body);
    let mut s = session.lock().unwrap_or_else(|e| e.into_inner());
    let res: Response = match (method.as_str(), path) {
        ("GET", "/") => {
            let open = match (param(query, "a"), param(query, "b"), param(query, "pair")) {
                (Some(a), Some(b), _) => s.find(&a, &b),
                (_, _, Some(n)) => n.parse().ok().filter(|n| *n < s.len()),
                _ => None,
            };
            (
                "200 OK",
                "text/html; charset=utf-8",
                page(&s.page(open, true)),
            )
        }
        ("GET", "/api/session") => {
            json(&serde_json::json!({ "session": s.info(), "queue": s.queue() }))
        }
        ("GET", "/api/report") => {
            match s.session_report(param(query, "redact").is_some_and(|v| v != "0")) {
                Ok(md) => ("200 OK", "text/markdown; charset=utf-8", md),
                Err(e) => ("500 Internal Server Error", "text/plain", e),
            }
        }
        ("GET", p) if p.starts_with("/api/pair/") => {
            match p["/api/pair/".len()..]
                .parse()
                .ok()
                .and_then(|i| s.payload(i, true))
            {
                Some(pl) => json(&pl),
                None => not_found(),
            }
        }
        ("GET", "/api/what_if") => {
            let pair = param(query, "pair")
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            let line = param(query, "line").and_then(|l| l.parse::<usize>().ok());
            let side = if param(query, "side").as_deref() == Some("b") {
                Side::B
            } else {
                Side::A
            };
            let kind = serde_json::from_value::<ErrorKind>(serde_json::Value::String(
                param(query, "error").unwrap_or_else(|| "constraint".into()),
            ));
            match (line, kind) {
                (Some(line), Ok(kind)) => match s.what_if(pair, side, line, kind) {
                    Some(w) => json(&w),
                    None => not_found(),
                },
                _ => bad("give line=N, and optionally pair=N, side=a|b and error=<kind>"),
            }
        }
        ("GET", "/api/overlay") => {
            match s.overlay(
                param(query, "pair")
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0),
            ) {
                Some(o) => json(&o),
                None => not_found(),
            }
        }
        ("POST", "/api/diff") => match serde_json::from_slice::<Edit>(&body) {
            Ok(e) => match s.scratch(e.pair, e.a, e.b) {
                Some(pl) => json(&pl),
                None => bad("no such pair"),
            },
            Err(e) => bad(e),
        },
        ("POST", "/api/assumption") => match serde_json::from_slice::<SetFlip>(&body) {
            Ok(f) => {
                s.set_flip(&f.id, f.flip);
                json(&serde_json::json!({ "session": s.info(), "queue": s.queue() }))
            }
            Err(e) => bad(e),
        },
        ("POST", "/api/review") => match serde_json::from_slice::<Review>(&body)
            .map_err(|e| e.to_string())
            .and_then(|r| s.review(&r.fingerprint, &r.snapshot, r.state, r.comment))
        {
            Ok(()) => json(&serde_json::json!({ "session": s.info(), "queue": s.queue() })),
            Err(e) => bad(e),
        },
        ("POST", "/api/refresh") => match s.refresh() {
            Ok(()) => json(&serde_json::json!({ "session": s.info(), "queue": s.queue() })),
            Err(e) => ("500 Internal Server Error", "text/plain", e),
        },
        _ => not_found(),
    };
    drop(s);
    respond(&mut stream, res)
}

pub(crate) fn respond(
    stream: &mut TcpStream,
    (status, ctype, body): Response,
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}
