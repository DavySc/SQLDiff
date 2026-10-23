//! What the filesystem and git adapters share: which files are SQL
//! artifacts, their dialect, name and kind.

use std::sync::OnceLock;

use regex::Regex;
use sha2::{Digest, Sha256};
use sqldiff_core::Dialect;

use crate::protocol::ObjectInfo;
use crate::Resolved;

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    sqldiff_core::model::hex(&Sha256::digest(bytes))
}

/// True for files a tree source lists: `.sql` files and anything `dialect_map` names.
pub(crate) fn is_candidate(rel: &str, config: &Resolved) -> bool {
    if config.is_ignored(rel) {
        return false;
    }
    let lower = rel.to_ascii_lowercase();
    lower.ends_with(".sql") || config.mapped_dialect(rel).is_some()
}

fn dialect_token(s: &str) -> Option<Dialect> {
    s.parse().ok()
}

/// Dialect from `dialect_map`, else from the path convention: a directory
/// or a dot-separated part of the file name that names a dialect
/// (`tsql/x.sql`, `x.pg.sql`, `pairs/x/db2i.sql`). Otherwise unknown.
pub(crate) fn classify(rel: &str, config: &Resolved) -> (Option<Dialect>, Option<String>) {
    if let Some((d, v)) = config.mapped_dialect(rel) {
        return (
            Some(d),
            Some(v.unwrap_or_else(|| config.dialect_version(d))),
        );
    }
    let mut segments: Vec<&str> = rel.split('/').collect();
    let file = segments.pop().unwrap_or("");
    let found = file
        .split('.')
        .skip(1)
        .chain(file.split('.').take(1))
        .find_map(dialect_token)
        .or_else(|| segments.iter().rev().find_map(|s| dialect_token(s)));
    (found, found.map(|d| config.dialect_version(d)))
}

/// Pairing name: the file name without extension and dialect markers; if
/// nothing is left, the nearest directory that is not a dialect marker.
pub(crate) fn object_name(rel: &str) -> String {
    let mut segments: Vec<&str> = rel.split('/').collect();
    let file = segments.pop().unwrap_or("");
    let mut parts: Vec<&str> = file.split('.').collect();
    if parts.len() > 1 {
        parts.pop();
    }
    let name: Vec<&str> = parts
        .into_iter()
        .filter(|p| dialect_token(p).is_none())
        .collect();
    if !name.is_empty() && name.iter().any(|p| !p.is_empty()) {
        return name.join(".");
    }
    segments
        .iter()
        .rev()
        .find(|s| dialect_token(s).is_none() && !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| file.to_string())
}

/// Object kind from the first CREATE statement.
pub(crate) fn object_kind(text: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?is)\bcreate\s+(?:or\s+(?:replace|alter)\s+)?(?:unique\s+)?(procedure|proc|function|trigger|view|table|index)\b",
        )
        .unwrap()
    });
    match re.captures(text).map(|c| c[1].to_ascii_lowercase()) {
        Some(k) if k == "proc" => "procedure".to_string(),
        Some(k) => k,
        None => "script".to_string(),
    }
}

pub(crate) fn object_info(
    rel: &str,
    text: &str,
    config: &Resolved,
    last_changed: Option<String>,
) -> ObjectInfo {
    let (dialect, version) = classify(rel, config);
    ObjectInfo {
        id: rel.to_string(),
        kind: object_kind(text),
        dialect: dialect.map(|d| d.id().to_string()),
        version,
        name: object_name(rel),
        content_hash: sqldiff_core::artifact::content_hash(text),
        bytes: text.len(),
        last_changed,
        stale: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conventions() {
        let c = Resolved::builtin();
        assert_eq!(classify("tsql/a.sql", &c).0, Some(Dialect::Tsql));
        assert_eq!(
            classify("x/movestock.pg.sql", &c).0,
            Some(Dialect::Postgres)
        );
        assert_eq!(classify("pairs/p1/db2i.sql", &c).0, Some(Dialect::Db2i));
        assert_eq!(classify("misc/a.sql", &c).0, None);
        assert_eq!(classify("tsql/a.sql", &c).1.as_deref(), Some("2022"));
        assert_eq!(object_name("x/movestock.pg.sql"), "movestock");
        assert_eq!(object_name("pairs/p1-lock/db2i.sql"), "p1-lock");
        assert_eq!(object_name("mssql/usp_get.sql"), "usp_get");
        assert_eq!(
            object_kind("-- x\nCREATE OR ALTER PROC dbo.x AS"),
            "procedure"
        );
        assert_eq!(object_kind("create or replace function f()"), "function");
        assert_eq!(object_kind("select 1"), "script");
    }
}
