//! The coverage invariant (phase 0 in docs/verification-roadmap.md): "equal"
//! may only rest on what was compared. Statement comparison works on units
//! (statements, blocks, routines); a significant token outside every unit is
//! compared here, or the verdict cannot be "equal".
//!
//! Today such tokens are T-SQL batch separators. `GO 5` runs the batch before
//! it five times, so `GO` against `GO 5` is a real difference that the unit
//! comparison never sees.
//!
//! Inside units the invariant is kept by receipts. Within one dialect every
//! step that decides a unit (the structural diff, statement equivalence, the
//! observer profile) leaves a [`Receipt`] for the text it decided, and
//! [`unreceipted`] checks that every significant token of both sides is
//! under one: a receipt that two units have the same tokens is checked
//! again here, a judgment by a named rule is listed by that rule, and a
//! finding covers its own text. A token under none was not compared, so the
//! verdict cannot be "equal". A comparison step that forgets a unit makes
//! the result unknown instead of wrong. Across dialects the lowering lists
//! what it does not cover (`equiv::lower`).

use std::collections::HashSet;

use sqldiff_syntax::{ast, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

use crate::artifact::Analyzed;
use crate::model::{Finding, Location, Verdict};
use crate::normalize::norm_token;

/// What a comparison step vouches for.
#[derive(Debug, Clone)]
pub enum Receipt {
    /// A unit of A and a unit of B have the same tokens (up to the loose
    /// normalization). Checked by [`unreceipted`].
    Same(SyntaxNode, SyntaxNode),
    /// A compound unit of A and one of B have the same own tokens, with
    /// their statement children in the same places. Checked by
    /// [`unreceipted`]; the children need receipts of their own.
    Shell(SyntaxNode, SyntaxNode),
    /// Text that a named rule decided makes no difference.
    Judged {
        rule: &'static str,
        a: Option<Part>,
        b: Option<Part>,
    },
}

/// The text of one side that a judgment covers.
#[derive(Debug, Clone)]
pub enum Part {
    /// All of a unit.
    Whole(SyntaxNode),
    /// A unit's own tokens, without its statement children.
    Own(SyntaxNode),
    /// The text a finding pointed at.
    At(usize, usize),
}

impl Part {
    pub fn at(l: &Location) -> Part {
        Part::At(l.byte_start, l.byte_end)
    }
}

fn compared(t: &SyntaxToken) -> bool {
    ast::is_significant(t.kind()) && t.kind() != SyntaxKind::SEMICOLON
}

/// A unit's own tokens in comparable form, each run of statement children
/// standing as one placeholder (the children lists are diffed on their own).
fn shell(n: &SyntaxNode, a: &Analyzed) -> Vec<String> {
    const CHILDREN: &str = "\u{0}children";
    let mut out: Vec<String> = Vec::new();
    for el in n.children_with_tokens() {
        match el {
            SyntaxElement::Token(t) if compared(&t) => out.push(norm_token(&t, a.dialect()).loose),
            SyntaxElement::Token(_) => {}
            SyntaxElement::Node(c) if c.kind().is_unit() || c.kind().is_container() => {
                if out.last().map(String::as_str) != Some(CHILDREN) {
                    out.push(CHILDREN.to_string())
                }
            }
            SyntaxElement::Node(c) => out.extend(shell(&c, a)),
        }
    }
    out
}

fn same(x: &SyntaxNode, a: &Analyzed, y: &SyntaxNode, b: &Analyzed) -> bool {
    let toks = |n: &SyntaxNode, s: &Analyzed| -> Vec<String> {
        ast::sig_tokens(n)
            .filter(compared)
            .map(|t| norm_token(&t, s.dialect()).loose)
            .collect()
    };
    toks(x, a) == toks(y, b)
}

fn starts(tokens: impl Iterator<Item = SyntaxToken>) -> impl Iterator<Item = usize> {
    tokens
        .filter(compared)
        .map(|t| t.text_range().start().into())
}

/// Why the verdict cannot be "equal" for text no comparison step vouched
/// for, if it cannot. Only tokens inside units are checked here; the ones
/// outside are [`uncovered`]'s.
pub fn unreceipted(
    a: &Analyzed,
    b: &Analyzed,
    receipts: &[Receipt],
    findings: &[Finding],
) -> Option<String> {
    let mut covered: [HashSet<usize>; 2] = Default::default();
    let mut ranges: [Vec<(usize, usize)>; 2] = Default::default();
    let mut part = |side: usize, p: &Part, covered: &mut [HashSet<usize>; 2]| match p {
        Part::Whole(n) => covered[side].extend(starts(ast::sig_tokens(n))),
        Part::Own(n) => covered[side].extend(starts(ast::header_tokens(n).into_iter())),
        Part::At(s, e) => ranges[side].push((*s, *e)),
    };
    for r in receipts {
        match r {
            Receipt::Same(x, y) => {
                if same(x, a, y, b) {
                    covered[0].extend(starts(ast::sig_tokens(x)));
                    covered[1].extend(starts(ast::sig_tokens(y)));
                }
            }
            Receipt::Shell(x, y) => {
                if shell(x, a) == shell(y, b) {
                    covered[0].extend(starts(ast::header_tokens(x).into_iter()));
                    covered[1].extend(starts(ast::header_tokens(y).into_iter()));
                }
            }
            Receipt::Judged { a: pa, b: pb, .. } => {
                if let Some(p) = pa {
                    part(0, p, &mut covered);
                }
                if let Some(p) = pb {
                    part(1, p, &mut covered);
                }
            }
        }
    }
    // A finding that does not say "equal" covers its text: it keeps the
    // verdict from being "equal", or states the assumption it rests on.
    for f in findings.iter().filter(|f| f.verdict != Verdict::Equal) {
        for (side, l) in [(0, &f.a), (1, &f.b)] {
            if let Some(l) = l {
                ranges[side].push((l.byte_start, l.byte_end));
            }
        }
    }
    let mut missed = Vec::new();
    for (side, an) in [(0, a), (1, b)] {
        let first = an
            .root
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(compared)
            .filter(|t| t.parent_ancestors().any(|n| n.kind().is_unit()))
            .find(|t| {
                let r = t.text_range();
                let (s, e): (usize, usize) = (r.start().into(), r.end().into());
                !covered[side].contains(&s) && !ranges[side].iter().any(|&(x, y)| x <= s && e <= y)
            });
        if let Some(t) = first {
            let r = t.text_range();
            let line = an.location(r.start().into(), r.end().into()).start_line;
            missed.push(format!(
                "side {} line {line} (`{}`)",
                if side == 0 { "A" } else { "B" },
                t.text()
            ));
        }
    }
    (!missed.is_empty()).then(|| {
        format!(
            "no comparison step vouched for the text at {}; it was not compared",
            missed.join(" and ")
        )
    })
}

/// The significant tokens outside every unit, in order. A batch separator is
/// written `GO n` with its repeat count (1 when absent), so spellings that
/// run the batch the same number of times compare equal.
pub fn outside_units(a: &Analyzed) -> Vec<String> {
    a.root
        .descendants_with_tokens()
        .filter_map(|el| match el {
            SyntaxElement::Token(t) => Some(t),
            SyntaxElement::Node(_) => None,
        })
        .filter(|t| ast::is_significant(t.kind()) && t.kind() != SyntaxKind::SEMICOLON)
        .filter(|t| !t.parent_ancestors().any(|n| n.kind().is_unit()))
        .map(|t| {
            if t.kind() == SyntaxKind::BATCH_SEP {
                let mut words = t.text().split_whitespace().skip(1);
                match (words.next().map(str::parse::<u64>), words.next()) {
                    (None, None) => "GO 1".to_string(),
                    (Some(Ok(n)), None) => format!("GO {n}"),
                    _ => format!("BATCH_SEP {}", t.text()),
                }
            } else {
                format!("{:?} {}", t.kind(), t.text())
            }
        })
        .collect()
}

/// Why the verdict cannot be "equal" for lack of coverage, if it cannot.
/// Within one dialect the tokens outside units must be the same on both
/// sides; across dialects only a plain batch separator may stand there
/// (statement equivalence pairs batches with blocks, `docs/cross-dialect-proofs.md`).
pub fn uncovered(a: &Analyzed, b: &Analyzed) -> Option<String> {
    let (oa, ob) = (outside_units(a), outside_units(b));
    let show = |v: &[String]| {
        if v.is_empty() {
            "nothing".to_string()
        } else {
            v.iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    if a.dialect() == b.dialect() {
        (oa != ob).then(|| {
            format!(
                "outside any statement, A has {} and B has {}; that text is not compared (a batch repeated with GO n runs n times)",
                show(&oa),
                show(&ob)
            )
        })
    } else {
        let odd: Vec<String> = oa
            .iter()
            .chain(&ob)
            .filter(|s| *s != "GO 1")
            .cloned()
            .collect();
        (!odd.is_empty()).then(|| {
            format!(
                "outside any statement there is {}, which the comparison across dialects does not cover",
                show(&odd)
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::Artifact;
    use crate::model::Side;
    use sqldiff_syntax::{Dialect, SyntaxNode};

    fn outside(text: &str) -> Vec<String> {
        let art = Artifact::new("a.sql", Dialect::Tsql, text);
        outside_units(&Analyzed::new(&art, Side::A, 1))
    }

    fn sides(x: &str, y: &str) -> (Artifact, Artifact) {
        (
            Artifact::new("a.sql", Dialect::Tsql, x),
            Artifact::new("b.sql", Dialect::Tsql, y),
        )
    }

    fn first(a: &Analyzed, kind: SyntaxKind) -> SyntaxNode {
        a.root.descendants().find(|n| n.kind() == kind).unwrap()
    }

    const PROC: &str = "CREATE PROCEDURE dbo.p AS\nBEGIN\n  IF @x = 1\n    SELECT 1;\n  ELSE\n    SELECT 2;\nEND\n";

    #[test]
    fn the_structural_diff_vouches_for_every_token() {
        let (x, y) = sides(PROC, &PROC.replace("SELECT 2", "select   2"));
        let (a, b) = (Analyzed::new(&x, Side::A, 1), Analyzed::new(&y, Side::B, 1));
        let td = crate::treediff::diff(&a, &b);
        assert!(td.findings.is_empty());
        assert_eq!(unreceipted(&a, &b, &td.receipts, &[]), None);
    }

    #[test]
    fn text_without_a_receipt_was_not_compared() {
        let (x, y) = sides(PROC, PROC);
        let (a, b) = (Analyzed::new(&x, Side::A, 1), Analyzed::new(&y, Side::B, 1));
        let why = unreceipted(&a, &b, &[], &[]).unwrap();
        assert!(why.contains("side A line 1 (`CREATE`)"), "{why}");
        // A receipt for the routine's own tokens leaves its statements open.
        let own = Receipt::Judged {
            rule: "test",
            a: Some(Part::Own(first(&a, SyntaxKind::ROUTINE))),
            b: Some(Part::Own(first(&b, SyntaxKind::ROUTINE))),
        };
        let why = unreceipted(&a, &b, &[own], &[]).unwrap();
        assert!(why.contains("side A line 2 (`BEGIN`)"), "{why}");
    }

    #[test]
    fn receipts_for_the_same_tokens_are_checked() {
        let (x, y) = sides(PROC, &PROC.replace("SELECT 2", "SELECT 3"));
        let (a, b) = (Analyzed::new(&x, Side::A, 1), Analyzed::new(&y, Side::B, 1));
        let r = [Receipt::Same(
            first(&a, SyntaxKind::ROUTINE),
            first(&b, SyntaxKind::ROUTINE),
        )];
        assert!(unreceipted(&a, &b, &r, &[]).is_some());
        // An IF with ELSE is not the shell of an IF without one, whatever
        // the statements inside.
        let (x, y) = sides(
            "IF @x = 1\n  SELECT 1;\nELSE\n  SELECT 2;\n",
            "IF @x = 1\n  SELECT 1;\n",
        );
        let (a, b) = (Analyzed::new(&x, Side::A, 1), Analyzed::new(&y, Side::B, 1));
        let shell = Receipt::Shell(
            first(&a, SyntaxKind::IF_STMT),
            first(&b, SyntaxKind::IF_STMT),
        );
        let why = unreceipted(&a, &b, &[shell], &[]).unwrap();
        assert!(why.contains("side A line 1 (`IF`)"), "{why}");
    }

    #[test]
    fn separators_compare_by_repeat_count() {
        assert_eq!(outside("SELECT 1;\nGO\nSELECT 2;\n"), ["GO 1"]);
        assert_eq!(outside("SELECT 1;\ngo 1\nSELECT 2;\n"), ["GO 1"]);
        assert_eq!(outside("SELECT 1;\nGO 5\nSELECT 2;\n"), ["GO 5"]);
        assert!(outside("SELECT 1;\nSELECT 2;\n").is_empty());
    }
}
