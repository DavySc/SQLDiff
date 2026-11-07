//! The digest of an answer covers every field of every catalog type, and a
//! read set has one canonical order (`record.rs`).
//!
//! Each type is destructured below without `..`: adding a field breaks this
//! file's build until the field has a line in the matching test, which has
//! to change the digest (a field the digest ignores would be a read the
//! check cannot see change).

use sqldiff_catalog::record::{Outcome, Query, ReadSink, Reads, Recorder, Side};
use sqldiff_catalog::types::DataType;
use sqldiff_catalog::{
    Catalog, Column, Index, Key, Param, ParamMode, Routine, RoutineKind, Table, TableKind, Trigger,
    Versioning,
};

type Mutation<T> = (&'static str, Box<dyn Fn(&mut T)>);

/// Every mutation must change the digest of the object it mutates.
fn all_change<T: std::fmt::Debug + Clone>(base: &T, mutations: Vec<Mutation<T>>) {
    let want = Outcome::of(base);
    assert_eq!(want, Outcome::of(&base.clone()), "the digest is stable");
    for (name, m) in mutations {
        let mut t = base.clone();
        m(&mut t);
        assert_ne!(want, Outcome::of(&t), "{name} is not in the digest");
    }
}

fn dt(base: &str) -> DataType {
    DataType {
        base: base.into(),
        args: vec![],
        max: false,
        text: base.into(),
    }
}

fn column() -> Column {
    Column {
        name: "a".into(),
        data_type: dt("int"),
        nullable: true,
        identity: false,
        has_default: false,
        generated: false,
        hidden: false,
        collation: None,
    }
}

#[test]
fn every_field_of_a_column_is_digested() {
    let Column {
        name: _,
        data_type: _,
        nullable: _,
        identity: _,
        has_default: _,
        generated: _,
        hidden: _,
        collation: _,
    } = column();
    all_change(
        &column(),
        vec![
            ("name", Box::new(|c| c.name.push('x'))),
            ("data_type", Box::new(|c| c.data_type = dt("bigint"))),
            ("nullable", Box::new(|c| c.nullable = false)),
            ("identity", Box::new(|c| c.identity = true)),
            ("has_default", Box::new(|c| c.has_default = true)),
            ("generated", Box::new(|c| c.generated = true)),
            ("hidden", Box::new(|c| c.hidden = true)),
            ("collation", Box::new(|c| c.collation = Some("c".into()))),
        ],
    );
}

#[test]
fn every_field_of_a_data_type_is_digested() {
    let DataType {
        base: _,
        args: _,
        max: _,
        text: _,
    } = dt("int");
    all_change(
        &dt("int"),
        vec![
            ("base", Box::new(|t| t.base.push('x'))),
            ("args", Box::new(|t| t.args.push(1))),
            ("max", Box::new(|t| t.max = true)),
            ("text", Box::new(|t| t.text.push('x'))),
        ],
    );
}

fn table() -> Table {
    let mut t = Table::new(Some("dbo".into()), "t", TableKind::Table);
    t.columns.push(column());
    t
}

#[test]
fn every_field_of_a_table_key_index_and_versioning_is_digested() {
    let Table {
        schema: _,
        name: _,
        kind: _,
        columns: _,
        columns_known: _,
        star_outputs: _,
        types_from_source: _,
        source_table: _,
        source_columns: _,
        keys: _,
        indexes: _,
        triggers: _,
        versioning: _,
    } = table();
    all_change(
        &table(),
        vec![
            ("schema", Box::new(|t| t.schema = None)),
            ("name", Box::new(|t| t.name.push('x'))),
            ("kind", Box::new(|t| t.kind = TableKind::View)),
            ("columns", Box::new(|t| t.columns.push(column()))),
            ("columns_known", Box::new(|t| t.columns_known = false)),
            ("star_outputs", Box::new(|t| t.star_outputs = true)),
            (
                "types_from_source",
                Box::new(|t| t.types_from_source = true),
            ),
            ("source_table", Box::new(|t| t.source_table = "s".into())),
            (
                "source_columns",
                Box::new(|t| t.source_columns.push(Some("a".into()))),
            ),
            (
                "keys",
                Box::new(|t| {
                    t.keys.push(Key {
                        name: None,
                        primary: true,
                        columns: vec!["a".into()],
                    })
                }),
            ),
            (
                "indexes",
                Box::new(|t| {
                    t.indexes.push(Index {
                        name: "i".into(),
                        unique: false,
                        columns: vec![],
                    })
                }),
            ),
            ("triggers", Box::new(|t| t.triggers.push("tr".into()))),
            (
                "versioning",
                Box::new(|t| {
                    t.versioning = Some(Versioning {
                        start: "s".into(),
                        end: "e".into(),
                        history: None,
                    })
                }),
            ),
        ],
    );
    let key = || Key {
        name: Some("pk".into()),
        primary: true,
        columns: vec!["a".into()],
    };
    let Key {
        name: _,
        primary: _,
        columns: _,
    } = key();
    all_change(
        &key(),
        vec![
            ("key.name", Box::new(|k| k.name = None)),
            ("key.primary", Box::new(|k| k.primary = false)),
            ("key.columns", Box::new(|k| k.columns.push("b".into()))),
        ],
    );
    let index = || Index {
        name: "i".into(),
        unique: false,
        columns: vec!["a".into()],
    };
    let Index {
        name: _,
        unique: _,
        columns: _,
    } = index();
    all_change(
        &index(),
        vec![
            ("index.name", Box::new(|i| i.name.push('x'))),
            ("index.unique", Box::new(|i| i.unique = true)),
            ("index.columns", Box::new(|i| i.columns.push("b".into()))),
        ],
    );
    let versioning = || Versioning {
        start: "s".into(),
        end: "e".into(),
        history: Some(vec!["h".into()]),
    };
    let Versioning {
        start: _,
        end: _,
        history: _,
    } = versioning();
    all_change(
        &versioning(),
        vec![
            ("versioning.start", Box::new(|v| v.start.push('x'))),
            ("versioning.end", Box::new(|v| v.end.push('x'))),
            ("versioning.history", Box::new(|v| v.history = None)),
        ],
    );
}

fn routine() -> Routine {
    Routine {
        schema: Some("dbo".into()),
        name: "p".into(),
        kind: RoutineKind::Procedure,
        params: vec![Param {
            name: "@a".into(),
            data_type: dt("int"),
            mode: ParamMode::In,
            has_default: false,
            default: None,
        }],
        returns: None,
        returns_status: false,
    }
}

#[test]
fn every_field_of_a_routine_and_its_parameters_is_digested() {
    let Routine {
        schema: _,
        name: _,
        kind: _,
        params: _,
        returns: _,
        returns_status: _,
    } = routine();
    all_change(
        &routine(),
        vec![
            ("schema", Box::new(|r| r.schema = None)),
            ("name", Box::new(|r| r.name.push('x'))),
            ("kind", Box::new(|r| r.kind = RoutineKind::Function)),
            ("params", Box::new(|r| r.params.clear())),
            ("returns", Box::new(|r| r.returns = Some(dt("int")))),
            ("returns_status", Box::new(|r| r.returns_status = true)),
        ],
    );
    let param = || routine().params.remove(0);
    let Param {
        name: _,
        data_type: _,
        mode: _,
        has_default: _,
        default: _,
    } = param();
    all_change(
        &param(),
        vec![
            ("param.name", Box::new(|p| p.name.push('x'))),
            ("param.data_type", Box::new(|p| p.data_type = dt("text"))),
            ("param.mode", Box::new(|p| p.mode = ParamMode::Out)),
            ("param.has_default", Box::new(|p| p.has_default = true)),
            ("param.default", Box::new(|p| p.default = Some("1".into()))),
        ],
    );
}

#[test]
fn every_field_of_a_trigger_and_a_catalog_is_digested() {
    let trigger = || Trigger {
        schema: Some("dbo".into()),
        name: "tr".into(),
        table: vec!["dbo".into(), "t".into()],
        events: vec!["INSERT".into()],
        instead: false,
        function: None,
    };
    let Trigger {
        schema: _,
        name: _,
        table: _,
        events: _,
        instead: _,
        function: _,
    } = trigger();
    all_change(
        &trigger(),
        vec![
            ("trigger.schema", Box::new(|t| t.schema = None)),
            ("trigger.name", Box::new(|t| t.name.push('x'))),
            ("trigger.table", Box::new(|t| t.table.push("u".into()))),
            (
                "trigger.events",
                Box::new(|t| t.events.push("UPDATE".into())),
            ),
            ("trigger.instead", Box::new(|t| t.instead = true)),
            (
                "trigger.function",
                Box::new(|t| t.function = Some(vec!["f".into()])),
            ),
        ],
    );
    let catalog = || Catalog {
        origin: "o".into(),
        tables: vec![table()],
        routines: vec![routine()],
        schemas: vec!["s".into()],
        triggers: vec![trigger()],
    };
    let Catalog {
        origin: _,
        tables: _,
        routines: _,
        schemas: _,
        triggers: _,
    } = catalog();
    all_change(
        &catalog(),
        vec![
            ("origin", Box::new(|c| c.origin.push('x'))),
            ("tables", Box::new(|c| c.tables.clear())),
            ("routines", Box::new(|c| c.routines.clear())),
            ("schemas", Box::new(|c| c.schemas.clear())),
            ("triggers", Box::new(|c| c.triggers.clear())),
            // Order is part of the answer (overloads, duplicates, last wins).
            (
                "table order",
                Box::new(|c| {
                    let mut second = table();
                    second.name = "u".into();
                    c.tables.push(second);
                    c.tables.reverse();
                }),
            ),
        ],
    );
}

#[test]
fn a_read_set_is_sorted_and_deduplicated_whatever_the_order_of_the_reads() {
    let reads = |order: &[usize]| {
        let r = Recorder::default();
        let qs = [
            (Side::Target, Query::Origin),
            (Side::Source, Query::Table(vec![("t".into(), false)])),
            (Side::Source, Query::Supplied),
            (Side::Source, Query::Table(vec![("t".into(), true)])),
        ];
        for &i in order {
            r.record(qs[i].0, &qs[i].1, Outcome::of(&i));
        }
        r.finish()
    };
    let a = reads(&[0, 1, 2, 3, 1]);
    let b = reads(&[3, 2, 1, 0]);
    assert_eq!(a.0, b.0);
    assert!(a.1.is_empty());
    assert_eq!(a.0.canonical(), b.0.canonical());
    // A quoted and an unquoted part are different questions.
    assert_eq!(a.0 .0.len(), 4);
    // Source reads sort before Target ones.
    let first = a.0 .0.keys().next().unwrap();
    assert_eq!(first.0, Side::Source);
}

#[test]
fn two_answers_to_one_question_are_flagged() {
    let r = Recorder::default();
    r.record(Side::Source, &Query::Supplied, Outcome::of(&1));
    r.record(Side::Source, &Query::Supplied, Outcome::of(&2));
    let (reads, conflicts): (Reads, _) = r.finish();
    assert_eq!(reads.0.len(), 1);
    assert_eq!(conflicts, vec![(Side::Source, Query::Supplied)]);
}
