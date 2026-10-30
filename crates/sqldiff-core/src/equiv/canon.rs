//! Lowering of parsed statements to canonical terms.
//!
//! Dialect spellings become one form (`ISNULL`/`COALESCE`, `+`/`||`,
//! `DATEDIFF(day)`/date subtraction/`DAYS()`, `STRING_AGG`/`LISTAGG`, paging,
//! upserts, generated-key returns, anti-join deletes), names are resolved
//! (parameters by position, locals by declaration order, aliases to tables),
//! and every dialect difference in behaviour stays an explicit child. Facts
//! that make a rewrite sound are either discharged from the catalog or
//! recorded as assumptions.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use sqldiff_syntax::Dialect;

use super::ast::{
    Call, ConflictAction, Expr, From, InsertSource, MergeAction, OrderItem, Output, SelItem,
    Select, Stmt,
};
use super::literal::{self, Target};
use super::lower::{Unit, UnitKind};
use super::schema::{Fam, Schema, Ty};
use super::term::{a, n, Term};
use crate::model::Assumption;

pub type R<T> = Result<T, String>;

/// Lowered query parts without the select list, and the select items with their types.
type Parts = (Vec<Term>, Vec<(Term, Option<Ty>)>);

/// Assumption identifiers and texts used by the equivalence rules.
pub mod asm {
    pub const LEN_BMP: (&str, &str) = (
        "ASM-TSQL-LEN-BMP",
        "Strings a T-SQL LEN counts have no character outside the Basic Multilingual Plane, or the collation is supplementary-character aware (_SC)",
    );
    pub const DATEFIRST_7: (&str, &str) = (
        "ASM-TSQL-DATEFIRST-7",
        "The T-SQL session keeps SET DATEFIRST 7 (the us_english default): DATEPART(weekday) counts Sunday as 1",
    );
    pub const PROCID_NAME: (&str, &str) = (
        "ASM-PROCID-NAME",
        "SQL Server stores the routine under the name and schema its header writes, letter for letter (OBJECT_NAME(@@PROCID) and OBJECT_SCHEMA_NAME(@@PROCID) become them as literals)",
    );
    pub const TSQL_ANSI_NULLS: (&str, &str) = (
        "ASM-TSQL-ANSI-NULLS",
        "The T-SQL routine was created with ANSI_NULLS ON (its sys.sql_modules.uses_ansi_nulls is 1)",
    );
    pub const TSQL_QUOTED_IDENTIFIER: (&str, &str) = (
        "ASM-TSQL-QUOTED-IDENTIFIER",
        "The T-SQL routine was created with QUOTED_IDENTIFIER ON (its sys.sql_modules.uses_quoted_identifier is 1)",
    );
    pub const SAME_DATABASE_NAME: (&str, &str) = (
        "ASM-SAME-DATABASE-NAME",
        "The PostgreSQL database has the same name, letter for letter, as the SQL Server database",
    );
    pub const QUOTENAME_IDENT: (&str, &str) = (
        "ASM-QUOTENAME-IDENT",
        "QUOTENAME's result is used only as an identifier, never as text, and is at most 128 characters: QUOTENAME brackets ([a]]b]) where quote_ident double-quotes only when needed (\"a]b\")",
    );
    pub const TSQL_CI: (&str, &str) = (
        "ASM-TSQL-CI-COLLATION",
        "T-SQL string comparisons use a case-insensitive collation",
    );
    /// Davy, 2026-10-29: T-SQL string equality against the other side's
    /// case-sensitive one is kept, and flagged with this per statement.
    pub const CI_COLLATION: (&str, &str) = (
        "ASM-CI-COLLATION",
        "String equality, LIKE and the substrings REPLACE and CHARINDEX find give the same result on both sides: no compared value differs from another only in letter case, in trailing spaces, or in characters T-SQL's collation treats as equal (SQL Server 2022's default SQL_Latin1_General_CP1_CI_AS finds N'æ' = N'ae', N'ß' = N'ss' and full-width N'Ａ' = N'A' and ignores NCHAR(0) in nvarchar), and no value LIKE tests has trailing spaces or matches its pattern only up to letter case or such characters (N'æx' LIKE N'ae%' is true in T-SQL); the other side compares those values as different",
    );
    pub const CASE_FOLD: (&str, &str) = (
        "ASM-CASE-FOLD",
        "UPPER/LOWER fold the same characters as the case-insensitive comparison on the other side",
    );
    pub const DB2_SRTSEQ: (&str, &str) = (
        "ASM-DB2-SRTSEQ-HEX",
        "Db2 for i compares strings with the *HEX sort sequence (case-sensitive)",
    );
    pub const UNTYPED: (&str, &str) = (
        "ASM-UNTYPED-NONCHAR",
        "Values whose type is unknown (no catalog) are not character strings, real (single precision), T-SQL tinyint or decimals of 38 digits, so comparison and arithmetic mean the same on both sides",
    );
    pub const DATEADD_RANGE: (&str, &str) = (
        "ASM-DATEADD-RANGE",
        "DATEADD results stay within the range of their type: T-SQL raises an error past 9999-12-31 (or before 0001-01-01), where PostgreSQL returns the date",
    );
    pub const DATEADD_YEAR_END: (&str, &str) = (
        "ASM-DATEADD-YEAR-END",
        "T-SQL DATEADD clamps years to the month end as it does months (2024-02-29 + 1 year = 2025-02-28), as PostgreSQL does; Microsoft documents the rule for months only",
    );
    pub const UNTYPED_CAST: (&str, &str) = (
        "ASM-UNTYPED-CAST",
        "Values of unknown type (no catalog) that are cast to a number or string are integers, or strings cast to a string type, which convert the same on both sides (a decimal or float to an integer, a float to text and text to a number do not)",
    );
    pub const PERIOD_WRITE: (&str, &str) = (
        "ASM-TEMPORAL-PERIOD-WRITE",
        "The statement gives a period column of a system-versioned table a value: assumes the code runs while versioning is off and the period dropped (SYSTEM_VERSIONING = OFF, DROP PERIOD FOR SYSTEM_TIME; in PostgreSQL the converted versioning triggers disabled), as data loads do, so both sides store the value as given. While versioning is on, both sides refuse the statement; while only the period remains, SQL Server refuses it and PostgreSQL stores the value",
    );
    pub const UNTYPED_STORE: (&str, &str) = (
        "ASM-UNTYPED-STORE",
        "Values stored in columns or variables of unknown type (no catalog) convert the same on both sides: no fraction goes into an integer, no number into a bit or boolean, no string longer than the column, and no text into a number",
    );
    pub const TSQL_PLUS_STRING: (&str, &str) = (
        "ASM-TSQL-PLUS-IS-CONCAT",
        "Operands of T-SQL `+` next to a string literal are character strings, so `+` concatenates",
    );
    pub const SYSDATETIME: (&str, &str) = (
        "ASM-TSQL-SYSDATETIME-PER-STATEMENT",
        "SYSDATETIME() is evaluated once per statement (observed SQL Server behaviour, not documented; fact V20)",
    );
    pub const STRING_AGG_8000: (&str, &str) = (
        "ASM-TSQL-STRING-AGG-8000",
        "The aggregated list stays under 8000 bytes (T-SQL STRING_AGG of a non-MAX input errors past that)",
    );
    pub const LISTAGG_LEN: (&str, &str) = (
        "ASM-DB2-LISTAGG-LENGTH",
        "The aggregated list fits the LISTAGG result length",
    );
    pub const DB2_MERGE_CONCURRENT: (&str, &str) = (
        "ASM-DB2-MERGE-CONCURRENT",
        "Db2 for i MERGE does not raise a duplicate key when two sessions insert the same key concurrently (fact V3)",
    );
    pub const SINGLE_ROW: (&str, &str) = (
        "ASM-SINGLE-ROW",
        "The query returns at most one row (otherwise each side keeps a different arbitrary row)",
    );
    pub const ORDER_TOTAL: (&str, &str) = (
        "ASM-ORDER-TOTAL",
        "The ORDER BY keys are unique, so both sides return the same rows in the same order",
    );
    pub const PAGE_UNORDERED: (&str, &str) = (
        "ASM-PAGE-UNORDERED",
        "A row limit or offset without ORDER BY: which rows each side keeps is up to its engine, so any such rows will do",
    );
    pub const PAGE_COUNT_NOT_NULL: (&str, &str) = (
        "ASM-PAGE-COUNT-NOT-NULL",
        "The row count and offset of TOP / LIMIT / FETCH / OFFSET are never NULL: T-SQL raises an error for NULL (Msg 1014), PostgreSQL reads LIMIT NULL as no limit and OFFSET NULL as 0, and Db2 for i was not checked",
    );
    pub const CURSOR_SNAPSHOT: (&str, &str) = (
        "ASM-CURSOR-SNAPSHOT",
        "The rows a cursor loop fetches are those its query gives at OPEN: no other session or trigger changes the tables it reads while the loop runs, and its values do not depend on when each row is fetched (a T-SQL cursor that is not STATIC reads rows as it fetches them, a PostgreSQL cursor reads the database as of OPEN)",
    );
    pub const CURSOR_ORDER: (&str, &str) = (
        "ASM-CURSOR-ORDER",
        "What a cursor loop does does not depend on the order of the rows beyond its ORDER BY: rows that ORDER BY leaves in no order may come in another order on each side",
    );
    pub const CURSOR_GLOBAL: (&str, &str) = (
        "ASM-CURSOR-GLOBAL",
        "No error or RETURN leaves a T-SQL cursor declared without LOCAL open: it then stays in the session and the routine's next DECLARE of it fails, where PostgreSQL closes it at the end of the transaction",
    );
    pub const RAISERROR_TEXT: (&str, &str) = (
        "ASM-RAISERROR-TEXT",
        "The RAISERROR message is 1 to 2,047 characters long: T-SQL cuts a longer one to 2,044 characters and '...', and sends an empty one as a single space, where PostgreSQL keeps the text as it is",
    );
    pub const RAISERROR_FORMAT: (&str, &str) = (
        "ASM-RAISERROR-FORMAT",
        "The variable RAISERROR takes as its message holds no % sign: T-SQL reads it as a format, so % starts a substitution",
    );
    pub const RESULT_TYPES: (&str, &str) = (
        "ASM-RESULT-TYPES",
        "Declared result column types hold the selected values without conversion",
    );
    pub const VALUES_ORDER: (&str, &str) = (
        "ASM-VALUES-ROW-ORDER",
        "A multi-row VALUES list gets the values the table generates (identity, sequence defaults) in the order its rows are written on both sides",
    );
    pub const OUTPUT_INTO_TARGET: (&str, &str) = (
        "ASM-OUTPUT-INTO-TARGET",
        "The table OUTPUT … INTO writes has no enabled trigger, CHECK constraint or rule and is on neither side of a foreign key: SQL Server rejects the statement otherwise (Msg 331, 332, 333), where the PostgreSQL insert runs",
    );
    pub const OUTPUT_ROW_ORDER: (&str, &str) = (
        "ASM-OUTPUT-ROW-ORDER",
        "The rows OUTPUT … INTO or RETURNING writes get the values their table generates (identity, sequence defaults) in the same order on both sides; neither engine specifies the order",
    );
    pub const RETURN_NULL_WARNING: (&str, &str) = (
        "ASM-TSQL-RETURN-NULL-WARNING",
        "Callers do not read the warning T-SQL sends when a procedure returns NULL (the status is 0 on both sides)",
    );
    pub const NO_CALLER_TXN: (&str, &str) = (
        "ASM-NO-CALLER-TXN",
        "The caller does not hold an open transaction",
    );

    pub const DYNAMIC_SESSION_OPTIONS: (&str, &str) = (
        "ASM-TSQL-DYNAMIC-SESSION-OPTIONS",
        "The session runs dynamic SQL with ANSI_NULLS and QUOTED_IDENTIFIER ON (the defaults): T-SQL takes them for a dynamic batch from the session, not from the procedure",
    );

    pub const OWNER_PRIVILEGES: (&str, &str) = (
        "ASM-OWNER-PRIVILEGES",
        "The routine owner holds every privilege its statements need on both sides (Db2 for i USRPRF *OWNER adds the owner's authority to the caller's; T-SQL EXECUTE AS OWNER and PostgreSQL SECURITY DEFINER use the owner's alone)",
    );
    pub const OWNERSHIP_CHAIN: (&str, &str) = (
        "ASM-TSQL-OWNERSHIP-CHAIN",
        "Callers hold the privileges the statements need themselves: T-SQL skips permission checks along an ownership chain (routine and objects with one owner), the other side does not",
    );
    pub const DB2_USRPRF_USER: (&str, &str) = (
        "ASM-DB2-USRPRF-USER",
        "The Db2 for i routine has no SET OPTION USRPRF, so its authority comes from USRPRF *NAMING: assumed to be the caller's (system naming at creation; under SQL naming it runs with the owner's authority; fact V25, not checked on a real system)",
    );
    pub const DEFAULT_SCHEMA: (&str, &str) = (
        "ASM-DEFAULT-SCHEMA",
        "Unqualified names, T-SQL dbo and PostgreSQL public resolve to the same schema on both sides (the default schema, search_path or Db2 for i current schema in effect)",
    );
    /// Same id as [`TSQL_CI`]: T-SQL has one case-insensitive collation
    /// assumption, stated for names here and for string comparisons there.
    pub const TSQL_CI_NAMES: (&str, &str) = (
        "ASM-TSQL-CI-COLLATION",
        "Identifiers and variables compare case-insensitively (case-insensitive catalog collation)",
    );
    /// Decision 1: across dialects, result column names that differ only in
    /// letter case are equal when the client matches names that way.
    pub const END_OF_TIME: (&str, &str) = (
        "ASM-END-OF-TIME",
        "T-SQL's end of time '9999-12-31 23:59:59.9999999' is written for PostgreSQL as '9999-12-31 23:59:59.999999' (PostgreSQL keeps 6 fraction digits and would round it to 10000-01-01 00:00:00): it is only an open-end sentinel, compared with values converted by the same rule (such as temporal ValidTo columns), and data moved from SQL Server truncates its 7th digit the same way rather than rounding it; no value lies between the two",
    );
    pub const RESULT_NAMES_CI: (&str, &str) = (
        "ASM-RESULT-NAMES-CI",
        "Clients read result columns by name case-insensitively (PostgreSQL returns an unquoted alias such as OrderId as orderid; T-SQL returns it as written)",
    );
    pub const IDENT_FOLD: (&str, &str) = (
        "ASM-IDENT-FOLD",
        "Names on both sides name the same objects and columns after case folding: the PostgreSQL (lower case) and Db2 for i (upper case) objects were created from unquoted names, or, in PostgreSQL, from the quoted names the statements write, which no other name of the statement or the catalog matches up to letter case; and T-SQL resolves names case-insensitively (case-insensitive catalog collation)",
    );
    /// `sqldiff convert` keeps a call unproven unless the converted callee's
    /// own file is converted or under assumptions, and carries those.
    pub const UDF_CALLEE: (&str, &str) = (
        "ASM-UDF-CALLEE",
        "The user function a call names on side B is the conversion of the one it names on side A, and that conversion is shown equal, or equal under the assumptions its file lists, in the file that creates it",
    );
    pub const PG_UNQUALIFIED_FUNCTION: (&str, &str) = (
        "ASM-PG-UNQUALIFIED-FUNCTION",
        "An unqualified PostgreSQL function call reaches the user function of that name in the default schema: no function of that name and arguments sits in a schema search_path lists before it, and pg_catalog, which PostgreSQL searches first unless search_path lists it later, has no function of that name, and it is no keyword the grammar reads itself (checked against PostgreSQL 18's)",
    );
    pub const UDF_CALL_CONTEXT: (&str, &str) = (
        "ASM-UDF-CALL-CONTEXT",
        "A user function called in a statement returns the same value for the same arguments throughout the statement, however often each side runs it: it reads no data the calling statement or another session changes while the statement runs, and no clock (SQL Server reads SYSDATETIME() at each call inside a function, PostgreSQL's statement_timestamp() and now() give the calling statement's or transaction's start)",
    );
    pub const ROUTINE_LABEL: (&str, &str) = (
        "ASM-ROUTINE-LABEL",
        "The routine's STABLE, IMMUTABLE or DETERMINISTIC label is truthful: its result depends only on its arguments and data that does not change while the calling statement runs, so evaluating it fewer times gives the same results",
    );

    pub fn make(x: (&str, &str)) -> crate::model::Assumption {
        crate::model::Assumption::default(x.0, x.1)
    }
}

/// How a value is converted to a type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum How {
    /// CAST / CONVERT / `::`.
    Cast,
    /// Stored in a column.
    Store,
    /// Assigned to a variable, parameter or return value.
    Var,
}

impl How {
    fn name(self) -> &'static str {
        match self {
            How::Cast => "cast",
            How::Store => "store",
            How::Var => "assign",
        }
    }
}

/// Whether a conversion means the same in every dialect.
enum Rule {
    Same,
    /// T-SQL drops the fraction first.
    Trunc,
    /// Dialect-specific (or not checked).
    Dialect,
}

#[derive(Debug, Clone)]
struct Binding {
    /// Alias or table name, lower case.
    key: String,
    /// Canonical table name.
    table: String,
    /// Derived single-row sources: column → value.
    derived: Option<Vec<(String, Term, Option<Ty>)>>,
    /// A derived table's unique key: the GROUP BY columns its query
    /// returns (`ast::group_key`), folded. Kept with the binding, as a
    /// subquery elsewhere in the statement may reuse the alias.
    unique: Option<Vec<String>>,
}

#[derive(Clone)]
pub struct Canon<'a> {
    pub d: Dialect,
    pub schema: &'a Schema,
    pub unit: &'a Unit,
    scopes: Vec<Vec<Binding>>,
    pub asm: Vec<Assumption>,
    pub discharged: Vec<String>,
    /// The statement being lowered evaluates its expressions for one row.
    single_row: bool,
    seen_tables: HashMap<String, usize>,
    /// PostgreSQL `EXCLUDED.c` while lowering ON CONFLICT DO UPDATE.
    excluded: Option<Vec<(String, Term, Option<Ty>)>>,
    /// Identify parameters and locals by name instead of position. Used when
    /// only some statements of two versions are compared, so a renaming is
    /// not checked across the whole routine.
    pub by_name: bool,
    /// PostgreSQL compared with T-SQL: quoted names may fold to lower case
    /// (see `stmt`).
    pub fold_quoted: bool,
    /// The dialect of the statements this one is compared with, when known:
    /// an unqualified PostgreSQL call may then be the conversion of a T-SQL
    /// `dbo` function (`user_fn`).
    pub other: Option<Dialect>,
    /// Table names of the statement being lowered (see `Item::names`).
    pub names: Vec<(String, Option<String>)>,
    /// Bit and boolean columns of derived tables (`sub:alias`), by name: a
    /// bit read through a derived table still meets numbers as a bit.
    sub_bits: HashMap<String, Vec<(String, Ty)>>,
    /// The rows the statement being lowered returns (see `output_stmt`).
    output: Option<Output>,
    /// The column types of each cursor's query, by key, from its OPEN
    /// (`cursor_op`); kept across statements.
    cursor_cols: HashMap<String, Vec<Option<Ty>>>,
    /// Why a cursor's OPEN was not read, by cursor key: its FETCHes say so.
    cursor_failed: HashMap<String, String>,
}

/// T-SQL `OBJECT_NAME(@@PROCID)` / `OBJECT_SCHEMA_NAME(@@PROCID)` in a
/// procedure: its own name or schema as the header writes it
/// (`asm::PROCID_NAME`). `None` elsewhere (a script's @@PROCID is 0, a
/// trigger's is the trigger, an inline table-valued function or an inlined
/// scalar function runs in its caller's query), for a temporary procedure,
/// and for a schema the header leaves out (the creator's default schema).
pub(super) fn own_name(u: &Unit, c: &Call) -> Option<String> {
    let name = c.name.last()?.to_ascii_lowercase();
    let schema = match name.as_str() {
        "object_name" => false,
        "object_schema_name" => true,
        _ => return None,
    };
    let procid = matches!(c.args.as_slice(), [Expr::Var(v)] if v.eq_ignore_ascii_case("@@procid"));
    if u.dialect != Dialect::Tsql
        || c.name.len() != 1
        || !procid
        || c.distinct
        || c.star
        || u.kind != UnitKind::Proc
    {
        return None;
    }
    let parts = &u.routine_parts;
    let last = parts.last().filter(|l| !l.starts_with('#'))?;
    if !schema {
        return Some(last.clone());
    }
    (parts.len() >= 2).then(|| parts[parts.len() - 2].clone())
}

/// T-SQL `(SELECT uses_ansi_nulls FROM sys.sql_modules WHERE object_id =
/// @@PROCID) = 1` (or `uses_quoted_identifier`) in a procedure:
/// the running routine tests its own setting, which every converted routine
/// is assumed to have been created with (the converter writes `true`).
fn module_setting_test(u: &Unit, e: &Expr) -> Option<(&'static str, &'static str)> {
    if u.dialect != Dialect::Tsql || u.kind != UnitKind::Proc {
        return None;
    }
    let Expr::Bin(op, l, r) = e else { return None };
    let (Expr::Subquery(q), Expr::Num(one)) = (l.as_ref(), r.as_ref()) else {
        return None;
    };
    if op != "=" || one != "1" {
        return None;
    }
    let plain = q.ctes.is_empty()
        && !q.for_json
        && q.set_ops.is_empty()
        && !q.distinct
        && q.top.is_none()
        && q.into.is_empty()
        && q.group_by.is_empty()
        && q.having.is_none()
        && q.order_by.is_empty()
        && q.offset.is_none()
        && q.limit.is_none()
        && q.locking.is_empty()
        && q.values.is_none();
    let is = |x: &str, y: &str| x.eq_ignore_ascii_case(y);
    let modules = matches!(q.from.as_slice(), [From::Table { name, alias: None, hints }]
        if hints.is_empty() && matches!(name.as_slice(), [s, m] if is(s, "sys") && is(m, "sql_modules")));
    let procid = matches!(&q.where_, Some(Expr::Bin(op, c, v))
        if op == "="
            && matches!(c.as_ref(), Expr::Name(p) if matches!(p.as_slice(), [x] if is(x, "object_id")))
            && matches!(v.as_ref(), Expr::Var(v) if is(v, "@@procid")));
    let setting = match q.items.as_slice() {
        [SelItem {
            expr: Expr::Name(p),
            alias: None,
            assign: None,
        }] => match p.as_slice() {
            [x] if is(x, "uses_ansi_nulls") => asm::TSQL_ANSI_NULLS,
            [x] if is(x, "uses_quoted_identifier") => asm::TSQL_QUOTED_IDENTIFIER,
            _ => return None,
        },
        _ => return None,
    };
    (plain && modules && procid).then_some(setting)
}

/// `e` with a T-SQL `OBJECT_NAME(@@PROCID)` (see `own_name`) read as the
/// string literal it is.
pub(super) fn own_name_literal<'e>(u: &Unit, e: &'e Expr) -> std::borrow::Cow<'e, Expr> {
    match e {
        Expr::Call(c) => match own_name(u, c) {
            Some(s) => std::borrow::Cow::Owned(Expr::Str(s)),
            None => std::borrow::Cow::Borrowed(e),
        },
        _ => std::borrow::Cow::Borrowed(e),
    }
}

/// Lower-case hex digits of a byte string written in hex, `None` unless
/// every character is a hex digit and they make whole bytes.
pub(super) fn hex_bytes(s: &str) -> Option<String> {
    (s.len() % 2 == 0 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
}

/// The name a client receives for a result column, from the parser's
/// spelling (`ast::quoted_name` marks a quoted name whose case matters with
/// a leading `"`): such a name as it is; otherwise as written in T-SQL,
/// folded to lower case in PostgreSQL and to upper case in Db2 for i.
fn returned_name(d: Dialect, s: &str) -> String {
    match s.strip_prefix('"') {
        Some(q) => q.to_string(),
        None => match d {
            Dialect::Tsql => s.to_string(),
            Dialect::Postgres => s.to_lowercase(),
            Dialect::Db2i => s.to_uppercase(),
        },
    }
}

/// A result column's name as decision 1 observes it: `(colname DIALECT
/// KIND NAME)`, KIND `alias`, `col` (a column reference without alias),
/// `decl` (a declared output column) or `expr` (unnamed, NAME empty).
/// `compare` checks names exactly within a dialect and up to letter case
/// across dialects, under `ASM-RESULT-NAMES-CI`.
fn colname(d: Dialect, kind: &str, name: &str) -> Term {
    n("colname", vec![a(d.id()), a(kind), a(name)])
}

/// Folded name; names whose quoting is significant (leading `"`) keep their
/// case, except while a PostgreSQL statement is lowered again with its
/// quoted names folded (see `Canon::stmt`).
fn lc(s: &str) -> String {
    let (stored, quoted) = match s.strip_prefix('"') {
        Some(q) => (q.to_string(), true),
        None => (s.to_lowercase(), false),
    };
    let fold = FOLDING.with(|f| match &mut *f.borrow_mut() {
        Folding::Off => false,
        Folding::Record(m) => {
            m.entry(stored.to_lowercase())
                .or_default()
                .insert(stored.clone());
            false
        }
        Folding::Fold => true,
    });
    if fold {
        stored.to_lowercase()
    } else if quoted {
        s.to_string()
    } else {
        stored
    }
}

/// The names of PostgreSQL 18's pg_catalog functions (profiles/).
const PG_CATALOG_FUNCTIONS: &str = include_str!("../../profiles/pg18_catalog_functions.txt");
/// PostgreSQL 18's keywords that are not unreserved (profiles/): among them
/// the grammar forms written like calls (`trim`, `coalesce`, `xmlelement`,
/// `json_object`), which are no pg_catalog function.
const PG_KEYWORDS: &str = include_str!("../../profiles/pg18_keywords.txt");

/// An unquoted PostgreSQL call of `name` (folded) may reach a built-in
/// before a user function of the same name: pg_catalog has a function of
/// that name, or the name is a keyword the grammar reads itself.
fn pg_builtin_name(name: &str) -> bool {
    let k = name.to_lowercase();
    PG_CATALOG_FUNCTIONS
        .lines()
        .chain(PG_KEYWORDS.lines())
        .any(|l| !l.starts_with('#') && l == k)
}

/// How `lc` treats a PostgreSQL statement's names.
enum Folding {
    Off,
    /// Records each name, by lower case, with the spellings PostgreSQL
    /// stores it under (a quoted name as written, others in lower case).
    Record(BTreeMap<String, BTreeSet<String>>),
    /// Folds quoted names to lower case as well.
    Fold,
}

thread_local! {
    static FOLDING: std::cell::RefCell<Folding> = const { std::cell::RefCell::new(Folding::Off) };
}

/// Sets how `lc` treats names until dropped.
struct FoldingGuard;

impl FoldingGuard {
    fn set(mode: Folding) -> FoldingGuard {
        FOLDING.with(|f| *f.borrow_mut() = mode);
        FoldingGuard
    }

    fn take(self) -> Folding {
        FOLDING.with(|f| std::mem::replace(&mut *f.borrow_mut(), Folding::Off))
    }
}

impl Drop for FoldingGuard {
    fn drop(&mut self) {
        FOLDING.with(|f| *f.borrow_mut() = Folding::Off);
    }
}

/// Whether a PostgreSQL statement's quoted names may fold to lower case to
/// meet the T-SQL names they were converted from (`ASM-IDENT-FOLD`):
/// folding resolves names as PostgreSQL does when no two names the statement
/// uses differ only in letter case and each is spelled as the catalog spells
/// it (`Schema::case_check`). `seen` is what a `Folding::Record` lowering
/// recorded.
fn fold_check(seen: &BTreeMap<String, BTreeSet<String>>, schema: &Schema) -> R<()> {
    for spellings in seen.values() {
        let mut it = spellings.iter();
        match (it.next(), it.next()) {
            (Some(x), Some(y)) => {
                return Err(format!(
                    "the PostgreSQL names {} and {} differ only in letter case, so a T-SQL name does not say which one it means",
                    shown(x),
                    shown(y)
                ))
            }
            (Some(x), None) => schema.case_check(x, false)?,
            _ => {}
        }
    }
    Ok(())
}

/// A PostgreSQL name as written: quoted unless it is stored in lower case.
fn shown(stored: &str) -> String {
    if stored == stored.to_lowercase() {
        stored.to_string()
    } else {
        format!("\"{stored}\"")
    }
}

impl<'a> Canon<'a> {
    pub fn new(unit: &'a Unit, schema: &'a Schema) -> Self {
        Canon {
            d: unit.dialect,
            schema,
            unit,
            scopes: Vec::new(),
            asm: Vec::new(),
            discharged: Vec::new(),
            single_row: false,
            seen_tables: HashMap::new(),
            excluded: None,
            sub_bits: HashMap::new(),
            cursor_cols: HashMap::new(),
            cursor_failed: HashMap::new(),
            by_name: false,
            fold_quoted: false,
            other: None,
            names: Vec::new(),
            output: None,
        }
    }

    pub fn reset(&mut self) {
        self.scopes.clear();
        self.asm.clear();
        self.discharged.clear();
        self.single_row = false;
        self.seen_tables.clear();
        self.excluded = None;
        self.names.clear();
        self.output = None;
    }

    fn assume(&mut self, x: (&str, &str)) {
        if !self.asm.iter().any(|y| y.id == x.0) {
            self.asm.push(asm::make(x));
        }
    }

    fn discharge(&mut self, s: String) {
        if !self.discharged.contains(&s) {
            self.discharged.push(s);
        }
    }

    fn dtag(&self) -> Term {
        a(self.d.id())
    }

    // ---- names ----------------------------------------------------------

    /// Key of a table name: the folded name, prefixed with its folded schema
    /// (and database) unless the schema is the default one (none written,
    /// T-SQL `dbo`, PostgreSQL `public`). Temporary tables get `temp:`; a
    /// table variable `@t` of routine `routine` converted to one of
    /// `other_temps` gets that table's key (`classify::table_variable_table`),
    /// and so does a bare name in `local_temps` (a PostgreSQL temporary
    /// table the unit created, which the bare name finds first).
    /// Returns the key and, for a default schema, how it was written.
    pub fn table_name(
        parts: &[String],
        other_temps: &std::collections::HashSet<String>,
        local_temps: &std::collections::HashSet<String>,
        routine: Option<&str>,
    ) -> (String, Option<String>) {
        let last = parts.last().map(|s| lc(s)).unwrap_or_default();
        if parts.len() == 1 && last.starts_with('@') {
            // From the spelling, as the converter names it: a reference
            // spelled with other capitals than the DECLARE does not pair.
            let t = lc(&crate::classify::table_variable_table(routine, &parts[0]));
            if other_temps.contains(&t) {
                return (format!("temp:{t}"), None);
            }
        }
        if parts.len() == 1 && local_temps.contains(&last) {
            return (format!("temp:{last}"), None);
        }
        let temp = last.starts_with('#')
            || last.starts_with('@')
            || parts.first().is_some_and(|p| {
                (p.eq_ignore_ascii_case("SESSION") || p.eq_ignore_ascii_case("pg_temp"))
                    && parts.len() > 1
            });
        if temp {
            return (
                format!("temp:{}", last.trim_start_matches(['#', '@'])),
                None,
            );
        }
        let raw = &parts[..parts.len().saturating_sub(1)];
        let quals: Vec<String> = raw.iter().map(|s| lc(s)).collect();
        match quals.as_slice() {
            [] => (last, Some(String::new())),
            // A quoted PostgreSQL "Public" (leading `"`) is another schema.
            [q] if (q == "dbo" || q == "public") && !raw[0].starts_with('"') => {
                (last, Some(q.clone()))
            }
            _ => (format!("{}.{last}", quals.join(".")), None),
        }
    }

    /// `table_name`, remembering the name for the statement's item.
    fn table_key(&mut self, parts: &[String]) -> String {
        let (key, default) = Self::table_name(
            parts,
            &self.unit.other_temps,
            &self.unit.local_temps,
            self.unit.routine.as_deref(),
        );
        // T-SQL `sys.name` is SQL Server's own catalog view; a table of that
        // name on another side is some other table, never its counterpart.
        let key = match parts {
            [.., s, _] if self.d != Dialect::Tsql && lc(s) == "sys" => {
                format!("{}:{key}", self.d.id())
            }
            _ => key,
        };
        let x = (key.clone(), default);
        if !self.names.contains(&x) {
            self.names.push(x);
        }
        key
    }

    fn bind_table(&mut self, parts: &[String], alias: &Option<String>) -> Binding {
        let table = self.table_key(parts);
        let k = self.seen_tables.entry(table.clone()).or_insert(0);
        *k += 1;
        let canon = if *k > 1 {
            format!("{table}#{k}")
        } else {
            table.clone()
        };
        Binding {
            key: alias
                .as_ref()
                .map(|x| lc(x))
                .unwrap_or_else(|| lc(parts.last().map(String::as_str).unwrap_or(""))),
            table: canon,
            derived: None,
            unique: None,
        }
    }

    fn all_bindings(&self) -> usize {
        self.scopes.iter().map(Vec::len).sum()
    }

    /// Catalog name of a table key: without its occurrence number and schema
    /// (the catalog is keyed by table name).
    fn base_table(t: &str) -> &str {
        let t = t.split('#').next().unwrap_or(t);
        if t.starts_with('"') {
            return t;
        }
        t.rsplit_once('.').map_or(t, |(_, name)| name)
    }

    fn col_ty(&self, table: &str, col: &str) -> (Option<Ty>, Option<bool>) {
        if let Some((_, ty)) = self
            .sub_bits
            .get(table)
            .and_then(|cs| cs.iter().find(|(c, _)| c == col))
        {
            return (Some(*ty), None);
        }
        match self
            .schema
            .table(Self::base_table(table))
            .and_then(|t| t.column(col))
        {
            Some(c) => (Some(c.ty), Some(c.not_null)),
            None => (None, None),
        }
    }

    /// A statement writes `col`: refused for a column the database computes
    /// (computed, GENERATED ALWAYS), whose explicit values one side rejects
    /// or replaces depending on settings and triggers the statement does not
    /// show. A period column of a system-versioned table is the exception:
    /// code writes it only with the period dropped, a state the statement
    /// does not show either, so that is an assumption.
    fn written(&mut self, table: &str, col: &str) -> R<()> {
        let (generated, period) = self
            .schema
            .table(Self::base_table(table))
            .and_then(|t| t.column(col))
            .map_or((false, false), |c| (c.generated, c.period));
        if period {
            self.assume(asm::PERIOD_WRITE);
        } else if generated {
            return Err(format!("writes the generated column {col}"));
        }
        Ok(())
    }

    /// An INSERT that leaves out a period column, in code that drops the
    /// table's period (it switches the versioning or writes a period
    /// column): with the period dropped, SQL Server takes the column's
    /// default or fails on NOT NULL, while PostgreSQL, its versioning
    /// triggers disabled, stores the converted default `-infinity`.
    fn period_omitted(&self, table: &str, cols: &[String]) -> R<()> {
        let Some(t) = self.schema.table(Self::base_table(table)) else {
            return Ok(());
        };
        let missing: Vec<&str> = t
            .columns
            .iter()
            .filter(|c| c.period && !cols.contains(&c.name))
            .map(|c| c.name.as_str())
            .collect();
        if missing.is_empty() || !self.drops_period(t) {
            return Ok(());
        }
        Err(format!(
            "the INSERT leaves out the period column {} of a table whose period this code drops or writes: with the period dropped SQL Server takes the column's default or fails, PostgreSQL stores '-infinity'",
            missing.join(", ")
        ))
    }

    /// Whether the unit switches `t`'s versioning, or writes (or may write:
    /// a data statement naming the column) one of its period columns.
    fn drops_period(&self, t: &super::schema::Table) -> bool {
        let switched = self.unit.calls.iter().any(|(node, _)| {
            matches!(
                super::call::parse(&super::ast::tokens(node), self.d),
                Ok(super::call::Stmt::Switch(s))
                    if s.table.last().is_some_and(|n| n.text.eq_ignore_ascii_case(&t.name))
            )
        });
        let period: Vec<&str> = t
            .columns
            .iter()
            .filter(|c| c.period)
            .map(|c| c.name.as_str())
            .collect();
        switched
            || self.unit.stmts.iter().any(|s| {
                s.toks
                    .first()
                    .is_some_and(|f| matches!(f.up.as_str(), "INSERT" | "UPDATE" | "MERGE"))
                    && s.toks.iter().any(|x| {
                        matches!(x.kind, super::ast::K::Word | super::ast::K::Quoted)
                            && period.contains(
                                &sqldiff_syntax::ast::unquote(&x.text)
                                    .to_lowercase()
                                    .as_str(),
                            )
                    })
            })
    }

    fn sym(&self, name: &str) -> Option<(Term, Option<Ty>)> {
        let n0 = lc(name.trim_start_matches('@'));
        if let Some(i) = self.unit.params.iter().position(|p| p.name == n0) {
            let id = if self.by_name {
                format!("p:{n0}")
            } else {
                format!("p{}", i + 1)
            };
            return Some((n("sym", vec![a(id)]), self.unit.params[i].ty));
        }
        if let Some(i) = self.unit.locals.iter().position(|l| l.name == n0) {
            let id = if self.by_name {
                format!("l:{n0}")
            } else if let Some(ids) = &self.unit.local_ids {
                ids[i].clone()
            } else {
                format!("l{}", i + 1)
            };
            return Some((n("sym", vec![a(id)]), self.unit.locals[i].ty));
        }
        None
    }

    fn column_in_scope(&self, col: &str) -> bool {
        self.scopes.iter().flatten().any(|b| {
            b.derived
                .as_ref()
                .map(|d| d.iter().any(|(c, _, _)| c == col))
                .unwrap_or_else(|| self.col_ty(&b.table, col).0.is_some())
        })
    }

    fn resolve(&mut self, parts: &[String]) -> R<(Term, Option<Ty>)> {
        let parts: Vec<String> = parts.iter().map(|p| lc(p)).collect();
        if parts.len() == 1 {
            let c = &parts[0];
            if self.d != Dialect::Tsql {
                if let Some(s) = self.sym(c) {
                    if self.column_in_scope(c) {
                        return Err(format!("`{c}` names both a column and a variable"));
                    }
                    return Ok(s);
                }
            }
            // Innermost scope that is known to have the column.
            for scope in self.scopes.iter().rev() {
                for b in scope {
                    if let Some(d) = &b.derived {
                        if let Some((_, t, ty)) = d.iter().find(|(x, _, _)| x == c) {
                            return Ok((t.clone(), *ty));
                        }
                    } else if self.col_ty(&b.table, c).0.is_some() {
                        let (ty, _) = self.col_ty(&b.table, c);
                        return Ok((n("col", vec![a(b.table.clone()), a(c.clone())]), ty));
                    }
                }
            }
            if self.all_bindings() == 1 {
                let b = self.scopes.iter().flatten().next().unwrap().clone();
                if b.derived.is_none() {
                    return Ok((n("col", vec![a(b.table), a(c.clone())]), None));
                }
            }
            return Ok((n("col", vec![a("?"), a(c.clone())]), None));
        }
        let (qual, col) = (&parts[parts.len() - 2], &parts[parts.len() - 1]);
        if parts.len() == 2 && qual == "excluded" && self.d == Dialect::Postgres {
            if let Some(ex) = &self.excluded {
                if let Some((_, t, ty)) = ex.iter().find(|(x, _, _)| x == col) {
                    return Ok((t.clone(), *ty));
                }
                return Err(format!("EXCLUDED.{col} is not an inserted column"));
            }
        }
        for scope in self.scopes.iter().rev() {
            for b in scope {
                let matches = b.key == *qual || Self::base_table(&b.table) == qual.as_str();
                if !matches {
                    continue;
                }
                if let Some(d) = &b.derived {
                    return match d.iter().find(|(x, _, _)| x == col) {
                        Some((_, t, ty)) => Ok((t.clone(), *ty)),
                        None => Err(format!("{qual}.{col} is not a column of the source")),
                    };
                }
                let (ty, _) = self.col_ty(&b.table, col);
                return Ok((n("col", vec![a(b.table.clone()), a(col.clone())]), ty));
            }
        }
        // Function-qualified parameter (PL/pgSQL) or unknown qualifier.
        Ok((n("col", vec![a(format!("?{qual}")), a(col.clone())]), None))
    }

    fn not_null(&self, t: &Term) -> Option<bool> {
        if t.is("col") {
            let tab = t.kid(0)?.atom()?;
            let col = t.kid(1)?.atom()?;
            return self.col_ty(tab, col).1;
        }
        match t.head() {
            "num" | "str" | "time" => Some(true),
            _ => None,
        }
    }

    // ---- types ----------------------------------------------------------

    pub fn ty_term(t: &Option<Ty>) -> Term {
        match t {
            None => a("?"),
            Some(t) => n(
                "ty",
                vec![
                    a(format!("{:?}", t.fam)),
                    a(t.len.map(|x| x.to_string()).unwrap_or_else(|| "-".into())),
                    a(t.scale.map(|x| x.to_string()).unwrap_or_else(|| "-".into())),
                ],
            ),
        }
    }

    /// Whether a value of type `src` is stored in `dst` unchanged.
    fn fits(&self, dst: &Ty, src: &Ty) -> bool {
        if dst.fam != src.fam {
            // PostgreSQL drops the trailing blanks of a CHAR value converted
            // to varchar or text ('a  ' becomes 'a'); SQL Server keeps them.
            return dst.is_string()
                && src.is_string()
                && dst.fam == Fam::VarChar
                && dst.len.is_none()
                && !(self.d == Dialect::Postgres && src.fam == Fam::Char);
        }
        match dst.fam {
            // A bounded binary target cuts a longer value off in T-SQL and
            // keeps or rejects it elsewhere.
            Fam::Int | Fam::VarChar | Fam::Binary => match (dst.len, src.len) {
                (None, _) => true,
                (Some(d), Some(s)) => d >= s,
                (Some(_), None) => false,
            },
            Fam::Char => dst.len == src.len,
            Fam::Exact => {
                dst.len.is_none()
                    || (src.len.is_some() && dst.len >= src.len && dst.scale == src.scale)
            }
            Fam::Float => dst.len >= src.len,
            // Fewer fractional digits (or other ticks) round the value. A time
            // of unknown precision (SYSDATETIME(), now()) is the time source's
            // question, not this one.
            Fam::Timestamp | Fam::TimestampTz | Fam::Time => {
                src.len.is_none()
                    || (dst.scale == src.scale && dst.len.zip(src.len).is_some_and(|(d, s)| d >= s))
            }
            // The engine does not know which type an `Other` is (BINARY(n),
            // IMAGE, MONEY, an alias type, …): the store may pad, cut off,
            // round or fail.
            Fam::Other => false,
            Fam::Date | Fam::Interval | Fam::Bool => true,
        }
    }

    /// A value stored in a table column. A string literal stored in a
    /// non-string column stands for the value it converts to, as in a cast:
    /// SQL Server converts it to the column's type and PostgreSQL resolves
    /// the untyped literal to it, both keeping the column's precision and
    /// scale. One whose conversion may round, fail or depend on settings
    /// goes through `conv` and is not compared across dialects.
    fn conv_col(&mut self, table: &str, col: &str, v: (Term, Option<Ty>)) -> (Term, Option<Ty>) {
        let (cty, _) = self.col_ty(table, col);
        if let (Some(d), Some(s)) = (cty, v.0.is("str").then(|| v.0.kid(0)).flatten()) {
            let target = self
                .schema
                .table(Self::base_table(table))
                .and_then(|t| t.column(col))
                .and_then(|c| c.target)
                .filter(|t| !matches!(t, Target::Bit | Target::Bool));
            if let Some(target) = target.filter(|_| !d.is_string()) {
                if let Some((t, asm)) = s
                    .atom()
                    .and_then(|s| literal::convert(s, target, self.d, true))
                {
                    if let Some(x) = asm {
                        self.assume(x);
                    }
                    return (t, Some(d));
                }
            }
        }
        self.conv(&cty, v)
    }

    /// Implicit conversion of a value to a declared type: dropped when it is
    /// provably a no-op.
    fn conv(&mut self, dst: &Option<Ty>, v: (Term, Option<Ty>)) -> (Term, Option<Ty>) {
        self.conv_in(dst, v, false)
    }

    /// `conv` for a PL/pgSQL variable, parameter or return value, which
    /// PostgreSQL converts through text where no assignment cast exists.
    fn conv_var(&mut self, dst: &Option<Ty>, v: (Term, Option<Ty>)) -> (Term, Option<Ty>) {
        self.conv_in(dst, v, true)
    }

    fn conv_in(
        &mut self,
        dst: &Option<Ty>,
        v: (Term, Option<Ty>),
        var: bool,
    ) -> (Term, Option<Ty>) {
        let Some(d) = dst else {
            if !matches!(v.0.head(), "null") {
                self.assume(asm::UNTYPED_STORE);
            }
            return v;
        };
        if let Some(b) = self.bool_conv(d, &v, var) {
            return b;
        }
        // An integer literal in the range of an integer type is stored as is.
        if let (Fam::Int, Some(bits), true) = (d.fam, d.len, v.0.is("num")) {
            let lit =
                v.0.kid(0)
                    .and_then(Term::atom)
                    .and_then(|x| x.parse::<i128>().ok());
            // T-SQL tinyint holds 0 to 255.
            let range = match bits {
                8 => 0..256,
                _ => -(1i128 << (bits - 1).min(127))..(1i128 << (bits - 1).min(127)),
            };
            if bits > 1 && bits <= 64 && lit.is_some_and(|x| range.contains(&x)) {
                return (v.0, Some(*d));
            }
        }
        match &v.1 {
            Some(s) if self.fits(d, s) => v,
            None if matches!(v.0.head(), "null") => v,
            _ => self.convert(*d, v, if var { How::Var } else { How::Store }),
        }
    }

    // ---- bit and boolean ------------------------------------------------
    //
    // A T-SQL bit holds 0 or 1 and a PostgreSQL boolean false or true, so
    // the terms write both as the bit values: a boolean literal compared
    // with, stored in or combined with a boolean becomes `num 1` / `num 0`.
    // PostgreSQL has no operator, cast or assignment between boolean and the
    // numbers; where it would need one, the term is `type-error` (the
    // statement fails there). Checked on SQL Server 2022 and PostgreSQL 18
    // (eval/bit-boolean).

    /// T-SQL bit.
    fn is_bit(&self, t: &Option<Ty>) -> bool {
        self.d == Dialect::Tsql && t.is_some_and(|t| t.fam == Fam::Int && t.len == Some(1))
    }

    /// PostgreSQL boolean.
    fn is_bool(&self, t: &Option<Ty>) -> bool {
        self.d == Dialect::Postgres && t.is_some_and(|t| t.fam == Fam::Bool)
    }

    /// TRUE / FALSE as the bit value 1 / 0.
    fn bit_value(t: Term) -> Term {
        match (t.head(), t.kid(0).and_then(Term::atom)) {
            ("bool", Some("TRUE")) => n("num", vec![a("1")]),
            ("bool", Some("FALSE")) => n("num", vec![a("0")]),
            _ => t,
        }
    }

    /// A PostgreSQL statement that fails on a boolean meeting a number.
    fn type_error(&self, kind: &str, kids: Vec<Term>) -> Term {
        let mut v = vec![a("postgres"), a(kind)];
        v.extend(kids);
        n("type-error", v)
    }

    /// Whether a numeric literal term is zero, nonzero, or not a literal.
    fn literal_zero(t: &Term) -> Option<bool> {
        if !t.is("num") {
            return None;
        }
        let s = t.kid(0)?.atom()?;
        let digits = s.trim_start_matches(['-', '+']);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            return None;
        }
        Some(digits.bytes().all(|b| b == b'0' || b == b'.'))
    }

    /// `x <> 0`: a number as a bit or boolean (NULL stays NULL).
    fn nonzero(t: Term) -> Term {
        // Of a CASE, the branch taken: a literal result is 0 or 1.
        if t.is("case") {
            let branch = |v: &Term| match Self::literal_zero(v) {
                Some(z) => n("num", vec![a(if z { "0" } else { "1" })]),
                None if v.atom() == Some("null") => v.clone(),
                None => Self::nonzero(v.clone()),
            };
            let ks = t.kids();
            let mut kids = vec![ks[0].clone()];
            for k in &ks[1..] {
                kids.push(match (k.is("when"), k.kid(0), k.kid(1)) {
                    (true, Some(c), Some(v)) => n("when", vec![c.clone(), branch(v)]),
                    _ => branch(k),
                });
            }
            return n("case", kids);
        }
        let (x, y, op) = order_operands("<>", t, n("num", vec![a("0")]));
        n("cmp", vec![a(op), x, y])
    }

    /// Storing a value in a bit or boolean, when one of them is involved.
    fn bool_conv(
        &mut self,
        d: &Ty,
        v: &(Term, Option<Ty>),
        var: bool,
    ) -> Option<(Term, Option<Ty>)> {
        let dst = Some(*d);
        if self.is_bit(&dst) {
            if self.is_bit(&v.1) {
                return None;
            }
            // T-SQL stores 0 as 0 and any other number as 1.
            if let Some(zero) = Self::literal_zero(&v.0) {
                return Some((n("num", vec![a(if zero { "0" } else { "1" })]), dst));
            }
            if v.0.is("str") {
                let s = v.0.kid(0)?.atom()?;
                let (t, _) = literal::convert(s, Target::Bit, self.d, false)?;
                return Some((t, dst));
            }
            if v.1.is_some_and(|t| t.is_numeric()) {
                return Some((Self::nonzero(v.0.clone()), dst));
            }
            return None;
        }
        if !self.is_bool(&dst) {
            return None;
        }
        if self.is_bool(&v.1) {
            return Some((Self::bit_value(v.0.clone()), dst));
        }
        if v.0.is("str") {
            let s = v.0.kid(0)?.atom()?;
            let (t, _) = literal::convert(s, Target::Bool, self.d, false)?;
            return Some((Self::bit_value(t), dst));
        }
        if v.1.is_some_and(|t| t.is_numeric()) {
            if !var {
                // column "c" is of type boolean but expression is of type integer
                return Some((self.type_error("assign", vec![v.0.clone()]), dst));
            }
            // PL/pgSQL converts through text: '0' and '1' are read, any other
            // number fails.
            return Some(match Self::literal_zero(&v.0) {
                Some(zero) if matches!(v.0.kid(0).and_then(Term::atom), Some("0" | "1")) => {
                    (n("num", vec![a(if zero { "0" } else { "1" })]), dst)
                }
                _ => (n("dfn", vec![self.dtag(), a("bool-io"), v.0.clone()]), dst),
            });
        }
        None
    }

    /// A condition: in PostgreSQL a bare boolean column or variable is the
    /// same test as T-SQL's `bit = 1`, and `NOT` of one as `bit = 0`.
    fn cond(&mut self, e: &Expr) -> R<(Term, Option<Ty>)> {
        let (t, ty) = self.expr(e)?;
        Ok((self.bare_bool(t, ty), ty))
    }

    fn bare_bool(&self, t: Term, ty: Option<Ty>) -> Term {
        if self.is_bool(&ty) && matches!(t.head(), "col" | "sym") {
            let (x, y, op) = order_operands("=", t, n("num", vec![a("1")]));
            return n("cmp", vec![a(op), x, y]);
        }
        t
    }

    /// PostgreSQL: values of one CASE or COALESCE must share a type; a
    /// boolean among numbers fails, TRUE / FALSE among booleans are bit values.
    fn bool_mix(&self, vals: &mut [(Term, Option<Ty>)]) -> Option<Term> {
        if !vals.iter().any(|v| self.is_bool(&v.1)) {
            return None;
        }
        if vals.iter().any(|v| v.1.is_some_and(|t| t.is_numeric())) {
            return Some(self.type_error("mix", vals.iter().map(|v| v.0.clone()).collect()));
        }
        for v in vals.iter_mut() {
            v.0 = Self::bit_value(v.0.clone());
        }
        None
    }

    // ---- expressions ----------------------------------------------------

    pub fn expr(&mut self, e: &Expr) -> R<(Term, Option<Ty>)> {
        if let Some(x) = module_setting_test(self.unit, e) {
            self.assume(x);
            return self.expr(&Expr::Special("TRUE".into()));
        }
        Ok(match e {
            Expr::Name(parts) => self.resolve(parts)?,
            Expr::Var(v) => {
                if v.starts_with("@@") {
                    (n("dfn", vec![a("tsql"), a(lc(v))]), None)
                } else {
                    self.sym(v)
                        .unwrap_or_else(|| (n("sym", vec![a(lc(v))]), None))
                }
            }
            Expr::Num(s)
                if self.d == Dialect::Tsql
                    && s.len() >= 2
                    && s[..2].eq_ignore_ascii_case("0x")
                    && hex_bytes(&s[2..]).is_some() =>
            {
                (
                    n("bytes", vec![a(hex_bytes(&s[2..]).unwrap_or_default())]),
                    None,
                )
            }
            Expr::Num(s) => self.num_lit(s),
            Expr::Str(s) => (
                n("str", vec![a(s.clone())]),
                Some(Ty {
                    fam: Fam::VarChar,
                    len: Some(s.chars().count().max(1) as u32),
                    scale: None,
                }),
            ),
            Expr::Null => (a("null"), None),
            Expr::Star(q) => (
                n(
                    "star",
                    vec![a(q.clone().map(|x| lc(&x)).unwrap_or_default())],
                ),
                None,
            ),
            Expr::Call(c) => self.call(c)?,
            Expr::Bin(op, l, r) => self.binary(op, l, r)?,
            Expr::Unary(op, x) => {
                let (t, ty) = self.expr(x)?;
                if op == "NOT" {
                    if self.is_bool(&ty) && matches!(t.head(), "col" | "sym") {
                        let (x, y, op) = order_operands("=", t, n("num", vec![a("0")]));
                        (n("cmp", vec![a(op), x, y]), Some(Ty::of(Fam::Bool)))
                    } else {
                        (n("not", vec![self.bare_bool(t, ty)]), None)
                    }
                } else {
                    (n("neg", vec![t]), ty)
                }
            }
            Expr::IsNull(x, neg) => {
                let (t, _) = self.expr(x)?;
                (n("isnull", vec![t, a(neg.to_string())]), None)
            }
            Expr::InList(x, list, neg) => {
                let (t, ty) = self.expr(x)?;
                let mut items = Vec::new();
                let mut item_tys = Vec::new();
                let (mut string, mut known) = (ty.is_some_and(|t| t.is_string()), ty.is_some());
                // String literals convert to the type of a non-string operand, as in `=`.
                let target = ty
                    .is_some_and(|t| !t.is_string())
                    .then(|| self.target_of(&t, ty));
                for l in list {
                    if let (Expr::Str(s), Some(target)) = (l, target) {
                        items.push(self.lit_as(s, target, false));
                        continue;
                    }
                    let (it, ity) = self.expr(l)?;
                    item_tys.push(ity);
                    string |= ity.is_some_and(|t| t.is_string());
                    known |= ity.is_some();
                    items.push(it);
                }
                if self.is_bool(&ty) {
                    if item_tys.iter().any(|t| t.is_some_and(|t| t.is_numeric())) {
                        let mut kids = vec![t, a(neg.to_string())];
                        kids.extend(items);
                        return Ok((self.type_error("in", kids), None));
                    }
                    items = items.into_iter().map(Self::bit_value).collect();
                }
                items.sort();
                let mut kids = vec![t, n("list", items), a(neg.to_string())];
                // Like `=`: string membership depends on the collation. With
                // a string operand and string items it is string equality
                // (`coll-eq`); otherwise a conversion may take part.
                if string {
                    let all_strings = ty.is_some_and(|t| t.is_string())
                        && item_tys.iter().all(|t| t.is_some_and(|t| t.is_string()));
                    kids.push(if all_strings {
                        n("coll-eq", vec![self.dtag()])
                    } else {
                        self.dtag()
                    });
                } else if !known {
                    self.assume(asm::UNTYPED);
                }
                (n("in", kids), None)
            }
            Expr::InQuery(x, q, neg) => {
                let (t, ty) = self.expr(x)?;
                let q = self.query(q, QCtx::Scalar)?;
                let mut kids = vec![t, q.0, a(neg.to_string())];
                // The subquery's column type is not tracked, so only a known
                // non-string operand avoids the collation.
                if ty.map_or(true, |t| t.is_string()) {
                    kids.push(self.dtag());
                }
                (n("inq", kids), None)
            }
            Expr::Between(x, lo, hi, neg) => {
                let (t, ty) = self.expr(x)?;
                // String literal bounds convert to the type of a non-string operand.
                let target = ty
                    .is_some_and(|t| !t.is_string())
                    .then(|| self.target_of(&t, ty));
                let bound = |e: &Expr, c: &mut Self| -> R<(Term, Option<Ty>)> {
                    Ok(match (e, target) {
                        (Expr::Str(s), Some(target)) => (c.lit_as(s, target, true), ty),
                        _ => c.expr(e)?,
                    })
                };
                let (lo, lty) = bound(lo, self)?;
                let (hi, hty) = bound(hi, self)?;
                // A boolean between numbers (or a number between booleans).
                let tys = [ty, lty, hty];
                let numeric = |t: &Option<Ty>| t.is_some_and(|t| t.is_numeric());
                if tys.iter().any(|t| self.is_bool(t)) && tys.iter().any(numeric) {
                    return Ok((self.type_error("cmp", vec![t, lo, hi]), None));
                }
                let (t, lo, hi) = match tys.iter().any(|t| self.is_bool(t)) {
                    true => (Self::bit_value(t), Self::bit_value(lo), Self::bit_value(hi)),
                    false => (t, lo, hi),
                };
                let mut kids = vec![t, lo, hi, a(neg.to_string())];
                if ty.map_or(true, |t| t.is_string()) {
                    kids.push(self.dtag());
                }
                (n("between", kids), None)
            }
            Expr::Like(x, p, neg, ci) => {
                let (x, ty) = self.expr(x)?;
                // A varchar value against a literal pattern without `[` or
                // `\` differs across dialects only by the collation
                // (`coll-like`): case, trailing spaces of the value (T-SQL
                // varchar drops them) and characters the collation expands
                // (checked on SQL Server 2022). T-SQL `[...]` is a character
                // class and PostgreSQL's `\` an escape; a char value is
                // blank-padded on one side only.
                let plain = ty.is_some_and(|t| t.fam == Fam::VarChar)
                    && matches!(p.as_ref(), Expr::Str(s) if !s.contains(['[', '\\']));
                let p = self.expr(p)?.0;
                let coll = if plain {
                    n("coll-like", vec![self.dtag()])
                } else {
                    self.dtag()
                };
                (
                    n(
                        "like",
                        vec![coll, x, p, a(neg.to_string()), a(ci.to_string())],
                    ),
                    None,
                )
            }
            // PostgreSQL `'\xAB01'::bytea` (hex input format): the bytes T-SQL
            // writes 0xAB01.
            Expr::Cast(x, ty)
                if self.d == Dialect::Postgres
                    && ty.name.eq_ignore_ascii_case("BYTEA")
                    && matches!(x.as_ref(), Expr::Str(s) if hex_bytes(s.strip_prefix("\\x").unwrap_or("-")).is_some()) =>
            {
                let Expr::Str(s) = x.as_ref() else {
                    unreachable!()
                };
                let hex = hex_bytes(&s[2..]).unwrap_or_default();
                (n("bytes", vec![a(hex)]), None)
            }
            Expr::Cast(x, ty) => {
                if let Expr::Bin(op, l, r) = x.as_ref() {
                    if self.d == Dialect::Postgres
                        && op == "+"
                        && ty.name.eq_ignore_ascii_case("DATE")
                        && ty.args.is_empty()
                    {
                        if let Some(t) = self.pg_date_add(l, r, true)? {
                            return Ok(t);
                        }
                    }
                }
                let dst = Ty::from_type(ty, self.d, true);
                // A string literal cast to a type whose conversion is modelled
                // stands for the value it converts to ('12' as int is 12 on
                // every engine); one that may round, fail or depend on
                // settings is not compared.
                if let (Expr::Str(s), Some(target)) = (x.as_ref(), Target::of_type(ty, self.d)) {
                    match literal::convert(s, target, self.d, true) {
                        Some((t, asm)) => {
                            if let Some(x) = asm {
                                self.assume(x);
                            }
                            return Ok((t, Some(dst)));
                        }
                        None => {
                            return Ok((
                                n(
                                    "lit-conv",
                                    vec![self.dtag(), a(target.name()), a(s.clone())],
                                ),
                                Some(dst),
                            ))
                        }
                    }
                }
                // EXTRACT(part FROM x)::int is DATEPART's integer; for
                // seconds only once floor or trunc drops the fraction
                // (::int rounds 59.9 to 60).
                if self.d == Dialect::Postgres && dst.fam == Fam::Int && dst.len == Some(32) {
                    let inner = match x.as_ref() {
                        Expr::Extract(f, y) if !f.eq_ignore_ascii_case("second") => Some((f, y)),
                        Expr::Call(c)
                            if c.name.len() == 1
                                && matches!(lc(&c.name[0]).as_str(), "floor" | "trunc")
                                && c.args.len() == 1 =>
                        {
                            match &c.args[0] {
                                Expr::Extract(f, y) if f.eq_ignore_ascii_case("second") => {
                                    Some((f, y))
                                }
                                _ => None,
                            }
                        }
                        _ => None,
                    };
                    if let Some((f, y)) = inner {
                        let v = self.expr(y)?;
                        if let Some(t) = self.datepart(&lc(f), v) {
                            return Ok(t);
                        }
                    }
                }
                let v = self.expr(x)?;
                self.cast(dst, v)
            }
            // The converter's CHARINDEX of a needle that may be empty:
            // CASE WHEN length(x) = 0 THEN 0 ELSE strpos(s, x) END.
            Expr::Case(None, whens, Some(els))
                if self.d == Dialect::Postgres && charindex_case(whens, els).is_some() =>
            {
                let (hay, needle) = charindex_case(whens, els).unwrap();
                let (h, hty) = self.expr(hay)?;
                let (x, xty) = self.expr(needle)?;
                if [hty, xty]
                    .iter()
                    .all(|t| t.is_some_and(|t| t.fam == Fam::VarChar))
                {
                    let ci = self.collation_marker();
                    return Ok((n("strpos", vec![a(ci), h, x]), Some(INT32)));
                }
                (n("dfn", vec![self.dtag(), a("charindex-case"), h, x]), None)
            }
            Expr::Case(op, whens, els) => {
                let mut kids = Vec::new();
                match op {
                    Some(o) => {
                        let (t, ty) = self.expr(o)?;
                        if ty.map_or(true, |t| t.is_string()) {
                            kids.push(self.dtag());
                        }
                        kids.push(t);
                    }
                    None => kids.push(a("-")),
                }
                let mut ty = None;
                let mut conds = Vec::new();
                let mut vals = Vec::new();
                for (w, t) in whens {
                    conds.push(if op.is_some() {
                        self.expr(w)?.0
                    } else {
                        self.cond(w)?.0
                    });
                    let (t, tty) = self.expr(t)?;
                    ty = ty.or(tty);
                    vals.push((t, tty));
                }
                if let Some(e) = els {
                    vals.push(self.expr(e)?);
                }
                if let Some(err) = self.bool_mix(&mut vals) {
                    return Ok((err, None));
                }
                let mut vals = vals.into_iter().map(|v| v.0);
                for w in conds {
                    kids.push(n("when", vec![w, vals.next().unwrap()]));
                }
                kids.push(match els {
                    Some(_) => vals.next().unwrap(),
                    None => a("null"),
                });
                (n("case", kids), ty)
            }
            Expr::Exists(q) => match super::objcheck::of(q, self.d) {
                // A catalog existence check: the same term on both sides
                // when it looks for the same kind and name.
                Some(c) => {
                    self.assume(super::objcheck::ASSUMPTION);
                    (n("catalog-exists", vec![a(c.kind.name()), a(c.name)]), None)
                }
                None => (n("exists", vec![self.query(q, QCtx::Exists)?.0]), None),
            },
            Expr::Subquery(q) => {
                let (t, ty) = self.query(q, QCtx::Scalar)?;
                (n("subq", vec![t]), ty)
            }
            Expr::Tuple(list) => {
                let mut v = Vec::new();
                for x in list {
                    v.push(self.expr(x)?.0);
                }
                (n("tuple", v), None)
            }
            Expr::Special(s) => self.special(s)?,
            Expr::Extract(f, x) => {
                let x = self.expr(x)?.0;
                (
                    n("extract", vec![a(f.clone()), x]),
                    Some(Ty::of(Fam::Exact)),
                )
            }
        })
    }

    fn time(&mut self, kind: &str) -> (Term, Option<Ty>) {
        if kind == "stmt" && self.d == Dialect::Tsql && !self.single_row {
            self.assume(asm::SYSDATETIME);
        }
        (n("time", vec![a(kind)]), Some(Ty::of(Fam::Timestamp)))
    }

    fn special(&mut self, s: &str) -> R<(Term, Option<Ty>)> {
        Ok(match (self.d, s) {
            (Dialect::Db2i, "CURRENT_TIMESTAMP") => self.time("stmt"),
            (Dialect::Postgres, "CURRENT_TIMESTAMP" | "LOCALTIMESTAMP") => self.time("txn"),
            (Dialect::Tsql, "CURRENT_TIMESTAMP") => self.time("stmt-datetime"),
            (_, "TRUE" | "FALSE") => (n("bool", vec![a(s)]), Some(Ty::of(Fam::Bool))),
            _ => (n("dfn", vec![self.dtag(), a(lc(s))]), None),
        })
    }

    fn cast(&mut self, dst: Ty, v: (Term, Option<Ty>)) -> (Term, Option<Ty>) {
        let to = Some(dst);
        if self.is_bool(&v.1) {
            // PostgreSQL casts boolean to integer (0 / 1, the bit value) and
            // to text ('true' / 'false'), and to no other type.
            return match dst.fam {
                Fam::Bool => (Self::bit_value(v.0), to),
                // 0 or 1: typed as a bit value, so its text is one digit.
                Fam::Int if dst.len == Some(32) => (
                    Self::bit_value(v.0),
                    Some(Ty {
                        len: Some(1),
                        ..dst
                    }),
                ),
                _ if dst.is_string() => (n("booltext", vec![Self::ty_term(&to), v.0]), to),
                _ => (self.type_error("cast", vec![Self::ty_term(&to), v.0]), to),
            };
        }
        if self.is_bool(&to) {
            // Only integer casts to boolean: nonzero is true.
            match v.1 {
                Some(t) if t.fam == Fam::Int && t.len == Some(32) => {
                    return (Self::nonzero(v.0), to)
                }
                Some(t) if t.is_numeric() => {
                    return (self.type_error("cast", vec![Self::ty_term(&to), v.0]), to)
                }
                _ => {}
            }
        }
        if self.is_bit(&to) && !self.is_bit(&v.1) {
            // T-SQL: 0 is 0, any other number 1.
            if let Some(zero) = Self::literal_zero(&v.0) {
                return (n("num", vec![a(if zero { "0" } else { "1" })]), to);
            }
            if v.1.is_some_and(|t| t.is_numeric()) {
                return (Self::nonzero(v.0), to);
            }
        }
        // A bit is 0 or 1 in int, as boolean::integer is. Only to int: in a
        // wider type the arithmetic that follows overflows elsewhere.
        if self.is_bit(&v.1) && dst.fam == Fam::Int && dst.len == Some(32) {
            return (v.0, to);
        }
        // Integer to string: every dialect prints the plain decimal form.
        if dst.is_string() && dst.fam == Fam::VarChar {
            if let Some(src) = v.1 {
                // The longest text of the type: -32768 for smallint.
                let width = match src.len {
                    Some(1) => 1,
                    Some(2..=8) => 3,
                    Some(9..=16) => 6,
                    Some(17..=32) => 11,
                    _ => 20,
                };
                if src.fam == Fam::Int && dst.len.map_or(true, |l| l >= width) {
                    let len = None;
                    return (
                        n("tostr", vec![v.0]),
                        Some(Ty {
                            fam: Fam::VarChar,
                            len,
                            scale: None,
                        }),
                    );
                }
            }
        }
        if dst.fam == Fam::Date
            && v.1
                .is_some_and(|t| matches!(t.fam, Fam::Timestamp | Fam::Date))
        {
            return (n("todate", vec![v.0]), Some(dst));
        }
        // A decimal of known precision and scale to a string long enough:
        // every dialect prints the digits with exactly `scale` decimals.
        if let (Fam::VarChar, Some(src)) = (dst.fam, v.1) {
            if let (Fam::Exact, Some(p), Some(s)) = (src.fam, src.len, src.scale) {
                let width = 1 + p.saturating_sub(s).max(1) + if s > 0 { s + 1 } else { 0 };
                if self.d != Dialect::Db2i && dst.len.map_or(true, |l| l >= width) {
                    return (
                        n("numstr", vec![a(s.to_string()), v.0]),
                        Some(Ty::of(Fam::VarChar)),
                    );
                }
            }
        }
        if let Some(src) = v.1 {
            if src == dst && src.fam != Fam::Other {
                return v;
            }
        }
        self.convert(dst, v, How::Cast)
    }

    // ---- casts and implicit conversions ----------------------------------
    //
    // A conversion keeps one term in every dialect only where SQL Server 2022
    // and PostgreSQL 18 give the same value or the same error (eval/casts).
    // T-SQL truncates a decimal or float converted to an integer, so that
    // conversion is written with `trunc`, which PostgreSQL's `trunc(x)` also
    // lowers to. Any other conversion keeps the dialect and the source type
    // (`dcast`), so it equals only itself. Db2 for i conversions are kept only
    // between integers, to wider decimals and for strings that fit; the open
    // questions are probes V31 (probes/db2i).

    /// The type of a numeric literal as each dialect reads it: an integer in
    /// int range is int; a larger one is decimal in T-SQL and bigint (then
    /// numeric) elsewhere; one with a decimal point is decimal(p, s); one with
    /// an exponent is float in T-SQL and Db2 for i but numeric in PostgreSQL;
    /// `0x1F` is varbinary in T-SQL and an integer in PostgreSQL.
    fn num_lit(&self, s: &str) -> (Term, Option<Ty>) {
        let d = self.d;
        let (neg, body) = match s.strip_prefix('-') {
            Some(b) => (true, b),
            None => (false, s),
        };
        let int = |bits: u32| Ty {
            fam: Fam::Int,
            len: Some(bits),
            scale: None,
        };
        let exact = |p: Option<u32>, s: u32| Ty {
            fam: Fam::Exact,
            len: p,
            scale: Some(s),
        };
        if let Some(hex) = body.strip_prefix("0x") {
            let value = (d == Dialect::Postgres)
                .then(|| i128::from_str_radix(&hex.replace('_', ""), 16).ok())
                .flatten();
            return match value {
                Some(v) => self.num_lit(&format!("{}{v}", if neg { "-" } else { "" })),
                None => (
                    n("hexlit", vec![self.dtag(), a(s)]),
                    Some(Ty::of(Fam::Other)),
                ),
            };
        }
        let text = if d == Dialect::Postgres {
            s.replace('_', "")
        } else {
            s.to_string()
        };
        let body = text.trim_start_matches('-');
        if body.contains('e') {
            if d == Dialect::Postgres {
                return (n("num", vec![a(text)]), Some(Ty::of(Fam::Exact)));
            }
            let ty = Ty {
                fam: Fam::Float,
                len: Some(53),
                scale: None,
            };
            return (n("flt", vec![a(text)]), Some(ty));
        }
        let ty = if let Some((i, f)) = body.split_once('.') {
            let digits = i.trim_start_matches('0').len() as u32 + f.len() as u32;
            exact(Some(digits.max(f.len() as u32).max(1)), f.len() as u32)
        } else {
            match text.parse::<i128>() {
                Ok(v) if i32::try_from(v).is_ok() => int(32),
                Ok(v) if d != Dialect::Tsql && i64::try_from(v).is_ok() => int(64),
                _ => exact(Some(body.trim_start_matches('0').len().max(1) as u32), 0),
            }
        };
        (n("num", vec![a(text)]), Some(ty))
    }

    /// `v` converted to `dst`: by CAST, by storing it in a column, or by
    /// assigning it to a variable, parameter or return value.
    fn convert(&mut self, dst: Ty, v: (Term, Option<Ty>), how: How) -> (Term, Option<Ty>) {
        let to = Some(dst);
        let head = if how == How::Cast { "cast" } else { "conv" };
        // A decimal literal without a fraction (2.0) converts to an integer
        // the same everywhere.
        let whole = v.0.is("num")
            && v.0.kid(0).and_then(Term::atom).is_some_and(|x| {
                x.split_once('.')
                    .is_some_and(|(_, f)| !f.contains('e') && f.bytes().all(|b| b == b'0'))
            });
        // Without a catalog most values have no type: storing or assigning
        // one, or casting it to a number or string, is assumed to convert
        // alike.
        if matches!(v.0.head(), "null") {
            return (n(head, vec![Self::ty_term(&to), v.0]), to);
        }
        if v.1.is_none() {
            let castable = matches!(
                dst.fam,
                Fam::Int | Fam::Exact | Fam::Float | Fam::VarChar | Fam::Char
            );
            if how != How::Cast || castable {
                self.assume(if how == How::Cast {
                    asm::UNTYPED_CAST
                } else {
                    asm::UNTYPED_STORE
                });
                return (n(head, vec![Self::ty_term(&to), v.0]), to);
            }
        }
        let rule = match self.conv_rule(v.1, dst, how) {
            Rule::Trunc | Rule::Dialect
                if whole && dst.fam == Fam::Int && self.d != Dialect::Db2i =>
            {
                Rule::Same
            }
            r => r,
        };
        match rule {
            Rule::Same => (n(head, vec![Self::ty_term(&to), v.0]), to),
            Rule::Trunc => (
                n(
                    head,
                    vec![Self::ty_term(&to), n("fn", vec![a("trunc"), v.0])],
                ),
                to,
            ),
            Rule::Dialect => (
                n(
                    "dcast",
                    vec![
                        self.dtag(),
                        a(how.name()),
                        Self::ty_term(&to),
                        Self::ty_term(&v.1),
                        v.0,
                    ],
                ),
                to,
            ),
        }
    }

    /// Whether converting a `src` value to `dst` gives the same result in
    /// every dialect (eval/casts has the runs behind each line).
    fn conv_rule(&self, src: Option<Ty>, dst: Ty, how: How) -> Rule {
        use Fam::*;
        let Some(src) = src else { return Rule::Dialect };
        // A binary string kept whole: the target holds any length, or one at
        // least as long as the value can be. T-SQL cuts a longer value off
        // silently where the others keep it or fail.
        if src.fam == Binary && dst.fam == Binary {
            let fits = match (dst.len, src.len) {
                (None, _) => true,
                (Some(d), Some(s)) => s <= d,
                (Some(_), None) => false,
            };
            return if fits { Rule::Same } else { Rule::Dialect };
        }
        if matches!(src.fam, Other | Binary) || matches!(dst.fam, Other | Binary) {
            return Rule::Dialect;
        }
        // A value without a fraction: an integer, a decimal of scale 0, or
        // the result of trunc().
        let integral = src.fam == Int || (matches!(src.fam, Exact | Float) && src.scale == Some(0));
        if self.d == Dialect::Db2i {
            return match (src.fam, dst.fam) {
                (Int, Int | Exact) => Rule::Same,
                // A string no longer than the column is stored as it is
                // (blank-padded in CHAR).
                (Char | VarChar, Char | VarChar)
                    if how == How::Store && src.len.zip(dst.len).is_some_and(|(s, d)| s <= d) =>
                {
                    Rule::Same
                }
                (Exact, Exact)
                    if dst.len.is_none()
                        || (src.len.is_some()
                            && src.scale.is_some()
                            && dst.scale >= src.scale
                            && dst.len.zip(src.len).is_some_and(|(d, s)| {
                                d - dst.scale.unwrap_or(0) >= s - src.scale.unwrap_or(0)
                            })) =>
                {
                    Rule::Same
                }
                _ => Rule::Dialect,
            };
        }
        match (src.fam, dst.fam) {
            // Out of range is an error on both sides.
            (Int, Int | Exact | Float) => Rule::Same,
            // Both round half away from zero (1.555 → 1.56, -2.5 → -3).
            (Exact, Exact) => Rule::Same,
            (Exact | Float, Int) if integral => Rule::Same,
            // T-SQL truncates (1.5 → 1, -1.5 → -1); PostgreSQL rounds,
            // numeric half away from zero and float half to even.
            (Exact | Float, Int) if self.d == Dialect::Tsql => Rule::Trunc,
            (Float, Float) => Rule::Same,
            // Explicit casts truncate silently on both sides, and storing too
            // long a string fails on both; a T-SQL variable truncates where a
            // PL/pgSQL one fails. CHAR pads on both; what differs is how
            // padded values compare and concatenate, which those terms keep.
            // PostgreSQL drops a CHAR value's trailing blanks when it becomes
            // varchar or text; SQL Server keeps them.
            (Char, VarChar) if self.d == Dialect::Postgres => Rule::Dialect,
            (Char | VarChar, Char | VarChar) if how != How::Var => Rule::Same,
            (Timestamp | Date, Date) | (Date, Timestamp) => Rule::Same,
            // Fewer fractional digits round differently: PostgreSQL rounds a
            // half away from 2000-01-01 and to 24:00:00.
            (Timestamp, Timestamp | Time) | (Time, Time)
                if src.scale == dst.scale
                    && src
                        .len
                        .zip(dst.len)
                        .map_or(src.len.is_none(), |(s, d)| d >= s) =>
            {
                Rule::Same
            }
            _ => Rule::Dialect,
        }
    }

    /// A call to a scalar user function the side's catalog holds: `ufn`
    /// with the function's name and the values its parameters receive, the
    /// same term in T-SQL and PostgreSQL. Each argument goes through the
    /// conversion to its parameter's type the side applies: SQL Server cuts,
    /// rounds and pads to the declared type, PostgreSQL ignores its length,
    /// precision and scale (eval/user-functions), so an argument the
    /// parameter does not take unchanged on both sides leaves the two terms
    /// apart. The function itself is the conversion of the other side's,
    /// compared in the file that creates it (`ASM-UDF-CALLEE`, which
    /// `sqldiff convert` checks against that file's own status), and called
    /// in a statement it may run once per row or fewer times
    /// (`ASM-UDF-CALL-CONTEXT`). `None` when the call is not of that form.
    fn user_fn(&mut self, c: &Call) -> R<Option<(Term, Option<Ty>)>> {
        // The converter writes T-SQL `dbo.f(x)` as `f(x)`: unqualified, a
        // PostgreSQL call compared with T-SQL may be that conversion, unless
        // a built-in of the name could take it first.
        let unqualified = c.name.len() == 1
            && self.d == Dialect::Postgres
            && self.other == Some(Dialect::Tsql)
            && !c.name[0].starts_with('"')
            && !pg_builtin_name(&c.name[0]);
        if !matches!(self.d, Dialect::Tsql | Dialect::Postgres)
            || (c.name.len() != 2 && !unqualified)
            || c.distinct
            || c.star
            || c.over
            || !c.order.is_empty()
        {
            return Ok(None);
        }
        let Some(view) = self.schema.catalog() else {
            return Ok(None);
        };
        let parts: Vec<crate::catalog::Ident> = c
            .name
            .iter()
            .map(|x| match x.strip_prefix('"') {
                Some(q) => crate::catalog::Ident::new(q, true),
                None => crate::catalog::Ident::new(x.as_str(), false),
            })
            .collect();
        let found: Vec<&crate::catalog::Routine> = match view.routines(&parts) {
            crate::catalog::Lookup::Found(rs) => rs
                .into_iter()
                .filter(|r| r.kind == crate::catalog::RoutineKind::Function)
                .collect(),
            _ => Vec::new(),
        };
        let [r] = found.as_slice() else {
            return Ok(None);
        };
        let Some(ret) = &r.returns else {
            return Ok(None);
        };
        let set = ["TABLE", "SETOF", "RECORD", "VOID", "TRIGGER"];
        if set.iter().any(|x| ret.base.starts_with(x))
            || r.params.len() != c.args.len()
            || r.params
                .iter()
                .any(|p| p.mode != crate::catalog::ParamMode::In)
        {
            return Ok(None);
        }
        let pg = |t: Ty| match self.d {
            Dialect::Postgres => untypmod(t),
            _ => t,
        };
        let (schema, name) = match c.name.as_slice() {
            [n] => (String::new(), lc(n)),
            _ => (lc(&c.name[0]), lc(&c.name[1])),
        };
        let default = unqualified
            || matches!(
                (self.d, schema.as_str()),
                (Dialect::Tsql, "dbo") | (Dialect::Postgres, "public")
            );
        let key = if default {
            name
        } else {
            format!("{schema}.{name}")
        };
        let params: Vec<Ty> = r
            .params
            .iter()
            .map(|p| pg(super::schema::ty_of(&p.data_type, self.d)))
            .collect();
        // PostgreSQL returns the value without the declared length,
        // precision and scale; under `ASM-UDF-CALLEE` it is the value the
        // T-SQL function returns, which has them.
        let ret = super::schema::ty_of(ret, self.d);
        let mut kids = vec![a(key.clone())];
        for (x, p) in c.args.iter().zip(params) {
            let v = self.expr(x)?;
            kids.push(self.conv_var(&Some(p), v).0);
        }
        if default {
            self.assume(asm::DEFAULT_SCHEMA);
        }
        if unqualified {
            self.assume(asm::PG_UNQUALIFIED_FUNCTION);
        }
        let id = format!("{}:{key}", asm::UDF_CALLEE.0);
        if !self.asm.iter().any(|y| y.id == id) {
            self.asm.push(Assumption::default(
                &id,
                &format!("{} (here `{}`)", asm::UDF_CALLEE.1, r.display()),
            ));
        }
        self.assume(asm::UDF_CALL_CONTEXT);
        Ok(Some((n("ufn", kids), Some(ret))))
    }

    fn call(&mut self, c: &Call) -> R<(Term, Option<Ty>)> {
        if c.over {
            return self.window(c);
        }
        if c.name.len() == 1 && self.other == Some(Dialect::Tsql) && self.d == Dialect::Postgres {
            if let Some(t) = self.user_fn(c)? {
                return Ok(t);
            }
        }
        let name = lc(c.name.last().map(String::as_str).unwrap_or(""));
        // A qualified name is a user function, even `dbo.max`: T-SQL calls a
        // built-in unqualified only.
        if c.name.len() > 1 {
            if let Some(t) = self.user_fn(c)? {
                return Ok(t);
            }
            let mut args = vec![
                self.dtag(),
                a(c.name.iter().map(|x| lc(x)).collect::<Vec<_>>().join(".")),
            ];
            for x in &c.args {
                args.push(self.expr(x)?.0);
            }
            return Ok((n("dfn", args), None));
        }
        let d = self.d;
        let argc = c.args.len();
        let plain = !c.distinct && c.order.is_empty() && !c.star;
        // OBJECT_NAME(@@PROCID) and OBJECT_SCHEMA_NAME(@@PROCID): the
        // running procedure's or function's own name, as its header writes
        // it (the converter writes the same literal).
        if let Some(part) = own_name(self.unit, c) {
            self.assume(asm::PROCID_NAME);
            return self.expr(&Expr::Str(part));
        }
        // Aggregates and functions with special forms first.
        match (d, name.as_str()) {
            (_, "count") if c.order.is_empty() => {
                let mut kids = vec![a(c.distinct.to_string())];
                if c.star {
                    kids.push(a("*"));
                } else {
                    for x in &c.args {
                        kids.push(self.expr(x)?.0);
                    }
                }
                // COUNT returns int in T-SQL and bigint elsewhere; equal while it fits.
                return Ok((n("count", kids), Some(Ty::of(Fam::Int))));
            }
            (Dialect::Tsql, "string_agg")
            | (Dialect::Postgres, "string_agg")
            | (Dialect::Db2i, "listagg")
                if argc == 2 && !c.distinct =>
            {
                let (x, _) = self.expr(&c.args[0])?;
                let (sep, _) = self.expr(&c.args[1])?;
                let order = self.order_terms(&c.order, None)?;
                match d {
                    Dialect::Tsql => self.assume(asm::STRING_AGG_8000),
                    Dialect::Db2i => self.assume(asm::LISTAGG_LEN),
                    Dialect::Postgres => {}
                }
                return Ok((
                    n("stringagg", vec![x, sep, n("order", order)]),
                    Some(Ty::of(Fam::VarChar)),
                ));
            }
            // CONVERT(type, x) without a style is CAST(x AS type).
            (Dialect::Tsql, "convert") if argc == 2 && plain => {
                let ty = match &c.args[0] {
                    Expr::Name(p) if p.len() == 1 => Some(super::ast::Type {
                        name: p[0].to_uppercase(),
                        args: Vec::new(),
                    }),
                    Expr::Call(t) if t.name.len() == 1 && t.args.iter().all(type_arg) => {
                        Some(super::ast::Type {
                            name: t.name[0].to_uppercase(),
                            args: t
                                .args
                                .iter()
                                .map(|x| match x {
                                    Expr::Num(v) => v.clone(),
                                    _ => "MAX".to_string(),
                                })
                                .collect(),
                        })
                    }
                    _ => None,
                };
                if let Some(ty) = ty {
                    return self.expr(&Expr::Cast(Box::new(c.args[1].clone()), ty));
                }
            }
            (Dialect::Tsql, "dateadd") if argc == 3 && plain => {
                if let Some(field) = dateadd_field(&c.args[0]) {
                    let k = self.expr(&c.args[1])?;
                    let d = self.expr(&c.args[2])?;
                    if let Some(t) = self.date_add(field, k, d) {
                        return Ok(t);
                    }
                }
            }
            // CONVERT(varchar(n), x, style) of a date or date-time: the text
            // to_char prints with the matching format.
            (Dialect::Tsql, "convert") if argc == 3 && plain => {
                if let Some(t) = self.tsql_convert_style(c)? {
                    return Ok(t);
                }
            }
            (Dialect::Tsql, "datepart") if argc == 2 && plain => {
                let part = match &c.args[0] {
                    Expr::Name(p) if p.len() == 1 => tsql_datepart(&p[0]),
                    _ => None,
                };
                if let Some(part) = part {
                    let x = self.expr(&c.args[1])?;
                    if part == "weekday" {
                        // Sunday is 1 under DATEFIRST 7: EXTRACT(DOW) + 1.
                        if let Some(dow) = self.datepart("dow", x) {
                            self.assume(asm::DATEFIRST_7);
                            let mut v = vec![dow.0, n("num", vec![a("1")])];
                            v.sort();
                            return Ok((n("add", v), dow.1));
                        }
                    } else if let Some(t) = self.datepart(part, x) {
                        return Ok(t);
                    }
                }
            }
            (Dialect::Tsql, "year" | "month" | "day") if argc == 1 && plain => {
                let x = self.expr(&c.args[0])?;
                if let Some(t) = self.datepart(&name, x) {
                    return Ok(t);
                }
            }
            // CONCAT of strings is `||` with each NULL read as '' (literals
            // are never NULL). Other argument types are converted to strings
            // first, and a result longer than 4,000 characters is cut unless
            // an argument is (n)varchar(max): neither has a rule here.
            (Dialect::Tsql, "concat") if argc >= 2 && plain => {
                let mut acc: Option<(Term, Option<u32>)> = None;
                for x in &c.args {
                    let (t, ty) = self.expr(x)?;
                    let (t, len) = match (x, ty) {
                        (Expr::Str(_), ty) => (t, ty.and_then(|y| y.len)),
                        (_, Some(y)) if y.fam == Fam::VarChar => (
                            n("fn", vec![a("coalesce"), t, n("str", vec![a("")])]),
                            y.len,
                        ),
                        _ => {
                            let mut kids = vec![a("tsql"), a("concat")];
                            for x in &c.args {
                                kids.push(self.expr(x)?.0);
                            }
                            return Ok((n("dfn", kids), Some(Ty::of(Fam::VarChar))));
                        }
                    };
                    acc = Some(match acc {
                        None => (t, len),
                        Some((l, ll)) => (n("concat", vec![l, t]), ll.zip(len).map(|(p, q)| p + q)),
                    });
                }
                let (t, len) = acc.expect("two or more arguments");
                if len.is_some_and(|l| l > 4000) {
                    return Ok((
                        n("dfn", vec![a("tsql"), a("concat-cut"), t]),
                        Some(Ty::of(Fam::VarChar)),
                    ));
                }
                return Ok((
                    t,
                    Some(Ty {
                        fam: Fam::VarChar,
                        len,
                        scale: None,
                    }),
                ));
            }
            // IIF is shorthand for this CASE; SQL Server rewrites it so.
            (Dialect::Tsql, "iif") if argc == 3 && plain => {
                let [c0, x, y] = [0, 1, 2].map(|k| c.args[k].clone());
                return self.expr(&Expr::Case(None, vec![(c0, x)], Some(Box::new(y))));
            }
            _ => {}
        }
        if !plain {
            return Err(format!(
                "{name}() with DISTINCT, ORDER BY or * is not supported"
            ));
        }
        let mut args = Vec::new();
        for x in &c.args {
            args.push(self.expr(x)?);
        }
        let first_ty = args.first().and_then(|x| x.1);
        let portable = |h: &str, args: Vec<(Term, Option<Ty>)>, ty| {
            (
                n(
                    "fn",
                    std::iter::once(a(h))
                        .chain(args.into_iter().map(|x| x.0))
                        .collect(),
                ),
                ty,
            )
        };
        Ok(match (d, name.as_str(), argc) {
            (_, "coalesce", _) => {
                let mut args = args;
                if let Some(err) = self.bool_mix(&mut args) {
                    return Ok((err, None));
                }
                let ty = args.iter().find_map(|x| x.1);
                portable("coalesce", args, ty)
            }
            (Dialect::Tsql, "isnull", 2) => {
                // ISNULL takes the type of its first argument; it equals COALESCE
                // when the replacement is stored in that type unchanged.
                if self.is_bit(&first_ty) {
                    // The replacement is converted to bit: 0 stays 0, any other number is 1.
                    if let Some((t, _)) = self.bool_conv(&first_ty.unwrap(), &args[1], false) {
                        let mut args = args;
                        args[1] = (t, first_ty);
                        return Ok(portable("coalesce", args, first_ty));
                    }
                }
                let (b, bty) = (&args[1].0, args[1].1);
                let fits = matches!(b.head(), "null")
                    || (b.is("str")
                        && b.kid(0).and_then(Term::atom) == Some("")
                        && first_ty.is_some_and(|t| t.is_string()))
                    || matches!((first_ty, bty), (Some(x), Some(y)) if self.fits(&x, &y));
                if fits {
                    portable("coalesce", args, first_ty)
                } else if let (Some(x), Some(y)) = (first_ty, bty) {
                    let _ = y;
                    (
                        n(
                            "isnull-narrow",
                            vec![
                                Self::ty_term(&Some(x)),
                                args[0].0.clone(),
                                args[1].0.clone(),
                            ],
                        ),
                        Some(x),
                    )
                } else {
                    (
                        n(
                            "dfn",
                            vec![a("tsql"), a("isnull"), args[0].0.clone(), args[1].0.clone()],
                        ),
                        first_ty,
                    )
                }
            }
            // REPLACE matches `from` under the collation: case-insensitively
            // in T-SQL (by default), exactly in PostgreSQL. The collation is
            // kept as a marker, as in `strcmp`, so the two are equal only
            // under ASM-CI-COLLATION in a conversion check. varchar only
            // (PostgreSQL strips char padding first), and only where T-SQL
            // cannot cut the result at 8,000 bytes.
            (Dialect::Tsql | Dialect::Postgres, "replace", 3)
                if args
                    .iter()
                    .all(|x| x.1.is_some_and(|t| t.fam == Fam::VarChar))
                    && replace_fits(d, &c.args, &args) =>
            {
                let ci = self.collation_marker();
                let mut kids = vec![a(ci)];
                kids.extend(args.into_iter().map(|x| x.0));
                (
                    n("strreplace", kids),
                    first_ty.map(|t| Ty { len: None, ..t }),
                )
            }
            // CHARINDEX(needle, s): 0 for an empty needle. PostgreSQL's
            // strpos(s, needle) agrees once the needle cannot be empty, or
            // inside the converter's `CASE WHEN length(x) = 0 THEN 0 ELSE
            // strpos(s, x) END` (lowered in `expr`). Collation as for REPLACE;
            // positions under ASM-TSQL-LEN-BMP.
            (Dialect::Tsql, "charindex", 2)
                if args
                    .iter()
                    .all(|x| x.1.is_some_and(|t| t.fam == Fam::VarChar)) =>
            {
                let ci = self.collation_marker();
                // Positions count UTF-16 code units in T-SQL (non-_SC
                // collations), code points in PostgreSQL.
                self.assume(asm::LEN_BMP);
                let (needle, hay) = (args[0].0.clone(), args[1].0.clone());
                (n("strpos", vec![a(ci), hay, needle]), Some(INT32))
            }
            (Dialect::Postgres, "strpos", 2)
                if args
                    .iter()
                    .all(|x| x.1.is_some_and(|t| t.fam == Fam::VarChar))
                    && nonempty_text(&c.args[1]) =>
            {
                let ci = self.collation_marker();
                let (hay, needle) = (args[0].0.clone(), args[1].0.clone());
                (n("strpos", vec![a(ci), hay, needle]), Some(INT32))
            }
            // LTRIM / RTRIM with one argument remove spaces only, on both
            // sides (not tabs, NBSP or ideographic spaces). varchar only:
            // PostgreSQL strips char padding first.
            (Dialect::Tsql | Dialect::Postgres, "ltrim" | "rtrim", 1)
                if first_ty.is_some_and(|t| t.fam == Fam::VarChar) =>
            {
                let x = args[0].0.clone();
                (n("trim", vec![a(&name[..1]), x]), first_ty)
            }
            // QUOTENAME brackets ([a]]b]), quote_ident double-quotes only
            // when needed ("a]b"): the same identifier, not the same text.
            // Equal only under ASM-QUOTENAME-IDENT, which the converter notes.
            (Dialect::Tsql, "quotename", 1) | (Dialect::Postgres, "quote_ident", 1)
                if first_ty.is_some_and(|t| t.is_string()) =>
            {
                self.assume(asm::QUOTENAME_IDENT);
                let x = args[0].0.clone();
                (n("quoteident", vec![x]), Some(Ty::of(Fam::VarChar)))
            }
            // CEILING / FLOOR: the same value of a number. The result types
            // differ (T-SQL keeps int and decimal(p, 0); PostgreSQL gives
            // double precision for an int and an unconstrained numeric), so
            // each side keeps its own and what follows is typed apart.
            // PostgreSQL takes ceil(double precision) for an integer, which
            // is exact only up to int (bigint above 2^53 is rounded).
            (Dialect::Tsql, "ceiling" | "floor", 1)
            | (Dialect::Postgres, "ceil" | "ceiling" | "floor", 1)
                if first_ty.is_some_and(|t| {
                    t.is_numeric()
                        && (d == Dialect::Tsql
                            || t.fam != Fam::Int
                            || t.len.is_some_and(|l| l <= 32))
                }) =>
            {
                let t = first_ty.unwrap();
                let ty = match (d, t.fam) {
                    (Dialect::Tsql, Fam::Exact) => Ty {
                        scale: Some(0),
                        ..t
                    },
                    (Dialect::Tsql, _) => t,
                    (_, Fam::Exact) => Ty::of(Fam::Exact),
                    _ => Ty {
                        fam: Fam::Float,
                        len: Some(53),
                        scale: None,
                    },
                };
                let h = if name == "floor" { "floor" } else { "ceil" };
                let x = args[0].0.clone();
                (n(h, vec![x]), Some(ty))
            }
            // SQRT of an integer or a float is the correctly rounded double
            // on both sides; PostgreSQL's sqrt(numeric) is a numeric with
            // other digits, so decimals are left out.
            (Dialect::Tsql | Dialect::Postgres, "sqrt", 1)
                if first_ty.is_some_and(|t| matches!(t.fam, Fam::Int | Fam::Float)) =>
            {
                let x = args[0].0.clone();
                (
                    n("sqrt", vec![x]),
                    Some(Ty {
                        fam: Fam::Float,
                        len: Some(53),
                        scale: None,
                    }),
                )
            }
            // LEN counts characters without trailing spaces: PostgreSQL's
            // length(rtrim(x)) (rtrim removes only spaces, as LEN ignores
            // only spaces). T-SQL counts UTF-16 code units.
            (Dialect::Tsql, "len", 1) if first_ty.is_some_and(|t| t.is_string()) => {
                self.assume(asm::LEN_BMP);
                let bits = if first_ty.and_then(|t| t.len).is_some() {
                    32
                } else {
                    64
                };
                let (x, _) = args.into_iter().next().unwrap();
                (
                    n("strlen", vec![x]),
                    Some(Ty {
                        fam: Fam::Int,
                        len: Some(bits),
                        scale: None,
                    }),
                )
            }
            (Dialect::Postgres, "length", 1)
                if matches!(&c.args[0], Expr::Call(r) if r.name.len() == 1
                    && r.name[0].eq_ignore_ascii_case("rtrim")
                    && r.args.len() == 1 && !r.distinct && !r.star && r.order.is_empty()) =>
            {
                let Expr::Call(r) = &c.args[0] else {
                    unreachable!()
                };
                let (x, ty) = self.expr(&r.args[0])?;
                if ty.is_some_and(|t| t.is_string()) {
                    (
                        n("strlen", vec![x]),
                        Some(Ty {
                            fam: Fam::Int,
                            len: Some(32),
                            scale: None,
                        }),
                    )
                } else {
                    (
                        n("dfn", vec![self.dtag(), a("length"), args[0].0.clone()]),
                        None,
                    )
                }
            }
            // REPLICATE and repeat agree for a count that is a literal from 0
            // up (a negative count is NULL in T-SQL, '' in PostgreSQL) while
            // the result stays within 4,000 characters of a known length
            // (T-SQL cuts a non-MAX result at 8,000 bytes).
            // Only varchar: PostgreSQL strips a char value's padding before
            // repeat, T-SQL repeats it.
            (Dialect::Tsql, "replicate", 2) | (Dialect::Postgres, "repeat", 2)
                if first_ty.is_some_and(|t| t.fam == Fam::VarChar) =>
            {
                let count = match &c.args[1] {
                    Expr::Num(v) => v.parse::<u32>().ok(),
                    _ => None,
                };
                let len = first_ty.and_then(|t| t.len);
                let fits = match (d, count, len) {
                    (_, None, _) => false,
                    (Dialect::Tsql, Some(k), Some(l)) => l.saturating_mul(k) <= 4000,
                    // No known length: possibly a non-MAX value cut at
                    // 8,000 bytes (DB_NAME() is nvarchar(128)), so unknown.
                    (Dialect::Tsql, Some(_), None) => false,
                    (_, Some(_), _) => true,
                };
                if fits {
                    let k = count.unwrap();
                    let ty = Ty {
                        fam: Fam::VarChar,
                        len: len.map(|l| l.saturating_mul(k)),
                        scale: None,
                    };
                    let x = args[0].0.clone();
                    (
                        n("repeat", vec![x, n("num", vec![a(k.to_string())])]),
                        Some(ty),
                    )
                } else {
                    (
                        n(
                            "dfn",
                            std::iter::once(self.dtag())
                                .chain(std::iter::once(a(name.clone())))
                                .chain(args.into_iter().map(|x| x.0))
                                .collect(),
                        ),
                        None,
                    )
                }
            }
            // A literal code point from 1 to 127 is the same one-character
            // string in every code page, in UTF-16 and in UTF-8.
            (Dialect::Tsql, "char" | "nchar", 1) | (Dialect::Postgres, "chr", 1)
                if ascii_code(&c.args[0]).is_some() =>
            {
                let ch = ascii_code(&c.args[0]).unwrap() as char;
                (
                    n("str", vec![a(ch.to_string())]),
                    Some(Ty {
                        fam: Fam::VarChar,
                        len: Some(1),
                        scale: None,
                    }),
                )
            }
            (Dialect::Tsql, "db_name", 0) | (Dialect::Postgres, "current_database", 0) => {
                if d == Dialect::Tsql {
                    self.assume(asm::SAME_DATABASE_NAME);
                }
                (n("dbname", vec![]), Some(Ty::of(Fam::VarChar)))
            }
            (Dialect::Postgres, "to_char", 2) => {
                let fmt = match &c.args[1] {
                    Expr::Str(f) => Some(f.as_str()),
                    _ => None,
                };
                let date_only = match first_ty.map(|t| t.fam) {
                    Some(Fam::Date) => Some(true),
                    Some(Fam::Timestamp) => Some(false),
                    _ => None,
                };
                match (fmt, date_only) {
                    // Only the formats a T-SQL style prints, of the input
                    // types it prints them for.
                    (Some(f), Some(dt))
                        if STYLES.iter().any(|&(st, fm, _)| {
                            fm == f && style_format(dt, st).is_some_and(|x| x.0 == f)
                        }) =>
                    {
                        let width = STYLES.iter().find(|x| x.1 == f).map(|x| x.2);
                        let x = args[0].0.clone();
                        (
                            n("datefmt", vec![a(f), x]),
                            Some(Ty {
                                fam: Fam::VarChar,
                                len: width,
                                scale: None,
                            }),
                        )
                    }
                    _ => (
                        n(
                            "dfn",
                            std::iter::once(self.dtag())
                                .chain(std::iter::once(a(name.clone())))
                                .chain(args.into_iter().map(|x| x.0))
                                .collect(),
                        ),
                        None,
                    ),
                }
            }
            (_, "upper" | "ucase", 1) => portable("upper", args, first_ty),
            (_, "lower" | "lcase", 1) => portable("lower", args, first_ty),
            (_, "abs", 1) => portable("abs", args, first_ty),
            (_, "min" | "max", 1) => match first_ty {
                Some(t) if t.is_string() => (
                    n("dfn", vec![self.dtag(), a(name.clone()), args[0].0.clone()]),
                    first_ty,
                ),
                None => {
                    self.assume(asm::UNTYPED);
                    portable(&name, args, first_ty)
                }
                _ => portable(&name, args, first_ty),
            },
            (Dialect::Tsql, "sysdatetime", 0) => self.time("stmt"),
            (Dialect::Tsql, "getdate", 0) => self.time("stmt-datetime"),
            (Dialect::Postgres, "statement_timestamp", 0) => self.time("stmt"),
            (Dialect::Postgres, "now" | "transaction_timestamp", 0) => self.time("txn"),
            (Dialect::Postgres, "clock_timestamp", 0) => self.time("clock"),
            (Dialect::Db2i, "now", 0) => self.time("stmt"),
            (Dialect::Tsql, "scope_identity", 0) => {
                (n("scope_identity", vec![]), Some(Ty::of(Fam::Exact)))
            }
            (Dialect::Tsql, "datediff", 3) => {
                let part = args[0].0.clone();
                let unit = part.kid(1).and_then(Term::atom).unwrap_or("");
                if part.is("col") && matches!(unit, "day" | "dd" | "d") {
                    (
                        n("daydiff", vec![args[1].0.clone(), args[2].0.clone()]),
                        Some(Ty {
                            fam: Fam::Int,
                            len: Some(32),
                            scale: None,
                        }),
                    )
                } else {
                    (
                        n(
                            "dfn",
                            vec![a("tsql"), a("datediff")]
                                .into_iter()
                                .chain(args.into_iter().map(|x| x.0))
                                .collect(),
                        ),
                        None,
                    )
                }
            }
            // trunc drops the fraction. Of anything but numeric it is
            // trunc(double precision), which loses digits past 2^53.
            (Dialect::Postgres, "trunc", 1) if first_ty.is_some_and(|t| t.is_numeric()) => {
                let t = first_ty.unwrap();
                let ty = match t.fam {
                    Fam::Exact => Ty {
                        scale: Some(0),
                        ..t
                    },
                    _ => Ty {
                        fam: Fam::Float,
                        len: Some(53),
                        scale: Some(0),
                    },
                };
                portable("trunc", args, Some(ty))
            }
            (Dialect::Db2i, "days", 1) => {
                (n("days", vec![args[0].0.clone()]), Some(Ty::of(Fam::Int)))
            }
            (Dialect::Db2i, "varchar" | "char", 1) => {
                let dst = Ty {
                    fam: if name == "char" {
                        Fam::Char
                    } else {
                        Fam::VarChar
                    },
                    len: None,
                    scale: None,
                };
                let v = args.into_iter().next().unwrap();
                self.cast(dst, v)
            }
            _ => {
                let ty = None;
                (
                    n(
                        "dfn",
                        std::iter::once(self.dtag())
                            .chain(std::iter::once(a(name.clone())))
                            .chain(args.into_iter().map(|x| x.0))
                            .collect(),
                    ),
                    ty,
                )
            }
        })
    }

    /// `d + n` (a number of days) or `d + make_interval(…)` in PostgreSQL,
    /// as the `date-add` T-SQL DATEADD lowers to. `to_date`: the sum is
    /// cast to date, which turns PostgreSQL's timestamp result of a date
    /// plus an interval back into a date. `None` for any other sum.
    fn pg_date_add(&mut self, l: &Expr, r: &Expr, to_date: bool) -> R<Option<(Term, Option<Ty>)>> {
        let interval = match r {
            Expr::Call(c) if is_make_interval(c) => Some(c),
            _ => None,
        };
        // Only these two shapes; anything else is lowered as it was.
        if interval.is_none() && to_date {
            return Ok(None);
        }
        let d = self.expr(l)?;
        let is_date = d.1.is_some_and(|t| t.fam == Fam::Date);
        let (field, k) = match interval {
            Some(c) => {
                // Exactly one amount other than a zero literal.
                let given: Vec<usize> = (0..c.args.len())
                    .filter(|&i| !is_zero_literal(&c.args[i]))
                    .collect();
                let [i] = given.as_slice() else {
                    return Ok(None);
                };
                // date + interval is a timestamp: only its cast back to date
                // is DATEADD on a date.
                if is_date != to_date {
                    return Ok(None);
                }
                (MAKE_INTERVAL_FIELDS[*i], self.expr(&c.args[*i])?)
            }
            None if is_date => ("days", self.expr(r)?),
            None => return Ok(None),
        };
        Ok(self.date_add(field, k, d))
    }

    /// DATEADD of `k` `field`s (`years` … `secs`, PostgreSQL's
    /// make_interval names) to `d`, where both engines give the same
    /// value: `k` an integer of at most 32 bits (T-SQL converts other
    /// numbers to int, truncating; PostgreSQL's secs keeps a fraction), and
    /// `d` a date (whole days or longer; T-SQL rejects smaller parts) or a
    /// timestamp without T-SQL's datetime/smalldatetime ticks, which round.
    /// Both add months by moving a day the month lacks to its last day,
    /// and add hours, minutes and seconds exactly. The result has `d`'s
    /// type, as in T-SQL.
    fn date_add(
        &mut self,
        field: &str,
        k: (Term, Option<Ty>),
        d: (Term, Option<Ty>),
    ) -> Option<(Term, Option<Ty>)> {
        let (kt, kty) = k;
        let (dt, dty) = d;
        let kty = kty?;
        if kty.fam != Fam::Int || kty.len.map_or(true, |b| b > 32) {
            return None;
        }
        let dty = dty?;
        let ok = match dty.fam {
            Fam::Date => matches!(field, "years" | "months" | "weeks" | "days"),
            Fam::Timestamp => dty.scale.is_none(),
            _ => false,
        };
        if !ok {
            return None;
        }
        // A week is seven days on both sides; a literal count is folded so
        // `d + 14` meets two weeks.
        let (field, kt) = match (field, int_literal(&kt)) {
            ("weeks", Some(w)) => ("days", n("num", vec![a((w * 7).to_string())])),
            _ => (field, kt),
        };
        self.assume(asm::DATEADD_RANGE);
        if field == "years" {
            self.assume(asm::DATEADD_YEAR_END);
        }
        Some((n("date-add", vec![a(field), kt, dt]), Some(dty)))
    }

    /// The collation of a string match, as `strcmp` writes it: `ci` in
    /// T-SQL (ASM-TSQL-CI-COLLATION), `cs` in PostgreSQL and Db2 for i.
    fn collation_marker(&mut self) -> &'static str {
        match self.d {
            Dialect::Tsql => {
                self.assume(asm::TSQL_CI);
                "ci"
            }
            Dialect::Postgres => "cs",
            Dialect::Db2i => {
                self.assume(asm::DB2_SRTSEQ);
                "cs"
            }
        }
    }

    /// A date part as an integer: `year`, `month`, `day`, `hour`,
    /// `minute`, `second` (truncated) or `dow` (Sunday 0), of a date,
    /// timestamp (without time zone) or time where both dialects define it.
    fn datepart(&mut self, part: &str, x: (Term, Option<Ty>)) -> Option<(Term, Option<Ty>)> {
        let fam = x.1?.fam;
        let ok = match part {
            "year" | "month" | "day" | "dow" => matches!(fam, Fam::Date | Fam::Timestamp),
            "hour" | "minute" | "second" => matches!(fam, Fam::Timestamp | Fam::Time),
            _ => false,
        };
        ok.then(|| {
            (
                n("datepart", vec![a(part), x.0]),
                Some(Ty {
                    fam: Fam::Int,
                    len: Some(32),
                    scale: None,
                }),
            )
        })
    }

    /// T-SQL CONVERT(varchar(n) / nvarchar(n), x, style) of a date or
    /// date-time `x`, for the styles `style_format` knows.
    fn tsql_convert_style(&mut self, c: &Call) -> R<Option<(Term, Option<Ty>)>> {
        let ty = match &c.args[0] {
            Expr::Name(p) if p.len() == 1 => (p[0].to_uppercase(), Some(30)),
            Expr::Call(t) if t.name.len() == 1 && t.args.len() == 1 => {
                let len = match &t.args[0] {
                    Expr::Num(v) => match v.parse::<u32>() {
                        Ok(n) if n > 0 => Some(n),
                        _ => return Ok(None),
                    },
                    Expr::Name(m) if m.len() == 1 && m[0].eq_ignore_ascii_case("max") => None,
                    _ => return Ok(None),
                };
                (t.name[0].to_uppercase(), len)
            }
            _ => return Ok(None),
        };
        if !matches!(ty.0.as_str(), "VARCHAR" | "NVARCHAR") {
            return Ok(None);
        }
        let style = match &c.args[2] {
            Expr::Num(v) => v.parse::<u32>().ok(),
            _ => None,
        };
        let x = self.expr(&c.args[1])?;
        let date_only = match x.1.map(|t| t.fam) {
            Some(Fam::Date) => true,
            Some(Fam::Timestamp) => false,
            _ => return Ok(None),
        };
        let Some((fmt, width)) = style.and_then(|st| style_format(date_only, st)) else {
            return Ok(None);
        };
        let v = (
            n("datefmt", vec![a(fmt), x.0]),
            Some(Ty {
                fam: Fam::VarChar,
                len: Some(width),
                scale: None,
            }),
        );
        // A target that holds the whole text changes nothing.
        if ty.1.map_or(true, |n| n >= width) {
            return Ok(Some(v));
        }
        let dst = Ty {
            fam: Fam::VarChar,
            len: ty.1,
            scale: None,
        };
        Ok(Some(self.cast(dst, v)))
    }

    /// An aggregate `OVER ()`: the aggregate of all the rows the query
    /// returns (after WHERE, GROUP BY and HAVING, before DISTINCT, ORDER BY
    /// and paging), repeated on each row, in both dialects. Only COUNT,
    /// MIN, MAX, SUM and AVG, lowered as without OVER (so a string MIN or
    /// a SUM keeps its dialect); ranking functions are refused, which
    /// T-SQL requires an ORDER BY for.
    fn window(&mut self, c: &Call) -> R<(Term, Option<Ty>)> {
        let name = lc(c.name.last().map(String::as_str).unwrap_or(""));
        if c.name.len() != 1 || !matches!(name.as_str(), "count" | "min" | "max" | "sum" | "avg") {
            return Err(format!("{name}() OVER is not supported"));
        }
        let plain = Call {
            over: false,
            ..c.clone()
        };
        let (t, ty) = self.call(&plain)?;
        Ok((n("win", vec![t]), ty))
    }

    fn binary(&mut self, op: &str, l: &Expr, r: &Expr) -> R<(Term, Option<Ty>)> {
        if self.d == Dialect::Postgres && op == "+" {
            if let Some(t) = self.pg_date_add(l, r, false)? {
                return Ok(t);
            }
        }
        if op == "AND" || op == "OR" {
            let mut parts = Vec::new();
            for side in [l, r] {
                let (t, _) = self.cond(side)?;
                if t.is(&op.to_lowercase()) {
                    parts.extend(t.kids().iter().cloned());
                } else {
                    parts.push(t);
                }
            }
            parts.sort();
            return Ok((n(&op.to_lowercase(), parts), Some(Ty::of(Fam::Bool))));
        }
        let (mut lt, mut lty) = self.expr(l)?;
        let (mut rt, mut rty) = self.expr(r)?;
        let cmp_op = matches!(op, "=" | "<>" | "<" | ">" | "<=" | ">=");
        if cmp_op {
            // A string literal against a value of another type converts to
            // that type: it compares as a value, not under a collation.
            let ordered = !matches!(op, "=" | "<>");
            let non_string = |t: &Option<Ty>| t.is_some_and(|t| !t.is_string());
            match (l, r) {
                (Expr::Str(s), _) if non_string(&rty) => {
                    let target = self.target_of(&rt, rty);
                    lt = self.lit_as(s, target, ordered);
                    lty = rty;
                }
                (_, Expr::Str(s)) if non_string(&lty) => {
                    let target = self.target_of(&lt, lty);
                    rt = self.lit_as(s, target, ordered);
                    rty = lty;
                }
                (Expr::Str(_), _) if rty.is_none() => {
                    return Ok((self.untyped_lit_cmp(op, lt, rt), Some(Ty::of(Fam::Bool))));
                }
                (_, Expr::Str(_)) if lty.is_none() => {
                    return Ok((self.untyped_lit_cmp(op, lt, rt), Some(Ty::of(Fam::Bool))));
                }
                _ => {}
            }
        }
        // SQL Server: no arithmetic between two bits (Msg 402, 8117).
        if self.is_bit(&lty) && self.is_bit(&rty) && matches!(op, "+" | "-" | "*" | "/" | "%") {
            return Ok((
                n("type-error", vec![a("tsql"), a("bit-arith"), a(op), lt, rt]),
                None,
            ));
        }
        if self.is_bool(&lty) || self.is_bool(&rty) {
            let numeric = |t: &Option<Ty>| t.is_some_and(|t| t.is_numeric());
            if numeric(&lty) || numeric(&rty) {
                // operator does not exist: boolean = integer (or + - * / %)
                let kind = if cmp_op { "cmp" } else { "arith" };
                return Ok((self.type_error(kind, vec![a(op), lt, rt]), None));
            }
            if cmp_op {
                lt = Self::bit_value(lt);
                rt = Self::bit_value(rt);
            }
        }
        let any =
            |f: &dyn Fn(&Ty) -> bool| lty.as_ref().is_some_and(f) || rty.as_ref().is_some_and(f);
        match op {
            "=" | "<>" | "<" | ">" | "<=" | ">=" => {
                let string = any(&|t| t.is_string());
                let known = lty.is_some() || rty.is_some();
                if string && any(&|t| !t.is_string()) {
                    // T-SQL converts the string to the other type; PostgreSQL
                    // has no such operator for a typed string.
                    let (x, y, op) = order_operands(op, lt, rt);
                    return Ok((
                        n("cmp-conv", vec![self.dtag(), a(op), x, y]),
                        Some(Ty::of(Fam::Bool)),
                    ));
                }
                if string {
                    return Ok((
                        self.strcmp(op, (lt, lty), (rt, rty)),
                        Some(Ty::of(Fam::Bool)),
                    ));
                }
                // A number against a value of unknown type, which may be real.
                let maybe_real = |t: &Option<Ty>, other: (&Term, Option<Ty>)| {
                    t.is_none()
                        && other.1.is_some_and(|x| x.is_numeric())
                        && !Self::small_int(other.0)
                };
                if !known || maybe_real(&lty, (&rt, rty)) || maybe_real(&rty, (&lt, lty)) {
                    self.assume(asm::UNTYPED);
                }
                if self.real_mix((&lt, lty), (&rt, rty)) {
                    // T-SQL converts the other value to real, PostgreSQL both
                    // to double precision: CAST(0.1 AS real) = 0.1 is true
                    // only in T-SQL.
                    let (x, y, op) = order_operands(op, lt, rt);
                    return Ok((
                        n("cmp-real", vec![self.dtag(), a(op), x, y]),
                        Some(Ty::of(Fam::Bool)),
                    ));
                }
                let (x, y, op) = order_operands(op, lt, rt);
                Ok((n("cmp", vec![a(op), x, y]), Some(Ty::of(Fam::Bool))))
            }
            "||" if self.is_binary(&lt, lty) && self.is_binary(&rt, rty) => {
                Ok(self.concat_bin(lt, lty, rt, rty))
            }
            "||" => {
                if any(&|t| t.fam == Fam::Char) {
                    return Ok((
                        n("concat-char", vec![self.dtag(), lt, rt]),
                        Some(Ty::of(Fam::VarChar)),
                    ));
                }
                let len = match (lty.and_then(|t| t.len), rty.and_then(|t| t.len)) {
                    (Some(x), Some(y)) => Some(x + y),
                    _ => None,
                };
                Ok((
                    n("concat", vec![lt, rt]),
                    Some(Ty {
                        fam: Fam::VarChar,
                        len,
                        scale: None,
                    }),
                ))
            }
            // T-SQL `+` between two binary strings concatenates them.
            "+" if self.d == Dialect::Tsql
                && self.is_binary(&lt, lty)
                && self.is_binary(&rt, rty) =>
            {
                Ok(self.concat_bin(lt, lty, rt, rty))
            }
            "+" if self.d == Dialect::Tsql && (any(&|t| t.is_string()) || l_or_r_is_str(l, r)) => {
                if any(&|t| t.is_numeric() || t.is_temporal()) {
                    return Ok((n("dfn", vec![a("tsql"), a("+"), lt, rt]), None));
                }
                if lty.is_none() || rty.is_none() {
                    self.assume(asm::TSQL_PLUS_STRING);
                }
                if any(&|t| t.fam == Fam::Char) {
                    return Ok((
                        n("concat-char", vec![self.dtag(), lt, rt]),
                        Some(Ty::of(Fam::VarChar)),
                    ));
                }
                let len = match (lty.and_then(|t| t.len), rty.and_then(|t| t.len)) {
                    (Some(x), Some(y)) => Some(x + y),
                    _ => None,
                };
                Ok((
                    n("concat", vec![lt, rt]),
                    Some(Ty {
                        fam: Fam::VarChar,
                        len,
                        scale: None,
                    }),
                ))
            }
            "-" if lt.is("days") && rt.is("days") => Ok((
                n("daydiff", vec![rt.kids()[0].clone(), lt.kids()[0].clone()]),
                Some(Ty {
                    fam: Fam::Int,
                    len: Some(32),
                    scale: None,
                }),
            )),
            "-" if lt.is("todate") && rt.is("todate") && self.d == Dialect::Postgres => Ok((
                n("daydiff", vec![rt.kids()[0].clone(), lt.kids()[0].clone()]),
                Some(Ty {
                    fam: Fam::Int,
                    len: Some(32),
                    scale: None,
                }),
            )),
            "+" | "-" | "*" | "/" | "%" => {
                // Only numbers add alike everywhere: T-SQL `+` concatenates
                // binary strings, PostgreSQL has no bytea `+`.
                if any(&|t| !t.is_numeric()) || lt.is("bytes") || rt.is("bytes") {
                    return Ok((n("arith", vec![self.dtag(), a(op), lt, rt]), None));
                }
                let both_int = lty.is_some_and(|t| t.fam == Fam::Int)
                    && rty.is_some_and(|t| t.fam == Fam::Int);
                if lty.is_none() || rty.is_none() {
                    self.assume(asm::UNTYPED);
                }
                if matches!(op, "*" | "/" | "%") && !both_int {
                    if let Some(ty) = (op == "*").then(|| self.mul_ty(lty, rty)).flatten() {
                        let mut v = vec![lt, rt];
                        v.sort();
                        return Ok((n("mul", v), ty));
                    }
                    // Decimal precision and scale rules differ per dialect.
                    return Ok((n("arith", vec![self.dtag(), a(op), lt, rt]), None));
                }
                let Some(ty) = self.arith_ty(lty, rty) else {
                    return Ok((n("arith", vec![self.dtag(), a(op), lt, rt]), None));
                };
                let h = match op {
                    "+" => "add",
                    "-" => "sub",
                    "*" => "mul",
                    "/" => "div",
                    _ => "mod",
                };
                if h == "add" || h == "mul" {
                    let mut v = vec![lt, rt];
                    v.sort();
                    return Ok((n(h, v), ty));
                }
                Ok((n(h, vec![lt, rt]), ty))
            }
            _ => Ok((n("dfn", vec![self.dtag(), a(op), lt, rt]), None)),
        }
    }

    /// A variable-length binary string: a value of that type, or a binary
    /// constant (`0x01`, `'\x01'::bytea`).
    fn is_binary(&self, t: &Term, ty: Option<Ty>) -> bool {
        ty.is_some_and(|t| t.fam == Fam::Binary) || t.is("bytes")
    }

    /// The length a binary operand can reach: a constant's byte count, a
    /// typed value's declared length, None when unbounded.
    fn binary_len(t: &Term, ty: Option<Ty>) -> Option<u32> {
        if t.is("bytes") {
            return t
                .kid(0)
                .and_then(Term::atom)
                .map(|h| h.len().div_ceil(2) as u32);
        }
        ty.and_then(|t| t.len)
    }

    /// Binary concatenation (T-SQL `+`, PostgreSQL and Db2 for i `||`). NULL
    /// in either operand gives NULL everywhere. T-SQL cuts the result to
    /// 8000 bytes unless an operand is varbinary(max), so a longer bounded
    /// result stays T-SQL's own.
    fn concat_bin(
        &mut self,
        lt: Term,
        lty: Option<Ty>,
        rt: Term,
        rty: Option<Ty>,
    ) -> (Term, Option<Ty>) {
        let len = Self::binary_len(&lt, lty)
            .zip(Self::binary_len(&rt, rty))
            .map(|(x, y)| x + y);
        if self.d == Dialect::Tsql && len.is_some_and(|l| l > 8000) {
            return (n("dfn", vec![a("tsql"), a("+"), lt, rt]), None);
        }
        let len = if self.d == Dialect::Tsql { len } else { None };
        (
            n("concat-bin", vec![lt, rt]),
            Some(Ty {
                fam: Fam::Binary,
                len,
                scale: None,
            }),
        )
    }

    /// Whether a real (single precision) meets a number of another type,
    /// other than a small integer literal.
    fn real_mix(&self, l: (&Term, Option<Ty>), r: (&Term, Option<Ty>)) -> bool {
        let real = |t: &Option<Ty>| t.is_some_and(|t| t.fam == Fam::Float && t.len == Some(24));
        let other = |(t, ty): (&Term, Option<Ty>)| {
            ty.is_some_and(|x| x.is_numeric()) && !real(&ty) && !Self::small_int(t)
        };
        (real(&l.1) && other(r)) || (real(&r.1) && other(l))
    }

    /// An integer literal that a real holds exactly.
    fn small_int(t: &Term) -> bool {
        t.is("num")
            && t.kid(0)
                .and_then(Term::atom)
                .and_then(|x| x.parse::<i64>().ok())
                .is_some_and(|x| x.abs() <= 1 << 24)
    }

    /// The type of an exact product of two exact numbers (integers or
    /// decimals), or None where it may not be exact. PostgreSQL multiplies
    /// numerics exactly. T-SQL gives decimal(p1 + p2 + 1, s1 + s2), exact
    /// when that precision is at most 38; above, it cuts the scale and
    /// rounds. An integer counts as decimal(3, 0), (5, 0), (10, 0) or
    /// (19, 0) by its width.
    fn mul_ty(&self, l: Option<Ty>, r: Option<Ty>) -> Option<Option<Ty>> {
        let (x, y) = (l?, r?);
        let exact = |t: &Ty| matches!(t.fam, Fam::Int | Fam::Exact);
        if !exact(&x) || !exact(&y) {
            return None;
        }
        let digits = |t: &Ty| match t.fam {
            Fam::Int => t.len.map(|b| {
                let p = match b {
                    0..=1 => 1,
                    2..=8 => 3,
                    9..=16 => 5,
                    17..=32 => 10,
                    _ => 19,
                };
                (p, 0)
            }),
            _ => t.len.zip(t.scale),
        };
        match (digits(&x), digits(&y)) {
            (Some((p1, s1)), Some((p2, s2))) => {
                let (p, s) = (p1 + p2 + 1, s1 + s2);
                if self.d == Dialect::Tsql && p > 38 {
                    return None;
                }
                Some(Some(Ty {
                    fam: Fam::Exact,
                    len: Some(p.min(38)),
                    scale: Some(s),
                }))
            }
            _ if self.d == Dialect::Postgres => Some(Some(Ty::of(Fam::Exact))),
            _ => None,
        }
    }

    /// The type of `+` or `-` of two numbers, or None where the dialects
    /// differ: T-SQL tinyint arithmetic (PostgreSQL has no tinyint), a real
    /// with another number (real in T-SQL, double precision in PostgreSQL),
    /// and a T-SQL decimal sum of known types that needs more than 38 digits
    /// (it overflows where PostgreSQL's numeric does not).
    fn arith_ty(&self, l: Option<Ty>, r: Option<Ty>) -> Option<Option<Ty>> {
        let (Some(x), Some(y)) = (l, r) else {
            return Some(l.or(r));
        };
        if !x.is_numeric() || !y.is_numeric() {
            return Some(l.or(r));
        }
        let small = |t: &Ty| t.fam == Fam::Int && matches!(t.len, Some(1..=8));
        if self.d == Dialect::Tsql && small(&x) && small(&y) {
            return None;
        }
        let real = |t: &Ty| t.fam == Fam::Float && t.len == Some(24);
        if real(&x) != real(&y) {
            return None;
        }
        if x.fam == Fam::Int && y.fam == Fam::Int {
            let len = x.len.zip(y.len).map(|(a, b)| a.max(b));
            return Some(Some(Ty { len, ..x }));
        }
        if x.fam == Fam::Float || y.fam == Fam::Float {
            let len = if real(&x) { Some(24) } else { Some(53) };
            return Some(Some(Ty {
                fam: Fam::Float,
                len,
                scale: None,
            }));
        }
        // Decimal digits before and after the point.
        let digits = |t: &Ty| match t.fam {
            Fam::Int => t.len.map(|b| {
                let p = match b {
                    0..=1 => 1,
                    2..=8 => 3,
                    9..=16 => 5,
                    17..=32 => 10,
                    _ => 19,
                };
                (p, 0)
            }),
            _ => t.len.zip(t.scale).map(|(p, s)| (p.saturating_sub(s), s)),
        };
        match (digits(&x), digits(&y)) {
            (Some((i1, s1)), Some((i2, s2))) => {
                let (i, s) = (i1.max(i2) + 1, s1.max(s2));
                if self.d == Dialect::Tsql && i + s > 38 {
                    return None;
                }
                Some(Some(Ty {
                    fam: Fam::Exact,
                    len: Some(i + s),
                    scale: Some(s),
                }))
            }
            _ => Some(Some(Ty::of(Fam::Exact))),
        }
    }

    /// What a string literal converts to when it meets this operand: the
    /// declared type of a column, or what the operand's type says alone.
    fn target_of(&self, t: &Term, ty: Option<Ty>) -> Option<Target> {
        if t.is("col") {
            let (table, col) = (t.kid(0)?.atom()?, t.kid(1)?.atom()?);
            return self
                .schema
                .table(Self::base_table(table))?
                .column(col)?
                .target;
        }
        match ty?.fam {
            Fam::Date => Target::of("DATE", &[], self.d),
            Fam::Bool => Target::of("BOOLEAN", &[], self.d),
            _ => None,
        }
    }

    /// The value a string literal converts to, or, when the conversion is not
    /// modelled for certain, a term that only equals the same literal in the
    /// same dialect. uniqueidentifier and uuid sort differently, so their
    /// order comparisons stay dialect-specific.
    fn lit_as(&mut self, s: &str, target: Option<Target>, ordered: bool) -> Term {
        let conv = target
            .filter(|t| !(ordered && *t == Target::Uuid))
            .and_then(|t| literal::convert(s, t, self.d, false));
        match conv {
            Some((t, asm)) => {
                if let Some(x) = asm {
                    self.assume(x);
                }
                t
            }
            None => n(
                "lit-conv",
                vec![self.dtag(), a(target.map_or("?", |t| t.name())), a(s)],
            ),
        }
    }

    /// A string literal against a value of unknown type (no catalog facts):
    /// a string comparison under each side's collation if the value is a
    /// string, a conversion of the literal otherwise.
    fn untyped_lit_cmp(&mut self, op: &str, l: Term, r: Term) -> Term {
        let (x, y, op) = order_operands(op, l, r);
        n("cmp-untyped", vec![self.dtag(), a(op), x, y])
    }

    /// String comparison with its collation behaviour made explicit:
    /// case sensitivity (`ci` / `cs`) and trailing blanks (`pad` / `nopad`).
    fn strcmp(&mut self, op: &str, l: (Term, Option<Ty>), r: (Term, Option<Ty>)) -> Term {
        let folded = |t: &Term| {
            t.is("fn") && matches!(t.kid(0).and_then(Term::atom), Some("upper" | "lower"))
        };
        let (mut lt, mut rt) = (l.0, r.0);
        let both_folded = folded(&lt) && folded(&rt);
        let fold = if both_folded {
            let f = lt.kid(0).and_then(Term::atom).unwrap_or("").to_string();
            let g = rt.kid(0).and_then(Term::atom).unwrap_or("").to_string();
            lt = lt.kids()[1].clone();
            rt = rt.kids()[1].clone();
            if f != g {
                return n("cmp-fold-mixed", vec![self.dtag(), a(op), lt, rt]);
            }
            true
        } else {
            false
        };
        let ordering = op != "=" && op != "<>";
        if ordering {
            // Ordering comparisons depend on the collation, not only on case and padding.
            return n("strcmp-order", vec![self.dtag(), a(op), lt, rt]);
        }
        let char_l = l.1.is_some_and(|t| t.fam == Fam::Char);
        let char_r = r.1.is_some_and(|t| t.fam == Fam::Char);
        let ci = if fold {
            if self.d != Dialect::Tsql {
                self.assume(asm::CASE_FOLD);
            }
            "ci"
        } else {
            match self.d {
                Dialect::Tsql => {
                    self.assume(asm::TSQL_CI);
                    "ci"
                }
                Dialect::Postgres => "cs",
                Dialect::Db2i => {
                    self.assume(asm::DB2_SRTSEQ);
                    "cs"
                }
            }
        };
        let pad = match self.d {
            Dialect::Tsql | Dialect::Db2i => "pad",
            Dialect::Postgres if char_l && char_r => "pad",
            Dialect::Postgres if char_l || char_r => "pad-mixed",
            Dialect::Postgres => "nopad",
        };
        let (x, y, op) = order_operands(op, lt, rt);
        n("strcmp", vec![a(op), a(ci), a(pad), x, y])
    }

    fn order_terms(&mut self, items: &[OrderItem], total_from: Option<&str>) -> R<Vec<Term>> {
        let _ = total_from;
        let mut v = Vec::new();
        for o in items {
            let (t, ty) = self.expr(&o.expr)?;
            let nulls_first = o.nulls_first.unwrap_or(match self.d {
                // T-SQL: NULL is the lowest value. PostgreSQL and Db2 for i: the highest
                // (fact V19, Db2 for i part not checked on a real system).
                Dialect::Tsql => !o.desc,
                Dialect::Postgres | Dialect::Db2i => o.desc,
            });
            let nulls = match self.not_null(&t) {
                Some(true) => "n/a".to_string(),
                Some(false) => if nulls_first { "nf" } else { "nl" }.to_string(),
                None => if nulls_first { "nf?" } else { "nl?" }.to_string(),
            };
            let mut kids = vec![t, a(if o.desc { "desc" } else { "asc" }), a(nulls)];
            match ty {
                // Character ordering depends on the collation of each dialect.
                Some(t) if t.is_string() => kids.push(self.dtag()),
                None => self.assume(asm::UNTYPED),
                _ => {}
            }
            v.push(n("ord", kids));
        }
        Ok(v)
    }

    // ---- queries --------------------------------------------------------

    fn push_from(&mut self, from: &[From]) -> R<Vec<Term>> {
        let mut terms = Vec::new();
        for f in from {
            terms.push(self.table_ref(f)?);
        }
        Ok(terms)
    }

    fn table_ref(&mut self, f: &From) -> R<Term> {
        match f {
            From::Table { name, alias, hints } => {
                if !hints.is_empty() {
                    return Err("table hints in FROM are not supported".into());
                }
                // The image a T-SQL trigger's event has no rows for.
                if let Some(tr) = &self.unit.trigger {
                    if let ([x], Some(m)) = (name.as_slice(), tr.missing) {
                        if x.trim_matches(['[', ']', '"']).eq_ignore_ascii_case(m) {
                            let table = tr.table.clone();
                            let alias = Some(alias.clone().unwrap_or_else(|| m.to_string()));
                            return Ok(self.empty_image(&table, &alias));
                        }
                    }
                }
                let b = self.bind_table(name, alias);
                let t = n("t", vec![a(b.table.clone())]);
                self.scopes.last_mut().unwrap().push(b);
                Ok(t)
            }
            From::Join {
                kind,
                left,
                right,
                on,
            } => {
                let l = self.table_ref(left)?;
                let r = self.table_ref(right)?;
                let on = match on {
                    Some(e) => self.cond(e)?.0,
                    None => a("-"),
                };
                Ok(n("join", vec![a(kind.to_lowercase()), l, r, on]))
            }
            From::Sub { query, alias, cols } => {
                if !cols.is_empty() {
                    return Err("derived column lists are not supported".into());
                }
                // `(SELECT * FROM t WHERE false)`: the conversion of an image
                // the trigger's event has no rows for.
                if let (Some(_), Some(table)) = (&self.unit.trigger, empty_query(query)) {
                    return Ok(self.empty_image(&table, alias));
                }
                let saved = std::mem::take(&mut self.scopes);
                let q = self.query_parts(query, QCtx::Set);
                self.scopes = saved;
                let (mut parts, items) = q?;
                let key = alias.clone().map(|x| lc(&x)).unwrap_or_default();
                // The bit and boolean columns, by the name the item gives them.
                if query.set_ops.is_empty() && query.items.len() == items.len() {
                    let bits: Vec<(String, Ty)> = query
                        .items
                        .iter()
                        .zip(&items)
                        .filter_map(|(it, (_, ty))| {
                            let ty = ty.filter(|_| self.is_bit(ty) || self.is_bool(ty))?;
                            let name = match (&it.alias, &it.expr) {
                                (Some(a), _) => a.clone(),
                                (None, Expr::Name(p)) => p.last()?.clone(),
                                _ => return None,
                            };
                            Some((lc(name.trim_matches(['[', ']', '"'])), ty))
                        })
                        .collect();
                    if !bits.is_empty() {
                        self.sub_bits.insert(format!("sub:{key}"), bits);
                    }
                }
                let unique = sqldiff_syntax::abstract_syntax::group_key(query)
                    .map(|k| k.iter().map(|c| lc(c)).collect());
                let item_terms: Vec<Term> = items.into_iter().map(|x| x.0).collect();
                parts.insert(1, n("items", item_terms));
                let q = (n("q", parts), None::<Ty>);
                self.scopes.last_mut().unwrap().push(Binding {
                    key: key.clone(),
                    table: format!("sub:{key}"),
                    derived: None,
                    unique,
                });
                Ok(n("sub", vec![q.0]))
            }
            From::Values { .. } | From::DataChange { .. } | From::Func { .. } => Err(
                "VALUES, table function or data-change table in FROM is not supported here".into(),
            ),
        }
    }

    /// A relation with the columns of `table` and no rows, under `alias`.
    fn empty_image(&mut self, table: &[String], alias: &Option<String>) -> Term {
        let b = self.bind_table(table, alias);
        let t = n("empty", vec![a(b.table.clone())]);
        self.scopes.last_mut().unwrap().push(b);
        t
    }

    /// Lowers a query; returns the term and the type of its first column.
    pub fn query(&mut self, s: &Select, ctx: QCtx) -> R<(Term, Option<Ty>)> {
        let (mut parts, items) = self.query_parts(s, ctx)?;
        let ty = items.first().and_then(|x| x.1);
        let item_terms: Vec<Term> = items.into_iter().map(|x| x.0).collect();
        parts.insert(1, n("items", item_terms));
        Ok((n("q", parts), ty))
    }

    /// Query parts without the select list, plus the select items.
    fn query_parts(&mut self, s: &Select, ctx: QCtx) -> R<Parts> {
        if !s.set_ops.is_empty() {
            return self.set_query(s, ctx);
        }
        if let Some(rows) = &s.values {
            let mut rs = Vec::new();
            for r in rows {
                let mut v = Vec::new();
                for x in r {
                    v.push(self.expr(x)?.0);
                }
                rs.push(n("row", v));
            }
            return Ok((vec![n("values", rs)], Vec::new()));
        }
        if !s.into.is_empty() && ctx != QCtx::Into {
            return Err("SELECT INTO here is not supported".into());
        }
        self.scopes.push(Vec::new());
        let r = self.query_inner(s, ctx);
        self.scopes.pop();
        r
    }

    /// `q1 UNION [ALL] q2 …`, EXCEPT, INTERSECT, with an ORDER BY of the
    /// whole: `(setq (setop OP left right [coll]) (order …))`, the items
    /// being the result columns `(ocol k)`. Left to right, as both T-SQL
    /// and PostgreSQL group UNION and EXCEPT (the parser refuses INTERSECT
    /// mixed with the others, which binds tighter in both). Only where both
    /// give the same rows:
    /// - each column has one known type in every branch, so neither
    ///   dialect converts a branch's values to a common type (their rules
    ///   for that differ);
    /// - UNION, EXCEPT and INTERSECT without ALL remove duplicates by `=`,
    ///   so string columns compare under each dialect's collation (kept as
    ///   the dialect, as for DISTINCT);
    /// - no branch has its own ORDER BY, TOP or paging, assignments or
    ///   INTO, and the whole has no TOP, paging or locking (where a T-SQL
    ///   TOP applies to the first branch and a PostgreSQL LIMIT to the
    ///   whole);
    /// - EXCEPT ALL and INTERSECT ALL, which T-SQL lacks, are refused;
    /// - the ORDER BY names a result column (by its name in the first
    ///   branch, or its position).
    fn set_query(&mut self, s: &Select, ctx: QCtx) -> R<Parts> {
        if s.top.is_some()
            || s.limit.is_some()
            || s.offset.is_some()
            || !s.locking.is_empty()
            || s.for_json
            || !s.ctes.is_empty()
            || !s.into.is_empty()
        {
            return Err("a set operation with TOP, paging, locking or INTO".into());
        }
        let mut head = s.clone();
        head.set_ops.clear();
        head.order_by.clear();
        let mut branches: Vec<(Option<&str>, &Select)> = vec![(None, &head)];
        for (op, b) in &s.set_ops {
            branches.push((Some(op.as_str()), b));
        }
        let mut acc: Option<Term> = None;
        let mut col_tys: Vec<Option<Ty>> = Vec::new();
        let mut col_terms: Vec<Vec<Term>> = Vec::new();
        for (op, b) in branches {
            if b.top.is_some()
                || b.limit.is_some()
                || b.offset.is_some()
                || !b.order_by.is_empty()
                || !b.locking.is_empty()
                || b.for_json
                || !b.ctes.is_empty()
                || !b.into.is_empty()
                || !b.set_ops.is_empty()
                || b.items.iter().any(|i| i.assign.is_some())
            {
                return Err(
                    "a set operation branch with its own ORDER BY, TOP, paging, INTO or assignment"
                        .into(),
                );
            }
            let (mut parts, items) = self.query_parts(b, QCtx::Scalar)?;
            if col_tys.is_empty() && acc.is_none() {
                col_tys = items.iter().map(|x| x.1).collect();
                col_terms = vec![Vec::new(); items.len()];
            } else if items.len() != col_tys.len() {
                return Err("set operation branches with different numbers of columns".into());
            }
            for (k, (t, ty)) in items.iter().enumerate() {
                if ty.is_none() || *ty != col_tys[k] {
                    return Err(format!(
                        "set operation column {} has different or unknown types in its branches",
                        k + 1
                    ));
                }
                col_terms[k].push(t.clone());
            }
            parts.insert(1, n("items", items.into_iter().map(|x| x.0).collect()));
            let q = n("q", parts);
            acc = Some(match (acc, op) {
                (None, _) => q,
                (Some(l), Some(op)) => {
                    let h = match op {
                        "UNION ALL" => "union-all",
                        "UNION" => "union",
                        "EXCEPT" => "except",
                        "INTERSECT" => "intersect",
                        _ => return Err(format!("{op} is not supported")),
                    };
                    let mut kids = vec![a(h), l, q];
                    if h != "union-all" && col_tys.iter().any(|t| t.is_some_and(|t| t.is_string()))
                    {
                        kids.push(n("coll", vec![self.dtag()]));
                    }
                    n("setop", kids)
                }
                (Some(_), None) => unreachable!("only the first branch has no operator"),
            });
        }
        let acc = acc.ok_or("an empty set operation")?;
        // Result columns, named as the first branch names them.
        let names: Vec<Option<String>> = head
            .items
            .iter()
            .map(|it| {
                it.alias.clone().map(|x| lc(&x)).or_else(|| match &it.expr {
                    Expr::Name(p) => p.last().map(|x| lc(x)),
                    _ => None,
                })
            })
            .collect();
        let mut order = Vec::new();
        for o in &s.order_by {
            let k = match &o.expr {
                Expr::Num(v) => v
                    .parse::<usize>()
                    .ok()
                    .filter(|k| (1..=col_tys.len()).contains(k))
                    .map(|k| k - 1),
                Expr::Name(p) if p.len() == 1 => {
                    let want = lc(&p[0]);
                    let hits: Vec<usize> = (0..names.len())
                        .filter(|&k| names[k].as_deref() == Some(want.as_str()))
                        .collect();
                    match hits.as_slice() {
                        [k] => Some(*k),
                        _ => None,
                    }
                }
                _ => None,
            }
            .ok_or("an ORDER BY of a set operation that is not one result column")?;
            let nulls_first = o.nulls_first.unwrap_or(match self.d {
                Dialect::Tsql => !o.desc,
                Dialect::Postgres | Dialect::Db2i => o.desc,
            });
            let not_null = col_terms[k].iter().all(|t| self.not_null(t) == Some(true));
            let nulls = match (not_null, nulls_first) {
                (true, _) => "n/a",
                (false, true) => "nf?",
                (false, false) => "nl?",
            };
            let mut kids = vec![
                n("ocol", vec![a(k.to_string())]),
                a(if o.desc { "desc" } else { "asc" }),
                a(nulls),
            ];
            if col_tys[k].is_some_and(|t| t.is_string()) {
                kids.push(self.dtag());
            }
            order.push(n("ord", kids));
        }
        let items = col_tys
            .iter()
            .enumerate()
            .map(|(k, ty)| {
                let t = n("ocol", vec![a(k.to_string())]);
                let t = match ctx {
                    QCtx::Set => n("item", vec![t, a(names[k].clone().unwrap_or_default())]),
                    _ => t,
                };
                (t, *ty)
            })
            .collect();
        let items = if ctx == QCtx::Exists {
            Vec::new()
        } else {
            items
        };
        Ok((vec![a("setq"), acc, n("order", order)], items))
    }

    fn query_inner(&mut self, s: &Select, ctx: QCtx) -> R<Parts> {
        let from = self.push_from(&s.from)?;
        let mut items = Vec::new();
        for it in &s.items {
            if it.assign.is_some() && ctx != QCtx::Into {
                return Err("assignment in a SELECT list here is not supported".into());
            }
            let (t, ty) = self.expr(&it.expr)?;
            let name = it
                .alias
                .clone()
                .map(|x| lc(&x))
                .or_else(|| match &it.expr {
                    Expr::Name(p) => p.last().map(|x| lc(x)),
                    _ => None,
                })
                .unwrap_or_default();
            let t = match ctx {
                QCtx::Set => n("item", vec![t, a(name)]),
                _ => t,
            };
            items.push((t, ty));
        }
        if ctx == QCtx::Exists {
            items.clear();
        }
        let where_ = match &s.where_ {
            Some(w) => self.cond(w)?.0,
            None => a("-"),
        };
        let mut group = Vec::new();
        for g in &s.group_by {
            let (t, ty) = self.expr(g)?;
            group.push(match ty {
                Some(t2) if t2.is_string() => n("collate", vec![self.dtag(), t]),
                None => {
                    self.assume(asm::UNTYPED);
                    t
                }
                _ => t,
            });
        }
        // DISTINCT compares strings under each dialect's collation.
        let distinct = if !s.distinct {
            a("all")
        } else if items.iter().any(|x| x.1.is_some_and(|t| t.is_string())) {
            n("distinct", vec![self.dtag()])
        } else {
            if items.iter().any(|x| x.1.is_none()) {
                self.assume(asm::UNTYPED);
            }
            a("distinct")
        };
        let having = match &s.having {
            Some(h) => self.cond(h)?.0,
            None => a("-"),
        };
        let order = self.order_terms(&s.order_by, None)?;
        let limit = match (&s.top, &s.limit) {
            (Some(t), _) | (None, Some(t)) => Some(self.row_count(t)?),
            _ => None,
        };
        let offset = match &s.offset {
            Some(o) => Some(self.row_count(o)?),
            None => None,
        };
        // EXISTS only asks whether a row qualifies: its ORDER BY and a limit
        // of one row or more change nothing (`EXISTS (SELECT TOP 1 …)`).
        let any_row = |l: &Term| {
            l.is("num")
                && l.kid(0)
                    .and_then(Term::atom)
                    .and_then(|x| x.parse::<u64>().ok())
                    .is_some_and(|x| x >= 1)
        };
        let (order, limit) =
            if ctx == QCtx::Exists && offset.is_none() && limit.as_ref().map_or(true, any_row) {
                (Vec::new(), None)
            } else {
                (order, limit)
            };
        let page = if limit.is_some() || offset.is_some() {
            if order.is_empty() {
                self.assume(asm::PAGE_UNORDERED);
            } else if !self.total_order(&from, &order) {
                self.assume(asm::ORDER_TOTAL);
            }
            n(
                "page",
                vec![
                    offset.unwrap_or(n("num", vec![a("0")])),
                    limit.unwrap_or(a("all")),
                ],
            )
        } else {
            a("-")
        };
        let lock = if s.locking.is_empty() {
            a("-")
        } else {
            n(
                "lock",
                std::iter::once(self.dtag())
                    .chain(s.locking.iter().map(a))
                    .collect(),
            )
        };
        Ok((
            vec![
                distinct,
                n("from", from),
                where_,
                n("group", group),
                having,
                n("order", order),
                page,
                lock,
            ],
            items,
        ))
    }

    /// The ORDER BY keys include a unique key of the only table in FROM.
    /// A row count or offset. A literal is itself; anything else may be
    /// NULL, which each dialect reads its own way (T-SQL raises an error,
    /// PostgreSQL drops the limit), and a fractional count is an error in
    /// T-SQL but rounded in PostgreSQL: `(count DIALECT int|? term)`, which
    /// `compare` matches across dialects for integers only, under
    /// `ASM-PAGE-COUNT-NOT-NULL`.
    fn row_count(&mut self, e: &Expr) -> R<Term> {
        let (t, ty) = self.expr(e)?;
        if t.is("num") {
            return Ok(t);
        }
        let int = ty.is_some_and(|ty| ty.fam == Fam::Int);
        Ok(n(
            "count",
            vec![self.dtag(), a(if int { "int" } else { "?" }), t],
        ))
    }

    fn total_order(&mut self, from: &[Term], order: &[Term]) -> bool {
        let [f] = from else { return false };
        if !f.is("t") {
            return false;
        }
        let table = f.kid(0).and_then(Term::atom).unwrap_or("").to_string();
        let cols: Vec<String> = order
            .iter()
            .filter_map(|o| o.kid(0))
            .filter(|c| c.is("col") && c.kid(0).and_then(Term::atom) == Some(table.as_str()))
            .filter_map(|c| c.kid(1).and_then(Term::atom).map(str::to_string))
            .collect();
        match self.schema.table(Self::base_table(&table)) {
            Some(t) if t.is_unique(&cols) => {
                self.discharge(format!(
                    "ORDER BY {} is unique in {table} (catalog)",
                    cols.join(", ")
                ));
                true
            }
            _ => false,
        }
    }

    /// The query returns at most one row: equality on a unique key of the
    /// only table, or an aggregate without GROUP BY.
    fn at_most_one(&mut self, q: &Term) -> bool {
        let Some(Term::N(_, from)) = q.kid(2) else {
            return false;
        };
        let items = q.kid(1).map(Term::kids).unwrap_or(&[]);
        let group = q.kid(4).map(Term::kids).unwrap_or(&[]);
        if group.is_empty()
            && !items.is_empty()
            && items.iter().all(|i| {
                i.is("count")
                    || i.is("stringagg")
                    || (i.is("fn") && matches!(i.kid(0).and_then(Term::atom), Some("min" | "max")))
            })
        {
            return true;
        }
        let [f] = from.as_slice() else { return false };
        if !f.is("t") {
            return false;
        }
        let table = f.kid(0).and_then(Term::atom).unwrap_or("").to_string();
        let w = q.kid(3).cloned().unwrap_or(a("-"));
        let conj: Vec<Term> = if w.is("and") {
            w.kids().to_vec()
        } else {
            vec![w]
        };
        let mut cols = Vec::new();
        for c in &conj {
            let (x, y) = match c.head() {
                "cmp" if c.kid(0).and_then(Term::atom) == Some("=") => (c.kid(1), c.kid(2)),
                // Only the dialect's own comparison matches its unique indexes: a
                // folded (upper/lower) comparison can match several keys.
                "strcmp"
                    if c.kid(0).and_then(Term::atom) == Some("=")
                        && c.kid(1).and_then(Term::atom)
                            == Some(if self.d == Dialect::Tsql { "ci" } else { "cs" }) =>
                {
                    (c.kid(3), c.kid(4))
                }
                _ => continue,
            };
            let (Some(x), Some(y)) = (x, y) else { continue };
            let refs_table = |t: &Term| {
                t.any(&|s| s.is("col") && s.kid(0).and_then(Term::atom) == Some(table.as_str()))
            };
            for (col, other) in [(x, y), (y, x)] {
                if col.is("col")
                    && col.kid(0).and_then(Term::atom) == Some(table.as_str())
                    && !refs_table(other)
                {
                    if let Some(c) = col.kid(1).and_then(Term::atom) {
                        cols.push(c.to_string());
                    }
                }
            }
        }
        match self.schema.table(Self::base_table(&table)) {
            Some(t) if t.is_unique(&cols) => {
                let key = t
                    .keys
                    .iter()
                    .find(|k| k.iter().all(|c| cols.contains(c)))
                    .cloned()
                    .unwrap_or_default();
                self.discharge(format!(
                    "{table}.{} is a unique key (catalog)",
                    key.join(", ")
                ));
                true
            }
            _ => false,
        }
    }

    /// `scalar(query, on-several-rows, on-zero-rows)`.
    fn scalar(&mut self, q: Term, many: &str, zero: &str) -> Term {
        if exactly_one(&q) {
            return n("scalar", vec![q, a("n/a"), a("n/a")]);
        }
        let many = if self.at_most_one(&q) {
            "n/a".to_string()
        } else if self.uniqueness_known(&q) {
            many.to_string()
        } else {
            // Without catalog facts for the table, "several rows" may be impossible.
            format!("{many}?")
        };
        n("scalar", vec![q, a(many), a(zero)])
    }

    /// The query reads one table that the catalog describes.
    fn uniqueness_known(&self, q: &Term) -> bool {
        let Some(Term::N(_, from)) = q.kid(2) else {
            return false;
        };
        match from.as_slice() {
            [f] if f.is("t") => f
                .kid(0)
                .and_then(Term::atom)
                .is_some_and(|t| self.schema.table(Self::base_table(t)).is_some()),
            _ => false,
        }
    }

    // ---- statements -----------------------------------------------------

    fn target_sym(&mut self, e: &Expr) -> R<(Term, Option<Ty>)> {
        let s = match e {
            Expr::Var(v) => self.sym(v),
            Expr::Name(p) if p.len() == 1 => self.sym(&p[0]),
            _ => None,
        };
        s.ok_or_else(|| "assignment target is not a parameter or local variable".to_string())
    }

    /// The condition of an IF or WHILE, evaluated once each time it is
    /// tested.
    pub fn condition(&mut self, e: &Expr) -> R<Term> {
        self.seen_tables.clear();
        self.single_row = true;
        Ok(self.cond(e)?.0)
    }

    /// Lowers a statement. A PostgreSQL statement with quoted names that
    /// keep capitals is lowered twice: once as written, recording its
    /// names, and, when `fold_check` allows, again with those names folded.
    pub fn stmt(&mut self, s: &Stmt) -> R<Term> {
        let nested = FOLDING.with(|f| !matches!(*f.borrow(), Folding::Off));
        if self.d != Dialect::Postgres || !self.fold_quoted || nested {
            return self.stmt_unfolded(s);
        }
        let saved = self.clone();
        let guard = FoldingGuard::set(Folding::Record(BTreeMap::new()));
        let t = self.stmt_unfolded(s);
        let Folding::Record(seen) = guard.take() else {
            return t;
        };
        let t = t?;
        // Lower-case names fold to themselves, but the catalog may still
        // hold another spelling the T-SQL name could mean.
        fold_check(&seen, self.schema)?;
        if seen.values().flatten().all(|x| *x == x.to_lowercase()) {
            return Ok(t);
        }
        *self = saved;
        let _guard = FoldingGuard::set(Folding::Fold);
        self.stmt_unfolded(s)
    }

    /// Lowers a statement that returns the rows it changed (T-SQL OUTPUT,
    /// PostgreSQL RETURNING; see `ast::Output`).
    pub fn output_stmt(&mut self, s: &Stmt, o: &Output) -> R<Term> {
        self.output = Some(o.clone());
        let t = self.stmt(s);
        self.output = None;
        t
    }

    /// `(output-into DML TABLE (set (kv COL VALUE)...))`: the data change
    /// `DML` and the rows it writes to `TABLE`, one per changed row, with
    /// VALUE read from the row before (`old`) or after (`new`) the change.
    /// T-SQL names the images `deleted` and `inserted`; PostgreSQL 18 names
    /// them `old` and `new`, and a bare or table-qualified column means the
    /// row after an INSERT or UPDATE and the row before a DELETE (checked
    /// on PostgreSQL 18). Only images, variables and constants are taken:
    /// no FROM list, subqueries or `*`. Rows returned to the caller (no
    /// INTO) are not compared.
    fn output_unfolded(&mut self, s: &Stmt, o: &Output) -> R<Term> {
        self.output = None;
        let (kind, target, alias) = match s {
            Stmt::Insert {
                target,
                returning,
                on_conflict: None,
                ..
            } if returning.is_empty() => ("insert", target, None),
            Stmt::Update {
                target,
                alias,
                from,
                ..
            } if from.is_empty() => ("update", target, alias.as_ref()),
            Stmt::Delete {
                target,
                alias,
                from,
                ..
            } if from.is_empty() => ("delete", target, alias.as_ref()),
            _ => return Err("OUTPUT or RETURNING of this statement is not compared".into()),
        };
        let Some((into, cols)) = &o.into else {
            return Err(
                "rows returned to the caller by OUTPUT or RETURNING are not compared".into(),
            );
        };
        if cols.is_empty() {
            return Err("OUTPUT … INTO without a column list".into());
        }
        if cols.len() != o.items.len() {
            return Err("OUTPUT … INTO column count does not match its items".into());
        }
        let dml = self.stmt_unfolded(s)?;
        let table = self.table_key(target);
        let Some(t) = self.schema.table(Self::base_table(&table)).cloned() else {
            return Err(format!("the columns of {table} are not known"));
        };
        let cur = if kind == "delete" {
            OLD_IMAGE
        } else {
            NEW_IMAGE
        };
        // What qualifies the changed row in PostgreSQL: the alias, else the
        // table's name.
        let qual = alias.or(target.last()).map(|x| lc(x)).unwrap_or_default();
        let image = |x: &str| -> R<&'static str> {
            match (self.d, x) {
                (Dialect::Tsql, "inserted") if kind != "delete" => Ok(NEW_IMAGE),
                (Dialect::Tsql, "deleted") if kind != "insert" => Ok(OLD_IMAGE),
                (Dialect::Postgres, "new") if kind != "delete" => Ok(NEW_IMAGE),
                (Dialect::Postgres, "old") if kind != "insert" => Ok(OLD_IMAGE),
                (Dialect::Postgres, q) if q == qual => Ok(cur),
                _ => Err(format!(
                    "OUTPUT or RETURNING names `{x}`, which is not a row image of this statement"
                )),
            }
        };
        let mut items = Vec::new();
        for it in &o.items {
            let var = |x: &str| self.sym(x).is_some();
            items.push(output_item(&it.expr, self.d, &t, cur, &image, &var)?);
        }
        let bind = |img: &str| Binding {
            key: img.to_string(),
            table: table.clone(),
            derived: Some(
                t.columns
                    .iter()
                    .map(|c| {
                        let side = if img == NEW_IMAGE { "new" } else { "old" };
                        (
                            c.name.clone(),
                            n("image", vec![a(side), a(table.clone()), a(c.name.clone())]),
                            Some(c.ty),
                        )
                    })
                    .collect(),
            ),
            unique: None,
        };
        self.scopes.push(vec![bind(NEW_IMAGE), bind(OLD_IMAGE)]);
        let vals: R<Vec<(Term, Option<Ty>)>> = items.iter().map(|e| self.expr(e)).collect();
        self.scopes.pop();
        let vals = vals?;
        if vals.iter().any(|(v, _)| has_head(v, "col")) {
            return Err("OUTPUT or RETURNING reads a column outside the changed row".into());
        }
        let out = self.table_key(into);
        let cols: Vec<String> = cols.iter().map(|c| lc(c)).collect();
        let mut kv = Vec::new();
        for (c, v) in cols.iter().zip(vals) {
            self.written(&out, c)?;
            let (v, _) = self.conv_col(&out, c, v);
            kv.push(n("kv", vec![a(c.clone()), v]));
        }
        kv.sort();
        if self.d == Dialect::Tsql {
            self.assume(asm::OUTPUT_INTO_TARGET);
        }
        let all_listed = self
            .schema
            .table(Self::base_table(&out))
            .is_some_and(|t| t.columns.iter().all(|c| cols.contains(&c.name)));
        if !all_listed {
            self.assume(asm::OUTPUT_ROW_ORDER);
        }
        Ok(n("output-into", vec![dml, a(out), n("set", kv)]))
    }

    fn stmt_unfolded(&mut self, s: &Stmt) -> R<Term> {
        if let Some(o) = self.output.clone() {
            return self.output_unfolded(s, &o);
        }
        self.seen_tables.clear();
        self.single_row = false;
        match s {
            Stmt::Select(sel) => self.select_stmt(sel),
            Stmt::Insert {
                target,
                cols,
                source,
                returning,
                returning_into,
                on_conflict,
            } => {
                let table = self.table_key(target);
                let cols: Vec<String> = if cols.is_empty() {
                    self.all_columns(&table, source)?
                } else {
                    cols.iter().map(|c| lc(c)).collect()
                };
                if let Some((keys, action)) = on_conflict {
                    return self.on_conflict(&table, &cols, source, keys, action, returning);
                }
                let ins = self.insert(&table, &cols, source)?;
                if returning.is_empty() {
                    return Ok(ins);
                }
                self.returning(ins, &table, returning, returning_into)
            }
            Stmt::Update {
                target,
                alias,
                sets,
                from,
                where_,
            } => {
                if !from.is_empty() {
                    return self.update_from(target, alias, sets, from, where_);
                }
                self.scopes.push(Vec::new());
                let b = self.bind_table(target, alias);
                let table = b.table.clone();
                self.scopes.last_mut().unwrap().push(b);
                let r = (|| {
                    let kv = self.update_sets(&table, sets)?;
                    let w = match where_ {
                        Some(w) => self.cond(w)?.0,
                        None => a("-"),
                    };
                    Ok(n("update", vec![a(table.clone()), n("set", kv), w]))
                })();
                self.scopes.pop();
                r
            }
            Stmt::Delete {
                target,
                alias,
                from,
                where_,
            } => self.delete(target, alias, from, where_),
            Stmt::Merge {
                target,
                alias,
                hints,
                source,
                on,
                whens,
            } => self.merge(target, alias, hints, source, on, whens),
            Stmt::Assign(t, v) => {
                let (sym, ty) = self.target_sym(t)?;
                self.single_row = true;
                // A string literal into a variable whose type's conversion is
                // modelled: the value it converts to, as a CAST gives it (an
                // assignment rounds like one on both engines).
                let decl = self
                    .unit
                    .locals
                    .iter()
                    .find(|l| super::script::names(self.d, t, &l.name))
                    .and_then(|l| l.decl.as_ref());
                if let (Expr::Str(s), Some(target)) =
                    (v, decl.and_then(|d| Target::of_type(d, self.d)))
                {
                    if let Some((term, asm)) = literal::convert(s, target, self.d, true) {
                        if let Some(x) = asm {
                            self.assume(x);
                        }
                        return Ok(n("assign", vec![sym, term]));
                    }
                }
                let val = match v {
                    Expr::Subquery(q) => {
                        let (q, qty) = self.query(q, QCtx::Scalar)?;
                        (self.scalar(q, "error", "null"), qty)
                    }
                    e => self.expr(e)?,
                };
                if val.0.is("scope_identity") {
                    return Ok(n("assign", vec![sym, val.0]));
                }
                let val = self.conv_var(&ty, val).0;
                Ok(n("assign", vec![sym, val]))
            }
            Stmt::Return(None) => Ok(n("return", vec![])),
            Stmt::Return(Some(e)) => {
                self.single_row = true;
                match (self.unit.kind, e) {
                    (UnitKind::TableFn, Expr::Subquery(q)) => self.resultset(q),
                    (_, Expr::Subquery(q)) => {
                        let (q, qty) = self.query(q, QCtx::Scalar)?;
                        let v = (self.scalar(q, "error", "null"), qty);
                        let ret = self.returned();
                        Ok(n("return", vec![self.conv_var(&ret, v).0]))
                    }
                    _ => {
                        let v = self.expr(e)?;
                        // A T-SQL procedure returns an int status.
                        let ret = match (self.unit.dialect, self.unit.kind) {
                            (Dialect::Tsql, UnitKind::Proc) => Some(Ty {
                                len: Some(32),
                                ..Ty::of(Fam::Int)
                            }),
                            _ => self.returned(),
                        };
                        Ok(n("return", vec![self.conv_var(&ret, v).0]))
                    }
                }
            }
            Stmt::Declare { .. } => Err("declaration".into()),
            // A constant string only: PRINT shows NULL as an empty line and a
            // number or date in its own format, RAISE NOTICE as <NULL> and in
            // PostgreSQL's text form.
            // PRINT cuts a message off after 8000 bytes (4000 characters for
            // nvarchar); shorter constants show the same on both sides.
            Stmt::Message(Expr::Str(s)) if s.chars().count() <= 4000 => {
                Ok(n("message", vec![n("str", vec![a(s.clone())])]))
            }
            Stmt::Message(e) => self.print(e),
        }
    }

    /// The type a RETURN converts its value to. PostgreSQL drops the length,
    /// precision and scale of a function's return type: `RETURNS varchar(5)`
    /// returns 'abcdefgh' whole and `RETURNS numeric(10,2)` returns 1.005
    /// unrounded, where SQL Server cuts and rounds (checked on PostgreSQL 18
    /// and SQL Server 2022, eval/user-functions).
    fn returned(&self) -> Option<Ty> {
        let ret = self.unit.returns?;
        if self.d != Dialect::Postgres {
            return Some(ret);
        }
        Some(untypmod(ret))
    }

    /// A message that is an expression: T-SQL `PRINT e`, PL/pgSQL
    /// `RAISE NOTICE '%', e`. Only a variable-length string or an integer,
    /// whose text is the same on both sides: PRINT converts an integer to
    /// its digits, as PostgreSQL shows it. A NULL shows as an empty line in
    /// T-SQL and as `<NULL>` in PostgreSQL, which is part of the term, so a
    /// T-SQL PRINT meets `RAISE NOTICE '%', COALESCE(e, '')`. PRINT cuts a
    /// message off after 8,000 bytes (4,000 characters for nvarchar), so the
    /// T-SQL string must be declared at most 4,000 characters long.
    fn print(&mut self, e: &Expr) -> R<Term> {
        let (t, ty) = self.expr(e)?;
        let Some(ty) = ty else {
            return Err("a message value of unknown type".into());
        };
        let null = match self.d {
            Dialect::Tsql => "",
            Dialect::Postgres => "<NULL>",
            Dialect::Db2i => return Err("a message that is not a string literal".into()),
        };
        let head = match ty.fam {
            Fam::VarChar if self.d != Dialect::Tsql || ty.len.is_some_and(|l| l <= 4000) => {
                "msg-str"
            }
            Fam::VarChar => {
                return Err("a PRINT value that may be longer than 4,000 characters".into())
            }
            Fam::Int => "msg-int",
            _ => {
                return Err(
                    "a message value that is not a variable-length string or an integer".into(),
                )
            }
        };
        Ok(n("message", vec![n(head, vec![t, a(null)])]))
    }

    /// A message or raised error a format builds (`lower::Msg`): the text
    /// as literal pieces and shown values, `message` (informational, as
    /// `PRINT` and `RAISE NOTICE`) or `raise-error`. A value is shown only
    /// when its text is the same on both sides: a variable-length string
    /// (not blank-padded `char`), or an integer of at most 32 bits for
    /// T-SQL `%d` (bigint is an error there). The NULL text is part of the
    /// term, so T-SQL `(null)` meets only a PostgreSQL `COALESCE(x, '(null)')`.
    pub fn message(&mut self, m: &super::lower::Msg) -> R<Term> {
        use super::lower::{MsgPart, Spec};
        let mut parts = Vec::new();
        let mut text = String::new();
        // Most characters the text can have (None: no bound), and whether it
        // can be empty.
        let mut most = Some(0usize);
        let mut empty = true;
        for p in &m.parts {
            match p {
                MsgPart::Text(s) => {
                    most = most.map(|x| x + s.chars().count());
                    empty &= s.is_empty();
                    text.push_str(s);
                }
                MsgPart::Arg { value, null, spec } => {
                    let (t, ty) = self.expr(value)?;
                    let Some(ty) = ty else {
                        return Err("a message value of unknown type".into());
                    };
                    let (head, width) = match (spec, ty.fam) {
                        (Spec::Str, Fam::VarChar) => ("msg-str", ty.len.map(|l| l as usize)),
                        // PostgreSQL `CAST(x AS text)` of a string is its text.
                        (Spec::Int, Fam::VarChar) if !m.tsql => {
                            ("msg-str", ty.len.map(|l| l as usize))
                        }
                        (Spec::Int, Fam::Int)
                            if self.d != Dialect::Tsql || ty.len.is_some_and(|b| b <= 32) =>
                        {
                            // "-2147483648"
                            empty = false;
                            ("msg-int", Some(11))
                        }
                        (Spec::Str, _) => {
                            return Err(
                                "a message value that is not a variable-length string".into()
                            )
                        }
                        (Spec::Int, _) => {
                            return Err("a %d message value that is not an int".into())
                        }
                    };
                    empty &= null.is_empty() || head == "msg-int";
                    most = most
                        .zip(width)
                        .map(|(x, w)| x + w.max(null.chars().count()));
                    if !text.is_empty() {
                        parts.push(n("str", vec![a(std::mem::take(&mut text))]));
                    }
                    parts.push(n(head, vec![t, a(null.clone())]));
                }
            }
        }
        if !text.is_empty() || parts.is_empty() {
            parts.push(n("str", vec![a(text)]));
        }
        if m.tsql && (empty || most.map_or(true, |x| x > 2047)) {
            self.assume(asm::RAISERROR_TEXT);
        }
        if m.format_var {
            self.assume(asm::RAISERROR_FORMAT);
        }
        Ok(n(if m.error { "raise-error" } else { "message" }, parts))
    }

    /// A statement of a cursor loop (`cursor.rs`): `cursor-open(key, q)`
    /// with the query as a list of values per row and its ORDER BY,
    /// `cursor-fetch(key, assign…)` with each target set to its column as an
    /// assignment converts it, `cursor-close(key)`. FETCH reads the column
    /// types of the OPEN before it.
    pub fn cursor_op(&mut self, op: &super::cursor::CursorOp) -> R<Term> {
        use super::cursor::CursorOp;
        match op {
            CursorOp::Open { key, query, tsql } => {
                let read = query
                    .as_ref()
                    .map_err(Clone::clone)
                    .and_then(|q| self.query_parts(q, QCtx::Scalar));
                let (mut parts, items) = match read {
                    Ok(x) => x,
                    Err(e) => {
                        self.cursor_failed.insert(key.clone(), e.clone());
                        return Err(e);
                    }
                };
                let from: Vec<Term> = parts.get(1).map(|f| f.kids().to_vec()).unwrap_or_default();
                let order: Vec<Term> = parts.get(5).map(|o| o.kids().to_vec()).unwrap_or_default();
                if order.is_empty() || !self.total_order(&from, &order) {
                    self.assume(asm::CURSOR_ORDER);
                }
                self.assume(asm::CURSOR_SNAPSHOT);
                if let Some((_, false)) = tsql {
                    self.assume(asm::CURSOR_GLOBAL);
                }
                self.cursor_cols
                    .insert(key.clone(), items.iter().map(|x| x.1).collect());
                parts.insert(1, n("items", items.into_iter().map(|x| x.0).collect()));
                Ok(n("cursor-open", vec![a(key.clone()), n("q", parts)]))
            }
            CursorOp::Fetch { key, targets } => {
                let cols = match self.cursor_cols.get(key) {
                    Some(c) => c.clone(),
                    None => {
                        return Err(match self.cursor_failed.get(key) {
                            Some(why) => format!(
                                "a FETCH of a cursor whose query was not read (in its OPEN: {why})"
                            ),
                            None => "a FETCH whose cursor's OPEN was not read".into(),
                        })
                    }
                };
                if cols.len() != targets.len() {
                    return Err(
                        "a FETCH into another number of variables than the cursor has columns"
                            .into(),
                    );
                }
                let mut kids = vec![a(key.clone())];
                for (i, (t, ty)) in targets.iter().zip(cols).enumerate() {
                    let (sym, vty) = self.target_sym(t)?;
                    let col = n("cursor-col", vec![a(key.clone()), a(i.to_string())]);
                    let v = self.conv_var(&vty, (col, ty)).0;
                    kids.push(n("assign", vec![sym, v]));
                }
                Ok(n("cursor-fetch", kids))
            }
            CursorOp::Close { key } => Ok(n("cursor-close", vec![a(key.clone())])),
        }
    }

    fn resultset(&mut self, q: &Select) -> R<Term> {
        let (mut parts, items) = self.query_parts(q, QCtx::Set)?;
        let decl = &self.unit.ret_cols;
        let mut item_terms = Vec::new();
        if !decl.is_empty() {
            if decl.len() != items.len() {
                return Err("RETURNS TABLE columns do not match the select list".into());
            }
            for (i, (t, ty)) in items.into_iter().enumerate() {
                let expr = t.kids()[0].clone();
                let (name, dty) = decl[i].clone();
                let expr = match (dty, ty) {
                    (Some(_), Some(_)) => self.conv(&dty, (expr, ty)).0,
                    _ => {
                        self.assume(asm::RESULT_TYPES);
                        expr
                    }
                };
                item_terms.push(n(
                    "item",
                    vec![expr, colname(self.d, "decl", &returned_name(self.d, &name))],
                ));
            }
        } else {
            // The names a client receives (decision 1); derived tables and
            // subqueries keep folded names, which nothing outside observes.
            let named = items.len() == q.items.len();
            item_terms = items
                .into_iter()
                .zip(q.items.iter().map(Some).chain(std::iter::repeat(None)))
                .map(|((t, _), it)| match (named, it, t.kids()) {
                    (true, Some(it), [expr, _]) => {
                        let name = match (&it.alias, &it.expr) {
                            (Some(x), _) => colname(self.d, "alias", &returned_name(self.d, x)),
                            (None, Expr::Name(p)) => match p.last() {
                                Some(x) => colname(self.d, "col", &returned_name(self.d, x)),
                                None => colname(self.d, "expr", ""),
                            },
                            (None, _) => colname(self.d, "expr", ""),
                        };
                        n("item", vec![expr.clone(), name])
                    }
                    _ => t,
                })
                .collect();
        }
        parts.insert(1, n("items", item_terms));
        Ok(n("resultset", vec![n("q", parts)]))
    }

    fn select_stmt(&mut self, sel: &Select) -> R<Term> {
        let assigns: Vec<&SelItem> = sel.items.iter().filter(|i| i.assign.is_some()).collect();
        if !assigns.is_empty() {
            // T-SQL `SELECT @v = col FROM ...`: keeps the last row; zero rows leave @v unchanged.
            if assigns.len() != sel.items.len() {
                return Err("SELECT mixing assignments with other items".into());
            }
            if assigns.len() > 1 {
                return self.assign_row(sel);
            }
            if let Some((var, v)) = sel.bare_assign() {
                return self.stmt(&Stmt::Assign(var, v.clone()));
            }
            let (sym, ty) = self.target_sym(&Expr::Var(assigns[0].assign.clone().unwrap()))?;
            let (q, qty) = self.query(sel, QCtx::Into)?;
            let v = (self.scalar(q, "last", "unchanged"), qty);
            return Ok(n("assign", vec![sym, self.conv_var(&ty, v).0]));
        }
        if !sel.into.is_empty() {
            if let [From::DataChange {
                kind,
                stmt,
                alias: _,
            }] = sel.from.as_slice()
            {
                return self.final_table(sel, kind, stmt);
            }
            if sel.into.len() != 1 {
                return Err("SELECT INTO several variables is not supported".into());
            }
            let (sym, ty) = self.target_sym(&sel.into[0])?;
            let (q, qty) = self.query(sel, QCtx::Into)?;
            let (many, zero) = match self.d {
                Dialect::Postgres if sel.into_strict => ("error", "error"),
                Dialect::Postgres => ("first", "null"),
                _ => ("error", "unchanged"),
            };
            let v = (self.scalar(q, many, zero), qty);
            return Ok(n("assign", vec![sym, self.conv_var(&ty, v).0]));
        }
        match (self.d, self.unit.kind, self.unit.sql_body) {
            (Dialect::Postgres, UnitKind::ScalarFn, true) => {
                let (q, qty) = self.query(sel, QCtx::Scalar)?;
                let v = (self.scalar(q, "first", "null"), qty);
                let ret = self.unit.returns;
                Ok(n("return", vec![self.conv(&ret, v).0]))
            }
            (
                Dialect::Postgres,
                UnitKind::ScalarFn | UnitKind::TableFn | UnitKind::Proc | UnitKind::Trigger,
                false,
            ) => Err("SELECT without INTO in PL/pgSQL".into()),
            _ => self.resultset(sel),
        }
    }

    /// The columns an INSERT without a column list fills: every column of
    /// the table, in the catalog's order. Only for a table without identity
    /// or computed columns: T-SQL leaves those out of the list, PostgreSQL
    /// fills them too. The values must then match the columns one for one
    /// (`insert_row`, `insert`), where PostgreSQL would also accept fewer.
    fn all_columns(&self, table: &str, source: &InsertSource) -> R<Vec<String>> {
        if matches!(source, InsertSource::Default) {
            return Ok(Vec::new());
        }
        let temp;
        let t = match table.strip_prefix("temp:") {
            Some(_) => {
                temp = self.temp_table(table);
                temp.as_ref()
            }
            None => self.schema.table_exact(
                table.rsplit_once('.').map(|(s, _)| s),
                Self::base_table(table),
            ),
        };
        let Some(t) = t else {
            return Err(
                "INSERT without a column list into a table whose definition is not known".into(),
            );
        };
        if t.columns.iter().any(|c| c.identity || c.generated) {
            return Err(
                "INSERT without a column list into a table with an identity or computed column"
                    .into(),
            );
        }
        if !t.complete || t.columns.is_empty() {
            return Err(
                "INSERT without a column list into a table whose columns are not all known".into(),
            );
        }
        Ok(t.columns.iter().map(|c| c.name.clone()).collect())
    }

    /// The one table variable or temporary table of this unit (`DECLARE
    /// @t TABLE (…)`, `CREATE TEMP TABLE t (…)`) that the key `temp:…`
    /// names, read from its column list.
    fn temp_table(&self, key: &str) -> Option<super::schema::Table> {
        use sqldiff_syntax::abstract_syntax::K;
        let mut found = Vec::new();
        for (node, _) in &self.unit.table_vars {
            let ts = sqldiff_syntax::abstract_syntax::tokens(node);
            let open = ts.iter().position(|t| t.kind == K::LParen)?;
            let parts: Vec<String> = match self.d {
                Dialect::Tsql => ts.get(1).map(|t| vec![t.text.clone()])?,
                Dialect::Postgres => {
                    let at = ts.iter().position(|t| t.up == "TABLE")? + 1;
                    ts[at..open]
                        .iter()
                        .filter(|t| t.kind != K::Dot)
                        .map(|t| t.text.clone())
                        .collect()
                }
                Dialect::Db2i => return None,
            };
            let (k, _) = Self::table_name(
                &parts,
                &self.unit.other_temps,
                &self.unit.local_temps,
                self.unit.routine.as_deref(),
            );
            if k == key {
                let text = node.text().to_string();
                let from = ts[open].off - usize::from(node.text_range().start());
                found.push(format!("CREATE TABLE t {}", text.get(from..)?));
            }
        }
        let [ddl] = found.as_slice() else {
            return None;
        };
        Schema::default().with_ddl(ddl, self.d).table("t").cloned()
    }

    /// T-SQL `SELECT @a = x, @b = y FROM …` (and the PL/pgSQL loop block
    /// the conversion writes for it): each row assigns every variable, so
    /// after the last row each holds that row's value, and with no row each
    /// keeps its own, as for one variable. Each value goes through its
    /// variable's assignment conversion. The pairs are kept in variable
    /// order, which does not matter once no item reads an assigned
    /// variable: T-SQL then assigns the same values in any order. A query
    /// that reads one, or assigns a variable twice, is refused.
    fn assign_row(&mut self, sel: &Select) -> R<Term> {
        let mut targets = Vec::new();
        for it in &sel.items {
            let v = it.assign.clone().unwrap_or_default();
            let (sym, ty) = self.target_sym(&Expr::Var(v))?;
            if targets.iter().any(|(s, _)| *s == sym) {
                return Err("SELECT assigning a variable twice".into());
            }
            targets.push((sym, ty));
        }
        let (mut parts, items) = self.query_parts(sel, QCtx::Into)?;
        let mut pairs: Vec<(Term, Term)> = Vec::new();
        for ((sym, ty), v) in targets.into_iter().zip(items) {
            let v = self.conv_var(&ty, v).0;
            pairs.push((sym, v));
        }
        let reads_target = parts
            .iter()
            .chain(pairs.iter().map(|(_, v)| v))
            .any(|t| t.any(&|x| pairs.iter().any(|(s, _)| x == s)));
        if reads_target {
            return Err("SELECT reading a variable it assigns".into());
        }
        pairs.sort();
        let (syms, vals): (Vec<Term>, Vec<Term>) = pairs.into_iter().unzip();
        parts.insert(1, n("items", vals));
        let q = n("q", parts);
        Ok(n(
            "assign-row",
            vec![n("targets", syms), self.scalar(q, "last", "unchanged")],
        ))
    }

    fn insert(&mut self, table: &str, cols: &[String], source: &InsertSource) -> R<Term> {
        match source {
            InsertSource::Values(rows) if rows.len() == 1 => {
                self.single_row = true;
                let kv = self.insert_row(table, cols, &rows[0])?;
                Ok(n(
                    "insert1",
                    vec![
                        a(table),
                        n(
                            "set",
                            kv.into_iter()
                                .map(|(c, v, _)| n("kv", vec![a(c), v]))
                                .collect(),
                        ),
                    ],
                ))
            }
            InsertSource::Values(rows) => {
                // One statement: every row goes in or none does, on every side.
                self.single_row = true;
                let mut sets = Vec::new();
                for r in rows {
                    let kv = self.insert_row(table, cols, r)?;
                    sets.push(n(
                        "set",
                        kv.into_iter()
                            .map(|(c, v, _)| n("kv", vec![a(c), v]))
                            .collect(),
                    ));
                }
                // Values the table generates (identity, sequence defaults) go
                // to the rows in some order; only listing every column rules
                // them out.
                let all_listed = self
                    .schema
                    .table(Self::base_table(table))
                    .is_some_and(|t| t.columns.iter().all(|c| cols.contains(&lc(&c.name))));
                if !all_listed {
                    self.assume(asm::VALUES_ORDER);
                }
                Ok(n("insertn", vec![a(table), n("rows", sets)]))
            }
            InsertSource::Query(q) => {
                self.period_omitted(table, cols)?;
                let (parts, items) = self.query_parts(q, QCtx::Scalar)?;
                if items.len() != cols.len() {
                    return Err("INSERT column count does not match the query".into());
                }
                for c in cols {
                    self.written(table, c)?;
                }
                let mut kv: Vec<Term> = cols
                    .iter()
                    .zip(items)
                    .map(|(c, (v, vty))| {
                        let (cty, _) = self.col_ty(table, c);
                        n("kv", vec![a(c.clone()), self.conv(&cty, (v, vty)).0])
                    })
                    .collect();
                kv.sort();
                Ok(n("insertq", vec![a(table), n("set", kv), n("q", parts)]))
            }
            InsertSource::Default => {
                self.period_omitted(table, &[])?;
                Ok(n("insertdefault", vec![a(table)]))
            }
        }
    }

    fn insert_row(
        &mut self,
        table: &str,
        cols: &[String],
        row: &[Expr],
    ) -> R<Vec<(String, Term, Option<Ty>)>> {
        self.period_omitted(table, cols)?;
        if row.len() != cols.len() {
            return Err("INSERT column count does not match VALUES".into());
        }
        let mut kv = Vec::new();
        for (c, e) in cols.iter().zip(row) {
            self.written(table, c)?;
            let v = self.expr(e)?;
            let (t, ty) = self.conv_col(table, c, v);
            kv.push((c.clone(), t, ty));
        }
        kv.sort_by(|x, y| x.0.cmp(&y.0));
        Ok(kv)
    }

    fn returning(
        &mut self,
        ins: Term,
        table: &str,
        returning: &[SelItem],
        into: &[Expr],
    ) -> R<Term> {
        if !ins.is("insert1") {
            return Err("RETURNING on a multi-row insert is not supported".into());
        }
        self.scopes.push(vec![Binding {
            key: table.to_string(),
            table: table.to_string(),
            derived: None,
            unique: None,
        }]);
        let items: R<Vec<Term>> = returning
            .iter()
            .map(|i| self.expr(&i.expr).map(|x| x.0))
            .collect();
        self.scopes.pop();
        let mut targets = Vec::new();
        for t in into {
            targets.push(self.target_sym(t)?.0);
        }
        if targets.is_empty() {
            return Err("RETURNING without INTO is not supported".into());
        }
        Ok(n(
            "insret",
            vec![ins, n("items", items?), n("into", targets)],
        ))
    }

    /// Db2 `SELECT c INTO v FROM FINAL TABLE (INSERT ...)`.
    fn final_table(&mut self, sel: &Select, kind: &str, stmt: &Stmt) -> R<Term> {
        if kind != "FINAL"
            || !sel.set_ops.is_empty()
            || sel.where_.is_some()
            || !sel.group_by.is_empty()
            || !sel.order_by.is_empty()
        {
            return Err("unsupported data-change table reference".into());
        }
        let Stmt::Insert {
            target,
            cols,
            source,
            returning,
            on_conflict,
            ..
        } = stmt
        else {
            return Err("FINAL TABLE of a statement other than INSERT".into());
        };
        if !returning.is_empty() || on_conflict.is_some() {
            return Err("unsupported INSERT in FINAL TABLE".into());
        }
        let table = self.table_key(target);
        let cols: Vec<String> = cols.iter().map(|c| lc(c)).collect();
        let ins = self.insert(&table, &cols, source)?;
        self.returning(ins, &table, &sel.items, &sel.into)
    }

    fn on_conflict(
        &mut self,
        table: &str,
        cols: &[String],
        source: &InsertSource,
        keys: &[String],
        action: &ConflictAction,
        returning: &[SelItem],
    ) -> R<Term> {
        let InsertSource::Values(rows) = source else {
            return Err("ON CONFLICT with a query source is not supported".into());
        };
        if rows.len() != 1 || !returning.is_empty() {
            return Err("only single-row ON CONFLICT is supported".into());
        }
        let ConflictAction::Update(sets, None) = action else {
            return Err("ON CONFLICT DO NOTHING or with WHERE is not supported".into());
        };
        self.single_row = true;
        let kv = self.insert_row(table, cols, &rows[0])?;
        let keys: Vec<String> = keys.iter().map(|k| lc(k)).collect();
        let mut keyvals = Vec::new();
        for k in &keys {
            let Some((_, v, _)) = kv.iter().find(|(c, _, _)| c == k) else {
                return Err("ON CONFLICT key is not an inserted column".into());
            };
            keyvals.push(n("kv", vec![a(k.clone()), v.clone()]));
        }
        self.excluded = Some(kv.clone());
        self.scopes.push(vec![Binding {
            key: table.to_string(),
            table: table.to_string(),
            derived: None,
            unique: None,
        }]);
        let upd: R<Vec<Term>> = sets
            .iter()
            .map(|(c, e)| {
                let c = lc(c);
                self.written(table, &c)?;
                let v = self.expr(e)?;
                let v = self.conv_col(table, &c, v).0;
                Ok(n("kv", vec![a(c), v]))
            })
            .collect();
        self.scopes.pop();
        self.excluded = None;
        let mut upd = upd?;
        upd.sort();
        Ok(self.upsert(table, keyvals, kv, upd, "safe"))
    }

    fn upsert(
        &mut self,
        table: &str,
        mut keyvals: Vec<Term>,
        ins: Vec<(String, Term, Option<Ty>)>,
        upd: Vec<Term>,
        conc: &str,
    ) -> Term {
        keyvals.sort();
        n(
            "upsert",
            vec![
                a(table),
                n("key", keyvals),
                n(
                    "insert",
                    ins.into_iter()
                        .map(|(c, v, _)| n("kv", vec![a(c), v]))
                        .collect(),
                ),
                n("update", upd),
                n("concurrency", vec![a(conc)]),
            ],
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn merge(
        &mut self,
        target: &[String],
        alias: &Option<String>,
        hints: &[String],
        source: &From,
        on: &Expr,
        whens: &[super::ast::When],
    ) -> R<Term> {
        let table = self.table_key(target);
        self.single_row = true;
        // Single-row source: SELECT without FROM, or one VALUES row.
        let src: Vec<(String, Term, Option<Ty>)> = match source {
            From::Sub {
                query,
                alias: Some(sa),
                cols,
            } if query.from.is_empty()
                && query.set_ops.is_empty()
                && query.values.is_none()
                && cols.is_empty()
                && query.where_.is_none() =>
            {
                let mut v = Vec::new();
                for it in &query.items {
                    let name = it
                        .alias
                        .clone()
                        .map(|x| lc(&x))
                        .ok_or("unnamed MERGE source column")?;
                    let (t, ty) = self.expr(&it.expr)?;
                    v.push((name, t, ty));
                }
                let _ = sa;
                v
            }
            From::Values {
                rows,
                alias: Some(_),
                cols,
            } if rows.len() == 1 && rows[0].len() == cols.len() => {
                let mut v = Vec::new();
                for (c, e) in cols.iter().zip(&rows[0]) {
                    let (t, ty) = self.expr(e)?;
                    v.push((lc(c), t, ty));
                }
                v
            }
            _ => return Err("MERGE from a multi-row source is not supported".into()),
        };
        let src_key = match source {
            From::Sub {
                alias: Some(sa), ..
            }
            | From::Values {
                alias: Some(sa), ..
            } => lc(sa),
            _ => String::new(),
        };
        let tb = Binding {
            key: alias
                .clone()
                .map(|x| lc(&x))
                .unwrap_or_else(|| table.clone()),
            table: table.clone(),
            derived: None,
            unique: None,
        };
        self.scopes.push(vec![
            tb,
            Binding {
                key: src_key,
                table: "merge:source".into(),
                derived: Some(src),
                unique: None,
            },
        ]);
        let r = self.merge_inner(&table, hints, on, whens);
        self.scopes.pop();
        r
    }

    fn merge_inner(
        &mut self,
        table: &str,
        hints: &[String],
        on: &Expr,
        whens: &[super::ast::When],
    ) -> R<Term> {
        // ON: conjunction of target.col = source value.
        let on_t = self.cond(on)?.0;
        let conj: Vec<Term> = if on_t.is("and") {
            on_t.kids().to_vec()
        } else {
            vec![on_t]
        };
        let mut keyvals = Vec::new();
        for c in conj {
            let (x, y) = match c.head() {
                "cmp" if c.kid(0).and_then(Term::atom) == Some("=") => {
                    (c.kids()[1].clone(), c.kids()[2].clone())
                }
                _ => return Err("MERGE ON is not a key equality".into()),
            };
            let is_target = |t: &Term| t.is("col") && t.kid(0).and_then(Term::atom) == Some(table);
            let (col, val) = if is_target(&x) && !is_target(&y) {
                (x, y)
            } else if is_target(&y) && !is_target(&x) {
                (y, x)
            } else {
                return Err("MERGE ON is not a key equality".into());
            };
            keyvals.push(n("kv", vec![col.kids()[1].clone(), val]));
        }
        let mut ins = None;
        let mut upd = None;
        for w in whens {
            if w.cond.is_some() || w.by_source {
                return Err("conditional or BY SOURCE MERGE clauses are not supported".into());
            }
            match (&w.action, w.matched) {
                (MergeAction::Update(sets), true) if upd.is_none() => {
                    let mut kv = Vec::new();
                    for (c, e) in sets {
                        let c = lc(c);
                        self.written(table, &c)?;
                        let v = self.expr(e)?;
                        let v = self.conv_col(table, &c, v).0;
                        kv.push(n("kv", vec![a(c), v]));
                    }
                    kv.sort();
                    upd = Some(kv);
                }
                (MergeAction::Insert(cols, vals), false)
                    if ins.is_none() && cols.len() == vals.len() && !cols.is_empty() =>
                {
                    let cols: Vec<String> = cols.iter().map(|c| lc(c)).collect();
                    ins = Some(self.insert_row(table, &cols, vals)?);
                }
                _ => return Err("unsupported MERGE clause combination".into()),
            }
        }
        let (Some(ins), Some(upd)) = (ins, upd) else {
            return Err("MERGE is not an upsert".into());
        };
        // The inserted key must equal the matched key.
        for kv in &keyvals {
            let col = kv.kids()[0].atom().unwrap_or("");
            if ins.iter().find(|(c, _, _)| c == col).map(|x| &x.1) != Some(&kv.kids()[1]) {
                return Err("MERGE inserts a different key than it matches".into());
            }
        }
        let key_cols: Vec<String> = keyvals
            .iter()
            .filter_map(|k| k.kids()[0].atom().map(str::to_string))
            .collect();
        match self.schema.table(table) {
            Some(t) if t.is_unique(&key_cols) => {
                self.discharge(format!(
                    "{table}.{} is a unique key (catalog)",
                    key_cols.join(", ")
                ));
            }
            _ => self.assume((
                "ASM-UPSERT-KEY-UNIQUE",
                "The MERGE match columns are a unique key of the target",
            )),
        }
        let conc = match self.d {
            Dialect::Tsql => {
                if hints.iter().any(|h| h == "HOLDLOCK" || h == "SERIALIZABLE") {
                    "safe"
                } else {
                    "race"
                }
            }
            Dialect::Postgres => "race",
            Dialect::Db2i => {
                self.assume(asm::DB2_MERGE_CONCURRENT);
                "safe"
            }
        };
        Ok(self.upsert(table, keyvals, ins, upd, conc))
    }

    /// The SET list of an UPDATE of `table`, sorted.
    fn update_sets(&mut self, table: &str, sets: &[(Vec<String>, Expr)]) -> R<Vec<Term>> {
        let mut kv = Vec::new();
        for (cols, e) in sets {
            for c in cols {
                self.written(table, &lc(c))?;
            }
            let (v, vty) = self.expr(e)?;
            if cols.len() == 1 {
                let col = lc(&cols[0]);
                let v = self.conv_col(table, &col, (v, vty)).0;
                kv.push(n("kv", vec![a(col), v]));
            } else {
                // `SET (a, b) = (subquery)`: errors on several rows, NULLs on none.
                let v = if v.is("subq") {
                    self.scalar(v.kids()[0].clone(), "error", "null")
                } else {
                    v
                };
                kv.push(n(
                    "kvs",
                    vec![n("cols", cols.iter().map(|c| a(lc(c))).collect()), v],
                ));
            }
        }
        kv.sort();
        Ok(kv)
    }

    /// `UPDATE … FROM`. T-SQL `UPDATE a SET … FROM <tables> WHERE w`, where
    /// `a` names one of the FROM tables, and PostgreSQL `UPDATE t [AS] a SET
    /// … FROM <others> WHERE w` both change a target row when the other
    /// tables have a row that satisfies the conditions, with values from
    /// that row. With several such rows each picks one unpredictably, so the
    /// statement is lowered only when the catalog shows there is at most one
    /// (`unique_match`). T-SQL is brought to PostgreSQL's shape: the target
    /// leaves the FROM list, and the ON condition of the inner join that tied
    /// it to its neighbour moves to WHERE (`tsql_update_target`). Both
    /// sides then need every other ON condition to stay clear of the target:
    /// PostgreSQL rejects a reference to it there, and a LEFT JOIN's
    /// condition cannot move.
    fn update_from(
        &mut self,
        target: &[String],
        alias: &Option<String>,
        sets: &[(Vec<String>, Expr)],
        from: &[From],
        where_: &Option<Expr>,
    ) -> R<Term> {
        let (tname, talias, others, mut conds) = match self.d {
            Dialect::Postgres => (target.to_vec(), alias.clone(), from.to_vec(), Vec::new()),
            Dialect::Tsql => tsql_update_target(target, alias, from)?,
            Dialect::Db2i => return Err("UPDATE ... FROM is not supported".into()),
        };
        conds.extend(where_.iter().cloned());
        if others.is_empty() {
            // `UPDATE t … FROM t WHERE w` is a plain UPDATE.
            let where_ = conds
                .into_iter()
                .reduce(|l, r| Expr::Bin("AND".into(), Box::new(l), Box::new(r)));
            return self.stmt(&Stmt::Update {
                target: tname,
                alias: talias,
                sets: sets.to_vec(),
                from: Vec::new(),
                where_,
            });
        }
        let tkey = talias
            .as_ref()
            .map(|x| lc(x))
            .unwrap_or_else(|| lc(tname.last().map(String::as_str).unwrap_or("")));
        let mut ons = Vec::new();
        for f in &others {
            join_conditions(f, &mut ons)?;
        }
        if !ons.is_empty() {
            let ttab = self
                .schema
                .table(tname.last().map(String::as_str).unwrap_or(""));
            for on in &ons {
                if mentions(on, &tkey) {
                    return Err(format!(
                        "UPDATE ... FROM: a join condition other than the target's own names the target `{tkey}`"
                    ));
                }
                let mut bare = Vec::new();
                unqualified(on, &mut bare);
                for name in bare {
                    if ttab.map_or(true, |t| t.column(&name).is_some()) {
                        return Err(format!(
                            "UPDATE ... FROM: `{name}` in a join condition may name a column of the target"
                        ));
                    }
                }
            }
        }
        self.scopes.push(Vec::new());
        let r = (|| {
            let b = self.bind_table(&tname, &talias);
            let table = b.table.clone();
            self.scopes.last_mut().unwrap().push(b);
            let from_t = self.push_from(&others)?;
            // The derived tables of FROM, in order, with their keys as bound
            // (WHERE's subqueries may reuse an alias).
            let subs: Vec<(String, Option<Vec<String>>)> = self
                .scopes
                .last()
                .map(|s| {
                    s.iter()
                        .filter(|b| b.table.starts_with("sub:"))
                        .map(|b| (b.table.clone(), b.unique.clone()))
                        .collect()
                })
                .unwrap_or_default();
            let w = match conds
                .into_iter()
                .reduce(|l, r| Expr::Bin("AND".into(), Box::new(l), Box::new(r)))
            {
                Some(w) => self.cond(&w)?.0,
                None => a("-"),
            };
            self.unique_match(&table, &from_t, &subs, &w)?;
            let kv = self.update_sets(&table, sets)?;
            Ok(n(
                "update-from",
                vec![a(table), n("set", kv), n("from", from_t), w],
            ))
        })();
        self.scopes.pop();
        r
    }

    /// Every row of `target` matches at most one row of the FROM tables
    /// under the WHERE condition `w`. A table is settled once the target or
    /// tables already settled fix a value for each column of one of its
    /// unique keys (catalog), by an equality: in its own ON condition for a
    /// LEFT JOIN, else in WHERE or any inner join's ON condition. String keys
    /// count only under the dialect's own comparison, which its unique keys
    /// follow.
    fn unique_match(
        &mut self,
        target: &str,
        from: &[Term],
        subs: &[(String, Option<Vec<String>>)],
        w: &Term,
    ) -> R<()> {
        fn conj(t: &Term) -> Vec<Term> {
            if t.is("and") {
                t.kids().to_vec()
            } else if t.atom() == Some("-") {
                Vec::new()
            } else {
                vec![t.clone()]
            }
        }
        // (table, own ON condition of a LEFT JOIN). A derived table is
        // named by the next of `subs`, its bindings in FROM order.
        type Subs<'s> = std::slice::Iter<'s, (String, Option<Vec<String>>)>;
        fn name(t: &Term, subs: &mut Subs) -> Option<String> {
            match t.head() {
                "t" => Some(t.kid(0).and_then(Term::atom).unwrap_or("").to_string()),
                "sub" => subs.next().map(|s| s.0.clone()),
                _ => None,
            }
        }
        fn leaves(
            t: &Term,
            out: &mut Vec<(String, Option<Term>)>,
            all: &mut Vec<Term>,
            subs: &mut Subs,
        ) -> bool {
            match t.head() {
                "t" | "sub" => {
                    let Some(name) = name(t, subs) else {
                        return false;
                    };
                    out.push((name, None));
                    true
                }
                "join" => {
                    let (Some(kind), Some(l), Some(r), Some(on)) =
                        (t.kid(0).and_then(Term::atom), t.kid(1), t.kid(2), t.kid(3))
                    else {
                        return false;
                    };
                    if !leaves(l, out, all, subs) || !(r.is("t") || r.is("sub")) {
                        return false;
                    }
                    let Some(name) = name(r, subs) else {
                        return false;
                    };
                    match kind {
                        "inner" | "cross" => {
                            all.extend(conj(on));
                            out.push((name, None));
                        }
                        "left" => out.push((name, Some(on.clone()))),
                        _ => return false,
                    }
                    true
                }
                _ => false,
            }
        }
        let fail = || {
            Err("UPDATE ... FROM: the catalog does not show that a target row matches at most one row of the other tables; with several, both dialects pick one unpredictably".to_string())
        };
        let mut ls = Vec::new();
        let mut all = conj(w);
        let mut sub_iter = subs.iter();
        for f in from {
            if !leaves(f, &mut ls, &mut all, &mut sub_iter) {
                return fail();
            }
        }
        let own = if self.d == Dialect::Tsql { "ci" } else { "cs" };
        let mut bound = vec![target.to_string()];
        loop {
            let mut progress = false;
            for (t, on) in &ls {
                if bound.contains(t) {
                    continue;
                }
                let cs = match on {
                    Some(on) => conj(on),
                    None => all.clone(),
                };
                let mut cols = Vec::new();
                for c in &cs {
                    let (x, y) = match c.head() {
                        "cmp" if c.kid(0).and_then(Term::atom) == Some("=") => (c.kid(1), c.kid(2)),
                        "strcmp"
                            if c.kid(0).and_then(Term::atom) == Some("=")
                                && c.kid(1).and_then(Term::atom) == Some(own) =>
                        {
                            (c.kid(3), c.kid(4))
                        }
                        _ => continue,
                    };
                    let (Some(x), Some(y)) = (x, y) else { continue };
                    let fixed = |e: &Term| {
                        !e.any(&|s| {
                            s.is("col")
                                && !s
                                    .kid(0)
                                    .and_then(Term::atom)
                                    .is_some_and(|tb| bound.iter().any(|b| b == tb))
                        })
                    };
                    for (col, other) in [(x, y), (y, x)] {
                        if col.is("col")
                            && col.kid(0).and_then(Term::atom) == Some(t.as_str())
                            && fixed(other)
                        {
                            if let Some(c) = col.kid(1).and_then(Term::atom) {
                                cols.push(c.to_string());
                            }
                        }
                    }
                }
                let (key, how) = if t.starts_with("sub:") {
                    let k = subs
                        .iter()
                        .find(|s| s.0 == *t)
                        .and_then(|s| s.1.as_ref())
                        .filter(|k| !k.is_empty() && k.iter().all(|c| cols.contains(c)))
                        .cloned();
                    (k, "GROUP BY")
                } else {
                    let k = self.schema.table(Self::base_table(t)).and_then(|tab| {
                        tab.keys
                            .iter()
                            .find(|k| !k.is_empty() && k.iter().all(|c| cols.contains(c)))
                            .cloned()
                    });
                    (k, "catalog")
                };
                if let Some(key) = key {
                    self.discharge(format!("{t}.{} is a unique key ({how})", key.join(", ")));
                    bound.push(t.clone());
                    progress = true;
                }
            }
            if !progress {
                break;
            }
        }
        if ls.iter().all(|(t, _)| bound.contains(t)) {
            Ok(())
        } else {
            fail()
        }
    }

    fn delete(
        &mut self,
        target: &[String],
        alias: &Option<String>,
        from: &[From],
        where_: &Option<Expr>,
    ) -> R<Term> {
        self.scopes.push(Vec::new());
        let r = (|| {
            if from.is_empty() {
                let b = self.bind_table(target, alias);
                let table = b.table.clone();
                self.scopes.last_mut().unwrap().push(b);
                let w = match where_ {
                    Some(w) => self.cond(w)?.0,
                    None => a("-"),
                };
                return Ok(n("delete", vec![a(table), w]));
            }
            if let Some(r) = self.semi_join_delete(target, alias, from, where_) {
                return r;
            }
            self.anti_join_delete(target, from, where_)
        })();
        self.scopes.pop();
        r
    }

    /// A DELETE whose FROM/USING list only filters the target rows:
    /// `DELETE FROM t WHERE EXISTS (SELECT 1 FROM <others> WHERE <conds>)`.
    /// PostgreSQL `DELETE FROM t USING u WHERE w`: the USING tables are
    /// the others and `w` the condition. T-SQL `DELETE t FROM <tables>`:
    /// `t` names exactly one FROM table (by its alias, or by a one-part name
    /// of an unaliased table), all joins are inner or cross joins, and the
    /// others are the remaining tables with every ON condition and WHERE as
    /// the condition. In both a target row is deleted once if at least one
    /// row of the others satisfies the condition. `None` when the shape is
    /// not one of these (outer joins, APPLY, hints, a target not in FROM).
    fn semi_join_delete(
        &mut self,
        target: &[String],
        alias: &Option<String>,
        from: &[From],
        where_: &Option<Expr>,
    ) -> Option<R<Term>> {
        let mut leaves = Vec::new();
        let mut conds = Vec::new();
        for f in from {
            if !inner_leaves(f, &mut leaves, &mut conds) {
                return None;
            }
        }
        let (tname, talias, others) = match self.d {
            Dialect::Postgres => (target.to_vec(), alias.clone(), leaves),
            Dialect::Tsql => {
                if alias.is_some() || target.len() != 1 {
                    return None;
                }
                let key = lc(&target[0]);
                let hits: Vec<usize> = (0..leaves.len())
                    .filter(|&i| match &leaves[i] {
                        From::Table { name, alias, .. } => match alias {
                            Some(a) => lc(a) == key,
                            None => lc(name.last().unwrap()) == key,
                        },
                        From::Sub { alias, .. } | From::Values { alias, .. } => {
                            alias.as_ref().is_some_and(|a| lc(a) == key)
                        }
                        _ => false,
                    })
                    .collect();
                let [hit] = hits[..] else { return None };
                let From::Table { name, alias, hints } = leaves.remove(hit) else {
                    return None;
                };
                if !hints.is_empty() {
                    return None;
                }
                (name, alias, leaves)
            }
            Dialect::Db2i => return None,
        };
        if others.iter().any(|f| match f {
            From::Table { hints, .. } => !hints.is_empty(),
            From::Sub { .. } | From::Values { .. } => false,
            _ => true,
        }) {
            return None;
        }
        conds.extend(where_.iter().cloned());
        if !others.is_empty() {
            // The conditions move into the EXISTS subquery, where an
            // unqualified name resolves to the inner tables first. In the
            // source all tables share one scope, so a name in two of them is
            // an error there; accept a name only when the catalog shows it in
            // exactly one table (or in none, for a parameter or variable).
            if conds.iter().any(has_query) {
                return Some(Err(
                    "DELETE semi-join: a subquery or CASE in the conditions may use unqualified names".into(),
                ));
            }
            let mut bare = Vec::new();
            for c in &conds {
                unqualified(c, &mut bare);
            }
            let mut tables = vec![tname.clone()];
            for f in &others {
                match f {
                    From::Table { name, .. } => tables.push(name.clone()),
                    _ if bare.is_empty() => {}
                    _ => {
                        return Some(Err(
                            "DELETE semi-join: unqualified names next to a derived table".into(),
                        ))
                    }
                }
            }
            for name in &bare {
                let mut hits = 0;
                for t in &tables {
                    match self.schema.table(t.last().unwrap()) {
                        Some(tab) => hits += usize::from(tab.column(name).is_some()),
                        None => {
                            return Some(Err(format!(
                            "DELETE semi-join: `{name}` may name a column of more than one table"
                        )))
                        }
                    }
                }
                if !(hits == 1 || hits == 0 && self.sym(name).is_some()) {
                    return Some(Err(format!(
                        "DELETE semi-join: `{name}` may name a column of more than one table"
                    )));
                }
            }
        }
        let cond = conds
            .into_iter()
            .reduce(|l, r| Expr::Bin("AND".into(), Box::new(l), Box::new(r)));
        let pred = if others.is_empty() {
            cond
        } else {
            Some(Expr::Exists(Box::new(Select {
                items: vec![SelItem {
                    expr: Expr::Num("1".into()),
                    alias: None,
                    assign: None,
                }],
                from: others,
                where_: cond,
                ..Select::default()
            })))
        };
        let b = self.bind_table(&tname, &talias);
        let table = b.table.clone();
        self.scopes.last_mut().unwrap().push(b);
        Some((|| {
            let w = match &pred {
                Some(p) => self.expr(p)?.0,
                None => a("-"),
            };
            Ok(n("delete", vec![a(table), w]))
        })())
    }

    /// `DELETE t FROM t LEFT JOIN u ON <eqs> WHERE u.c IS NULL [AND rest]`
    /// is `DELETE FROM t WHERE NOT EXISTS (SELECT 1 FROM u WHERE <eqs>) [AND rest]`
    /// when `u.c` takes part in an equality of the ON clause: a matched row
    /// has `u.c` non-null, so `u.c IS NULL` holds exactly for unmatched rows.
    fn anti_join_delete(
        &mut self,
        target: &[String],
        from: &[From],
        where_: &Option<Expr>,
    ) -> R<Term> {
        let unsupported =
            || Err("DELETE with FROM/USING is not supported unless it is an anti-join".to_string());
        let [From::Join {
            kind,
            left,
            right,
            on: Some(on),
        }] = from
        else {
            return unsupported();
        };
        if kind != "LEFT" {
            return unsupported();
        }
        let (
            From::Table {
                name: lname,
                alias: lalias,
                hints: lh,
            },
            From::Table {
                name: rname,
                alias: ralias,
                hints: rh,
            },
        ) = (left.as_ref(), right.as_ref())
        else {
            return unsupported();
        };
        if !lh.is_empty() || !rh.is_empty() || target.len() != 1 {
            return unsupported();
        }
        let tkey = lc(&target[0]);
        let lkey = lalias
            .clone()
            .map(|x| lc(&x))
            .unwrap_or_else(|| lc(lname.last().unwrap()));
        if tkey != lkey && tkey != lc(lname.last().unwrap()) {
            return unsupported();
        }
        let rkey = ralias
            .clone()
            .map(|x| lc(&x))
            .unwrap_or_else(|| lc(rname.last().unwrap()));
        // WHERE: u.c IS NULL [AND rest].
        let Some(w) = where_ else {
            return unsupported();
        };
        let mut conj = Vec::new();
        flatten_and(w, &mut conj);
        let is_null_pos = conj.iter().position(|c| {
            matches!(c, Expr::IsNull(x, false) if matches!(x.as_ref(), Expr::Name(p) if p.len() == 2 && lc(&p[0]) == rkey))
        });
        let Some(pos) = is_null_pos else {
            return unsupported();
        };
        let Expr::IsNull(x, _) = conj.remove(pos) else {
            unreachable!()
        };
        let Expr::Name(p) = *x else { unreachable!() };
        let null_col = lc(&p[1]);
        let mut on_conj = Vec::new();
        flatten_and(on, &mut on_conj);
        let in_eq = on_conj.iter().any(|c| match c {
            Expr::Bin(op, l, r) if op == "=" => [l, r].iter().any(|s| matches!(s.as_ref(), Expr::Name(q) if q.len() == 2 && lc(&q[0]) == rkey && lc(&q[1]) == null_col)),
            _ => false,
        });
        if !in_eq || conj.iter().any(|c| mentions(c, &rkey)) {
            return unsupported();
        }
        // Unqualified names in the rest of WHERE could belong to the joined
        // table; keep them only when the catalog shows they cannot.
        let mut bare = Vec::new();
        for c in &conj {
            unqualified(c, &mut bare);
        }
        let ltab = self.schema.table(lname.last().unwrap());
        let rtab = self.schema.table(rname.last().unwrap());
        for name in &bare {
            let in_l = ltab.map(|t| t.column(name).is_some());
            let in_r = rtab.map(|t| t.column(name).is_some());
            let ok = match (in_l, in_r) {
                (Some(true), Some(false)) => true,
                (Some(false), Some(false)) => self.sym(name).is_some(),
                _ => false,
            };
            if !ok {
                return Err(format!(
                    "DELETE anti-join: `{name}` may name a column of the joined table"
                ));
            }
        }
        let sub = Select {
            items: vec![SelItem {
                expr: Expr::Num("1".into()),
                alias: None,
                assign: None,
            }],
            from: vec![From::Table {
                name: rname.clone(),
                alias: ralias.clone(),
                hints: Vec::new(),
            }],
            where_: Some(on.clone()),
            ..Select::default()
        };
        let mut pred = Expr::Unary("NOT".into(), Box::new(Expr::Exists(Box::new(sub))));
        for c in conj {
            pred = Expr::Bin("AND".into(), Box::new(pred), Box::new(c));
        }
        let b = self.bind_table(lname, lalias);
        let table = b.table.clone();
        self.scopes.last_mut().unwrap().push(b);
        let w = self.expr(&pred)?.0;
        Ok(n("delete", vec![a(table), w]))
    }

    /// `assign(return_code, coalesce(v, 0))` then `return` in a PostgreSQL
    /// procedure, `return_code` being its INOUT parameter of that name and
    /// `v` an integer literal or an integer variable or parameter: `v`, and
    /// whether it may be NULL.
    fn return_code(&self, x: &Term, y: &Term) -> Option<(Term, bool)> {
        if self.unit.dialect != Dialect::Postgres
            || self.unit.kind != UnitKind::Proc
            || !y.is("return")
            || !y.kids().is_empty()
            || !x.is("assign")
        {
            return None;
        }
        let param = |s: &Term| {
            let k = s.kid(0)?.atom()?.strip_prefix('p')?.parse::<usize>().ok()?;
            self.unit.params.get(k.checked_sub(1)?)
        };
        let target = x.kid(0).filter(|s| s.is("sym")).and_then(param)?;
        if target.name != "return_code" || !target.out {
            return None;
        }
        let v = x.kid(1).filter(|v| {
            v.is("fn")
                && v.kids().len() == 3
                && v.kid(0).and_then(Term::atom) == Some("coalesce")
                && v.kid(2) == Some(&n("num", vec![a("0")]))
        })?;
        let v = v.kid(1)?;
        let int = |t: &Option<Ty>| {
            t.is_some_and(|t| t.fam == Fam::Int && t.len.is_some_and(|b| b > 1 && b <= 32))
        };
        if v.is("num") {
            v.kid(0)?.atom()?.parse::<i32>().ok()?;
            return Some((v.clone(), false));
        }
        if !v.is("sym") {
            return None;
        }
        let name = v.kid(0)?.atom()?;
        let k = name[1..].parse::<usize>().ok()?.checked_sub(1)?;
        let ty = match name.as_bytes().first()? {
            b'l' => self.unit.locals.get(k)?.ty,
            b'p' => self.unit.params.get(k)?.ty,
            _ => return None,
        };
        int(&ty).then(|| (v.clone(), true))
    }

    /// Statement list of the unit, after the cross-statement lifts.
    pub fn lift(&mut self, stmts: Vec<Item>) -> Vec<Item> {
        let mut out: Vec<Item> = Vec::new();
        for mut it in stmts {
            // T-SQL: INSERT ...; SET @v = SCOPE_IDENTITY();
            let t = it.term.clone();
            if t.is("assign") && t.kid(1).is_some_and(|v| v.is("scope_identity")) {
                if let Some(prev) = out.last() {
                    if prev.term.is("insert1") && prev.err.is_none() {
                        let table = prev.term.kids()[0].atom().unwrap_or("").to_string();
                        let id = match self.schema.table(&table).and_then(|x| x.identity()) {
                            Some(c) => {
                                it.disc.push(format!(
                                    "{table}.{} is the identity column (catalog)",
                                    c.name
                                ));
                                n("col", vec![a(table.clone()), a(c.name.clone())])
                            }
                            None => n("identity", vec![a(table.clone())]),
                        };
                        let mut ins = out.pop().unwrap();
                        ins.term = n(
                            "insret",
                            vec![
                                ins.term,
                                n("items", vec![id]),
                                n("into", vec![t.kids()[0].clone()]),
                            ],
                        );
                        ins.absorb(it);
                        out.push(ins);
                        continue;
                    }
                }
            }
            out.push(it);
        }
        // PostgreSQL procedure converted from T-SQL: `return_code :=
        // COALESCE(v, 0); RETURN;` is T-SQL's `RETURN v` (a procedure's
        // status is never NULL: T-SQL returns 0 instead). Only for an
        // integer `v` the assignment stores as it is (no conversion), so
        // T-SQL's conversion of `v` to int changes nothing either.
        let mut i = 0;
        while i + 1 < out.len() {
            match self.return_code(&out[i].term, &out[i + 1].term) {
                Some((v, may_be_null)) => {
                    let next = out.remove(i + 1);
                    out[i].term = n("return", vec![v]);
                    out[i].absorb(next);
                    if may_be_null {
                        out[i].asm.push(asm::make(asm::RETURN_NULL_WARNING));
                    }
                }
                None => i += 1,
            }
        }
        // Inline `assign(local, v); return(local)` when the local is fresh.
        let mut i = 0;
        while i + 1 < out.len() {
            let (x, y) = (&out[i].term, &out[i + 1].term);
            let local = x
                .kid(0)
                .filter(|s| {
                    x.is("assign")
                        && s.is("sym")
                        && s.kid(0)
                            .and_then(Term::atom)
                            .is_some_and(|v| v.starts_with('l'))
                })
                .cloned();
            if let Some(sym) = local {
                // Cheapest test first: the use count and the scan for an
                // earlier definition walk every statement, so they only run
                // in front of a `return` of the local.
                let returns_it = || y.is("return") && y.kid(0) == Some(&sym);
                let uses = || {
                    out.iter()
                        .map(|s| s.term.count(&|t| t == &sym))
                        .sum::<usize>()
                };
                // A statement that was not lowered may assign the local too.
                let first_def = || {
                    !out[..i]
                        .iter()
                        .any(|s| s.err.is_some() || s.term.any(&|t| t == &sym))
                };
                if returns_it() && uses() == 2 && first_def() {
                    let v = x.kids()[1].map(&|t| {
                        (t.is("scalar") && t.kid(2).and_then(Term::atom) == Some("unchanged")).then(
                            || {
                                n(
                                    "scalar",
                                    vec![t.kids()[0].clone(), t.kids()[1].clone(), a("null")],
                                )
                            },
                        )
                    });
                    let ret = self.unit.returns;
                    let local_ty = sym
                        .kid(0)
                        .and_then(Term::atom)
                        .and_then(|l| l[1..].parse::<usize>().ok())
                        .and_then(|k| self.unit.locals.get(k - 1))
                        .and_then(|l| l.ty);
                    let ty = ret_ty_of(&v).or(local_ty);
                    let v = self.conv_var(&ret, (v, ty)).0;
                    let next = out.remove(i + 1);
                    out[i].term = n("return", vec![v]);
                    out[i].absorb(next);
                    continue;
                }
            }
            i += 1;
        }
        out
    }
}

/// One lowered statement with its source nodes and the facts it relies on.
#[derive(Debug, Clone)]
pub struct Item {
    pub term: Term,
    pub nodes: Vec<sqldiff_syntax::SyntaxNode>,
    pub asm: Vec<Assumption>,
    pub disc: Vec<String>,
    pub err: Option<String>,
    /// Tables the statement names: (key, schema as written when it is the
    /// default one: "", "dbo" or "public"; `None` for an explicit schema).
    pub names: Vec<(String, Option<String>)>,
}

impl Item {
    fn absorb(&mut self, other: Item) {
        self.nodes.extend(other.nodes);
        for x in other.asm {
            if !self.asm.iter().any(|y| y.id == x.id) {
                self.asm.push(x);
            }
        }
        for d in other.disc {
            if !self.disc.contains(&d) {
                self.disc.push(d);
            }
        }
        if self.err.is_none() {
            self.err = other.err;
        }
        for x in other.names {
            if !self.names.contains(&x) {
                self.names.push(x);
            }
        }
    }
}

/// Best-effort type of a lowered value, for dropping no-op conversions.
/// The query returns exactly one row: an aggregate query without GROUP BY,
/// HAVING or paging (`q(distinct, items, from, where, group, having, order,
/// page, lock)`). It aggregates even when WHERE matches no row; HAVING, TOP 0
/// or OFFSET can drop that row. Only an aggregate in the select list itself
/// counts, not one inside a subquery.
const INT32: Ty = Ty {
    fam: Fam::Int,
    len: Some(32),
    scale: None,
};

/// A string expression that is never empty (NULL aside): a non-empty
/// literal, or a `||` chain with one.
fn nonempty_text(e: &Expr) -> bool {
    match e {
        Expr::Str(s) => !s.is_empty(),
        Expr::Bin(op, l, r) if op == "||" => nonempty_text(l) || nonempty_text(r),
        _ => false,
    }
}

/// `CASE WHEN length(x) = 0 THEN 0 ELSE strpos(s, x) END`: (s, x).
fn charindex_case<'e>(whens: &'e [(Expr, Expr)], els: &'e Expr) -> Option<(&'e Expr, &'e Expr)> {
    let [(Expr::Bin(op, l, r), Expr::Num(zero))] = whens else {
        return None;
    };
    let call = |e: &'e Expr, name: &str, argc: usize| match e {
        Expr::Call(c)
            if c.name.len() == 1
                && c.name[0].eq_ignore_ascii_case(name)
                && c.args.len() == argc
                && !c.distinct
                && !c.star
                && c.order.is_empty() =>
        {
            Some(&c.args)
        }
        _ => None,
    };
    let x = &call(l, "length", 1)?[0];
    let pos = call(els, "strpos", 2)?;
    (op == "=" && zero == "0" && matches!(r.as_ref(), Expr::Num(z) if z == "0") && &pos[1] == x)
        .then_some((&pos[0], x))
}

/// Whether REPLACE(s, from, to) cannot outgrow what T-SQL returns uncut:
/// 8,000 bytes for a non-MAX value, so 4,000 UTF-16 code units (nvarchar)
/// or 4,000 bytes (varchar, also under a UTF-8 collation) to be safe.
/// `from` is counted in characters (never more than its units or bytes),
/// `to` in UTF-8 bytes (never fewer than its UTF-16 units or its bytes in
/// any code page; a typed `to` at 4 bytes a character). Then the result
/// is never longer than `s` when `to` is no longer than any non-empty
/// `from` (an empty `from` changes nothing), or `s` has a known length
/// whose worst growth stays within the bound. PostgreSQL never cuts.
fn replace_fits(d: Dialect, e: &[Expr], args: &[(Term, Option<Ty>)]) -> bool {
    if d != Dialect::Tsql {
        return true;
    }
    let min_from = match &e[1] {
        Expr::Str(s) => s.chars().count() as u32,
        _ => 1,
    }
    .max(1);
    let max_to = match &e[2] {
        Expr::Str(s) => Some(s.len() as u32),
        _ => args[2].1.and_then(|t| t.len).map(|l| l.saturating_mul(4)),
    };
    let Some(max_to) = max_to else {
        return false;
    };
    if max_to <= min_from {
        return true;
    }
    match args[0].1.and_then(|t| t.len) {
        Some(len) => {
            (len / min_from + 1)
                .saturating_mul(max_to)
                .saturating_add(len)
                <= 4000
        }
        None => false,
    }
}

/// T-SQL CONVERT styles with a PostgreSQL to_char format that prints the
/// same text: (style, format, length). Checked on SQL Server 2022 and
/// PostgreSQL 18 for date, datetime and datetime2 (eval/builtins/2026-10-29).
const STYLES: &[(u32, &str, u32)] = &[
    (20, "YYYY-MM-DD HH24:MI:SS", 19),
    (120, "YYYY-MM-DD HH24:MI:SS", 19),
    (23, "YYYY-MM-DD", 10),
    (112, "YYYYMMDD", 8),
    (102, "YYYY.MM.DD", 10),
    (111, "YYYY/MM/DD", 10),
    (108, "HH24:MI:SS", 8),
];

/// The to_char format and text length of CONVERT(varchar, x, style), for a
/// date (`date_only`) or a date-time (datetime, datetime2, smalldatetime).
/// A date prints only its date part, also in styles 20 and 120; a time-only
/// style of a date is an error in T-SQL. Shared with the converter.
pub fn style_format(date_only: bool, style: u32) -> Option<(&'static str, u32)> {
    let &(_, fmt, width) = STYLES.iter().find(|x| x.0 == style)?;
    match (date_only, style) {
        (true, 20 | 120) => Some(("YYYY-MM-DD", 10)),
        (true, 108) => None,
        _ => Some((fmt, width)),
    }
}

/// DATEPART's part names (and abbreviations) that have a PostgreSQL
/// counterpart.
fn tsql_datepart(p: &str) -> Option<&'static str> {
    Some(match p.to_ascii_lowercase().as_str() {
        "year" | "yy" | "yyyy" => "year",
        "month" | "mm" | "m" => "month",
        "day" | "dd" | "d" => "day",
        "hour" | "hh" => "hour",
        "minute" | "mi" | "n" => "minute",
        "second" | "ss" | "s" => "second",
        "weekday" | "dw" | "w" => "weekday",
        _ => return None,
    })
}

/// A literal code point from 1 to 127.
fn ascii_code(e: &Expr) -> Option<u8> {
    match e {
        Expr::Num(v) => v.parse::<u8>().ok().filter(|c| (1..=127).contains(c)),
        _ => None,
    }
}

fn exactly_one(q: &Term) -> bool {
    if !q.is("q") {
        return false;
    }
    let none = |k: usize| q.kid(k).and_then(Term::atom) == Some("-");
    let ungrouped = q
        .kid(4)
        .is_some_and(|g| g.is("group") && g.kids().is_empty());
    let items = q
        .kid(1)
        .filter(|i| i.is("items"))
        .map(Term::kids)
        .unwrap_or(&[]);
    ungrouped && none(5) && none(7) && items.iter().any(aggregates)
}

/// An aggregate call in this expression, outside any subquery.
fn aggregates(t: &Term) -> bool {
    // An aggregate OVER () keeps every row.
    if t.is("q") || t.is("win") {
        return false;
    }
    // MIN and MAX of one argument; a string's comparison is the dialect's.
    // SUM and AVG stay the dialect's own (result types differ), but they
    // aggregate all the same (an empty input gives one NULL row).
    let named = |name: Option<&Term>, names: &[&str]| {
        name.and_then(Term::atom)
            .is_some_and(|x| names.contains(&x))
    };
    t.is("count")
        || t.is("stringagg")
        || (t.is("fn") && t.kids().len() == 2 && named(t.kid(0), &["min", "max"]))
        || (t.is("dfn") && t.kids().len() == 3 && named(t.kid(1), &["min", "max", "sum", "avg"]))
        || t.kids().iter().any(aggregates)
}

/// A PostgreSQL routine's parameter or return type as PostgreSQL uses it:
/// without length, precision and scale (`CREATE FUNCTION` keeps only the
/// base type). Time types keep their largest precision, 6.
pub(super) fn untypmod(t: Ty) -> Ty {
    match t.fam {
        Fam::VarChar | Fam::Char | Fam::Exact | Fam::Binary => Ty {
            len: None,
            scale: None,
            ..t
        },
        Fam::Timestamp | Fam::TimestampTz | Fam::Time => Ty {
            len: Some(6),
            scale: None,
            ..t
        },
        _ => t,
    }
}

fn ret_ty_of(t: &Term) -> Option<Ty> {
    if t.is("conv") {
        // conv(ty(fam, len, scale), v)
        let ty = t.kid(0)?;
        let fam = match ty.kid(0)?.atom()? {
            "Int" => Fam::Int,
            "Exact" => Fam::Exact,
            "VarChar" => Fam::VarChar,
            "Char" => Fam::Char,
            "Timestamp" => Fam::Timestamp,
            "Date" => Fam::Date,
            _ => return None,
        };
        let num = |i: usize| ty.kid(i).and_then(Term::atom).and_then(|s| s.parse().ok());
        return Some(Ty {
            fam,
            len: num(1),
            scale: num(2),
        });
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QCtx {
    /// A result set: column names matter.
    Set,
    /// One value per row; names do not matter.
    Scalar,
    /// EXISTS: the select list does not matter.
    Exists,
    /// SELECT INTO / assignment select.
    Into,
}

fn order_operands(op: &str, x: Term, y: Term) -> (Term, Term, String) {
    if matches!(op, "=" | "<>") {
        if x <= y {
            (x, y, op.to_string())
        } else {
            (y, x, op.to_string())
        }
    } else if x <= y {
        (x, y, op.to_string())
    } else {
        let flipped = match op {
            "<" => ">",
            ">" => "<",
            "<=" => ">=",
            ">=" => "<=",
            o => o,
        };
        (y, x, flipped.to_string())
    }
}

fn l_or_r_is_str(l: &Expr, r: &Expr) -> bool {
    matches!(l, Expr::Str(_)) || matches!(r, Expr::Str(_))
}

/// Whether an expression holds a subquery or CASE anywhere.
fn has_query(e: &Expr) -> bool {
    match e {
        Expr::Exists(_) | Expr::Subquery(_) | Expr::InQuery(..) | Expr::Case(..) => true,
        Expr::Bin(_, l, r) | Expr::Like(l, r, _, _) => has_query(l) || has_query(r),
        Expr::Unary(_, x) | Expr::IsNull(x, _) | Expr::Cast(x, _) | Expr::Extract(_, x) => {
            has_query(x)
        }
        Expr::Call(c) => c.args.iter().any(has_query) || c.order.iter().any(|o| has_query(&o.expr)),
        Expr::InList(x, l, _) => has_query(x) || l.iter().any(has_query),
        Expr::Between(x, y, z, _) => has_query(x) || has_query(y) || has_query(z),
        Expr::Tuple(l) => l.iter().any(has_query),
        _ => false,
    }
}

/// The tables of a tree of inner and cross joins, and its ON conditions;
/// false when the tree has an outer join.
fn inner_leaves(f: &From, leaves: &mut Vec<From>, conds: &mut Vec<Expr>) -> bool {
    match f {
        From::Join {
            kind,
            left,
            right,
            on,
        } => {
            matches!(kind.as_str(), "INNER" | "CROSS")
                && inner_leaves(left, leaves, conds)
                && inner_leaves(right, leaves, conds)
                && {
                    conds.extend(on.iter().cloned());
                    true
                }
        }
        _ => {
            leaves.push(f.clone());
            true
        }
    }
}

/// T-SQL `UPDATE x … FROM <tables>`: the FROM table `x` names (by its
/// alias, or by the one-part name of an unaliased table), the FROM list
/// without it, and the ON condition that joined it, now a WHERE condition.
/// The target must head a comma item or a join chain, or follow its head,
/// joined by an inner or cross join: `(t ⋈k s) ⟕ u = t ⋈k (s ⟕ u)` when
/// the LEFT JOIN's condition does not name `t`, which `update_from` checks.
#[allow(clippy::type_complexity)]
fn tsql_update_target(
    target: &[String],
    alias: &Option<String>,
    from: &[From],
) -> R<(Vec<String>, Option<String>, Vec<From>, Vec<Expr>)> {
    let unsupported = |why: &str| Err(format!("UPDATE ... FROM: {why}"));
    if target.is_empty() || target.len() > 2 {
        return unsupported("the target is not a table name, a schema-qualified one or an alias");
    }
    if alias.is_some() {
        return unsupported("the target has an alias");
    }
    let t: Vec<String> = target.iter().map(|x| lc(x)).collect();
    let one = t.len() == 1;
    let key = t.last().cloned().unwrap_or_default();
    // SQL Server binds the target to the FROM reference of the same table,
    // aliased or not, and a one-part target also to an alias or to an
    // unaliased table's last name part (checked on SQL Server 2022). Where
    // only one side states a schema the default schema decides, which is
    // not modelled.
    let mut unclear = None;
    let mut binds = |f: &From| match f {
        From::Table { name, alias, .. } => {
            let n: Vec<String> = name.iter().map(|x| lc(x)).collect();
            if let Some(a) = alias {
                if lc(a) == key {
                    if !one {
                        unclear = Some("the alias of another FROM item has the target's name");
                    }
                    return one;
                }
            }
            if n.last() != Some(&key) {
                return false;
            }
            if n == t || (one && alias.is_none()) {
                return true;
            }
            if n.len() != t.len() {
                unclear = Some("the target and a FROM table may or may not be the same table (only one states a schema)");
            }
            false
        }
        From::Sub { alias, .. } | From::Values { alias, .. } | From::Func { alias, .. } => {
            let hit = alias.as_ref().is_some_and(|a| lc(a) == key);
            if hit && !one {
                unclear = Some("the alias of another FROM item has the target's name");
            }
            hit && one
        }
        _ => false,
    };
    let mut chains = Vec::new();
    for f in from {
        let mut c = Vec::new();
        if !chain(f, &mut c) {
            return unsupported("a join that is not a chain of inner, left and cross joins");
        }
        chains.push(c);
    }
    let mut hits: Vec<(usize, usize)> = Vec::new();
    for (i, c) in chains.iter().enumerate() {
        for (j, s) in c.iter().enumerate() {
            if binds(&s.1) {
                hits.push((i, j));
            }
        }
    }
    if let Some(w) = unclear {
        return unsupported(w);
    }
    if hits.is_empty() {
        // The target is not in FROM: SQL Server joins it to the FROM
        // tables, in scope in SET and WHERE but not in ON, which is
        // PostgreSQL's shape (checked on SQL Server 2022). A FROM item that
        // also shows the target's name is not modelled.
        let shown = chains.iter().flatten().any(|s| match &s.1 {
            From::Table { name, alias, .. } => match alias {
                Some(a) => lc(a) == key,
                None => name.last().is_some_and(|x| lc(x) == key),
            },
            From::Sub { alias, .. } | From::Values { alias, .. } | From::Func { alias, .. } => {
                alias.as_ref().is_some_and(|a| lc(a) == key)
            }
            _ => true,
        });
        if shown {
            return unsupported("the target is not in FROM, and a FROM item is also named like it");
        }
        return Ok((target.to_vec(), None, from.to_vec(), Vec::new()));
    }
    let [(ci, li)] = hits[..] else {
        return unsupported("the target matches more than one FROM table");
    };
    let c = &mut chains[ci];
    let From::Table { name, alias, hints } = c[li].1.clone() else {
        return unsupported("the target is not a table");
    };
    if !hints.is_empty() {
        return unsupported("table hints on the target");
    }
    let inner = |k: &Option<String>| matches!(k.as_deref(), Some("INNER" | "CROSS"));
    let mut conds = Vec::new();
    match li {
        0 if c.len() == 1 => {
            chains.remove(ci);
        }
        0 if inner(&c[1].0) => {
            conds.extend(c[1].2.take());
            c.remove(0);
            c[0].0 = None;
        }
        1 if inner(&c[1].0) => {
            conds.extend(c[1].2.take());
            c.remove(1);
        }
        _ => return unsupported("the target is not joined first, by an inner or cross join"),
    }
    let others = chains
        .into_iter()
        .filter_map(|c| {
            c.into_iter().reduce(|l, r| {
                let (kind, right, on) = (r.0, r.1, r.2);
                (
                    None,
                    From::Join {
                        kind: kind.unwrap_or_default(),
                        left: Box::new(l.1),
                        right: Box::new(right),
                        on,
                    },
                    None,
                )
            })
        })
        .map(|x| x.1)
        .collect();
    Ok((name, alias, others, conds))
}

/// A left-deep chain of inner, left and cross joins as its tables, each
/// with the join kind (none for the first) and ON condition that adds it.
fn chain(f: &From, out: &mut Vec<(Option<String>, From, Option<Expr>)>) -> bool {
    match f {
        From::Join {
            kind,
            left,
            right,
            on,
        } => {
            matches!(kind.as_str(), "INNER" | "LEFT" | "CROSS")
                && !matches!(right.as_ref(), From::Join { .. })
                && chain(left, out)
                && {
                    out.push((Some(kind.clone()), right.as_ref().clone(), on.clone()));
                    true
                }
        }
        _ => {
            out.push((None, f.clone(), None));
            true
        }
    }
}

/// The ON conditions of a FROM item; an error for joins other than inner,
/// left and cross ones.
fn join_conditions(f: &From, out: &mut Vec<Expr>) -> R<()> {
    if let From::Join {
        kind,
        left,
        right,
        on,
    } = f
    {
        if !matches!(kind.as_str(), "INNER" | "LEFT" | "CROSS") {
            return Err(format!(
                "UPDATE ... FROM with a {kind} JOIN is not supported"
            ));
        }
        join_conditions(left, out)?;
        join_conditions(right, out)?;
        out.extend(on.iter().cloned());
    }
    Ok(())
}

fn flatten_and(e: &Expr, out: &mut Vec<Expr>) {
    match e {
        Expr::Bin(op, l, r) if op == "AND" => {
            flatten_and(l, out);
            flatten_and(r, out);
        }
        _ => out.push(e.clone()),
    }
}

/// Single-part names in an expression (subqueries are refused by `mentions`).
fn unqualified(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Name(p) if p.len() == 1 => out.push(p[0].clone()),
        Expr::Bin(_, l, r) | Expr::Like(l, r, _, _) => {
            unqualified(l, out);
            unqualified(r, out);
        }
        Expr::Unary(_, x) | Expr::IsNull(x, _) | Expr::Cast(x, _) | Expr::Extract(_, x) => {
            unqualified(x, out)
        }
        Expr::Call(c) => {
            c.args.iter().for_each(|x| unqualified(x, out));
            c.order.iter().for_each(|o| unqualified(&o.expr, out));
        }
        Expr::InList(x, l, _) => {
            unqualified(x, out);
            l.iter().for_each(|y| unqualified(y, out));
        }
        Expr::Between(x, y, z, _) => [x, y, z].iter().for_each(|v| unqualified(v, out)),
        Expr::Tuple(l) => l.iter().for_each(|y| unqualified(y, out)),
        _ => {}
    }
}

fn mentions(e: &Expr, alias: &str) -> bool {
    match e {
        Expr::Name(p) => p.len() > 1 && lc(&p[p.len() - 2]) == alias,
        Expr::Bin(_, l, r) => mentions(l, alias) || mentions(r, alias),
        Expr::Unary(_, x) | Expr::IsNull(x, _) | Expr::Cast(x, _) | Expr::Extract(_, x) => {
            mentions(x, alias)
        }
        Expr::Call(c) => c.args.iter().any(|x| mentions(x, alias)),
        Expr::InList(x, l, _) => mentions(x, alias) || l.iter().any(|y| mentions(y, alias)),
        Expr::Between(x, y, z, _) => mentions(x, alias) || mentions(y, alias) || mentions(z, alias),
        Expr::Like(x, y, _, _) => mentions(x, alias) || mentions(y, alias),
        Expr::Tuple(l) => l.iter().any(|y| mentions(y, alias)),
        // Subqueries could reference the alias; treat as mentioning it.
        Expr::Exists(_) | Expr::Subquery(_) | Expr::InQuery(..) | Expr::Case(..) => true,
        _ => false,
    }
}

/// A type argument written in CONVERT: a length or MAX.
fn type_arg(e: &Expr) -> bool {
    match e {
        Expr::Num(v) => v.bytes().all(|b| b.is_ascii_digit()),
        Expr::Name(p) => p.len() == 1 && p[0].eq_ignore_ascii_case("max"),
        _ => false,
    }
}

/// The table of `SELECT * FROM t WHERE false` and nothing else.
fn empty_query(q: &Select) -> Option<Vec<String>> {
    let plain = q.ctes.is_empty()
        && !q.for_json
        && q.set_ops.is_empty()
        && !q.distinct
        && q.top.is_none()
        && q.into.is_empty()
        && q.group_by.is_empty()
        && q.having.is_none()
        && q.order_by.is_empty()
        && q.offset.is_none()
        && q.limit.is_none()
        && q.locking.is_empty()
        && q.values.is_none()
        && matches!(
            q.items.as_slice(),
            [SelItem {
                expr: Expr::Star(None),
                alias: None,
                assign: None
            }]
        );
    let is_false = match &q.where_ {
        Some(Expr::Special(s)) => s.eq_ignore_ascii_case("FALSE"),
        Some(Expr::Name(p)) => matches!(p.as_slice(), [x] if x.eq_ignore_ascii_case("false")),
        _ => false,
    };
    match q.from.as_slice() {
        [From::Table {
            name,
            alias: None,
            hints,
        }] if plain && is_false && hints.is_empty() => Some(name.clone()),
        _ => None,
    }
}

/// Scope keys of the row images an OUTPUT or RETURNING item reads; no
/// name a statement can spell.
const NEW_IMAGE: &str = "\u{1}new";
const OLD_IMAGE: &str = "\u{1}old";

/// An OUTPUT / RETURNING item with every column it names rewritten to
/// `image.column` (`NEW_IMAGE` or `OLD_IMAGE`). `image` maps a qualifier
/// to its image; an unqualified name is a column of the changed row (`cur`)
/// in PostgreSQL when the table has it, and otherwise left for `resolve`
/// (a PL/pgSQL variable); a name that is both a column and a variable
/// (`var`) is refused, as PL/pgSQL raises "column reference is ambiguous"
/// for it. Subqueries, `*` and window functions are refused.
fn output_item(
    e: &Expr,
    d: Dialect,
    t: &super::schema::Table,
    cur: &str,
    image: &dyn Fn(&str) -> R<&'static str>,
    var: &dyn Fn(&str) -> bool,
) -> R<Expr> {
    let go = |x: &Expr| output_item(x, d, t, cur, image, var);
    let bx = |x: &Expr| go(x).map(Box::new);
    Ok(match e {
        Expr::Name(p) if p.len() == 1 => {
            if d == Dialect::Tsql {
                return Err("an unqualified column in OUTPUT (name inserted. or deleted.)".into());
            }
            if t.column(&p[0]).is_some() {
                if var(&p[0]) {
                    return Err(format!("`{}` names both a column and a variable", lc(&p[0])));
                }
                Expr::Name(vec![cur.to_string(), p[0].clone()])
            } else {
                e.clone()
            }
        }
        Expr::Name(p) => {
            let (col, qual) = p.split_last().expect("a qualified name");
            let q = qual.last().map(|x| lc(x)).unwrap_or_default();
            if qual.len() > 1 && d == Dialect::Tsql {
                return Err("a schema-qualified column in OUTPUT".into());
            }
            if qual.len() > 1 && !matches!(image(&q), Ok(i) if i == cur) {
                return Err(format!("`{}` in RETURNING", p.join(".")));
            }
            Expr::Name(vec![image(&q)?.to_string(), col.clone()])
        }
        Expr::Var(_) | Expr::Num(_) | Expr::Str(_) | Expr::Null | Expr::Special(_) => e.clone(),
        Expr::Call(c) if !c.over && c.order.is_empty() && !c.distinct => {
            let mut c2 = (**c).clone();
            c2.args = c.args.iter().map(go).collect::<R<_>>()?;
            Expr::Call(Box::new(c2))
        }
        Expr::Bin(op, x, y) => Expr::Bin(op.clone(), bx(x)?, bx(y)?),
        Expr::Unary(op, x) => Expr::Unary(op.clone(), bx(x)?),
        Expr::IsNull(x, b) => Expr::IsNull(bx(x)?, *b),
        Expr::Cast(x, ty) => Expr::Cast(bx(x)?, ty.clone()),
        Expr::Case(op, whens, els) => Expr::Case(
            op.as_deref().map(bx).transpose()?,
            whens
                .iter()
                .map(|(w, v)| Ok((go(w)?, go(v)?)))
                .collect::<R<_>>()?,
            els.as_deref().map(bx).transpose()?,
        ),
        Expr::Extract(f, x) => Expr::Extract(f.clone(), bx(x)?),
        _ => return Err("an OUTPUT or RETURNING item other than columns of the changed row, variables, constants and functions of them".into()),
    })
}

/// Whether `t` holds a node with head `h`.
fn has_head(t: &Term, h: &str) -> bool {
    t.is(h) || t.kids().iter().any(|k| has_head(k, h))
}

/// PostgreSQL's make_interval parameters in order (the parser writes named
/// arguments out by position); each one left out is zero.
const MAKE_INTERVAL_FIELDS: [&str; 7] =
    ["years", "months", "weeks", "days", "hours", "mins", "secs"];

fn is_make_interval(c: &Call) -> bool {
    let builtin = match c.name.as_slice() {
        [f] => f,
        [s, f] if s.eq_ignore_ascii_case("pg_catalog") => f,
        _ => return false,
    };
    builtin.eq_ignore_ascii_case("make_interval")
        && (1..=MAKE_INTERVAL_FIELDS.len()).contains(&c.args.len())
        && !c.distinct
        && c.order.is_empty()
        && !c.star
        && !c.over
        && !c.within_group
}

fn is_zero_literal(e: &Expr) -> bool {
    matches!(e, Expr::Num(s) if s.parse::<f64>().is_ok_and(|v| v == 0.0))
}

/// The make_interval field a T-SQL DATEADD datepart adds; the parts whose
/// amounts round (milliseconds and smaller) and quarters have none.
fn dateadd_field(e: &Expr) -> Option<&'static str> {
    let Expr::Name(p) = e else {
        return None;
    };
    let [part] = p.as_slice() else {
        return None;
    };
    // DATEADD adds days for dayofyear and weekday, as for day.
    Some(match lc(part).as_str() {
        "year" | "yy" | "yyyy" => "years",
        "month" | "mm" | "m" => "months",
        "week" | "wk" | "ww" => "weeks",
        "dayofyear" | "dy" | "y" | "day" | "dd" | "d" | "weekday" | "dw" | "w" => "days",
        "hour" | "hh" => "hours",
        "minute" | "mi" | "n" => "mins",
        "second" | "ss" | "s" => "secs",
        _ => return None,
    })
}

/// The value of an integer literal term (`num` or a negated one).
fn int_literal(t: &Term) -> Option<i64> {
    if t.is("neg") {
        return t.kid(0).and_then(int_literal).map(|v| -v);
    }
    if !t.is("num") {
        return None;
    }
    t.kid(0).and_then(Term::atom)?.parse().ok()
}
