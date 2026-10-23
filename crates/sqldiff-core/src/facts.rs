//! Routine-level transaction and error facts, and the behavioural checks
//! built on them (design section 7: error handling, transactions, statement
//! atomicity). This is a summary-level precursor of the CFG-based
//! behavioural engine: it compares what happens when a later write fails.

use sqldiff_syntax::{ast, Dialect, SyntaxKind, SyntaxNode};

use crate::artifact::Analyzed;
use crate::classify::{is_temp_target, nested_write, stmt_kind, write_target, StmtKind};
use crate::model::{
    fingerprint, Assumption, ChangeClass, Finding, FingerprintInput, Severity, Verdict,
};

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

fn return_code(node: &SyntaxNode) -> Option<String> {
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
fn find_commit_level(a: &Analyzed) -> Option<(String, usize)> {
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

/// What happens to earlier writes when a later write fails.
#[derive(Debug, Clone)]
pub enum Atomicity {
    Atomic {
        evidence: String,
        assumptions: Vec<Assumption>,
    },
    NotAtomic {
        evidence: String,
        assumptions: Vec<Assumption>,
    },
    CallerDependent {
        evidence: String,
        assumptions: Vec<Assumption>,
    },
    Unknown {
        reason: String,
        assumptions: Vec<Assumption>,
    },
}

impl Atomicity {
    fn parts(&self) -> (&str, &[Assumption]) {
        match self {
            Atomicity::Atomic {
                evidence,
                assumptions,
            }
            | Atomicity::NotAtomic {
                evidence,
                assumptions,
            }
            | Atomicity::CallerDependent {
                evidence,
                assumptions,
            }
            | Atomicity::Unknown {
                reason: evidence,
                assumptions,
            } => (evidence, assumptions),
        }
    }
}

fn asm_caller_xact_abort() -> Assumption {
    Assumption::default(
        "ASM-TSQL-CALLER-XACT-ABORT-OFF",
        "The caller's XACT_ABORT setting is OFF",
    )
}

fn asm_no_triggers() -> Assumption {
    Assumption::default(
        "ASM-NO-TRIGGERS",
        "No triggers on the written tables change the error or transaction outcome",
    )
}

fn is_db2_no_commit(level: &str) -> bool {
    matches!(level, "*NONE" | "*NC")
}

pub fn atomicity(a: &Analyzed, f: &Facts) -> Atomicity {
    let line = |n: &SyntaxNode| format!("{:?} line {}", a.side, a.line_of(n));
    match a.dialect() {
        Dialect::Tsql => {
            if let Some(b) = f.begins.first() {
                if let Some(x) = &f.xact_abort_on {
                    Atomicity::Atomic {
                        evidence: format!(
                            "SET XACT_ABORT ON ({}) with BEGIN TRAN ({})",
                            line(x),
                            line(b)
                        ),
                        assumptions: vec![],
                    }
                } else if let Some(r) = f.error_rollbacks.first() {
                    Atomicity::Atomic {
                        evidence: format!("ROLLBACK on the error path at {}", line(r)),
                        assumptions: vec![asm_caller_xact_abort()],
                    }
                } else {
                    Atomicity::NotAtomic {
                        evidence: format!(
                            "BEGIN TRAN at {} without XACT_ABORT or an error-path ROLLBACK: a failed statement is rolled back on its own and execution continues",
                            line(b)
                        ),
                        assumptions: vec![asm_caller_xact_abort()],
                    }
                }
            } else {
                Atomicity::NotAtomic {
                    evidence: "no explicit transaction, so each statement commits on its own"
                        .to_string(),
                    assumptions: vec![
                        asm_caller_xact_abort(),
                        Assumption::default(
                            "ASM-NO-CALLER-TXN",
                            "The caller does not hold an open transaction",
                        ),
                    ],
                }
            }
        }
        Dialect::Db2i => {
            if let Some((level, off)) = &f.commit_level {
                if is_db2_no_commit(level) {
                    let l = format!("{:?} line {}", a.side, a.lines.line_col(*off).0);
                    return Atomicity::NotAtomic {
                        evidence: format!("COMMIT = {level} at {l}: no commitment control, every change is durable immediately"),
                        assumptions: vec![],
                    };
                }
            }
            if let Some(b) = f.atomic_blocks.first() {
                return Atomicity::Atomic {
                    evidence: format!("ATOMIC compound statement at {}", line(b)),
                    assumptions: vec![],
                };
            }
            if let Some(r) = f.error_rollbacks.first() {
                return Atomicity::Atomic {
                    evidence: format!("ROLLBACK in a handler at {}", line(r)),
                    assumptions: vec![],
                };
            }
            match &f.commit_level {
                Some((level, _)) => Atomicity::CallerDependent {
                    evidence: format!(
                        "commitment control (COMMIT = {level}) without an ATOMIC block or handler: earlier changes stay uncommitted and are undone only if the caller rolls back"
                    ),
                    assumptions: vec![Assumption::default(
                        "ASM-DB2-CALLER-ROLLBACK",
                        "The caller rolls back its unit of work after an error",
                    )],
                },
                None => Atomicity::Unknown {
                    reason: "the commit level is not declared and the default depends on how the routine is created (fact V5)".to_string(),
                    assumptions: vec![Assumption::default(
                        "ASM-DB2-COMMIT-LEVEL",
                        "The routine's commit level (SET OPTION COMMIT) as created",
                    )],
                },
            }
        }
        Dialect::Postgres => {
            let has_exception = a
                .root
                .descendants()
                .any(|n| n.kind() == SyntaxKind::EXCEPTION_SECTION);
            match f.routine {
                Some(RoutineKind::Function | RoutineKind::Trigger) => {
                    if has_exception {
                        Atomicity::Unknown {
                            reason: "EXCEPTION blocks are subtransactions; the rollback scope needs control-flow analysis".to_string(),
                            assumptions: vec![],
                        }
                    } else {
                        Atomicity::Atomic {
                            evidence: "a PostgreSQL function runs inside the calling statement, and an unhandled error aborts it with all its changes".to_string(),
                            assumptions: vec![],
                        }
                    }
                }
                Some(RoutineKind::Procedure | RoutineKind::Anonymous) => {
                    // Only a COMMIT before the last write makes part of the work durable.
                    let last_write = f.writes.iter().map(|w| w.node.text_range().start()).max();
                    let early_commit = f
                        .commits
                        .iter()
                        .find(|c| last_write.is_some_and(|lw| c.text_range().start() < lw));
                    if let Some(c) = early_commit {
                        Atomicity::NotAtomic {
                            evidence: format!(
                                "COMMIT inside the routine at {} makes earlier work durable",
                                line(c)
                            ),
                            assumptions: vec![],
                        }
                    } else if has_exception {
                        Atomicity::Unknown {
                            reason: "EXCEPTION blocks are subtransactions; the rollback scope needs control-flow analysis".to_string(),
                            assumptions: vec![],
                        }
                    } else {
                        Atomicity::Atomic {
                            evidence: "the CALL runs in one transaction, and an unhandled error aborts it with all its changes".to_string(),
                            assumptions: vec![],
                        }
                    }
                }
                _ => {
                    if let Some(b) = f.begins.first() {
                        Atomicity::Atomic {
                            evidence: format!(
                                "explicit transaction block from {}: after an error PostgreSQL rejects further statements and COMMIT rolls back",
                                line(b)
                            ),
                            assumptions: vec![],
                        }
                    } else {
                        Atomicity::NotAtomic {
                            evidence: "no transaction block, so each statement commits on its own"
                                .to_string(),
                            assumptions: vec![],
                        }
                    }
                }
            }
        }
    }
}

fn write_phrase(w: &Write) -> String {
    match w.kind {
        StmtKind::Insert => format!("the INSERT into {}", w.target),
        StmtKind::Update => format!("the UPDATE of {}", w.target),
        StmtKind::Delete => format!("the DELETE from {}", w.target),
        StmtKind::Merge => format!("the MERGE into {}", w.target),
        _ => format!("the write to {}", w.target),
    }
}

fn side_name(a: &Analyzed) -> String {
    format!("side {:?} ({})", a.side, a.dialect().display_name())
}

struct Builder<'x, 'a> {
    a: &'x Analyzed<'a>,
    b: &'x Analyzed<'a>,
    out: Vec<Finding>,
    assumptions: Vec<Assumption>,
}

impl Builder<'_, '_> {
    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        rule: &str,
        class: ChangeClass,
        severity: Severity,
        verdict: Verdict,
        witness: String,
        na: Option<&SyntaxNode>,
        nb: Option<&SyntaxNode>,
        assumptions: Vec<Assumption>,
    ) {
        let ids = assumptions.iter().map(|x| x.id.clone()).collect();
        for x in assumptions {
            if !self.assumptions.iter().any(|y| y.id == x.id) {
                self.assumptions.push(x);
            }
        }
        let anchor = |n: Option<&SyntaxNode>| {
            n.map(|n| ast::first_words(n, 3).join(" "))
                .unwrap_or_default()
        };
        self.out.push(Finding {
            fingerprint: fingerprint(&FingerprintInput {
                object: &self.a.object,
                class,
                rule,
                anchor_a: &anchor(na),
                anchor_b: &anchor(nb),
            }),
            rule: rule.to_string(),
            class,
            severity,
            verdict,
            witness,
            a: na.map(|n| self.a.node_location(n)),
            b: nb.map(|n| self.b.node_location(n)),
            assumptions: ids,
        });
    }
}

/// Behavioural findings from comparing the facts of both sides.
pub fn compare(a: &Analyzed, b: &Analyzed) -> (Vec<Finding>, Vec<Assumption>) {
    let fa = extract(a);
    let fb = extract(b);
    let mut bld = Builder {
        a,
        b,
        out: Vec::new(),
        assumptions: Vec::new(),
    };
    check_atomicity(&mut bld, &fa, &fb);
    check_statement_atomicity(&mut bld, &fa, &fb);
    check_caller_transaction(&mut bld, &fa, &fb);
    check_error_contract(&mut bld, &fa, &fb);
    (bld.out, bld.assumptions)
}

fn check_atomicity(bld: &mut Builder, fa: &Facts, fb: &Facts) {
    if fa.writes.len() < 2 || fb.writes.len() < 2 {
        return;
    }
    let (a, b) = (bld.a, bld.b);
    let ta = atomicity(a, fa);
    let tb = atomicity(b, fb);
    use Atomicity::*;
    let (atomic_is_a, other) = match (&ta, &tb) {
        (Atomic { .. }, NotAtomic { .. } | CallerDependent { .. } | Unknown { .. }) => (true, &tb),
        (NotAtomic { .. } | CallerDependent { .. } | Unknown { .. }, Atomic { .. }) => (false, &ta),
        _ => return,
    };
    let (atomic_side, other_side, fo) = if atomic_is_a { (a, b, fb) } else { (b, a, fa) };
    let (atomic_ev, atomic_asm) = if atomic_is_a { ta.parts() } else { tb.parts() };
    let (other_ev, other_asm) = other.parts();
    let first = &fo.writes[0];
    let second = &fo.writes[1];
    let scenario = write_phrase(second);
    let mut assumptions: Vec<Assumption> = atomic_asm.iter().chain(other_asm).cloned().collect();
    assumptions.push(asm_no_triggers());
    let (na, nb) = if atomic_is_a {
        (None, Some(&second.node))
    } else {
        (Some(&second.node), None)
    };
    let (verdict, other_outcome) = match other {
        NotAtomic { .. } => (
            Verdict::Different,
            format!(
                "exits with {} ({:?} line {}) already durable ({other_ev})",
                write_phrase(first),
                other_side.side,
                other_side.line_of(&first.node)
            ),
        ),
        CallerDependent { .. } => (
            Verdict::Unknown,
            format!(
                "leaves {} ({:?} line {}) to the caller ({other_ev})",
                write_phrase(first),
                other_side.side,
                other_side.line_of(&first.node)
            ),
        ),
        _ => (
            Verdict::Unknown,
            format!(
                "may keep {} ({:?} line {}): {other_ev}",
                write_phrase(first),
                other_side.side,
                other_side.line_of(&first.node)
            ),
        ),
    };
    let witness = format!(
        "If {scenario} fails ({:?} line {}), {} undoes the earlier changes ({atomic_ev}); {} {other_outcome}.",
        other_side.side,
        other_side.line_of(&second.node),
        side_name(atomic_side),
        side_name(other_side),
    );
    bld.push(
        "txn.atomicity",
        ChangeClass::Transactional,
        Severity::Critical,
        verdict,
        witness,
        na,
        nb,
        assumptions,
    );
}

/// Under Db2 for i `COMMIT = *NONE`, a single failing statement is not atomic.
fn check_statement_atomicity(bld: &mut Builder, fa: &Facts, fb: &Facts) {
    let (a, b) = (bld.a, bld.b);
    for (side, f, other_side, of) in [(b, fb, a, fa), (a, fa, b, fb)] {
        let Some((level, _)) = &f.commit_level else {
            continue;
        };
        if !is_db2_no_commit(level) {
            continue;
        }
        if other_side.dialect() == Dialect::Db2i
            && of
                .commit_level
                .as_ref()
                .is_some_and(|(l, _)| is_db2_no_commit(l))
        {
            continue;
        }
        let Some(w) = f.writes.iter().find(|w| {
            matches!(
                w.kind,
                StmtKind::Update | StmtKind::Delete | StmtKind::Merge
            )
        }) else {
            continue;
        };
        let witness = format!(
            "If {} at line {} fails part-way through its rows, {} rolls the statement back; {} keeps the rows already changed, because statements are atomic only under commitment control (COMMIT = {level}).",
            write_phrase(w),
            side.line_of(&w.node),
            side_name(other_side),
            side_name(side),
        );
        let (na, nb) = if side.side == crate::model::Side::A {
            (Some(&w.node), None)
        } else {
            (None, Some(&w.node))
        };
        bld.push(
            "txn.statement-atomicity",
            ChangeClass::Transactional,
            Severity::High,
            Verdict::Different,
            witness,
            na,
            nb,
            vec![Assumption::default(
                "ASM-MULTI-ROW",
                "The statement can affect several rows",
            )],
        );
        break;
    }
}

/// A T-SQL BEGIN TRAN/COMMIT nested in a caller's transaction vs Db2 `*NONE`.
fn check_caller_transaction(bld: &mut Builder, fa: &Facts, fb: &Facts) {
    let (a, b) = (bld.a, bld.b);
    for (ts, ft, other, fo, ts_is_a) in [(a, fa, b, fb, true), (b, fb, a, fa, false)] {
        if ts.dialect() != Dialect::Tsql || other.dialect() != Dialect::Db2i {
            continue;
        }
        let (Some(_), Some(commit)) = (ft.begins.first(), ft.commits.first()) else {
            continue;
        };
        let Some((level, _)) = &fo.commit_level else {
            continue;
        };
        if !is_db2_no_commit(level) {
            continue;
        }
        let rollback = ft
            .rollbacks
            .first()
            .map(|r| {
                format!(
                    ", and its ROLLBACK (line {}) would undo the caller's entire transaction",
                    ts.line_of(r)
                )
            })
            .unwrap_or_default();
        let witness = format!(
            "If the caller already holds an open transaction, the COMMIT on {} (line {}) only decrements @@TRANCOUNT, so the caller can still roll the work back{rollback}; {} runs under COMMIT = {level}, so its changes are durable immediately and the caller cannot roll them back.",
            side_name(ts),
            ts.line_of(commit),
            side_name(other),
        );
        let (na, nb) = if ts_is_a {
            (Some(commit), None)
        } else {
            (None, Some(commit))
        };
        bld.push(
            "txn.caller-transaction",
            ChangeClass::Transactional,
            Severity::Critical,
            Verdict::Different,
            witness,
            na,
            nb,
            vec![Assumption::default(
                "ASM-CALLER-TXN-UNKNOWN",
                "Whether callers hold an open transaction is unknown",
            )],
        );
        return;
    }
}

/// Failure reported by return code on one side, propagated as an error on the other.
fn check_error_contract(bld: &mut Builder, fa: &Facts, fb: &Facts) {
    let (a, b) = (bld.a, bld.b);
    for (rc_side, frc, other, fo, rc_is_a) in [(a, fa, b, fb, true), (b, fb, a, fa, false)] {
        if frc.error_returns.is_empty() || !fo.handlers.is_empty() || !fo.error_returns.is_empty() {
            continue;
        }
        let mut codes: Vec<String> = frc.error_returns.iter().map(|(c, _)| c.clone()).collect();
        codes.dedup();
        let lines: Vec<String> = frc
            .error_returns
            .iter()
            .map(|(_, n)| rc_side.line_of(n).to_string())
            .collect();
        let raises = if frc.raises.is_empty() {
            " and raises nothing to the caller"
        } else {
            ""
        };
        let witness = format!(
            "If a statement fails, {} handles the failure and returns code {} (lines {}){raises}; {} has no handler, so the routine exits and the error (SQLSTATE) propagates to the caller.",
            side_name(rc_side),
            codes.join(" or "),
            lines.join(", "),
            side_name(other),
        );
        let n = &frc.error_returns[0].1;
        let (na, nb) = if rc_is_a {
            (Some(n), None)
        } else {
            (None, Some(n))
        };
        bld.push(
            "error.contract",
            ChangeClass::Semantic,
            Severity::High,
            Verdict::Different,
            witness,
            na,
            nb,
            vec![Assumption::default(
                "ASM-CALLER-CHECKS-RC",
                "Callers inspect the return code",
            )],
        );
        return;
    }
}
