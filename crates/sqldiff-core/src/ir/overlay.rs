//! Transaction overlay data (design §6.2): brackets for transactions,
//! savepoints, atomic and exception regions and handlers on each side, with
//! each bracket paired to its partner on the other side so the GUI can
//! highlight both.

use serde::{Deserialize, Serialize};
use sqldiff_syntax::{Dialect, SyntaxKind};

use super::cfg::{lower, Action, Op, ScopeKind};
use crate::artifact::Analyzed;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BracketKind {
    /// An explicit transaction (BEGIN TRAN ... COMMIT) or, where
    /// transactions start implicitly, a unit of work ending in COMMIT or
    /// ROLLBACK.
    Transaction,
    Savepoint,
    /// Db2 `BEGIN ATOMIC`.
    Atomic,
    /// A PL/pgSQL block with an EXCEPTION clause (a subtransaction).
    ExceptionBlock,
    /// T-SQL `BEGIN TRY`.
    Try,
    /// A CATCH block, EXCEPTION clause or declared handler.
    Handler,
}

impl BracketKind {
    /// Brackets of one family pair across dialects.
    fn family(self) -> u8 {
        match self {
            BracketKind::Transaction => 0,
            BracketKind::Savepoint => 1,
            BracketKind::Atomic | BracketKind::ExceptionBlock | BracketKind::Try => 2,
            BracketKind::Handler => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bracket {
    pub kind: BracketKind,
    pub start_line: usize,
    pub end_line: usize,
    pub label: String,
    /// Index of the matching bracket in the other side's list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partner: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overlay {
    pub a: Vec<Bracket>,
    pub b: Vec<Bracket>,
}

/// Brackets of one side, in document order.
pub fn brackets(a: &Analyzed) -> Vec<Bracket> {
    let cfg = lower(a);
    let line = |n: &sqldiff_syntax::SyntaxNode| {
        let l = a.node_location(n);
        (l.start_line, l.end_line)
    };
    let mut out: Vec<Bracket> = Vec::new();
    // Statements with a transaction action, in document order.
    let mut stmts: Vec<(usize, usize, &Action)> = cfg
        .nodes
        .iter()
        .filter_map(|n| {
            let s = n.stmt()?;
            let syn = n.syntax.as_ref()?;
            let (l, _) = line(syn);
            Some((usize::from(syn.text_range().start()), l, &s.action))
        })
        .collect();
    stmts.sort_by_key(|x| x.0);
    let body_start = a
        .root
        .descendants()
        .find(|n| n.kind() == SyntaxKind::ROUTINE_BODY)
        .map(|n| line(&n).0)
        .unwrap_or(1);
    // A transaction runs from BEGIN (or, where transactions start
    // implicitly, from the start of the body or the first statement after
    // the previous unit's end) to the last COMMIT or ROLLBACK before the next
    // one starts; branches can end it at several places (ends with no other
    // statement between them), all listed in the label.
    let implicit = a.dialect() != Dialect::Tsql;
    let mut units: Vec<(usize, Vec<(usize, &str)>)> = Vec::new();
    let mut cur: Option<(usize, Vec<(usize, &str)>)> = None;
    let mut savepoints: Vec<(String, usize)> = Vec::new();
    // Where the next implicit unit starts, once the previous one has ended.
    let mut next_start: Option<usize> = None;
    for (_, l, act) in &stmts {
        match act {
            Action::Begin => {
                if let Some(u) = cur.take() {
                    if !u.1.is_empty() || !implicit {
                        units.push(u);
                    }
                }
                cur = Some((*l, Vec::new()));
            }
            Action::Commit | Action::Rollback => {
                let what = if matches!(act, Action::Commit) {
                    "COMMIT"
                } else {
                    "ROLLBACK"
                };
                match &mut cur {
                    Some(u) => u.1.push((*l, what)),
                    None if implicit => {
                        let start = next_start.take().unwrap_or(body_start);
                        cur = Some((start, vec![(*l, what)]));
                    }
                    None => {}
                }
                savepoints.clear();
            }
            Action::Savepoint(n) => savepoints.push((n.clone(), *l)),
            Action::RollbackTo(n) | Action::Release(n) => {
                if let Some(i) = savepoints.iter().rposition(|(x, _)| x == n) {
                    let (_, s) = savepoints.remove(i);
                    out.push(Bracket {
                        kind: BracketKind::Savepoint,
                        start_line: s,
                        end_line: *l,
                        label: format!("savepoint {}", n.to_lowercase()),
                        partner: None,
                    });
                }
            }
            // Work after an ended unit starts the next one.
            _ if implicit && cur.as_ref().is_some_and(|u| !u.1.is_empty()) => {
                units.extend(cur.take());
                next_start = Some(*l);
            }
            _ => {}
        }
    }
    units.extend(cur);
    let end = a.node_location(&a.root).end_line;
    for (start, ends) in units {
        let (end_line, label) = match ends.last() {
            Some((l, _)) => (
                *l,
                format!(
                    "{} ending in {}",
                    if implicit {
                        "unit of work"
                    } else {
                        "transaction"
                    },
                    ends.iter()
                        .map(|(l, w)| format!("{w} (line {l})"))
                        .collect::<Vec<_>>()
                        .join(" or ")
                ),
            ),
            None => (end, "transaction left open at the end".to_string()),
        };
        out.push(Bracket {
            kind: BracketKind::Transaction,
            start_line: start,
            end_line,
            label,
            partner: None,
        });
    }
    if !out.iter().any(|b| b.kind == BracketKind::Transaction) {
        // No transaction control in the body: show who owns the transaction.
        let prof = super::profile::Profile::of(a.dialect());
        let label = match prof.routine_mode(cfg.routine.key()) {
            "caller" => Some("runs inside the calling statement's transaction".to_string()),
            "call" => {
                Some("runs as one transaction of the CALL, committed when it returns".to_string())
            }
            "commit_level" => match &cfg.commit_level {
                Some(l) if prof.is_no_commit_level(l) => Some(format!(
                    "no commitment control (COMMIT = {l}): each statement is durable at once"
                )),
                Some(l) => Some(format!(
                    "unit of work under COMMIT = {l}, committed by the caller"
                )),
                None => Some("unit of work; the commit level is not declared".to_string()),
            },
            _ => None,
        };
        if let Some(label) = label {
            out.push(Bracket {
                kind: BracketKind::Transaction,
                start_line: body_start,
                end_line: end,
                label,
                partner: None,
            });
        }
    }
    for n in &cfg.nodes {
        let (Op::Enter(scope), Some(syn)) = (&n.op, &n.syntax) else {
            continue;
        };
        let (kind, label) = match cfg.scopes[*scope].kind {
            ScopeKind::Atomic => (BracketKind::Atomic, "atomic compound"),
            ScopeKind::ExceptionBlock => (
                BracketKind::ExceptionBlock,
                "block with EXCEPTION (subtransaction)",
            ),
            ScopeKind::Try => (BracketKind::Try, "TRY block"),
            ScopeKind::Block => continue,
        };
        let (s, e) = line(syn);
        out.push(Bracket {
            kind,
            start_line: s,
            end_line: e,
            label: label.into(),
            partner: None,
        });
    }
    for n in a.root.descendants() {
        let label = match n.kind() {
            SyntaxKind::CATCH_BLOCK => "CATCH block",
            SyntaxKind::EXCEPTION_SECTION => "EXCEPTION clause",
            SyntaxKind::HANDLER => "handler",
            _ => continue,
        };
        let (s, e) = line(&n);
        let label = if n.kind() == SyntaxKind::HANDLER {
            let w = sqldiff_syntax::ast::first_words(&n, 2);
            format!(
                "{} handler",
                w.get(1).map(|x| x.to_lowercase()).unwrap_or_default()
            )
            .trim_start()
            .to_string()
        } else {
            label.to_string()
        };
        out.push(Bracket {
            kind: BracketKind::Handler,
            start_line: s,
            end_line: e,
            label,
            partner: None,
        });
    }
    out.sort_by_key(|b| (b.start_line, std::cmp::Reverse(b.end_line)));
    out
}

/// Brackets of both sides, each paired with the n-th bracket of the same
/// family on the other side.
pub fn overlay(a: &Analyzed, b: &Analyzed) -> Overlay {
    let mut o = Overlay {
        a: brackets(a),
        b: brackets(b),
    };
    for fam in 0..4u8 {
        let ia: Vec<usize> = (0..o.a.len())
            .filter(|i| o.a[*i].kind.family() == fam)
            .collect();
        let ib: Vec<usize> = (0..o.b.len())
            .filter(|i| o.b[*i].kind.family() == fam)
            .collect();
        for (x, y) in ia.iter().zip(&ib) {
            o.a[*x].partner = Some(*y);
            o.b[*y].partner = Some(*x);
        }
    }
    o
}
