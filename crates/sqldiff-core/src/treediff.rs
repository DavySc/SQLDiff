//! Same-dialect structural diff: statements aligned by content, not by line.

use std::collections::HashSet;

use sqldiff_syntax::{ast, SyntaxKind, SyntaxNode};

use crate::artifact::{snippet_of, Analyzed};
use crate::classify::{stmt_kind, StmtKind};
use crate::coverage::{Part, Receipt};
use crate::model::{fingerprint, ChangeClass, Finding, FingerprintInput, Verdict};
use crate::normalize::{
    loose_hash, norm_of, norm_tokens, similarity, strict_hash, token_hunks, NormTok,
};

#[derive(Clone)]
struct Unit {
    node: SyntaxNode,
    kind: StmtKind,
    toks: Vec<NormTok>,
    full: String,
    strict: String,
    header: Vec<NormTok>,
    header_hash: String,
    children: Vec<Unit>,
    effectful: bool,
}

fn build_unit(node: SyntaxNode, a: &Analyzed) -> Unit {
    let d = a.dialect();
    let header = norm_of(ast::header_tokens(&node).into_iter(), d);
    let children: Vec<Unit> = ast::structural_children(&node)
        .into_iter()
        .map(|c| build_unit(c, a))
        .collect();
    // The same tokens as `norm_tokens(&node, d)`, reusing the children's:
    // normalizing every subtree again at each level made deep nesting slow.
    let mut toks = Vec::new();
    let mut child = children.iter();
    for el in node.children_with_tokens() {
        match el {
            rowan::NodeOrToken::Token(t) => toks.extend(norm_of(
                std::iter::once(t).filter(|t| ast::is_significant(t.kind())),
                d,
            )),
            rowan::NodeOrToken::Node(n) if n.kind().is_unit() || n.kind().is_container() => {
                let c = child.next().expect("structural child");
                debug_assert!(c.node == n);
                toks.extend(c.toks.iter().cloned());
            }
            rowan::NodeOrToken::Node(n) => toks.extend(norm_tokens(&n, d)),
        }
    }
    let kind = stmt_kind(&node, d);
    let effectful = kind.is_effectful() || children.iter().any(|c| c.effectful);
    Unit {
        full: loose_hash(&toks),
        strict: strict_hash(&toks),
        header_hash: loose_hash(&header),
        node,
        kind,
        toks,
        header,
        children,
        effectful,
    }
}

pub struct TreeDiff {
    pub findings: Vec<Finding>,
    pub cosmetic: usize,
    pub structural: usize,
    /// T-SQL identifiers matched only case-insensitively.
    pub used_case_insensitive_idents: bool,
    /// What the diff vouches for (`coverage::unreceipted`).
    pub receipts: Vec<Receipt>,
}

pub fn diff(a: &Analyzed, b: &Analyzed) -> TreeDiff {
    let ua: Vec<Unit> = ast::structural_children(&a.root)
        .into_iter()
        .map(|n| build_unit(n, a))
        .collect();
    let ub: Vec<Unit> = ast::structural_children(&b.root)
        .into_iter()
        .map(|n| build_unit(n, b))
        .collect();
    let mut ctx = Ctx {
        a,
        b,
        out: TreeDiff {
            findings: Vec::new(),
            cosmetic: 0,
            structural: 0,
            used_case_insensitive_idents: false,
            receipts: Vec::new(),
        },
        removed: Vec::new(),
        added: Vec::new(),
        hashes_a: all_hashes(&ua),
        hashes_b: all_hashes(&ub),
    };
    ctx.diff_lists(&ua, &ub);
    ctx.resolve_unpaired();
    ctx.out
}

struct Ctx<'x, 'a> {
    a: &'x Analyzed<'a>,
    b: &'x Analyzed<'a>,
    out: TreeDiff,
    removed: Vec<Unit>,
    added: Vec<Unit>,
    /// Content hashes of every unit at any depth, per side: a unit whose
    /// twin exists on the other side may have moved.
    hashes_a: HashSet<String>,
    hashes_b: HashSet<String>,
}

fn all_hashes(units: &[Unit]) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut stack: Vec<&Unit> = units.iter().collect();
    while let Some(u) = stack.pop() {
        out.insert(u.full.clone());
        stack.extend(u.children.iter());
    }
    out
}

impl Ctx<'_, '_> {
    fn object(&self) -> &str {
        &self.a.object
    }

    fn diff_lists(&mut self, a: &[Unit], b: &[Unit]) {
        let ka: Vec<&str> = a.iter().map(|u| u.full.as_str()).collect();
        let kb: Vec<&str> = b.iter().map(|u| u.full.as_str()).collect();
        let anchors = crate::normalize::lcs_pairs(&ka, &kb);
        let (mut ai, mut bj) = (0, 0);
        for (i, j) in anchors
            .into_iter()
            .chain(std::iter::once((a.len(), b.len())))
        {
            self.diff_gap(&a[ai..i], &b[bj..j]);
            if i < a.len() {
                self.matched(&a[i], &b[j]);
            }
            ai = i + 1;
            bj = j + 1;
        }
    }

    fn matched(&mut self, x: &Unit, y: &Unit) {
        let (xs, xe) = ast::sig_range(&x.node);
        let (ys, ye) = ast::sig_range(&y.node);
        if self.a.artifact.text[xs..xe] != self.b.artifact.text[ys..ye] {
            self.out.cosmetic += 1;
        }
        self.out
            .receipts
            .push(Receipt::Same(x.node.clone(), y.node.clone()));
        self.case_only(x, y);
    }

    /// `x` and `y` are equal up to the case of names, which T-SQL folds
    /// under a case-insensitive collation (assumed). Result column names keep
    /// their case whatever the collation (decision 1 in
    /// docs/verification-roadmap.md), so a SELECT that differs only that way
    /// is left to statement equivalence, which compares the names a client
    /// receives exactly. Compound statements are looked into, since the
    /// SELECT can be nested.
    fn case_only(&mut self, x: &Unit, y: &Unit) {
        if x.strict == y.strict {
            return;
        }
        if !x.children.is_empty() && x.children.len() == y.children.len() {
            if strict_hash(&x.header) != strict_hash(&y.header) {
                self.out.used_case_insensitive_idents = true;
            }
            for (cx, cy) in x.children.iter().zip(&y.children) {
                self.case_only(cx, cy);
            }
            return;
        }
        if x.kind == StmtKind::Select && y.kind == StmtKind::Select && x.children.is_empty() {
            let exact = |t: &[NormTok]| -> Vec<NormTok> {
                t.iter()
                    .map(|t| NormTok {
                        loose: t.strict.clone(),
                        ..t.clone()
                    })
                    .collect()
            };
            self.push_change(x, y, &exact(&x.toks), &exact(&y.toks), "stmt.modified");
            return;
        }
        self.out.used_case_insensitive_idents = true;
    }

    fn score(x: &Unit, y: &Unit) -> f64 {
        if x.node.kind() != y.node.kind() || x.kind != y.kind {
            return 0.0;
        }
        if !x.children.is_empty() && x.header_hash == y.header_hash {
            return 1.0;
        }
        let s = similarity(&x.toks, &y.toks);
        // Same statement kind and same leading words (e.g. `UPDATE t`): treat as an edit.
        let lead = |u: &Unit| {
            u.toks
                .iter()
                .take(2)
                .map(|t| t.loose.clone())
                .collect::<Vec<_>>()
        };
        if x.kind != StmtKind::Other && lead(x) == lead(y) {
            s.max(0.5)
        } else {
            s
        }
    }

    fn diff_gap(&mut self, ga: &[Unit], gb: &[Unit]) {
        let mut next = 0;
        let mut paired_b = vec![false; gb.len()];
        let mut pairs = Vec::new();
        for (xi, x) in ga.iter().enumerate() {
            let mut best: Option<(usize, f64)> = None;
            for (yj, y) in gb.iter().enumerate().skip(next) {
                let s = Self::score(x, y);
                if s >= 0.5 && best.map_or(true, |(_, bs)| s > bs) {
                    best = Some((yj, s));
                }
            }
            if let Some((yj, _)) = best {
                pairs.push((xi, yj));
                paired_b[yj] = true;
                next = yj + 1;
            }
        }
        pairs.extend(self.pair_by_kind(ga, gb, &pairs));
        pairs.sort_unstable();
        for &(_, yj) in &pairs {
            paired_b[yj] = true;
        }
        let paired_a: Vec<usize> = pairs.iter().map(|p| p.0).collect();
        for (xi, x) in ga.iter().enumerate() {
            if !paired_a.contains(&xi) {
                self.removed.push(x.clone());
            }
        }
        for (yj, y) in gb.iter().enumerate() {
            if !paired_b[yj] {
                self.added.push(y.clone());
            }
        }
        for (xi, yj) in pairs {
            self.modified(&ga[xi], &gb[yj]);
        }
    }

    /// Statements the similarity score left unpaired, paired in order by
    /// kind within each stretch between two scored pairs. Two SELECTs with
    /// rewritten expressions (`SELECT FLOOR(RAND())` against `SELECT 0e0`)
    /// share few tokens, yet each side still runs one SELECT there: calling
    /// them removed and added would claim a result set without counterpart.
    /// As a pair they are "modified" (unknown) and statement equivalence
    /// gets to decide them. Only effectful leaf statements are paired, never
    /// transaction control, and never one whose exact text appears on the
    /// other side (that one may have moved).
    fn pair_by_kind(
        &self,
        ga: &[Unit],
        gb: &[Unit],
        pairs: &[(usize, usize)],
    ) -> Vec<(usize, usize)> {
        let eligible = |u: &Unit, other: &HashSet<String>| {
            u.children.is_empty()
                && u.kind.is_effectful()
                && !u.kind.is_txn_control()
                && !other.contains(&u.full)
        };
        let key = |u: &Unit| {
            // DDL of different verbs or object kinds (`CREATE TABLE` against
            // `DROP INDEX`) are not edits of one another.
            let lead = if u.kind == StmtKind::Ddl {
                u.toks
                    .iter()
                    .take(2)
                    .map(|t| t.loose.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                String::new()
            };
            (u.node.kind(), u.kind, lead)
        };
        let mut out = Vec::new();
        let (mut ai, mut bj) = (0, 0);
        for (i, j) in pairs
            .iter()
            .copied()
            .chain(std::iter::once((ga.len(), gb.len())))
        {
            let xs: Vec<usize> = (ai..i)
                .filter(|&k| eligible(&ga[k], &self.hashes_b))
                .collect();
            let ys: Vec<usize> = (bj..j)
                .filter(|&k| eligible(&gb[k], &self.hashes_a))
                .collect();
            if !xs.is_empty() && !ys.is_empty() {
                let ka: Vec<_> = xs.iter().map(|&k| key(&ga[k])).collect();
                let kb: Vec<_> = ys.iter().map(|&k| key(&gb[k])).collect();
                out.extend(
                    crate::normalize::lcs_pairs(&ka, &kb)
                        .into_iter()
                        .map(|(p, q)| (xs[p], ys[q])),
                );
            }
            ai = i + 1;
            bj = j + 1;
        }
        out
    }

    fn modified(&mut self, x: &Unit, y: &Unit) {
        if x.children.is_empty() && y.children.is_empty() {
            self.push_change(x, y, &x.toks, &y.toks, "stmt.modified");
            return;
        }
        if x.header_hash != y.header_hash {
            self.push_change(x, y, &x.header, &y.header, "header.modified");
        } else {
            self.out
                .receipts
                .push(Receipt::Shell(x.node.clone(), y.node.clone()));
        }
        self.diff_lists(&x.children, &y.children);
    }

    fn push_change(&mut self, x: &Unit, y: &Unit, tx: &[NormTok], ty: &[NormTok], rule: &str) {
        let hunks = token_hunks(tx, ty);
        let mut class = x.kind.class().max(y.kind.class());
        let touches_txn = hunks.iter().any(|(r, a)| {
            r.iter().chain(a).any(|t| {
                matches!(
                    t.to_ascii_uppercase().as_str(),
                    "COMMIT"
                        | "ATOMIC"
                        | "XACT_ABORT"
                        | "ROLLBACK"
                        | "SAVEPOINT"
                        | "NONE"
                        | "NC"
                        | "CHG"
                        | "CS"
                        | "RR"
                )
            })
        });
        if touches_txn {
            class = ChangeClass::Transactional;
        }
        if class == ChangeClass::Structural {
            self.out.structural += 1;
            let part = |u: &Unit| {
                if rule == "header.modified" {
                    Part::Own(u.node.clone())
                } else {
                    Part::Whole(u.node.clone())
                }
            };
            self.out.receipts.push(Receipt::Judged {
                rule: "a change of structure only (treediff)",
                a: Some(part(x)),
                b: Some(part(y)),
            });
            return;
        }
        let what = match (rule, x.kind) {
            ("header.modified", StmtKind::Routine) => "Routine definition".to_string(),
            ("header.modified", k) => format!("{} header", capitalize(k.label())),
            (_, k) => capitalize(k.label()),
        };
        let changes: Vec<String> = hunks
            .iter()
            .take(3)
            .map(|(r, a)| match (r.is_empty(), a.is_empty()) {
                (false, false) => format!("`{}` became `{}`", snippet_of(r, 60), snippet_of(a, 60)),
                (false, true) => format!("`{}` was removed", snippet_of(r, 60)),
                (true, false) => format!("`{}` was added", snippet_of(a, 60)),
                (true, true) => String::new(),
            })
            .collect();
        let more = if hunks.len() > 3 {
            format!(" (+{} more)", hunks.len() - 3)
        } else {
            String::new()
        };
        let witness = format!(
            "{what} changed (A line {}, B line {}): {}{more}. Equivalence is not established.",
            self.a.line_of(&x.node),
            self.b.line_of(&y.node),
            changes.join("; ")
        );
        self.out.findings.push(Finding {
            fingerprint: fingerprint(&FingerprintInput {
                object: self.object(),
                class,
                rule,
                anchor_a: &x.full,
                anchor_b: &y.full,
            }),
            rule: rule.to_string(),
            class,
            severity: class.default_severity(),
            verdict: Verdict::Unknown,
            witness,
            a: Some(self.a.node_location(&x.node)),
            b: Some(self.b.node_location(&y.node)),
            assumptions: Vec::new(),
        });
    }

    fn resolve_unpaired(&mut self) {
        let removed = std::mem::take(&mut self.removed);
        let mut added: Vec<Option<Unit>> = std::mem::take(&mut self.added)
            .into_iter()
            .map(Some)
            .collect();
        for x in removed {
            let moved = added
                .iter()
                .position(|y| y.as_ref().is_some_and(|y| y.full == x.full));
            match moved {
                Some(k) => {
                    let y = added[k].take().unwrap();
                    self.push_moved(&x, &y);
                }
                None => self.push_unpaired(&x, true),
            }
        }
        for y in added.into_iter().flatten() {
            self.push_unpaired(&y, false);
        }
    }

    fn push_moved(&mut self, x: &Unit, y: &Unit) {
        let crossed = self.crossed_txn_statement(x, y);
        let (class, verdict) = if x.kind.is_txn_control() || crossed.is_some() {
            (ChangeClass::Transactional, Verdict::Different)
        } else if x.effectful {
            (ChangeClass::Semantic, Verdict::Unknown)
        } else {
            self.out.structural += 1;
            self.out.receipts.push(Receipt::Judged {
                rule: "a statement without effects moved (treediff)",
                a: Some(Part::Whole(x.node.clone())),
                b: Some(Part::Whole(y.node.clone())),
            });
            return;
        };
        let witness = format!(
            "{} `{}` moved from A line {} to B line {}, so it runs {}.",
            capitalize(x.kind.label()),
            self.a.snippet(&x.node, 60),
            self.a.line_of(&x.node),
            self.b.line_of(&y.node),
            match &crossed {
                Some(t) => format!("on the other side of `{t}`: it is inside the transaction on one side and outside it on the other"),
                None => "in a different order relative to the statements around it".to_string(),
            }
        );
        self.out.findings.push(Finding {
            fingerprint: fingerprint(&FingerprintInput {
                object: self.object(),
                class,
                rule: "stmt.moved",
                anchor_a: &x.full,
                anchor_b: &y.full,
            }),
            rule: "stmt.moved".to_string(),
            class,
            severity: class.default_severity(),
            verdict,
            witness,
            a: Some(self.a.node_location(&x.node)),
            b: Some(self.b.node_location(&y.node)),
            assumptions: Vec::new(),
        });
    }

    /// A transaction-control sibling that the moved statement now runs on the other side of.
    fn crossed_txn_statement(&self, x: &Unit, y: &Unit) -> Option<String> {
        let sibs = |u: &Unit, an: &Analyzed| -> Vec<(String, StmtKind, bool, String)> {
            let Some(parent) = u.node.parent() else {
                return Vec::new();
            };
            ast::structural_children(&parent)
                .into_iter()
                .map(|n| {
                    let h = loose_hash(&norm_tokens(&n, an.dialect()));
                    (
                        h,
                        stmt_kind(&n, an.dialect()),
                        n == u.node,
                        an.snippet(&n, 40),
                    )
                })
                .collect()
        };
        let sa = sibs(x, self.a);
        let sb = sibs(y, self.b);
        let pos = |v: &[(String, StmtKind, bool, String)]| v.iter().position(|e| e.2);
        let (px, py) = (pos(&sa)?, pos(&sb)?);
        for (i, (h, k, me, text)) in sa.iter().enumerate() {
            if *me || !k.is_txn_control() {
                continue;
            }
            let Some(j) = sb.iter().position(|e| &e.0 == h && !e.2) else {
                continue;
            };
            if (px < i) != (py < j) {
                return Some(text.clone());
            }
        }
        None
    }

    fn push_unpaired(&mut self, u: &Unit, removed: bool) {
        if u.node.kind() == SyntaxKind::LABEL {
            self.out.structural += 1;
            let p = Some(Part::Whole(u.node.clone()));
            self.out.receipts.push(Receipt::Judged {
                rule: "a label added or removed (treediff)",
                a: if removed { p.clone() } else { None },
                b: if removed { None } else { p },
            });
            return;
        }
        let (class, verdict) = if u.effectful {
            let c = if u.kind.is_effectful() {
                u.kind.class()
            } else {
                // Compound: the most severe class among its statements.
                max_child_class(u).unwrap_or(ChangeClass::Semantic)
            };
            (c, Verdict::Different)
        } else {
            (ChangeClass::Structural, Verdict::Unknown)
        };
        let side = if removed { self.a } else { self.b };
        let (this, other) = if removed { ("A", "B") } else { ("B", "A") };
        let witness = format!(
            "Side {this} runs {} `{}` (line {}); side {other} has no counterpart.",
            article(u.kind.label()),
            side.snippet(&u.node, 80),
            side.line_of(&u.node)
        );
        let rule = if removed {
            "stmt.removed"
        } else {
            "stmt.added"
        };
        let (anchor_a, anchor_b) = if removed {
            (u.full.as_str(), "")
        } else {
            ("", u.full.as_str())
        };
        let loc = side.node_location(&u.node);
        self.out.findings.push(Finding {
            fingerprint: fingerprint(&FingerprintInput {
                object: self.object(),
                class,
                rule,
                anchor_a,
                anchor_b,
            }),
            rule: rule.to_string(),
            class,
            severity: class.default_severity(),
            verdict,
            witness,
            a: removed.then(|| loc.clone()),
            b: (!removed).then_some(loc),
            assumptions: Vec::new(),
        });
    }
}

fn max_child_class(u: &Unit) -> Option<ChangeClass> {
    u.children
        .iter()
        .filter_map(|c| {
            if c.kind.is_effectful() {
                Some(c.kind.class())
            } else {
                max_child_class(c)
            }
        })
        .max_by_key(|c| c.default_severity())
}

pub fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) if f.is_lowercase() => f.to_uppercase().collect::<String>() + c.as_str(),
        Some(f) => f.to_string() + c.as_str(),
        None => String::new(),
    }
}

pub fn article(label: &str) -> String {
    let first = label.chars().next().unwrap_or('x').to_ascii_lowercase();
    let an = matches!(first, 'a' | 'e' | 'i' | 'o' | 'u')
        || label.starts_with("UPDATE")
        || label.starts_with("INSERT")
        || label.starts_with("IF");
    let is_upper_kw = label.chars().all(|c| c.is_ascii_uppercase() || c == '/');
    if is_upper_kw {
        // "an UPDATE", "a COMMIT", "a SELECT"
        let an = matches!(label.chars().next(), Some('U' | 'I' | 'E' | 'A' | 'O'));
        format!("{} {label}", if an { "an" } else { "a" })
    } else {
        format!("{} {label}", if an { "an" } else { "a" })
    }
}
