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
