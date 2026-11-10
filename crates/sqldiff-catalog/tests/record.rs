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

// ---- RecordingView -------------------------------------------------------

mod recording_view {
    use super::*;
    use sqldiff_catalog::record::{NoSink, RecordingView};
    use sqldiff_catalog::{ddl, CatalogView, Ident, Lookup};
    use sqldiff_syntax::Dialect;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn view() -> CatalogView {
        let cat = ddl::load(
            "CREATE TABLE dbo.orders (id int NOT NULL, note varchar(10));",
            Dialect::Tsql,
            "t.sql",
        );
        CatalogView::new(Dialect::Tsql, &[&cat])
    }

    fn ident(t: &str, quoted: bool) -> Ident {
        Ident::new(t, quoted)
    }

    #[test]
    fn every_question_is_recorded_as_posed_with_the_digest_of_its_answer() {
        let v = view();
        let rec = Recorder::default();
        let rv = RecordingView::new(&v, Side::Target, &rec);
        let name = [ident("dbo", false), ident("Orders", true)];
        let found = rv.table(&name);
        assert!(matches!(found, Lookup::Found(_)));
        let found_digest = Outcome::of(&found);
        assert!(rv.supplied());
        assert!(rv.tables_authoritative());
        let _ = rv.origin();
        let _ = rv.routines(&[ident("f", false)]);
        let _ = rv.whole();
        let (reads, conflicts) = rec.finish();
        assert!(conflicts.is_empty());
        let key = |q: Query| (Side::Target, q);
        // The question keeps its text and its quoting.
        let q = Query::Table(vec![("dbo".into(), false), ("Orders".into(), true)]);
        assert_eq!(reads.0[&key(q)], found_digest);
        for q in [
            Query::Supplied,
            Query::Origin,
            Query::TablesAuthoritative,
            Query::Routines(vec![("f".into(), false)]),
            Query::WholeCatalog,
        ] {
            assert!(reads.0.contains_key(&key(q.clone())), "{q:?} not recorded");
        }
        assert_eq!(reads.0.len(), 6);
        // A different question about the same table is a different read.
        let other = Query::Table(vec![("dbo".into(), false), ("Orders".into(), false)]);
        assert!(!reads.0.contains_key(&key(other)));
    }

    #[test]
    fn the_answer_digest_follows_the_catalog() {
        let digest = |ddl_text: &str| {
            let cat = ddl::load(ddl_text, Dialect::Tsql, "t.sql");
            let v = CatalogView::new(Dialect::Tsql, &[&cat]);
            let rec = Recorder::default();
            let rv = RecordingView::new(&v, Side::Source, &rec);
            let _ = rv.table(&[ident("dbo", false), ident("orders", false)]);
            let (reads, _) = rec.finish();
            reads.0.into_values().next().unwrap()
        };
        let a = digest("CREATE TABLE dbo.orders (id int NOT NULL);");
        assert_eq!(a, digest("CREATE TABLE dbo.orders (id int NOT NULL);"));
        assert_ne!(a, digest("CREATE TABLE dbo.orders (id bigint NOT NULL);"));
    }

    #[test]
    fn a_sink_that_keeps_nothing_costs_no_digest() {
        struct Counting(AtomicUsize);
        impl ReadSink for Counting {
            fn record(&self, _: Side, _: &Query, _: Outcome) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
            fn active(&self) -> bool {
                false
            }
        }
        let v = view();
        let s = Counting(AtomicUsize::new(0));
        let rv = RecordingView::new(&v, Side::Source, &s);
        let _ = rv.table(&[ident("orders", false)]);
        let _ = rv.whole();
        assert_eq!(s.0.load(Ordering::SeqCst), 0);
        let none = RecordingView::new(&v, Side::Source, &NoSink);
        assert!(none.supplied());
    }
}

// ---- SourceReads (pass 1) ------------------------------------------------

mod source_reads {
    use super::*;
    use sqldiff_catalog::record::{RecordingView, SourceDigests, SourceReads};
    use sqldiff_catalog::{ddl, CatalogView, Ident};
    use sqldiff_syntax::Dialect;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn catalog(sql: &str) -> Catalog {
        ddl::load(sql, Dialect::Tsql, "t.sql")
    }

    const SQL: &str = "CREATE TABLE dbo.a (id int NOT NULL, n varchar(10));\
                       CREATE TABLE dbo.b (id int NOT NULL, w decimal(9));";

    fn name(t: &str) -> [Ident; 2] {
        [Ident::new("dbo", false), Ident::new(t, false)]
    }

    #[test]
    fn reads_are_filed_under_the_draft_side_and_replay_asks_them_again() {
        let cat = catalog(SQL);
        let digests = SourceDigests::new(&cat);
        let first = Recorder::default();
        let r = SourceReads::recording(&cat, Dialect::Tsql, &first, &digests);
        r.table(&name("a"));
        r.table(&name("zzz"));
        r.column_types();
        r.tables();
        let (reads, conflicts) = first.finish();
        assert!(conflicts.is_empty());
        assert!(reads.0.keys().all(|(side, _)| *side == Side::Draft));
        assert_eq!(reads.0.len(), 4);
        // Replay through the same methods gives the same set, answer for answer.
        let second = Recorder::default();
        let digests2 = SourceDigests::new(&cat);
        let r2 = SourceReads::recording(&cat, Dialect::Tsql, &second, &digests2);
        for (_, q) in reads.0.keys() {
            assert!(r2.ask(q), "{q:?}");
        }
        assert_eq!(second.finish().0, reads);
    }

    #[test]
    fn the_answer_follows_the_table_and_not_other_tables() {
        let digest_of = |sql: &str, t: &str| {
            let cat = catalog(sql);
            let d = SourceDigests::new(&cat);
            let rec = Recorder::default();
            SourceReads::recording(&cat, Dialect::Tsql, &rec, &d).table(&name(t));
            let (reads, _) = rec.finish();
            *reads.0.values().next().unwrap()
        };
        let base = digest_of(SQL, "a");
        // Another table's change does not move the answer for `a`.
        assert_eq!(base, digest_of(&SQL.replace("w decimal(9)", "w int"), "a"));
        // A column of `a` does.
        assert_ne!(
            base,
            digest_of(&SQL.replace("n varchar(10)", "n varchar(11)"), "a")
        );
        // A missing table is a different answer from a present one.
        assert_ne!(base, digest_of(SQL, "zzz"));
    }

    #[test]
    fn column_types_change_only_with_a_type_shape() {
        let digest = |sql: &str| {
            let cat = catalog(sql);
            let d = SourceDigests::new(&cat);
            let rec = Recorder::default();
            let r = SourceReads::recording(&cat, Dialect::Tsql, &rec, &d);
            let types = r.column_types().into_owned();
            let (reads, _) = rec.finish();
            assert_eq!(reads.0.len(), 1, "one narrow query, not the whole catalog");
            assert!(reads.0.contains_key(&(Side::Draft, Query::ColumnTypes)));
            (types, *reads.0.values().next().unwrap())
        };
        let (t0, d0) = digest(SQL);
        assert!(t0.windows(2).all(|w| w[0] < w[1]), "sorted, distinct");
        // A renamed column or a new table of an existing shape: same answer.
        let (_, d1) = digest(&SQL.replace("n varchar(10)", "renamed varchar(10)"));
        assert_eq!(d0, d1);
        let (_, d2) = digest(&format!("{SQL}CREATE TABLE dbo.c (id int NOT NULL);"));
        assert_eq!(d0, d2);
        // A new shape (or a changed argument) moves it.
        let (_, d3) = digest(&SQL.replace("w decimal(9)", "w bit"));
        assert_ne!(d0, d3);
        let (_, d4) = digest(&SQL.replace("varchar(10)", "varchar(11)"));
        assert_ne!(d0, d4);
    }

    #[test]
    fn the_same_question_is_digested_once_and_the_cache_is_shared() {
        struct Count(AtomicUsize, Recorder);
        impl ReadSink for Count {
            fn record(&self, s: Side, q: &Query, o: Outcome) {
                self.0.fetch_add(1, Ordering::SeqCst);
                self.1.record(s, q, o);
            }
            fn holds(&self, s: Side, q: &Query) -> bool {
                self.1.holds(s, q)
            }
        }
        let cat = catalog(SQL);
        let d = SourceDigests::new(&cat);
        let sink = Count(AtomicUsize::new(0), Recorder::default());
        let r = SourceReads::recording(&cat, Dialect::Tsql, &sink, &d);
        for _ in 0..50 {
            r.table(&name("a"));
            r.tables();
        }
        assert_eq!(sink.0.load(Ordering::SeqCst), 2, "each question once");
    }

    #[test]
    fn a_sink_that_keeps_nothing_costs_no_record() {
        let cat = catalog(SQL);
        let r = SourceReads::new(&cat, Dialect::Tsql);
        assert!(r.table(&name("a")).is_some());
        assert_eq!(r.column_types().len(), 3);
    }

    #[test]
    fn a_pass_one_read_is_never_replayed_through_a_pass_two_view_or_back() {
        let cat = catalog(SQL);
        let view = CatalogView::new(Dialect::Tsql, &[&cat]);
        let rec = Recorder::default();
        let draft_view = RecordingView::new(&view, Side::Draft, &rec);
        assert!(!draft_view.ask(&Query::Supplied));
        assert!(!draft_view.ask(&Query::Table(vec![("a".into(), false)])));
        // The other direction: a query pass 1 never asks is refused.
        let r = SourceReads::new(&cat, Dialect::Tsql);
        assert!(!r.ask(&Query::Supplied));
        assert!(!r.ask(&Query::Origin));
        assert!(!r.ask(&Query::Routines(vec![("p".into(), false)])));
        assert!(!r.ask(&Query::SchemaTables("a".into())));
        assert!(rec.finish().0 .0.is_empty());
    }

    #[test]
    #[should_panic(expected = "not built for")]
    fn digests_paired_with_another_catalog_are_refused() {
        let a = catalog(SQL);
        let b = catalog(SQL);
        let d = SourceDigests::new(&a);
        let rec = Recorder::default();
        let _ = SourceReads::recording(&b, Dialect::Tsql, &rec, &d);
    }
}
