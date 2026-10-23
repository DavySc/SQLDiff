//! Statement equivalence against the shared corpus (corpus/).
//!
//! Soundness (design section 12): the engine never says "equal" (or "equal
//! under assumptions") for a pair the corpus expects to differ, with or
//! without catalog facts. With the corpus catalogs loaded, every expectation
//! not listed in `KNOWN_MISSES` must match exactly, so regressions fail and
//! newly matching cases can be removed from the list.

use std::path::{Path, PathBuf};

use sqldiff_core::{diff, Artifact, Dialect, Options, Verdict};

/// Expectations the engine does not reach yet: (case id, side a, side b).
/// Each is still checked for soundness.
const KNOWN_MISSES: &[(&str, &str, &str)] = &[
    // A failed INSERT before `SET @id = SCOPE_IDENTITY()` continues with XACT_ABORT off;
    // the error-path difference (an OUTPUT parameter set to NULL) needs the IR.
    ("p07-insert-return-id", "tsql", "pg"),
    ("p07-insert-return-id", "tsql", "db2i"),
    // Named-lock idioms (sp_getapplock, advisory locks, row-lock emulation) are not lifted.
    ("p13-named-lock", "tsql", "pg"),
    ("p13-named-lock", "tsql", "db2i"),
    ("p13-named-lock", "pg", "db2i"),
    // Same-dialect mutations outside statement equivalence (handlers, binding, lints).
    ("m14-terminator-before-throw", "original", "mutated"),
    ("m15-dropped-column-reference", "original", "mutated"),
    ("m16-outdated-signature", "original", "mutated"),
    ("m18-dialect-residue", "original", "mutated"),
];

fn corpus() -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
    p.join("MANIFEST.toml").exists().then_some(p)
}

fn verdict(s: &str) -> Verdict {
    match s {
        "equal" => Verdict::Equal,
        "equal-under-assumptions" => Verdict::EqualUnderAssumptions,
        "different" => Verdict::Different,
        _ => Verdict::Unknown,
    }
}

/// `got` claims no more than `expected` allows.
fn sound(got: Verdict, expected: Verdict) -> bool {
    match got {
        Verdict::Equal => expected == Verdict::Equal,
        Verdict::EqualUnderAssumptions => {
            matches!(expected, Verdict::Equal | Verdict::EqualUnderAssumptions)
        }
        Verdict::Unknown | Verdict::Different => true,
    }
}

fn catalog(root: &Path, d: Dialect) -> Artifact {
    let file = match d {
        Dialect::Tsql => "tsql.sql",
        Dialect::Postgres => "pg.sql",
        Dialect::Db2i => "db2i.sql",
    };
    let path = root.join("catalog").join(file);
    Artifact::new(
        path.display().to_string(),
        d,
        std::fs::read_to_string(&path).unwrap(),
    )
}

struct Case {
    id: String,
    a_name: String,
    b_name: String,
    a: Artifact,
    b: Artifact,
    expected: Verdict,
}

fn cases(root: &Path) -> Vec<Case> {
    let mut out = Vec::new();
    for group in ["pairs", "mutations"] {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(root.join(group))
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.join("case.toml").exists())
            .collect();
        dirs.sort();
        for dir in dirs {
            let case: toml::Value =
                toml::from_str(&std::fs::read_to_string(dir.join("case.toml")).unwrap()).unwrap();
            let id = case["id"].as_str().unwrap().to_string();
            let side = |name: &str| {
                let s = &case["sides"][name];
                let d: Dialect = s["dialect"].as_str().unwrap().parse().unwrap();
                let path = dir.join(s["file"].as_str().unwrap());
                Artifact::new(
                    path.display().to_string(),
                    d,
                    std::fs::read_to_string(&path).unwrap(),
                )
            };
            for e in case["expect"].as_array().unwrap() {
                let (a, b) = (e["a"].as_str().unwrap(), e["b"].as_str().unwrap());
                out.push(Case {
                    id: id.clone(),
                    a_name: a.to_string(),
                    b_name: b.to_string(),
                    a: side(a),
                    b: side(b),
                    expected: verdict(e["verdict"].as_str().unwrap()),
                });
            }
        }
    }
    out
}

#[test]
fn corpus_verdicts_are_sound_and_match() {
    let Some(root) = corpus() else { return };
    let mut unsound = Vec::new();
    let mut regressed = Vec::new();
    let mut fixed = Vec::new();
    let mut matched = 0;
    let all = cases(&root);
    for c in &all {
        let plain = diff(&c.a, &c.b, &Options::default());
        let with_catalog = diff(
            &c.a,
            &c.b,
            &Options {
                catalog_a: vec![catalog(&root, c.a.dialect)],
                catalog_b: vec![catalog(&root, c.b.dialect)],
                ..Options::default()
            },
        );
        let label = format!("{} {}/{}", c.id, c.a_name, c.b_name);
        for (mode, r) in [("no catalog", &plain), ("catalog", &with_catalog)] {
            if !sound(r.verdict, c.expected) {
                unsound.push(format!(
                    "{label} ({mode}): got {} but expected {}",
                    r.verdict.label(),
                    c.expected.label()
                ));
            }
        }
        let known_miss = KNOWN_MISSES
            .iter()
            .any(|(i, a, b)| *i == c.id && *a == c.a_name && *b == c.b_name);
        let hit = with_catalog.verdict == c.expected;
        matched += usize::from(hit);
        if !hit && !known_miss {
            regressed.push(format!(
                "{label}: got {} but expected {}",
                with_catalog.verdict.label(),
                c.expected.label()
            ));
        }
        if hit && known_miss {
            fixed.push(label);
        }
    }
    eprintln!(
        "corpus: {matched} of {} expectations match with catalogs",
        all.len()
    );
    assert!(unsound.is_empty(), "false equal:\n{}", unsound.join("\n"));
    assert!(
        regressed.is_empty(),
        "no longer matching:\n{}",
        regressed.join("\n")
    );
    assert!(
        fixed.is_empty(),
        "now matching, remove from KNOWN_MISSES:\n{}",
        fixed.join("\n")
    );
}
