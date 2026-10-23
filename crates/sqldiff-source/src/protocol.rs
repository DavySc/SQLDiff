//! The adapter protocol (design section 5.4), version 1.
//!
//! Transport: one JSON object per line on the adapter's stdin and stdout.
//! Requests are `{"id": N, "method": "...", "params": {...}}`; responses are
//! `{"id": N, "result": ...}` or `{"id": N, "error": {"code": C, "message": "..."}}`.
//! The adapter writes nothing else to stdout; diagnostics go to stderr.
//! The full description is in `docs/adapter-protocol.md`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sqldiff_core::catalog::{self as core, types::DataType};

pub const PROTOCOL_VERSION: u32 = 1;

/// Default and maximum chunk size for `get_artifact`.
pub const DEFAULT_CHUNK: usize = 1 << 20;

pub mod method {
    pub const DESCRIBE: &str = "describe";
    pub const LIST_OBJECTS: &str = "list_objects";
    pub const GET_ARTIFACT: &str = "get_artifact";
    pub const GET_PAIRS: &str = "get_pairs";
    pub const GET_COMPOSITION: &str = "get_composition";
    pub const GET_CATALOG: &str = "get_catalog";
    pub const GET_HISTORY: &str = "get_history";
    pub const GET_SETTINGS: &str = "get_settings";
    pub const SHUTDOWN: &str = "shutdown";
}

pub mod code {
    pub const PARSE: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const NOT_FOUND: i64 = 1;
    pub const SOURCE: i64 = 2;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Describe {
    pub name: String,
    pub version: String,
    pub protocol: u32,
    /// Methods beyond `describe` that the adapter answers.
    pub capabilities: Vec<String>,
    /// Human-readable description of what is being read (root, revision).
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dialect: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Object ids starting with this prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// RFC 3339; objects changed at or after it (incremental runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_since: Option<String>,
}

impl ObjectFilter {
    pub fn matches(&self, o: &ObjectInfo) -> bool {
        self.dialect.as_ref().map_or(true, |d| {
            d.parse::<sqldiff_core::Dialect>()
                .ok()
                .map(|d| d.id().to_string())
                == o.dialect
        }) && self.kind.as_ref().map_or(true, |k| *k == o.kind)
            && self
                .prefix
                .as_ref()
                .map_or(true, |p| o.id.starts_with(p.as_str()))
            && self.changed_since.as_ref().map_or(true, |t| {
                o.last_changed
                    .as_ref()
                    .map_or(true, |c| c.as_str() >= t.as_str())
            })
    }
}

/// One object a source holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectInfo {
    /// Stable identity within the source, e.g. a relative path.
    pub id: String,
    /// `procedure`, `function`, `trigger`, `view`, `table`, `index` or `script`.
    pub kind: String,
    /// Dialect id (`tsql`, `postgres`, `db2i`); never guessed, so may be absent.
    #[serde(default)]
    pub dialect: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    /// Name used for pairing (file stem without dialect markers, or object name).
    pub name: String,
    /// Normalized content hash (line endings, BOM, trailing whitespace).
    pub content_hash: String,
    /// Exact byte length of the text.
    pub bytes: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_changed: Option<String>,
    /// Generated output older than its inputs; verdicts that depend on it are capped at unknown.
    #[serde(default)]
    pub stale: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// `human`, `generated` or `unknown`.
    pub author_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

/// A whole artifact, as the core sees it after chunk reassembly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactData {
    pub info: ObjectInfo,
    pub text: String,
    #[serde(default)]
    pub provenance: Provenance,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetArtifact {
    pub id: String,
    #[serde(default)]
    pub offset: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<usize>,
}

/// One chunk of `get_artifact`. `offset` and `data.len()` are in bytes of
/// UTF-8 text; chunks end on character boundaries. The client checks that
/// the reassembled text has `total_bytes` bytes and SHA-256 `sha256`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactChunk {
    pub info: ObjectInfo,
    #[serde(default)]
    pub provenance: Provenance,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    pub offset: usize,
    pub data: String,
    pub total_bytes: usize,
    /// SHA-256 of the exact text bytes (not normalized).
    pub sha256: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairScope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a_dialect: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub b_dialect: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Pair {
    pub a: String,
    pub b: String,
    /// Pairing key shared by both sides.
    pub key: String,
    /// What made the pair: `explicit`, `adapter` or the pairing rule name.
    pub rule: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Composition {
    pub id: String,
    pub parts: Vec<CompositionPart>,
    /// The output is older than its templates or parameters; verdicts that
    /// depend on it are capped at unknown.
    #[serde(default)]
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompositionPart {
    pub name: String,
    /// Byte span in the composed output.
    pub start: usize,
    pub end: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default)]
    pub parameters: BTreeMap<String, String>,
    /// Content hash (as `content_hash` in `ObjectInfo`) of the template the
    /// part was generated from; check mode flags the part stale when the
    /// current template hashes differently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_hash: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogScope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    #[serde(default)]
    pub tables: Vec<Table>,
    #[serde(default)]
    pub routines: Vec<RoutineSignature>,
}

impl Catalog {
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty() && self.routines.is_empty()
    }

    /// The core's catalog model, for binding. `origin` names the source in witnesses.
    pub fn to_core(&self, origin: &str) -> core::Catalog {
        let tables = self
            .tables
            .iter()
            .map(|t| {
                let mut x =
                    core::Table::new(t.schema.clone(), t.name.clone(), core::TableKind::Table);
                x.columns = t
                    .columns
                    .iter()
                    .map(|c| core::Column {
                        name: c.name.clone(),
                        data_type: DataType::parse_str(&c.data_type),
                        nullable: c.nullable,
                        identity: false,
                        has_default: false,
                        collation: c.collation.clone(),
                    })
                    .collect();
                x.keys = t
                    .keys
                    .iter()
                    .map(|(name, cols)| core::Key {
                        primary: name == "primary",
                        name: Some(name.clone()),
                        columns: cols.clone(),
                    })
                    .collect();
                x.indexes = t
                    .indexes
                    .iter()
                    .map(|(name, cols)| core::Index {
                        name: name.clone(),
                        unique: false,
                        columns: cols.clone(),
                    })
                    .collect();
                x.triggers = t.triggers.clone();
                x
            })
            .collect();
        let routines = self
            .routines
            .iter()
            .map(|r| core::Routine {
                schema: r.schema.clone(),
                name: r.name.clone(),
                kind: match r.kind.to_ascii_lowercase().as_str() {
                    "function" => core::RoutineKind::Function,
                    "trigger" => core::RoutineKind::Trigger,
                    _ => core::RoutineKind::Procedure,
                },
                params: r
                    .parameters
                    .iter()
                    .map(|p| core::Param {
                        name: p.name.clone(),
                        data_type: DataType::parse_str(&p.data_type),
                        mode: match p.mode.to_ascii_lowercase().as_str() {
                            "out" => core::ParamMode::Out,
                            "inout" => core::ParamMode::InOut,
                            "variadic" => core::ParamMode::Variadic,
                            _ => core::ParamMode::In,
                        },
                        has_default: p.has_default,
                    })
                    .collect(),
                returns: r.returns.as_deref().map(DataType::parse_str),
            })
            .collect();
        core::Catalog {
            origin: origin.to_string(),
            tables,
            routines,
            schemas: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Table {
    #[serde(default)]
    pub schema: Option<String>,
    pub name: String,
    #[serde(default)]
    pub columns: Vec<Column>,
    /// Key name to column names; the primary key is `primary`.
    #[serde(default)]
    pub keys: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub indexes: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub triggers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: String,
    #[serde(default = "yes")]
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collation: Option<String>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutineSignature {
    #[serde(default)]
    pub schema: Option<String>,
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub parameters: Vec<Parameter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Parameter {
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: String,
    /// `in`, `out` or `inout`.
    #[serde(default = "mode_in")]
    pub mode: String,
    /// The parameter has a default, so callers may omit it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub has_default: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn mode_in() -> String {
    "in".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub description: String,
    /// Work-item ids extracted by pack change patterns.
    #[serde(default)]
    pub work_items: Vec<String>,
    /// Id to pass to `get_artifact` for this version's text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdParam {
    pub id: String,
}

/// Declared runtime settings (commit level, naming, isolation, ...).
pub type Settings = BTreeMap<String, String>;
