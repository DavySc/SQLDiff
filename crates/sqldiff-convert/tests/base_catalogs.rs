//! Base catalogs (`sqldiff convert --catalog FILE`): DDL of objects that exist
//! already but are not part of the tree. They bind on both sides, the tree's
//! own objects win, they are converted for the target catalog only and never
//! written or counted, and what uses a base object that does not convert
//! carries `ASM-BASE-SUSPECT`. With the target's own base DDL (`--catalog-b`), the
//! target side binds against that as given instead.

use std::path::PathBuf;

use sqldiff_convert::base;
use sqldiff_convert::gate;
use sqldiff_convert::job::{convert_into, Job};
use sqldiff_convert::review::{append, file_hash, By, Record, State, CATALOGS_CHANGED};
use sqldiff_convert::tree::{self, Hooks, TemplateMode, Tree};
use sqldiff_convert::Status;
use sqldiff_core::catalog::{Catalog, Ident};
use sqldiff_core::Dialect;

const ORDERS: &str = "CREATE TABLE orders (order_id int NOT NULL PRIMARY KEY, status varchar(10) NOT NULL);\nGO\nCREATE TABLE order_items (order_id int NOT NULL, line_no int NOT NULL, qty int NOT NULL);\nGO\n";
const AUDIT: (&str, &str) = (
    "src/tables/audit_log.sql",
    "CREATE TABLE audit_log (id int NOT NULL);\n",
);
const NEXT_LINE: &str = "CREATE PROCEDURE next_line @order_id int, @line_no int OUTPUT AS\nBEGIN\n    SELECT @line_no = COUNT(*) + 1 FROM order_items WHERE order_id = @order_id;\nEND\nGO\n";

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str, files: &[(&str, &str)]) -> Fixture {
        let dir = std::env::temp_dir().join(format!("sqldiff-base-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fx = Fixture { dir };
        for (p, text) in files {
            fx.write(p, text);
        }
        fx
    }

    fn write(&self, rel: &str, text: &str) {
        let f = self.dir.join(rel);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, text).unwrap();
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    fn args(&self, catalogs: &[&str]) -> tree::Args {
        tree::Args {
            src: self.dir.join("src"),
            from: Dialect::Tsql,
            to: Dialect::Postgres,
            jobs: Some(2),
            no_catalog: false,
            template: None,
            extensions: tree::extensions(Vec::new()),
            helpers: Vec::new(),
            catalogs: catalogs.iter().map(|c| self.path(c)).collect(),
            catalogs_b: Vec::new(),
        }
    }

    fn args_b(&self, catalogs: &[&str], b: &[&str]) -> tree::Args {
        let mut a = self.args(catalogs);
        a.catalogs_b = b.iter().map(|c| self.path(c)).collect();
        a
    }

    fn convert(&self, catalogs: &[&str]) -> Tree {
        let a = self.args(catalogs);
        let (inputs, unreadable) = tree::read(&a).unwrap();
        tree::convert(&a, &inputs, unreadable, None).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The units of `path` with their rules (worst notes and findings).
fn unit_rules(t: &Tree, path: &str) -> Vec<(Status, Vec<String>)> {
    let k = t
        .report
        .files
        .iter()
        .position(|f| f.path == path)
        .unwrap_or_else(|| panic!("{path} in the report"));
    let f = &t.report.files[k];
    f.units
        .iter()
        .chain(&f.converted_units)
        .map(|u| {
            let mut r = u.rules.clone();
            r.extend(u.findings.iter().cloned());
            (u.status, r)
        })
        .collect()
}

fn status(t: &Tree, path: &str) -> Status {
    t.report
        .files
        .iter()
        .find(|f| f.path == path)
        .unwrap()
        .status
}

fn has(t: &Tree, path: &str, rule: &str) -> bool {
    unit_rules(t, path)
        .iter()
        .any(|(_, r)| r.iter().any(|x| x == rule))
}

#[test]
fn a_table_only_in_a_catalog_binds_with_it() {
    let fx = Fixture::new(
        "binds",
        &[
            ("base/orders.sql", ORDERS),
            (
                "src/procs/update_item.sql",
                "UPDATE orders SET status = 'open' WHERE order_id = 1;\n",
            ),
            // The upgrade's own table: the catalogs describe tables, so a
            // table missing from them does not exist.
            (AUDIT.0, AUDIT.1),
        ],
    );
    let without = fx.convert(&[]);
    assert_eq!(status(&without, "procs/update_item.sql"), Status::Differs);
    assert!(has(&without, "procs/update_item.sql", "bind.unknown-table"));

    let with = fx.convert(&["base/orders.sql"]);
    let s = status(&with, "procs/update_item.sql");
    assert!(
        s <= Status::Assumptions,
        "{:?}",
        unit_rules(&with, "procs/update_item.sql")
    );
    assert!(!has(&with, "procs/update_item.sql", "bind.unknown-table"));
    // The report says what it bound against.
    let c = &with.report.base_catalogs;
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].path, fx.path("base/orders.sql").display().to_string());
    assert_eq!(c[0].sha256, file_hash(ORDERS));
    assert_eq!((c[0].tables, c[0].routines), (2, 0));
    assert!(with.report.base_hash.is_some());
    assert!(without.report.base_hash.is_none() && without.report.base_catalogs.is_empty());
    let md = tree::markdown(&with.report);
    assert!(md.contains("## Base catalogs"), "{md}");
    assert!(md.contains(&file_hash(ORDERS)), "{md}");
    let json = serde_json::to_value(&with.report).unwrap();
    assert_eq!(json["base_catalogs"][0]["sha256"], file_hash(ORDERS));
    // A report without base catalogs reads as before.
    let json = serde_json::to_value(&without.report).unwrap();
    assert!(json.get("base_catalogs").is_none() && json.get("base_hash").is_none());
}

#[test]
fn base_ddl_is_not_written_listed_or_counted() {
    let fx = Fixture::new(
        "written",
        &[
            ("base/orders.sql", ORDERS),
            (
                "src/q.sql",
                "SELECT status FROM orders WHERE order_id = 1;\n",
            ),
        ],
    );
    let job = Job {
        tree: fx.args(&["base/orders.sql"]),
        out: fx.path("out"),
        overwrite: false,
        discard_edits: false,
        instances: None,
        queue_all: false,
    };
    let w = convert_into(&job, &Hooks::default()).unwrap();
    let mut written: Vec<String> = std::fs::read_dir(fx.path("out"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    written.sort();
    assert_eq!(
        written,
        [
            "conversion-report.json",
            "conversion-report.md",
            "q.sql",
            "review-queue.json",
            "review-queue.md"
        ]
    );
    let r = &w.tree.report;
    let paths: Vec<&str> = r.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["q.sql"]);
    assert_eq!(r.totals.files, 1);
    let units: usize = r.totals.units.values().sum();
    let c = &r.files[0].counts;
    let own = c.converted + c.assumptions + c.decision + c.unproven + c.differs + c.not_converted;
    assert_eq!(units, own);
    assert_eq!(units, 1);
    // The report on disk says it too.
    let json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fx.path("out/conversion-report.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(json["files"].as_array().unwrap().len(), 1);
    assert_eq!(json["base_catalogs"].as_array().unwrap().len(), 1);
}

#[test]
fn the_tree_wins_over_a_catalog() {
    // The catalog's orders has no region; the tree's own orders has it and
    // no status.
    let fx = Fixture::new(
        "wins",
        &[
            ("base/orders.sql", ORDERS),
            (
                "src/tables/orders.sql",
                "CREATE TABLE orders (order_id int NOT NULL PRIMARY KEY, region int NOT NULL);\n",
            ),
            (
                "src/q1.sql",
                "SELECT region FROM orders WHERE order_id = 1;\n",
            ),
            (
                "src/q2.sql",
                "SELECT status FROM orders WHERE order_id = 1;\n",
            ),
            // A table only the catalog has still binds.
            (
                "src/q3.sql",
                "SELECT qty FROM order_items WHERE order_id = 1;\n",
            ),
        ],
    );
    let t = fx.convert(&["base/orders.sql"]);
    assert!(
        !has(&t, "q1.sql", "bind.unknown-column"),
        "{:?}",
        unit_rules(&t, "q1.sql")
    );
    assert!(
        has(&t, "q2.sql", "bind.unknown-column"),
        "{:?}",
        unit_rules(&t, "q2.sql")
    );
    assert_eq!(status(&t, "q2.sql"), Status::Differs);
    assert!(
        status(&t, "q3.sql") <= Status::Assumptions,
        "{:?}",
        unit_rules(&t, "q3.sql")
    );
}

#[test]
fn a_tree_object_replaces_every_base_object_of_its_name() {
    let d = Dialect::Tsql;
    let mut base = sqldiff_core::catalog::ddl::load(
        "CREATE TABLE dbo.orders (order_id int, status int);\nCREATE PROCEDURE dbo.p @a int AS SELECT 1;\nGO\nCREATE PROCEDURE q @a int AS SELECT 1;\n",
        d,
        "base",
    );
    base.origin = "base".into();
    // The tree's `orders` (no schema) is dbo's; its `p` has another
    // parameter count, so the base `p` would otherwise stay as an overload.
    let tree_ddl = "CREATE TABLE orders (order_id int, region int);\nGO\nCREATE PROCEDURE p @a int, @b int AS SELECT 1;\nGO\nALTER TABLE q_t ADD x int;\n";
    let cat: Catalog = base::over(&base, [tree_ddl].into_iter(), d, "tree".into(), None);
    let orders: Vec<_> = cat
        .tables
        .iter()
        .filter(|t| t.name.eq_ignore_ascii_case("orders"))
        .collect();
    assert_eq!(orders.len(), 1, "{:#?}", cat.tables);
    let cols: Vec<&str> = orders[0].columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(cols, ["order_id", "region"]);
    let p: Vec<_> = cat.routines.iter().filter(|r| r.name == "p").collect();
    assert_eq!(p.len(), 1, "{:#?}", cat.routines);
    assert_eq!(p[0].params.len(), 2);
    // Objects the tree does not define stay.
    assert!(cat.routines.iter().any(|r| r.name == "q"));
    assert_eq!(cat.origin, "tree");

    // A statement in the tree that alters a base table changes it.
    let cat = base::over(
        &base,
        ["ALTER TABLE dbo.orders ADD note varchar(10);\n"].into_iter(),
        d,
        "tree".into(),
        None,
    );
    let view = sqldiff_core::catalog::CatalogView::new(d, &[&cat]);
    let sqldiff_core::catalog::Lookup::Found(t) = view.table(&[Ident::plain("orders")]) else {
        panic!("orders")
    };
    assert!(t.columns.iter().any(|c| c.name == "note"), "{t:#?}");
}

#[test]
fn a_base_object_that_does_not_convert_flags_its_users() {
    let fx = Fixture::new(
        "unconverted",
        &[
            // No rule converts geography: the table is refused.
            (
                "base/install.sql",
                "CREATE TABLE orders (order_id int NOT NULL PRIMARY KEY, site geography NULL);\nGO\nCREATE VIEW open_orders AS SELECT order_id FROM orders;\nGO\nCREATE TABLE order_items (order_id int NOT NULL, qty int NOT NULL);\nGO\n",
            ),
            // datetime2 keeps 7 fraction digits, PostgreSQL's timestamp 6: the
            // table is converted but differs. The view over it converts, and
            // is suspect through the table.
            (
                "base/stamps.sql",
                "CREATE TABLE stamps (stamp_id int NOT NULL PRIMARY KEY, placed datetime2 NOT NULL);\nGO\n",
            ),
            (
                "base/views.sql",
                "CREATE VIEW late_orders AS SELECT order_id FROM open_orders;\nGO\nCREATE VIEW recent_stamps AS SELECT stamp_id FROM stamps;\nGO\n",
            ),
            ("src/q1.sql", "SELECT order_id FROM orders;\n"),
            ("src/q2.sql", "SELECT order_id FROM late_orders;\n"),
            ("src/q3.sql", "SELECT qty FROM order_items;\n"),
            ("src/q4.sql", "SELECT stamp_id FROM recent_stamps;\n"),
            // A tree view over a suspect object is suspect, and so is what
            // uses it, in another file.
            (
                "src/views/open_items.sql",
                "CREATE VIEW open_items AS SELECT i.qty FROM order_items AS i JOIN open_orders AS o ON o.order_id = i.order_id;\n",
            ),
            ("src/q5.sql", "SELECT qty FROM open_items;\n"),
            (AUDIT.0, AUDIT.1),
        ],
    );
    let t = fx.convert(&["base/install.sql", "base/stamps.sql", "base/views.sql"]);
    for q in [
        "q1.sql",
        "q2.sql",
        "q4.sql",
        "views/open_items.sql",
        "q5.sql",
    ] {
        assert!(
            has(&t, q, base::BASE_SUSPECT),
            "{q}: {:?}",
            unit_rules(&t, q)
        );
        assert!(status(&t, q) >= Status::Assumptions);
        // The suspect objects are in the target catalog.
        assert!(
            !has(&t, q, "bind.unknown-table"),
            "{q}: {:?}",
            unit_rules(&t, q)
        );
    }
    assert!(
        !has(&t, "q3.sql", base::BASE_SUSPECT),
        "{:?}",
        unit_rules(&t, "q3.sql")
    );
    assert!(!has(&t, "q3.sql", "bind.unknown-table"));
    // The note names the object and why.
    let k = t
        .report
        .files
        .iter()
        .position(|f| f.path == "q5.sql")
        .unwrap();
    let note = t.conversions[k]
        .units
        .iter()
        .flat_map(|u| &u.notes)
        .find(|n| n.rule == base::BASE_SUSPECT)
        .unwrap();
    assert!(note.text.contains("view `open_items`"), "{}", note.text);
    assert!(note.text.contains("uses `open_orders`"), "{}", note.text);
    let suspect: Vec<(&str, &str, Status)> = t
        .report
        .base_objects
        .iter()
        .filter(|o| o.suspect)
        .map(|o| (o.kind.as_str(), o.name.as_str(), o.status))
        .collect();
    assert_eq!(
        suspect,
        [
            ("table", "orders", Status::NotConverted),
            ("view", "open_orders", Status::NotConverted),
            ("table", "stamps", Status::Differs),
            ("view", "late_orders", Status::NotConverted),
            ("view", "recent_stamps", Status::Differs),
        ],
        "{:#?}",
        t.report.base_objects
    );
    let orders = &t.report.base_objects[0];
    assert_eq!(orders.line, 1);
    assert!(orders.file.ends_with("install.sql"));
    assert!(orders.reason.contains("GEOGRAPHY"), "{orders:?}");
    let recent = t
        .report
        .base_objects
        .iter()
        .find(|o| o.name == "recent_stamps")
        .unwrap();
    assert!(recent.reason.starts_with("Uses `stamps`"), "{recent:?}");
    assert!(t
        .report
        .base_objects
        .iter()
        .all(|o| o.name != "order_items" || !o.suspect));
    let md = tree::markdown(&t.report);
    assert!(md.contains("### Suspect base objects"), "{md}");
    assert!(md.contains("table `orders`"), "{md}");
    assert!(tree::summary(&t.report).contains("5 suspect base object(s)"));
}

#[test]
fn a_tree_object_of_the_same_name_is_not_suspect() {
    let fx = Fixture::new(
        "suspect-replaced",
        &[
            (
                "base/stamps.sql",
                "CREATE TABLE stamps (stamp_id int NOT NULL PRIMARY KEY, placed datetime2 NOT NULL);\nGO\n",
            ),
            (
                "src/tables/stamps.sql",
                "CREATE TABLE stamps (stamp_id int NOT NULL PRIMARY KEY, placed datetime NOT NULL);\n",
            ),
            ("src/q1.sql", "SELECT stamp_id FROM stamps;\n"),
        ],
    );
    let t = fx.convert(&["base/stamps.sql"]);
    assert!(t.report.base_objects.iter().any(|o| o.suspect));
    assert!(
        !has(&t, "q1.sql", base::BASE_SUSPECT),
        "{:?}",
        unit_rules(&t, "q1.sql")
    );
}

#[test]
fn a_catalog_only_procedure_changes_the_rules_and_the_ledger_follows_its_contents() {
    let fx = Fixture::new(
        "exec",
        &[
            ("base/orders.sql", ORDERS),
            ("base/procs.sql", NEXT_LINE),
            (
                "src/procs/add_item.sql",
                "CREATE PROCEDURE add_item @order_id int AS\nBEGIN\n    DECLARE @line int;\n    EXEC next_line @order_id, @line OUTPUT;\n    INSERT INTO order_items (order_id, line_no, qty) VALUES (@order_id, @line, 1);\nEND\n",
            ),
        ],
    );
    let output = |t: &Tree| t.outputs[0].clone();
    // Without the catalog the OUTPUT call has no callee to convert to.
    let without = fx.convert(&[]);
    assert!(
        !output(&without).contains("CALL next_line(p_order_id, v_line)"),
        "{}",
        output(&without)
    );
    let with = fx.convert(&["base/orders.sql", "base/procs.sql"]);
    assert!(
        output(&with).contains("CALL next_line(p_order_id, v_line)"),
        "{}",
        output(&with)
    );

    // A decision recorded on this conversion applies while the catalogs read
    // the same, and is outdated once their contents change.
    let a = fx.args(&["base/orders.sql", "base/procs.sql"]);
    let out = fx.path("out");
    let job = |a: tree::Args| Job {
        tree: a,
        out: out.clone(),
        overwrite: true,
        discard_edits: false,
        instances: None,
        queue_all: false,
    };
    let w = convert_into(
        &job(fx.args(&["base/orders.sql", "base/procs.sql"])),
        &Hooks::default(),
    )
    .unwrap();
    let f = &w.tree.report.files[0];
    // The INSERT: its text is the same with or without the catalogs.
    let u = f
        .units
        .iter()
        .chain(&f.converted_units)
        .find(|u| u.src_line == 5)
        .unwrap();
    append(
        &out,
        &Record {
            unit: u.id.clone(),
            path: f.path.clone(),
            rule: String::new(),
            state: State::Approved,
            by: By::Human,
            author: "Davy".into(),
            reason: "read it".into(),
            at: "2026-10-28T12:00:00Z".into(),
            input_hash: file_hash(&w.inputs[0].text),
            output_hash: file_hash(&w.tree.conversions[0].output),
            text: None,
            base_catalogs: w.tree.report.base_hash.clone(),
        },
    )
    .unwrap();
    let decision = |a: &tree::Args| {
        let (inputs, unreadable) = tree::read(a).unwrap();
        let mut t = tree::convert(a, &inputs, unreadable, Some(&out)).unwrap();
        tree::review_in(&mut t, &inputs, &out).unwrap();
        let g = gate::check(&t, a, &out, None).unwrap();
        let f = &t.report.files[0];
        let x = f
            .units
            .iter()
            .chain(&f.converted_units)
            .find(|x| x.id == u.id)
            .unwrap_or_else(|| panic!("{} in {}", u.id, serde_json::to_string_pretty(f).unwrap()));
        let d = x.review.as_ref().unwrap().decisions[0].clone();
        (d, g)
    };
    let (d, g) = decision(&a);
    assert!(d.current, "{d:?}");
    assert!(!g.counts.contains_key("outdated"), "{g:#?}");
    // The same contents with Windows line endings keep it.
    fx.write("base/procs.sql", &NEXT_LINE.replace('\n', "\r\n"));
    assert!(decision(&a).0.current);
    // Other contents outdate it, and so does converting without catalogs.
    fx.write(
        "base/procs.sql",
        &NEXT_LINE.replace("COUNT(*) + 1", "COUNT(*) + 2"),
    );
    let (d, g) = decision(&a);
    assert!(!d.current);
    assert_eq!(d.why_not.as_deref(), Some(CATALOGS_CHANGED));
    assert!(g.counts.get("outdated").is_some_and(|n| *n > 0), "{g:#?}");
    fx.write("base/procs.sql", NEXT_LINE);
    assert!(decision(&a).0.current);
    // Without the catalogs the file converts differently too.
    assert!(!decision(&fx.args(&[])).0.current);
}

#[test]
fn template_trees_take_catalogs_as_plain_ddl() {
    // `<%X%>` in the catalog file is not a placeholder: catalog files are
    // never read as templates (here it is only inside a comment).
    let fx = Fixture::new(
        "template",
        &[
            (
                "base/orders.sql",
                &format!("-- <%NOT_A_PLACEHOLDER%>\n{ORDERS}"),
            ),
            (AUDIT.0, AUDIT.1),
            (
                "src/procs/touch.sql",
                "UPDATE orders SET status = '<%STATUS%>' WHERE order_id = 1;\n",
            ),
        ],
    );
    let def: sqldiff_source::pack::TemplateSyntax =
        serde_json::from_value(serde_json::json!({ "open": "<%", "close": "%>" })).unwrap();
    let mut a = fx.args(&["base/orders.sql"]);
    a.template = Some(TemplateMode::new(&def, None).unwrap());
    let mut b = fx.args(&[]);
    b.template = Some(TemplateMode::new(&def, None).unwrap());
    let (inputs, unreadable) = tree::read(&a).unwrap();
    let t = tree::convert(&a, &inputs, unreadable, None).unwrap();
    assert!(t.report.templates);
    assert_eq!(t.report.base_catalogs.len(), 1);
    assert_eq!(t.report.files.len(), 2);
    assert!(
        !has(&t, "procs/touch.sql", "bind.unknown-table"),
        "{:?}",
        unit_rules(&t, "procs/touch.sql")
    );
    let (inputs, unreadable) = tree::read(&b).unwrap();
    let t0 = tree::convert(&b, &inputs, unreadable, None).unwrap();
    assert!(
        has(&t0, "procs/touch.sql", "bind.unknown-table"),
        "{:?}",
        unit_rules(&t0, "procs/touch.sql")
    );
}

#[test]
fn bad_catalogs_are_refused_before_anything_is_written() {
    let fx = Fixture::new(
        "refused",
        &[
            ("base/orders.sql", ORDERS),
            ("base/bundle.json", "{}"),
            ("src/q.sql", "SELECT 1;\n"),
        ],
    );
    let refused = |a: tree::Args| {
        let job = Job {
            tree: a,
            out: fx.path("out"),
            overwrite: false,
            discard_edits: false,
            instances: None,
            queue_all: false,
        };
        let e = convert_into(&job, &Hooks::default()).err().unwrap();
        assert!(!fx.path("out").exists(), "{e}");
        e
    };
    assert!(refused(fx.args(&["base/missing.sql"])).contains("not a file"));
    assert!(refused(fx.args(&["base/bundle.json"])).contains("snapshot bundles"));
    assert!(refused(fx.args(&["base/orders.sql", "base/orders.sql"])).contains("twice"));
    let mut a = fx.args(&["base/orders.sql"]);
    a.no_catalog = true;
    assert!(refused(a).contains("--no-catalog"));
    std::fs::write(fx.path("base/latin1.sql"), b"-- caf\xe9\n").unwrap();
    assert!(refused(fx.args(&["base/latin1.sql"])).contains("not UTF-8"));
}

#[test]
fn the_base_hash_reads_contents_in_order_not_paths() {
    let f = |p: &str, t: &str| base::File {
        path: p.into(),
        text: t.into(),
    };
    let h = |v: Vec<base::File>| base::hash(&v, &[]);
    assert_eq!(h(Vec::new()), None);
    assert_eq!(h(vec![f("a.sql", "x\n")]), h(vec![f("C:\\b.sql", "x\r\n")]));
    assert_ne!(h(vec![f("a.sql", "x\n")]), h(vec![f("a.sql", "y\n")]));
    assert_ne!(
        h(vec![f("a.sql", "x\n"), f("b.sql", "y\n")]),
        h(vec![f("b.sql", "y\n"), f("a.sql", "x\n")])
    );
}

#[test]
fn the_base_hash_covers_the_target_catalogs() {
    let f = |p: &str, t: &str| base::File {
        path: p.into(),
        text: t.into(),
    };
    let a = [f("a.sql", "x\n")];
    let without = base::hash(&a, &[]);
    let with = base::hash(&a, &[f("b.sql", "y\n")]);
    assert!(with.is_some());
    assert_ne!(without, with);
    assert_ne!(with, base::hash(&a, &[f("b.sql", "z\n")]));
    // Moving a file from one side to the other is another base.
    assert_ne!(
        base::hash(&[f("a.sql", "x\n"), f("b.sql", "y\n")], &[]),
        with
    );
}

/// The base of `a_base_object_that_does_not_convert_leaves_its_users_differs`,
/// with the target's own DDL for it.
const INSTALL: &str = "CREATE TABLE orders (order_id int NOT NULL PRIMARY KEY, site geography NULL);\nGO\nCREATE VIEW open_orders AS SELECT order_id FROM orders;\nGO\nCREATE TABLE order_items (order_id int NOT NULL, qty int NOT NULL);\nGO\n";
const INSTALL_PG: &str = "CREATE TABLE orders (order_id integer NOT NULL PRIMARY KEY, site text NULL);\nCREATE VIEW open_orders AS SELECT order_id FROM orders;\n";

#[test]
fn target_catalogs_replace_the_converted_base() {
    let fx = Fixture::new(
        "catalog-b",
        &[
            ("base/install.sql", INSTALL),
            ("base_pg/install.sql", INSTALL_PG),
            ("src/q1.sql", "SELECT order_id FROM orders;\n"),
            ("src/q2.sql", "SELECT order_id FROM open_orders;\n"),
            // In the source's base only: unknown on the target side.
            ("src/q3.sql", "SELECT qty FROM order_items;\n"),
            (AUDIT.0, AUDIT.1),
        ],
    );
    // Converted, orders does not convert: its users are flagged.
    let t = fx.convert(&["base/install.sql"]);
    assert!(has(&t, "q1.sql", base::BASE_SUSPECT));
    // With the target's DDL, orders binds on both sides, as given.
    let a = fx.args_b(&["base/install.sql"], &["base_pg/install.sql"]);
    let (inputs, unreadable) = tree::read(&a).unwrap();
    let t = tree::convert(&a, &inputs, unreadable, None).unwrap();
    for q in ["q1.sql", "q2.sql"] {
        assert!(!has(&t, q, base::BASE_SUSPECT));
        assert!(
            !has(&t, q, "bind.unknown-table"),
            "{q}: {:?}",
            unit_rules(&t, q)
        );
        assert!(
            status(&t, q) < Status::Differs,
            "{q}: {:?}",
            unit_rules(&t, q)
        );
    }
    assert_eq!(status(&t, "q3.sql"), Status::Differs);
    assert!(has(&t, "q3.sql", "bind.unknown-table"));
    // Nothing converted, so nothing left out; both sides' files listed.
    assert!(
        t.report.base_objects.is_empty(),
        "{:#?}",
        t.report.base_objects
    );
    assert!(t.report.base_statements.is_empty());
    assert_eq!(t.report.base_catalogs.len(), 1);
    assert_eq!(t.report.base_catalogs[0].tables, 3);
    assert_eq!(t.report.base_catalogs_b.len(), 1);
    assert_eq!(t.report.base_catalogs_b[0].tables, 2);
    assert_eq!(t.report.base_catalogs_b[0].sha256, file_hash(INSTALL_PG));
    let md = tree::markdown(&t.report);
    assert!(md.contains("Target side"), "{md}");
    assert!(tree::summary(&t.report).contains("--catalog-b"));
    let files: Vec<&str> = t.report.files.iter().map(|f| f.path.as_str()).collect();
    assert!(!files.iter().any(|f| f.contains("install")), "{files:?}");
}

#[test]
fn bad_target_catalogs_are_refused() {
    let fx = Fixture::new(
        "catalog-b-refused",
        &[
            ("base/install.sql", INSTALL),
            ("base_pg/install.sql", INSTALL_PG),
            ("base_pg/bundle.json", "{}"),
            ("src/q1.sql", "SELECT order_id FROM orders;\n"),
        ],
    );
    let refused = |a: tree::Args| tree::read(&a).err().unwrap();
    assert!(refused(fx.args_b(&[], &["base_pg/install.sql"])).contains("needs --catalog"));
    assert!(
        refused(fx.args_b(&["base/install.sql"], &["base_pg/bundle.json"]))
            .contains("target dialect")
    );
    assert!(refused(fx.args_b(
        &["base/install.sql"],
        &["base_pg/install.sql", "base_pg/install.sql"]
    ))
    .contains("given twice as --catalog-b"));
    let mut a = fx.args_b(&["base/install.sql"], &["base_pg/install.sql"]);
    a.no_catalog = true;
    assert!(refused(a).contains("--no-catalog"));
}
