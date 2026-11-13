//! Forward `GOTO`: which jumps of a statement list are a labelled block.
//!
//! PL/pgSQL has no GOTO, so the converter writes a forward jump as a labelled
//! block left with `EXIT` (`<<goto_l>> BEGIN … EXIT goto_l; … END;`), and the
//! lowering reads the T-SQL jump the same way (`Region`): one rule for both,
//! so the conversion is compared with the very region it was made from.
//!
//! A label of a statement list is a region's end when
//! - its name occurs once in the routine, is plain (ASCII letters, digits,
//!   `_`) and short,
//! - at least one `GOTO` names it, and every one is inside a unit of the same
//!   list **before** the label.
//!
//! The region runs from the first unit holding such a jump to the label.
//! Regions that cross (neither nested nor disjoint) are dropped. Every other
//! jump (backward, into a block, from another list, to a label used twice)
//! is not a region and stays unread.

use std::collections::HashMap;

use sqldiff_syntax::{SyntaxKind, SyntaxNode};

use super::ast::tokens_shared as tokens;

/// The most labels a routine (or file) may have for any region to be read:
/// regions nest like blocks, even across statement lists, and every level is
/// recursion in the lowering and the alignment. More labels than this and no
/// region is read, so "not proven" instead of a deep recursion.
const MAX_REGIONS: usize = 64;

/// One label of a statement list and the units its jumps start from.
#[derive(Debug, Clone)]
pub struct Region {
    /// Index in the list of the first unit holding a jump to the label.
    pub start: usize,
    /// Index in the list of the label.
    pub label: usize,
    /// The label, lower-cased.
    pub name: String,
}

/// The labels and jumps of a routine (or of a script), found once.
#[derive(Debug, Default)]
pub struct Index {
    /// Occurrences of each label name (lower-cased) in the routine.
    labels: HashMap<String, usize>,
    /// Every `GOTO`, by the label it names (lower-cased).
    gotos: HashMap<String, Vec<SyntaxNode>>,
}

/// The name a label node declares (`name:`), lower-cased.
pub fn label_name(n: &SyntaxNode) -> String {
    n.text()
        .to_string()
        .trim()
        .trim_end_matches(':')
        .trim()
        .to_lowercase()
}

/// The label a `GOTO name` statement names, lower-cased, when it is exactly
/// that (an optional `;` aside).
pub fn goto_target(stmt: &SyntaxNode) -> Option<String> {
    let ts: Vec<_> = tokens(stmt)
        .iter()
        .filter(|t| t.kind != super::ast::K::Semi)
        .cloned()
        .collect();
    match ts.as_slice() {
        [g, l] if g.up == "GOTO" && l.kind == super::ast::K::Word => Some(l.text.to_lowercase()),
        _ => None,
    }
}

impl Index {
    /// The labels and jumps under `root` (a routine, or the whole file).
    pub fn of(root: &SyntaxNode) -> Index {
        let mut ix = Index::default();
        for d in root.descendants() {
            match d.kind() {
                SyntaxKind::LABEL => *ix.labels.entry(label_name(&d)).or_insert(0) += 1,
                SyntaxKind::STATEMENT => {
                    let w = sqldiff_syntax::ast::first_words(&d, 1);
                    if w.first().is_some_and(|x| x == "GOTO") {
                        // A jump that is not `GOTO name` names no label.
                        ix.gotos
                            .entry(goto_target(&d).unwrap_or_default())
                            .or_default()
                            .push(d.clone());
                    }
                }
                _ => {}
            }
        }
        ix
    }

    /// Whether the routine has a label at all.
    pub fn has_labels(&self) -> bool {
        !self.labels.is_empty()
    }

    /// The regions of the statement list `units`.
    pub fn regions(&self, units: &[SyntaxNode]) -> Vec<Region> {
        let mut out = Vec::new();
        if self.labels.len() > MAX_REGIONS {
            return out;
        }
        for (li, ln) in units.iter().enumerate() {
            if ln.kind() != SyntaxKind::LABEL {
                continue;
            }
            let name = label_name(ln);
            let plain = !name.is_empty()
                && name.len() <= 50
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !plain || self.labels.get(&name) != Some(&1) {
                continue;
            }
            let Some(mine) = self.gotos.get(&name) else {
                continue;
            };
            let at = |g: &SyntaxNode| {
                units
                    .iter()
                    .position(|u| u.text_range().contains_range(g.text_range()))
            };
            let starts: Vec<Option<usize>> = mine.iter().map(at).collect();
            if starts.iter().any(|s| s.map_or(true, |s| s >= li)) {
                continue;
            }
            let start = starts.into_iter().flatten().min().unwrap_or(li);
            // The block's node is the label (see `mark_at`), so a call inside the
            // region sits at a different place on the T-SQL side than on the
            // PostgreSQL side, where the block starts at its BEGIN: placement
            // comparisons stay conservative ("not proven"), never a false equal.
            out.push(Region {
                start,
                label: li,
                name,
            });
        }
        let crossing: Vec<bool> = out
            .iter()
            .map(|a| {
                out.iter().any(|b| {
                    (a.start < b.start && b.start < a.label && a.label < b.label)
                        || (b.start < a.start && a.start < b.label && b.label < a.label)
                })
            })
            .collect();
        let mut keep = crossing.into_iter();
        out.retain(|_| !keep.next().unwrap_or(false));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqldiff_syntax::Dialect;

    /// `n` nested regions in one script: `IF .. GOTO l<i>` lines, a
    /// statement, then the labels in reverse.
    fn script(n: usize) -> String {
        let mut t = String::new();
        for i in 0..n {
            t.push_str(&format!("IF @x = {i} GOTO l{i};\n"));
        }
        t.push_str("SET @y = 1;\n");
        for i in (0..n).rev() {
            t.push_str(&format!("l{i}:\n"));
        }
        t.push_str("SET @y = 2;");
        t
    }

    fn regions(n: usize) -> usize {
        let root = sqldiff_syntax::parse(&script(n), Dialect::Tsql).syntax();
        let units: Vec<SyntaxNode> = root
            .descendants()
            .filter(|d| matches!(d.kind(), SyntaxKind::STATEMENT | SyntaxKind::LABEL))
            .collect();
        Index::of(&root).regions(&units).len()
    }

    #[test]
    fn regions_are_read_within_the_bound_and_none_past_it() {
        assert_eq!(regions(3), 3);
        assert_eq!(regions(MAX_REGIONS), MAX_REGIONS);
        // Past the bound nothing is read, so nesting stays bounded.
        assert_eq!(regions(MAX_REGIONS + 1), 0);
    }
}
