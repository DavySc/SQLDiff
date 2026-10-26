//! Names a generator fills in, checked against the target's rules for
//! identifiers. A template converted with its placeholders as names
//! (`ASM-TEMPLATE-PLACEHOLDER`) is right only for values the target accepts
//! there; these checks run on woven instances, where the values are known.

use sqldiff_core::Dialect;
use sqldiff_syntax::{parse, SyntaxKind};

use crate::Status;

/// PostgreSQL's longest identifier in bytes (`NAMEDATALEN - 1`, the
/// default build); longer names are truncated, with only a notice.
pub const PG_MAX_NAME: usize = 63;
/// The longest SQL identifier on Db2 for i and SQL Server.
pub const LONG_MAX_NAME: usize = 128;

/// PostgreSQL 18 keywords that cannot be a table or column name unquoted:
/// categories "reserved" and "reserved (can be function or type)" of
/// `src/include/parser/kwlist.h` on REL_18_STABLE (`RESERVED_KEYWORD`,
/// `TYPE_FUNC_NAME_KEYWORD`), the same set `pg_get_keywords()` lists as `R`
/// and `T` on PostgreSQL 16.
const PG_RESERVED: &[&str] = &[
    "all",
    "analyse",
    "analyze",
    "and",
    "any",
    "array",
    "as",
    "asc",
    "asymmetric",
    "authorization",
    "binary",
    "both",
    "case",
    "cast",
    "check",
    "collate",
    "collation",
    "column",
    "concurrently",
    "constraint",
    "create",
    "cross",
    "current_catalog",
    "current_date",
    "current_role",
    "current_schema",
    "current_time",
    "current_timestamp",
    "current_user",
    "default",
    "deferrable",
    "desc",
    "distinct",
    "do",
    "else",
    "end",
    "except",
    "false",
    "fetch",
    "for",
    "foreign",
    "freeze",
    "from",
    "full",
    "grant",
    "group",
    "having",
    "ilike",
    "in",
    "initially",
    "inner",
    "intersect",
    "into",
    "is",
    "isnull",
    "join",
    "lateral",
    "leading",
    "left",
    "like",
    "limit",
    "localtime",
    "localtimestamp",
    "natural",
    "not",
    "notnull",
    "null",
    "offset",
    "on",
    "only",
    "or",
    "order",
    "outer",
    "overlaps",
    "placing",
    "primary",
    "references",
    "returning",
    "right",
    "select",
    "session_user",
    "similar",
    "some",
    "symmetric",
    "system_user",
    "table",
    "tablesample",
    "then",
    "to",
    "trailing",
    "true",
    "union",
    "unique",
    "user",
    "using",
    "variadic",
    "verbose",
    "when",
    "where",
    "window",
    "with",
];

/// What a filled-in value made of a name in the woven text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameIssue {
    pub status: Status,
    pub rule: &'static str,
    /// Byte range of the name in the woven text.
    pub start: usize,
    pub end: usize,
    pub text: String,
    /// For a PostgreSQL name over the limit: the name it becomes (folded,
    /// truncated) and the folded full name, so a caller can find two names
    /// that become one.
    pub truncated: Option<(String, String)>,
}

/// Checks every name a value is part of in `text`, woven in `dialect`.
/// `values` are byte ranges of filled-in values. A value inside a string,
/// comment or number is not a name and is not checked here.
pub fn check_values(text: &str, dialect: Dialect, values: &[(usize, usize)]) -> Vec<NameIssue> {
    let root = parse(text, dialect).syntax();
    let mut out: Vec<NameIssue> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for t in root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
    {
        let r = t.text_range();
        let (s, e) = (usize::from(r.start()), usize::from(r.end()));
        if !values.iter().any(|&(vs, ve)| vs < e && s < ve.max(vs + 1)) || !seen.insert(s) {
            continue;
        }
        let quoted = match t.kind() {
            SyntaxKind::IDENT => false,
            SyntaxKind::QUOTED_IDENT => true,
            _ => continue,
        };
        let name = if quoted {
            unquote(t.text())
        } else {
            t.text().to_string()
        };
        let issue = |status, rule, text: String| NameIssue {
            status,
            rule,
            start: s,
            end: e,
            text,
            truncated: None,
        };
        match dialect {
            Dialect::Postgres => {
                // PostgreSQL folds only ASCII letters in a multibyte
                // encoding (UTF-8): `Ä` stays `Ä`.
                let folded = if quoted {
                    name.clone()
                } else {
                    name.to_ascii_lowercase()
                };
                if !quoted && PG_RESERVED.contains(&folded.as_str()) {
                    out.push(issue(
                        Status::Differs,
                        "pg.reserved-name",
                        format!(
                            "`{name}` is a reserved word in PostgreSQL: unquoted it cannot name a table or column"
                        ),
                    ));
                }
                if folded.len() > PG_MAX_NAME {
                    let mut cut = PG_MAX_NAME;
                    while !folded.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    let mut i = issue(
                        Status::Assumptions,
                        "ASM-PG-NAME-TRUNCATED",
                        format!(
                            "`{name}` is {} bytes; PostgreSQL keeps the first {PG_MAX_NAME} (`{}`), so every reference must truncate to the same name and no other name may",
                            folded.len(),
                            &folded[..cut]
                        ),
                    );
                    i.truncated = Some((folded[..cut].to_string(), folded.clone()));
                    out.push(i);
                }
            }
            Dialect::Db2i | Dialect::Tsql => {
                if name.chars().count() > LONG_MAX_NAME {
                    out.push(issue(
                        Status::Differs,
                        "name.too-long",
                        format!(
                            "`{name}` is longer than {LONG_MAX_NAME} characters, the longest {} name",
                            dialect.display_name()
                        ),
                    ));
                }
                if dialect == Dialect::Db2i && !quoted {
                    out.push(issue(
                        Status::Assumptions,
                        "ASM-DB2I-RESERVED-UNCHECKED",
                        format!(
                            "`{name}` was not checked against Db2 for i's reserved words: no verified list is built in"
                        ),
                    ));
                }
            }
        }
    }
    out
}

/// The name inside `"x"` or `[x]`, with doubled closing quotes undone.
fn unquote(q: &str) -> String {
    let inner = q
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .map(|r| r.replace("\"\"", "\""))
        .or_else(|| {
            q.strip_prefix('[')
                .and_then(|r| r.strip_suffix(']'))
                .map(|r| r.replace("]]", "]"))
        });
    inner.unwrap_or_else(|| q.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str, v: &str) -> (usize, usize) {
        let s = text.find(v).unwrap();
        (s, s + v.len())
    }

    #[test]
    fn reserved_words_unquoted_only() {
        let t = "UPDATE public.Order SET x = 1; UPDATE public.\"order\" SET x = 1;";
        let i = check_values(t, Dialect::Postgres, &[at(t, "Order"), at(t, "order")]);
        assert_eq!(i.len(), 1, "{i:?}");
        assert_eq!(i[0].rule, "pg.reserved-name");
        assert_eq!(&t[i[0].start..i[0].end], "Order");
    }

    #[test]
    fn a_glued_value_checks_the_whole_name() {
        // `<%T%>_log` with T = 60 characters: the name is 64 bytes.
        let v = "a".repeat(60);
        let t = format!("CREATE TABLE {v}_log (id integer);");
        let i = check_values(&t, Dialect::Postgres, &[at(&t, &v)]);
        assert_eq!(i.len(), 1, "{i:?}");
        assert_eq!(i[0].rule, "ASM-PG-NAME-TRUNCATED");
        assert_eq!(
            i[0].truncated,
            Some((format!("{v}_lo"), format!("{v}_log")))
        );
    }

    #[test]
    fn only_ascii_letters_fold() {
        // 31 two-byte letters and `_X`: 64 bytes, kept as written.
        let v = format!("{}_X", "Ä".repeat(31));
        let t = format!("CREATE TABLE {v} (id integer);");
        let i = check_values(&t, Dialect::Postgres, &[at(&t, &v)]);
        assert_eq!(
            i[0].truncated,
            Some((
                format!("{}_", "Ä".repeat(31)),
                format!("{}_x", "Ä".repeat(31))
            ))
        );
    }

    #[test]
    fn values_in_strings_are_not_names() {
        let t = "SELECT 'order' AS note FROM t;";
        assert!(check_values(t, Dialect::Postgres, &[at(t, "order")]).is_empty());
    }

    #[test]
    fn db2i_names() {
        let long = "B".repeat(129);
        let t = format!("UPDATE S.{long} SET X = 1; UPDATE S.\"q\" SET X = 1;");
        let i = check_values(&t, Dialect::Db2i, &[at(&t, &long), at(&t, "q")]);
        let rules: Vec<&str> = i.iter().map(|i| i.rule).collect();
        assert_eq!(rules, ["name.too-long", "ASM-DB2I-RESERVED-UNCHECKED"]);
    }
}
