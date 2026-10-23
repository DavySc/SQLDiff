//! Structured witnesses (design §2, §3 Layer 5 item 4): the scenario a
//! divergence was found under and what each side does in it, so the GUI's
//! "what if this fails?" view, the MCP `what_if` tool and review comments can
//! show both outcomes without re-reading the plain-language witness.

use serde::{Deserialize, Serialize};

use super::profile::ErrorKind;
use crate::model::Finding;

/// One step of the path a scenario follows: a condition the interpreter
/// could not decide and the value this path gives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathStep {
    pub condition: String,
    pub value: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioKind {
    /// Every statement succeeds.
    Baseline,
    /// One statement fails with one error kind.
    Fault,
    /// The caller already holds an open transaction.
    CallerTransaction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioInfo {
    pub kind: ScenarioKind,
    /// The failing statement, e.g. "the INSERT into stock_log".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<ErrorKind>,
    /// Line of the failing (or anchoring) statement on each side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_a: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_b: Option<usize>,
    /// The caller holds an open transaction / runs with XACT_ABORT ON.
    pub caller_transaction: bool,
    pub xact_abort: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<PathStep>,
    /// One sentence: "If the INSERT into stock_log fails with a constraint violation".
    pub text: String,
}

/// What one side does under a scenario: the outcome tuple of design §3
/// Layer 5 (control target, transaction effect, durable effects).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideOutcome {
    /// Where the error sends control: a handler, the next statement, or out
    /// of the routine. "runs to the end" when nothing fails.
    pub control: String,
    /// How the routine ends, as the caller sees it.
    pub exit: String,
    /// What the error rolls back.
    pub rollback: String,
    /// Transaction state at exit (open, doomed, left to the caller).
    pub transaction: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub durable: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub undone: Vec<String>,
    /// Kept part-way: the failing statement's rows changed before the error.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partial: Vec<String>,
    /// Still uncommitted; the caller commits or rolls back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub left_to_caller: Vec<String>,
    /// Lines of the statements run, in order (the path to step through).
    pub path: Vec<usize>,
    /// Caveats that keep the outcome from being exact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_proven: Vec<String>,
}

/// The structured witness of one behavioural finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Witness {
    /// Fingerprint of the finding it backs.
    pub fingerprint: String,
    pub rule: String,
    pub scenario: ScenarioInfo,
    pub a: SideOutcome,
    pub b: SideOutcome,
}

/// The result of asking "what if this statement fails with this error kind?"
/// (design §6.1 `what_if`, §6.2 "What if this fails?").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhatIf {
    pub scenario: ScenarioInfo,
    /// Whether each side's statement can raise this kind (profile `raises`).
    pub raisable_a: bool,
    pub raisable_b: bool,
    /// One outcome per path that reaches the statement.
    pub a: Vec<SideOutcome>,
    pub b: Vec<SideOutcome>,
    /// The divergences under this scenario, as findings.
    pub findings: Vec<Finding>,
    pub witnesses: Vec<Witness>,
    /// Why no answer could be given (statement not found, not aligned).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}
