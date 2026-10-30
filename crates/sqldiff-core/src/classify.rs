//! Statement classification over the CST.

use sqldiff_syntax::{ast, Dialect, SyntaxKind, SyntaxNode, SyntaxToken};

use crate::model::ChangeClass;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StmtKind {
    Select,
    Insert,
    Update,
    Delete,
    Merge,
    Assign,
    SessionSetting,
    Declare,
    TxnBegin,
    TxnCommit,
    TxnRollback,
    Savepoint,
    TxnSetting,
    Return,
    Raise,
    Call,
    Ddl,
    Block,
    If,
    Loop,
    Case,
    Handler,
    TryCatch,
    ExceptionSection,
    Routine,
    Label,
    Container,
    Other,
}

impl StmtKind {
    pub fn label(self) -> &'static str {
        match self {
            StmtKind::Select => "SELECT",
            StmtKind::Insert => "INSERT",
            StmtKind::Update => "UPDATE",
            StmtKind::Delete => "DELETE",
            StmtKind::Merge => "MERGE",
            StmtKind::Assign => "assignment",
            StmtKind::SessionSetting => "session setting",
            StmtKind::Declare => "declaration",
            StmtKind::TxnBegin => "transaction start",
            StmtKind::TxnCommit => "COMMIT",
            StmtKind::TxnRollback => "ROLLBACK",
            StmtKind::Savepoint => "savepoint",
            StmtKind::TxnSetting => "transaction setting",
            StmtKind::Return => "RETURN",
            StmtKind::Raise => "error raise",
            StmtKind::Call => "call",
            StmtKind::Ddl => "DDL statement",
            StmtKind::Block => "block",
            StmtKind::If => "IF",
            StmtKind::Loop => "loop",
            StmtKind::Case => "CASE",
            StmtKind::Handler => "error handler",
            StmtKind::TryCatch => "TRY/CATCH",
            StmtKind::ExceptionSection => "EXCEPTION section",
            StmtKind::Routine => "routine",
            StmtKind::Label => "label",
            StmtKind::Container => "statement list",
            StmtKind::Other => "statement",
        }
    }

    pub fn class(self) -> ChangeClass {
        match self {
            StmtKind::TxnBegin
            | StmtKind::TxnCommit
            | StmtKind::TxnRollback
            | StmtKind::Savepoint
            | StmtKind::TxnSetting
            | StmtKind::Handler
            | StmtKind::TryCatch
            | StmtKind::ExceptionSection => ChangeClass::Transactional,
            StmtKind::Label | StmtKind::Container => ChangeClass::Structural,
            _ => ChangeClass::Semantic,
        }
    }

    pub fn is_write(self) -> bool {
        matches!(
            self,
            StmtKind::Insert | StmtKind::Update | StmtKind::Delete | StmtKind::Merge
        )
    }

    pub fn is_txn_control(self) -> bool {
        matches!(
            self,
            StmtKind::TxnBegin
                | StmtKind::TxnCommit
                | StmtKind::TxnRollback
                | StmtKind::Savepoint
                | StmtKind::TxnSetting
        )
    }

    /// Whether running or not running the statement is observable.
    pub fn is_effectful(self) -> bool {
        !matches!(
            self,
            StmtKind::Declare
                | StmtKind::Label
                | StmtKind::Container
                | StmtKind::Block
                | StmtKind::If
                | StmtKind::Loop
                | StmtKind::Case
                | StmtKind::Routine
                | StmtKind::Other
        )
    }
}

fn word(t: &SyntaxToken) -> Option<String> {
    (t.kind() == SyntaxKind::IDENT).then(|| t.text().to_ascii_uppercase())
}

/// Index of the first DML keyword at paren depth 0 (skips a leading CTE).
fn dml_index(toks: &[SyntaxToken]) -> Option<usize> {
    let mut depth = 0i32;
    for (i, t) in toks.iter().enumerate() {
        match t.kind() {
            SyntaxKind::L_PAREN => depth += 1,
            SyntaxKind::R_PAREN => depth -= 1,
            SyntaxKind::IDENT if depth == 0 => {
                if let Some(w) = word(t) {
                    if matches!(
                        w.as_str(),
                        "SELECT" | "INSERT" | "UPDATE" | "DELETE" | "MERGE"
                    ) {
                        return Some(i);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

pub fn stmt_kind(node: &SyntaxNode, dialect: Dialect) -> StmtKind {
    match node.kind() {
        SyntaxKind::BLOCK => return StmtKind::Block,
        SyntaxKind::IF_STMT => return StmtKind::If,
        SyntaxKind::LOOP_STMT => return StmtKind::Loop,
        SyntaxKind::CASE_STMT => return StmtKind::Case,
        SyntaxKind::HANDLER | SyntaxKind::CATCH_BLOCK | SyntaxKind::EXCEPTION_HANDLER => {
            return StmtKind::Handler
        }
        SyntaxKind::TRY_CATCH => return StmtKind::TryCatch,
        SyntaxKind::EXCEPTION_SECTION => return StmtKind::ExceptionSection,
        SyntaxKind::ROUTINE => return StmtKind::Routine,
        SyntaxKind::LABEL => return StmtKind::Label,
        SyntaxKind::STATEMENT => {}
        _ => return StmtKind::Container,
    }
    let toks: Vec<SyntaxToken> = ast::sig_tokens(node).collect();
    let w = |i: usize| toks.get(i).and_then(word).unwrap_or_default();
    let w0 = w(0);
    let w1 = w(1);
    if toks.get(1).is_some_and(|t| t.text() == ":=") {
        return StmtKind::Assign;
    }
    match w0.as_str() {
        "SELECT" | "VALUES" => StmtKind::Select,
        "WITH" => match dml_index(&toks).map(w).as_deref() {
            Some("INSERT") => StmtKind::Insert,
            Some("UPDATE") => StmtKind::Update,
            Some("DELETE") => StmtKind::Delete,
            Some("MERGE") => StmtKind::Merge,
            _ => StmtKind::Select,
        },
        "INSERT" => StmtKind::Insert,
        "UPDATE" => StmtKind::Update,
        "DELETE" => StmtKind::Delete,
        "MERGE" => StmtKind::Merge,
        "SET" => match w1.as_str() {
            "XACT_ABORT" | "IMPLICIT_TRANSACTIONS" | "TRANSACTION" => StmtKind::TxnSetting,
            "OPTION" if toks.iter().any(|t| t.text().eq_ignore_ascii_case("COMMIT")) => {
                StmtKind::TxnSetting
            }
            _ if dialect == Dialect::Tsql
                && toks
                    .get(1)
                    .is_some_and(|t| t.kind() == SyntaxKind::VARIABLE) =>
            {
                StmtKind::Assign
            }
            _ if dialect == Dialect::Db2i => StmtKind::Assign,
            _ => StmtKind::SessionSetting,
        },
        "DECLARE" => StmtKind::Declare,
        "BEGIN" | "START" => StmtKind::TxnBegin,
        "COMMIT" | "END" => StmtKind::TxnCommit,
        "ROLLBACK" | "ABORT" => StmtKind::TxnRollback,
        "SAVE" | "SAVEPOINT" | "RELEASE" => StmtKind::Savepoint,
        "RETURN" => StmtKind::Return,
        "THROW" | "RAISERROR" | "SIGNAL" | "RESIGNAL" => StmtKind::Raise,
        "RAISE" => {
            if matches!(w1.as_str(), "DEBUG" | "LOG" | "INFO" | "NOTICE" | "WARNING") {
                StmtKind::Other
            } else {
                StmtKind::Raise
            }
        }
        "EXEC" | "EXECUTE" | "CALL" | "PERFORM" => StmtKind::Call,
        "CREATE" | "ALTER" | "DROP" | "TRUNCATE" | "RENAME" | "COMMENT" | "GRANT" | "REVOKE" => {
            StmtKind::Ddl
        }
        _ => StmtKind::Other,
    }
}

/// Target object of a write statement, as unquoted name parts.
pub fn write_target(node: &SyntaxNode) -> Option<Vec<String>> {
    let toks: Vec<SyntaxToken> = ast::sig_tokens(node).collect();
    let mut i = dml_index(&toks)?;
    i += 1;
    while let Some(t) = toks.get(i) {
        match word(t).as_deref() {
            Some("INTO" | "FROM" | "ONLY") => i += 1,
            Some("TOP") => {
                i += 1;
                if toks.get(i).is_some_and(|t| t.kind() == SyntaxKind::L_PAREN) {
                    let mut depth = 0;
                    while let Some(t) = toks.get(i) {
                        match t.kind() {
                            SyntaxKind::L_PAREN => depth += 1,
                            SyntaxKind::R_PAREN => depth -= 1,
                            _ => {}
                        }
                        i += 1;
                        if depth == 0 {
                            break;
                        }
                    }
                } else {
                    i += 1;
                }
            }
            _ => break,
        }
    }
    let parts = ast::qualified_name_at(&toks, i);
    if parts.len() == 1 {
        if let Some(real) = resolve_alias(&toks, i + 1, &parts[0]) {
            return Some(real);
        }
    }
    (!parts.is_empty()).then_some(parts)
}

/// A statement's data-modifying CTEs (kind and target) and its OUTPUT … INTO
/// tables.
pub type SideWrites = (Vec<(StmtKind, Vec<String>)>, Vec<Vec<String>>);

/// Writes a statement makes besides its own: the data-modifying CTEs of
/// `WITH o AS (INSERT|UPDATE|DELETE …) …`, which run first, and the table
/// of a T-SQL `OUTPUT … INTO t`, written after.
pub fn side_writes(node: &SyntaxNode) -> SideWrites {
    let toks: Vec<SyntaxToken> = ast::sig_tokens(node).collect();
    let w = |i: usize| toks.get(i).and_then(word);
    let close = |open: usize| {
        let mut depth = 0;
        for (j, t) in toks.iter().enumerate().skip(open) {
            match t.kind() {
                SyntaxKind::L_PAREN => depth += 1,
                SyntaxKind::R_PAREN => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(j);
                    }
                }
                _ => {}
            }
        }
        None
    };
    let target_at = |mut i: usize| {
        while matches!(w(i).as_deref(), Some("INTO" | "FROM" | "ONLY")) {
            i += 1;
        }
        ast::qualified_name_at(&toks, i)
    };
    let mut before = Vec::new();
    if w(0).as_deref() == Some("WITH") {
        let mut i = 1;
        if w(i).as_deref() == Some("RECURSIVE") {
            i += 1;
        }
        loop {
            i += 1; // the name
            if toks.get(i).is_some_and(|t| t.kind() == SyntaxKind::L_PAREN) {
                match close(i) {
                    Some(j) => i = j + 1,
                    None => break,
                }
            }
            if w(i).as_deref() != Some("AS") {
                break;
            }
            i += 1;
            while matches!(w(i).as_deref(), Some("NOT" | "MATERIALIZED")) {
                i += 1;
            }
            if !toks.get(i).is_some_and(|t| t.kind() == SyntaxKind::L_PAREN) {
                break;
            }
            let Some(j) = close(i) else { break };
            let kind = match w(i + 1).as_deref() {
                Some("INSERT") => Some(StmtKind::Insert),
                Some("UPDATE") => Some(StmtKind::Update),
                Some("DELETE") => Some(StmtKind::Delete),
                Some("MERGE") => Some(StmtKind::Merge),
                _ => None,
            };
            if let Some(kind) = kind {
                let parts = target_at(i + 2);
                if !parts.is_empty() {
                    before.push((kind, parts));
                }
            }
            if !toks
                .get(j + 1)
                .is_some_and(|t| t.kind() == SyntaxKind::COMMA)
            {
                break;
            }
            i = j + 2;
        }
    }
    let mut after = Vec::new();
    let mut depth = 0i32;
    let mut output = false;
    for (i, t) in toks.iter().enumerate() {
        match t.kind() {
            SyntaxKind::L_PAREN => depth += 1,
            SyntaxKind::R_PAREN => depth -= 1,
            _ if depth == 0 => match word(t).as_deref() {
                Some("OUTPUT") => output = true,
                Some("INTO") if output => {
                    let parts = ast::qualified_name_at(&toks, i + 1);
                    if !parts.is_empty() {
                        after.push(parts);
                    }
                    break;
                }
                _ => {}
            },
            _ => {}
        }
    }
    (before, after)
}

/// T-SQL `DELETE a FROM t a` / `UPDATE a SET .. FROM t AS a`: the target is an alias.
fn resolve_alias(toks: &[SyntaxToken], from: usize, alias: &str) -> Option<Vec<String>> {
    let mut i = from;
    while i < toks.len() {
        let t = &toks[i];
        let is_from = matches!(word(t).as_deref(), Some("FROM" | "JOIN"));
        i += 1;
        if !is_from {
            continue;
        }
        let name = ast::qualified_name_at(toks, i);
        if name.is_empty() {
            continue;
        }
        // Skip the name tokens and separators.
        let mut j = i + name.len() * 2 - 1;
        if toks.get(j).and_then(word).as_deref() == Some("AS") {
            j += 1;
        }
        if toks
            .get(j)
            .is_some_and(|t| t.text().eq_ignore_ascii_case(alias))
        {
            return Some(name);
        }
    }
    None
}

/// Db2 data-change table reference: `SELECT .. FROM FINAL TABLE (INSERT INTO t ..)`.
pub fn nested_write(node: &SyntaxNode) -> Option<(StmtKind, Vec<String>)> {
    let toks: Vec<SyntaxToken> = ast::sig_tokens(node).collect();
    for i in 0..toks.len().saturating_sub(3) {
        let w0 = word(&toks[i]);
        if !matches!(w0.as_deref(), Some("FINAL" | "NEW" | "OLD"))
            || word(&toks[i + 1]).as_deref() != Some("TABLE")
            || toks[i + 2].kind() != SyntaxKind::L_PAREN
        {
            continue;
        }
        let kind = match word(&toks[i + 3]).as_deref() {
            Some("INSERT") => StmtKind::Insert,
            Some("UPDATE") => StmtKind::Update,
            Some("DELETE") => StmtKind::Delete,
            Some("MERGE") => StmtKind::Merge,
            _ => continue,
        };
        let mut j = i + 4;
        while matches!(
            word(&toks[j.min(toks.len() - 1)]).as_deref(),
            Some("INTO" | "FROM")
        ) {
            j += 1;
        }
        let parts = ast::qualified_name_at(&toks, j);
        if !parts.is_empty() {
            return Some((kind, parts));
        }
    }
    None
}

/// Temporary tables and table variables do not hold durable state.
pub fn is_temp_target(parts: &[String]) -> bool {
    let last = parts.last().map(String::as_str).unwrap_or("");
    last.starts_with('#')
        || last.starts_with('@')
        || parts
            .first()
            .is_some_and(|p| p.eq_ignore_ascii_case("SESSION"))
        || parts
            .first()
            .is_some_and(|p| p.eq_ignore_ascii_case("pg_temp"))
}

/// The temporary table a T-SQL table variable becomes when converted
/// (PostgreSQL `pg_temp.<name>`, Db2 for i `SESSION.<name>`). The converter
/// writes it and the engine expects it, so the two pair up.
///
/// Every routine that can be active in one session (a routine and its
/// callers, transitively) creates its tables in the one session-wide
/// temporary schema, and the conversion drops and creates a table where the
/// DECLARE stands. So the name must be injective over (routine, variable):
/// a called routine's DROP TABLE IF EXISTS must never hit its caller's
/// table, whatever the two are called.
///
/// `tv__<part>__…__<part>__<variable>`: the routine's name parts as its
/// header writes them (schema and name, unquoted; `dbo.ab` and `dbo.a_b`,
/// `app.p` and `dbo.p` are different routines) and the variable, lower-cased
/// and joined with `__`. That is the name when every part is *tidy* (only
/// `a-z`, `0-9` and `_`, with no leading, trailing or doubled underscore)
/// and it fits PostgreSQL's 63 bytes. A tidy part holds no `__`, so the
/// `__` runs of the name are exactly its separators and the name reads back
/// to its parts: two different keys give two different names. A script's
/// `tv__<variable>` has one part, a routine's at least two.
///
/// Otherwise the parts are sanitised (every other character becomes `_`),
/// the name is cut to 52 bytes, and `___` and the 32-bit FNV-1a hash of the
/// key `<parts joined by '.'>@<variable>` (lower case) follow as eight hex
/// digits. A tidy name never holds three underscores in a row, so the two
/// forms cannot meet, and two keys with different hashes get different
/// names whatever was cut. The hash is stable across builds and platforms
/// and ASCII, so the name is the same wherever it is computed. Two keys
/// with the same hash and the same sanitised text would collide: the
/// converter refuses the later of two such routines in a tree.
pub fn table_variable_table(routine: Option<&str>, variable: &str) -> String {
    let mut parts: Vec<String> = routine.map(name_parts).unwrap_or_default();
    parts.push(variable.trim_start_matches('@').to_lowercase());
    let ok = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_';
    let tidy = |p: &str| {
        !p.is_empty()
            && !p.starts_with('_')
            && !p.ends_with('_')
            && !p.contains("__")
            && p.bytes().all(ok)
    };
    let plain = format!("tv__{}", parts.join("__"));
    if parts.iter().all(|p| tidy(p)) && plain.len() <= 63 {
        return plain;
    }
    let (var, routine) = parts.split_last().expect("the variable is a part");
    let key = format!("{}@{var}", routine.join("."));
    let sanitised: Vec<String> = parts
        .iter()
        .map(|p| {
            p.bytes()
                .map(|b| if ok(b) { b as char } else { '_' })
                .collect()
        })
        .collect();
    let readable = format!("tv__{}", sanitised.join("__"));
    // All ASCII now, so any cut is a character boundary.
    let cut = readable.len().min(52);
    format!("{}___{:08x}", &readable[..cut], fnv1a(&key))
}

/// A routine name's parts, lower-cased and unquoted: `[dbo].[Command_Execute]`
/// gives `dbo`, `command_execute`. Dots inside brackets or double quotes do
/// not split.
fn name_parts(routine: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in routine.chars() {
        match (quote, c) {
            (None, '[') => quote = Some(']'),
            (None, '"') => quote = Some('"'),
            (Some(q), c) if c == q => quote = None,
            (None, '.') => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .map(|p| p.to_lowercase())
        .collect()
}

/// Lower-case names of the temporary tables `root` names as `pg_temp.x` or
/// `SESSION.x`: where a converted T-SQL table variable may have gone.
pub fn temp_table_names(root: &SyntaxNode) -> std::collections::HashSet<String> {
    let toks: Vec<SyntaxToken> = ast::sig_tokens(root).collect();
    toks.windows(3)
        .filter(|w| {
            (w[0].text().eq_ignore_ascii_case("pg_temp")
                || w[0].text().eq_ignore_ascii_case("SESSION"))
                && w[1].kind() == SyntaxKind::DOT
                && w[2].kind() == SyntaxKind::IDENT
        })
        .map(|w| w[2].text().to_lowercase())
        .collect()
}

/// A T-SQL ROLLBACK in `node`, other than in a CATCH that only rolls back
/// and rethrows (`IF @@TRANCOUNT > 0 ROLLBACK; THROW;`): that error ends the
/// routine, and its table variables with it. A PostgreSQL ROLLBACK empties
/// the temporary table a table variable became, where T-SQL keeps the rows.
pub fn rolls_back(node: &SyntaxNode) -> bool {
    node.descendants()
        .filter(|n| n.kind() == SyntaxKind::STATEMENT)
        .filter(|n| ast::starts_with_kw(n, "ROLLBACK"))
        .any(|n| {
            !n.ancestors()
                .any(|a| a.kind() == SyntaxKind::CATCH_BLOCK && rollback_rethrow(&a))
        })
}

/// A T-SQL ROLLBACK that its PostgreSQL conversion runs: one outside TRY
/// and CATCH. The converter refuses transaction control inside either (a
/// block with an EXCEPTION clause is a subtransaction), and the
/// rollback-and-rethrow CATCH becomes the error itself.
pub fn pg_rolls_back(node: &SyntaxNode) -> bool {
    node.descendants()
        .filter(|n| n.kind() == SyntaxKind::STATEMENT)
        .filter(|n| ast::starts_with_kw(n, "ROLLBACK"))
        .any(|n| {
            !n.ancestors()
                .any(|a| matches!(a.kind(), SyntaxKind::TRY_BLOCK | SyntaxKind::CATCH_BLOCK))
        })
}

/// CATCH body that is only `IF @@TRANCOUNT > 0 ROLLBACK [TRAN]` and `THROW;`.
pub fn rollback_rethrow(catch: &SyntaxNode) -> bool {
    let words: Vec<String> = ast::sig_tokens(catch)
        .map(|t| t.text().to_ascii_uppercase())
        .filter(|w| {
            !matches!(
                w.as_str(),
                ";" | "BEGIN" | "END" | "CATCH" | "TRAN" | "TRANSACTION"
            )
        })
        .collect();
    let w: Vec<&str> = words.iter().map(String::as_str).collect();
    matches!(
        w.as_slice(),
        ["IF", "@@TRANCOUNT", ">", "0", "ROLLBACK", "THROW"] | ["ROLLBACK", "THROW"]
    )
}

/// 32-bit FNV-1a: stable across builds and platforms, unlike `DefaultHasher`.
fn fnv1a(s: &str) -> u32 {
    s.bytes().fold(0x811c_9dc5, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

/// Name used to pair objects across dialects: last part, upper case, without `_`.
pub fn pairing_key(parts: &[String]) -> String {
    parts
        .last()
        .map(|p| p.to_uppercase().replace('_', ""))
        .unwrap_or_default()
}
