//! Measurement tool for perf plan W6 (pair-states). Not shipped.
//!
//! `pairstates SRC_DIR OUT_DIR FROM_DIALECT TO_DIALECT [CAP]` pairs every
//! `.sql` file under SRC_DIR with the file of the same relative path under
//! OUT_DIR (a `sqldiff convert` output) and prints one JSON line per routine
//! pair and environment with the pair-state counts of the four key variants
//! (see `ir::interp::pairs`). Files are visited in sorted order; the output
//! is deterministic.
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
        eprintln!("usage: pairstates SRC OUT FROM TO [CAP]");
        std::process::exit(2);
    }
    let (src, out) = (PathBuf::from(&a[1]), PathBuf::from(&a[2]));
    let from: Dialect = a[3].parse().expect("from dialect");
    let to: Dialect = a[4].parse().expect("to dialect");
    let cap: usize = a.get(5).map_or(20000, |x| x.parse().unwrap());
    let mut files = Vec::new();
    walk(&src, &mut files);
    for f in files {
        let rel = f.strip_prefix(&src).unwrap().to_path_buf();
        let g = out.join(&rel);
        let (Ok(ta), Ok(tb)) = (std::fs::read_to_string(&f), std::fs::read_to_string(&g)) else {
            continue;
        };
        let rel_s = rel.to_string_lossy().replace('\\', "/");
        let rows = sqldiff_core::with_engine_stack(|| {
            let aa = Artifact::new(rel_s.clone(), from, ta);
            let bb = Artifact::new(rel_s.clone(), to, tb);
            sqldiff_core::pair_states(&aa, &bb, &Options::default(), cap)
        });
        for r in rows {
            let js = |m: &sqldiff_core::ir::interp::pairs::Measure| {
                serde_json::json!({
                    "paths": m.paths.map(|p| p.to_string()),
                    "states": m.states, "leaves": m.leaves, "ticks": m.ticks,
                    "joins": m.joins, "max_per_join": m.max_per_join,
                    "single_a": m.single_a, "single_b": m.single_b,
                    "max_single": m.max_single, "desync": m.desync, "why": m.why, "capped": m.capped,
                })
            };
            let ms: Vec<_> = r.measures.iter().map(js).collect();
            println!(
                "{}",
                serde_json::json!({
                    "file": rel_s, "routine": r.routine, "env": r.env,
                    "lines_a": r.lines_a, "lines_b": r.lines_b,
                    "nodes_a": r.nodes_a, "nodes_b": r.nodes_b,
                    "decisions_a": r.decisions_a, "decisions_b": r.decisions_b,
                    "matched_a": r.matched_a, "matched_b": r.matched_b,
                    "prod_paths_a": r.prod_paths_a, "prod_paths_b": r.prod_paths_b, "prod_cut": r.prod_cut, "join_paths_a": r.join_paths_a, "join_paths_b": r.join_paths_b, "joined_a": r.joined_a, "joined_b": r.joined_b, "join_cut": r.join_cut, "gate_a": r.gate_a, "gate_b": r.gate_b,
                    "variants": ms, "self_a": js(&r.self_a), "self_b": js(&r.self_b),
                })
            );
        }
    }
}
