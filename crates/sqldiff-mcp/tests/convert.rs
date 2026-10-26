//! The conversion tools: the engine of `sqldiff convert`, over stdio.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqldiff-mcp-convert-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// Send each request and collect its response.
fn session(messages: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sqldiff-mcp"))
        .arg("--isolated")
        .env(
            "SQLDIFF_DATA_DIR",
            std::env::temp_dir().join(format!("sqldiff-mcp-data-{}", std::process::id())),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut responses = Vec::new();
    for m in messages {
        writeln!(stdin, "{m}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        responses.push(serde_json::from_str(&line).unwrap());
    }
    drop(stdin);
    assert!(child.wait().unwrap().success());
    responses
}

fn call(id: u64, name: &str, args: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": args } })
}

fn result(r: &Value) -> &Value {
    assert_eq!(r["result"]["isError"], false, "{r}");
    &r["result"]["structuredContent"]
}

/// A small tree in its own directory per test (tests run in parallel).
fn tree(name: &str) -> (PathBuf, PathBuf) {
    let d = scratch(name);
    let src = d.join("src");
    write(
        &src.join("Sales/Procs/NextOrder.sql"),
        "CREATE PROCEDURE Sales.NextOrder @CustomerID int, @OrderID int OUTPUT AS\nBEGIN\n  SET @OrderID = @CustomerID;\nEND\n",
    );
    write(
        &src.join("Sales/Procs/Caller.sql"),
        "CREATE PROCEDURE Sales.Caller AS\nBEGIN\n  DECLARE @id int;\n  EXEC Sales.NextOrder 5, @id OUTPUT;\n  UPDATE Sales.Orders SET Total = 0;\nEND\n",
    );
    write(
        &src.join("Sales/Tables/Orders.sql"),
        "CREATE TABLE Sales.Orders (OrderID int NOT NULL, Total decimal(18, 2) NULL);\n",
    );
    (d, src)
}

#[test]
fn tools_are_listed() {
    let r = session(&[json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })]);
    let names: Vec<&str> = r[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for n in [
        "convert",
        "convert_tree",
        "conversion_queue",
        "write_unit",
        "converted_file",
    ] {
        assert!(names.contains(&n), "{names:?}");
    }
}

#[test]
fn tree_writes_what_the_report_says_and_the_listing_matches_it() {
    let (d, src) = tree("write");
    let out = d.join("out");
    // Holds an earlier run's file; `overwrite` replaces what a run writes.
    let other = d.join("other");
    write(&other.join("old.txt"), "earlier run");
    let r = session(&[
        call(
            1,
            "convert_tree",
            json!({ "src": src, "to": "postgres", "out": out, "max_units": 1000 }),
        ),
        // Not empty now: refused without `overwrite`, as the CLI refuses.
        call(
            2,
            "convert_tree",
            json!({ "src": src, "to": "postgres", "out": out }),
        ),
        call(
            3,
            "convert_tree",
            json!({ "src": src, "to": "db2i", "out": other, "overwrite": true }),
        ),
        call(
            4,
            "convert_tree",
            json!({ "src": src, "to": "postgres", "out": src.join("inside") }),
        ),
    ]);
    let s = result(&r[0]);
    assert_eq!(s["totals"]["files"], 3);
    assert_eq!(s["catalogs"], true);
    // The cross-file call converts because NextOrder is in the tree.
    let caller = std::fs::read_to_string(out.join("Sales/Procs/Caller.sql")).unwrap();
    assert!(
        caller.contains("CALL Sales.NextOrder(5, v_id);"),
        "{caller}"
    );
    // The listing is the written report, file for file and unit for unit.
    let report: Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("conversion-report.json")).unwrap())
            .unwrap();
    assert!(out.join("conversion-report.md").is_file());
    assert_eq!(s["totals"]["units"], report["totals"]["units"]);
    assert_eq!(
        s["totals"]["files_by_status"],
        report["totals"]["files_by_status"]
    );
    for f in s["files"].as_array().unwrap() {
        let w = report["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["path"] == f["path"])
            .unwrap();
        for k in ["status", "counts", "units", "unplaced"] {
            assert_eq!(f[k], w[k], "{k} of {}", f["path"]);
        }
        assert_eq!(f["omitted_units"], 0);
    }
    let worst = report["files"]
        .as_array()
        .unwrap()
        .iter()
        .all(|f| ["converted", "assumptions"].contains(&f["status"].as_str().unwrap()));
    assert_eq!(s["ok"], worst);

    assert_eq!(r[1]["result"]["isError"], true);
    let e = r[1]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(e.contains("not empty"), "{e}");
    assert_eq!(result(&r[2])["to"], "db2i");
    assert!(other.join("Sales/Procs/Caller.sql").is_file());
    let e = r[3]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(e.contains("inside the source tree"), "{e}");
}

#[test]
fn tree_without_out_writes_nothing_and_shows_files() {
    let (_d, src) = tree("memory");
    let before: Vec<_> = walk(&src);
    let r = session(&[
        call(
            1,
            "convert_tree",
            json!({ "src": src, "to": "postgres", "limit": 1, "max_units": 0 }),
        ),
        call(
            2,
            "converted_file",
            json!({ "tree": "tree-1", "path": "Sales/Procs/Caller.sql" }),
        ),
        call(
            3,
            "converted_file",
            json!({ "tree": "tree-9", "path": "x.sql" }),
        ),
        call(
            4,
            "converted_file",
            json!({ "tree": "tree-1", "path": "x.sql" }),
        ),
        call(
            5,
            "convert_tree",
            json!({ "src": src, "to": "postgres", "statuses": ["converted"], "limit": 100 }),
        ),
    ]);
    assert_eq!(walk(&src), before);
    let s = result(&r[0]);
    assert_eq!(s["tree"], "tree-1");
    assert!(s["out"].is_null());
    assert_eq!(s["files"].as_array().unwrap().len(), 1);
    assert_eq!(s["omitted_files"], 2);
    let f = &s["files"][0];
    // max_units 0: every unit that is not plainly converted is counted.
    assert_eq!(
        f["omitted_units"].as_u64().unwrap() as usize,
        f["counts"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(k, _)| *k != "converted")
            .map(|(_, v)| v.as_u64().unwrap() as usize)
            .sum::<usize>()
    );
    let c = result(&r[1]);
    assert!(c["output"]
        .as_str()
        .unwrap()
        .contains("CALL Sales.NextOrder(5, v_id);"));
    assert_eq!(c["file"]["path"], "Sales/Procs/Caller.sql");
    assert_eq!(r[2]["result"]["isError"], true);
    assert_eq!(r[3]["result"]["isError"], true);
    let only = result(&r[4]);
    assert!(only["files"]
        .as_array()
        .unwrap()
        .iter()
        .all(|f| f["status"] == "converted"));
    assert_eq!(only["tree"], "tree-2");
}

#[test]
fn one_file_or_text_converts_like_a_tree_of_one() {
    let (_d, src) = tree("one");
    let orders = src.join("Sales/Tables/Orders.sql");
    let text = std::fs::read_to_string(&orders).unwrap();
    let r = session(&[
        call(
            1,
            "convert",
            json!({ "artifact": { "path": orders, "dialect": "tsql" }, "to": "postgres" }),
        ),
        call(
            2,
            "convert_tree",
            json!({ "src": orders, "to": "postgres" }),
        ),
        call(
            3,
            "convert",
            json!({ "artifact": { "text": text, "name": "Orders.sql", "dialect": "tsql" }, "to": "postgres" }),
        ),
        call(
            4,
            "convert",
            json!({ "artifact": { "text": "CREATE TABLE t (a int,);\n", "dialect": "tsql" }, "to": "db2i" }),
        ),
        call(
            5,
            "convert",
            json!({ "artifact": { "path": src, "dialect": "tsql" }, "to": "postgres" }),
        ),
        call(
            6,
            "convert",
            json!({ "artifact": { "text": "select 1", "dialect": "tsql" } }),
        ),
    ]);
    let one = result(&r[0]);
    let t = result(&r[1]);
    // Same file, same report entry, whichever tool.
    let mut tf = t["files"][0].clone();
    tf.as_object_mut().unwrap().remove("omitted_units");
    let mut of = one["file"].clone();
    for v in [&mut tf, &mut of] {
        v.as_object_mut().unwrap().remove("seconds");
    }
    // The tree listing counts plainly converted units; the file lists them.
    assert_eq!(
        tf.as_object_mut()
            .unwrap()
            .remove("converted_units")
            .unwrap(),
        of["converted_units"].as_array().unwrap().len()
    );
    of.as_object_mut().unwrap().remove("converted_units");
    assert_eq!(of, tf);
    let inline = result(&r[2]);
    assert_eq!(inline["output"], one["output"]);
    assert_eq!(inline["file"]["status"], one["file"]["status"]);

    let refused = result(&r[3]);
    assert_eq!(refused["file"]["status"], "not_converted");
    assert_eq!(refused["ok"], false);
    let reason = refused["file"]["units"][0]["reason"].as_str().unwrap();
    assert!(reason.contains("trailing comma"), "{reason}");
    assert!(refused["output"]
        .as_str()
        .unwrap()
        .contains("[not converted]"));

    let e = r[4]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(e.contains("convert_tree"), "{e}");
    assert_eq!(r[5]["result"]["isError"], true);
}

fn walk(d: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(d).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            v.extend(walk(&p));
        } else {
            v.push(p);
        }
    }
    v.sort();
    v
}

#[test]
fn the_queue_reads_the_tree_again_and_writes_nothing() {
    let (d, src) = tree("queue");
    let out = d.join("out");
    let r = session(&[call(
        1,
        "convert_tree",
        json!({ "src": src, "to": "postgres", "out": out }),
    )]);
    result(&r[0]);
    // convert_tree writes the queue next to the report, as the CLI does.
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("review-queue.json")).unwrap())
            .unwrap();
    let before = walk(&out);
    let r = session(&[
        call(
            2,
            "conversion_queue",
            json!({ "src": src, "to": "postgres", "out": out }),
        ),
        call(
            3,
            "conversion_queue",
            json!({ "src": src, "to": "postgres" }),
        ),
    ]);
    let q = result(&r[0]);
    assert_eq!(q["tiers"], written["tiers"]);
    assert_eq!(
        q["items"].as_array().unwrap().len() as u64 + q["omitted_items"].as_u64().unwrap(),
        written["items"].as_array().unwrap().len() as u64
    );
    let tiers: Vec<u64> = q["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["tier"].as_u64().unwrap())
        .collect();
    assert!(tiers.windows(2).all(|w| w[0] <= w[1]), "{tiers:?}");
    assert_eq!(walk(&out), before, "the queue writes nothing");
    assert_eq!(r[1]["result"]["isError"], true);
}

#[test]
fn an_agent_writes_a_unit_and_it_is_checked_and_waits_for_a_person() {
    let (d, src) = tree("write-unit");
    let out = d.join("out");
    let caller = "Sales/Procs/Caller.sql";
    let r = session(&[
        call(
            1,
            "convert_tree",
            json!({ "src": src, "to": "postgres", "out": out }),
        ),
        call(
            2,
            "conversion_queue",
            json!({ "src": src, "to": "postgres", "out": out, "all": true, "limit": 100 }),
        ),
    ]);
    result(&r[0]);
    let output = std::fs::read_to_string(out.join(caller)).unwrap();
    let (n, line) = output
        .lines()
        .enumerate()
        .find(|(_, l)| l.trim_start().starts_with("UPDATE"))
        .unwrap();
    let item = result(&r[1])["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["path"] == caller && i["out_line"] == n as u64 + 1)
        .unwrap_or_else(|| panic!("no item for line {} in {:#}", n + 1, result(&r[1])))
        .clone();
    // Same rows, written differently; `by` from the caller is ignored.
    let text = format!("{} WHERE 1 = 1;", line.trim_end().trim_end_matches(';'));
    let args = json!({
        "src": src, "to": "postgres", "out": out, "path": caller,
        "unit": item["unit"], "text": text, "reason": "a test", "by": "human",
    });
    let r = session(&[
        call(3, "write_unit", args.clone()),
        call(
            4,
            "conversion_queue",
            json!({ "src": src, "to": "postgres", "out": out, "all": true, "limit": 100 }),
        ),
        // The id is gone now: the unit has the text's id.
        call(5, "write_unit", args),
    ]);
    let w = result(&r[0]);
    assert_eq!(w["applied"], true, "{w:#}");
    assert_eq!(w["recorded"]["by"], "agent");
    assert_ne!(w["unit"], item["unit"]);
    // Checked like the rules' output, and never plainly converted.
    assert_ne!(w["status"], "converted", "{w:#}");
    assert!(
        w["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["rule"] == "ASM-HAND-WRITTEN"),
        "{w:#}"
    );
    assert!(std::fs::read_to_string(out.join(caller))
        .unwrap()
        .contains(&text));
    let now = result(&r[1])["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["unit"] == w["unit"])
        .unwrap()
        .clone();
    assert!(now["tier"].as_u64().unwrap() <= 3, "{now:#}");
    assert_eq!(r[2]["result"]["isError"], true, "{}", r[2]);

    // A file edited by hand since the last run refuses the write, and
    // nothing is recorded.
    let ledger = std::fs::read_to_string(out.join("review-decisions.jsonl")).unwrap();
    let f = out.join("Sales/Procs/NextOrder.sql");
    let t = std::fs::read_to_string(&f).unwrap();
    std::fs::write(&f, t + "-- edited\n").unwrap();
    let r = session(&[call(
        6,
        "write_unit",
        json!({
            "src": src, "to": "postgres", "out": out, "path": caller,
            "unit": w["unit"], "text": text, "reason": "again",
        }),
    )]);
    assert_eq!(r[0]["result"]["isError"], true, "{}", r[0]);
    assert!(r[0].to_string().contains("NextOrder.sql"), "{}", r[0]);
    assert_eq!(
        std::fs::read_to_string(out.join("review-decisions.jsonl")).unwrap(),
        ledger
    );
}
