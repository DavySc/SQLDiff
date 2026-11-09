//! `LStmt` keeps no token list: it re-tokenises its node on demand
//! (`LStmt::toks`), and `Canon::drops_period` asks the per-unit summary
//! `Unit::dml_words` instead of scanning every statement. Two oracles keep
//! that exact:
//!
//! - `dml_words` answers `period ∩ dml_words ≠ ∅` as the scan of every
//!   statement's tokens did (the scan is kept here as `scan_drops`), for every
//!   name that occurs in a unit lowered from the corpus pairs, mutations and
//!   catalog files, from vendor code, and from the temporal-table sources
//!   below, and for every call of `drops_period` in the comparisons of them.
//! - While the owned list existed (`LStmt::toks: Vec<Tok>`), the on-demand
//!   list was checked equal to it, token by token (kind, text, upper-case
//!   text, offset), for every statement of every unit lowered or compared
//!   here (see the commit that removed the field for the counts). The field
//!   is gone, so this file keeps the other oracle only.

use super::lower::{self, Unit};
use super::schema::Schema;
use super::{ast, Analyzed};
use crate::artifact::Artifact;
use crate::model::Side;
use sqldiff_syntax::Dialect;
use std::cell::Cell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

thread_local! {
    /// Units and statements checked by [`check_unit`]; statements that start
    /// after the first token (a `CREATE VIEW` query); units whose summary is
    /// not empty.
    static UNITS: Cell<[usize; 4]> = const { Cell::new([0; 4]) };
    /// Calls of `drops_period`: all, with a period column, answered true by
    /// the statement scan (not by a switch), answered false with a period.
    static CALLS: Cell<[usize; 4]> = const { Cell::new([0; 4]) };
}

fn bump(c: &'static std::thread::LocalKey<Cell<[usize; 4]>>, by: [usize; 4]) {
    c.with(|c| {
        let mut v = c.get();
        for (x, y) in v.iter_mut().zip(by) {
            *x += y;
        }
        c.set(v);
    });
}

/// `Canon::drops_period`'s statement scan as it was before `dml_words`: some
/// statement starts with INSERT, UPDATE or MERGE and has a `Word` or `Quoted`
/// token that names a period column.
fn scan_drops(u: &Unit, period: &[&str]) -> bool {
    u.stmts.iter().any(|s| {
        let toks = s.toks();
        toks.first()
            .is_some_and(|f| matches!(f.up.as_str(), "INSERT" | "UPDATE" | "MERGE"))
            && toks.iter().any(|x| {
                matches!(x.kind, ast::K::Word | ast::K::Quoted)
                    && period.contains(
                        &sqldiff_syntax::ast::unquote(&x.text)
                            .to_lowercase()
                            .as_str(),
                    )
            })
    })
}

/// The summary answers as the scan does for every name in the unit (and one
/// that is in none).
fn words_agree(u: &Unit) {
    let mut names: BTreeSet<String> = BTreeSet::new();
    names.insert("no_such_name_anywhere".into());
    for s in &u.stmts {
        for x in s.toks().iter() {
            names.insert(sqldiff_syntax::ast::unquote(&x.text).to_lowercase());
        }
    }
    for n in &names {
        assert_eq!(
            u.dml_words.contains(n),
            scan_drops(u, &[n.as_str()]),
            "name {n:?}"
        );
    }
    // Nothing in the summary that no statement names.
    for w in &u.dml_words {
        assert!(names.contains(w), "{w:?} is in no statement");
    }
    // Two names at once, as a table with two period columns asks.
    let v: Vec<&String> = names.iter().collect();
    for pair in v.windows(2) {
        let p = [pair[0].as_str(), pair[1].as_str()];
        assert_eq!(
            p.iter().any(|n| u.dml_words.contains(*n)),
            scan_drops(u, &p),
            "names {p:?}"
        );
    }
}

/// Called by `items` (test builds only) for each unit it lowers a comparison
/// from.
pub(super) fn check_unit(u: &Unit) {
    let toks: usize = u.stmts.iter().map(|s| s.toks().len()).sum();
    if toks <= 300 {
        words_agree(u);
    }
    bump(
        &UNITS,
        [
            1,
            u.stmts.len(),
            u.stmts.iter().filter(|s| s.tok_from > 0).count(),
            usize::from(!u.dml_words.is_empty()),
        ],
    );
}

/// Called by `Canon::drops_period` (test builds only): the answer, with the
/// part that does not come from a switch.
pub(super) fn drops_period_agrees(u: &Unit, period: &[&str], switched: bool, got: bool) {
    let scan = scan_drops(u, period);
    assert_eq!(got, switched || scan, "period {period:?}");
    bump(
        &CALLS,
        [
            1,
            usize::from(!period.is_empty()),
            usize::from(!switched && scan),
            usize::from(!switched && !scan && !period.is_empty()),
        ],
    );
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

type Source = (String, Dialect, String);

/// The sides of every corpus pair and mutation, the catalog files and the
/// vendor code, with their dialects, line endings normalised. The path of a
/// side is `<dir>/<file>`; the sides of one case are consecutive.
fn corpus_sources() -> Vec<Source> {
    let mut out: Vec<Source> = Vec::new();
    for kind in ["pairs", "mutations"] {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(corpus().join(kind))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.join("case.toml").is_file())
            .collect();
        dirs.sort();
        for dir in dirs {
            let case: toml::Table = read(&dir.join("case.toml")).parse().unwrap();
            for (_, s) in case["sides"].as_table().unwrap() {
                let file = s["file"].as_str().unwrap();
                out.push((
                    format!("{}/{file}", dir.display()),
                    dialect(s["dialect"].as_str().unwrap()),
                    read(&dir.join(file)),
                ));
            }
        }
    }
    for (file, d) in [
        ("tsql.sql", Dialect::Tsql),
        ("pg.sql", Dialect::Postgres),
        ("db2i.sql", Dialect::Db2i),
    ] {
        out.push((
            format!("catalog/{file}"),
            d,
            read(&corpus().join("catalog").join(file)),
        ));
    }
    out.push((
        "sys_catalog.sql".into(),
        Dialect::Tsql,
        include_str!("sys_catalog.sql").replace("\r\n", "\n"),
    ));
    out.push((
        "pg_catalog.sql".into(),
        Dialect::Postgres,
        include_str!("pg_catalog.sql").replace("\r\n", "\n"),
    ));
    for rel in [
        "camel/dtaq_to_kafka/banking_kafka_example.sql",
        "camel/db2_bash/setup.sql",
        "nodejs/fastify/dns_api/dns.sql",
    ] {
        let p = corpus()
            .join("real-code/vendor/ibmi-oss-examples")
            .join(rel);
        out.push((rel.into(), Dialect::Db2i, read(&p)));
    }
    out
}

/// Sources that lower a `CREATE VIEW … AS query` into one statement whose
/// tokens start after `AS`.
fn view_sources() -> Vec<Source> {
    let (t, p) = (Dialect::Tsql, Dialect::Postgres);
    let s = |n: &str, d, x: &str| (n.to_string(), d, x.to_string());
    vec![
        s(
            "v1.sql",
            t,
            "CREATE VIEW dbo.v AS SELECT a, b FROM dbo.t WHERE a > 1;\n",
        ),
        s(
            "v2.sql",
            t,
            "CREATE OR ALTER VIEW dbo.v (x, y) AS\nSELECT a, b FROM dbo.t;\n",
        ),
        s(
            "v3.sql",
            p,
            "CREATE OR REPLACE VIEW s.v AS SELECT a, b FROM s.t WHERE a > 1;\n",
        ),
        s("v4.sql", p, "CREATE VIEW v AS SELECT 1 AS one;\n"),
    ]
}

/// System-versioned tables written by code that switches versioning or
/// names a period column, and code that does neither (`temporal_switch.rs`
/// in `sqldiff-core` runs such sources through the whole product).
const CAT_T: &str = "CREATE TABLE Sales.Groups (
    GroupID int NOT NULL CONSTRAINT PK_Groups PRIMARY KEY,
    Name nvarchar(50) NOT NULL,
    ValidFrom datetime2(6) GENERATED ALWAYS AS ROW START NOT NULL,
    ValidTo datetime2(6) GENERATED ALWAYS AS ROW END NOT NULL,
    PERIOD FOR SYSTEM_TIME (ValidFrom, ValidTo)
)
WITH (SYSTEM_VERSIONING = ON (HISTORY_TABLE = Sales.Groups_Archive, DATA_CONSISTENCY_CHECK = ON));
GO
CREATE TABLE Sales.Groups_Archive (
    GroupID int NOT NULL,
    Name nvarchar(50) NOT NULL,
    ValidFrom datetime2(6) NOT NULL,
    ValidTo datetime2(6) NOT NULL
);
GO
CREATE TABLE Sales.Plain (
    Id int NOT NULL CONSTRAINT PK_Plain PRIMARY KEY,
    Total AS Id * 2
);
";

const CAT_P: &str = "CREATE TABLE Sales.Groups (
    GroupID integer NOT NULL CONSTRAINT PK_Groups PRIMARY KEY,
    Name varchar(50) NOT NULL,
    ValidFrom timestamp(6) NOT NULL DEFAULT '-infinity',
    ValidTo timestamp(6) NOT NULL DEFAULT '-infinity'
);
CREATE TABLE Sales.Groups_Archive (
    GroupID integer NOT NULL,
    Name varchar(50) NOT NULL,
    ValidFrom timestamp(6) NOT NULL,
    ValidTo timestamp(6) NOT NULL
);
CREATE TABLE Sales.Plain (
    Id integer NOT NULL CONSTRAINT PK_Plain PRIMARY KEY,
    Total integer GENERATED ALWAYS AS (Id * 2) STORED
);
";

const OFF_T: &str = "    ALTER TABLE Sales.Groups SET (SYSTEM_VERSIONING = OFF);\n    ALTER TABLE Sales.Groups DROP PERIOD FOR SYSTEM_TIME;";
const OFF_P: &str = "    ALTER TABLE Sales.Groups DISABLE TRIGGER Groups_period_update;\n    ALTER TABLE Sales.Groups DISABLE TRIGGER Groups_versioning, DISABLE TRIGGER Groups_versioning_truncate;";

fn proc_t(body: &str) -> String {
    format!("CREATE PROCEDURE dbo.Load\nAS\nBEGIN\n    SET XACT_ABORT ON;\n{body}\nEND;")
}

fn proc_p(body: &str) -> String {
    format!("CREATE OR REPLACE PROCEDURE Load()\nLANGUAGE plpgsql\nAS $$\nBEGIN\n{body}\nEND;\n$$;")
}

/// (T-SQL body, PostgreSQL body) pairs over the temporal tables.
fn temporal_bodies() -> Vec<(String, String)> {
    let ins_t = |c: &str, v: &str| format!("    INSERT Sales.Groups ({c}) VALUES ({v});");
    let ins_p = |c: &str, v: &str| format!("    INSERT INTO Sales.Groups ({c}) VALUES ({v});");
    let upd =
        |c: &str| format!("    UPDATE Sales.Groups SET {c} = '2020-01-02' WHERE GroupID = 1;");
    let all = "GroupID, Name, ValidFrom, ValidTo";
    let two = "GroupID, Name";
    let vals = "1, 'a', '2020-01-01', '9999-12-31'";
    vec![
        // Switched off, then a full write; the period left out; a period write.
        (
            format!("{OFF_T}\n{}", ins_t(all, vals)),
            format!("{OFF_P}\n{}", ins_p(all, vals)),
        ),
        (
            format!("{OFF_T}\n{}", ins_t(two, "1, 'a'")),
            format!("{OFF_P}\n{}", ins_p(two, "1, 'a'")),
        ),
        (
            format!("{OFF_T}\n{}", upd("ValidFrom")),
            format!("{OFF_P}\n{}", upd("ValidFrom")),
        ),
        // No switch: a period write by name, then an INSERT that leaves it out.
        (
            format!("{}\n{}", upd("ValidFrom"), ins_t(two, "2, 'b'")),
            format!("{}\n{}", upd("ValidFrom"), ins_p(two, "2, 'b'")),
        ),
        // The period named quoted and in another case, in an UPDATE only.
        (
            format!("{}\n{}", upd("[VALIDTO]"), ins_t(two, "2, 'b'")),
            format!("{}\n{}", upd("\"validto\""), ins_p(two, "2, 'b'")),
        ),
        // Neither: the versioning triggers stamp the period.
        (ins_t(two, "1, 'a'"), ins_p(two, "1, 'a'")),
        // The name in a statement that is not INSERT, UPDATE or MERGE.
        (
            format!("    SELECT ValidFrom FROM Sales.Groups;\n{}", ins_t(two, "1, 'a'")),
            format!("    SELECT ValidFrom FROM Sales.Groups;\n{}", ins_p(two, "1, 'a'")),
        ),
        // MERGE naming the period.
        (
            "    MERGE Sales.Groups AS g USING (SELECT 1 AS GroupID) AS s ON g.GroupID = s.GroupID WHEN MATCHED THEN UPDATE SET ValidTo = '2020-01-01';\n".to_string()
                + &ins_t(two, "1, 'a'"),
            "    UPDATE Sales.Groups SET ValidTo = '2020-01-01' WHERE GroupID = 1;\n".to_string()
                + &ins_p(two, "1, 'a'"),
        ),
        // A plain table.
        (
            "    UPDATE Sales.Plain SET Total = 3 WHERE Id = 1;".into(),
            "    UPDATE Sales.Plain SET Total = 3 WHERE Id = 1;".into(),
        ),
    ]
}

fn temporal_sources() -> Vec<(Source, Source)> {
    temporal_bodies()
        .into_iter()
        .enumerate()
        .map(|(i, (a, b))| {
            (
                (format!("t{i}.a.sql"), Dialect::Tsql, proc_t(&a)),
                (format!("t{i}.b.sql"), Dialect::Postgres, proc_p(&b)),
            )
        })
        .collect()
}

fn artifact(s: &Source) -> Artifact {
    Artifact::new(&s.0, s.1, s.2.clone())
}

/// Lowers every source on its own and against each other dialect: the
/// on-demand tokens and the summary are checked for each unit.
#[test]
fn units_lowered_from_the_corpus_agree() {
    let mut sources = corpus_sources();
    sources.extend(view_sources());
    for (a, b) in temporal_sources() {
        sources.push(a);
        sources.push(b);
    }
    assert!(sources.len() > 40, "corpus not found at {:?}", corpus());
    UNITS.with(|c| c.set([0; 4]));
    let mut with_words = 0;
    for s in &sources {
        let x = artifact(s);
        let a = Analyzed::new(&x, Side::A, 0);
        let mut units = vec![lower::lower(&a)];
        for other in [Dialect::Tsql, Dialect::Postgres, Dialect::Db2i] {
            units.push(lower::lower_against(&a, other));
        }
        for u in &units {
            check_unit(u);
            words_agree(u);
            with_words += usize::from(!u.dml_words.is_empty());
        }
    }
    let [units, stmts, offset, _] = UNITS.with(Cell::get);
    eprintln!(
        "lowered {} sources: {units} units, {stmts} statements, {offset} with tok_from > 0, {with_words} units with a non-empty dml_words",
        sources.len()
    );
    assert!(
        stmts > 200 && offset >= 4 && with_words > 20,
        "the oracle saw too little"
    );
}

/// Every comparison of the corpus pairs and mutations (both directions) and
/// of the temporal sources lowers and canonicalises units the way a run does
/// (`items` checks each unit; `drops_period` checks each answer).
#[test]
fn comparisons_of_the_corpus_agree() {
    UNITS.with(|c| c.set([0; 4]));
    CALLS.with(|c| c.set([0; 4]));
    let mut cases: Vec<(Source, Source, Vec<Source>)> = Vec::new();
    // Corpus: the sides of each case, pairwise both ways.
    let all = corpus_sources();
    let mut by_dir: Vec<Vec<Source>> = Vec::new();
    let mut last = String::new();
    for s in all
        .iter()
        .filter(|s| s.0.contains("/pairs/") || s.0.contains("/mutations/"))
    {
        let dir = Path::new(&s.0).parent().unwrap().display().to_string();
        if dir != last {
            by_dir.push(Vec::new());
            last = dir;
        }
        by_dir.last_mut().unwrap().push(s.clone());
    }
    for sides in &by_dir {
        for i in 0..sides.len() {
            for j in 0..sides.len() {
                if i != j {
                    cases.push((sides[i].clone(), sides[j].clone(), Vec::new()));
                }
            }
        }
    }
    assert!(cases.len() > 40, "corpus not found at {:?}", corpus());
    let corpus_cases = cases.len();
    // Temporal sources, with the catalog both sides read.
    let cat_t: Source = ("cat.sql".into(), Dialect::Tsql, CAT_T.into());
    let cat_p: Source = ("cat.pg.sql".into(), Dialect::Postgres, CAT_P.into());
    for (a, b) in temporal_sources() {
        cases.push((a.clone(), b.clone(), vec![cat_t.clone(), cat_p.clone()]));
        cases.push((b, a, vec![cat_p.clone(), cat_t.clone()]));
    }
    for (a, b, cats) in &cases {
        let (xa, xb) = (artifact(a), artifact(b));
        let (aa, ab) = (
            Analyzed::new(&xa, Side::A, 0),
            Analyzed::new(&xb, Side::B, 0),
        );
        let load = |x: &Artifact, cat: Option<&Source>| match cat {
            Some(c) => Schema::load(&[Artifact::new(&c.0, c.1, c.2.clone()), x.clone()]),
            None => Schema::load(std::slice::from_ref(x)),
        };
        let (sa, sb) = (load(&xa, cats.first()), load(&xb, cats.get(1)));
        let _ = super::compare(&aa, &ab, &sa, &sb, &[]);
    }
    let [units, stmts, offset, with_words] = UNITS.with(Cell::get);
    let [calls, with_period, by_scan, not_by_scan] = CALLS.with(Cell::get);
    eprintln!(
        "{} comparisons ({corpus_cases} corpus): {units} units, {stmts} statements ({offset} with tok_from > 0), {with_words} units with a non-empty dml_words; drops_period: {calls} calls, {with_period} with a period column, {by_scan} true by the statement scan, {not_by_scan} false by it",
        cases.len()
    );
    assert!(units > 100 && stmts > 300, "the oracle saw too little");
    assert!(
        by_scan > 0 && not_by_scan > 0,
        "drops_period was not asked both ways: the oracle saw nothing"
    );
}
