//! Turns binding results into report findings: early-check issues, and
//! references that resolve to a different declaration on the two sides of a
//! same-dialect diff (the effect of a new shadowing declaration).

use std::collections::{BTreeMap, HashSet};

use super::{Bound, DeclKind, Issue, Resolution, Target};
use crate::artifact::Analyzed;
use crate::model::{
    fingerprint, Assumption, ChangeClass, Finding, FingerprintInput, Severity, Side, Verdict,
};

/// Binding results of a diff or check.
pub struct Bindings<'a> {
    pub a: (&'a Analyzed<'a>, &'a Bound, bool),
    /// Side B, when diffing. The flag says whether a catalog was supplied.
    pub b: Option<(&'a Analyzed<'a>, &'a Bound, bool)>,
}

/// Findings and assumptions contributed by binding.
pub fn binding_findings(x: &Bindings) -> (Vec<Finding>, Vec<Assumption>) {
    let mut findings = Vec::new();
    let mut assumptions: Vec<Assumption> = Vec::new();
    let (aa, ba, cat_a) = x.a;
    let push_asm = |out: &mut Vec<Assumption>, b: &Bound, only: Option<&HashSet<String>>| {
        for r in &b.refs {
            if let Resolution::Assumed(id) = &r.resolution {
                if only.is_some_and(|o| o.contains(id)) {
                    continue;
                }
                if let Some(asm) = b.assumption(id) {
                    if !out.iter().any(|y| y.id == asm.id) {
                        out.push(asm.clone());
                    }
                }
            }
        }
        for i in &b.issues {
            for id in &i.assumptions {
                if let Some(asm) = b.assumption(id) {
                    if !out.iter().any(|y| y.id == asm.id) {
                        out.push(asm.clone());
                    }
                }
            }
        }
    };
    match x.b {
        None => {
            for i in &ba.issues {
                findings.push(to_finding(aa, i, verdict(i), None));
            }
            if cat_a {
                push_asm(&mut assumptions, ba, None);
            }
        }
        Some((ab, bb, cat_b)) => {
            let same = aa.dialect() == ab.dialect();
            let key = |i: &Issue| format!("{}|{}", i.rule, i.key);
            let keys_a: HashSet<String> = ba.issues.iter().map(key).collect();
            let keys_b: HashSet<String> = bb.issues.iter().map(key).collect();
            for i in &bb.issues {
                if keys_a.contains(&key(i)) {
                    continue;
                }
                findings.push(to_finding(ab, i, verdict(i), Some(aa)));
            }
            for i in &ba.issues {
                if keys_b.contains(&key(i)) {
                    continue;
                }
                // Same dialect: a non-proven issue that B removed is not a difference.
                if same && !i.proven {
                    continue;
                }
                findings.push(to_finding(aa, i, verdict(i), Some(ab)));
            }
            if same {
                findings.extend(rebound(aa, ba, ab, bb));
                if cat_b {
                    let assumed_a: HashSet<String> = ba
                        .refs
                        .iter()
                        .filter_map(|r| match &r.resolution {
                            Resolution::Assumed(id) => Some(id.clone()),
                            _ => None,
                        })
                        .collect();
                    push_asm(&mut assumptions, bb, Some(&assumed_a));
                }
            } else {
                if cat_a {
                    push_asm(&mut assumptions, ba, None);
                }
                if cat_b {
                    push_asm(&mut assumptions, bb, None);
                }
            }
        }
    }
    (findings, assumptions)
}

fn verdict(i: &Issue) -> Verdict {
    if i.proven {
        Verdict::Different
    } else {
        Verdict::Unknown
    }
}

fn to_finding(side: &Analyzed, i: &Issue, verdict: Verdict, other: Option<&Analyzed>) -> Finding {
    let loc = side.location(i.start, i.end);
    let (line, _) = side.lines.line_col(i.start);
    let tail = match other {
        Some(o) if o.dialect() == side.dialect() => {
            " The other side does not have this problem.".to_string()
        }
        _ => String::new(),
    };
    Finding {
        fingerprint: fingerprint(&FingerprintInput {
            object: &side.object,
            class: i.class,
            rule: i.rule,
            anchor_a: if side.side == Side::A { &i.key } else { "" },
            anchor_b: if side.side == Side::B { &i.key } else { "" },
        }),
        rule: i.rule.to_string(),
        class: i.class,
        severity: i.severity,
        verdict,
        witness: format!("Side {:?} line {line}: {}{tail}", side.side, i.message),
        a: (side.side == Side::A).then(|| loc.clone()),
        b: (side.side == Side::B).then_some(loc),
        assumptions: i.assumptions.clone(),
    }
}

/// A variable reference as seen from its statement: statement text,
/// variable key, occurrence within the statement.
type RefKey = (String, String, usize);

struct Use {
    decl: usize,
    stmt: (usize, usize),
    start: usize,
}

fn uses(side: &Analyzed, b: &Bound) -> BTreeMap<RefKey, Use> {
    let mut seen: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut out = BTreeMap::new();
    for r in &b.refs {
        let Resolution::Resolved(Target::Decl(d)) = r.resolution else {
            continue;
        };
        let decl = &b.decls[d];
        if !matches!(
            decl.kind,
            DeclKind::Variable | DeclKind::Parameter | DeclKind::LoopVar
        ) {
            continue;
        }
        let text = normalize(&side.artifact.text[r.stmt.0..r.stmt.1]);
        let n = seen.entry((text.clone(), decl.key.clone())).or_insert(0);
        out.insert(
            (text, decl.key.clone(), *n),
            Use {
                decl: d,
                stmt: r.stmt,
                start: r.start,
            },
        );
        *n += 1;
    }
    out
}

fn normalize(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_uppercase()
}

/// Same statement, same variable, different declaration: a shadowing
/// declaration captured references that used to reach another variable.
fn rebound(aa: &Analyzed, ba: &Bound, ab: &Analyzed, bb: &Bound) -> Vec<Finding> {
    let shadowed: HashSet<String> = ba
        .issues
        .iter()
        .chain(&bb.issues)
        .filter(|i| i.rule == "bind.shadowed-variable")
        .filter_map(|i| i.key.strip_prefix("shadow ").map(str::to_string))
        .collect();
    if shadowed.is_empty() {
        return Vec::new();
    }
    let ua = uses(aa, ba);
    let ub = uses(ab, bb);
    let mut out = Vec::new();
    let mut done: HashSet<(usize, String)> = HashSet::new();
    for (k, u_b) in &ub {
        let Some(u_a) = ua.get(k) else { continue };
        let (da, db) = (&ba.decls[u_a.decl], &bb.decls[u_b.decl]);
        if da.ordinal == db.ordinal || !shadowed.contains(&db.key) {
            continue;
        }
        if !done.insert((u_b.stmt.0, db.key.clone())) {
            continue;
        }
        let line_b = ab.lines.line_col(u_b.start).0;
        let line_a = aa.lines.line_col(u_a.start).0;
        let decl_line = |s: &Analyzed, off: usize| s.lines.line_col(off).0;
        let snippet = crate::artifact::snippet_of(
            &ab.artifact.text[u_b.stmt.0..u_b.stmt.1]
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>(),
            70,
        );
        // Who still reads the declaration A used?
        let reader = ub.values().find(|u| {
            bb.decls[u.decl].key == db.key
                && bb.decls[u.decl].ordinal == da.ordinal
                && u.stmt != u_b.stmt
        });
        let tail = reader
            .map(|u| {
                format!(
                    " The `{}` that line {} reads is no longer updated here, so the two sides return different values.",
                    da.name,
                    ab.lines.line_col(u.start).0
                )
            })
            .unwrap_or_default();
        let witness = format!(
            "`{snippet}`: on side A (line {line_a}) `{}` is the variable declared on line {}; on side B (line {line_b}) it is the `{}` declared on line {}, which shadows it.{tail}",
            da.name,
            decl_line(aa, da.start),
            db.name,
            decl_line(ab, db.start),
        );
        let anchor = format!("{} {}", k.0, k.1);
        let class = ChangeClass::Semantic;
        out.push(Finding {
            fingerprint: fingerprint(&FingerprintInput {
                object: &aa.object,
                class,
                rule: "bind.rebound-variable",
                anchor_a: &anchor,
                anchor_b: &anchor,
            }),
            rule: "bind.rebound-variable".to_string(),
            class,
            severity: Severity::High,
            verdict: Verdict::Different,
            witness,
            a: Some(aa.location(u_a.stmt.0, u_a.stmt.1)),
            b: Some(ab.location(u_b.stmt.0, u_b.stmt.1)),
            assumptions: Vec::new(),
        });
    }
    out
}
