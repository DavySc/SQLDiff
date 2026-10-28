//! `sqldiff convert`: a source tree in, a mirrored output tree and a report out.

use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqldiff-convert-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(p: &Path, text: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_sqldiff"))
        .args(args)
        .output()
        .unwrap()
}

fn utf16le_with_bom(s: &str) -> Vec<u8> {
    let mut b = vec![0xFF, 0xFE];
    for u in s.encode_utf16() {
        b.extend_from_slice(&u.to_le_bytes());
    }
    b
}

#[test]
fn converts_a_tree_with_cross_file_context() {
    let d = scratch("tree");
    let src = d.join("src");
    let out = d.join("out");
    write(
        &src.join("Sales/Procs/NextOrder.sql"),
        b"\xEF\xBB\xBFCREATE PROCEDURE Sales.NextOrder @CustomerID int, @OrderID int OUTPUT AS\nBEGIN\n  SET @OrderID = @CustomerID;\nEND\n",
    );
    write(
        &src.join("Sales/Procs/Caller.sql"),
        &utf16le_with_bom("CREATE PROCEDURE Sales.Caller AS\nBEGIN\n  DECLARE @id int;\n  EXEC Sales.NextOrder 5, @id OUTPUT;\n  UPDATE Sales.Orders SET Total = 0;\nEND\n"),
    );
    write(
        &src.join("Sales/Tables/Orders.sql"),
        b"CREATE TABLE Sales.Orders (OrderID int NOT NULL, Total decimal(18, 2) NULL);\n",
    );
    write(&src.join("notes.txt"), b"not sql");

    let o = run(&[
        "convert",
        src.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--to",
        "postgres",
        "-j",
        "2",
    ]);
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.code().is_some_and(|c| c <= 1), "{stderr}");

    // Same layout, .sql files only.
    for f in [
        "Sales/Procs/NextOrder.sql",
        "Sales/Procs/Caller.sql",
        "Sales/Tables/Orders.sql",
    ] {
        assert!(out.join(f).is_file(), "{f} missing; {stderr}");
    }
    assert!(!out.join("notes.txt").exists());
    let caller = std::fs::read_to_string(out.join("Sales/Procs/Caller.sql")).unwrap();
    // The OUTPUT call converts only because NextOrder is in the tree.
    assert!(
        caller.contains("CALL Sales.NextOrder(5, v_id);"),
        "{caller}"
    );

    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("conversion-report.json")).unwrap())
            .unwrap();
    assert_eq!(report["totals"]["files"], 3);
    assert!(report["catalogs"].as_bool().unwrap());
    let md = std::fs::read_to_string(out.join("conversion-report.md")).unwrap();
    assert!(md.contains("# Conversion report"));

    // Every unit listed in the report points at its line in the written file,
    // and every non-converted status is marked in the file itself.
    for f in report["files"].as_array().unwrap() {
        let text = std::fs::read_to_string(out.join(f["path"].as_str().unwrap())).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        for u in f["units"].as_array().unwrap() {
            let l = u["out_line"].as_u64().unwrap() as usize;
            let status = u["status"].as_str().unwrap();
            let above = lines[l - 2];
            if status == "not_converted" {
                assert!(
                    lines[l - 1].contains("[not converted]"),
                    "{}:{l}\n{text}",
                    f["path"]
                );
            } else {
                assert!(
                    above.contains("-- [sqldiff: "),
                    "{}:{l} {status}\n{text}",
                    f["path"]
                );
            }
        }
    }

    // A second run refuses to write into a non-empty directory.
    let again = run(&[
        "convert",
        src.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--to",
        "postgres",
    ]);
    assert_eq!(again.status.code(), Some(2));
    let again = run(&[
        "convert",
        src.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--to",
        "db2i",
        "--overwrite",
    ]);
    assert!(
        again.status.code().is_some_and(|c| c <= 1),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    // The output directory may not sit inside the source tree.
    let inside = run(&[
        "convert",
        src.to_str().unwrap(),
        "-o",
        src.join("x").to_str().unwrap(),
        "--to",
        "postgres",
    ]);
    assert_eq!(inside.status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_file_is_only_converted_when_every_unit_is() {
    let d = scratch("status");
    let src = d.join("src");
    let out = d.join("out");
    write(&src.join("ok.sql"), b"UPDATE t SET a = 1;\n");
    write(
        &src.join("mixed.sql"),
        b"UPDATE t SET a = 1;\nSELECT HASHBYTES('MD5', 'x') AS h;\n",
    );
    let o = run(&[
        "convert",
        src.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--to",
        "postgres",
        "--no-catalog",
    ]);
    assert_eq!(
        o.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("conversion-report.json")).unwrap())
            .unwrap();
    let status = |p: &str| {
        report["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["path"] == p)
            .unwrap()["status"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(status("ok.sql"), "converted");
    assert_eq!(status("mixed.sql"), "not_converted");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_unit_the_engine_cannot_decide_is_marked_not_proven() {
    // SQL Server rounds '1.005' to decimal(10,2) (1.01); PostgreSQL does not.
    let d = scratch("unproven");
    let (src, out) = (d.join("src"), d.join("out"));
    write(
        &src.join("orders.sql"),
        b"CREATE TABLE dbo.Orders (id int NOT NULL PRIMARY KEY, amt decimal(10,2) NOT NULL);\nGO\n",
    );
    write(
        &src.join("del.sql"),
        b"CREATE PROCEDURE dbo.DelOrders AS\nBEGIN\n  DELETE FROM dbo.Orders WHERE amt = '1.005';\nEND;\nGO\n",
    );
    let o = run(&[
        "convert",
        src.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--to",
        "postgres",
    ]);
    // Not converted or under assumptions: the exit code says so.
    assert_eq!(
        o.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let text = std::fs::read_to_string(out.join("del.sql")).unwrap();
    assert!(
        text.contains("-- [sqldiff: not proven] equiv.unproven\n  DELETE FROM Orders"),
        "{text}"
    );
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("conversion-report.json")).unwrap())
            .unwrap();
    let f = report["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "del.sql")
        .unwrap();
    assert_eq!(f["status"], "unproven", "{f}");
    assert_eq!(f["counts"]["unproven"], 1, "{f}");
    assert_eq!(f["counts"]["differs"], 0, "{f}");
    let u = f["units"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["status"] == "unproven")
        .unwrap();
    assert_eq!(u["rules"][0], "equiv.unproven", "{u}");
    assert!(u["reason"].as_str().unwrap().contains("'1.005'"), "{u}");
    assert_eq!(report["totals"]["units"]["not proven"], 1, "{report}");
    let md = std::fs::read_to_string(out.join("conversion-report.md")).unwrap();
    assert!(md.contains("### `del.sql`: not proven"), "{md}");
    let _ = std::fs::remove_dir_all(&d);
}

fn example_pack() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/examples/pack")
        .to_str()
        .unwrap()
        .to_string()
}

const ENTITY_TABLE: &[u8] =
    b"CREATE TABLE <%SCHEMA%>.<%TABLE%> (Id int NOT NULL, Price decimal(12, 2) NOT NULL);\n";

#[test]
fn converts_a_template_tree() {
    let d = scratch("templates");
    let src = d.join("src");
    let out = d.join("out");
    write(&src.join("tables/entity.tpl"), ENTITY_TABLE);
    write(
        &src.join("procs/touch.tpl"),
        b"<%@include header.sql %>\nCREATE PROCEDURE <%SCHEMA%>.Touch<%ENTITY%> @Id int AS\nBEGIN\n  UPDATE <%SCHEMA%>.<%TABLE%> SET Price = 0 WHERE Id = @Id;\nEND\n",
    );
    write(
        &src.join("procs/cost.tpl"),
        b"CREATE PROCEDURE <%SCHEMA%>.Cost @Id int AS\nBEGIN\n  UPDATE <%SCHEMA%>.<%TABLE%> SET Cost = 0 WHERE Id = @Id;\nEND\n",
    );
    write(&src.join("plain.sql"), b"not read: wrong extension\n");
    let pack = example_pack();
    let o = run(&[
        "convert",
        src.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--to",
        "postgres",
        "--template",
        "--extension",
        ".tpl",
        "--pack",
        &pack,
        "--isolated",
    ]);
    assert_eq!(
        o.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(!out.join("plain.sql").exists());

    let proc = std::fs::read_to_string(out.join("procs/touch.tpl")).unwrap();
    // A template for the target: placeholders and directives kept.
    assert!(proc.contains("<%@include header.sql %>"), "{proc}");
    assert!(
        proc.contains("PROCEDURE <%SCHEMA%>.Touch<%ENTITY%>(p_id integer)"),
        "{proc}"
    );
    assert!(!proc.to_ascii_lowercase().contains("sqldifftpl"), "{proc}");
    // The directive assumption is said once, at the top.
    assert!(
        proc.starts_with("-- [sqldiff: under assumptions] ASM-TEMPLATE-DIRECTIVE"),
        "{proc}"
    );
    assert_eq!(proc.matches("ASM-TEMPLATE-DIRECTIVE").count(), 1, "{proc}");
    // The table another template creates binds: Price exists, Cost does not.
    let lines: Vec<&str> = proc.lines().collect();
    let at = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap();
    assert!(!lines[at("SET Price") - 1].contains("differs"), "{proc}");
    let cost = std::fs::read_to_string(out.join("procs/cost.tpl")).unwrap();
    assert!(
        cost.contains(
            "-- [sqldiff: differs] bind.unknown-column\n  UPDATE <%SCHEMA%>.<%TABLE%> SET Cost"
        ),
        "{cost}"
    );

    let table = std::fs::read_to_string(out.join("tables/entity.tpl")).unwrap();
    assert!(
        table.contains("CREATE TABLE <%SCHEMA%>.<%TABLE%> (\n    Id integer NOT NULL,\n    Price numeric(12, 2) NOT NULL\n);"),
        "{table}"
    );

    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("conversion-report.json")).unwrap())
            .unwrap();
    assert_eq!(report["templates"], true);
    let file = report["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "procs/touch.tpl")
        .unwrap();
    assert_eq!(file["status"], "assumptions");
    assert_eq!(file["template"]["special_forms"][0], "include");
    let params: Vec<&str> = file["template"]["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert_eq!(params, ["SCHEMA", "ENTITY", "TABLE"]);
    // Every reported line points at the unit in the annotated output.
    for u in file["units"].as_array().unwrap() {
        let l = u["out_line"].as_u64().unwrap() as usize;
        assert!(
            !lines[l - 1].trim_start().starts_with("-- [sqldiff:"),
            "{u}"
        );
    }
    // Reasons quote placeholders as the template writes them.
    let cost = report["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "procs/cost.tpl")
        .unwrap();
    assert_eq!(cost["status"], "differs");
    let reason = cost["units"][1]["reason"].as_str().unwrap();
    assert!(reason.contains("<%SCHEMA%>.<%TABLE%>"), "{reason}");
    assert!(
        !reason.to_ascii_lowercase().contains("sqldifftpl"),
        "{reason}"
    );
}

#[test]
fn a_template_object_two_files_create_is_not_bound() {
    // Two templates create a table under the same placeholder names: which
    // one an instance gets is the generator's business, so neither binds.
    let d = scratch("templates-shared");
    let src = d.join("src");
    let out = d.join("out");
    write(&src.join("a.sql"), ENTITY_TABLE);
    write(
        &src.join("b.sql"),
        b"CREATE TABLE <%SCHEMA%>.<%TABLE%> (Id int NOT NULL, Cost decimal(12, 2) NOT NULL);\n",
    );
    write(
        &src.join("p.sql"),
        b"CREATE PROCEDURE <%SCHEMA%>.P @Id int AS\nBEGIN\n  UPDATE <%SCHEMA%>.<%TABLE%> SET Cost = 0 WHERE Id = @Id;\nEND\n",
    );
    let pack = example_pack();
    let o = run(&[
        "convert",
        src.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--to",
        "postgres",
        "--template",
        "--pack",
        &pack,
        "--isolated",
    ]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let p = std::fs::read_to_string(out.join("p.sql")).unwrap();
    assert!(!p.contains("bind.unknown-column"), "{p}");
    assert!(p.contains("ASM-TEMPLATE-PLACEHOLDER"), "{p}");
    // Binding across templates rests on one value per placeholder per weave.
    assert!(p.contains("ASM-TEMPLATE-SHARED-VALUES"), "{p}");
    let alone = d.join("alone");
    let o = run(&[
        "convert",
        src.to_str().unwrap(),
        "-o",
        alone.to_str().unwrap(),
        "--to",
        "postgres",
        "--template",
        "--no-catalog",
        "--pack",
        &pack,
        "--isolated",
    ]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let p = std::fs::read_to_string(alone.join("p.sql")).unwrap();
    assert!(!p.contains("ASM-TEMPLATE-SHARED-VALUES"), "{p}");
}

#[test]
fn template_mode_needs_a_template_syntax() {
    let d = scratch("templates-nosyntax");
    let src = d.join("src");
    write(&src.join("a.sql"), b"SELECT 1;\n");
    let o = run(&[
        "convert",
        src.to_str().unwrap(),
        "-o",
        d.join("out").to_str().unwrap(),
        "--to",
        "postgres",
        "--template",
        "--isolated",
    ]);
    assert_eq!(o.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("[template]"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn a_template_unit_written_by_hand_is_spliced_in_unchecked() {
    use sqldiff_convert::review::{file_hash, By, Record, State, LEDGER_FILE};
    let d = scratch("template-written");
    let (src, out) = (d.join("src"), d.join("out"));
    let proc = "CREATE PROCEDURE <%SCHEMA%>.Touch @Id int AS\nBEGIN\n  UPDATE <%SCHEMA%>.<%TABLE%> SET Price = 0 WHERE Id = @Id;\nEND\n";
    write(&src.join("touch.sql"), proc.as_bytes());
    let pack = example_pack();
    let convert = |extra: &[&str]| {
        let mut args = vec![
            "convert",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--to",
            "postgres",
            "--template",
            "--pack",
            &pack,
            "--isolated",
        ];
        args.extend_from_slice(extra);
        let o = run(&args);
        assert!(
            o.status.code().is_some(),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
        let r: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(out.join("conversion-report.json")).unwrap(),
        )
        .unwrap();
        r["files"][0].clone()
    };
    let f = convert(&[]);
    let before = std::fs::read_to_string(out.join("touch.sql")).unwrap();
    // The UPDATE carries the template's assumptions, so the report lists it.
    let id = f["units"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["src_line"] == 3)
        .unwrap_or_else(|| panic!("no unit on line 3 in {f:#}"))["id"]
        .as_str()
        .unwrap()
        .to_string();
    let text = "  UPDATE <%SCHEMA%>.<%TABLE%> SET price = 0 WHERE id = p_id;";
    let r = Record {
        unit: id,
        path: "touch.sql".into(),
        rule: String::new(),
        state: State::Written,
        by: By::Human,
        author: "Davy".into(),
        reason: "lower-case columns".into(),
        at: "2026-10-26T12:00:00Z".into(),
        input_hash: file_hash(proc),
        output_hash: file_hash(&before),
        text: Some(text.into()),
        base_catalogs: None,
    };
    std::fs::write(
        out.join(LEDGER_FILE),
        serde_json::to_string(&r).unwrap() + "\n",
    )
    .unwrap();
    let f = convert(&["--overwrite"]);
    let after = std::fs::read_to_string(out.join("touch.sql")).unwrap();
    assert!(after.contains(text), "{after}");
    let u = f["units"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["src_line"] == 3)
        .unwrap_or_else(|| panic!("{f:#}"));
    // Not checked by convert: never better than not proven.
    assert_eq!(u["status"], "unproven", "{u:#}");
    assert!(
        u["rules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "hand-written.unchecked"),
        "{u:#}"
    );
}

/// A source file name with backslashes reads as a path that climbs out of
/// the output directory: it is reported unreadable and nothing is written
/// outside the output directory.
#[cfg(unix)]
#[test]
fn a_file_name_cannot_climb_out_of_the_output_directory() {
    let d = scratch("climb");
    let src = d.join("w/x/src");
    let out = d.join("w/x/out");
    write(&src.join("ok.sql"), b"SELECT 1;\n");
    write(&src.join("sub/a\\..\\..\\..\\..\\evil.sql"), b"SELECT 1;\n");
    let o = run(&[
        "convert",
        src.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--to",
        "postgres",
    ]);
    assert_eq!(
        o.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(!d.join("w/evil.sql").exists());
    assert!(!d.join("w/x/evil.sql").exists());
    assert!(out.join("ok.sql").exists());
    let report = std::fs::read_to_string(out.join("conversion-report.json")).unwrap();
    assert!(report.contains("evil.sql"), "{report}");
}

/// `--overwrite` does not write through a symbolic link in the output
/// directory, which could point at the source file itself.
#[cfg(unix)]
#[test]
fn overwrite_does_not_follow_links_in_the_output_directory() {
    let d = scratch("symlink");
    let src = d.join("src");
    let out = d.join("out");
    let source = b"SELECT TOP 1 a FROM t;\n";
    write(&src.join("x.sql"), source);
    std::fs::create_dir_all(&out).unwrap();
    std::os::unix::fs::symlink(src.join("x.sql"), out.join("x.sql")).unwrap();
    let o = run(&[
        "convert",
        src.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--to",
        "postgres",
        "--overwrite",
    ]);
    assert_eq!(
        o.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(String::from_utf8_lossy(&o.stderr).contains("symbolic link"));
    assert_eq!(std::fs::read(src.join("x.sql")).unwrap(), source);
}

#[test]
fn base_catalogs_bind_and_are_neither_written_nor_counted() {
    let d = scratch("catalogs");
    let (src, out, base) = (d.join("src"), d.join("out"), d.join("base"));
    write(
        &base.join("orders.sql"),
        b"CREATE TABLE orders (order_id int NOT NULL PRIMARY KEY, status varchar(10) NOT NULL);\nGO\n",
    );
    write(
        &base.join("items.sql"),
        b"CREATE TABLE order_items (order_id int NOT NULL, qty int NOT NULL);\nGO\n",
    );
    write(
        &src.join("procs/update_item.sql"),
        b"UPDATE orders SET status = 'open' WHERE order_id = 1;\n",
    );
    write(
        &src.join("tables/audit_log.sql"),
        b"CREATE TABLE audit_log (id int NOT NULL);\n",
    );
    let (s, o, b1, b2) = (
        src.to_str().unwrap(),
        out.to_str().unwrap(),
        base.join("orders.sql"),
        base.join("items.sql"),
    );
    let (b1, b2) = (b1.to_str().unwrap(), b2.to_str().unwrap());
    // Repeatable, in order.
    let r = run(&[
        "convert",
        s,
        "-o",
        o,
        "--to",
        "postgres",
        "--isolated",
        "--catalog",
        b1,
        "--catalog",
        b2,
    ]);
    let stdout = String::from_utf8_lossy(&r.stdout);
    assert!(stdout.contains("base catalogs: 2 file(s)"), "{stdout}");
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("conversion-report.json")).unwrap())
            .unwrap();
    let paths: Vec<&str> = report["base_catalogs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, [b1, b2]);
    let files: Vec<&str> = report["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(files, ["procs/update_item.sql", "tables/audit_log.sql"]);
    assert!(!out.join("orders.sql").exists() && !out.join("items.sql").exists());
    let f = &report["files"][0];
    assert!(
        f["units"].as_array().unwrap().iter().all(|u| !u["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x == "bind.unknown-table")),
        "{f:#}"
    );

    // The done gate converts again with the same catalogs; without them the
    // output differs from what convert wrote.
    let gate = |extra: &[&str]| {
        let mut args = vec![
            "review-status",
            s,
            o,
            "--to",
            "postgres",
            "--isolated",
            "--json",
        ];
        args.extend_from_slice(extra);
        let r = run(&args);
        let v: serde_json::Value = serde_json::from_slice(&r.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&r.stderr)));
        v["counts"]
            .as_object()
            .unwrap()
            .get("stale-output")
            .and_then(|n| n.as_u64())
            .unwrap_or(0)
    };
    assert_eq!(gate(&["--catalog", b1, "--catalog", b2]), 0);
    assert!(gate(&[]) > 0);

    // Not with --no-catalog.
    let r = run(&[
        "convert",
        s,
        "-o",
        o,
        "--to",
        "postgres",
        "--isolated",
        "--overwrite",
        "--no-catalog",
        "--catalog",
        b1,
    ]);
    assert_eq!(r.status.code(), Some(2));
    let err = String::from_utf8_lossy(&r.stderr);
    assert!(
        err.contains("--no-catalog") && err.contains("--catalog"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&d);
}
