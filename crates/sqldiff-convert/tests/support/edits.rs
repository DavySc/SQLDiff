//! A generic, seeded generator of T-SQL source trees and of edits to them,
//! for the "replay equals fresh" proof test (design note W4.3, T1).
//!
//! Nothing here is specific to a corpus: a tree is a handful of tables with
//! columns, views over them, routines that read and write them, triggers and
//! functions, drawn from a seed. An [`Edit`] changes one catalog object (or
//! nothing at all) and says which kind of read it is meant to disturb, so a
//! failure prints the seed and the edit.
//!
//! The generator renders the *source* tree (T-SQL) and a base catalog in the
//! source dialect and in the target dialect, so edits reach the source
//! catalog and the target catalog (base catalogs).
//!
//! An edit only proves something if some file whose own text did not change
//! produces other output because of it. So the tree has fixed *dependents*,
//! routines whose text never changes and that use what the edits change:
//! column types in arithmetic and concatenation, an `INSERT` without a
//! column list, `SELECT *` from tables and views, writes to tables that gain
//! or lose triggers, calls of functions and procedures whose signature changes
//! or that do not exist yet, reads of base-catalog tables and columns, and
//! names that appear later (a table, a schema, a mixed-case column). Edits
//! pick their target among the objects the dependents use. The probe test in
//! `replay_equals_fresh.rs` checks that every real edit changes some
//! dependent's fresh output.

#![allow(dead_code)]

use sqldiff_core::Dialect;

/// A small deterministic generator (xorshift64*); the seed is part of every
/// failure message.
pub struct Rng(pub u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    pub fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[self.below(v.len())]
    }
}

const TYPES: &[&str] = &[
    "int",
    "bigint",
    "smallint",
    "nvarchar(40)",
    "varchar(20)",
    "decimal(12,2)",
    "datetime2(3)",
    "bit",
    "uniqueidentifier",
];

#[derive(Clone, Debug)]
pub struct Col {
    pub name: String,
    pub ty: String,
    pub null: bool,
    pub default: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Table {
    pub schema: String,
    pub name: String,
    pub cols: Vec<Col>,
    /// An extra non-clustered index on this column, when present.
    pub index: Option<usize>,
    /// System-versioned (period columns, history table).
    pub versioned: bool,
}

#[derive(Clone, Debug)]
pub struct View {
    pub name: String,
    pub over: String,
    pub star: bool,
}

#[derive(Clone, Debug)]
pub struct Routine {
    pub name: String,
    pub reads: String,
    pub param: String,
    pub function: bool,
}

#[derive(Clone, Debug)]
pub struct Trigger {
    pub name: String,
    pub on: String,
}

/// The whole source tree plus the base catalog the target side starts from.
#[derive(Clone, Debug)]
pub struct Model {
    pub tables: Vec<Table>,
    pub views: Vec<View>,
    pub routines: Vec<Routine>,
    pub triggers: Vec<Trigger>,
    /// Objects that exist already (`--catalog`).
    pub base: Vec<Table>,
    /// Extra files edits add.
    pub extra: Vec<(String, String)>,
    /// Text appended to a file by a no-op edit.
    pub notes: Vec<(String, String)>,
    /// The base catalog is given as two files (reasons name the catalogs by
    /// their number, so every reason that names the catalog changes).
    pub split_base: bool,
    /// The dependents (all procedures) are left out: the catalog has no
    /// procedure but the ones in `routines`.
    pub no_procs: bool,
    /// The target base catalog also holds names that differ from the
    /// tree's only in letter case (PostgreSQL, Db2 for i).
    pub case_clash: bool,
    /// Tables of other schemas with the names the tree uses are in the
    /// source base catalog / the target base catalog.
    pub look_a: bool,
    pub look_b: bool,
    /// The base catalog of a side defines the tree's own tables again,
    /// shaped differently.
    pub over_a: bool,
    pub over_b: bool,
    /// The source / target base catalog is not given at all.
    pub no_base_a: bool,
    pub no_base_b: bool,
    /// The job runs with `--no-catalog`.
    pub no_catalog: bool,
}

/// The three tables the fixed dependents use.
pub const ANCHORS: usize = 3;

fn key() -> Col {
    Col {
        name: "Id".into(),
        ty: "int".into(),
        null: false,
        default: None,
    }
}

fn col(name: &str, ty: &str, null: bool) -> Col {
    Col {
        name: name.into(),
        ty: ty.into(),
        null,
        default: None,
    }
}

impl Model {
    /// [`Model::generate`], and with `hidden` a system-versioned table in
    /// the tree from the start (so a star over a view or table is already
    /// refused where it hides columns).
    pub fn generate_with(seed: u64, hidden: bool) -> Model {
        let mut m = Model::generate(seed);
        if hidden {
            m.tables[2].versioned = true;
        }
        m
    }

    pub fn generate(seed: u64) -> Model {
        let mut r = Rng::new(seed);
        let mut tables = Vec::new();
        for i in 0..ANCHORS {
            let c0 = *r.pick(&["nvarchar(40)", "varchar(20)"]);
            let c1 = *r.pick(&["int", "bigint", "smallint"]);
            let nullable = r.below(2) == 0;
            tables.push(Table {
                schema: "dbo".into(),
                name: format!("T{i}"),
                cols: vec![
                    key(),
                    col("C0", c0, true),
                    col("C1", c1, nullable),
                    col("C2", "datetime2(3)", false),
                ],
                index: None,
                versioned: false,
            });
        }
        // Tables nothing depends on, so a few edits have no dependent: they
        // keep the tree from being only the anchors.
        for i in 0..1 + r.below(3) {
            tables.push(Table {
                schema: "dbo".into(),
                name: format!("X{i}"),
                cols: vec![
                    key(),
                    col(
                        "C0",
                        {
                            let _ = r.pick(TYPES);
                            "nvarchar(40)"
                        },
                        true,
                    ),
                ],
                index: None,
                versioned: false,
            });
        }
        // Fixed tables the dependents copy rows into with `INSERT ... SELECT *`.
        let sink = |name: &str, cols: Vec<Col>| Table {
            schema: "dbo".into(),
            name: name.into(),
            cols,
            index: None,
            versioned: false,
        };
        tables.push(sink(
            "Sink",
            vec![
                key(),
                col("C0", "nvarchar(40)", true),
                col("C1", "int", true),
                col("C2", "datetime2(3)", true),
            ],
        ));
        tables.push(Table {
            schema: "dbo".into(),
            name: "Idx".into(),
            cols: vec![key(), col("C0", "nvarchar(40)", true)],
            index: Some(1),
            versioned: false,
        });
        tables.push(sink("SinkNN", vec![key(), col("C1", "int", false)]));
        tables.push(sink("SinkV", vec![key(), col("C0", "nvarchar(40)", true)]));
        tables.push(sink(
            "SinkRef",
            vec![key(), col("Note", "nvarchar(40)", true)],
        ));
        let views = vec![
            View {
                name: "V0".into(),
                over: "T0".into(),
                star: r.below(2) == 0,
            },
            View {
                name: "V1".into(),
                over: "T1".into(),
                star: r.below(2) == 0,
            },
        ];
        let routines = vec![
            Routine {
                name: "F0".into(),
                reads: "T0".into(),
                param: "int".into(),
                function: true,
            },
            Routine {
                name: "Callee".into(),
                reads: "T1".into(),
                param: "int".into(),
                function: false,
            },
        ];
        let triggers = vec![Trigger {
            name: "Tr0".into(),
            on: "T1".into(),
        }];
        let base = vec![Table {
            schema: "ext".into(),
            name: "Ref".into(),
            cols: vec![key(), col("Note", "nvarchar(40)", true)],
            index: None,
            versioned: false,
        }];
        Model {
            tables,
            views,
            routines,
            triggers,
            base,
            extra: Vec::new(),
            notes: Vec::new(),
            split_base: false,
            no_procs: false,
            case_clash: false,
            look_a: false,
            look_b: false,
            over_a: false,
            over_b: false,
            no_base_a: false,
            no_base_b: false,
            no_catalog: false,
        }
    }

    /// The files of the source tree, `(relative path, text)`.
    pub fn files(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for t in &self.tables {
            out.push((
                format!("tables/{}.{}.sql", t.schema, t.name),
                create_table(t, Dialect::Tsql),
            ));
        }
        for v in &self.views {
            let body = if v.star {
                format!("SELECT * FROM dbo.{}", v.over)
            } else {
                format!("SELECT Id, C0 FROM dbo.{}", v.over)
            };
            out.push((
                format!("views/{}.sql", v.name),
                format!("CREATE VIEW dbo.{} AS {};\nGO\n", v.name, body),
            ));
        }
        for r in &self.routines {
            out.push((format!("routines/{}.sql", r.name), routine(r)));
        }
        for g in &self.triggers {
            out.push((
                format!("triggers/{}.sql", g.name),
                format!(
                    "CREATE TRIGGER dbo.{} ON dbo.{} AFTER INSERT AS\nBEGIN\n  SET NOCOUNT ON;\n  INSERT INTO dbo.X0 (Id) SELECT Id FROM inserted;\nEND\nGO\n",
                    g.name, g.on
                ),
            ));
        }
        if !self.no_procs {
            out.extend(dependents());
        }
        // A file that stays as it is whatever the edits do, and whose
        // statements call things the tree does not define: whether the
        // catalog is authoritative for functions and procedures decides
        // what they are.
        out.push((
            "triggers/TrCall.sql".into(),
            "CREATE TRIGGER dbo.TrCall ON dbo.X0 AFTER INSERT AS\nBEGIN\n  SET NOCOUNT ON;\n  EXEC dbo.GoneProc @x = 1;\n  INSERT INTO dbo.Sink (Id) SELECT dbo.GoneFn(1);\n  INSERT INTO dbo.SinkV (Id, C0) SELECT Id, C0 FROM inserted;\nEND\nGO\n".into(),
        ));
        // Names the tree spells one way and PostgreSQL / Db2 for i names
        // that differ only in case from them are checked against.
        out.push((
            "sequences/Seq0.sql".into(),
            "CREATE SEQUENCE dbo.Seq0 AS int START WITH 1;\nGO\n".into(),
        ));
        out.push(("schemas/ops.sql".into(), "CREATE SCHEMA ops;\nGO\n".into()));
        out.extend(self.extra.iter().cloned());
        for (path, note) in &self.notes {
            if let Some(f) = out.iter_mut().find(|f| &f.0 == path) {
                f.1.push_str(note);
            }
        }
        out
    }

    /// The base catalog as DDL in `d`: the source dialect for `--catalog`,
    /// the target dialect for `--catalog-b`.
    /// The base catalog as files, in the order given on the command line.
    pub fn base_files(&self, d: Dialect) -> Vec<String> {
        if self.no_base_a || self.no_catalog || (d != Dialect::Tsql && self.no_base_b) {
            return Vec::new();
        }
        let mut v = vec![self.base_catalog(d)];
        if self.split_base {
            v.push("-- a second, empty catalog file\n".to_string());
        }
        v
    }

    pub fn base_catalog(&self, d: Dialect) -> String {
        let mut s = self
            .base
            .iter()
            .map(|t| create_table(t, d))
            .collect::<Vec<_>>()
            .join("\n");
        if (d == Dialect::Tsql && self.look_a) || (d != Dialect::Tsql && self.look_b) {
            // Tables of another schema with the names the tree uses.
            for n in [
                "Fresh", "inserted", "More", "Other", "Sink", "SinkNN", "SinkRef", "SinkV", "T0",
                "T1", "T2", "X0",
            ] {
                s.push('\n');
                s.push_str(&create_table(
                    &Table {
                        schema: "ops".into(),
                        name: n.into(),
                        cols: vec![key(), col("C0", "nvarchar(40)", true)],
                        index: None,
                        versioned: false,
                    },
                    d,
                ));
            }
        }
        if (d == Dialect::Tsql && self.look_a) || (d != Dialect::Tsql && self.look_b) {
            // Views of another schema, with the names of tables and a view
            // of the tree, that read through `*`.
            for n in ["T0", "T1", "T2", "V0"] {
                s.push_str(&format!(
                    "\nCREATE VIEW {}.{} AS SELECT * FROM {}.{};\n{}",
                    q(d, "vw"),
                    q(d, n),
                    q(d, "ops"),
                    q(d, "Sink"),
                    if d == Dialect::Tsql { "GO\n" } else { "" }
                ));
            }
        }
        if (d == Dialect::Tsql && self.over_a) || (d != Dialect::Tsql && self.over_b) {
            // The tree's own tables again, with other columns; and (T-SQL)
            // a trigger of the name the tree has.
            for n in [
                "T1", "T2", "Sink", "SinkNN", "SinkRef", "SinkV", "X0", "Idx",
            ] {
                s.push('\n');
                s.push_str(&create_table(
                    &Table {
                        schema: "dbo".into(),
                        name: n.into(),
                        cols: vec![key(), col("Extra", "int", true)],
                        index: None,
                        versioned: false,
                    },
                    d,
                ));
            }
            if d == Dialect::Tsql {
                s.push_str("\nCREATE TRIGGER dbo.Tr0 ON dbo.X0 AFTER INSERT AS\nBEGIN\n  SET NOCOUNT ON;\n  INSERT INTO dbo.X0 (Id) SELECT Id FROM inserted;\nEND\nGO\n");
            }
        }
        if self.case_clash && d != Dialect::Tsql {
            // Other spellings of names the tree uses, as the names of
            // tables, columns, indexes, a schema and a routine (the objects
            // `Spellings` covers; a sequence, a trigger and a schema are
            // checked against them too).
            let other = |n: &str| match d {
                Dialect::Postgres => format!("\"{}\"", n.to_uppercase()),
                _ => format!("\"{n}\""),
            };
            for t in ["T0", "T1", "T2", "Idx"] {
                s.push_str(&format!(
                    "\nCREATE TABLE {} ({} int PRIMARY KEY, {} varchar(40), {} int, {} timestamp(3){});\n",
                    other(t),
                    other("Id"),
                    other("C0"),
                    other("C1"),
                    other("C2"),
                    if t == "T0" { format!(", {} varchar(40)", other("Note")) } else { String::new() },
                ));
                // On the T0 spelling in every case, so the table the index
                // names is not one the tree reads by its own name.
                s.push_str(&format!(
                    "CREATE INDEX {} ON {} ({});\n",
                    other(&format!("IX_{t}_{}", if t == "Idx" { 1 } else { 2 })),
                    other("T0"),
                    other("C1")
                ));
            }
            for n in ["Seq0", "ops", "Tr0", "Ref"] {
                s.push_str(&format!(
                    "CREATE TABLE {} ({} int);\n",
                    other(n),
                    other("Id")
                ));
            }
            s.push_str(&format!("CREATE SCHEMA {};\n", other("app")));
            if d == Dialect::Postgres {
                s.push_str(&format!(
                    "CREATE FUNCTION {}(x int) RETURNS int LANGUAGE sql AS 'select 1';\n",
                    other("Callee"),
                ));
            }
        }
        s
    }
}

/// Routines whose text never changes. Each holds one statement that reads
/// something an edit changes; see the module comment. One statement per
/// routine, because a routine with several writes is reported as differing
/// in its transaction behaviour whatever the catalogs say, which hides every
/// other change. Reads go into fixed sink tables with `INSERT ... SELECT`: a
/// statement that returns rows or assigns a variable is not compared at all.
fn dependents() -> Vec<(String, String)> {
    let mut stmts: Vec<(String, String)> = Vec::new();
    let mut add = |name: String, stmt: String| stmts.push((name, stmt));
    for k in 0..ANCHORS {
        let n = (k + 1) % ANCHORS;
        add(
            format!("Expr{k}"),
            format!("INSERT INTO dbo.Sink (Id, C0, C1) SELECT Id, C0 + N'x', C1 + @x FROM dbo.T{k} WHERE Id = @x"),
        );
        add(
            format!("Star{k}"),
            format!("INSERT INTO dbo.Sink SELECT * FROM dbo.T{k}"),
        );
        add(
            format!("Positional{k}"),
            format!("INSERT INTO dbo.T{k} VALUES (@x, N'a', 1, '2020-01-01')"),
        );
        add(
            format!("Defaults{k}"),
            format!("INSERT INTO dbo.T{k} (Id) VALUES (@x)"),
        );
        add(
            format!("Named{k}"),
            format!("INSERT INTO dbo.T{k} (Id, C0, C1, C2) VALUES (@x, N'a', 1, '2020-01-01')"),
        );
        // Two writes: whether the first can fail (a NULL for a NOT NULL
        // column, an omitted column with no default) decides how the pair
        // behaves when the second does not run.
        add(
            format!("NullPair{k}"),
            format!("INSERT INTO dbo.T{k} VALUES (@x, N'a', NULL, '2020-01-01');\n  UPDATE dbo.T{k} SET C0 = N'b' WHERE Id = @x"),
        );
        add(
            format!("DefaultPair{k}"),
            format!("INSERT INTO dbo.T{k} (Id, C0) VALUES (@x, N'a');\n  UPDATE dbo.T{k} SET C0 = N'b' WHERE Id = @x"),
        );
        add(
            format!("NotNullSink{k}"),
            format!("INSERT INTO dbo.SinkNN (Id, C1) SELECT Id, C1 FROM dbo.T{k}"),
        );
        add(
            format!("OrderBy{k}"),
            format!(
                "INSERT INTO dbo.Sink (Id) SELECT TOP (1) Id FROM dbo.T{k} ORDER BY C0, C1, C2"
            ),
        );
        add(
            format!("DefaultKeyword{k}"),
            format!("INSERT INTO dbo.T{k} (Id, C0, C1, C2) VALUES (@x, DEFAULT, DEFAULT, DEFAULT)"),
        );
        add(
            format!("Arith{k}"),
            format!("UPDATE dbo.T{k} SET C1 = C1 + @x"),
        );
        add(
            format!("NotIn{k}"),
            format!("INSERT INTO dbo.Sink (Id) SELECT Id FROM dbo.T{k} WHERE C1 NOT IN (SELECT C1 FROM dbo.T{n})"),
        );
        add(
            format!("Delete{k}"),
            format!("DELETE FROM dbo.T{k} WHERE C1 <> @x"),
        );
        add(
            format!("Update{k}"),
            format!("UPDATE dbo.T{k} SET C1 = 1 WHERE Id = @x"),
        );
        add(
            format!("Join{k}"),
            format!("UPDATE t SET C0 = s.C0 FROM dbo.T{k} AS t JOIN dbo.T{n} AS s ON s.C1 = t.C1"),
        );
    }
    add("DropTr0".into(), "DROP TRIGGER dbo.Tr0".into());
    add("DropTg1".into(), "DROP TRIGGER dbo.Tg1".into());
    add(
        "ViewStar".into(),
        "INSERT INTO dbo.SinkV SELECT * FROM dbo.V0".into(),
    );
    add(
        "ViewFilter".into(),
        "INSERT INTO dbo.Sink (Id) SELECT Id FROM dbo.V1 WHERE C0 = N'a'".into(),
    );
    add(
        "FunctionInt".into(),
        "INSERT INTO dbo.Sink (Id) SELECT dbo.F0(@x)".into(),
    );
    add(
        "FunctionText".into(),
        "INSERT INTO dbo.Sink (Id) SELECT dbo.F0(N'a')".into(),
    );
    add("CallInt".into(), "EXEC dbo.Callee @x".into());
    add("CallText".into(), "EXEC dbo.Callee N'a'".into());
    add("CallAdded".into(), "EXEC dbo.Added @x".into());
    add(
        "BaseAll".into(),
        "INSERT INTO dbo.SinkRef (Id, Note) SELECT Id, Note FROM ext.Ref".into(),
    );
    add(
        "BaseNote".into(),
        "INSERT INTO dbo.SinkRef (Id, Note) SELECT Id, Note FROM ext.Ref WHERE Id = @x".into(),
    );
    add(
        "BaseMore".into(),
        "INSERT INTO dbo.Sink (Id) SELECT Id FROM ext.More".into(),
    );
    add(
        "NewTable".into(),
        "INSERT INTO dbo.Sink (Id) SELECT Id FROM dbo.Fresh".into(),
    );
    add(
        "NewSchema".into(),
        "INSERT INTO dbo.Sink (Id) SELECT Id FROM app.Other".into(),
    );
    add(
        "GhostTable".into(),
        "DELETE FROM dbo.Ghost WHERE Id = @x".into(),
    );
    add(
        "GhostSchema".into(),
        "DELETE FROM zz.Ghost WHERE Id = @x".into(),
    );
    add(
        "QualMissing".into(),
        "INSERT INTO dbo.Sink (Id) SELECT T0.Nope FROM dbo.T0".into(),
    );
    add("CallGone".into(), "EXEC dbo.GoneProc @x".into());
    add(
        "FunctionGone".into(),
        "INSERT INTO dbo.Sink (Id) SELECT dbo.GoneFn(@x)".into(),
    );
    add(
        "MixedCase".into(),
        "INSERT INTO dbo.Sink (Id, C1) SELECT Id, [MiXed] FROM dbo.T0".into(),
    );
    stmts
        .into_iter()
        .map(|(name, stmt)| {
            if stmt.starts_with("DROP TRIGGER") {
                return (format!("dependents/{name}.sql"), format!("{stmt};\nGO\n"));
            }
            (
                format!("dependents/{name}.sql"),
                format!(
                    "CREATE PROCEDURE dbo.{name} @x int\nAS\nBEGIN\n  SET NOCOUNT ON;\n  {stmt};\nEND\nGO\n"
                ),
            )
        })
        .collect()
}

fn q(d: Dialect, s: &str) -> String {
    match d {
        Dialect::Tsql => s.to_string(),
        Dialect::Postgres => s.to_lowercase(),
        _ => s.to_uppercase(),
    }
}

fn ty_in(d: Dialect, ty: &str) -> String {
    if d == Dialect::Tsql {
        return ty.to_string();
    }
    match ty {
        "nvarchar(40)" | "varchar(20)" => "varchar(40)".to_string(),
        "datetime2(3)" => "timestamp(3)".to_string(),
        "bit" => "smallint".to_string(),
        "uniqueidentifier" => "char(36)".to_string(),
        t => t.to_string(),
    }
}

fn create_table(t: &Table, d: Dialect) -> String {
    let mut s = String::new();
    let tsql = d == Dialect::Tsql;
    s.push_str(&format!(
        "CREATE TABLE {}.{} (\n",
        q(d, &t.schema),
        q(d, &t.name)
    ));
    for (i, c) in t.cols.iter().enumerate() {
        s.push_str(&format!(
            "  {} {}{}{}{}",
            q(d, &c.name),
            ty_in(d, &c.ty),
            if c.null { " NULL" } else { " NOT NULL" },
            c.default
                .as_ref()
                .filter(|_| tsql)
                .map(|x| format!(" DEFAULT {x}"))
                .unwrap_or_default(),
            if i == 0 {
                format!(",\n  PRIMARY KEY ({})", q(d, &c.name))
            } else {
                String::new()
            }
        ));
        s.push_str(if i + 1 < t.cols.len() || t.versioned {
            ",\n"
        } else {
            "\n"
        });
    }
    if t.versioned && tsql {
        s.push_str(
            "  ValidFrom datetime2(7) GENERATED ALWAYS AS ROW START HIDDEN NOT NULL,\n  ValidTo datetime2(7) GENERATED ALWAYS AS ROW END HIDDEN NOT NULL,\n  PERIOD FOR SYSTEM_TIME (ValidFrom, ValidTo)\n",
        );
    } else if t.versioned {
        s.push_str("  ValidFrom timestamp NOT NULL\n");
    }
    s.push_str(");\n");
    if tsql {
        if t.versioned {
            s.push_str(&format!(
                "ALTER TABLE {}.{} SET (SYSTEM_VERSIONING = ON);\n",
                t.schema, t.name
            ));
        }
        s.push_str("GO\n");
    }
    if let Some(i) = t.index {
        s.push_str(&format!(
            "CREATE UNIQUE INDEX IX_{}_{} ON {}.{} ({});\n",
            q(d, &t.name),
            i,
            q(d, &t.schema),
            q(d, &t.name),
            q(d, &t.cols[i.min(t.cols.len() - 1)].name)
        ));
        if tsql {
            s.push_str("GO\n");
        }
    }
    s
}

fn routine(r: &Routine) -> String {
    if r.function {
        return format!(
            "CREATE FUNCTION dbo.{}(@x {}) RETURNS int AS\nBEGIN\n  RETURN (SELECT COUNT(*) FROM dbo.{} WHERE Id = @x);\nEND\nGO\n",
            r.name, r.param, r.reads
        );
    }
    format!(
        "CREATE PROCEDURE dbo.{} @x {}\nAS\nBEGIN\n  SET NOCOUNT ON;\n  INSERT INTO dbo.Sink (Id) VALUES (@x);\nEND\nGO\n",
        r.name, r.param
    )
}

/// One catalog edit. Each variant names the read it is meant to disturb.
#[derive(Clone, Copy, Debug)]
pub enum Edit {
    /// No read changes: a comment on one file.
    NoOpComment,
    /// No read changes: the catalog's tables are listed in another order.
    NoOpReorderTables,
    AddTable,
    DropTable,
    RenameTable,
    AddColumn,
    /// A column whose quoted name has upper and lower case (A5).
    AddMixedCaseColumn,
    DropColumn,
    RetypeColumn,
    ReorderColumns,
    FlipNullability,
    ChangeDefault,
    /// A unique index on one column appears or goes.
    ToggleIndex,
    /// A view's body changes (`*` or a column list).
    ViewBody,
    AddRoutine,
    DropRoutine,
    ChangeParameterType,
    /// The base catalog is two files instead of one.
    SplitBase,
    /// A table in a schema that did not exist (schema appears).
    AddSchemaTable,
    AddTrigger,
    DropTrigger,
    /// System versioning on (a HIDDEN column appears; A1).
    VersioningOn,
    /// A column of the base catalog (both dialects) changes type.
    BaseRetype,
    /// The base catalog gains a table a dependent reads.
    BaseTable,
    /// The base catalog loses its table.
    BaseDrop,
    /// Every table of the tree and of the base catalog is gone.
    DropAllTables,
    /// No procedure is defined any more (the dependents go with them).
    NoProcedures,
    /// No function is defined any more.
    DropFunctions,
    /// The target base catalog gains other spellings of the tree's names.
    CaseClash,
    /// The source base catalog defines the tree's tables again, shaped
    /// differently.
    SourceOverrides,
    /// The same for the target base catalog.
    TargetOverrides,
    /// The source base catalog gains tables of another schema with the
    /// tree's names.
    SourceLookalikes,
    /// The same for the target base catalog.
    TargetLookalikes,
    /// No base catalog is given (the target side needs the source's).
    NoBaseA,
    /// The target base catalog is not given.
    NoBaseB,
    /// A table only a trigger's pseudo table reads changes type.
    RetypeSide,
    /// The job runs with `--no-catalog`: no side has a catalog.
    NoCatalog,
}

pub const ALL_EDITS: &[Edit] = &[
    Edit::NoOpComment,
    Edit::NoOpReorderTables,
    Edit::AddTable,
    Edit::DropTable,
    Edit::RenameTable,
    Edit::AddColumn,
    Edit::AddMixedCaseColumn,
    Edit::DropColumn,
    Edit::RetypeColumn,
    Edit::ReorderColumns,
    Edit::FlipNullability,
    Edit::ChangeDefault,
    Edit::ToggleIndex,
    Edit::ViewBody,
    Edit::AddRoutine,
    Edit::DropRoutine,
    Edit::ChangeParameterType,
    Edit::AddSchemaTable,
    Edit::AddTrigger,
    Edit::DropTrigger,
    Edit::VersioningOn,
    Edit::BaseRetype,
    Edit::BaseTable,
    Edit::BaseDrop,
    Edit::SplitBase,
    Edit::DropAllTables,
    Edit::NoProcedures,
    Edit::DropFunctions,
    Edit::CaseClash,
    Edit::SourceOverrides,
    Edit::TargetOverrides,
    Edit::SourceLookalikes,
    Edit::TargetLookalikes,
    Edit::NoBaseA,
    Edit::NoBaseB,
    Edit::RetypeSide,
    Edit::NoCatalog,
];

impl Edit {
    /// Whether the edit is meant to change nothing a file can see.
    pub fn is_no_op(&self) -> bool {
        matches!(self, Edit::NoOpComment | Edit::NoOpReorderTables)
    }

    /// Applies the edit to `m`, drawing every choice from `r`. Returns a
    /// short description for failure messages.
    pub fn apply(&self, m: &mut Model, r: &mut Rng) -> String {
        // The anchor table and a non-key column of it the edit works on.
        let ti = r.below(ANCHORS);
        let ci = 1 + r.below(3);
        let tname = m.tables[ti].name.clone();
        match self {
            Edit::NoOpComment => {
                let files = m.files();
                let f = &files[r.below(files.len())];
                m.notes.push((f.0.clone(), "-- unrelated comment\n".into()));
                format!("comment on {}", f.0)
            }
            Edit::NoOpReorderTables => {
                m.tables.reverse();
                "table order reversed (files unchanged)".into()
            }
            Edit::AddTable => {
                m.tables.push(Table {
                    schema: "dbo".into(),
                    name: "Fresh".into(),
                    cols: vec![key(), col("C0", "int", true)],
                    index: None,
                    versioned: false,
                });
                "add table Fresh".into()
            }
            Edit::DropTable => {
                m.tables.remove(ti);
                format!("drop table {tname}")
            }
            Edit::RenameTable => {
                let new = format!("{tname}x");
                m.tables[ti].name = new.clone();
                format!("rename table {tname} to {new} (dependents keep the old name)")
            }
            Edit::AddColumn => {
                let nm = format!("X{}", r.below(100));
                m.tables[ti].cols.push(col(&nm, "int", true));
                format!("add column {nm} to {tname}")
            }
            Edit::AddMixedCaseColumn => {
                m.tables[0].cols.push(col("MiXed", "int", true));
                "add column MiXed to T0".into()
            }
            Edit::DropColumn => {
                let c = m.tables[ti].cols.remove(ci);
                m.tables[ti].index = None;
                format!("drop column {} of {tname}", c.name)
            }
            Edit::RetypeColumn => {
                let c = &mut m.tables[ti].cols[ci];
                let new = match c.ty.as_str() {
                    "nvarchar(40)" => "int",
                    "varchar(20)" => "bigint",
                    "int" | "smallint" => "nvarchar(40)",
                    "bigint" => "varchar(20)",
                    _ => "int",
                };
                let old = std::mem::replace(&mut c.ty, new.into());
                c.default = None;
                format!("retype {tname}.{} {old} -> {new}", c.name)
            }
            Edit::ReorderColumns => {
                let n = m.tables[ti].cols.len();
                m.tables[ti].cols[1..n].reverse();
                m.tables[ti].index = None;
                format!("reorder columns of {tname}")
            }
            Edit::FlipNullability => {
                let c = &mut m.tables[ti].cols[ci];
                c.null ^= true;
                format!("flip nullability {tname}.{}", c.name)
            }
            Edit::ChangeDefault => {
                let c = &mut m.tables[ti].cols[ci];
                c.default = match (&c.default, c.ty.contains("char"), c.ty.contains("datetime")) {
                    (Some(_), _, _) => None,
                    (None, true, _) => Some("N'a'".into()),
                    (None, _, true) => Some("SYSDATETIME()".into()),
                    (None, _, _) => Some("1".into()),
                };
                format!("change default {tname}.{}", c.name)
            }
            Edit::ToggleIndex => {
                m.tables[ti].index = match m.tables[ti].index {
                    Some(_) => None,
                    None => Some(2),
                };
                format!("toggle unique index on {tname}.C1")
            }
            Edit::ViewBody => {
                // Both views, so some dependent of each sees it.
                let _ = r.below(m.views.len());
                for v in &mut m.views {
                    v.star ^= true;
                }
                "view bodies".to_string()
            }
            Edit::AddRoutine => {
                m.routines.push(Routine {
                    name: "Added".into(),
                    reads: "T2".into(),
                    param: "int".into(),
                    function: false,
                });
                "add routine Added".into()
            }
            Edit::DropRoutine => {
                let ri = r.below(m.routines.len());
                let g = m.routines.remove(ri);
                format!("drop routine {}", g.name)
            }
            Edit::ChangeParameterType => {
                let ri = r.below(m.routines.len());
                let new = if m.routines[ri].param == "int" {
                    "nvarchar(40)"
                } else {
                    "int"
                };
                m.routines[ri].param = new.into();
                format!("parameter of {} -> {new}", m.routines[ri].name)
            }
            Edit::AddSchemaTable => {
                m.tables.push(Table {
                    schema: "app".into(),
                    name: "Other".into(),
                    cols: vec![key()],
                    index: None,
                    versioned: false,
                });
                m.extra
                    .push(("schemas/app.sql".into(), "CREATE SCHEMA app;\nGO\n".into()));
                "table app.Other in a new schema".into()
            }
            Edit::AddTrigger => {
                // A table other than the one Tr0 is on.
                let on = if ti == 1 { "T2".to_string() } else { tname };
                m.triggers.push(Trigger {
                    name: "Tg1".into(),
                    on: on.clone(),
                });
                format!("add trigger Tg1 on {on}")
            }
            Edit::DropTrigger => {
                m.triggers.clear();
                "drop trigger Tr0".into()
            }
            Edit::VersioningOn => {
                m.tables[ti].versioned = true;
                format!("system versioning on for {tname}")
            }
            Edit::BaseRetype => {
                m.base[0].cols[1].ty = "int".into();
                "base catalog: ext.Ref.Note becomes int".into()
            }
            Edit::BaseTable => {
                m.base.push(Table {
                    schema: "ext".into(),
                    name: "More".into(),
                    cols: vec![key()],
                    index: None,
                    versioned: false,
                });
                "base catalog: ext.More appears".into()
            }
            Edit::SplitBase => {
                m.split_base = true;
                "the base catalog is two files".into()
            }
            Edit::DropAllTables => {
                m.tables.clear();
                m.views.clear();
                m.base.clear();
                "no table or view at all".into()
            }
            Edit::NoProcedures => {
                m.no_procs = true;
                m.routines.retain(|r| r.function);
                "no procedure".into()
            }
            Edit::DropFunctions => {
                m.routines.retain(|r| !r.function);
                "no function".into()
            }
            Edit::SourceOverrides => {
                m.over_a = true;
                "the source base catalog redefines tree tables".into()
            }
            Edit::TargetOverrides => {
                m.over_b = true;
                "the target base catalog redefines tree tables".into()
            }
            Edit::SourceLookalikes => {
                m.look_a = true;
                "the source base catalog has lookalike tables".into()
            }
            Edit::TargetLookalikes => {
                m.look_b = true;
                "the target base catalog has lookalike tables".into()
            }
            Edit::NoBaseA => {
                m.no_base_a = true;
                "no base catalog".into()
            }
            Edit::NoBaseB => {
                m.no_base_b = true;
                "no target base catalog".into()
            }
            Edit::NoCatalog => {
                m.no_catalog = true;
                "checked without a catalog".into()
            }
            Edit::RetypeSide => {
                let x = m.tables.iter_mut().find(|t| t.name == "X0").unwrap();
                x.cols[1].ty = "int".into();
                "X0.C0 becomes int".into()
            }
            Edit::CaseClash => {
                m.case_clash = true;
                "the target base catalog has other spellings".into()
            }
            Edit::BaseDrop => {
                m.base.clear();
                "base catalog: ext.Ref disappears".into()
            }
        }
    }
}
