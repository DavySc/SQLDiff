//! The check reads nothing outside its arguments (design review of
//! incremental large conversions, step 2): a stored result is trusted when
//! its key covers everything the check reads, and "the compiler enforces it"
//! holds for arguments only, not for the environment, files, clocks or
//! statics. This test lists, per source file, the constructs that can carry
//! such a read. A new file or a new construct fails it: read the code, and
//! either keep the read out of the check (a debug print, a test, tree
//! orchestration that is not part of one file's check) or make it part of
//! the input the key hashes, then add the entry here.
//!
//! What each entry is, for the reader who adds one:
//! - `env::var` in `equiv`: SQLDIFF_DEBUG_* switches that only print to
//!   stderr; a geography test reads its fuzz files. Neither changes a result.
//! - `thread_local!`, `OnceLock`, `OnceCell`, `Mutex`: memoisation of
//!   values that depend on the build (profiles, builtins, rule sets) or on
//!   the arguments, and scoped modes the check sets itself around its diffs
//!   (`ci_collation_scope`, folding, `without_continuation`), constants of
//!   the code, not of the environment.
//! - `fs::`, `File::open`, `current_exe`, `Instant::now` in `sqldiff-convert`:
//!   the tree run (reading sources, writing the output, timing the report)
//!   and the build hash of `instances.rs`; `store.rs` reads and writes the
//!   result store and seeds the sampled re-check, none of which the check sees; timings are kept out of the
//!   bytes a stored result is compared by; `tree.rs` also memoises the tree
//!   claims (derived from the inputs) in a `OnceLock` for the per-routine rebuild.
//! - `abstract_syntax.rs` (`sqldiff-syntax`) keeps a bounded per-thread cache of the
//!   tokens of a node (`tokens_shared`), a pure function of the node.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const PATTERNS: &[&str] = &[
    "env::var",
    "current_exe",
    "SystemTime",
    "Instant::now",
    "fs::",
    "File::open",
    "thread_local!",
    "static mut",
    "OnceLock",
    "OnceCell",
    "Mutex",
    // Nothing the check computes may depend on a per-process hash seed or
    // on which thread runs it.
    "DefaultHasher",
    "RandomState",
    "thread::current",
];

const ALLOWED: &[(&str, &[&str])] = &[
    ("sqldiff-convert/src/base.rs", &["Mutex", "fs::"]),
    ("sqldiff-convert/src/drift.rs", &["fs::"]),
    ("sqldiff-convert/src/gate.rs", &["fs::"]),
    (
        "sqldiff-convert/src/instances.rs",
        &["File::open", "OnceLock", "current_exe", "fs::"],
    ),
    ("sqldiff-convert/src/job.rs", &["fs::"]),
    ("sqldiff-convert/src/queue.rs", &["fs::"]),
    ("sqldiff-convert/src/review.rs", &["fs::"]),
    ("sqldiff-convert/src/shared.rs", &["Mutex"]),
    ("sqldiff-convert/src/store.rs", &["fs::", "thread::current"]),
    (
        "sqldiff-convert/src/tree.rs",
        &["Instant::now", "Mutex", "OnceLock", "fs::"],
    ),
    (
        "sqldiff-convert/src/tsql.rs",
        &["OnceCell", "thread_local!"],
    ),
    ("sqldiff-core/src/assumptions.rs", &["OnceLock"]),
    ("sqldiff-syntax/src/abstract_syntax.rs", &["thread_local!"]),
    ("sqldiff-catalog/src/builtins.rs", &["OnceLock"]),
    ("sqldiff-catalog/src/ddl.rs", &["fs::"]),
    ("sqldiff-core/src/engine.rs", &["Mutex", "OnceLock"]),
    (
        "sqldiff-core/src/equiv/canon.rs",
        &["Mutex", "OnceLock", "thread_local!"],
    ),
    ("sqldiff-core/src/equiv/compare.rs", &["thread_local!"]),
    (
        "sqldiff-core/src/equiv/edit.rs",
        &["OnceCell", "env::var", "thread_local!"],
    ),
    ("sqldiff-core/src/equiv/eval.rs", &["env::var"]),
    ("sqldiff-core/src/equiv/geography.rs", &["env::var", "fs::"]),
    ("sqldiff-core/src/equiv/mod.rs", &["OnceCell", "env::var"]),
    ("sqldiff-core/src/equiv/schema.rs", &["OnceLock", "fs::"]),
    ("sqldiff-core/src/format/tests.rs", &["fs::"]),
    ("sqldiff-core/src/ir/behaviour.rs", &["OnceCell"]),
    ("sqldiff-core/src/ir/profile.rs", &["OnceLock"]),
    ("sqldiff-core/src/lint.rs", &["OnceLock"]),
    ("sqldiff-core/src/rule_proofs.rs", &["OnceLock"]),
    ("sqldiff-core/src/rules/rule.rs", &["OnceLock"]),
    ("sqldiff-core/src/target.rs", &["Mutex", "OnceLock"]),
];

fn scan(dir: &Path, root: &Path, out: &mut BTreeMap<String, BTreeSet<&'static str>>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            scan(&p, root, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            let name = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            for line in std::fs::read_to_string(&p).unwrap().lines() {
                let s = line.trim();
                if s.starts_with("//") {
                    continue;
                }
                for q in PATTERNS {
                    if s.contains(q) {
                        out.entry(name.clone()).or_default().insert(*q);
                    }
                }
            }
        }
    }
}

#[test]
fn the_check_reads_nothing_outside_its_arguments() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut found = BTreeMap::new();
    for krate in [
        "sqldiff-core",
        "sqldiff-convert",
        "sqldiff-syntax",
        "sqldiff-catalog",
    ] {
        scan(&root.join(krate).join("src"), &root, &mut found);
    }
    let allowed: BTreeMap<&str, BTreeSet<&str>> = ALLOWED
        .iter()
        .map(|(f, ps)| (*f, ps.iter().copied().collect()))
        .collect();
    let mut bad = Vec::new();
    for (f, ps) in &found {
        let ok = allowed.get(f.as_str());
        for p in ps {
            if !ok.is_some_and(|s| s.contains(p)) {
                bad.push(format!("{f}: {p}"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "new ambient-read constructs (see the header of this test): {bad:#?}"
    );
}
