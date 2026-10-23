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
        "WITH" => match dml_index(&toks).map(&w).as_deref() {
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

/// Name used to pair objects across dialects: last part, upper case, without `_`.
pub fn pairing_key(parts: &[String]) -> String {
    parts
        .last()
        .map(|p| p.to_uppercase().replace('_', ""))
        .unwrap_or_default()
}
