//! (C3d part 2, review) What a tree may run that no scan of it sees.
//!
//! The certificate sends a catalog table to the checker only when the
//! trees' DDL describes it (`cert::Tables`). Anything a deployment script
//! or a routine runs that the scan cannot read (text built at run time, a
//! call of a routine whose body is not in the tree, a client command, a
//! setting that changes name resolution) may change any table, so it keeps
//! every table out ("dynamic"). The reviews of C3d part 2 found such
//! statements one after another; this module is therefore an allow-list,
//! not a deny-list:
//!
//! * **Statement kinds** ([`Scan::unseen`]): every top-level statement of
//!   a tree (the converted tree, the source tree and the base catalogs) must
//!   be of a kind listed in [`Scan::tsql_statement`] or
//!   [`Scan::pg_statement`]; every other kind, every node the parser could
//!   not read (`ERROR`, an `ERROR_TOKEN` such as a psql meta-command), a
//!   PostgreSQL `U&"..."` name (which no name comparison reads) is dynamic.
//! * **Calls** ([`Scan::body_unseen`]), in routine, trigger and view
//!   bodies, column defaults, checks, computed and generated columns,
//!   index expressions and top-level data statements: each call is of a
//!   routine the tree declares with a body this scan reads
//!   ([`declared_bodies`]), or of a built-in on a short list that runs no
//!   statement and changes no state; T-SQL text runners, `OPENQUERY` and
//!   friends, an `EXEC` of anything else, PostgreSQL `EXECUTE`, `COPY ...
//!   PROGRAM` and a change of `search_path`, the role or the session
//!   authorization are dynamic. A call the scan cannot classify is dynamic.

use sqldiff_syntax::abstract_syntax::{quoted_name, tokens, Tok, K};
use sqldiff_syntax::{parse, Dialect, SyntaxElement, SyntaxKind, SyntaxNode};

/// The T-SQL procedures that run text or change names in ways no scan of
/// the tree can see, and the rowset functions that run a statement on a
/// server, possibly this one. Any mention of one is dynamic SQL, whatever
/// the tree declares.
const TSQL_TEXT_RUNNERS: &[&str] = &[
    "sp_executesql",
    "sp_sqlexec",
    "sp_prepexec",
    "sp_prepexecrpc",
    "sp_prepare",
    "sp_execute",
    "sp_cursoropen",
    "sp_cursorprepare",
    "sp_cursorprepexec",
    "sp_cursorexecute",
    "sp_msforeachtable",
    "sp_msforeachdb",
    "sp_msforeach_worker",
    "sp_rename",
    "xp_cmdshell",
    "sp_send_dbmail",
    "sp_start_job",
    "sp_add_jobstep",
    "sp_update_jobstep",
    "openquery",
    "openrowset",
    "opendatasource",
    "setuser",
];

/// The system procedures an `EXEC` may call without counting as dynamic
/// SQL: they set or drop an extended property, a description no read sees.
/// Unqualified or in `sys` or `dbo`.
const TSQL_HARMLESS_PROCS: &[&str] = &[
    "sp_addextendedproperty",
    "sp_updateextendedproperty",
    "sp_dropextendedproperty",
];

/// T-SQL session options a top-level `SET ... ON|OFF` may change: they
/// change how statements compile and run, which the checker's residual
/// assumptions name (`ASM-TSQL-ANSI-NULLS`, ...) or `cert.rs` reads
/// (`ANSI_PADDING`), and no table.
const TSQL_SET_OPTIONS: &[&str] = &[
    "ANSI_NULLS",
    "ANSI_PADDING",
    "ANSI_WARNINGS",
    "ANSI_NULL_DFLT_ON",
    "ANSI_NULL_DFLT_OFF",
    "ANSI_DEFAULTS",
    "QUOTED_IDENTIFIER",
    "NOCOUNT",
    "XACT_ABORT",
    "ARITHABORT",
    "CONCAT_NULL_YIELDS_NULL",
    "NUMERIC_ROUNDABORT",
];

/// PostgreSQL settings a top-level `SET` may change: client-side settings
/// that change no name resolution and no table (not `search_path`, `role`
/// or `session_authorization`).
const PG_SET_OPTIONS: &[&str] = &[
    "client_encoding",
    "standard_conforming_strings",
    "check_function_bodies",
    "client_min_messages",
];

/// The words that may stand before `(` in a T-SQL statement without
/// calling a routine: clauses, operators, the special forms the grammar
/// spells like calls, type names with a length and the constraint words of
/// a table definition.
const TSQL_PAREN_WORDS: &[&str] = &[
    "IN",
    "EXISTS",
    "VALUES",
    "AS",
    "OVER",
    "CAST",
    "CONVERT",
    "TRY_CAST",
    "TRY_CONVERT",
    "PARSE",
    "TRY_PARSE",
    "IIF",
    "CHOOSE",
    "COALESCE",
    "ISNULL",
    "NULLIF",
    "SELECT",
    "WHERE",
    "AND",
    "OR",
    "NOT",
    "ON",
    "THEN",
    "ELSE",
    "WHEN",
    "CASE",
    "FROM",
    "JOIN",
    "APPLY",
    "INTO",
    "BY",
    "HAVING",
    "SET",
    "TABLE",
    "WITH",
    "UNION",
    "EXCEPT",
    "INTERSECT",
    "ALL",
    "ANY",
    "SOME",
    "IS",
    "LIKE",
    "BETWEEN",
    "IF",
    "WHILE",
    "RETURN",
    "RETURNS",
    "PRINT",
    "RAISERROR",
    "TOP",
    "OUTPUT",
    "OPTION",
    "PATH",
    "ROOT",
    "ELEMENTS",
    "VARCHAR",
    "NVARCHAR",
    "CHAR",
    "NCHAR",
    "VARBINARY",
    "BINARY",
    "DECIMAL",
    "NUMERIC",
    "DEC",
    "DATETIME2",
    "DATETIMEOFFSET",
    "TIME",
    "FLOAT",
    "KEY",
    "PRIMARY",
    "UNIQUE",
    "CHECK",
    "DEFAULT",
    "CLUSTERED",
    "NONCLUSTERED",
    "COLUMNSTORE",
    "INCLUDE",
    "IDENTITY",
    "SYSTEM_TIME",
    "FOREIGN",
    "REFERENCES",
    "CONTAINS",
    "FREETEXT",
    "PIVOT",
    "UNPIVOT",
    "GROUP",
    "GROUPING",
    "SETS",
    "CUBE",
    "ROLLUP",
    "PARTITION",
    "TABLESAMPLE",
    "DISTINCT",
    "USING",
    "BEGIN",
    "END",
    "ROWS",
    "RANGE",
    "OFFSET",
    "FETCH",
    "FOR",
    "MATCHED",
    "HOLDLOCK",
    // `IF UPDATE(col)` in a trigger: whether the statement set the column.
    "UPDATE",
];

/// T-SQL built-ins a statement may call without counting as dynamic SQL:
/// system functions that read their arguments or the session and run no
/// statement and change no state (Microsoft Learn, "What are the SQL
/// database functions?"; from memory). Unqualified.
const TSQL_PURE_BUILTINS: &[&str] = &[
    "abs",
    "acos",
    "app_name",
    "ascii",
    "asin",
    "atan",
    "atn2",
    "avg",
    "binary_checksum",
    "ceiling",
    "char",
    "charindex",
    "checksum",
    "checksum_agg",
    "col_length",
    "col_name",
    "columnproperty",
    "compress",
    "concat",
    "concat_ws",
    "cos",
    "cot",
    "count",
    "count_big",
    "current_timezone",
    "cursor_status",
    "datalength",
    "date_bucket",
    "dateadd",
    "datediff",
    "datediff_big",
    "datefromparts",
    "datename",
    "datepart",
    "datetime2fromparts",
    "datetimefromparts",
    "datetimeoffsetfromparts",
    "datetrunc",
    "day",
    "db_id",
    "db_name",
    "decompress",
    "degrees",
    "dense_rank",
    "difference",
    "eomonth",
    "error_line",
    "error_message",
    "error_number",
    "error_procedure",
    "error_severity",
    "error_state",
    "exp",
    "first_value",
    "floor",
    "format",
    "formatmessage",
    "getdate",
    "getutcdate",
    "greatest",
    "has_perms_by_name",
    "hashbytes",
    "host_name",
    "ident_current",
    "isdate",
    "isjson",
    "isnumeric",
    "is_member",
    "is_rolemember",
    "json_modify",
    "json_query",
    "json_value",
    "lag",
    "last_value",
    "lead",
    "least",
    "left",
    "len",
    "log",
    "log10",
    "lower",
    "ltrim",
    "max",
    "min",
    "month",
    "nchar",
    "newid",
    "ntile",
    "object_id",
    "object_name",
    "object_schema_name",
    "objectproperty",
    "openjson",
    "original_login",
    "patindex",
    "pi",
    "power",
    "quotename",
    "radians",
    "rand",
    "rank",
    "replace",
    "replicate",
    "reverse",
    "right",
    "round",
    "row_number",
    "rtrim",
    "schema_id",
    "schema_name",
    "scope_identity",
    "session_context",
    "sign",
    "sin",
    "soundex",
    "space",
    "sqrt",
    "square",
    "stdev",
    "str",
    "string_agg",
    "string_escape",
    "string_split",
    "stuff",
    "substring",
    "sum",
    "suser_sname",
    "switchoffset",
];

/// More of the same (split to keep each list readable).
const TSQL_PURE_BUILTINS_2: &[&str] = &[
    "sysdatetime",
    "sysdatetimeoffset",
    "sysutcdatetime",
    "tan",
    "timefromparts",
    "todatetimeoffset",
    "translate",
    "trim",
    "type_id",
    "type_name",
    "unicode",
    "upper",
    "user_name",
    "var",
    "xact_state",
    "year",
    "smalldatetimefromparts",
    "columns_updated",
    "trigger_nestlevel",
];

/// The words that may stand before `(` in a PostgreSQL statement without
/// calling a routine: clauses, operators, the special forms the grammar
/// spells like calls (`COALESCE`, `EXTRACT`, ...), type names with a
/// modifier, constraint words and index access methods.
const PG_PAREN_WORDS: &[&str] = &[
    "IN",
    "EXISTS",
    "VALUES",
    "AS",
    "OVER",
    "FILTER",
    "CAST",
    "ANY",
    "ALL",
    "SOME",
    "ROW",
    "ARRAY",
    "USING",
    "ON",
    "WHERE",
    "AND",
    "OR",
    "NOT",
    "SELECT",
    "THEN",
    "ELSE",
    "WHEN",
    "CASE",
    "FROM",
    "JOIN",
    "INTO",
    "BY",
    "HAVING",
    "RETURNING",
    "SET",
    "LATERAL",
    "TABLE",
    "WITH",
    "MATERIALIZED",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "IS",
    "LIKE",
    "ILIKE",
    "BETWEEN",
    "DISTINCT",
    "LIMIT",
    "OFFSET",
    "CONFLICT",
    "DO",
    "CUBE",
    "ROLLUP",
    "SETS",
    "GROUPING",
    "ROWS",
    "RANGE",
    "GROUPS",
    "WITHIN",
    "COALESCE",
    "NULLIF",
    "GREATEST",
    "LEAST",
    "EXTRACT",
    "POSITION",
    "SUBSTRING",
    "OVERLAY",
    "TRIM",
    "INTERVAL",
    "NUMERIC",
    "DECIMAL",
    "VARCHAR",
    "CHAR",
    "CHARACTER",
    "VARYING",
    "TIMESTAMP",
    "TIME",
    "BIT",
    "KEY",
    "PRIMARY",
    "UNIQUE",
    "CHECK",
    "DEFAULT",
    "REFERENCES",
    "FOREIGN",
    "INCLUDE",
    "IDENTITY",
    "EXCLUDE",
    "INHERITS",
    "TO",
    "IF",
    "ELSIF",
    "WHILE",
    "RETURN",
    "QUERY",
    "RETURNS",
    "WINDOW",
    "PARTITION",
    "ORDER",
    "BTREE",
    "HASH",
    "GIST",
    "GIN",
    "SPGIST",
    "BRIN",
    "VARIADIC",
    "STRICT",
    "PERFORM",
];

/// PostgreSQL built-ins a statement may call without counting as dynamic
/// SQL: they read their arguments (or the clock, a setting, a random
/// source, the session's last sequence value) and run no statement and change no state (no sequence, setting,
/// lock, file or other database). Unqualified or in `pg_catalog`.
const PG_PURE_BUILTINS: &[&str] = &[
    "count",
    "sum",
    "min",
    "max",
    "avg",
    "lower",
    "upper",
    "length",
    "char_length",
    "character_length",
    "octet_length",
    "bit_length",
    "abs",
    "round",
    "trunc",
    "floor",
    "ceil",
    "ceiling",
    "mod",
    "div",
    "power",
    "sqrt",
    "sign",
    "exp",
    "ln",
    "log",
    "now",
    "clock_timestamp",
    "statement_timestamp",
    "transaction_timestamp",
    "timeofday",
    "concat",
    "concat_ws",
    "substr",
    "left",
    "right",
    "btrim",
    "ltrim",
    "rtrim",
    "lpad",
    "rpad",
    "replace",
    "translate",
    "reverse",
    "initcap",
    "repeat",
    "chr",
    "ascii",
    "strpos",
    "starts_with",
    "split_part",
    "regexp_replace",
    "regexp_match",
    "regexp_matches",
    "regexp_like",
    "to_char",
    "to_date",
    "to_timestamp",
    "to_number",
    "date_trunc",
    "date_part",
    "date_bin",
    "make_date",
    "make_time",
    "make_timestamp",
    "make_interval",
    "age",
    "format",
    "md5",
    "string_agg",
    "array_agg",
    "array_length",
    "array_to_string",
    "string_to_array",
    "cardinality",
    "unnest",
    "generate_series",
    "bool_and",
    "bool_or",
    "every",
    "row_number",
    "rank",
    "dense_rank",
    "lag",
    "lead",
    "first_value",
    "last_value",
    "json_build_object",
    "jsonb_build_object",
    "to_json",
    "to_jsonb",
    "json_agg",
    "jsonb_agg",
    "quote_ident",
    "quote_literal",
    "quote_nullable",
    "random",
    "gen_random_uuid",
    // The session's last sequence value, read (`SCOPE_IDENTITY()`'s
    // conversion): no sequence advances.
    "lastval",
    "currval",
];

/// A part of a name as the dialect compares it: quotes and brackets
/// removed; T-SQL in lower case (its names compare without case under the
/// collations the slice admits); PostgreSQL folded (`"Foo` for a quoted
/// name whose case matters, as `quoted_name` keeps it).
pub(crate) fn name_part(t: &Tok, d: Dialect) -> Option<String> {
    match t.kind {
        K::Word => Some(t.text.to_lowercase()),
        K::Quoted => {
            let q = quoted_name(&t.text, d);
            Some(match d {
                Dialect::Tsql => q.to_lowercase(),
                _ => q,
            })
        }
        _ => None,
    }
}

/// The dotted name starting at `t[i]`, its parts as `name_part` gives
/// them, and the position after it; `None` when no name starts there or a
/// part is missing (`db..p`).
pub(crate) fn name_at(t: &[Tok], i: usize, d: Dialect) -> Option<(Vec<String>, usize)> {
    let mut parts = vec![name_part(t.get(i)?, d)?];
    let mut j = i + 1;
    while t.get(j).is_some_and(|x| x.kind == K::Dot) {
        parts.push(name_part(t.get(j + 1)?, d)?);
        j += 2;
    }
    Some((parts, j))
}

/// A PostgreSQL routine whose body no scan reads: a body given as a string
/// (`AS '...'`, also a C or internal function's object file and symbol),
/// one the reader keeps as one dollar-quoted token (a language it does not
/// parse), or a `LANGUAGE` other than `plpgsql` and `sql`.
pub(crate) fn pg_body_unread(n: &SyntaxNode) -> bool {
    let toks: Vec<sqldiff_syntax::SyntaxToken> = n
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| !t.kind().is_trivia())
        .collect();
    let quoted = toks.windows(2).any(|w| {
        w[0].kind() == SyntaxKind::IDENT
            && w[0].text().eq_ignore_ascii_case("AS")
            && matches!(w[1].kind(), SyntaxKind::STRING | SyntaxKind::DOLLAR_STRING)
    });
    let language = toks.windows(2).any(|w| {
        w[0].kind() == SyntaxKind::IDENT
            && w[0].text().eq_ignore_ascii_case("LANGUAGE")
            && !matches!(
                w[1].text()
                    .trim_matches(|c| c == '\'' || c == '"')
                    .to_lowercase()
                    .as_str(),
                "plpgsql" | "sql"
            )
    });
    quoted || language || toks.iter().any(|t| t.kind() == SyntaxKind::DOLLAR_STRING)
}

/// The kind word of a routine node (`PROCEDURE`, `FUNCTION`, `VIEW`,
/// `TRIGGER`, `DO`, ...) and its position, past `CREATE`, `OR`, `ALTER`
/// and `REPLACE`.
fn routine_kind(t: &[Tok]) -> Option<(usize, &str)> {
    t.iter()
        .take(4)
        .position(|x| !matches!(x.up.as_str(), "CREATE" | "OR" | "ALTER" | "REPLACE"))
        .map(|k| (k, t[k].up.as_str()))
}

/// The routines a tree declares and whose bodies the scan reads, each
/// name's parts as `name_part` gives them: the targets a call or an `EXEC`
/// may name without counting as dynamic SQL. Not a T-SQL CLR routine
/// (`EXTERNAL NAME`), nor a PostgreSQL routine whose body is not read
/// (`pg_body_unread`), which run what no scan sees.
pub(crate) fn declared_bodies(texts: &[&str], d: Dialect) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for text in texts {
        let root = parse(text, d).syntax();
        for n in root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::ROUTINE)
        {
            let t = tokens(&n);
            let Some((k, kind)) = routine_kind(&t) else {
                continue;
            };
            if !matches!(kind, "PROCEDURE" | "PROC" | "FUNCTION") {
                continue;
            }
            let clr = d == Dialect::Tsql
                && t.windows(2)
                    .any(|w| w[0].up == "EXTERNAL" && w[1].up == "NAME");
            if clr || (d == Dialect::Postgres && pg_body_unread(&n)) {
                continue;
            }
            if let Some((parts, _)) = name_at(&t, k + 1, d) {
                out.push(parts);
            }
        }
    }
    out
}

/// Where a statement's tokens are read by [`Scan::body_unseen`].
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Ctx {
    #[default]
    Plain,
    /// `CREATE INDEX ... ON t (...)`: the name after `ON` is the table.
    Index,
    /// A PostgreSQL `CREATE TRIGGER`: `EXECUTE FUNCTION f()` names the
    /// function it runs, which must be one the tree declares.
    Trigger,
}

/// The allow-list over one dialect's trees ([`Scan::unseen`]).
pub(crate) struct Scan {
    d: Dialect,
    declared: Vec<Vec<String>>,
}

impl Scan {
    /// The scan of the trees `texts` (with the routines they declare).
    pub(crate) fn new(texts: &[&str], d: Dialect) -> Scan {
        Scan {
            d,
            declared: declared_bodies(texts, d),
        }
    }

    /// The tree `root` (one text) may run something no scan of it sees:
    /// a node the parser could not read, a PostgreSQL `U&"..."` name, or a
    /// top-level statement off the allow-list or failing the call rule.
    pub(crate) fn unseen(&self, root: &SyntaxNode) -> bool {
        let unreadable = root.descendants_with_tokens().any(|e| match e {
            SyntaxElement::Node(n) => n.kind() == SyntaxKind::ERROR,
            SyntaxElement::Token(t) => {
                t.kind() == SyntaxKind::ERROR_TOKEN
                    || (self.d == Dialect::Postgres
                        && matches!(t.kind(), SyntaxKind::QUOTED_IDENT | SyntaxKind::STRING)
                        && (t.text().starts_with("U&") || t.text().starts_with("u&")))
            }
        });
        if unreadable {
            return true;
        }
        let top: Vec<SyntaxElement> = match self.d {
            Dialect::Tsql => {
                let mut v = Vec::new();
                for e in root.children_with_tokens() {
                    match e {
                        SyntaxElement::Node(b) if b.kind() == SyntaxKind::BATCH => {
                            let kids: Vec<SyntaxElement> = b.children_with_tokens().collect();
                            if self.batch_unseen(&kids) {
                                return true;
                            }
                        }
                        other => v.push(other),
                    }
                }
                v
            }
            _ => {
                let kids: Vec<SyntaxElement> = root.children_with_tokens().collect();
                return self.batch_unseen(&kids);
            }
        };
        // T-SQL: outside a batch only separators.
        top.iter().any(|e| match e {
            SyntaxElement::Token(t) => !harmless_token(t.kind()),
            SyntaxElement::Node(_) => true,
        })
    }

    /// The top-level elements of a T-SQL batch or a PostgreSQL file.
    fn batch_unseen(&self, kids: &[SyntaxElement]) -> bool {
        let units: Vec<&SyntaxNode> = kids.iter().filter_map(|e| e.as_node()).collect();
        let stray = kids.iter().any(|e| match e {
            SyntaxElement::Token(t) => !harmless_token(t.kind()),
            SyntaxElement::Node(_) => false,
        });
        stray
            || units.iter().enumerate().any(|(i, n)| {
                let last = i + 1 == units.len();
                match n.kind() {
                    SyntaxKind::STATEMENT => match self.d {
                        Dialect::Tsql => self.tsql_statement(n, last),
                        _ => self.pg_statement(n, last),
                    },
                    SyntaxKind::ROUTINE => self.routine(n),
                    _ => true,
                }
            })
    }

    /// A routine definition: T-SQL `CREATE`/`ALTER`/`CREATE OR ALTER`
    /// `PROCEDURE`, `FUNCTION`, `VIEW` or DML `TRIGGER` (not CLR, not on
    /// the database or the server); PostgreSQL `CREATE [OR REPLACE]
    /// FUNCTION` or `PROCEDURE` whose body the scan reads (not a `DO`
    /// block). Its tokens, body included, pass the call rule.
    fn routine(&self, n: &SyntaxNode) -> bool {
        let t = tokens(n);
        let Some((k, kind)) = routine_kind(&t) else {
            return true;
        };
        if !matches!(t[0].up.as_str(), "CREATE" | "ALTER") {
            return true;
        }
        let ok = match self.d {
            Dialect::Tsql => {
                let clr = t
                    .windows(2)
                    .any(|w| w[0].up == "EXTERNAL" && w[1].up == "NAME");
                let server = kind == "TRIGGER"
                    && t.iter()
                        .position(|x| x.up == "ON")
                        .is_some_and(|i| matches!(up(&t, i + 1), Some("DATABASE" | "ALL")));
                matches!(kind, "PROCEDURE" | "PROC" | "FUNCTION" | "VIEW" | "TRIGGER")
                    && !clr
                    && !server
            }
            _ => matches!(kind, "PROCEDURE" | "FUNCTION") && !pg_body_unread(n),
        };
        let _ = k;
        !ok || self.body_unseen(&t, Ctx::Plain) || body_ddl_unseen(n)
    }

    /// A top-level T-SQL statement: `CREATE TABLE`; `CREATE [UNIQUE]
    /// [CLUSTERED | NONCLUSTERED] [COLUMNSTORE] INDEX`; `ALTER TABLE` (a
    /// form that may change a read names its table, `cert.rs`); a bare
    /// `CREATE SCHEMA name [AUTHORIZATION owner]` that ends its batch or
    /// its statement (else its schema elements follow it); `CREATE
    /// SEQUENCE`; `SET` of the session options `TSQL_SET_OPTIONS`; `EXEC`
    /// of an extended-property procedure; `GRANT`, `DENY`, `REVOKE` (they
    /// name their table); `INSERT`, `UPDATE`, `DELETE` (they name theirs).
    /// Everything else is dynamic: `USE`, `MERGE`, `SELECT`, `DECLARE`,
    /// control flow, `DROP`, `CREATE TYPE`, `ASSEMBLY`, `SYNONYM`, `SERVER`,
    /// security policies, `ALTER DATABASE`, any other `EXEC`, a procedure
    /// called without `EXEC`, sqlcmd commands, `DBCC`, ...
    fn tsql_statement(&self, n: &SyntaxNode, last: bool) -> bool {
        let t = tokens(n);
        let w = |i: usize| up(&t, i);
        match (w(0), w(1)) {
            (Some("CREATE"), Some("TABLE")) | (Some("ALTER"), Some("TABLE")) => {
                self.body_unseen(&t, Ctx::Plain)
            }
            (Some("CREATE"), Some("SCHEMA")) => {
                !(schema_bare(&t) && (last || ends_with_semicolon(n)))
            }
            (Some("CREATE"), Some("SEQUENCE")) => self.body_unseen(&t, Ctx::Plain),
            (Some("CREATE"), _) => {
                let mut j = 1;
                while matches!(
                    w(j),
                    Some("UNIQUE" | "CLUSTERED" | "NONCLUSTERED" | "COLUMNSTORE")
                ) {
                    j += 1;
                }
                w(j) != Some("INDEX") || self.body_unseen(&t, Ctx::Index)
            }
            (Some("SET"), _) => !tsql_set_ok(&t),
            (Some("EXEC" | "EXECUTE"), _) => {
                let harmless =
                    name_at(&t, 1, self.d).is_some_and(|(parts, _)| harmless_proc(&parts));
                !harmless || self.body_unseen(&t, Ctx::Plain)
            }
            (Some("GRANT" | "DENY" | "REVOKE"), _) => false,
            (Some("INSERT" | "UPDATE" | "DELETE"), _) => self.body_unseen(&t, Ctx::Plain),
            _ => true,
        }
    }

    /// A top-level PostgreSQL statement: `CREATE [GLOBAL | LOCAL] [TEMP |
    /// TEMPORARY | UNLOGGED] TABLE`; `CREATE [UNIQUE] INDEX`; `ALTER TABLE`;
    /// `CREATE [OR REPLACE] [CONSTRAINT] TRIGGER` (a table trigger, whose
    /// function the tree declares); `CREATE [OR REPLACE] [TEMP | TEMPORARY]
    /// [RECURSIVE] VIEW`; a bare `CREATE SCHEMA` that ends its statement;
    /// `CREATE SEQUENCE`; `COMMENT ON`; `SET` of `PG_SET_OPTIONS`;
    /// `SELECT`, `VALUES`, `WITH`, `TABLE`, `CALL`, `INSERT`, `UPDATE`,
    /// `DELETE`, none writing a system catalog. Everything else is dynamic:
    /// `DO`, `COPY`, `CREATE EXTENSION`, `LANGUAGE`, `CAST`, `OPERATOR`,
    /// `AGGREGATE`, `RULE`, `POLICY`, `EVENT TRIGGER`, a materialized view,
    /// `LOAD`, `ALTER DATABASE`, `ALTER ROLE`, another `SET`, `DROP`,
    /// `GRANT`, transaction control, ... (a psql meta-command is an
    /// `ERROR_TOKEN`, [`Scan::unseen`]).
    fn pg_statement(&self, n: &SyntaxNode, last: bool) -> bool {
        let t = tokens(n);
        let w = |i: usize| up(&t, i);
        if w(0) == Some("CREATE") {
            let mut j = 1;
            if w(1) == Some("OR") && w(2) == Some("REPLACE") {
                j = 3;
            }
            while matches!(
                w(j),
                Some(
                    "GLOBAL" | "LOCAL" | "TEMP" | "TEMPORARY" | "UNLOGGED" | "UNIQUE" | "RECURSIVE"
                )
            ) {
                j += 1;
            }
            return match w(j) {
                Some("TABLE" | "SEQUENCE" | "VIEW") => self.body_unseen(&t, Ctx::Plain),
                Some("INDEX") => self.body_unseen(&t, Ctx::Index),
                Some("TRIGGER") => self.body_unseen(&t, Ctx::Trigger),
                Some("CONSTRAINT") if w(j + 1) == Some("TRIGGER") => {
                    self.body_unseen(&t, Ctx::Trigger)
                }
                Some("SCHEMA") if j == 1 => !(schema_bare(&t) && (last || ends_with_semicolon(n))),
                _ => true,
            };
        }
        match (w(0), w(1)) {
            (Some("ALTER"), Some("TABLE")) => self.body_unseen(&t, Ctx::Plain),
            (Some("COMMENT"), Some("ON")) => false,
            // The converter writes a T-SQL grant as one: it names its table.
            (Some("GRANT" | "REVOKE"), _) => false,
            (Some("SET"), _) => !pg_set_ok(&t),
            (
                Some(
                    "SELECT" | "VALUES" | "WITH" | "TABLE" | "CALL" | "INSERT" | "UPDATE"
                    | "DELETE",
                ),
                _,
            ) => writes_catalog(&t, self.d) || self.body_unseen(&t, Ctx::Plain),
            _ => true,
        }
    }

    /// The call rule over a statement's or a body's tokens: no T-SQL text
    /// runner, rowset function or `EXEC` of what the tree does not show
    /// (`tsql_call_seen`); no PostgreSQL `EXECUTE` (in PL/pgSQL always
    /// text; in a table trigger the function, which must be declared),
    /// `COPY ... PROGRAM` or change of `search_path`, role or session
    /// authorization; and every call of a routine the tree declares or of
    /// a pure built-in ([`Scan::calls_unseen`]).
    fn body_unseen(&self, t: &[Tok], ctx: Ctx) -> bool {
        match self.d {
            Dialect::Tsql => {
                let mention = t.iter().any(|x| {
                    matches!(x.kind, K::Word | K::Quoted)
                        && name_part(x, self.d)
                            .is_some_and(|w| TSQL_TEXT_RUNNERS.contains(&w.as_str()))
                });
                let runs_text = |j: usize| {
                    t.get(j).map_or(true, |y| {
                        matches!(y.kind, K::LParen | K::Var | K::Str | K::Param)
                    })
                };
                let exec = t.iter().enumerate().any(|(i, x)| {
                    if !(x.kind == K::Word && (x.up == "EXEC" || x.up == "EXECUTE")) {
                        return false;
                    }
                    // `WITH [option, ] EXECUTE AS {CALLER | SELF | OWNER |
                    // 'user'}` in a routine's head: an option, no call.
                    let option = up(t, i + 1) == Some("AS")
                        && i > 0
                        && (t[i - 1].kind == K::Comma || up(t, i - 1) == Some("WITH"));
                    if option {
                        return false;
                    }
                    // `EXEC @rc = p` captures p's return code.
                    let rc = t.get(i + 1).is_some_and(|y| y.kind == K::Var)
                        && t.get(i + 2)
                            .is_some_and(|y| y.kind == K::Op && y.text == "=");
                    let j = if rc { i + 3 } else { i + 1 };
                    runs_text(j) || !self.tsql_call_seen(t, j)
                });
                mention || exec || self.calls_unseen(t, ctx)
            }
            _ => {
                let trigger_fn = |i: usize| {
                    ctx == Ctx::Trigger
                        && matches!(up(t, i + 1), Some("FUNCTION" | "PROCEDURE"))
                        && name_at(t, i + 2, self.d).is_some_and(|(p, _)| self.own(&p))
                };
                let execute = t
                    .iter()
                    .enumerate()
                    .any(|(i, x)| x.kind == K::Word && x.up == "EXECUTE" && !trigger_fn(i));
                let program = t
                    .windows(2)
                    .any(|w| w[0].up == "FROM" && w[1].up == "PROGRAM")
                    || t.windows(2)
                        .any(|w| w[0].up == "TO" && w[1].up == "PROGRAM");
                let setting = t.windows(2).any(|w| {
                    w[0].up == "SET"
                        && matches!(
                            w[1].up.as_str(),
                            "SEARCH_PATH" | "ROLE" | "SESSION" | "LOCAL"
                        )
                }) || t.windows(3).any(|w| {
                    w[0].up == "RESET" || (w[0].up == "SESSION" && w[1].up == "AUTHORIZATION")
                });
                execute || program || setting || self.calls_unseen(t, ctx)
            }
        }
    }

    /// A routine the tree declares with a body the scan reads, by the
    /// name's parts; never a one-part `sp_`/`xp_` (T-SQL, the system
    /// procedure is found first) or a `pg_` name (`pg_catalog` is searched
    /// first).
    fn own(&self, parts: &[String]) -> bool {
        let last = parts.last().map(String::as_str).unwrap_or_default();
        let shadowed = match self.d {
            Dialect::Tsql => last.starts_with("sp_") || last.starts_with("xp_"),
            _ => last.starts_with("pg_"),
        };
        !shadowed && self.declared.iter().any(|d| d == parts)
    }

    /// A T-SQL call of the procedure named at `t[j]` that runs nothing a
    /// scan of the tree cannot see: one the tree declares, compared with
    /// brackets and case removed, or an extended-property procedure.
    fn tsql_call_seen(&self, t: &[Tok], j: usize) -> bool {
        let Some((parts, _)) = name_at(t, j, Dialect::Tsql) else {
            return false;
        };
        harmless_proc(&parts) || self.own(&parts)
    }

    /// A call the scan cannot show harmless: a name before `(` that is no
    /// clause or type word, no type after `::` or a T-SQL variable, no
    /// table or routine being defined (after `INTO`, `TABLE`, `REFERENCES`,
    /// `FUNCTION`, ...; not after `AS`, which may start a T-SQL computed
    /// column's expression), no index's table, and neither a routine the tree
    /// declares nor a pure built-in. Anything unsure counts as a call.
    fn calls_unseen(&self, t: &[Tok], ctx: Ctx) -> bool {
        let d = self.d;
        let words: &[&str] = match d {
            Dialect::Tsql => TSQL_PAREN_WORDS,
            _ => PG_PAREN_WORDS,
        };
        (0..t.len()).any(|i| {
            if !(matches!(t[i].kind, K::Word | K::Quoted)
                && t.get(i + 1).is_some_and(|y| y.kind == K::LParen))
            {
                return false;
            }
            let mut s = i;
            while s >= 2 && t[s - 1].kind == K::Dot && matches!(t[s - 2].kind, K::Word | K::Quoted)
            {
                s -= 2;
            }
            if s == i && t[i].kind == K::Word && words.contains(&t[i].up.as_str()) {
                return false;
            }
            let prev = s.checked_sub(1).map(|k| &t[k]);
            let defined = prev.is_some_and(|p| {
                (p.kind == K::Op && p.text == "::")
                    || (d == Dialect::Tsql && p.kind == K::Var)
                    || (p.kind == K::Word
                        && matches!(
                            p.up.as_str(),
                            "INTO"
                                | "INSERT"
                                | "TABLE"
                                | "REFERENCES"
                                | "FUNCTION"
                                | "PROCEDURE"
                                | "PROC"
                                | "TRIGGER"
                                | "VIEW"
                                | "INDEX"
                                | "TYPE"
                                | "WITH"
                                | "RECURSIVE"
                        ))
                    // `CREATE INDEX [name] ON t (...)`, at the top level or
                    // in a body: the table, after an `INDEX` just before.
                    || (p.kind == K::Word
                        && p.up == "ON"
                        && (ctx == Ctx::Index
                            || t[s.saturating_sub(7)..s].iter().any(|x| x.kind == K::Word && x.up == "INDEX")))
            });
            if defined {
                return false;
            }
            let Some((parts, _)) = name_at(t, s, d) else {
                return true;
            };
            let last = parts.last().map(String::as_str).unwrap_or_default();
            let builtin = match d {
                Dialect::Tsql => {
                    parts.len() == 1
                        && (TSQL_PURE_BUILTINS.contains(&last) || TSQL_PURE_BUILTINS_2.contains(&last))
                }
                _ => {
                    PG_PURE_BUILTINS.contains(&last)
                        && (parts.len() == 1 || (parts.len() == 2 && parts[0] == "pg_catalog"))
                }
            };
            !(builtin || self.own(&parts))
        })
    }
}

/// DDL in a routine body runs when the routine runs, which a top-level
/// call or another routine may make happen at deployment: it is held to
/// an allow-list too. A body statement that starts with a DDL word is
/// allowed only as `CREATE [GLOBAL | LOCAL] [TEMP | TEMPORARY | UNLOGGED]
/// TABLE`, `CREATE [UNIQUE] [CLUSTERED | NONCLUSTERED] INDEX`, `ALTER
/// TABLE`, `DROP TABLE` or `DROP INDEX` (each names its table, `cert.rs`);
/// any other (`ALTER DATABASE`, `ALTER ROLE`, `CREATE EXTENSION`, `CREATE
/// RULE`, `CREATE VIEW`, `GRANT`, `USE`, `DBCC`, `COPY`, ...) is dynamic.
fn body_ddl_unseen(n: &SyntaxNode) -> bool {
    n.descendants()
        .filter(|s| s.kind() == SyntaxKind::STATEMENT)
        .any(|s| {
            let t = tokens(&s);
            let w = |i: usize| up(&t, i);
            let ddl = matches!(
                w(0),
                Some(
                    "CREATE"
                        | "ALTER"
                        | "DROP"
                        | "ENABLE"
                        | "DISABLE"
                        | "USE"
                        | "DBCC"
                        | "GRANT"
                        | "DENY"
                        | "REVOKE"
                        | "COPY"
                        | "LOAD"
                        | "DO"
                        | "IMPORT"
                        | "REINDEX"
                        | "CLUSTER"
                        | "VACUUM"
                        | "SECURITY"
                        | "REASSIGN"
                        | "COMMENT"
                        | "BULK"
                )
            );
            if !ddl {
                return false;
            }
            let mut j = 1;
            while matches!(
                w(j),
                Some(
                    "GLOBAL"
                        | "LOCAL"
                        | "TEMP"
                        | "TEMPORARY"
                        | "UNLOGGED"
                        | "UNIQUE"
                        | "CLUSTERED"
                        | "NONCLUSTERED"
                )
            ) {
                j += 1;
            }
            !match w(0) {
                Some("CREATE") => matches!(w(j), Some("TABLE" | "INDEX")),
                Some("ALTER") => w(1) == Some("TABLE"),
                Some("DROP") => matches!(w(1), Some("TABLE" | "INDEX")),
                _ => false,
            }
        })
}

/// The upper-case word at `t[i]`, if a word.
fn up(t: &[Tok], i: usize) -> Option<&str> {
    t.get(i)
        .filter(|x| x.kind == K::Word)
        .map(|x| x.up.as_str())
}

/// A token that may stand between statements: whitespace, comments, a
/// terminator, a batch separator.
fn harmless_token(k: SyntaxKind) -> bool {
    k.is_trivia()
        || matches!(
            k,
            SyntaxKind::SEMICOLON | SyntaxKind::INFERRED_TERMINATOR | SyntaxKind::BATCH_SEP
        )
}

/// An extended-property procedure, unqualified or in `sys` or `dbo`.
fn harmless_proc(parts: &[String]) -> bool {
    let last = parts.last().map(String::as_str).unwrap_or_default();
    TSQL_HARMLESS_PROCS.contains(&last)
        && (parts.len() == 1 || (parts.len() == 2 && matches!(parts[0].as_str(), "sys" | "dbo")))
}

/// `CREATE SCHEMA name [AUTHORIZATION owner]` and nothing else
/// (PostgreSQL also `IF NOT EXISTS`, and `AUTHORIZATION owner` alone).
fn schema_bare(t: &[Tok]) -> bool {
    let mut i = 2;
    if up(t, i) == Some("IF") {
        if !(up(t, i + 1) == Some("NOT") && up(t, i + 2) == Some("EXISTS")) {
            return false;
        }
        i += 3;
    }
    let name = |i: usize| {
        t.get(i)
            .is_some_and(|x| matches!(x.kind, K::Word | K::Quoted))
    };
    if up(t, i) == Some("AUTHORIZATION") {
        return name(i + 1) && t.len() == i + 2;
    }
    if !name(i) {
        return false;
    }
    i += 1;
    if up(t, i) == Some("AUTHORIZATION") {
        return name(i + 1) && t.len() == i + 2;
    }
    t.len() == i
}

/// The statement ends with its own `;` (a schema's elements end there).
fn ends_with_semicolon(n: &SyntaxNode) -> bool {
    n.children_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| !t.kind().is_trivia())
        .last()
        .is_some_and(|t| t.kind() == SyntaxKind::SEMICOLON)
}

/// `SET opt[, opt...] {ON | OFF}` over `TSQL_SET_OPTIONS`.
fn tsql_set_ok(t: &[Tok]) -> bool {
    let Some((last, opts)) = t[1..].split_last() else {
        return false;
    };
    matches!(last.up.as_str(), "ON" | "OFF")
        && !opts.is_empty()
        && opts.iter().enumerate().all(|(i, x)| {
            if i % 2 == 1 {
                x.kind == K::Comma
            } else {
                x.kind == K::Word && TSQL_SET_OPTIONS.contains(&x.up.as_str())
            }
        })
        && opts.len() % 2 == 1
}

/// `SET [SESSION] name {= | TO} value` over `PG_SET_OPTIONS`, the value
/// words, strings or numbers.
fn pg_set_ok(t: &[Tok]) -> bool {
    let mut i = 1;
    if up(t, i) == Some("SESSION") {
        i += 1;
    }
    let Some(name) = t.get(i).filter(|x| x.kind == K::Word) else {
        return false;
    };
    let eq = t
        .get(i + 1)
        .is_some_and(|x| (x.kind == K::Op && x.text == "=") || x.up == "TO");
    PG_SET_OPTIONS.contains(&name.text.to_lowercase().as_str())
        && eq
        && t.len() > i + 2
        && t[i + 2..]
            .iter()
            .all(|x| matches!(x.kind, K::Word | K::Str | K::Num | K::Comma))
}

/// A data statement that may write a system catalog: one that writes
/// (`INSERT`, `UPDATE`, `DELETE`, `MERGE` anywhere in it) and names a
/// `pg_` relation, `pg_catalog` or `information_schema`.
fn writes_catalog(t: &[Tok], d: Dialect) -> bool {
    let writes = t.iter().any(|x| {
        x.kind == K::Word && matches!(x.up.as_str(), "INSERT" | "UPDATE" | "DELETE" | "MERGE")
    });
    writes
        && t.iter().any(|x| {
            name_part(x, d).is_some_and(|w| {
                let w = w.trim_start_matches('"').to_lowercase();
                w.starts_with("pg_") || w == "information_schema"
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unseen(texts: &[&str], d: Dialect) -> bool {
        let scan = Scan::new(texts, d);
        texts.iter().any(|x| scan.unseen(&parse(x, d).syntax()))
    }

    /// The converter's own output and the corpus's DDL are on the list.
    #[test]
    fn what_the_converter_writes_is_on_the_list() {
        let pg = "CREATE SCHEMA sales;\n\
                  CREATE SEQUENCE sales.s START WITH 1;\n\
                  CREATE TABLE sales.item (id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
                  name varchar(20) NOT NULL, price numeric(12,2) NOT NULL, \
                  added timestamp(6) NOT NULL DEFAULT statement_timestamp()::timestamp, \
                  code character varying(4) CHECK (length(code) > 0));\n\
                  ALTER TABLE sales.item ADD CONSTRAINT ck CHECK (price > 0);\n\
                  CREATE UNIQUE INDEX ix ON sales.item USING btree (lower(name)) INCLUDE (price);\n\
                  CREATE OR REPLACE VIEW sales.v AS SELECT id, upper(name) AS n FROM sales.item;\n\
                  CREATE OR REPLACE FUNCTION sales.t_fn() RETURNS trigger LANGUAGE plpgsql AS $$ \
                  BEGIN NEW.added := clock_timestamp(); RETURN NEW; END $$;\n\
                  CREATE TRIGGER tr BEFORE UPDATE ON sales.item FOR EACH ROW EXECUTE FUNCTION sales.t_fn();\n\
                  CREATE OR REPLACE PROCEDURE look(p_id integer, INOUT p_q integer) LANGUAGE plpgsql AS $$\n\
                  DECLARE v varchar(20);\nBEGIN\n  p_q := coalesce(p_q, 0) + abs(p_id);\n  \
                  SELECT COUNT(*) INTO p_q FROM sales.item WHERE id = p_id;\n  \
                  CALL sales_log(p_id);\nEND;\n$$;\n\
                  CREATE OR REPLACE PROCEDURE sales_log(p integer) LANGUAGE plpgsql AS $$ BEGIN NULL; END $$;\n\
                  COMMENT ON TABLE sales.item IS 'x';\n\
                  SET client_min_messages = warning;\n\
                  INSERT INTO sales.item (name, price, added) VALUES ('a', 1, now());\n\
                  SELECT lower('A'), pg_catalog.upper('b');\nCALL look(1, NULL);\n";
        assert!(!unseen(&[pg], Dialect::Postgres));
        let tsql = "CREATE SCHEMA [Sales] AUTHORIZATION dbo;\nGO\n\
                    SET ANSI_NULLS, QUOTED_IDENTIFIER ON;\nGO\n\
                    CREATE TABLE [Sales].[Item] ([Id] int NOT NULL CONSTRAINT [PK] PRIMARY KEY CLUSTERED, \
                    Name varchar(20) NOT NULL DEFAULT (''), Price decimal(12,2) NULL, \
                    Added datetime2(7) NOT NULL DEFAULT (SYSDATETIME()), Total AS (Price * 2), \
                    INDEX ix NONCLUSTERED (Name));\nGO\n\
                    ALTER TABLE [Sales].[Item] WITH CHECK ADD CONSTRAINT ck CHECK (LEN(Name) > 0);\nGO\n\
                    CREATE NONCLUSTERED INDEX ix2 ON Sales.Item (Price) INCLUDE (Name) WHERE Price > 0;\nGO\n\
                    CREATE SEQUENCE Sales.S AS int START WITH 1;\nGO\n\
                    CREATE PROCEDURE dbo.Log @n int AS BEGIN SET NOCOUNT ON; END\nGO\n\
                    CREATE PROCEDURE dbo.p @id int, @q int OUTPUT WITH EXECUTE AS OWNER AS\nBEGIN\n  \
                    DECLARE @s varchar(20) = CONVERT(varchar(20), @id), @t TABLE (a int);\n  \
                    SELECT @q = COUNT(*) FROM Sales.Item WHERE Id = @id AND ISNULL(Name, '') <> LOWER(@s);\n  \
                    EXEC dbo.Log @q;\n  EXEC @q = dbo.Log 1;\nEND\nGO\n\
                    CREATE PROCEDURE dbo.tmp AS BEGIN CREATE TABLE #t (a int); \
                    CREATE INDEX ix ON #t (a); DROP TABLE #t; END\nGO\n\
                    CREATE VIEW dbo.v AS SELECT Id, UPPER(Name) AS N FROM Sales.Item;\nGO\n\
                    CREATE TRIGGER dbo.tr ON Sales.Item AFTER INSERT AS BEGIN SET NOCOUNT ON; END\nGO\n\
                    EXEC sys.sp_addextendedproperty @name = N'd', @value = N'x';\nGO\n\
                    GRANT SELECT ON Sales.Item TO someone;\nGO\n\
                    INSERT INTO Sales.Item (Name) VALUES ('a');\nGO\n";
        let scan = Scan::new(&[tsql], Dialect::Tsql);
        for batch in tsql.split("GO\n").filter(|b| !b.trim().is_empty()) {
            assert!(
                !scan.unseen(&parse(batch, Dialect::Tsql).syntax()),
                "{batch}"
            );
        }
        assert!(!unseen(&[tsql], Dialect::Tsql));
    }

    /// Each kind off the list, and each call the scan cannot show
    /// harmless, is dynamic (the third review's reproductions among them).
    #[test]
    fn everything_else_is_dynamic() {
        for (text, d) in [
            // F1: a name no comparison reads.
            ("ALTER TABLE U&\"\\0069tem\" ENABLE ROW LEVEL SECURITY;\n", Dialect::Postgres),
            ("CREATE TABLE a (n integer) INHERITS (U&\"\\0069tem\");\n", Dialect::Postgres),
            // F2: calls run later, client commands, catalog writes.
            ("CREATE FUNCTION setup() RETURNS void LANGUAGE plpgsql AS $$ BEGIN \
              PERFORM dblink_exec('x', 'y'); END $$;\n", Dialect::Postgres),
            ("CREATE TABLE log (id integer, note text DEFAULT dblink_exec('x', 'y'));\n", Dialect::Postgres),
            ("CREATE VIEW v AS SELECT dblink_exec('x', 'y') AS r;\n", Dialect::Postgres),
            ("CREATE RULE r AS ON INSERT TO log DO ALSO SELECT 1;\n", Dialect::Postgres),
            ("SELECT format('x %I', 'y') \\gexec\n", Dialect::Postgres),
            ("\\i extra.sql\n", Dialect::Postgres),
            ("\\! psql -f extra.sql\n", Dialect::Postgres),
            ("CREATE EXTENSION dblink;\n", Dialect::Postgres),
            ("CREATE OPERATOR ### (RIGHTARG = text, FUNCTION = f);\n", Dialect::Postgres),
            ("CREATE EVENT TRIGGER et ON ddl_command_end EXECUTE FUNCTION f();\n", Dialect::Postgres),
            ("UPDATE pg_catalog.pg_class SET relhasrules = relhasrules WHERE relname = 'item';\n", Dialect::Postgres),
            ("DO $$ BEGIN NULL; END $$;\n", Dialect::Postgres),
            ("COPY item FROM PROGRAM 'cat x';\n", Dialect::Postgres),
            ("CREATE MATERIALIZED VIEW v AS SELECT 1;\n", Dialect::Postgres),
            ("CREATE TRIGGER tr AFTER INSERT ON log FOR EACH ROW EXECUTE FUNCTION nowhere();\n", Dialect::Postgres),
            ("CREATE SCHEMA s CREATE TABLE t (a integer)\n", Dialect::Postgres),
            ("SELECT not_declared();\n", Dialect::Postgres),
            ("SELECT nextval('s');\n", Dialect::Postgres),
            ("DROP TABLE IF EXISTS x;\n", Dialect::Postgres),
            ("BEGIN;\n", Dialect::Postgres),
            // F4: search_path, roles, databases.
            ("ALTER DATABASE db SET search_path = app, public;\n", Dialect::Postgres),
            ("ALTER ROLE r SET search_path = app;\n", Dialect::Postgres),
            ("SET search_path = app, public;\n", Dialect::Postgres),
            ("SET ROLE r;\n", Dialect::Postgres),
            ("SELECT set_config('search_path', 'app', false);\n", Dialect::Postgres),
            ("CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$ BEGIN SET search_path = app; END $$;\n", Dialect::Postgres),
            ("CREATE FUNCTION f() RETURNS int LANGUAGE sql SET search_path = app AS $$ SELECT 1 $$;\n", Dialect::Postgres),
            // DDL a declared body runs when a top-level call runs it.
            ("CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$ BEGIN CREATE EXTENSION dblink; END $$;\nSELECT f();\n", Dialect::Postgres),
            ("CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$ BEGIN CREATE RULE r AS ON INSERT TO log DO ALSO NOTIFY x; END $$;\n", Dialect::Postgres),
            ("CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$ BEGIN CREATE VIEW app.item AS SELECT 1; END $$;\n", Dialect::Postgres),
            ("CREATE PROCEDURE dbo.q AS BEGIN CREATE SYNONYM dbo.I FOR dbo.Item; END\nGO\n", Dialect::Tsql),
            // F3: what a T-SQL trigger or view runs.
            ("CREATE TRIGGER dbo.tr ON dbo.Log AFTER INSERT AS BEGIN DECLARE @s nvarchar(9) = N'x'; EXEC (@s); END\nGO\n", Dialect::Tsql),
            ("CREATE VIEW dbo.v AS SELECT x FROM OPENQUERY(loopback, 'SELECT 1 AS x');\nGO\n", Dialect::Tsql),
            ("CREATE TRIGGER tr ON DATABASE FOR CREATE_TABLE AS BEGIN SET NOCOUNT ON; END\nGO\n", Dialect::Tsql),
            ("CREATE FUNCTION dbo.f() RETURNS int AS BEGIN RETURN dbo.elsewhere(1); END\nGO\n", Dialect::Tsql),
            ("CREATE PROCEDURE dbo.Clr AS EXTERNAL NAME A.B.C;\nGO\n", Dialect::Tsql),
            // F6: a procedure called without EXEC, whatever its name.
            ("Comment;\nGO\n", Dialect::Tsql),
            ("Get;\nGO\n", Dialect::Tsql),
            ("Move;\nGO\n", Dialect::Tsql),
            ("TRY\nGO\n", Dialect::Tsql),
            ("[sp_executesql] N'SELECT 1';\nGO\n", Dialect::Tsql),
            // T-SQL statements off the list.
            ("USE other;\nGO\n", Dialect::Tsql),
            ("ALTER DATABASE CURRENT SET RECURSIVE_TRIGGERS ON;\nGO\n", Dialect::Tsql),
            ("EXEC dbo.Setup;\nGO\n", Dialect::Tsql),
            ("CREATE SYNONYM dbo.Item2 FOR dbo.Item;\nGO\n", Dialect::Tsql),
            ("CREATE TYPE dbo.T AS TABLE (a int);\nGO\n", Dialect::Tsql),
            ("SELECT 1;\nGO\n", Dialect::Tsql),
            ("IF 1 = 1 PRINT 'x';\nGO\n", Dialect::Tsql),
            ("BEGIN\n  SELECT 1;\nEND\nGO\n", Dialect::Tsql),
            (":r other.sql\nGO\n", Dialect::Tsql),
            ("SET IDENTITY_INSERT dbo.Item ON;\nGO\n", Dialect::Tsql),
            ("SET DATEFORMAT dmy;\nGO\n", Dialect::Tsql),
            ("CREATE SCHEMA s AUTHORIZATION dbo\nCREATE TABLE t (a int);\nGO\n", Dialect::Tsql),
            ("MERGE dbo.Item AS t USING (SELECT 1 AS Id) AS s ON t.Id = s.Id WHEN MATCHED THEN DELETE;\nGO\n", Dialect::Tsql),
        ] {
            assert!(unseen(&[text], d), "{text}");
        }
    }
}
