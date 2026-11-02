//! SQL Server features that cannot be converted automatically.
//!
//! Most refusals mean "no rule yet": someone can write one. A few SQL
//! Server-only features have no counterpart on the target at all (In-Memory
//! OLTP, SQL Server Audit, a hierarchyid, …); no rule can ever make their
//! output equal. The converter names such a refusal with a [`Feature`] and
//! the report keeps it in a bucket of its own, apart from the work that is
//! left to do.
//!
//! This changes no status. A unit with an impossible feature is still
//! `not converted`: its original stays as a comment, it never counts as
//! converted, it still keeps its file from counting as converted, and the
//! engine never compared it. [`Counts::impossible`](crate::Counts) and
//! [`Totals::impossible`](crate::tree::Totals) count such units as a part of
//! the not converted ones, never instead of them.
//!
//! The marker is the reason's text: [`reason`] builds
//! `impossible to convert automatically [id] (label): why`, and [`of_note`]
//! reads it back, so it survives wherever a note's text goes (comments in
//! the output, reports, the review ledger's `kept_notes`).

use crate::{Note, Status};

/// One SQL Server-only feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Feature {
    /// Stable id, kebab-case: what reports and the GUI group by.
    pub id: &'static str,
    /// What the feature is called in SQL Server's documentation.
    pub label: &'static str,
}

const fn f(id: &'static str, label: &'static str) -> Feature {
    Feature { id, label }
}

pub const IN_MEMORY_OLTP: Feature = f("in-memory-oltp", "In-Memory OLTP");
pub const SECURITY_POLICY: Feature = f("security-policy", "row-level security policy");
pub const SERVER_AUDIT: Feature = f("sql-server-audit", "SQL Server Audit");
pub const POLYBASE: Feature = f("polybase", "PolyBase external data");
pub const FULL_TEXT: Feature = f("full-text", "full-text search");
pub const COLUMNSTORE: Feature = f("columnstore", "columnstore index");
pub const PARTITIONING: Feature = f("partition-scheme", "partition function or scheme");
pub const XML_SCHEMA: Feature = f("xml-schema-collection", "XML schema collection");
pub const HIERARCHYID: Feature = f("hierarchyid", "hierarchyid type");
pub const HASH_FUNCTIONS: Feature = f("sql-server-hash", "CHECKSUM, BINARY_CHECKSUM and HASHBYTES");
pub const SYSTEM_TIME_QUERY: Feature = f("system-time-query", "FOR SYSTEM_TIME query");
pub const ALWAYS_ENCRYPTED: Feature = f("always-encrypted", "Always Encrypted keys");
pub const INSTANCE_METADATA: Feature = f("instance-metadata", "instance and catalog metadata");
pub const DATABASE_LEVEL: Feature = f(
    "database-level",
    "database-level statements (USE, CREATE and ALTER DATABASE)",
);
pub const BULK_INSERT: Feature = f("bulk-insert-format", "BULK INSERT file formats");

/// Every feature the converter can name, in report order.
pub const ALL: [Feature; 15] = [
    IN_MEMORY_OLTP,
    SECURITY_POLICY,
    SERVER_AUDIT,
    POLYBASE,
    FULL_TEXT,
    COLUMNSTORE,
    PARTITIONING,
    XML_SCHEMA,
    HIERARCHYID,
    HASH_FUNCTIONS,
    SYSTEM_TIME_QUERY,
    ALWAYS_ENCRYPTED,
    INSTANCE_METADATA,
    DATABASE_LEVEL,
    BULK_INSERT,
];

const PREFIX: &str = "impossible to convert automatically [";

/// The refusal text for `why` under feature `x`.
pub fn reason(x: Feature, why: &str) -> String {
    format!("{PREFIX}{}] ({}): {why}", x.id, x.label)
}

/// The feature a reason names, if it was built by [`reason`].
pub fn of_reason(text: &str) -> Option<Feature> {
    let id = text.strip_prefix(PREFIX)?.split_once(']')?.0;
    ALL.iter().find(|x| x.id == id).copied()
}

/// The feature a unit's notes name: its worst notes are not-converted
/// ones and the first of them says so.
pub fn of_note(notes: &[Note]) -> Option<Feature> {
    notes
        .iter()
        .find(|n| n.status == Status::NotConverted)
        .and_then(|n| of_reason(&n.text))
}

/// The feature of `CREATE|ALTER|DROP <words>` statements with no rule, from
/// the words after the verb (upper case).
pub fn of_ddl(words: &[&str]) -> Option<Feature> {
    let w = |i: usize| words.get(i).copied().unwrap_or("");
    Some(match (w(0), w(1)) {
        ("SECURITY", "POLICY") => SECURITY_POLICY,
        ("SERVER" | "DATABASE", "AUDIT") => SERVER_AUDIT,
        ("EXTERNAL", _) => POLYBASE,
        ("FULLTEXT", _) => FULL_TEXT,
        ("XML", "SCHEMA") => XML_SCHEMA,
        ("PARTITION", "FUNCTION" | "SCHEME") => PARTITIONING,
        ("COLUMN", "MASTER" | "ENCRYPTION") => ALWAYS_ENCRYPTED,
        ("DATABASE", _) => DATABASE_LEVEL,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reason_reads_back() {
        for x in ALL {
            assert_eq!(of_reason(&reason(x, "why")), Some(x));
        }
        assert_eq!(of_reason("no rule for this statement yet"), None);
        assert_eq!(
            of_reason("impossible to convert automatically [nope] (x): y"),
            None
        );
    }

    #[test]
    fn ids_are_unique() {
        for (i, a) in ALL.iter().enumerate() {
            for b in &ALL[i + 1..] {
                assert_ne!(a.id, b.id);
            }
        }
    }
}
