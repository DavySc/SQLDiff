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

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use crate::page;
use crate::review::ReviewState;
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
    state: Option<ReviewState>,
    #[serde(default)]
    comment: Option<String>,
}

/// Largest request body accepted (scratchpad texts).
pub(crate) const MAX_BODY: usize = 16 * 1024 * 1024;

pub fn serve(listener: TcpListener, session: Session) -> std::io::Result<()> {
    let session = Arc::new(Mutex::new(session));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let session = Arc::clone(&session);
        std::thread::spawn(move || {
            let _ = handle(stream, &session);
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

/// Reads one request. `Err(response)` when it must be refused: a body over
/// `MAX_BODY`, or a cross-site POST (an `Origin` that is not this server).
pub(crate) fn read_request(stream: &TcpStream) -> std::io::Result<Result<Request, Response>> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let mut content_length = 0usize;
    let (mut host, mut origin) = (None, None);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim();
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.parse().unwrap_or(0);
            } else if k.eq_ignore_ascii_case("host") {
                host = Some(v.to_string());
            } else if k.eq_ignore_ascii_case("origin") {
                origin = Some(v.to_string());
            }
        }
    }
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let mut body = Vec::new();
    if method == "POST" {
        // Browsers send Origin on cross-site POSTs: only this server's own page may post.
        let own = |o: &str| {
            let authority = o.split_once("://").map_or(o, |(_, a)| a);
            host.as_deref() == Some(authority)
        };
        if origin.as_deref().is_some_and(|o| !own(o)) {
            return Ok(Err((
                "403 Forbidden",
                "text/plain",
                "cross-origin request".to_string(),
            )));
        }
        if content_length > MAX_BODY {
            return Ok(Err(("413 Payload Too Large", "text/plain", String::new())));
        }
        body = vec![0; content_length];
        reader.read_exact(&mut body)?;
    }
    Ok(Ok(Request {
        method,
        path: path.to_string(),
        query: query.to_string(),
        body,
    }))
}

fn handle(mut stream: TcpStream, session: &Mutex<Session>) -> std::io::Result<()> {
    let req = match read_request(&stream)? {
        Ok(r) => r,
        Err(res) => return respond(&mut stream, res),
    };
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
        ("GET", "/api/report") => (
            "200 OK",
            "text/markdown; charset=utf-8",
            s.session_report(param(query, "redact").is_some_and(|v| v != "0")),
        ),
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
