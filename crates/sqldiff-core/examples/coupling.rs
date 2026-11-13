//! Report for the region-comparison design, stage 0a (variable coupling).
//! Not shipped.
//!
//! `coupling SRC_DIR OUT_DIR FROM_DIALECT TO_DIALECT` pairs every `.sql`
//! file under SRC_DIR with the file of the same relative path under OUT_DIR
//! and prints one JSON line per routine pair with the class counts of the
//! paired conditions under (a) loops asynchronous and (b) loop-head
//! fixpoint, for kappa_ff and kappa_all, then one line per condition.
//! Files are visited in sorted order; the output is deterministic.
use sqldiff_core::equiv::coupling::CouplingRow;
use sqldiff_core::{Artifact, Options};
use std::collections::BTreeMap;
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

fn counts(
    rows: &[CouplingRow],
    f: impl Fn(&CouplingRow) -> &'static str,
) -> BTreeMap<&'static str, usize> {
    let mut m = BTreeMap::new();
    for r in rows {
        *m.entry(f(r)).or_insert(0) += 1;
    }
    m
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 5 {
        eprintln!("usage: coupling SRC OUT FROM TO");
        std::process::exit(2);
    }
    let (src, out) = (PathBuf::from(&a[1]), PathBuf::from(&a[2]));
    let from = a[3].parse().expect("from dialect");
    let to = a[4].parse().expect("to dialect");
    let mut files = Vec::new();
    walk(&src, &mut files);
    for f in files {
        let rel = f.strip_prefix(&src).unwrap().to_path_buf();
        let g = out.join(&rel);
        let (Ok(ta), Ok(tb)) = (std::fs::read_to_string(&f), std::fs::read_to_string(&g)) else {
            continue;
        };
        let rel_s = rel.to_string_lossy().replace('\\', "/");
        let (reports, work) = sqldiff_core::with_engine_stack(|| {
            let aa = Artifact::new(rel_s.clone(), from, ta);
            let bb = Artifact::new(rel_s.clone(), to, tb);
            sqldiff_core::coupling(&aa, &bb, &Options::default())
        });
        for (u, r) in reports.iter().enumerate() {
            println!(
                "{}",
                serde_json::json!({
                    "file": rel_s, "unit": u, "candidates": r.candidates,
                    "paired": r.rows.len(), "refused": r.refused,
                    "classes_a": counts(&r.rows, |c| c.ff_a.label()),
                    "classes_b": counts(&r.rows, |c| c.ff_b.label()),
                    "classes_all_a": counts(&r.rows, |c| c.all_a.label()),
                    "classes_all_b": counts(&r.rows, |c| c.all_b.label()),
                    "exit_ff": [r.ff_a.exit, r.ff_b.exit],
                    "exit_all": [r.all_a.exit, r.all_b.exit],
                    "rounds_ff_b": r.ff_b.rounds, "rounds_all_b": r.all_b.rounds,
                    "steps": r.ff_a.steps + r.ff_b.steps + r.all_a.steps + r.all_b.steps,
                })
            );
            for c in &r.rows {
                println!(
                    "{}",
                    serde_json::json!({
                        "file": rel_s, "unit": u, "kind": c.kind,
                        "a_line": c.a_line, "b_line": c.b_line,
                        "ff_a": c.ff_a.label(), "ff_b": c.ff_b.label(),
                        "all_a": c.all_a.label(), "all_b": c.all_b.label(),
                        "why_ff_b": c.ff_b.why(),
                    })
                );
            }
        }
        println!(
            "{}",
            serde_json::json!({"file": rel_s, "work": work.to_string()})
        );
    }
}
