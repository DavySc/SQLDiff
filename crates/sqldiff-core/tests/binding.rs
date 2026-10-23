//! Binding and early checks against the corpus (design §3 layer 2, §3 layer 5
//! early checks; work package WP1 gate: m13, m15, m16, m17, m18).

use std::path::{Path, PathBuf};

use sqldiff_core::artifact::Analyzed;
use sqldiff_core::bind::{self, BindInput, Resolution};
use sqldiff_core::catalog::{ddl, Catalog, CatalogView};
use sqldiff_core::model::{ChangeClass, Side};
use sqldiff_core::{diff, Artifact, Dialect, Options, Verdict};

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus")
}

fn dialect_of(file: &str) -> Dialect {
    let stem = file.trim_end_matches(".sql");
    if stem.ends_with("tsql") {
        Dialect::Tsql
    } else if stem.ends_with("pg") {
        Dialect::Postgres
    } else {
        Dialect::Db2i
    }
}

fn catalog(d: Dialect) -> Artifact {
    let f = match d {
        Dialect::Tsql => "tsql.sql",
        Dialect::Postgres => "pg.sql",
        Dialect::Db2i => "db2i.sql",
    };
    let p = corpus().join("catalog").join(f);
    Artifact::new(
        format!("corpus/catalog/{f}"),
        d,
        std::fs::read_to_string(p).unwrap(),
    )
}

fn load(case: &Path, file: &str) -> Artifact {
    let d = dialect_of(file);
    Artifact::new(file, d, std::fs::read_to_string(case.join(file)).unwrap())
}

fn opts(a: Dialect, b: Dialect) -> Options {
    Options {
        catalog_a: vec![catalog(a)],
        catalog_b: vec![catalog(b)],
        ..Options::default()
    }
}

fn run_mutation(name: &str) -> sqldiff_core::Report {
    let dir = std::fs::read_dir(corpus().join("mutations"))
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().starts_with(name))
        .unwrap()
        .path();
    let files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|f| f.ends_with(".sql"))
        .collect();
    let a = files.iter().find(|f| f.starts_with("original")).unwrap();
    let b = files.iter().find(|f| f.starts_with("mutated")).unwrap();
    let (a, b) = (load(&dir, a), load(&dir, b));
    diff(&a, &b, &opts(a.dialect, b.dialect))
}

fn rules(r: &sqldiff_core::Report) -> Vec<(String, ChangeClass, Verdict)> {
    r.findings
        .iter()
        .filter(|f| f.rule.starts_with("bind.") || f.rule.starts_with("types."))
        .map(|f| (f.rule.clone(), f.class, f.verdict))
        .collect()
}

fn has(r: &sqldiff_core::Report, rule: &str, class: ChangeClass, verdict: Verdict) -> bool {
    rules(r)
        .iter()
        .any(|(x, c, v)| x == rule && *c == class && *v == verdict)
}

#[test]
fn m13_isnull_truncation() {
    let r = run_mutation("m13");
    assert_eq!(r.verdict, Verdict::Different);
    assert!(
        has(
            &r,
            "types.isnull-truncation",
            ChangeClass::Semantic,
            Verdict::Different
        ),
        "{:?}",
        rules(&r)
    );
    let w = &r
        .findings
        .iter()
        .find(|f| f.rule == "types.isnull-truncation")
        .unwrap()
        .witness;
    assert!(
        w.contains("varchar(50)") && w.contains("varchar(254)"),
        "{w}"
    );
}

#[test]
fn m15_dropped_column() {
    let r = run_mutation("m15");
    assert_eq!(r.verdict, Verdict::Different);
    assert!(
        has(
            &r,
            "bind.unknown-column",
            ChangeClass::Binding,
            Verdict::Different
        ),
        "{:?}",
        rules(&r)
    );
    assert!(r
        .findings
        .iter()
        .any(|f| f.witness.contains("reserved_qty")));
}

#[test]
fn m15_without_catalog_is_unknown() {
    let dir = corpus().join("mutations/m15-dropped-column-reference");
    let (a, b) = (load(&dir, "original.pg.sql"), load(&dir, "mutated.pg.sql"));
    let r = diff(&a, &b, &Options::default());
    assert!(rules(&r).is_empty(), "{:?}", rules(&r));
    assert_ne!(r.verdict, Verdict::Different);
}

#[test]
fn m16_outdated_signature() {
    let r = run_mutation("m16");
    assert_eq!(r.verdict, Verdict::Different);
    assert!(
        has(
            &r,
            "bind.signature",
            ChangeClass::Binding,
            Verdict::Different
        ),
        "{:?}",
        rules(&r)
    );
    assert!(r
        .findings
        .iter()
        .any(|f| f.witness.contains("Missing: @Reason")));
}

#[test]
fn m17_shadowed_variable() {
    let r = run_mutation("m17");
    assert_eq!(r.verdict, Verdict::Different);
    assert!(
        has(
            &r,
            "bind.rebound-variable",
            ChangeClass::Semantic,
            Verdict::Different
        ),
        "{:?}",
        rules(&r)
    );
    assert!(
        has(
            &r,
            "bind.shadowed-variable",
            ChangeClass::Binding,
            Verdict::Unknown
        ),
        "{:?}",
        rules(&r)
    );
}

#[test]
fn m18_dialect_residue() {
    let r = run_mutation("m18");
    assert_eq!(r.verdict, Verdict::Different);
    assert!(
        has(
            &r,
            "bind.unknown-schema",
            ChangeClass::Binding,
            Verdict::Different
        ),
        "{:?}",
        rules(&r)
    );
    assert!(
        has(
            &r,
            "bind.unknown-builtin",
            ChangeClass::Binding,
            Verdict::Different
        ),
        "{:?}",
        rules(&r)
    );
}

fn sql_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sql"))
        .collect();
    v.sort();
    v
}

/// Every corpus artifact: (case directory name, file name, artifact).
fn corpus_files() -> Vec<(String, String, Artifact)> {
    let mut out = Vec::new();
    for kind in ["pairs", "mutations"] {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(corpus().join(kind))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            let case = dir.file_name().unwrap().to_string_lossy().to_string();
            for f in sql_files(&dir) {
                let name = f.file_name().unwrap().to_string_lossy().to_string();
                let a = load(&dir, &name);
                out.push((case.clone(), name, a));
            }
        }
    }
    out
}

/// With the corpus catalogs, only the seeded mutations report binding
/// problems, and only the trigger without a declared table needs an assumption.
#[test]
fn corpus_checks_are_clean() {
    let expected = [
        "m13-coalesce-to-isnull/mutated.tsql.sql",
        "m15-dropped-column-reference/mutated.pg.sql",
        "m16-outdated-signature/mutated.tsql.sql",
        "m17-shadowed-variable/mutated.db2i.sql",
        "m18-dialect-residue/mutated.pg.sql",
    ];
    let mut files = 0;
    for (case, name, a) in corpus_files() {
        files += 1;
        let id = format!("{case}/{name}");
        let o = Options {
            catalog_a: vec![catalog(a.dialect)],
            ..Options::default()
        };
        let r = sqldiff_core::check(&a, &o);
        let found = rules(&r);
        if expected.contains(&id.as_str()) {
            assert!(!found.is_empty(), "{id}: expected a binding finding");
        } else {
            assert!(found.is_empty(), "{id}: unexpected {found:?}");
        }
        let asm: Vec<&str> = r.assumptions.iter().map(|x| x.id.as_str()).collect();
        if id == "m20-trigger-to-out-of-engine-handler/original.pg.sql" {
            assert_eq!(asm, ["ASM-BIND-TRIGGER-ROW"], "{id}");
        } else {
            assert!(asm.is_empty(), "{id}: unexpected assumptions {asm:?}");
        }
    }
    assert!(files > 40, "corpus not found ({files} files)");
}

/// Cross-dialect pairs with catalogs: no binding finding on either side.
#[test]
fn corpus_pairs_bind_cleanly() {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(corpus().join("pairs"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let files = sql_files(&dir);
        for (i, x) in files.iter().enumerate() {
            for y in &files[i + 1..] {
                let a = load(&dir, &x.file_name().unwrap().to_string_lossy());
                let b = load(&dir, &y.file_name().unwrap().to_string_lossy());
                let r = diff(&a, &b, &opts(a.dialect, b.dialect));
                assert!(rules(&r).is_empty(), "{}: {:?}", dir.display(), rules(&r));
            }
        }
    }
}

/// The WP1 invariant: every reference the binder records either resolves,
/// fails with a reported issue, or rests on a recorded assumption, with and
/// without a catalog.
#[test]
fn every_reference_resolves_or_is_assumed() {
    let mut refs = 0;
    for (case, name, a) in corpus_files() {
        let analyzed = Analyzed::new(&a, Side::A, 3);
        let cat = ddl::load(&catalog(a.dialect).text, a.dialect, "corpus");
        for supplied in [true, false] {
            let sources: Vec<&Catalog> = if supplied { vec![&cat] } else { Vec::new() };
            let view = CatalogView::new(a.dialect, &sources);
            let bound = bind::bind(
                &analyzed,
                &BindInput {
                    view: &view,
                    helpers: &[],
                },
            );
            for r in &bound.refs {
                refs += 1;
                match &r.resolution {
                    Resolution::Resolved(_) => {}
                    Resolution::Failed(i) => assert!(
                        *i < bound.issues.len(),
                        "{case}/{name}: `{}` failed without an issue",
                        r.name
                    ),
                    Resolution::Assumed(id) => assert!(
                        bound.assumption(id).is_some(),
                        "{case}/{name}: `{}` rests on unrecorded assumption {id}",
                        r.name
                    ),
                }
                if !supplied {
                    // Without a catalog nothing persistent is proven missing.
                    if let Resolution::Failed(i) = r.resolution {
                        let rule = bound.issues[i].rule;
                        assert!(
                            !matches!(
                                rule,
                                "bind.unknown-table"
                                    | "bind.unknown-schema"
                                    | "bind.signature"
                                    | "bind.unknown-routine"
                            ),
                            "{case}/{name}: `{}` failed with {rule} without a catalog",
                            r.name
                        );
                    }
                }
            }
            for i in &bound.issues {
                for id in &i.assumptions {
                    assert!(
                        bound.assumption(id).is_some(),
                        "{case}/{name}: issue {} cites unrecorded {id}",
                        i.rule
                    );
                }
            }
        }
    }
    assert!(refs > 1000, "too few references recorded: {refs}");
}

/// `START WITH` inside an identity clause is not the query of `CREATE TABLE ... AS`.
#[test]
fn identity_options_are_not_a_query() {
    let pg = "CREATE TABLE t (\n  id integer GENERATED ALWAYS AS IDENTITY (START WITH 1 INCREMENT BY 1) PRIMARY KEY,\n  flag boolean NOT NULL DEFAULT FALSE\n);\n";
    let r = sqldiff_core::check(
        &Artifact::new("t.sql", Dialect::Postgres, pg),
        &Options::default(),
    );
    assert!(rules(&r).is_empty(), "{:?}", rules(&r));
    let as_query = "CREATE TABLE t2 AS SELECT nope FROM t;\n";
    let o = Options {
        catalog_a: vec![Artifact::new("c.sql", Dialect::Postgres, pg)],
        ..Options::default()
    };
    let r = sqldiff_core::check(&Artifact::new("t2.sql", Dialect::Postgres, as_query), &o);
    assert!(
        has(
            &r,
            "bind.unknown-column",
            ChangeClass::Binding,
            Verdict::Different
        ),
        "{:?}",
        rules(&r)
    );
}
