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
    /// Every `GOTO`, with the label it names (lower-cased).
    gotos: Vec<(SyntaxNode, String)>,
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
                            .push((d.clone(), goto_target(&d).unwrap_or_default()));
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
            let mine: Vec<&SyntaxNode> = self
                .gotos
                .iter()
                .filter(|(_, l)| *l == name)
                .map(|(d, _)| d)
                .collect();
            if mine.is_empty() {
                continue;
            }
            let at = |g: &SyntaxNode| {
                units
                    .iter()
                    .position(|u| u.text_range().contains_range(g.text_range()))
            };
            let starts: Vec<Option<usize>> = mine.iter().map(|g| at(g)).collect();
            if starts.iter().any(|s| s.map_or(true, |s| s >= li)) {
                continue;
            }
            let start = starts.into_iter().flatten().min().unwrap_or(li);
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
