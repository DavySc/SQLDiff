//! Dialect identity and the lexical part of each dialect profile.

use std::fmt;
use std::str::FromStr;

/// A SQL dialect. Dialect is a property of an artifact and is never guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Dialect {
    /// Microsoft SQL Server T-SQL.
    Tsql,
    /// PostgreSQL, including PL/pgSQL bodies.
    Postgres,
    /// IBM Db2 for i SQL and SQL PL.
    Db2i,
}

impl Dialect {
    pub const ALL: [Dialect; 3] = [Dialect::Tsql, Dialect::Postgres, Dialect::Db2i];

    /// Short, stable identifier used in output and configuration.
    pub fn id(self) -> &'static str {
        match self {
            Dialect::Tsql => "tsql",
            Dialect::Postgres => "postgres",
            Dialect::Db2i => "db2i",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Dialect::Tsql => "T-SQL",
            Dialect::Postgres => "PostgreSQL",
            Dialect::Db2i => "Db2 for i",
        }
    }

    /// Default target engine version. Version-dependent facts (built-in
    /// availability, MERGE clauses, defaults) are evaluated against it.
    /// Project policy: SQL Server 2022 and PostgreSQL 18.
    pub fn default_version(self) -> &'static str {
        match self {
            Dialect::Tsql => "2022",
            Dialect::Postgres => "18",
            Dialect::Db2i => "7.6",
        }
    }

    pub fn lexical(self) -> &'static LexicalProfile {
        match self {
            Dialect::Tsql => &TSQL,
            Dialect::Postgres => &POSTGRES,
            Dialect::Db2i => &DB2I,
        }
    }
}

impl fmt::Display for Dialect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

impl FromStr for Dialect {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "tsql" | "t-sql" | "mssql" | "sqlserver" | "sql-server" => Ok(Dialect::Tsql),
            "pg" | "postgres" | "postgresql" | "plpgsql" => Ok(Dialect::Postgres),
            "db2" | "db2i" | "db2-i" | "db2fori" | "db2-for-i" => Ok(Dialect::Db2i),
            other => Err(format!(
                "unknown dialect `{other}` (expected one of: tsql, postgres, db2i)"
            )),
        }
    }
}

/// How unquoted identifiers are folded before comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentFolding {
    /// Unquoted identifiers fold to lower case (PostgreSQL).
    Lower,
    /// Unquoted identifiers fold to upper case (Db2).
    Upper,
    /// Identifier comparison follows the catalog collation (SQL Server).
    /// Case-insensitive is the common default but it is an assumption.
    Collation,
}

/// Lexical settings of a dialect profile.
#[derive(Debug)]
pub struct LexicalProfile {
    /// Block comments nest (`/* /* */ */`).
    pub nested_block_comments: bool,
    /// `[ident]` is a quoted identifier.
    pub bracket_identifiers: bool,
    /// `@name` and `@@name` are variables.
    pub at_variables: bool,
    /// `$tag$ ... $tag$` strings and `$1` parameters.
    pub dollar_quoting: bool,
    /// `GO` on its own line separates batches.
    pub go_batches: bool,
    /// `?` is a parameter marker (in PostgreSQL it is an operator character).
    pub question_param: bool,
    /// String prefixes that may directly precede `'` (upper case).
    pub string_prefixes: &'static [&'static str],
    /// Characters that may start an unquoted identifier besides letters and `_`.
    pub extra_ident_start: &'static [char],
    /// Characters that may continue an unquoted identifier besides alphanumerics and `_`.
    pub extra_ident_continue: &'static [char],
    /// Multi-character operators, longest first (dialects without greedy operator lexing).
    pub operators: &'static [&'static str],
    /// PostgreSQL-style greedy operator lexing.
    pub greedy_operators: bool,
    pub folding: IdentFolding,
}

static TSQL: LexicalProfile = LexicalProfile {
    nested_block_comments: true,
    bracket_identifiers: true,
    at_variables: true,
    dollar_quoting: false,
    go_batches: true,
    question_param: true,
    string_prefixes: &["N"],
    extra_ident_start: &['#', '$'],
    extra_ident_continue: &['#', '$', '@'],
    operators: &[
        "<>", "!=", "<=", ">=", "!<", "!>", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "||",
        "::",
    ],
    greedy_operators: false,
    folding: IdentFolding::Collation,
};

static POSTGRES: LexicalProfile = LexicalProfile {
    nested_block_comments: true,
    bracket_identifiers: false,
    at_variables: false,
    dollar_quoting: true,
    go_batches: false,
    question_param: false,
    string_prefixes: &["E", "N", "B", "X"],
    extra_ident_start: &[],
    extra_ident_continue: &['$'],
    operators: &[],
    greedy_operators: true,
    folding: IdentFolding::Lower,
};

static DB2I: LexicalProfile = LexicalProfile {
    // (verify) nesting of bracketed comments on Db2 for i.
    nested_block_comments: true,
    bracket_identifiers: false,
    at_variables: false,
    dollar_quoting: false,
    go_batches: false,
    question_param: true,
    string_prefixes: &["X", "N", "G", "GX", "UX", "BX"],
    extra_ident_start: &['$', '#', '@'],
    extra_ident_continue: &['$', '#', '@'],
    operators: &["<>", "<=", ">=", "!=", "||", "=>"],
    greedy_operators: false,
    folding: IdentFolding::Upper,
};
