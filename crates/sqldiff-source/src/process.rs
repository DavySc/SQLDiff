//! Out-of-process adapters: the client side ([`ProcessSource`]) and the
//! server loop ([`serve`]) that exposes any [`Source`] over stdio. Private
//! adapters are separate programs; they are never linked into the core.

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use crate::pack::AdapterSpec;
use crate::protocol::{
    code, method, ArtifactChunk, ArtifactData, Catalog, CatalogScope, Composition, Describe,
    GetArtifact, HistoryEntry, IdParam, ObjectFilter, ObjectInfo, Pair, PairScope, Request,
    Response, RpcError, Settings, DEFAULT_CHUNK, PROTOCOL_VERSION,
};
use crate::source::Source;
use crate::walk::sha256_hex;
use crate::{Error, Result};

struct Conn {
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

/// A running adapter program.
pub struct ProcessSource {
    name: String,
    child: Mutex<Child>,
    conn: Mutex<Conn>,
    describe: Describe,
}

impl ProcessSource {
    /// Starts the adapter. A credential reference is passed in
    /// `SQLDIFF_CREDENTIAL_REF`; the adapter resolves it.
    pub fn spawn(spec: &AdapterSpec, credential: Option<&str>) -> Result<ProcessSource> {
        let mut cmd = Command::new(&spec.command[0]);
        cmd.args(&spec.command[1..])
            .envs(&spec.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(c) = credential.or(spec.credential.as_deref()) {
            crate::pack::check_credential_ref(c)?;
            cmd.env("SQLDIFF_CREDENTIAL_REF", c);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| Error::new(format!("adapter `{}`: {e}", spec.name)))?;
        let conn = Conn {
            stdin: BufWriter::new(child.stdin.take().expect("piped")),
            stdout: BufReader::new(child.stdout.take().expect("piped")),
            next: 1,
        };
        let mut src = ProcessSource {
            name: spec.name.clone(),
            child: Mutex::new(child),
            conn: Mutex::new(conn),
            describe: Describe {
                name: spec.name.clone(),
                version: String::new(),
                protocol: PROTOCOL_VERSION,
                capabilities: Vec::new(),
                source: String::new(),
            },
        };
        let d: Describe = src.call(method::DESCRIBE, json!({}))?;
        if d.protocol != PROTOCOL_VERSION {
            return Err(Error::new(format!(
                "adapter `{}` speaks protocol {}, expected {PROTOCOL_VERSION}",
                spec.name, d.protocol
            )));
        }
        src.describe = d;
        Ok(src)
    }

    fn call<T: DeserializeOwned>(&self, method: &str, params: impl Serialize) -> Result<T> {
        let at = |e: Error| e.context(format!("adapter `{}`: {method}", self.name));
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| Error::new("adapter connection poisoned"))?;
        let id = conn.next;
        conn.next += 1;
        let req = Request {
            id,
            method: method.to_string(),
            params: serde_json::to_value(params)?,
        };
        let line = serde_json::to_string(&req)?;
        writeln!(conn.stdin, "{line}")
            .and_then(|_| conn.stdin.flush())
            .map_err(|e| at(e.into()))?;
        let mut buf = String::new();
        if conn.stdout.read_line(&mut buf).map_err(|e| at(e.into()))? == 0 {
            return Err(at(Error::new("adapter closed its output")));
        }
        let resp: Response = serde_json::from_str(&buf).map_err(|e| at(e.into()))?;
        if resp.id != id {
            return Err(at(Error::new(format!(
                "response id {} for request {id}",
                resp.id
            ))));
        }
        if let Some(e) = resp.error {
            return Err(at(Error::new(format!("error {}: {}", e.code, e.message))));
        }
        serde_json::from_value(resp.result.unwrap_or(Value::Null)).map_err(|e| at(e.into()))
    }

    fn has(&self, m: &str) -> bool {
        self.describe.capabilities.iter().any(|c| c == m)
    }
}

impl Drop for ProcessSource {
    fn drop(&mut self) {
        let _ = self.call::<Value>(method::SHUTDOWN, json!({}));
        if let Ok(mut c) = self.child.lock() {
            let _ = c.wait();
        }
    }
}

impl Source for ProcessSource {
    fn describe(&self) -> Result<Describe> {
        Ok(self.describe.clone())
    }

    fn list_objects(&self, filter: &ObjectFilter) -> Result<Vec<ObjectInfo>> {
        self.call(method::LIST_OBJECTS, filter)
    }

    /// Reads the artifact in chunks and verifies the reassembled text.
    fn get_artifact(&self, id: &str) -> Result<ArtifactData> {
        let mut text = String::new();
        loop {
            let c: ArtifactChunk = self.call(
                method::GET_ARTIFACT,
                GetArtifact {
                    id: id.to_string(),
                    offset: text.len(),
                    max_bytes: None,
                },
            )?;
            if c.offset != text.len() {
                return Err(Error::new(format!(
                    "{id}: chunk at offset {}, expected {}",
                    c.offset,
                    text.len()
                )));
            }
            text.push_str(&c.data);
            if text.len() > c.total_bytes || (c.data.is_empty() && text.len() < c.total_bytes) {
                return Err(Error::new(format!(
                    "{id}: chunks do not add up to {} bytes",
                    c.total_bytes
                )));
            }
            if text.len() == c.total_bytes {
                if sha256_hex(text.as_bytes()) != c.sha256 {
                    return Err(Error::new(format!("{id}: reassembled text fails its hash")));
                }
                let mut info = c.info;
                info.bytes = text.len();
                return Ok(ArtifactData {
                    info,
                    text,
                    provenance: c.provenance,
                    metadata: c.metadata,
                    template: c.template,
                });
            }
        }
    }

    fn get_pairs(&self, scope: &PairScope) -> Result<Vec<Pair>> {
        if !self.has(method::GET_PAIRS) {
            return Ok(Vec::new());
        }
        self.call(method::GET_PAIRS, scope)
    }

    fn get_composition(&self, id: &str) -> Result<Option<Composition>> {
        if !self.has(method::GET_COMPOSITION) {
            return Ok(None);
        }
        self.call(method::GET_COMPOSITION, IdParam { id: id.to_string() })
    }

    fn get_catalog(&self, scope: &CatalogScope) -> Result<Catalog> {
        if !self.has(method::GET_CATALOG) {
            return Ok(Catalog::default());
        }
        self.call(method::GET_CATALOG, scope)
    }

    fn get_history(&self, id: &str) -> Result<Vec<HistoryEntry>> {
        if !self.has(method::GET_HISTORY) {
            return Ok(Vec::new());
        }
        self.call(method::GET_HISTORY, IdParam { id: id.to_string() })
    }

    fn get_settings(&self, scope: &CatalogScope) -> Result<Settings> {
        if !self.has(method::GET_SETTINGS) {
            return Ok(Settings::new());
        }
        self.call(method::GET_SETTINGS, scope)
    }
}

/// Chunk `n` of an artifact, ending on a character boundary.
pub fn chunk(data: &ArtifactData, offset: usize, max_bytes: usize) -> Result<ArtifactChunk> {
    let text = &data.text;
    if offset > text.len() || !text.is_char_boundary(offset) {
        return Err(Error::new(format!("{}: bad offset {offset}", data.info.id)));
    }
    let mut end = (offset + max_bytes.max(4)).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Ok(ArtifactChunk {
        info: data.info.clone(),
        provenance: data.provenance.clone(),
        metadata: data.metadata.clone(),
        template: data.template.clone(),
        offset,
        data: text[offset..end].to_string(),
        total_bytes: text.len(),
        sha256: sha256_hex(text.as_bytes()),
    })
}

fn params<T: DeserializeOwned + Default>(v: Value) -> std::result::Result<T, RpcError> {
    if v.is_null() {
        return Ok(T::default());
    }
    serde_json::from_value(v).map_err(|e| RpcError {
        code: code::INVALID_PARAMS,
        message: e.to_string(),
    })
}

fn dispatch(
    src: &dyn Source,
    req: Request,
    max_chunk: usize,
) -> std::result::Result<Value, RpcError> {
    let fail = |e: Error| RpcError {
        code: if e.0.contains("no such object") || e.0.contains("not in snapshot") {
            code::NOT_FOUND
        } else {
            code::SOURCE
        },
        message: e.0,
    };
    let to = |v: std::result::Result<Value, serde_json::Error>| {
        v.map_err(|e| RpcError {
            code: code::SOURCE,
            message: e.to_string(),
        })
    };
    let caps = src.describe().map_err(fail)?.capabilities;
    if req.method != method::DESCRIBE
        && req.method != method::SHUTDOWN
        && !caps.contains(&req.method)
    {
        return Err(RpcError {
            code: code::METHOD_NOT_FOUND,
            message: format!("method `{}` not supported", req.method),
        });
    }
    match req.method.as_str() {
        method::DESCRIBE => to(serde_json::to_value(src.describe().map_err(fail)?)),
        method::SHUTDOWN => Ok(Value::Null),
        method::LIST_OBJECTS => {
            let f: ObjectFilter = params(req.params)?;
            to(serde_json::to_value(src.list_objects(&f).map_err(fail)?))
        }
        method::GET_ARTIFACT => {
            let p: GetArtifact = serde_json::from_value(req.params).map_err(|e| RpcError {
                code: code::INVALID_PARAMS,
                message: e.to_string(),
            })?;
            let data = src.get_artifact(&p.id).map_err(fail)?;
            let max = p.max_bytes.unwrap_or(max_chunk).min(max_chunk);
            to(serde_json::to_value(
                chunk(&data, p.offset, max).map_err(fail)?,
            ))
        }
        method::GET_PAIRS => {
            let s: PairScope = params(req.params)?;
            to(serde_json::to_value(src.get_pairs(&s).map_err(fail)?))
        }
        method::GET_COMPOSITION => {
            let p: IdParam = params(req.params)?;
            to(serde_json::to_value(
                src.get_composition(&p.id).map_err(fail)?,
            ))
        }
        method::GET_CATALOG => {
            let s: CatalogScope = params(req.params)?;
            to(serde_json::to_value(src.get_catalog(&s).map_err(fail)?))
        }
        method::GET_HISTORY => {
            let p: IdParam = params(req.params)?;
            to(serde_json::to_value(src.get_history(&p.id).map_err(fail)?))
        }
        method::GET_SETTINGS => {
            let s: CatalogScope = params(req.params)?;
            to(serde_json::to_value(src.get_settings(&s).map_err(fail)?))
        }
        other => Err(RpcError {
            code: code::METHOD_NOT_FOUND,
            message: format!("unknown method `{other}`"),
        }),
    }
}

/// Serves a source over the protocol until `shutdown` or end of input.
pub fn serve(
    src: &dyn Source,
    input: impl BufRead,
    mut output: impl Write,
    max_chunk: usize,
) -> Result<()> {
    let max_chunk = if max_chunk == 0 {
        DEFAULT_CHUNK
    } else {
        max_chunk
    };
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let (resp, stop) = match serde_json::from_str::<Request>(&line) {
            Err(e) => {
                let code = if serde_json::from_str::<Value>(&line).is_ok() {
                    code::INVALID_REQUEST
                } else {
                    code::PARSE
                };
                (
                    Response {
                        id: 0,
                        result: None,
                        error: Some(RpcError {
                            code,
                            message: e.to_string(),
                        }),
                    },
                    false,
                )
            }
            Ok(req) => {
                let id = req.id;
                let stop = req.method == method::SHUTDOWN;
                let r = dispatch(src, req, max_chunk);
                let (result, error) = match r {
                    Ok(v) => (Some(v), None),
                    Err(e) => (None, Some(e)),
                };
                (Response { id, result, error }, stop)
            }
        };
        writeln!(output, "{}", serde_json::to_string(&resp)?)?;
        output.flush()?;
        if stop {
            break;
        }
    }
    Ok(())
}
