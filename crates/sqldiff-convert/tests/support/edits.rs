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
//! catalog, the target catalog (base catalogs) and the stand-ins.

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
    pub writes: Option<String>,
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
    /// Objects that exist already (`--catalog`): `(schema, name, columns)`.
    pub base: Vec<Table>,
    /// Extra files edits add (an `ALTER TABLE` file, a comment).
    pub extra: Vec<(String, String)>,
    /// Text appended to a file by a no-op edit.
    pub notes: Vec<(String, String)>,
}

fn col(r: &mut Rng, name: &str) -> Col {
    let ty = r.pick(TYPES).to_string();
    let null = r.below(2) == 0;
    let default = match (ty.as_str(), r.below(3)) {
        ("int" | "bigint" | "smallint", 0) => Some("0".to_string()),
        ("nvarchar(40)" | "varchar(20)", 0) => Some("N''".to_string()),
        _ => None,
    };
    Col {
        name: name.to_string(),
        ty,
        null,
        default,
    }
}

impl Model {
    pub fn generate(seed: u64) -> Model {
        let mut r = Rng::new(seed);
        let n_tables = 4 + r.below(3);
        let mut tables = Vec::new();
        for i in 0..n_tables {
            let mut cols = vec![Col {
                name: "Id".into(),
                ty: "int".into(),
                null: false,
                default: None,
            }];
            for j in 0..2 + r.below(3) {
                cols.push(col(&mut r, &format!("C{j}")));
            }
            let index = (r.below(2) == 0).then(|| 1 + r.below(cols.len() - 1));
            tables.push(Table {
                schema: "dbo".into(),
                name: format!("T{i}"),
                cols,
                index,
                versioned: false,
            });
        }
        let mut views = Vec::new();
        for i in 0..2 + r.below(2) {
            views.push(View {
                name: format!("V{i}"),
                over: r.pick(&tables).name.clone(),
                star: r.below(2) == 0,
            });
        }
        let mut routines = Vec::new();
        for i in 0..3 + r.below(3) {
            let writes = (r.below(2) == 0).then(|| r.pick(&tables).name.clone());
            routines.push(Routine {
                name: format!("P{i}"),
                reads: r.pick(&tables).name.clone(),
                writes,
                param: r.pick(&["int", "bigint", "nvarchar(40)"]).to_string(),
                function: false,
            });
        }
        routines.push(Routine {
            name: "F0".into(),
            reads: tables[0].name.clone(),
            writes: None,
            param: "int".into(),
            function: true,
        });
        let triggers = vec![Trigger {
            name: "Tr0".into(),
            on: tables[1].name.clone(),
        }];
        let base = vec![Table {
            schema: "ext".into(),
            name: "Ref".into(),
            cols: vec![
                Col {
                    name: "Id".into(),
                    ty: "int".into(),
                    null: false,
                    default: None,
                },
                Col {
                    name: "Note".into(),
                    ty: "nvarchar(40)".into(),
                    null: true,
                    default: None,
                },
            ],
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
        }
    }

    fn table(&self, name: &str) -> Option<&Table> {
        self.tables.iter().find(|t| t.name == name)
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
            let body = match (self.table(&v.over), v.star) {
                (_, true) => format!("SELECT * FROM dbo.{}", v.over),
                (Some(t), false) => format!(
                    "SELECT {} FROM dbo.{}",
                    t.cols
                        .iter()
                        .take(2)
                        .map(|c| c.name.clone())
                        .collect::<Vec<_>>()
                        .join(", "),
                    v.over
                ),
                (None, false) => format!("SELECT 1 AS Id FROM dbo.{}", v.over),
            };
            out.push((
                format!("views/{}.sql", v.name),
                format!("CREATE VIEW dbo.{} AS {};\nGO\n", v.name, body),
            ));
        }
        for r in &self.routines {
            out.push((format!("routines/{}.sql", r.name), routine(self, r)));
        }
        for g in &self.triggers {
            out.push((
                format!("triggers/{}.sql", g.name),
                format!(
                    "CREATE TRIGGER dbo.{} ON dbo.{} AFTER INSERT AS\nBEGIN\n  SET NOCOUNT ON;\n  UPDATE dbo.{} SET Id = Id WHERE Id IN (SELECT Id FROM inserted);\nEND\nGO\n",
                    g.name, g.on, g.on
                ),
            ));
        }
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
    pub fn base_catalog(&self, d: Dialect) -> String {
        self.base
            .iter()
            .map(|t| create_table(t, d))
            .collect::<Vec<_>>()
            .join("\n")
    }
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
            "CREATE INDEX IX_{}_{} ON {}.{} ({});\n",
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

fn routine(m: &Model, r: &Routine) -> String {
    let first = m
        .table(&r.reads)
        .and_then(|t| t.cols.first())
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "Id".into());
    if r.function {
        return format!(
            "CREATE FUNCTION dbo.{}(@x {}) RETURNS int AS\nBEGIN\n  RETURN (SELECT COUNT(*) FROM dbo.{} WHERE {} = @x);\nEND\nGO\n",
            r.name, r.param, r.reads, first
        );
    }
    let mut body = format!(
        "  SELECT {} FROM dbo.{} WHERE {} = @x;\n",
        first, r.reads, first
    );
    if let Some(w) = &r.writes {
        let wc = m
            .table(w)
            .and_then(|t| t.cols.first())
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "Id".into());
        body.push_str(&format!(
            "  UPDATE dbo.{w} SET {wc} = {wc} WHERE {wc} = @x;\n  SELECT TOP (1) * FROM ext.Ref;\n"
        ));
    }
    format!(
        "CREATE PROCEDURE dbo.{} @x {}\nAS\nBEGIN\n  SET NOCOUNT ON;\n{}END\nGO\n",
        r.name, r.param, body
    )
}

/// One catalog edit. Each variant names the read it is meant to disturb.
#[derive(Clone, Debug)]
pub enum Edit {
    /// No read changes: a comment on one file.
    NoOpComment,
    /// No read changes: a table definition's order in the catalog moves.
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
    ToggleIndex,
    /// A view's body changes.
    ViewBody,
    /// An `ALTER TABLE ... ADD` in a file of its own (A4: the source table
    /// of a view changes).
    AlterSourceTable,
    AddRoutine,
    DropRoutine,
    ChangeParameterType,
    /// A table in a schema that did not exist (schema appears).
    AddSchemaTable,
    AddTrigger,
    DropTrigger,
    /// System versioning on (a HIDDEN period column appears; A1).
    VersioningOn,
    /// The base catalog (target side) gains a column.
    BaseColumn,
    /// The base catalog gains a table another file reads.
    BaseTable,
    /// The base catalog loses its table.
    BaseDrop,
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
    Edit::AlterSourceTable,
    Edit::AddRoutine,
    Edit::DropRoutine,
    Edit::ChangeParameterType,
    Edit::AddSchemaTable,
    Edit::AddTrigger,
    Edit::DropTrigger,
    Edit::VersioningOn,
    Edit::BaseColumn,
    Edit::BaseTable,
    Edit::BaseDrop,
];

impl Edit {
    /// Applies the edit to `m`, drawing every choice from `r`. Returns a
    /// short description for failure messages.
    pub fn apply(&self, m: &mut Model, r: &mut Rng) -> String {
        let ti = r.below(m.tables.len());
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
                let name = format!("N{}", r.below(1000));
                let cols = vec![
                    Col {
                        name: "Id".into(),
                        ty: "int".into(),
                        null: false,
                        default: None,
                    },
                    col(r, "C0"),
                ];
                m.tables.push(Table {
                    schema: "dbo".into(),
                    name: name.clone(),
                    cols,
                    index: None,
                    versioned: false,
                });
                format!("add table {name}")
            }
            Edit::DropTable => {
                let t = m.tables.remove(ti);
                format!("drop table {}", t.name)
            }
            Edit::RenameTable => {
                let old = m.tables[ti].name.clone();
                let new = format!("{old}x");
                m.tables[ti].name = new.clone();
                format!("rename table {old} to {new} (dependents keep the old name)")
            }
            Edit::AddColumn => {
                let nm = format!("X{}", r.below(100));
                let c = col(r, &nm);
                let n = c.name.clone();
                m.tables[ti].cols.push(c);
                format!("add column {n} to {}", m.tables[ti].name)
            }
            Edit::AddMixedCaseColumn => {
                m.tables[ti].cols.push(Col {
                    name: "MiXed".into(),
                    ty: "int".into(),
                    null: true,
                    default: None,
                });
                format!("add column MiXed to {}", m.tables[ti].name)
            }
            Edit::DropColumn => {
                let n = m.tables[ti].cols.len();
                if n > 2 {
                    let c = m.tables[ti].cols.remove(1 + r.below(n - 1));
                    m.tables[ti].index = None;
                    format!("drop column {} of {}", c.name, m.tables[ti].name)
                } else {
                    "drop column (table too narrow, no change)".into()
                }
            }
            Edit::RetypeColumn => {
                let n = m.tables[ti].cols.len();
                let ci = 1 + r.below(n - 1);
                let old = m.tables[ti].cols[ci].ty.clone();
                let mut new = r.pick(TYPES).to_string();
                if new == old {
                    new = "float".into();
                }
                m.tables[ti].cols[ci].ty = new.clone();
                m.tables[ti].cols[ci].default = None;
                format!(
                    "retype {}.{} {old} -> {new}",
                    m.tables[ti].name, m.tables[ti].cols[ci].name
                )
            }
            Edit::ReorderColumns => {
                let n = m.tables[ti].cols.len();
                m.tables[ti].cols[1..n].reverse();
                m.tables[ti].index = None;
                format!("reorder columns of {}", m.tables[ti].name)
            }
            Edit::FlipNullability => {
                let n = m.tables[ti].cols.len();
                let ci = 1 + r.below(n - 1);
                m.tables[ti].cols[ci].null ^= true;
                format!(
                    "flip nullability {}.{}",
                    m.tables[ti].name, m.tables[ti].cols[ci].name
                )
            }
            Edit::ChangeDefault => {
                let n = m.tables[ti].cols.len();
                let start = r.below(n - 1);
                let tname = m.tables[ti].name.clone();
                for k in 0..n - 1 {
                    let ci = 1 + (start + k) % (n - 1);
                    let c = &mut m.tables[ti].cols[ci];
                    let new = match &c.default {
                        Some(_) => Some(None),
                        None if c.ty.contains("char") => Some(Some("N'a'".to_string())),
                        None if c.ty.contains("int") => Some(Some("1".to_string())),
                        None => None,
                    };
                    if let Some(d) = new {
                        c.default = d;
                        return format!("change default {tname}.{}", c.name);
                    }
                }
                format!("change default of {tname} (no column takes one: no change)")
            }
            Edit::ToggleIndex => {
                let n = m.tables[ti].cols.len();
                m.tables[ti].index = match m.tables[ti].index {
                    Some(_) => None,
                    None => Some(1 + r.below(n - 1)),
                };
                format!("toggle index on {}", m.tables[ti].name)
            }
            Edit::ViewBody => {
                let vi = r.below(m.views.len());
                m.views[vi].star ^= true;
                format!("edit view {}", m.views[vi].name)
            }
            Edit::AlterSourceTable => {
                let over = m.views[r.below(m.views.len())].over.clone();
                m.extra.push((
                    "alters/a.sql".into(),
                    format!(
                        "ALTER TABLE dbo.{over} ADD Extra{} int NULL;\nGO\n",
                        r.below(9)
                    ),
                ));
                format!("ALTER TABLE {over} ADD in a file of its own")
            }
            Edit::AddRoutine => {
                let name = format!("Q{}", r.below(1000));
                m.routines.push(Routine {
                    name: name.clone(),
                    reads: m.tables[ti].name.clone(),
                    writes: None,
                    param: "int".into(),
                    function: false,
                });
                format!("add routine {name}")
            }
            Edit::DropRoutine => {
                let ri = r.below(m.routines.len());
                let g = m.routines.remove(ri);
                format!("drop routine {}", g.name)
            }
            Edit::ChangeParameterType => {
                let ri = r.below(m.routines.len());
                let new = if m.routines[ri].param == "int" {
                    "bigint"
                } else {
                    "int"
                };
                m.routines[ri].param = new.into();
                format!("parameter of {} -> {new}", m.routines[ri].name)
            }
            Edit::AddSchemaTable => {
                let schema = format!("s{}", r.below(50));
                m.tables.push(Table {
                    schema: schema.clone(),
                    name: "Other".into(),
                    cols: vec![Col {
                        name: "Id".into(),
                        ty: "int".into(),
                        null: false,
                        default: None,
                    }],
                    index: None,
                    versioned: false,
                });
                m.extra.push((
                    "schemas/s.sql".into(),
                    format!("CREATE SCHEMA {schema};\nGO\n"),
                ));
                format!("table {schema}.Other in a new schema")
            }
            Edit::AddTrigger => {
                let name = format!("Tg{}", r.below(1000));
                m.triggers.push(Trigger {
                    name: name.clone(),
                    on: m.tables[ti].name.clone(),
                });
                format!("add trigger {name} on {}", m.tables[ti].name)
            }
            Edit::DropTrigger => {
                if m.triggers.is_empty() {
                    return "drop trigger (none)".into();
                }
                let g = m.triggers.remove(0);
                format!("drop trigger {}", g.name)
            }
            Edit::VersioningOn => {
                m.tables[ti].versioned = true;
                format!("system versioning on for {}", m.tables[ti].name)
            }
            Edit::BaseColumn => {
                m.base[0].cols.push(Col {
                    name: "Extra".into(),
                    ty: "int".into(),
                    null: true,
                    default: None,
                });
                "base catalog: ext.Ref gains a column".into()
            }
            Edit::BaseTable => {
                m.base.push(Table {
                    schema: "ext".into(),
                    name: "More".into(),
                    cols: vec![Col {
                        name: "Id".into(),
                        ty: "int".into(),
                        null: false,
                        default: None,
                    }],
                    index: None,
                    versioned: false,
                });
                "base catalog: ext.More appears".into()
            }
            Edit::BaseDrop => {
                m.base.clear();
                "base catalog: ext.Ref disappears".into()
            }
        }
    }
}
