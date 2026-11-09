//! The shared routes of the control-flow graph against the oracle they
//! replaced: for every node of every lowered routine, every kind's route and
//! the order of the routes must equal what each statement used to build for
//! itself (`resolve_oracle`).

use super::*;
use crate::artifact::Artifact;
use crate::model::Side;
use std::path::{Path, PathBuf};

/// What a comparison saw.
#[derive(Default, Debug)]
struct Seen {
    cfgs: usize,
    nodes: usize,
    routes: usize,
    handled: usize,
    constructs: BTreeMap<&'static str, usize>,
    deepest_unwind: usize,
}

/// Lowers the artifact as a script and each of its routines, and asserts
/// for every node that the shared routes equal the oracle's, kind by
/// kind, in the same order, and that the nodes they lead to are those of
/// the old iteration.
fn compare(path: &str, d: Dialect, text: &str, seen: &mut Seen) {
    let text = text.replace("\r\n", "\n");
    let art = Artifact::new(path, d, text);
    let a = Analyzed::new(&art, Side::A, 0);
    let mut targets: Vec<Option<SyntaxNode>> = vec![None];
    targets.extend(
        a.root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::ROUTINE)
            .map(Some),
    );
    for t in targets {
        let cfg = lower_routine(&a, HashSet::new(), t);
        seen.cfgs += 1;
        for (id, node) in cfg.nodes.iter().enumerate() {
            seen.nodes += 1;
            let old = &node.oracle_routes;
            let new = node.all_routes();
            assert_eq!(old.len(), new.len(), "{path}: node {id}: route count");
            for ((ok, or), (nk, nr)) in old.iter().zip(&new) {
                assert_eq!(ok, nk, "{path}: node {id}: kind order");
                assert_eq!(or, nr, "{path}: node {id}: route of {ok:?}");
                assert_eq!(
                    node.route(*ok).as_ref(),
                    Some(or),
                    "{path}: node {id}: route({ok:?})"
                );
                seen.routes += 1;
                seen.deepest_unwind = seen.deepest_unwind.max(or.unwind.len());
                if let Some(h) = &or.handler {
                    seen.handled += 1;
                    *seen.constructs.entry(h.construct).or_insert(0) += 1;
                }
            }
            let mut want = Vec::new();
            for (_, r) in old {
                want.push(r.resume);
                if let Some(h) = &r.handler {
                    want.push(h.entry);
                }
            }
            let mut got = Vec::new();
            node.push_route_targets(&mut got);
            assert_eq!(want, got, "{path}: node {id}: route targets");
            if old.is_empty() {
                assert!(node.route(ErrorKind::Other).is_none());
            }
        }
    }
}

const TSQL_TRY: &str = "CREATE PROCEDURE dbo.p @n int
AS
BEGIN
    SET XACT_ABORT ON;
    BEGIN TRY
        UPDATE dbo.t SET v = 1 WHERE id = @n;
        BEGIN
            DELETE FROM dbo.t WHERE id = @n;
            BEGIN TRY
                INSERT INTO dbo.u (id) VALUES (@n);
            END TRY
            BEGIN CATCH
                THROW;
            END CATCH
        END
        IF @n > 1
            UPDATE dbo.t SET v = 2 WHERE id = @n;
    END TRY
    BEGIN CATCH
        UPDATE dbo.t SET v = 3 WHERE id = @n;
        RAISERROR('x', 16, 1);
    END CATCH
    UPDATE dbo.t SET v = 4 WHERE id = @n;
END;
";

const PG_EXCEPTION: &str = "CREATE OR REPLACE PROCEDURE p(p_n integer)
LANGUAGE plpgsql
AS $$
BEGIN
  UPDATE t SET v = 1 WHERE id = p_n;
  BEGIN
    INSERT INTO u (id) VALUES (p_n);
    BEGIN
      DELETE FROM u WHERE id = p_n;
    EXCEPTION
      WHEN division_by_zero THEN
        UPDATE t SET v = 2 WHERE id = p_n;
    END;
  EXCEPTION
    WHEN unique_violation THEN
      UPDATE t SET v = 3 WHERE id = p_n;
    WHEN OTHERS THEN
      RAISE;
  END;
  UPDATE t SET v = 4 WHERE id = p_n;
END;
$$;
";

const DB2I_HANDLERS: &str = "CREATE PROCEDURE p (IN p_n INTEGER)
LANGUAGE SQL
MODIFIES SQL DATA
SET OPTION COMMIT = *CHG
BEGIN
  DECLARE CONTINUE HANDLER FOR SQLSTATE '23505'
  BEGIN
    UPDATE t SET v = 9 WHERE id = p_n;
  END;
  DECLARE EXIT HANDLER FOR SQLEXCEPTION
  BEGIN
    ROLLBACK;
    RESIGNAL;
  END;
  UPDATE t SET v = 1 WHERE id = p_n;
  BEGIN
    DECLARE UNDO HANDLER FOR NOT FOUND
    BEGIN
      UPDATE t SET v = 8 WHERE id = p_n;
    END;
    DELETE FROM u WHERE id = p_n;
  END;
  COMMIT;
END;
";

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

fn sql_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            sql_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "sql") {
            out.push(p);
        }
    }
}

/// The handler arms the inline sources must reach, so the comparison
/// cannot pass without them.
fn assert_handler_arms(seen: &Seen) {
    for c in ["try_catch", "exception_block", "exit_handler"] {
        assert!(
            seen.constructs.get(c).copied().unwrap_or(0) > 0,
            "no route reached a `{c}` handler: {seen:?}"
        );
    }
    assert!(seen.deepest_unwind >= 3, "{seen:?}");
}

#[test]
fn shared_routes_equal_the_per_statement_oracle_on_handlers() {
    let mut seen = Seen::default();
    compare("tsql.sql", Dialect::Tsql, TSQL_TRY, &mut seen);
    compare("pg.sql", Dialect::Postgres, PG_EXCEPTION, &mut seen);
    compare("db2i.sql", Dialect::Db2i, DB2I_HANDLERS, &mut seen);
    assert_handler_arms(&seen);
    eprintln!("{seen:?}");
}

#[test]
fn shared_routes_equal_the_per_statement_oracle_on_the_corpus() {
    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus");
    let mut files = Vec::new();
    for kind in ["pairs", "mutations"] {
        sql_files(&corpus.join(kind), &mut files);
    }
    assert!(files.len() > 50, "corpus not found at {corpus:?}");
    let mut seen = Seen::default();
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        if name == "setup.sql" {
            continue;
        }
        let text = std::fs::read_to_string(f).unwrap();
        compare(&f.to_string_lossy(), dialect_of(&name), &text, &mut seen);
    }
    assert!(seen.nodes > 500, "{seen:?}");
    assert!(seen.constructs.contains_key("try_catch"), "{seen:?}");
    assert!(seen.constructs.contains_key("exit_handler"), "{seen:?}");
    eprintln!("{seen:?}");
}

/// Local run over larger corpora: `SQLDIFF_ROUTE_ORACLE_DIRS` is a
/// `;`-separated list of `dialect=path` (`tsql`, `pg` or `db2i`), each a
/// directory walked for `.sql` files or one file. Prints what it compared.
#[test]
#[ignore = "reads corpora named by SQLDIFF_ROUTE_ORACLE_DIRS"]
fn shared_routes_equal_the_per_statement_oracle_on_named_corpora() {
    let spec = std::env::var("SQLDIFF_ROUTE_ORACLE_DIRS").expect("SQLDIFF_ROUTE_ORACLE_DIRS");
    for part in spec.split(';').filter(|p| !p.is_empty()) {
        let (d, dir) = part.split_once('=').expect("dialect=path");
        let d = match d {
            "tsql" => Dialect::Tsql,
            "pg" => Dialect::Postgres,
            _ => Dialect::Db2i,
        };
        let mut files = Vec::new();
        let root = Path::new(dir);
        if root.is_dir() {
            sql_files(root, &mut files);
        } else {
            files.push(root.to_path_buf());
        }
        let mut seen = Seen::default();
        for f in &files {
            let Ok(text) = std::fs::read_to_string(f) else {
                continue;
            };
            compare(&f.to_string_lossy(), d, &text, &mut seen);
        }
        eprintln!("ROUTE-ORACLE {part}: files={} {seen:?}", files.len());
    }
}
