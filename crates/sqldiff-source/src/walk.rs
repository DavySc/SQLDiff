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
pub fn classify(rel: &str, config: &Resolved) -> (Option<Dialect>, Option<String>) {
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

/// The objects of a file: one per CREATE when it holds several (ids
/// `path#name`, see [`crate::split`]), else the file itself. `dialect`
/// overrides the configured one.
pub(crate) fn objects(
    rel: &str,
    text: &str,
    config: &Resolved,
    last_changed: Option<String>,
    dialect: Option<Dialect>,
) -> Vec<ObjectInfo> {
    let mut whole = object_info(rel, text, config, last_changed);
    let Some(d) = dialect.or_else(|| whole.dialect.as_deref().and_then(|d| d.parse().ok())) else {
        return vec![whole];
    };
    whole.dialect = Some(d.id().to_string());
    let parts = crate::split::parts(text, d);
    if parts.is_empty() {
        whole.defines = defined_name(text, d);
        return vec![whole];
    }
    parts
        .into_iter()
        .map(|p| {
            let t = crate::split::blank_outside(text, &p.ranges);
            ObjectInfo {
                id: format!("{rel}#{}", p.fragment),
                defines: p.defines(),
                kind: p.kind,
                name: p.name,
                content_hash: sqldiff_core::artifact::content_hash(&t),
                ..whole.clone()
            }
        })
        .collect()
}

/// The info [`objects`] lists for a file read whole.
pub(crate) fn whole_info(
    rel: &str,
    text: &str,
    config: &Resolved,
    last_changed: Option<String>,
    dialect: Option<Dialect>,
) -> ObjectInfo {
    objects(rel, text, config, last_changed.clone(), dialect)
        .into_iter()
        .find(|o| o.id == rel)
        .unwrap_or_else(|| object_info(rel, text, config, last_changed))
}

/// Text of an object id read through `read` (relative path → file text):
/// a whole file, or one object of it for `path#name`.
pub(crate) fn object_text(
    id: &str,
    config: &Resolved,
    dialect: Option<Dialect>,
    read: impl Fn(&str) -> crate::Result<String>,
) -> crate::Result<(String, String)> {
    let (rel, frag) = crate::split::split_id(id);
    let Some(frag) = frag else {
        return Ok((id.to_string(), read(id)?));
    };
    let text = read(rel)?;
    let d = dialect.or(classify(rel, config).0).ok_or_else(|| {
        crate::Error::new(format!("{id}: dialect unknown, cannot find the object"))
    })?;
    crate::split::object_text(&text, d, frag)
        .map(|t| (rel.to_string(), t))
        .ok_or_else(|| crate::Error::new(format!("{id}: no object `{frag}` in {rel}")))
}

/// The one object a file defines, as `relation:schema.name` or
/// `routine:schema.name` folded to lower case, from its top-level DDL.
/// `None` when the file defines no object, or more than one: pairing by it
/// is then not attempted. Tables and views share a namespace, as do
/// procedures and functions (a T-SQL procedure is often a PostgreSQL
/// function).
pub fn defined_name(text: &str, dialect: Dialect) -> Option<String> {
    use sqldiff_core::catalog::{ddl, TableKind};
    let cat = ddl::load(text, dialect, "");
    let key = |ns: &str, schema: &Option<String>, name: &str| {
        format!(
            "{ns}:{}.{}",
            schema.as_deref().unwrap_or("").to_lowercase(),
            name.to_lowercase()
        )
    };
    let mut names: Vec<String> = cat
        .tables
        .iter()
        .filter(|t| matches!(t.kind, TableKind::Table | TableKind::View))
        .map(|t| key("relation", &t.schema, &t.name))
        .chain(
            cat.routines
                .iter()
                .map(|r| key("routine", &r.schema, &r.name)),
        )
        .collect();
    names.dedup();
    (names.len() == 1).then(|| names.remove(0))
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
        defines: None,
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

    #[test]
    fn defined_names() {
        let t = Dialect::Tsql;
        assert_eq!(
            defined_name(
                "\u{feff}CREATE VIEW [WebApi].[Cities]\nAS\nSELECT 1 AS x",
                t
            )
            .as_deref(),
            Some("relation:webapi.cities")
        );
        assert_eq!(
            defined_name("CREATE TABLE Application.Cities (CityID int NOT NULL)", t).as_deref(),
            Some("relation:application.cities")
        );
        assert_eq!(
            defined_name("CREATE PROCEDURE dbo.p AS BEGIN SELECT 1; END", t).as_deref(),
            Some("routine:dbo.p")
        );
        // Two objects, or none: no name.
        assert_eq!(
            defined_name("CREATE TABLE a (x int); CREATE TABLE b (y int);", t),
            None
        );
        assert_eq!(defined_name("SELECT 1;", t), None);
    }
}
