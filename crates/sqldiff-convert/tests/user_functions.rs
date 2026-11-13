//! A converted tree's calls to its own user functions: a call counts only as
//! far as the called function's own conversion does (`callees::resolve`).

use std::path::PathBuf;

use sqldiff_convert::tree::{self, Tree};
use sqldiff_convert::{Status, Unit};
use sqldiff_core::Dialect;

const TABLES: &str = "CREATE TABLE App.Codes (Id int NOT NULL PRIMARY KEY, Code nvarchar(5) NOT NULL, ValidTo datetime2(7) NOT NULL);
GO
CREATE TABLE App.Items (Id int NOT NULL PRIMARY KEY, CodeId int NULL);
GO
";

/// Reads a code; proven under assumptions (string comparison, row counts).
const GOOD: &str = "CREATE FUNCTION App.GetCode(@Code nvarchar(5))
RETURNS int
AS
BEGIN
    DECLARE @Id int;
    SELECT @Id = Id FROM App.Codes WHERE Code = @Code;
    RETURN @Id;
END;
";

/// T-SQL's NEWID() and PostgreSQL's gen_random_uuid() are not shown the
/// same: the function is not proven.
const BAD: &str = "CREATE FUNCTION App.GetCode(@Code nvarchar(5))
RETURNS int
AS
BEGIN
    DECLARE @Id int;
    SELECT @Id = Id FROM App.Codes WHERE Code = @Code AND CAST(NEWID() AS varchar(36)) <> '';
    RETURN @Id;
END;
";

/// WideWorldImporters' GetStateProvinceID: the current row by its end of
/// time, spelt compactly; PostgreSQL gets 6 digits under ASM-END-OF-TIME.
const EOT: &str = "CREATE FUNCTION App.GetCode(@Code nvarchar(5))
RETURNS int
AS
BEGIN
    DECLARE @Id int;
    SELECT @Id = Id FROM App.Codes WHERE Code = @Code AND ValidTo = '99991231 23:59:59.9999999';
    RETURN @Id;
END;
";

const CALLER: &str = "CREATE PROCEDURE App.AddItem @Id int, @Code nvarchar(5)
AS
BEGIN
    SET NOCOUNT ON;
    SET XACT_ABORT ON;
    UPDATE App.Items SET CodeId = App.GetCode(@Code) WHERE Id = @Id;
END;
";

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str, files: &[(&str, &str)]) -> Fixture {
        let dir = std::env::temp_dir().join(format!("sqldiff-udf-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (p, text) in files {
            let f = dir.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        Fixture { dir }
    }

    fn convert(&self) -> Tree {
        let a = tree::Args {
            rules: Default::default(),
            src: self.dir.clone(),
            from: Dialect::Tsql,
            to: Dialect::Postgres,
            jobs: Some(1),
            no_catalog: false,
            recheck: Default::default(),
            template: None,
            extensions: tree::extensions(Vec::new()),
            helpers: Vec::new(),
            deployment: Default::default(),
            catalogs: Vec::new(),
            catalogs_b: Vec::new(),
        };
        let (inputs, unreadable) = tree::read(&a).unwrap();
        tree::convert(&a, &inputs, unreadable, None).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The unit of `path` whose source holds `needle`, the smallest one.
fn unit<'t>(t: &'t Tree, path: &str, text: &str, needle: &str) -> &'t Unit {
    let k = t
        .report
        .files
        .iter()
        .position(|f| f.path == path)
        .unwrap_or_else(|| panic!("no file {path}"));
    let at = text.find(needle).unwrap();
    t.conversions[k]
        .units
        .iter()
        .filter(|u| u.src.start <= at && at < u.src.end)
        .min_by_key(|u| u.src.end - u.src.start)
        .unwrap()
}

fn rules(u: &Unit) -> Vec<&str> {
    u.notes.iter().map(|n| n.rule.as_str()).collect()
}

const CALL: &str = "UPDATE App.Items";

#[test]
fn a_call_to_a_proven_function_carries_its_assumptions() {
    let fx = Fixture::new(
        "good",
        &[("t.sql", TABLES), ("f.sql", GOOD), ("p.sql", CALLER)],
    );
    let t = fx.convert();
    let u = unit(&t, "p.sql", CALLER, CALL);
    assert_eq!(u.status, Status::Assumptions, "{:#?}", u.notes);
    let r = rules(u);
    assert!(r.contains(&"ASM-UDF-CALLEE:app.getcode"), "{r:?}");
    assert!(r.contains(&"ASM-UDF-CALL-CONTEXT"), "{r:?}");
    // The function's own assumption, carried to the call.
    assert!(r.contains(&"ASM-OBSERVER-ROWCOUNT"), "{r:?}");
}

#[test]
fn the_compact_end_of_time_is_compared() {
    let fx = Fixture::new(
        "eot",
        &[("t.sql", TABLES), ("f.sql", EOT), ("p.sql", CALLER)],
    );
    let t = fx.convert();
    let f = unit(&t, "f.sql", EOT, "SELECT @Id");
    assert_eq!(f.status, Status::Assumptions, "{:#?}", f.notes);
    assert!(rules(f).contains(&"ASM-END-OF-TIME"), "{:?}", rules(f));
    let u = unit(&t, "p.sql", CALLER, CALL);
    assert_eq!(u.status, Status::Assumptions, "{:#?}", u.notes);
    assert!(rules(u).contains(&"ASM-END-OF-TIME"), "{:?}", rules(u));
}

#[test]
fn a_call_to_an_unproven_function_is_not_proven() {
    let fx = Fixture::new(
        "bad",
        &[("t.sql", TABLES), ("f.sql", BAD), ("p.sql", CALLER)],
    );
    let t = fx.convert();
    let f = unit(&t, "f.sql", BAD, "SELECT @Id");
    assert_eq!(f.status, Status::Unproven, "{:#?}", f.notes);
    let u = unit(&t, "p.sql", CALLER, CALL);
    assert_eq!(u.status, Status::Unproven, "{:#?}", u.notes);
    let n = u
        .notes
        .iter()
        .find(|n| n.rule == sqldiff_convert::callees::CALLEE_RULE)
        .unwrap_or_else(|| panic!("{:#?}", u.notes));
    assert!(
        n.text.contains("app.getcode") && n.text.contains("f.sql"),
        "{}",
        n.text
    );
}

#[test]
fn a_call_to_a_function_outside_the_tree_is_not_proven() {
    // The function's header is in the catalog of both sides only through
    // the converted file: without that file the engine has no function to
    // compare, and with it but outside the tree (a base catalog) its own
    // conversion is not known here.
    let fx = Fixture::new("missing", &[("t.sql", TABLES), ("p.sql", CALLER)]);
    let t = fx.convert();
    let u = unit(&t, "p.sql", CALLER, CALL);
    assert!(u.status >= Status::Unproven, "{:#?}", u.notes);
}

#[test]
fn recursive_functions_are_not_proven() {
    let rec = "CREATE FUNCTION App.Fact(@n int)
RETURNS int
AS
BEGIN
    RETURN CASE WHEN @n <= 1 THEN 1 ELSE @n * App.Fact(@n - 1) END;
END;
";
    let caller = "CREATE PROCEDURE App.Use @Id int
AS
BEGIN
    SET NOCOUNT ON;
    SET XACT_ABORT ON;
    UPDATE App.Items SET CodeId = App.Fact(@Id) WHERE Id = @Id;
END;
";
    let fx = Fixture::new(
        "rec",
        &[("t.sql", TABLES), ("f.sql", rec), ("p.sql", caller)],
    );
    let t = fx.convert();
    let u = unit(&t, "f.sql", rec, "RETURN CASE");
    assert!(u.status >= Status::Unproven, "{:#?}", u.notes);
    let u = unit(&t, "p.sql", caller, CALL);
    assert!(u.status >= Status::Unproven, "{:#?}", u.notes);
}

/// A tree whose files are held as a core and a store key while the
/// cross-file passes run (`convert_stored`) comes out as the one held whole
/// (`convert`): the caller of a function that is not proven is not proven
/// either, and everything else is the same.
#[test]
fn a_tree_held_in_the_store_gives_the_tree_held_whole() {
    let fx = Fixture::new(
        "stored",
        &[("t.sql", TABLES), ("f.sql", BAD), ("p.sql", CALLER)],
    );
    let whole = fx.convert();
    let a = tree::Args {
        rules: Default::default(),
        src: fx.dir.clone(),
        from: Dialect::Tsql,
        to: Dialect::Postgres,
        jobs: Some(2),
        no_catalog: false,
        recheck: Default::default(),
        template: None,
        extensions: tree::extensions(Vec::new()),
        helpers: Vec::new(),
        deployment: Default::default(),
        catalogs: Vec::new(),
        catalogs_b: Vec::new(),
    };
    let (inputs, unreadable) = tree::read(&a).unwrap();
    let out = fx
        .dir
        .parent()
        .unwrap()
        .join(format!("sqldiff-udf-stored-out-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    let hooks = tree::Hooks::default();
    let held = tree::convert_stored(&a, &inputs, unreadable, &out, &hooks, true).unwrap();
    let _ = std::fs::remove_dir_all(&out);
    assert_eq!(held.outputs, whole.outputs);
    let json = |t: &Tree| serde_json::to_string(&t.conversions).unwrap();
    assert_eq!(json(&held), json(&whole));
    let p = whole
        .report
        .files
        .iter()
        .find(|f| f.path == "p.sql")
        .unwrap();
    assert!(
        p.counts.unproven > 0,
        "the caller of an unproven function is unproven in both: {:?}",
        p.counts
    );
    let statuses = |t: &Tree| {
        t.report
            .files
            .iter()
            .map(|f| (f.path.clone(), f.status, format!("{:?}", f.counts)))
            .collect::<Vec<_>>()
    };
    assert_eq!(statuses(&held), statuses(&whole));
}

/// A caller resolved before its callee goes bad gets only the "not proven"
/// note, not the assumptions the callee seemed to carry before (the calls
/// are resolved callees first), and the notes do not depend on file names.
#[test]
fn a_call_through_an_unproven_chain_carries_no_assumptions_of_it() {
    let mid = "CREATE FUNCTION App.Mid(@Code nvarchar(5))
RETURNS int
AS
BEGIN
    RETURN App.GetCode(@Code);
END;
";
    let caller = "CREATE PROCEDURE App.AddItem @Id int, @Code nvarchar(5)
AS
BEGIN
    SET NOCOUNT ON;
    SET XACT_ABORT ON;
    UPDATE App.Items SET CodeId = App.Mid(@Code) WHERE Id = @Id;
END;
";
    let mut seen = Vec::new();
    // Caller first, then callee last: the order in which the old rounds
    // left the middle function's assumptions on the caller.
    for (name, p, m, f) in [
        ("chain-a", "a.sql", "b.sql", "c.sql"),
        ("chain-z", "z.sql", "y.sql", "x.sql"),
    ] {
        let fx = Fixture::new(name, &[("t.sql", TABLES), (f, BAD), (m, mid), (p, caller)]);
        let t = fx.convert();
        let u = unit(&t, m, mid, "RETURN App");
        assert_eq!(u.status, Status::Unproven, "{:#?}", u.notes);
        let u = unit(&t, p, caller, CALL);
        assert_eq!(u.status, Status::Unproven, "{:#?}", u.notes);
        assert!(
            u.notes
                .iter()
                .all(|n| n.status != Status::Assumptions || !n.rule.ends_with("app.getcode")),
            "{:#?}",
            u.notes
        );
        let r: Vec<String> = u.notes.iter().map(|n| n.rule.to_string()).collect();
        seen.push(r);
    }
    assert_eq!(seen[0], seen[1]);
}
