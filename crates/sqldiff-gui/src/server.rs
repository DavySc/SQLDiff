//! A minimal local HTTP server for the pair view (std only, loopback by default).
//!
//! `GET /` serves the page for the files as they are on disk now, so a
//! browser refresh re-reads them. `POST /api/diff` with `{"a": "...", "b": "..."}`
//! re-diffs scratchpad texts; nothing is ever written to a source.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use sqldiff_core::{Artifact, Dialect, Options};

use crate::{build, page};

/// The pair the server shows.
#[derive(Debug, Clone)]
pub struct Pair {
    pub a: PathBuf,
    pub b: PathBuf,
    pub dialect_a: Dialect,
    pub dialect_b: Dialect,
    pub excerpt_lines: usize,
}

impl Pair {
    fn artifacts(&self, texts: Option<(String, String)>) -> Result<(Artifact, Artifact), String> {
        let (ta, tb) = match texts {
            Some(t) => t,
            None => (read(&self.a)?, read(&self.b)?),
        };
        Ok((
            Artifact::new(self.a.display().to_string(), self.dialect_a, ta),
            Artifact::new(self.b.display().to_string(), self.dialect_b, tb),
        ))
    }

    fn options(&self) -> Options {
        Options {
            excerpt_lines: self.excerpt_lines,
        }
    }
}

fn read(p: &PathBuf) -> Result<String, String> {
    std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))
}

#[derive(Deserialize)]
struct Edit {
    a: String,
    b: String,
}

/// Largest request body accepted (scratchpad texts).
const MAX_BODY: usize = 16 * 1024 * 1024;

pub fn serve(listener: TcpListener, pair: Pair) -> std::io::Result<()> {
    let pair = Arc::new(pair);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let pair = Arc::clone(&pair);
        std::thread::spawn(move || {
            let _ = handle(stream, &pair);
        });
    }
    Ok(())
}

fn handle(mut stream: TcpStream, pair: &Pair) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let path = path.split('?').next().unwrap_or("");
    let (status, ctype, body) = match (method.as_str(), path) {
        ("GET", "/") => match pair.artifacts(None) {
            Ok((a, b)) => (
                "200 OK",
                "text/html; charset=utf-8",
                page(&build(&a, &b, &pair.options(), true)),
            ),
            Err(e) => ("500 Internal Server Error", "text/plain", e),
        },
        ("POST", "/api/diff") if content_length <= MAX_BODY => {
            let mut buf = vec![0; content_length];
            reader.read_exact(&mut buf)?;
            match serde_json::from_slice::<Edit>(&buf)
                .map_err(|e| e.to_string())
                .and_then(|e| pair.artifacts(Some((e.a, e.b))))
            {
                Ok((a, b)) => (
                    "200 OK",
                    "application/json",
                    serde_json::to_string(&build(&a, &b, &pair.options(), true)).unwrap(),
                ),
                Err(e) => ("400 Bad Request", "text/plain", e),
            }
        }
        ("POST", "/api/diff") => ("413 Payload Too Large", "text/plain", String::new()),
        _ => ("404 Not Found", "text/plain", "not found".to_string()),
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}
