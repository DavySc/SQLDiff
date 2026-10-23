//! Packs (design section 5.2): declarative, versioned, validated at load and
//! never written by the tool.
//!
//! A pack is a `pack.toml` file, or a directory containing one. It has a
//! `[pack]` header and any of the [`Policy`] sections. Project and user
//! config files carry the same sections (see [`crate::config`]). The format
//! is documented in `docs/packs-and-config.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};
use sqldiff_core::{ChangeClass, Dialect, Severity};

use crate::glob::Glob;
use crate::{Error, Result};

/// Pack and config schema version this build reads.
pub const SCHEMA_VERSION: u32 = 1;

fn default_schema() -> u32 {
    SCHEMA_VERSION
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackMeta {
    pub name: String,
    pub version: String,
    #[serde(default = "default_schema")]
    pub schema: u32,
    #[serde(default)]
    pub description: String,
}

/// Everything a pack or config layer can declare. Every section is optional.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Pairing rules, explicit pairs and shared-artifact markers.
    #[serde(default)]
    pub pairing: Pairing,
    /// Which files are written in which dialect, first match wins.
    #[serde(default)]
    pub dialect_map: Vec<DialectMap>,
    /// Target engine version per dialect, e.g. `tsql = "2022"`.
    #[serde(default)]
    pub dialect_versions: BTreeMap<String, String>,
    /// Paths a filesystem or git source skips.
    #[serde(default)]
    pub ignore: Vec<String>,
    /// Template syntax of a code generator (section 5.3).
    #[serde(default)]
    pub template: Option<TemplateSyntax>,
    /// How to split composed output and read provenance markers.
    #[serde(default)]
    pub composition: Vec<CompositionPattern>,
    /// Organisation-supplied functions, added to the built-in catalog.
    #[serde(default)]
    pub helpers: Vec<Helper>,
    /// Comment conventions for review notes, intentional differences, sync partners.
    #[serde(default)]
    pub author_markers: Vec<AuthorMarker>,
    /// Work-item identifiers and keywords in change descriptions.
    #[serde(default)]
    pub change_patterns: Vec<ChangePattern>,
    /// Engine and platform limits, reported as class `limit`.
    #[serde(default)]
    pub limits: Vec<LimitRule>,
    /// Declared facts, by assumption id.
    #[serde(default)]
    pub assumptions: BTreeMap<String, String>,
    /// Local severity and class policy, first match wins.
    #[serde(default)]
    pub overrides: Vec<Override>,
    /// Out-of-process adapters, by name.
    #[serde(default)]
    pub adapters: Vec<AdapterSpec>,
    /// Terms that must never appear in the core repository, fixtures or exports.
    #[serde(default)]
    pub deny_list: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pairing {
    /// Name normalizations, first matching rule wins.
    #[serde(default)]
    pub rules: Vec<PairingRule>,
    /// Explicit pairs by object id; these objects skip rule-based pairing.
    #[serde(default)]
    pub pairs: Vec<ExplicitPair>,
    /// Globs of artifacts shared by all dialects; never paired.
    #[serde(default)]
    pub shared: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Case {
    #[default]
    Lower,
    Upper,
    Preserve,
}

/// Turns an object name into a pairing key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairingRule {
    /// Shown as the reason a pair was made.
    pub name: String,
    /// Only objects in this dialect.
    #[serde(default)]
    pub dialect: Option<String>,
    /// Only objects whose id matches this glob.
    #[serde(default)]
    pub path: Option<String>,
    /// Prefixes removed from the name (first match, compared after case folding).
    #[serde(default)]
    pub strip_prefix: Vec<String>,
    #[serde(default)]
    pub strip_suffix: Vec<String>,
    #[serde(default)]
    pub case: Case,
    /// Characters removed from the name.
    #[serde(default = "default_ignore_chars")]
    pub ignore_chars: String,
}

fn default_ignore_chars() -> String {
    "_-".to_string()
}

impl Default for PairingRule {
    fn default() -> Self {
        PairingRule {
            name: "default".to_string(),
            dialect: None,
            path: None,
            strip_prefix: Vec::new(),
            strip_suffix: Vec::new(),
            case: Case::Lower,
            ignore_chars: default_ignore_chars(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplicitPair {
    pub a: String,
    pub b: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DialectMap {
    pub glob: String,
    pub dialect: String,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateSyntax {
    pub open: String,
    pub close: String,
    /// Regex for placeholder names.
    #[serde(default = "default_placeholder_name")]
    pub name_pattern: String,
    /// Special forms, e.g. conditionals or includes, by name and regex.
    #[serde(default)]
    pub special_forms: Vec<NamedPattern>,
    /// Where parameter values come from, in lookup order.
    #[serde(default)]
    pub parameter_sources: Vec<String>,
    /// What happens when two sources define a parameter: `first`, `last` or `error`.
    #[serde(default)]
    pub collision: Option<String>,
}

fn default_placeholder_name() -> String {
    "[A-Za-z_][A-Za-z0-9_]*".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedPattern {
    pub name: String,
    pub pattern: String,
}

/// A part of composed output is delimited by a begin and an end marker.
/// `begin` may capture a `part` group naming the part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionPattern {
    pub name: String,
    pub begin: String,
    pub end: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Helper {
    pub name: String,
    pub dialect: String,
    #[serde(default)]
    pub returns: Option<String>,
    #[serde(default)]
    pub deterministic: Option<bool>,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerKind {
    ReviewNote,
    IntentionalDifference,
    SyncPartner,
    /// A premise the author states but has not verified; becomes an
    /// author-declared assumption (design section 9).
    Assumption,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorMarker {
    pub kind: MarkerKind,
    /// Regex; the first capture group, if any, is the marker's value.
    pub pattern: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangePattern {
    pub name: String,
    /// Regex; the first capture group, if any, is the extracted identifier.
    pub pattern: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitRule {
    pub id: String,
    pub message: String,
    /// Dialects the rule applies to; empty means all.
    #[serde(default)]
    pub dialects: Vec<String>,
    /// Regex; every match is a finding.
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub max_bytes: Option<usize>,
    #[serde(default)]
    pub max_lines: Option<usize>,
    #[serde(default)]
    pub severity: Option<Severity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Override {
    /// Glob over finding rule ids, e.g. `txn.*`.
    pub rule: String,
    #[serde(default)]
    pub class: Option<ChangeClass>,
    #[serde(default)]
    pub severity: Option<Severity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterSpec {
    pub name: String,
    /// Program and arguments.
    pub command: Vec<String>,
    /// Extra environment for the adapter process. Never secrets.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Credential reference resolved by the adapter: `env:NAME`,
    /// `keychain:SERVICE/ACCOUNT` or `vault:PATH`.
    #[serde(default)]
    pub credential: Option<String>,
}

/// A loaded, validated pack.
#[derive(Debug, Clone, PartialEq)]
pub struct Pack {
    pub meta: PackMeta,
    pub policy: Policy,
    /// The `pack.toml` file.
    pub path: PathBuf,
    /// Content hash of `pack.toml`.
    pub hash: String,
}

impl Pack {
    /// Loads `path/pack.toml` or the `.toml` file at `path`.
    pub fn load(path: &Path) -> Result<Pack> {
        let file = if path.is_dir() {
            path.join("pack.toml")
        } else {
            path.to_path_buf()
        };
        let text = std::fs::read_to_string(&file)
            .map_err(|e| Error::new(format!("{}: {e}", file.display())))?;
        Pack::parse(&text, &file)
    }

    pub fn parse(text: &str, path: &Path) -> Result<Pack> {
        let at = path.display().to_string();
        let mut table: toml::Table =
            toml::from_str(text).map_err(|e| Error::from(e).context(&at))?;
        reject_secrets(&toml::Value::Table(table.clone()), "").map_err(|e| e.context(&at))?;
        let meta = table
            .remove("pack")
            .ok_or_else(|| Error::new(format!("{at}: missing [pack] header")))?;
        let meta: PackMeta = meta
            .try_into()
            .map_err(|e: toml::de::Error| Error::new(format!("{at}: [pack]: {e}")))?;
        if meta.schema > SCHEMA_VERSION {
            return Err(Error::new(format!(
                "{at}: pack schema {} is newer than this build reads ({SCHEMA_VERSION})",
                meta.schema
            )));
        }
        let policy: Policy = toml::Value::Table(table)
            .try_into()
            .map_err(|e: toml::de::Error| Error::new(format!("{at}: {e}")))?;
        policy.validate().map_err(|e| e.context(&at))?;
        Ok(Pack {
            meta,
            policy,
            path: path.to_path_buf(),
            hash: sqldiff_core::artifact::content_hash(text),
        })
    }
}

/// Keys that suggest a secret. Packs and config name credentials by
/// reference; the adapter process resolves them (section 5.5).
const SECRET_KEYS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "private_key",
    "connection_string",
];

pub(crate) fn reject_secrets(v: &toml::Value, at: &str) -> Result<()> {
    match v {
        toml::Value::Table(t) => {
            for (k, v) in t {
                let path = if at.is_empty() {
                    k.clone()
                } else {
                    format!("{at}.{k}")
                };
                let lower = k.to_ascii_lowercase();
                if SECRET_KEYS
                    .iter()
                    .any(|s| lower == *s || lower.ends_with(&format!("_{s}")))
                {
                    return Err(Error::new(format!(
                        "`{path}` looks like a secret; packs and config hold only credential \
                         references (credential = \"env:NAME\"), resolved by the adapter"
                    )));
                }
                reject_secrets(v, &path)?;
            }
        }
        toml::Value::Array(a) => {
            for v in a {
                reject_secrets(v, at)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn parse_dialect(s: &str, at: &str) -> Result<Dialect> {
    s.parse::<Dialect>()
        .map_err(|e| Error::new(format!("{at}: {e}")))
}

impl Policy {
    /// Checks every regex, glob, dialect and credential reference.
    pub fn validate(&self) -> Result<()> {
        for r in &self.pairing.rules {
            if let Some(d) = &r.dialect {
                parse_dialect(d, &format!("pairing rule `{}`", r.name))?;
            }
            if let Some(p) = &r.path {
                Glob::new(p)?;
            }
        }
        for g in &self.pairing.shared {
            Glob::new(g)?;
        }
        for g in &self.ignore {
            Glob::new(g)?;
        }
        for m in &self.dialect_map {
            Glob::new(&m.glob)?;
            parse_dialect(&m.dialect, &format!("dialect_map `{}`", m.glob))?;
        }
        for d in self.dialect_versions.keys() {
            parse_dialect(d, "dialect_versions")?;
        }
        if let Some(t) = &self.template {
            if t.open.is_empty() || t.close.is_empty() {
                return Err(Error::new(
                    "template: open and close delimiters must be non-empty",
                ));
            }
            Regex::new(&t.name_pattern)
                .map_err(|e| Error::new(format!("template.name_pattern: {e}")))?;
            for f in &t.special_forms {
                Regex::new(&f.pattern)
                    .map_err(|e| Error::new(format!("template special form `{}`: {e}", f.name)))?;
            }
            if let Some(c) = &t.collision {
                if !matches!(c.as_str(), "first" | "last" | "error") {
                    return Err(Error::new(format!(
                        "template.collision `{c}`: expected first, last or error"
                    )));
                }
            }
        }
        for c in &self.composition {
            for p in [&c.begin, &c.end] {
                Regex::new(p).map_err(|e| Error::new(format!("composition `{}`: {e}", c.name)))?;
            }
        }
        for h in &self.helpers {
            parse_dialect(&h.dialect, &format!("helper `{}`", h.name))?;
        }
        for m in &self.author_markers {
            Regex::new(&m.pattern).map_err(|e| Error::new(format!("author marker: {e}")))?;
        }
        for c in &self.change_patterns {
            Regex::new(&c.pattern)
                .map_err(|e| Error::new(format!("change pattern `{}`: {e}", c.name)))?;
        }
        for l in &self.limits {
            for d in &l.dialects {
                parse_dialect(d, &format!("limit `{}`", l.id))?;
            }
            if let Some(p) = &l.pattern {
                Regex::new(p).map_err(|e| Error::new(format!("limit `{}`: {e}", l.id)))?;
            }
            if l.pattern.is_none() && l.max_bytes.is_none() && l.max_lines.is_none() {
                return Err(Error::new(format!(
                    "limit `{}`: give pattern, max_bytes or max_lines",
                    l.id
                )));
            }
        }
        for o in &self.overrides {
            Glob::new(&o.rule)?;
        }
        for a in &self.adapters {
            if a.command.is_empty() {
                return Err(Error::new(format!("adapter `{}`: empty command", a.name)));
            }
            if let Some(c) = &a.credential {
                check_credential_ref(c).map_err(|e| e.context(format!("adapter `{}`", a.name)))?;
            }
        }
        Ok(())
    }
}

/// A credential reference names where a secret lives, never the secret.
pub fn check_credential_ref(r: &str) -> Result<()> {
    let (scheme, rest) = r.split_once(':').unwrap_or(("", ""));
    if !matches!(scheme, "env" | "keychain" | "vault") || rest.is_empty() {
        return Err(Error::new(format!(
            "credential `{r}`: expected env:NAME, keychain:SERVICE/ACCOUNT or vault:PATH"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Pack> {
        Pack::parse(text, Path::new("pack.toml"))
    }

    #[test]
    fn minimal_pack() {
        let p = parse("[pack]\nname = \"x\"\nversion = \"1\"\n").unwrap();
        assert_eq!(p.meta.name, "x");
        assert_eq!(p.policy, Policy::default());
    }

    #[test]
    fn rejects_unknown_keys_and_bad_regex() {
        assert!(parse("[pack]\nname = \"x\"\nversion = \"1\"\nbogus = 1\n").is_err());
        let e = parse(
            "[pack]\nname = \"x\"\nversion = \"1\"\n[[change_patterns]]\nname=\"w\"\npattern=\"(\"\n",
        )
        .unwrap_err();
        assert!(e.0.contains("change pattern `w`"), "{e}");
    }

    #[test]
    fn rejects_secrets() {
        let e = parse(
            "[pack]\nname = \"x\"\nversion = \"1\"\n[[adapters]]\nname=\"a\"\ncommand=[\"a\"]\nenv = { DB_PASSWORD = \"x\" }\n",
        )
        .unwrap_err();
        assert!(e.0.contains("looks like a secret"), "{e}");
        let e = parse(
            "[pack]\nname = \"x\"\nversion = \"1\"\n[[adapters]]\nname=\"a\"\ncommand=[\"a\"]\ncredential=\"hunter2\"\n",
        )
        .unwrap_err();
        assert!(e.0.contains("credential"), "{e}");
    }

    #[test]
    fn rejects_newer_schema() {
        assert!(parse("[pack]\nname = \"x\"\nversion = \"1\"\nschema = 99\n").is_err());
    }
}
