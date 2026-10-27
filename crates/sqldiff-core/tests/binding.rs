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

fn check_rules(d: Dialect, text: &str, catalog: Option<&str>) -> sqldiff_core::Report {
    let o = Options {
        catalog_a: catalog
            .map(|c| vec![Artifact::new("c.sql", d, c)])
            .unwrap_or_default(),
        ..Options::default()
    };
    sqldiff_core::check(&Artifact::new("x.sql", d, text), &o)
}

/// T-SQL variables live for the whole batch, loops and CATCH included.
#[test]
fn tsql_loop_is_not_a_scope() {
    let src = "CREATE PROCEDURE p AS\nBEGIN\n  WHILE 1 = 1\n  BEGIN\n    DECLARE @x int = 1;\n    BREAK;\n  END\n  SELECT @x;\n  BEGIN TRY SELECT 1; END TRY\n  BEGIN CATCH DECLARE @e int = 1; END CATCH\n  SELECT @e;\nEND";
    let r = check_rules(Dialect::Tsql, src, None);
    assert!(rules(&r).is_empty(), "{:?}", rules(&r));
    let dup = "CREATE PROCEDURE p AS\nBEGIN\n  DECLARE @x int;\n  WHILE 1 = 1\n  BEGIN\n    DECLARE @x int;\n    BREAK;\n  END\nEND";
    let r = check_rules(Dialect::Tsql, dup, None);
    assert!(
        has(
            &r,
            "bind.duplicate-declaration",
            ChangeClass::Binding,
            Verdict::Different
        ),
        "{:?}",
        rules(&r)
    );
}

/// Object-naming statements (`LABEL ON`, `RENAME`) and statements the binder
/// does not classify hold no references, so they give no binding findings (#32).
#[test]
fn object_naming_statements_bind_nothing() {
    let src = "CREATE TABLE shipment (id INTEGER NOT NULL, name VARCHAR(50), insert_user VARCHAR(30));\nLABEL ON TABLE shipment IS 'Shipment';\nLABEL ON COLUMN shipment.name IS 'Name';\nLABEL ON COLUMN shipment (id TEXT IS 'Shipment id', name TEXT IS 'Shipment name', insert_user TEXT IS 'Created by');\nRENAME TABLE shipment TO shipment_old;\nRENAME INDEX shipment_ix TO shipment_old_ix;\nCOMMENT ON TABLE shipment_old IS 'x';\nGRANT SELECT ON shipment_old TO PUBLIC;\nCREATE ALIAS shipment_a FOR shipment_old;\n";
    let r = check_rules(Dialect::Db2i, src, None);
    assert!(rules(&r).is_empty(), "{:?}", rules(&r));
    let pg = "COPY t (a, b) FROM STDIN;\nREFRESH MATERIALIZED VIEW mv;\n";
    let r = check_rules(Dialect::Postgres, pg, None);
    assert!(rules(&r).is_empty(), "{:?}", rules(&r));
    // Statements that carry expressions are still checked.
    let undeclared = "CREATE PROCEDURE p ()\nLANGUAGE SQL\nBEGIN\n  DECLARE n INTEGER DEFAULT 0;\n  SET n = missing_var + 1;\nEND";
    let r = check_rules(Dialect::Db2i, undeclared, None);
    assert!(
        has(
            &r,
            "bind.unknown-variable",
            ChangeClass::Binding,
            Verdict::Different
        ),
        "{:?}",
        rules(&r)
    );
}

/// Ordinals count within the routine: an unrelated routine added in front
/// does not make unchanged references look rebound.
#[test]
fn rebound_ignores_other_routines() {
    let g = "CREATE FUNCTION g() RETURNS int LANGUAGE plpgsql AS $$\nDECLARE n int := 0;\nBEGIN\n  DECLARE n int := 1;\n  BEGIN\n    n := 2;\n  END;\n  RETURN n;\nEND;\n$$;\n";
    let h = "CREATE FUNCTION h() RETURNS int LANGUAGE plpgsql AS $$\nDECLARE n int := 5;\nBEGIN\n  RETURN n;\nEND;\n$$;\n";
    let a = Artifact::new("a.sql", Dialect::Postgres, g);
    let b = Artifact::new("b.sql", Dialect::Postgres, format!("{h}{g}"));
    let r = diff(&a, &b, &Options::default());
    assert!(
        !r.findings.iter().any(|f| f.rule == "bind.rebound-variable"),
        "{:?}",
        rules(&r)
    );
}

/// Catalog files fold in order: a later ALTER TABLE applies to an earlier CREATE.
#[test]
fn catalog_files_fold_in_order() {
    let d = Dialect::Postgres;
    let o = Options {
        catalog_a: vec![
            Artifact::new("001.sql", d, "CREATE TABLE t (a int);"),
            Artifact::new("002.sql", d, "ALTER TABLE t ADD COLUMN b int;"),
        ],
        ..Options::default()
    };
    let r = sqldiff_core::check(&Artifact::new("x.sql", d, "SELECT a, b FROM t;"), &o);
    assert!(rules(&r).is_empty(), "{:?}", rules(&r));
    let r = sqldiff_core::check(&Artifact::new("x.sql", d, "SELECT c FROM t;"), &o);
    assert!(has(
        &r,
        "bind.unknown-column",
        ChangeClass::Binding,
        Verdict::Different
    ));
}

/// Built-ins missing from the list are not proven errors; verified-absent ones are.
#[test]
fn builtin_list_gaps_are_assumptions() {
    let r = check_rules(
        Dialect::Tsql,
        "SELECT BIT_COUNT(5), GET_BIT(5, 0), LEFT_SHIFT(1, 2);",
        None,
    );
    assert!(rules(&r).is_empty(), "{:?}", rules(&r));
    let r = check_rules(Dialect::Postgres, "SELECT regr_count(y, x) FROM t;", None);
    assert!(rules(&r).is_empty(), "{:?}", rules(&r));
    // An other-dialect name that is not verified absent: Unknown, with its assumption.
    let r = check_rules(Dialect::Db2i, "VALUES ISNULL(1, 2);", None);
    assert!(
        has(
            &r,
            "bind.unknown-builtin",
            ChangeClass::Binding,
            Verdict::Unknown
        ),
        "{:?}",
        rules(&r)
    );
    assert!(r
        .assumptions
        .iter()
        .any(|a| a.id == "ASM-BUILTIN-ABSENT:isnull"));
    let r = check_rules(Dialect::Postgres, "SELECT GETDATE();", None);
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

/// PL/pgSQL FOREACH assigns to a declared variable; it does not declare one.
#[test]
fn foreach_uses_declared_target() {
    let ok = "CREATE FUNCTION f(a int[]) RETURNS int LANGUAGE plpgsql AS $$\nDECLARE x int; s int := 0;\nBEGIN\n  FOREACH x IN ARRAY a LOOP\n    s := s + x;\n  END LOOP;\n  RETURN s;\nEND;\n$$;";
    let r = check_rules(Dialect::Postgres, ok, None);
    assert!(rules(&r).is_empty(), "{:?}", rules(&r));
    let missing = ok.replace("DECLARE x int; s int := 0;", "DECLARE s int := 0;");
    let r = check_rules(Dialect::Postgres, &missing, None);
    assert!(
        r.findings
            .iter()
            .any(|f| f.rule.starts_with("bind.") && f.witness.contains("`x`")),
        "{:?}",
        rules(&r)
    );
}

/// Every assumption a finding cites is in the report, with or without a catalog.
#[test]
fn cited_assumptions_are_reported() {
    let with_temp = "CREATE FUNCTION f(id int) RETURNS int LANGUAGE plpgsql AS $$\nBEGIN\n  CREATE TEMP TABLE t (id int);\n  RETURN (SELECT id FROM t WHERE t.id = id);\nEND;\n$$;";
    let plain = "CREATE FUNCTION f(id int) RETURNS int LANGUAGE plpgsql AS $$\nBEGIN\n  RETURN (SELECT id FROM t WHERE t.id = id);\nEND;\n$$;";
    for (src, cat) in [(with_temp, None), (plain, Some("CREATE TABLE t (id int);"))] {
        let r = check_rules(Dialect::Postgres, src, cat);
        assert!(
            r.findings.iter().any(|f| !f.assumptions.is_empty()),
            "{:?}",
            rules(&r)
        );
        for f in &r.findings {
            for id in &f.assumptions {
                assert!(
                    r.assumptions.iter().any(|a| &a.id == id),
                    "{id} not in report ({cat:?})"
                );
            }
        }
    }
}

/// The GUI re-diffs on every keystroke: binding and catalog loading must not
/// panic on any prefix of real code (unclosed parentheses, half statements).
#[test]
fn incomplete_input_never_panics() {
    let mut texts: Vec<(String, Dialect, String)> = corpus_files()
        .into_iter()
        .map(|(case, name, a)| (format!("{case}/{name}"), a.dialect, a.text))
        .collect();
    for d in [Dialect::Tsql, Dialect::Postgres, Dialect::Db2i] {
        texts.push(("catalog".into(), d, catalog(d).text));
    }
    for (snippet, d) in [
        ("SELECT f(", Dialect::Tsql),
        ("CREATE TABLE t (", Dialect::Postgres),
        ("CREATE VIEW v (", Dialect::Postgres),
        ("DECLARE @t TABLE (", Dialect::Tsql),
        ("CALL p(", Dialect::Db2i),
        (
            "CREATE FUNCTION f() RETURNS int LANGUAGE plpgsql AS $$ DECLARE x ALIAS; BEGIN END $$;",
            Dialect::Postgres,
        ),
    ] {
        texts.push((snippet.into(), d, snippet.into()));
    }
    let mut failures = Vec::new();
    let prev = std::panic::take_hook();
    static WHERE: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
    std::panic::set_hook(Box::new(|info| {
        if let Ok(mut w) = WHERE.lock() {
            *w = info.location().map(|l| l.to_string()).unwrap_or_default();
        }
    }));
    for (id, d, text) in &texts {
        let cat = ddl::load(&catalog(*d).text, *d, "corpus");
        let cuts: Vec<usize> = text
            .char_indices()
            .filter(|(_, c)| !c.is_alphanumeric() && *c != '_')
            .map(|(i, _)| i)
            .chain([text.len()])
            .collect();
        for cut in cuts {
            let prefix = &text[..cut];
            let r = std::panic::catch_unwind(|| {
                let a = Artifact::new("p.sql", *d, prefix);
                let analyzed = Analyzed::new(&a, Side::A, 3);
                let view = CatalogView::new(*d, &[&cat]);
                bind::bind(
                    &analyzed,
                    &BindInput {
                        view: &view,
                        helpers: &[],
                    },
                );
                ddl::load(prefix, *d, "p");
            });
            if r.is_err() {
                let at = WHERE.lock().map(|w| w.clone()).unwrap_or_default();
                let tail: String = prefix
                    .chars()
                    .rev()
                    .take(40)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                failures.push(format!("{id} @{cut} ({at}): …{tail}"));
                break;
            }
        }
    }
    std::panic::set_hook(prev);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// ALTER TABLE on a temp table the routine created adds its columns.
#[test]
fn alter_on_local_table() {
    let src = "CREATE PROCEDURE p AS\nBEGIN\n  CREATE TABLE #t (a int);\n  ALTER TABLE #t ADD b int;\n  SELECT a, b FROM #t;\nEND";
    let r = check_rules(Dialect::Tsql, src, Some("CREATE TABLE dbo.x (id int);"));
    assert!(rules(&r).is_empty(), "{:?}", rules(&r));
    let pg = "CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$\nBEGIN\n  CREATE TEMP TABLE t (a int);\n  ALTER TABLE t ADD COLUMN b int;\n  PERFORM b FROM t;\nEND;\n$$;";
    let r = check_rules(Dialect::Postgres, pg, Some("CREATE TABLE x (id int);"));
    assert!(rules(&r).is_empty(), "{:?}", rules(&r));
}

/// ISNULL lengths: literal characters ('' is one quote), and a T-SQL
/// `varchar` without a length holds one character.
#[test]
fn isnull_lengths() {
    let cat = "CREATE TABLE dbo.c (id int NOT NULL, name varchar(7) NULL, code varchar(3) NULL);";
    let quote = "CREATE PROCEDURE p AS\nBEGIN\n  SELECT ISNULL(name, 'O''Brien') FROM dbo.c;\nEND";
    let r = check_rules(Dialect::Tsql, quote, Some(cat));
    assert!(rules(&r).is_empty(), "{:?}", rules(&r));
    let long = "CREATE PROCEDURE p AS\nBEGIN\n  SELECT ISNULL(name, 'O''Briens') FROM dbo.c;\nEND";
    let r = check_rules(Dialect::Tsql, long, Some(cat));
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
    let bare = "CREATE PROCEDURE p AS\nBEGIN\n  DECLARE @v varchar;\n  SELECT ISNULL(code, @v) FROM dbo.c;\nEND";
    let r = check_rules(Dialect::Tsql, bare, Some(cat));
    assert!(rules(&r).is_empty(), "{:?}", rules(&r));
}

/// A view keeps NOT NULL only through a plain inner query.
#[test]
fn view_nullability() {
    let d = Dialect::Postgres;
    let c = ddl::load(
        "CREATE TABLE a (id int NOT NULL, x int NOT NULL);\nCREATE TABLE b (id int NOT NULL, y int NOT NULL);\nCREATE VIEW v1 AS SELECT a.x FROM a JOIN b ON a.id = b.id;\nCREATE VIEW v2 AS SELECT b.y FROM a LEFT JOIN b ON a.id = b.id;\nCREATE VIEW v3 AS SELECT x FROM a UNION SELECT NULL FROM b;",
        d,
        "c",
    );
    let view = CatalogView::new(d, &[&c]);
    let nullable = |v: &str| match view.table(&[sqldiff_core::catalog::Ident::plain(v)]) {
        sqldiff_core::catalog::Lookup::Found(t) => t.columns[0].nullable,
        _ => panic!("{v}"),
    };
    assert!(!nullable("v1"));
    assert!(nullable("v2"));
    assert!(nullable("v3"));
}

/// Db2 for i: a table function in FROM is written `TABLE(f(…)) AS x`. A bare
/// call (the T-SQL and PostgreSQL form) does not bind as one.
#[test]
fn db2_table_function_needs_table() {
    let bare = "SELECT x.a FROM f(1) x;\nSELECT * FROM t, lib.g(2) AS y;\n";
    let r = check_rules(Dialect::Db2i, bare, None);
    let hits = r
        .findings
        .iter()
        .filter(|f| f.rule == "bind.table-function-form" && f.class == ChangeClass::Binding)
        .count();
    assert_eq!(hits, 2, "{:?}", rules(&r));
    let ok = "SELECT x.a FROM TABLE(f(1)) AS x;\nSELECT * FROM t JOIN TABLE(lib.g(2)) y ON 1 = 1;\nSELECT u.a FROM UNNEST(ARRAY[1, 2]) AS u (a);\n";
    let r = check_rules(Dialect::Db2i, ok, None);
    assert!(
        !r.findings
            .iter()
            .any(|f| f.rule == "bind.table-function-form"),
        "{:?}",
        rules(&r)
    );
    let r = check_rules(Dialect::Postgres, "SELECT x.a FROM f(1) x;", None);
    assert!(
        !r.findings
            .iter()
            .any(|f| f.rule == "bind.table-function-form"),
        "{:?}",
        rules(&r)
    );
}

/// Db2 for i: a subselect needs FROM, and SYSIBM.SYSDUMMY1 always exists,
/// also against a catalog that lists every table.
#[test]
fn db2_select_needs_from_and_sysdummy1_binds() {
    let src = "SELECT 1 AS one;\nSELECT a FROM t WHERE a IN (SELECT 2);\nGRANT SELECT, INSERT ON t TO u;\nSELECT TRIM(' ' FROM a) FROM t;\nSELECT 1 FROM SYSIBM.SYSDUMMY1;\n";
    let r = check_rules(Dialect::Db2i, src, Some("CREATE TABLE t (a INTEGER);"));
    let hits: Vec<_> = r
        .findings
        .iter()
        .filter(|f| f.rule == "residue.db2i.select-without-from")
        .collect();
    assert_eq!(hits.len(), 2, "{:?}", rules(&r));
    assert!(
        !r.findings.iter().any(|f| f.rule == "bind.unknown-table"),
        "{:?}",
        rules(&r)
    );
    let r = check_rules(Dialect::Postgres, "SELECT 1;", None);
    assert!(rules(&r).is_empty(), "{:?}", rules(&r));
}

/// T-SQL `inserted`/`deleted` as FROM tables of a trigger, and the transition
/// tables a PostgreSQL trigger function gets from its CREATE TRIGGER, bind to
/// the trigger's table; outside a trigger they do not.
#[test]
fn trigger_images_bind_to_the_trigger_table() {
    let tables = |d| {
        Artifact::new(
            "t.sql",
            d,
            "CREATE TABLE orders (order_id int NOT NULL PRIMARY KEY, qty int NOT NULL);",
        )
    };
    let check = |d, sql: &str| {
        let o = Options {
            catalog_a: vec![tables(d)],
            ..Options::default()
        };
        rules(&sqldiff_core::check(&Artifact::new("x.sql", d, sql), &o))
    };
    let tsql = "CREATE TRIGGER tu ON orders AFTER UPDATE AS
UPDATE o SET qty = i.qty FROM orders o JOIN inserted i ON i.order_id = o.order_id LEFT JOIN deleted AS d ON d.order_id = i.order_id;";
    assert!(
        check(Dialect::Tsql, tsql).is_empty(),
        "{:?}",
        check(Dialect::Tsql, tsql)
    );
    let pg = "CREATE FUNCTION tu() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  UPDATE orders o SET qty = i.qty FROM inserted i WHERE i.order_id = o.order_id AND EXISTS (SELECT 1 FROM deleted d WHERE d.order_id = i.order_id);
  RETURN NULL;
END;
$$;
CREATE TRIGGER tu AFTER UPDATE ON orders REFERENCING NEW TABLE AS inserted OLD TABLE AS deleted FOR EACH STATEMENT EXECUTE FUNCTION tu();";
    assert!(
        check(Dialect::Postgres, pg).is_empty(),
        "{:?}",
        check(Dialect::Postgres, pg)
    );
    let outside = "CREATE PROCEDURE p AS SELECT order_id FROM inserted;";
    assert!(!check(Dialect::Tsql, outside).is_empty());
}
