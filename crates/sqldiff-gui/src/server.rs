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
//!
//! Every request must name a loopback host (or the address the server is
//! bound to) in `Host`, which defeats DNS rebinding. POSTs must also be
//! `application/json` and, when the browser sends `Origin`, come from this
//! server's own origin, so another web page cannot write decisions (CSRF).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde::Deserialize;

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
const MAX_BODY: usize = 16 * 1024 * 1024;

pub fn serve(listener: TcpListener, session: Session) -> std::io::Result<()> {
    let session = Arc::new(Mutex::new(session));
    let hosts: Arc<Vec<String>> = Arc::new(allowed_hosts(&listener)?);
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

type Response = (&'static str, &'static str, String);

fn json<T: serde::Serialize>(v: &T) -> Response {
    (
        "200 OK",
        "application/json",
        serde_json::to_string(v).unwrap(),
    )
}

fn bad(e: impl ToString) -> Response {
    ("400 Bad Request", "text/plain", e.to_string())
}

fn not_found() -> Response {
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

/// Host names (without port) a request may address.
fn allowed_hosts(listener: &TcpListener) -> std::io::Result<Vec<String>> {
    let ip = listener.local_addr()?.ip();
    let mut v: Vec<String> = ["localhost", "127.0.0.1", "[::1]"]
        .map(String::from)
        .to_vec();
    if !ip.is_unspecified() {
        v.push(match ip {
            std::net::IpAddr::V4(a) => a.to_string(),
            std::net::IpAddr::V6(a) => format!("[{a}]"),
        });
    }
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
    if let Some(o) = &h.origin {
        let ok = o.strip_prefix("http://").is_some_and(|rest| {
            h.host
                .as_deref()
                .is_some_and(|x| rest.eq_ignore_ascii_case(x.trim()))
        });
        if !ok {
            return forbidden("cross-origin request");
        }
    }
    if h.fetch_site.as_deref().is_some_and(|s| s == "cross-site") {
        return forbidden("cross-site request");
    }
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
    None
}

fn handle(
    mut stream: TcpStream,
    session: &Mutex<Session>,
    hosts: &[String],
) -> std::io::Result<()> {
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
        return respond(&mut stream, r);
    }
    let content_length = h.content_length;
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let mut body = Vec::new();
    if method == "POST" {
        if content_length > MAX_BODY {
            return respond(
                &mut stream,
                ("413 Payload Too Large", "text/plain", String::new()),
            );
        }
        body = vec![0; content_length];
        reader.read_exact(&mut body)?;
    }
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

fn respond(stream: &mut TcpStream, (status, ctype, body): Response) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}
