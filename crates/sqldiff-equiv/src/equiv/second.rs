//! The second opinion of a same-dialect comparison (T-SQL against T-SQL
//! equivalence, step 1; `tsql-tsql/design.md`).
//!
//! The structural diff (`treediff`) pairs statements by their text, so a
//! rename of a local or a reordered operand leaves `stmt.modified` /
//! `header.modified` findings that the statement lowering (`compare_within`,
//! the comparison of different-dialect pairs) can prove equal. This module
//! decides when that proof may remove the structural findings, and only then.
//!
//! The rule is all or nothing for the routines it lowers, and it removes nothing
//! it did not compare. Only the routines that hold a removable finding are
//! lowered ([`unread_routines`]: the others are blanked on both sides); every
//! removable finding then lies in a routine that was read:
//!
//! - B must be A with variables renamed and tokens reordered and nothing
//!   else ([`same_tokens`]): the lowering does not compare what a conversion
//!   legitimately changes (a declared type, a literal), which in one dialect
//!   is a change of meaning. Round 1 clears only renames and reorderings.
//! - Every routine header (name, parameters with type, length, default,
//!   OUTPUT and READONLY, return type, options) must be token-identical, so
//!   the strict same-dialect signature holds by construction. Header
//!   findings are never removed.
//! - The lowering must find nothing: no finding, no unknown reason, no
//!   transaction statement left uncompared, no refinement (B only refining
//!   A is not equal), and it must have proved at least one statement pair
//!   (a comparison that compared nothing cannot clear anything). It names
//!   every statement or construct it does not compare as an unknown reason,
//!   so a clean result compared them all.
//! - A statement the lowering only noted instead of comparing (`SET
//!   NOCOUNT`, ANSI settings, `ADD FILEGROUP`) must stand with the same text
//!   after the same tokens ([`noted_in_place`]): the lowering notes them
//!   because across dialects they are not observed, but within one dialect
//!   their position decides what the statements after them do.
//! - Premises of the cross-dialect lowering are taken only where they are
//!   vacuous for one dialect: `ASM-UNTYPED-STORE` is not recorded when both
//!   sides are one dialect (`Canon::assume_store`); `ASM-TSQL-OWNERSHIP-CHAIN`
//!   is dropped only when neither header names `EXECUTE` (both sides then
//!   run as the caller with the same owner, and touch the same tables). Any
//!   other assumption of the lowering leaves the pair as the structural diff
//!   has it: the owner has not approved its reading for one dialect.
//! - Only structural findings without a witness are removed
//!   ([`removable`]); a finding that carries a witness, and any finding of
//!   another analysis, stays and keeps the verdict from being equal.

use sqldiff_syntax::{ast as cst, SyntaxKind};

use super::canon::asm;
use super::Outcome;
use crate::artifact::Analyzed;
use crate::coverage::{Part, Receipt};
use crate::model::{Finding, Verdict};

/// The structural findings the lowering can clear.
const STRUCTURAL: &[&str] = &[
    "stmt.modified",
    "stmt.added",
    "stmt.removed",
    "stmt.moved",
    "header.modified",
];

/// Whether `f` is a structural finding: the pairing's report of text with no
/// counterpart or a changed counterpart, which states no witness.
pub fn removable(f: &Finding) -> bool {
    STRUCTURAL.contains(&f.rule.as_str())
}

/// The significant header tokens of every routine of `a`, in order, exactly
/// as written (a header that differs only in case is not taken as the same:
/// whether it is a different signature is the structural diff's to report).
fn headers(a: &Analyzed) -> Vec<Vec<String>> {
    a.root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::ROUTINE)
        .map(|n| {
            cst::header_tokens(&n)
                .into_iter()
                .map(|t| t.text().to_string())
                .collect()
        })
        .collect()
}

/// Whether both artifacts have the same routines in the same order with
/// token-identical headers (strict same-dialect signature).
pub fn same_headers(a: &Analyzed, b: &Analyzed) -> bool {
    headers(a) == headers(b)
}

/// A variable or parameter token (`@x`, not `@@rowcount`), whose name a
/// rename may change.
fn is_variable(t: &sqldiff_syntax::SyntaxToken) -> bool {
    t.text().starts_with('@') && !t.text().starts_with("@@")
}

/// The significant tokens of `a`, variable names masked.
fn masked(a: &Analyzed) -> Vec<String> {
    cst::sig_tokens(&a.root)
        .map(|t| {
            if is_variable(&t) {
                "@".to_string()
            } else {
                t.text().to_string()
            }
        })
        .collect()
}

/// The significant tokens of every `DECLARE` statement of `a`, in order,
/// variable names masked: a declared type, length, default or table shape.
fn declarations(a: &Analyzed) -> Vec<Vec<String>> {
    a.root
        .descendants()
        .filter(|n| n.kind().is_unit() && cst::starts_with_kw(n, "DECLARE"))
        .map(|n| {
            cst::sig_tokens(&n)
                .map(|t| {
                    if is_variable(&t) {
                        "@".to_string()
                    } else {
                        t.text().to_string()
                    }
                })
                .collect()
        })
        .collect()
}

/// Whether B is A with variables renamed and tokens reordered, and nothing
/// else: the same tokens (variable names masked, as written otherwise),
/// and every declaration (type, length, default) the same in the same
/// place. The statement lowering was written for pairs in two dialects and
/// does not compare what a conversion legitimately changes (a variable's
/// declared type, a literal's spelling); in one dialect a changed token of
/// that kind is a change of meaning, so a pair that adds, drops or changes
/// any token is left to the structural diff.
pub fn same_tokens(a: &Analyzed, b: &Analyzed) -> bool {
    let (mut x, mut y) = (masked(a), masked(b));
    x.sort_unstable();
    y.sort_unstable();
    x == y && declarations(a) == declarations(b) && national_units(a) == national_units(b)
}

/// For each string literal of `n`, in order, whether it is a national one
/// (`N'..'`). The statement lowering reads both spellings as one string, as
/// it must across dialects; in T-SQL `'x'` is `varchar` and `N'x'` is
/// `nvarchar`, so a non-Latin character becomes `?` in the first, a result
/// column or `SELECT ... INTO` column has the other type, and a pair that
/// puts one where the other stood changes the meaning.
pub fn string_prefixes(n: &sqldiff_syntax::SyntaxNode) -> Vec<bool> {
    n.descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| t.kind() == SyntaxKind::STRING)
        .map(|t| t.text().starts_with(['N', 'n']))
        .collect()
}

/// The string-literal prefixes of each statement of `a` (its own tokens,
/// not those of a statement nested in it), one entry per statement that has
/// any, in document order. Not sorted: a literal moved within a statement or
/// between statements changes the sequence, and so does a statement moved
/// past another whose literals have other prefixes (refused, never cleared).
/// Statements whose literals all have the same prefix may still swap.
fn national_units(a: &Analyzed) -> Vec<Vec<bool>> {
    fn own(n: &sqldiff_syntax::SyntaxNode, out: &mut Vec<bool>) {
        for e in n.children_with_tokens() {
            match e {
                rowan::NodeOrToken::Token(t) => {
                    if t.kind() == SyntaxKind::STRING {
                        out.push(t.text().starts_with(['N', 'n']));
                    }
                }
                rowan::NodeOrToken::Node(c) => {
                    if !(c.kind().is_unit() || c.kind().is_container()) {
                        own(&c, out);
                    }
                }
            }
        }
    }
    let v: Vec<Vec<bool>> = a
        .root
        .descendants()
        .filter(|n| n.kind().is_unit())
        .map(|n| {
            let mut f = Vec::new();
            own(&n, &mut f);
            f
        })
        .filter(|f| !f.is_empty())
        .collect();
    v
}

/// Whether every statement the lowering passed over instead of comparing
/// (`SET NOCOUNT`, `SET ANSI_NULLS ON`, `SET QUOTED_IDENTIFIER ON`,
/// `ADD FILEGROUP`, `CHECK CONSTRAINT` after an add) stands as it did: the
/// same text, after the same tokens. The lowering notes them because across
/// dialects they are not observed; within one dialect where they sit changes
/// what the statements after them do (a rows-affected count is sent or not, an
/// identifier is a name or a string).
fn noted_in_place(a: &Analyzed, b: &Analyzed, sec: &Outcome) -> bool {
    let masked_at = |x: &Analyzed| -> Vec<(usize, String)> {
        cst::sig_tokens(&x.root)
            .map(|t| {
                let at: usize = t.text_range().start().into();
                let m = if is_variable(&t) {
                    "@".to_string()
                } else {
                    t.text().to_string()
                };
                (at, m)
            })
            .collect()
    };
    if sec.noted[0].len() != sec.noted[1].len() {
        return false;
    }
    if sec.noted[0].is_empty() {
        return true;
    }
    let (ta, tb) = (masked_at(a), masked_at(b));
    let slice = |t: &[(usize, String)], (s, e): (usize, usize)| -> (Vec<String>, Vec<String>) {
        let lo = t.partition_point(|x| x.0 < s);
        let hi = t.partition_point(|x| x.0 < e);
        (
            t[..lo].iter().map(|x| x.1.clone()).collect(),
            t[lo..hi].iter().map(|x| x.1.clone()).collect(),
        )
    };
    sec.noted[0]
        .iter()
        .zip(&sec.noted[1])
        .all(|(&ra, &rb)| slice(&ta, ra) == slice(&tb, rb))
}

/// Byte ranges of routines in one artifact.
type Ranges = Vec<(usize, usize)>;

/// The byte ranges of the routines the lowering need not read: when every
/// removable finding lies inside a routine, on side A inside routine i of A
/// or on side B inside routine i of B (routines paired by position, which
/// `same_headers` guarantees to be the same routines), the routines without
/// one are blanked on both sides and only the others are lowered. A routine
/// no removable finding names has nothing the proof could clear, and its
/// unrelated unknown reasons no longer keep another routine's rename from
/// being cleared. `None` when nothing can be left out: a finding outside
/// every routine (a script's own statement), a different number of routines,
/// or every routine holds one.
pub fn unread_routines(
    a: &Analyzed,
    b: &Analyzed,
    findings: &[Finding],
) -> Option<(Ranges, Ranges)> {
    let ranges = |x: &Analyzed| -> Ranges {
        x.root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::ROUTINE)
            .map(|n| {
                let r = n.text_range();
                (usize::from(r.start()), usize::from(r.end()))
            })
            .collect()
    };
    let (ra, rb) = (ranges(a), ranges(b));
    if ra.is_empty() || ra.len() != rb.len() {
        return None;
    }
    let at = |rs: &[(usize, usize)], l: &crate::model::Location| {
        rs.iter()
            .position(|&(s, e)| l.byte_start >= s && l.byte_start < e)
    };
    let mut live = vec![false; ra.len()];
    for f in findings.iter().filter(|f| removable(f)) {
        let ia = f.a.as_ref().map(|l| at(&ra, l));
        let ib = f.b.as_ref().map(|l| at(&rb, l));
        match (ia, ib) {
            // A side the finding names must sit in a routine, and the
            // routines must be the same pair.
            (Some(None), _) | (_, Some(None)) | (None, None) => return None,
            (Some(Some(i)), Some(Some(j))) if i != j => return None,
            (Some(Some(i)), _) | (_, Some(Some(i))) => live[i] = true,
        }
    }
    if live.iter().all(|&l| l) {
        return None;
    }
    let pick = |rs: &[(usize, usize)]| -> Vec<(usize, usize)> {
        rs.iter()
            .zip(&live)
            .filter(|(_, &l)| !l)
            .map(|(&r, _)| r)
            .collect()
    };
    Some((pick(&ra), pick(&rb)))
}

/// Whether any routine header of `a` names `EXECUTE` (`EXECUTE AS ...`).
fn has_execute_as(a: &Analyzed) -> bool {
    headers(a).iter().any(|h| h.iter().any(|t| t == "EXECUTE"))
}

/// Why a pair cannot be cleared whatever the lowering finds, or `None`: the
/// strict signature and the token shape. Cheap, so the engine asks before it
/// pays for the lowering.
pub fn eligible(a: &Analyzed, b: &Analyzed) -> Option<&'static str> {
    if !same_headers(a, b) {
        return Some("routine headers differ");
    }
    if !same_tokens(a, b) {
        return Some("the pair does more than rename variables and reorder");
    }
    None
}

/// Why the lowering's result may not clear structural findings, or `None`
/// when it may. `sec` is `compare_within` on the pair.
pub fn refusal(a: &Analyzed, b: &Analyzed, sec: &Outcome) -> Option<&'static str> {
    if let Some(why) = eligible(a, b) {
        return Some(why);
    }
    if !sec.findings.is_empty()
        || !sec.unknown_reasons.is_empty()
        || !sec.txn_unknown.is_empty()
        || !sec.refinements.is_empty()
    {
        return Some("the statement comparison found differences or could not decide");
    }
    // Premises of the lowering are taken only where they are vacuous for one
    // dialect. `ASM-UNTYPED-STORE` is not recorded for one dialect at all
    // (`Canon::assume_store`); the ownership chain is dropped below; any
    // other premise (owner privileges, argument lengths, ...) was written
    // for a conversion and its reading for a second version of one routine
    // needs the owner's OK, so the pair stays as the structural diff has it.
    if sec
        .assumptions
        .iter()
        .any(|x| x.id != asm::OWNERSHIP_CHAIN.0)
    {
        return Some("the comparison rests on an assumption written for conversions");
    }
    if sec
        .assumptions
        .iter()
        .any(|x| x.id == asm::OWNERSHIP_CHAIN.0)
        && (has_execute_as(a) || has_execute_as(b))
    {
        return Some("a routine names EXECUTE AS");
    }
    if sec.passed_over {
        return Some("a table or DDL comparison passed over a detail");
    }
    if !noted_in_place(a, b, sec) {
        return Some("a statement the lowering only noted (SET, ALTER DATABASE) is not in place");
    }
    if sec.equal == 0 {
        return Some("the statement comparison paired and proved no statement");
    }
    None
}

/// Removes the structural findings the accepted second opinion clears and
/// returns a receipt for the text of each (`coverage::unreceipted` then
/// finds that text vouched for). A witnessed difference or a finding of
/// another analysis stays; if one is present at all nothing is removed (the
/// two analyses disagree, which is reported by keeping both).
pub fn clear(findings: &mut Vec<Finding>) -> Vec<Receipt> {
    if findings
        .iter()
        .any(|f| f.verdict == Verdict::Different && !removable(f))
    {
        return Vec::new();
    }
    let (gone, kept): (Vec<Finding>, Vec<Finding>) =
        std::mem::take(findings).into_iter().partition(removable);
    *findings = kept;
    gone.iter()
        .map(|f| Receipt::Judged {
            rule: "statement lowering proved the pair equal (same dialect)",
            a: f.a.as_ref().map(Part::at),
            b: f.b.as_ref().map(Part::at),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ChangeClass, Severity};

    fn finding(rule: &str, verdict: Verdict) -> Finding {
        Finding {
            fingerprint: rule.to_string(),
            rule: rule.to_string(),
            class: ChangeClass::Semantic,
            severity: Severity::Medium,
            verdict,
            witness: String::new(),
            a: None,
            b: None,
            assumptions: Vec::new(),
        }
    }

    #[test]
    fn only_structural_findings_are_cleared() {
        let mut fs = vec![
            finding("stmt.modified", Verdict::Unknown),
            finding("header.modified", Verdict::Unknown),
            finding("stmt.removed", Verdict::Different),
            finding("lint.something", Verdict::Unknown),
            finding("error.contract", Verdict::Unknown),
        ];
        let receipts = clear(&mut fs);
        assert_eq!(receipts.len(), 3);
        let left: Vec<&str> = fs.iter().map(|f| f.rule.as_str()).collect();
        assert_eq!(left, ["lint.something", "error.contract"]);
    }

    #[test]
    fn a_witnessed_difference_stops_the_clearing() {
        // The lowering proved the pair equal, another analysis has a
        // witness: the two disagree, so nothing is cleared and both stay.
        for other in [
            "equiv.edit",
            "error.contract",
            "txn.atomicity",
            "equiv.unproven",
        ] {
            let mut fs = vec![
                finding("stmt.modified", Verdict::Unknown),
                finding(other, Verdict::Different),
            ];
            assert!(clear(&mut fs).is_empty(), "{other}");
            assert_eq!(fs.len(), 2, "{other}");
        }
    }
}
