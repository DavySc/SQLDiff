//! Report for perf plan W6c step 1 (aligned merging, identified conditions).
//! Not shipped.
//!
//! `identity SRC_DIR OUT_DIR FROM_DIALECT TO_DIALECT` pairs every `.sql`
//! file under SRC_DIR with the file of the same relative path under OUT_DIR
//! and prints one JSON line per paired condition and one per routine pair
//! (`conds_a`, `conds_b`, refusal). Files are visited in sorted order; the
//! output is deterministic. Scope of the facts: fault-free runs only.
use sqldiff_core::{Artifact, Dialect, Options};
use std::path::{Path, PathBuf};

fn walk(d: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(d) else { return };
    let mut v: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    v.sort();
    for p in v {
        if p.is_dir() {
            if p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            {
                continue;
            }
            walk(&p, out);
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("sql")) {
            out.push(p);
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 5 {
        eprintln!("usage: identity SRC OUT FROM TO");
        std::process::exit(2);
    }
    let (src, out) = (PathBuf::from(&a[1]), PathBuf::from(&a[2]));
    let from: Dialect = a[3].parse().expect("from dialect");
    let to: Dialect = a[4].parse().expect("to dialect");
    let mut files = Vec::new();
    walk(&src, &mut files);
    for f in files {
        let rel = f.strip_prefix(&src).unwrap().to_path_buf();
        let g = out.join(&rel);
        let (Ok(ta), Ok(tb)) = (std::fs::read_to_string(&f), std::fs::read_to_string(&g)) else {
            continue;
        };
        let rel_s = rel.to_string_lossy().replace('\\', "/");
        let reports = sqldiff_core::with_engine_stack(|| {
            let aa = Artifact::new(rel_s.clone(), from, ta);
            let bb = Artifact::new(rel_s.clone(), to, tb);
            sqldiff_core::identity(&aa, &bb, &Options::default())
        });
        let shared = sqldiff_core::with_engine_stack(|| {
            let aa = Artifact::new(rel_s.clone(), from, std::fs::read_to_string(&f).unwrap());
            let bb = Artifact::new(rel_s.clone(), to, std::fs::read_to_string(&g).unwrap());
            sqldiff_core::shared_conditions(&aa, &bb, &Options::default())
        });
        // E2: the conditions `pairs()` and `feasible()` treat as one decision
        // because their folded text is equal. Identified only when every
        // condition with that text on either side sits in an identified pair
        // with a condition of the same text on the other side.
        let ident = |a_start: usize, b_start: usize| {
            reports
                .iter()
                .flat_map(|r| &r.rows)
                .any(|c| c.a_start == a_start && c.b_start == b_start && c.class.identified())
        };
        for s in &shared {
            let ok = s.a.iter().all(|x| s.b.iter().any(|y| ident(*x, *y)))
                && s.b.iter().all(|y| s.a.iter().any(|x| ident(*x, *y)));
            println!(
                "{}",
                serde_json::json!({
                    "file": rel_s, "e2": s.routine, "text": s.text,
                    "a": s.a.len(), "b": s.b.len(), "identified": ok,
                })
            );
        }
        for (u, r) in reports.iter().enumerate() {
            println!(
                "{}",
                serde_json::json!({
                    "file": rel_s, "unit": u, "conds_a": r.conds_a, "conds_b": r.conds_b,
                    "paired": r.rows.len(), "refused": r.refused,
                    "scope": sqldiff_core::equiv::identity::SCOPE,
                })
            );
            for c in &r.rows {
                println!(
                    "{}",
                    serde_json::json!({
                        "file": rel_s, "unit": u, "kind": c.kind,
                        "a_line": c.a_line, "b_line": c.b_line,
                        "class": c.class.label(),
                        "why": match &c.class { sqldiff_core::equiv::identity::Class::No(w) => Some(w.clone()), _ => None },
                        "assumptions": c.assumptions,
                    })
                );
            }
        }
    }
}
