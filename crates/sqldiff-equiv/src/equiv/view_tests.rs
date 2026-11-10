//! `table_pair` reads each routine unit as a script's without copying the
//! unit (`Canon::as_script`). Two guards keep that exact:
//!
//! - `canon.rs` reads the unit's kind and `sql_body` only through `Canon`'s
//!   own fields, so a later reader cannot silently see the real kind where
//!   `table_pair` means the script reading.
//! - For every `table_pair` call over the corpus fixtures and the inline
//!   sources below, the statement comparison on the view gives the same
//!   result as on the unit clone `table_pair` used to build (kind set to
//!   `Script`, `sql_body` cleared): the same assumptions, class, rule and
//!   witness text. The clone is rebuilt here, in `agrees_with_clone`, which
//!   `table_pair` calls in test builds only.
//!
//! The test reads the corpus and `canon.rs`, as `cfg/route_tests.rs` does in
//! `sqldiff-core` (`check_purity` lists it).

use super::lower::{Unit, UnitKind};
use super::schema::Schema;
use super::{ast, same_stmt_in, Same};
use crate::artifact::Artifact;
use crate::model::Side;
use sqldiff_syntax::Dialect;
use std::cell::Cell;
use std::path::{Path, PathBuf};

thread_local! {
    /// Calls checked on this thread: (all, with a unit that is not a script).
    static CHECKED: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}

fn show(s: &Same) -> String {
    match s {
        Same::Yes(a) => format!("Yes {a:?}"),
        Same::No(class, rule, w) => format!("No {class:?} {rule} {w}"),
        Same::Unknown(w) => format!("Unknown {w}"),
    }
}

/// The unit as `table_pair` used to copy it: a clone read as a script's.
fn as_script_clone(u: &Unit) -> Unit {
    let mut c = u.clone();
    if c.kind != UnitKind::Script {
        c.kind = UnitKind::Script;
        c.sql_body = false;
    }
    c
}

/// Called by `table_pair` (test builds) after each comparison on the view:
/// the same comparison on the old clone must give the same result.
#[allow(clippy::too_many_arguments)]
pub(super) fn agrees_with_clone(
    x: &[ast::Tok],
    y: &[ast::Tok],
    ua: &Unit,
    ub: &Unit,
    sa: &Schema,
    sb: &Schema,
    na: &str,
    nb: &str,
    stored: bool,
    got: &Same,
) {
    let (ca, cb) = (as_script_clone(ua), as_script_clone(ub));
    let want = same_stmt_in(x, y, &ca, &cb, sa, sb, na, nb, stored, false);
    assert_eq!(show(&want), show(got), "{na} / {nb}: stored = {stored}");
    CHECKED.with(|c| {
        let (all, routine) = c.get();
        let routine =
            routine + usize::from(ua.kind != UnitKind::Script || ub.kind != UnitKind::Script);
        c.set((all + 1, routine));
    });
}

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus")
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p)
        .unwrap_or_else(|e| panic!("{p:?}: {e}"))
        .replace("\r\n", "\n")
}

fn dialect(key: &str) -> Dialect {
    match key {
        "tsql" => Dialect::Tsql,
        "pg" => Dialect::Postgres,
        "db2i" => Dialect::Db2i,
        k => panic!("dialect {k}"),
    }
}

/// Runs the whole comparison of two sources; `table_pair` checks itself.
fn run(path_a: &str, da: Dialect, ta: &str, path_b: &str, db: Dialect, tb: &str) {
    let (xa, xb) = (
        Artifact::new(path_a, da, ta.to_string()),
        Artifact::new(path_b, db, tb.to_string()),
    );
    let (sa, sb) = (
        Schema::load(std::slice::from_ref(&xa)),
        Schema::load(std::slice::from_ref(&xb)),
    );
    let (aa, ab) = (
        super::Analyzed::new(&xa, Side::A, 0),
        super::Analyzed::new(&xb, Side::B, 0),
    );
    let _ = super::compare(&aa, &ab, &sa, &sb, &[]);
}

/// Every pair of sides of every corpus case.
fn corpus_cases() -> Vec<(String, Dialect, String, String, Dialect, String)> {
    let mut out = Vec::new();
    for kind in ["pairs", "mutations"] {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(corpus().join(kind))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.join("case.toml").is_file())
            .collect();
        dirs.sort();
        for dir in dirs {
            let case: toml::Table = read(&dir.join("case.toml")).parse().unwrap();
            let sides = case["sides"].as_table().unwrap();
            let mut loaded = Vec::new();
            for (_, s) in sides {
                let file = s["file"].as_str().unwrap();
                let d = dialect(s["dialect"].as_str().unwrap());
                loaded.push((file.to_string(), d, read(&dir.join(file))));
            }
            for i in 0..loaded.len() {
                for j in 0..loaded.len() {
                    if i != j {
                        let (a, b) = (&loaded[i], &loaded[j]);
                        out.push((a.0.clone(), a.1, a.2.clone(), b.0.clone(), b.1, b.2.clone()));
                    }
                }
            }
        }
    }
    out
}

/// Column lists of a table a routine creates: defaults, CHECK constraints
/// and a computed column, in the spelling of each dialect: the first three
/// of each list agree, the rest of the PostgreSQL list differ or respell.
const TSQL_COLS: &[&str] = &[
    "id int NOT NULL, qty int NOT NULL DEFAULT 0, note nvarchar(20) NULL DEFAULT N'x', total AS (id * qty), CHECK (qty >= 0), CONSTRAINT ck_id CHECK (id > 0 AND id < 1000)",
    "id int NOT NULL, qty int NOT NULL DEFAULT 5, CHECK (qty < 10)",
    "id int NOT NULL, stamp datetime NOT NULL DEFAULT GETDATE(), CHECK (id <> 7)",
];
const PG_COLS: &[&str] = &[
    "id integer NOT NULL, qty integer NOT NULL DEFAULT 0, note varchar(20) NULL DEFAULT 'x', total integer GENERATED ALWAYS AS (id * qty) STORED, CHECK (qty >= 0), CONSTRAINT ck_id CHECK (id > 0 AND id < 1000)",
    "id integer NOT NULL, qty integer NOT NULL DEFAULT 5, CHECK (qty < 10)",
    "id integer NOT NULL, stamp timestamp NOT NULL DEFAULT now(), CHECK (id <> 7)",
    "id integer NOT NULL, qty integer NOT NULL DEFAULT 6, CHECK (qty <= 10)",
    "id integer NOT NULL, stamp timestamp NOT NULL DEFAULT clock_timestamp(), CHECK (id <> 8)",
    "id integer NOT NULL, qty integer NOT NULL DEFAULT 5 + 0, CHECK (NOT (qty >= 10))",
];
const DB2_COLS: &[&str] = &[
    "id INTEGER NOT NULL, qty INTEGER NOT NULL DEFAULT 0, note VARCHAR(20) DEFAULT 'x', CHECK (qty >= 0)",
    "id INTEGER NOT NULL, qty INTEGER NOT NULL DEFAULT 5, CHECK (qty < 10)",
    "id INTEGER NOT NULL, stamp TIMESTAMP NOT NULL DEFAULT CURRENT TIMESTAMP, CHECK (id <> 7)",
];

fn tsql_var(cols: &str) -> String {
    format!("CREATE PROCEDURE dbo.p AS\nBEGIN\n  SET XACT_ABORT ON;\n  DECLARE @t TABLE ({cols});\n  INSERT INTO @t (id) VALUES (1);\n  DELETE FROM @t WHERE id = 2;\nEND\n")
}

fn tsql_temp(cols: &str) -> String {
    format!("CREATE PROCEDURE dbo.p AS\nBEGIN\n  SET XACT_ABORT ON;\n  CREATE TABLE #t ({cols});\n  INSERT INTO #t (id) VALUES (1);\n  DELETE FROM #t WHERE id = 2;\n  DROP TABLE #t;\nEND\n")
}

fn pg_proc(cols: &str) -> String {
    format!("CREATE OR REPLACE PROCEDURE p()\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  DROP TABLE IF EXISTS pg_temp.tv__dbo__p__t;\n  CREATE TEMP TABLE pg_temp.tv__dbo__p__t ({cols});\n  INSERT INTO pg_temp.tv__dbo__p__t (id) VALUES (1);\n  DELETE FROM pg_temp.tv__dbo__p__t WHERE id = 2;\nEND;\n$$;\n")
}

fn pg_temp(cols: &str) -> String {
    format!("CREATE OR REPLACE PROCEDURE p()\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  CREATE TEMP TABLE t ({cols});\n  INSERT INTO t (id) VALUES (1);\n  DELETE FROM t WHERE id = 2;\n  DROP TABLE t;\nEND;\n$$;\n")
}

fn pg_fn(cols: &str) -> String {
    format!("CREATE OR REPLACE FUNCTION p() RETURNS TABLE (id integer)\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  DROP TABLE IF EXISTS pg_temp.tv__dbo__p__t;\n  CREATE TEMP TABLE pg_temp.tv__dbo__p__t ({cols});\n  INSERT INTO pg_temp.tv__dbo__p__t (id) VALUES (1);\n  RETURN QUERY SELECT CAST(x.id AS integer) FROM pg_temp.tv__dbo__p__t x;\nEND;\n$$;\n")
}

fn tsql_rs(cols: &str) -> String {
    format!("CREATE PROCEDURE dbo.p AS\nBEGIN\n  SET XACT_ABORT ON;\n  DECLARE @t TABLE ({cols});\n  INSERT INTO @t (id) VALUES (1);\n  SELECT id FROM @t;\nEND\n")
}

fn pg_sqlfn(cols: &str) -> String {
    format!("CREATE OR REPLACE FUNCTION p() RETURNS integer\nLANGUAGE plpgsql\nAS $$\nBEGIN\n  DROP TABLE IF EXISTS pg_temp.tv__dbo__p__t;\n  CREATE TEMP TABLE pg_temp.tv__dbo__p__t ({cols});\n  INSERT INTO pg_temp.tv__dbo__p__t (id) VALUES (1);\n  RETURN 0;\nEND;\n$$;\n")
}

fn db2_proc(cols: &str) -> String {
    format!("CREATE OR REPLACE PROCEDURE p ()\nLANGUAGE SQL\nBEGIN\n  DECLARE GLOBAL TEMPORARY TABLE SESSION.tv__dbo__p__t ({cols}) ON COMMIT PRESERVE ROWS NOT LOGGED ON ROLLBACK PRESERVE ROWS WITH REPLACE;\n  INSERT INTO SESSION.tv__dbo__p__t (id) VALUES (1);\n  DELETE FROM SESSION.tv__dbo__p__t WHERE id = 2;\nEND;\n")
}

/// Every source of one dialect against every source of another, both ways.
fn inline_cases() -> Vec<(String, Dialect, String, String, Dialect, String)> {
    let (t, p, d) = (Dialect::Tsql, Dialect::Postgres, Dialect::Db2i);
    let mut out = Vec::new();
    for (i, tc) in TSQL_COLS.iter().enumerate() {
        for (j, pc) in PG_COLS.iter().enumerate() {
            let name = format!("inline_{i}_{j}");
            for (a, b) in [
                (tsql_var(tc), pg_proc(pc)),
                (tsql_temp(tc), pg_temp(pc)),
                (tsql_rs(tc), pg_fn(pc)),
                (tsql_var(tc), pg_sqlfn(pc)),
                (tsql_var(tc), tsql_var(TSQL_COLS[j % TSQL_COLS.len()])),
            ] {
                let pb = if b.contains("LANGUAGE plpgsql") { p } else { t };
                out.push((
                    format!("{name}.a.sql"),
                    t,
                    a.clone(),
                    format!("{name}.b.sql"),
                    pb,
                    b.clone(),
                ));
                out.push((
                    format!("{name}.b.sql"),
                    pb,
                    b,
                    format!("{name}.a.sql"),
                    t,
                    a,
                ));
            }
        }
        for (j, dc) in DB2_COLS.iter().enumerate() {
            let name = format!("inline_db2_{i}_{j}");
            let (a, b) = (tsql_var(tc), db2_proc(dc));
            out.push((
                format!("{name}.a.sql"),
                t,
                a.clone(),
                format!("{name}.b.sql"),
                d,
                b.clone(),
            ));
            out.push((format!("{name}.b.sql"), d, b, format!("{name}.a.sql"), t, a));
        }
    }
    out
}

#[test]
fn table_pair_on_the_view_equals_table_pair_on_the_clone() {
    CHECKED.with(|c| c.set((0, 0)));
    let cases = corpus_cases();
    assert!(cases.len() > 40, "corpus not found at {:?}", corpus());
    for (pa, da, ta, pb, db, tb) in &cases {
        run(pa, *da, ta, pb, *db, tb);
    }
    let (corpus_all, corpus_routine) = CHECKED.with(Cell::get);
    for (pa, da, ta, pb, db, tb) in &inline_cases() {
        run(pa, *da, ta, pb, *db, tb);
    }
    let (all, routine) = CHECKED.with(Cell::get);
    eprintln!("table_pair comparisons: corpus {corpus_all} ({corpus_routine} in a routine), total {all} ({routine} in a routine)");
    assert!(
        routine > 0 && all > corpus_all,
        "no table of a routine was compared: the oracle saw nothing"
    );
}

/// `canon.rs` reads the unit's kind and `sql_body` through `Canon::kind` and
/// `Canon::sql_body` only (comment lines are not code).
#[test]
fn canon_reads_kind_only_through_its_own_field() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/equiv/canon.rs");
    let text = read(&path);
    let mut bad = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let s = line.trim();
        if s.starts_with("//") {
            continue;
        }
        // `Canon::new` copies them once.
        if s == "kind: unit.kind," || s == "sql_body: unit.sql_body," {
            continue;
        }
        for pat in ["unit.kind", "unit.sql_body", "u.kind", "u.sql_body"] {
            if s.contains(pat) {
                bad.push(format!("canon.rs:{}: {pat}: {s}", i + 1));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "read `self.kind` / `self.sql_body` (`Canon::as_script` changes them for table_pair): {bad:#?}"
    );
}
