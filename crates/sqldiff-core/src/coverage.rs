//! The coverage invariant (phase 0 in docs/verification-roadmap.md): "equal"
//! may only rest on what was compared. Statement comparison works on units
//! (statements, blocks, routines); a significant token outside every unit is
//! compared here, or the verdict cannot be "equal".
//!
//! Today such tokens are T-SQL batch separators. `GO 5` runs the batch before
//! it five times, so `GO` against `GO 5` is a real difference that the unit
//! comparison never sees.

use sqldiff_syntax::{ast, SyntaxElement, SyntaxKind};

use crate::artifact::Analyzed;

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
    use sqldiff_syntax::Dialect;

    fn outside(text: &str) -> Vec<String> {
        let art = Artifact::new("a.sql", Dialect::Tsql, text);
        outside_units(&Analyzed::new(&art, Side::A, 1))
    }

    #[test]
    fn separators_compare_by_repeat_count() {
        assert_eq!(outside("SELECT 1;\nGO\nSELECT 2;\n"), ["GO 1"]);
        assert_eq!(outside("SELECT 1;\ngo 1\nSELECT 2;\n"), ["GO 1"]);
        assert_eq!(outside("SELECT 1;\nGO 5\nSELECT 2;\n"), ["GO 5"]);
        assert!(outside("SELECT 1;\nSELECT 2;\n").is_empty());
    }
}
