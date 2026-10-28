//! The proof gate (decisions 2 and 3 in docs/verification-roadmap.md): an
//! "equal" verdict is sent to a checker whose answer is backed by a Lean
//! theorem (`formal/lean`). In `report` mode the answer is recorded; in
//! `require` mode an unconfirmed "equal" becomes "unknown", with the reason.
//!
//! The core never starts a process: a front end supplies the [`Checker`].
//! A missing checker, a failure or anything unexpected is "unconfirmed",
//! never "confirmed".

use std::fmt::Write as _;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqldiff_syntax::{SyntaxElement, SyntaxNode};

use crate::artifact::Analyzed;
use crate::model::{Report, Verdict};

/// The line protocol `sqldiff-check` speaks (`formal/lean/Main.lean`).
pub const PROTOCOL: &str = "sqldiff-check/1";

/// What the gate does with an "equal" verdict.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// The checker is not called.
    #[default]
    Off,
    /// The answer is recorded; the verdict is unchanged.
    Report,
    /// An unconfirmed "equal" or "equal under assumptions" becomes "unknown".
    Require,
}

impl std::str::FromStr for Mode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "off" => Ok(Mode::Off),
            "report" => Ok(Mode::Report),
            "require" => Ok(Mode::Require),
            _ => Err(format!("proof mode `{s}`: expected off, report or require")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Confirmed,
    Unconfirmed,
    /// Nothing to confirm: the verdict is not "equal".
    NotApplicable,
}

/// What the gate found, as reports carry it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proof {
    pub status: Status,
    /// The theorem behind a confirmation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theorem: Option<String>,
    /// Why it is not confirmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The checker that answered (`sqldiff-check 0.1.0 (sqldiff-check/1)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checker: Option<String>,
}

/// One comparison, as the checker receives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub dialect_a: String,
    pub target_a: String,
    pub tree_a: String,
    pub dialect_b: String,
    pub target_b: String,
    pub tree_b: String,
}

impl Request {
    pub fn new(a: &Analyzed, b: &Analyzed) -> Self {
        Request {
            dialect_a: a.dialect().id().to_string(),
            target_a: a.artifact.version.clone(),
            tree_a: tree_line(&a.root),
            dialect_b: b.dialect().id().to_string(),
            target_b: b.artifact.version.clone(),
            tree_b: tree_line(&b.root),
        }
    }

    /// The request line, without the line end. `None` when a field would
    /// break the tab-separated format (a target with a tab or line break),
    /// which the gate answers as unconfirmed.
    pub fn line(&self, id: u64) -> Option<String> {
        let fields = [
            &self.dialect_a,
            &self.target_a,
            &self.tree_a,
            &self.dialect_b,
            &self.target_b,
            &self.tree_b,
        ];
        if fields.iter().any(|f| f.contains(['\t', '\n', '\r'])) {
            return None;
        }
        let mut s = format!("{PROTOCOL}\t{id}");
        for f in fields {
            s.push('\t');
            s.push_str(f);
        }
        s.push_str("\tend");
        Some(s)
    }
}

/// The checker's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Confirmed { theorem: String },
    Unconfirmed { reason: String },
}

/// A front end's connection to `sqldiff-check`. Every failure (no binary,
/// a crash, a timeout, an answer that does not parse) is
/// [`Answer::Unconfirmed`].
pub trait Checker: Send + Sync + std::fmt::Debug {
    /// Name and version, recorded in the report.
    fn name(&self) -> String;
    fn check(&self, req: &Request) -> Answer;
}

/// The gate's settings for one run.
#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub mode: Mode,
    pub checker: Option<Arc<dyn Checker>>,
}

/// A syntax tree without whitespace and comments, on one line: a node is
/// `(KIND child ...)`, a token `KIND:hex` with its text in lower-case hex.
/// Kind names are upper case with underscores and digits, so the form is
/// injective: two lines are equal only for equal trees. The Lean side
/// trusts this (formal/lean/Sqldiff/Gate.lean).
pub fn tree_line(node: &SyntaxNode) -> String {
    let mut out = String::new();
    write_node(node, &mut out);
    out
}

fn write_node(node: &SyntaxNode, out: &mut String) {
    let _ = write!(out, "({:?}", node.kind());
    for child in node.children_with_tokens() {
        match child {
            SyntaxElement::Node(n) => {
                out.push(' ');
                write_node(&n, out);
            }
            SyntaxElement::Token(t) if !t.kind().is_trivia() => {
                let _ = write!(out, " {:?}:", t.kind());
                for b in t.text().as_bytes() {
                    let _ = write!(out, "{b:02x}");
                }
            }
            SyntaxElement::Token(_) => {}
        }
    }
    out.push(')');
}

/// Runs the gate on a finished diff report.
pub(crate) fn apply(r: &mut Report, a: &Analyzed, b: &Analyzed, s: &Settings) {
    if s.mode == Mode::Off {
        return;
    }
    if !matches!(r.verdict(), Verdict::Equal | Verdict::EqualUnderAssumptions) {
        r.proof = Some(Proof {
            status: Status::NotApplicable,
            theorem: None,
            reason: None,
            checker: None,
        });
        return;
    }
    let (answer, checker) = match &s.checker {
        None => (
            Answer::Unconfirmed {
                reason: "no checker: sqldiff-check was not found".into(),
            },
            None,
        ),
        // The phase 0 fragment needs the same dialect and target on both
        // sides; the checker would answer the same, so the trees are not sent.
        Some(c) if a.dialect() != b.dialect() || a.artifact.version != b.artifact.version => (
            Answer::Unconfirmed {
                reason: "outside the verified fragment: different dialects or targets".into(),
            },
            Some(c.name()),
        ),
        Some(c) => (c.check(&Request::new(a, b)), Some(c.name())),
    };
    match answer {
        Answer::Confirmed { theorem } => {
            r.proof = Some(Proof {
                status: Status::Confirmed,
                theorem: Some(theorem),
                reason: None,
                checker,
            });
        }
        Answer::Unconfirmed { reason } => {
            if s.mode == Mode::Require {
                r.verdict_before_proof = Some(r.verdict());
                r.unknown_reasons
                    .push(format!("not confirmed by the proof checker: {reason}"));
                r.settle();
            }
            r.proof = Some(Proof {
                status: Status::Unconfirmed,
                theorem: None,
                reason: Some(reason),
                checker,
            });
        }
    }
}

/// A checker that answers from a fixed function; for tests.
#[derive(Debug)]
pub struct FnChecker(pub fn(&Request) -> Answer);

impl Checker for FnChecker {
    fn name(&self) -> String {
        "test checker".into()
    }
    fn check(&self, req: &Request) -> Answer {
        (self.0)(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqldiff_syntax::{parse, Dialect};

    fn line(text: &str, d: Dialect) -> String {
        tree_line(&parse(text, d).syntax())
    }

    #[test]
    fn whitespace_and_comments_are_left_out() {
        let d = Dialect::Tsql;
        assert_eq!(
            line("UPDATE t SET x = 1 WHERE id = 2;", d),
            line("UPDATE t\n  SET x = 1 -- set\n  /* c */ WHERE id = 2;", d)
        );
    }

    #[test]
    fn everything_else_counts() {
        let d = Dialect::Tsql;
        let base = line("UPDATE t SET x = 1 WHERE id = 2;", d);
        for other in [
            "update t SET x = 1 WHERE id = 2;",
            "UPDATE T SET x = 1 WHERE id = 2;",
            "UPDATE t SET x = 1 WHERE id = 2",
            "UPDATE t SET x = 01 WHERE id = 2;",
            "UPDATE t SET x = 1 WHERE id = 2 ;\nGO",
        ] {
            assert_ne!(base, line(other, d), "{other}");
        }
    }

    #[test]
    fn a_batch_separator_is_not_an_alias() {
        // `GO` ends a batch only on a line of its own; elsewhere it can be a
        // column alias. The tokens are the same, the trees are not.
        let d = Dialect::Tsql;
        assert_ne!(line("SELECT 1\nGO\n", d), line("SELECT 1 GO\n", d));
    }

    #[test]
    fn request_lines_have_nine_fields() {
        let d = Dialect::Postgres;
        let r = Request {
            dialect_a: "postgres".into(),
            target_a: "18".into(),
            tree_a: line("SELECT 'a\tb';", d),
            dialect_b: "postgres".into(),
            target_b: "18".into(),
            tree_b: line("SELECT 1;", d),
        };
        let l = r.line(7).unwrap();
        assert_eq!(l.split('\t').count(), 9, "{l}");
        assert!(l.starts_with("sqldiff-check/1\t7\tpostgres\t18\t("));
        assert!(l.ends_with("\tend"));
        let bad = Request {
            target_a: "18\tx".into(),
            ..r
        };
        assert!(bad.line(1).is_none());
    }
}
