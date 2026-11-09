//! Words whose letter case the formatter may change.
//!
//! In PostgreSQL and Db2 for i an unquoted word is folded before it is
//! looked up, so changing its case never changes what it names. In T-SQL an
//! identifier compares under the database collation, which can be case
//! sensitive, so only reserved keywords are safe there: a reserved keyword
//! cannot be an undelimited identifier. The T-SQL list is a subset of the
//! reserved keywords of SQL Server 2022, the ones that appear as keywords in
//! everyday code.

use sqldiff_syntax::Dialect;

/// Reserved T-SQL keywords the formatter may recase in every dialect.
const RESERVED: &[&str] = &[
    "ADD",
    "ALL",
    "ALTER",
    "AND",
    "ANY",
    "AS",
    "ASC",
    "BEGIN",
    "BETWEEN",
    "BREAK",
    "BY",
    "CASCADE",
    "CASE",
    "CHECK",
    "CLOSE",
    "CLUSTERED",
    "COALESCE",
    "COLLATE",
    "COLUMN",
    "COMMIT",
    "CONSTRAINT",
    "CONTINUE",
    "CONVERT",
    "CREATE",
    "CROSS",
    "CURRENT",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "CURRENT_USER",
    "CURSOR",
    "DEALLOCATE",
    "DECLARE",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DROP",
    "ELSE",
    "END",
    "ESCAPE",
    "EXCEPT",
    "EXEC",
    "EXECUTE",
    "EXISTS",
    "FETCH",
    "FOR",
    "FOREIGN",
    "FROM",
    "FULL",
    "FUNCTION",
    "GOTO",
    "GRANT",
    "GROUP",
    "HAVING",
    "IDENTITY",
    "IF",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "JOIN",
    "LEFT",
    "LIKE",
    "MERGE",
    "NONCLUSTERED",
    "NOT",
    "NULL",
    "NULLIF",
    "OF",
    "ON",
    "OPEN",
    "OR",
    "ORDER",
    "OUTER",
    "OVER",
    "PIVOT",
    "PRIMARY",
    "PRINT",
    "PROC",
    "PROCEDURE",
    "RAISERROR",
    "REFERENCES",
    "RETURN",
    "REVOKE",
    "RIGHT",
    "ROLLBACK",
    "SELECT",
    "SET",
    "TABLE",
    "THEN",
    "TO",
    "TOP",
    "TRAN",
    "TRANSACTION",
    "TRIGGER",
    "TRUNCATE",
    "UNION",
    "UNIQUE",
    "UNPIVOT",
    "UPDATE",
    "USE",
    "VALUES",
    "VIEW",
    "WHEN",
    "WHERE",
    "WHILE",
    "WITH",
];

/// Whether the formatter may change the case of the unquoted word `upper`
/// (already upper case) in `dialect`.
///
/// In T-SQL the word must also be one the engine compares without regard to
/// case ([`crate::normalize::is_keyword`]): for any other word the engine
/// can only show the change harmless under a collation assumption, so the
/// check would reject the statement and keep it as written.
pub fn recasable(upper: &str, dialect: Dialect) -> bool {
    let engine = crate::normalize::is_keyword(upper);
    match dialect {
        Dialect::Tsql => RESERVED.contains(&upper) && engine,
        _ => RESERVED.contains(&upper) || engine,
    }
}

/// Built-in function and type names, and the literals `TRUE` and `FALSE`,
/// the formatter may also recase (`COUNT(*)`, `varchar(10)`).
///
/// `call` says the word is followed by `(`. A function name counts only
/// then, so a column called like a function keeps its case. PostgreSQL and
/// Db2 for i fold every unquoted word, so there a built-in name, type or
/// literal is always safe. T-SQL compares names under the database
/// collation, which can be case sensitive, and the engine can show a
/// recased function or type name harmless only under that assumption, so
/// the check would keep the statement as written: nothing is recased there
/// beyond [`recasable`].
pub fn recasable_name(upper: &str, dialect: Dialect, call: bool) -> bool {
    use sqldiff_catalog::builtins::{of, Availability};
    if dialect == Dialect::Tsql {
        return false;
    }
    let b = of(dialect);
    let known = |a: Availability| !matches!(a, Availability::Unknown);
    matches!(upper, "TRUE" | "FALSE")
        || (call && known(b.function(upper, "")))
        || known(b.data_type(upper, ""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_names_are_recased_in_postgres_and_db2i_only() {
        for d in [Dialect::Postgres, Dialect::Db2i] {
            assert!(recasable_name("COUNT", d, true), "{d:?}");
            assert!(!recasable_name("COUNT", d, false), "{d:?}: not a call");
            assert!(recasable_name("VARCHAR", d, false), "{d:?}");
            assert!(!recasable_name("MY_TABLE", d, true), "{d:?}");
        }
        assert!(recasable_name("TRUE", Dialect::Postgres, false));
        assert!(!recasable_name("COUNT", Dialect::Tsql, true));
        assert!(!recasable_name("VARCHAR", Dialect::Tsql, false));
    }

    #[test]
    fn tsql_never_recases_unreserved_words() {
        // Non-reserved in T-SQL: may name a column in a case-sensitive database.
        for w in [
            "NAME", "DATE", "INT", "NOCOUNT", "TRY", "THROW", "KEY", "USER",
        ] {
            assert!(!recasable(w, Dialect::Tsql), "{w}");
        }
        assert!(recasable("SELECT", Dialect::Tsql));
        // Reserved, but the engine compares it as a name in T-SQL.
        assert!(!recasable("IDENTITY", Dialect::Tsql));
        assert!(recasable("IDENTITY", Dialect::Postgres));
        assert!(recasable("INT", Dialect::Postgres));
        assert!(recasable("INT", Dialect::Db2i));
    }
}
