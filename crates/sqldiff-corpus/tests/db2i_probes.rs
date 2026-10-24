//! The Db2 for i probes (probes/db2i): the probe SQL parses as Db2 for i, and the
//! text that hedges on an unchecked fact follows the fact's status from real runs.
//!
//! The parser is lenient inside statements, so the parse tests catch broken block
//! structure (BEGIN/END, IF/END IF, handlers, terminators), not every grammar error.
//! Only a run on IBM i proves the SQL itself.

use sqldiff_syntax::{parse, Dialect, LineIndex};
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn toml_file(rel: &str) -> toml::Value {
    read(rel).parse().unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn str_of<'a>(v: &'a toml::Value, key: &str) -> &'a str {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("")
}

/// Parse problems in `text` as Db2 for i, with line numbers.
fn problems(text: &str) -> Vec<String> {
    let p = parse(text, Dialect::Db2i);
    let idx = LineIndex::new(text);
    p.diagnostics
        .iter()
        .map(|d| {
            let (line, col) = idx.line_col(d.offset);
            format!("{line}:{col} {}", d.message)
        })
        .collect()
}

#[test]
fn probe_procedures_parse_as_db2_for_i() {
    let defs = toml_file("probes/db2i/probes.toml");
    let mut bad = Vec::new();
    for p in defs["procedure"].as_array().unwrap() {
        let params = str_of(p, "params");
        let out = "OUT O_STATE CHAR(5), OUT O_CODE INTEGER, OUT O_N1 INTEGER, OUT O_N2 INTEGER, OUT O_TEXT VARCHAR(200)";
        let plist = if params.is_empty() {
            out.to_string()
        } else {
            format!("{params}, {out}")
        };
        let commit = str_of(p, "commit");
        let option = if commit.is_empty() {
            String::new()
        } else {
            format!("SET OPTION COMMIT = {commit}\n")
        };
        let text = format!(
            "CREATE OR REPLACE PROCEDURE SQLDFPROBE.P_{} ({plist})\nLANGUAGE SQL\nMODIFIES SQL DATA\n{option}{};\n",
            str_of(p, "name"),
            str_of(p, "body").trim()
        );
        for e in problems(&text) {
            bad.push(format!("P_{}: {e}", str_of(p, "name")));
        }
    }
    for pr in defs["probe"].as_array().unwrap() {
        if let Some(body) = pr.get("inline").and_then(|b| b.as_str()) {
            let text = format!(
                "BEGIN\nDECLARE O_STATE CHAR(5);\nDECLARE O_CODE INTEGER;\nDECLARE O_N1 INTEGER;\nDECLARE O_N2 INTEGER;\nDECLARE O_TEXT VARCHAR(200);\n{body}END;\n"
            );
            for e in problems(&text) {
                bad.push(format!("{}: {e}", str_of(pr, "id")));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "probe SQL does not parse:\n{}",
        bad.join("\n")
    );
}

#[test]
fn generated_script_parses_as_db2_for_i() {
    let bad = problems(&read("probes/db2i/sqldiff-db2i-probes.sql"));
    assert!(
        bad.is_empty(),
        "sqldiff-db2i-probes.sql:\n{}",
        bad.join("\n")
    );
}

const RESOLVED: [&str; 3] = ["confirmed", "corrected", "contradicted"];

/// Hedges stay while a fact is open and go once a real run has answered it.
#[test]
fn hedges_follow_the_fact_status() {
    let facts = toml_file("probes/db2i/facts.toml");
    let status: serde_json::Value =
        serde_json::from_str(&read("probes/db2i/status.json")).expect("status.json");
    let status_of = |id: &str| -> String {
        status["facts"][id]["status"]
            .as_str()
            .unwrap_or_else(|| panic!("status.json has no fact {id}"))
            .to_string()
    };
    let vendor = read("docs/vendor-facts-verification.md");
    let mut bad = Vec::new();

    for f in facts["fact"].as_array().unwrap() {
        let id = str_of(f, "id");
        let st = status_of(id);
        if RESOLVED.contains(&st.as_str()) {
            let row = vendor
                .lines()
                .find(|l| l.starts_with(&format!("| {id} |")))
                .unwrap_or_else(|| panic!("no summary row for {id}"));
            if !row.contains("probe") {
                bad.push(format!(
                    "{id} is {st}: its summary row in vendor-facts-verification.md must cite the probe run"
                ));
            }
        }
    }

    for link in facts["link"].as_array().unwrap() {
        let file = str_of(link, "file");
        let anchor = str_of(link, "anchor");
        let hedge = str_of(link, "hedge");
        let ids: Vec<&str> = link["facts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let resolved = ids
            .iter()
            .all(|id| RESOLVED.contains(&status_of(id).as_str()));
        let text = read(file);
        let lines: Vec<&str> = text.lines().collect();
        let hits: Vec<usize> = (0..lines.len())
            .filter(|&i| lines[i].contains(anchor))
            .collect();
        if hits.is_empty() {
            bad.push(format!(
                "{file}: anchor {anchor:?} not found; update probes/db2i/facts.toml"
            ));
        }
        for i in hits {
            let window = lines[i..(i + 3).min(lines.len())].join("\n");
            let has = window.contains(hedge);
            if !resolved && !has {
                bad.push(format!(
                    "{file}:{}: {ids:?} not settled on a real IBM i, so the text at {anchor:?} must keep {hedge:?}",
                    i + 1
                ));
            }
            if resolved && has {
                bad.push(format!(
                    "{file}:{}: {ids:?} answered by a probe run (see probes/db2i/status.json): rewrite the text at {anchor:?} from the evidence and drop {hedge:?}",
                    i + 1
                ));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
