//! Routine-level facts read straight from the syntax tree: durable writes
//! (for write pairing), transaction statements, handlers and the Db2 for i
//! commit level. The behavioural checks that used to live here run on the
//! semantic IR now (`crate::ir`).

use sqldiff_syntax::{ast, Dialect, SyntaxKind, SyntaxNode};

use crate::artifact::Analyzed;
use crate::classify::{is_temp_target, nested_write, stmt_kind, write_target, StmtKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutineKind {
    Procedure,
    Function,
    Trigger,
    Anonymous,
    Script,
}

#[derive(Debug, Clone)]
pub struct Write {
    pub node: SyntaxNode,
    pub kind: StmtKind,
    pub target: String,
    /// MERGE, or INSERT ... ON CONFLICT.
    pub upsert: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub routine: Option<RoutineKind>,
    /// Writes to tables that hold durable state (temporary tables excluded).
    pub writes: Vec<Write>,
    pub begins: Vec<SyntaxNode>,
    pub commits: Vec<SyntaxNode>,
    pub rollbacks: Vec<SyntaxNode>,
    pub error_rollbacks: Vec<SyntaxNode>,
    pub handlers: Vec<SyntaxNode>,
    pub atomic_blocks: Vec<SyntaxNode>,
    /// Db2 for i `COMMIT = *X` level and the token offset where it is set.
    pub commit_level: Option<(String, usize)>,
    pub xact_abort_on: Option<SyntaxNode>,
    /// `RETURN <non-zero>` on an error path, with the code.
    pub error_returns: Vec<(String, SyntaxNode)>,
    pub raises: Vec<SyntaxNode>,
}

pub fn extract(a: &Analyzed) -> Facts {
    let d = a.dialect();
    let mut f = Facts {
        routine: Some(RoutineKind::Script),
        ..Facts::default()
    };
    for node in a.root.descendants() {
        match node.kind() {
            SyntaxKind::ROUTINE => {
                let words = ast::first_words(&node, 4);
                f.routine = Some(if words.first().is_some_and(|w| w == "DO") {
                    RoutineKind::Anonymous
                } else if words.iter().any(|w| w == "FUNCTION") {
                    RoutineKind::Function
                } else if words.iter().any(|w| w == "TRIGGER") {
                    RoutineKind::Trigger
                } else {
                    RoutineKind::Procedure
                });
            }
            SyntaxKind::TRY_CATCH | SyntaxKind::HANDLER | SyntaxKind::EXCEPTION_SECTION => {
                f.handlers.push(node.clone())
            }
            SyntaxKind::IF_STMT if checks_error_status(&node) => f.handlers.push(node.clone()),
            SyntaxKind::BLOCK if block_is_atomic(&node) => f.atomic_blocks.push(node.clone()),
            SyntaxKind::STATEMENT => {
                let kind = stmt_kind(&node, d);
                let on_error = on_error_path(&node);
                match kind {
                    k if k.is_write() => {
                        if let Some(parts) = write_target(&node) {
                            if !is_temp_target(&parts) {
                                f.writes.push(Write {
                                    node: node.clone(),
                                    kind,
                                    target: parts.last().cloned().unwrap_or_default(),
                                    upsert: kind == StmtKind::Merge || has_on_conflict(&node),
                                });
                            }
                        }
                    }
                    StmtKind::Select => {
                        if let Some((kind, parts)) = nested_write(&node) {
                            if !is_temp_target(&parts) {
                                f.writes.push(Write {
                                    node: node.clone(),
                                    kind,
                                    target: parts.last().cloned().unwrap_or_default(),
                                    upsert: kind == StmtKind::Merge,
                                });
                            }
                        }
                    }
                    StmtKind::TxnBegin => f.begins.push(node.clone()),
                    StmtKind::TxnCommit => f.commits.push(node.clone()),
                    StmtKind::TxnRollback => {
                        if on_error {
                            f.error_rollbacks.push(node.clone());
                        }
                        f.rollbacks.push(node.clone());
                    }
                    StmtKind::TxnSetting => {
                        let w = ast::first_words(&node, 3);
                        if w.get(1).is_some_and(|x| x == "XACT_ABORT")
                            && w.get(2).is_some_and(|x| x == "ON")
                        {
                            f.xact_abort_on = Some(node.clone());
                        }
                    }
                    StmtKind::Return if on_error => {
                        if let Some(code) = return_code(&node) {
                            if code != "0" {
                                f.error_returns.push((code, node.clone()));
                            }
                        }
                    }
                    StmtKind::Raise => f.raises.push(node.clone()),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if d == Dialect::Db2i {
        f.commit_level = find_commit_level(a);
    }
    f
}

fn has_on_conflict(node: &SyntaxNode) -> bool {
    let words: Vec<String> = ast::sig_tokens(node)
        .map(|t| t.text().to_ascii_uppercase())
        .collect();
    words.windows(2).any(|w| w[0] == "ON" && w[1] == "CONFLICT")
}

fn block_is_atomic(node: &SyntaxNode) -> bool {
    let toks: Vec<String> = ast::header_tokens(node)
        .iter()
        .take(4)
        .map(|t| t.text().to_ascii_uppercase())
        .collect();
    let i = toks.iter().position(|t| t == "BEGIN");
    match i {
        Some(i) => toks.get(i + 1).is_some_and(|t| t == "ATOMIC"),
        None => false,
    }
}

/// `IF @@ERROR <> 0`, `IF @@TRANCOUNT > 0`, `IF XACT_STATE() ...`.
fn checks_error_status(if_stmt: &SyntaxNode) -> bool {
    if_stmt
        .children()
        .find(|c| c.kind() == SyntaxKind::CONDITION)
        .is_some_and(|c| {
            ast::sig_tokens(&c).any(|t| {
                let u = t.text().to_ascii_uppercase();
                u == "@@ERROR" || u == "XACT_STATE"
            })
        })
}

/// Whether a statement only runs after an error: in a CATCH block, a
/// handler, a PL/pgSQL exception handler, or the THEN branch of an
/// `IF @@ERROR ...` check.
fn on_error_path(node: &SyntaxNode) -> bool {
    let mut cur = node.parent();
    let mut child = node.clone();
    while let Some(p) = cur {
        match p.kind() {
            SyntaxKind::CATCH_BLOCK | SyntaxKind::HANDLER | SyntaxKind::EXCEPTION_HANDLER => {
                return true
            }
            SyntaxKind::IF_STMT if checks_error_status(&p) => {
                let first_branch = p.children().find(|c| c.kind() == SyntaxKind::BRANCH);
                if first_branch.as_ref() == Some(&child) {
                    return true;
                }
            }
            _ => {}
        }
        child = p.clone();
        cur = p.parent();
    }
    false
}

pub fn return_code(node: &SyntaxNode) -> Option<String> {
    let toks: Vec<_> = ast::sig_tokens(node)
        .skip(1)
        .filter(|t| t.kind() != SyntaxKind::SEMICOLON)
        .collect();
    match toks.as_slice() {
        [n] if n.kind() == SyntaxKind::NUMBER => Some(n.text().to_string()),
        [m, n] if m.text() == "-" && n.kind() == SyntaxKind::NUMBER => {
            Some(format!("-{}", n.text()))
        }
        _ => None,
    }
}

/// `COMMIT = *NONE` in a Db2 for i `SET OPTION` clause or statement.
pub fn find_commit_level(a: &Analyzed) -> Option<(String, usize)> {
    let toks: Vec<_> = ast::sig_tokens(&a.root).collect();
    for w in toks.windows(4) {
        if w[0].text().eq_ignore_ascii_case("COMMIT")
            && w[1].text() == "="
            && w[2].text() == "*"
            && w[3].kind() == SyntaxKind::IDENT
        {
            let level = format!("*{}", w[3].text().to_ascii_uppercase());
            return Some((level, w[0].text_range().start().into()));
        }
    }
    None
}
