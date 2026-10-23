//! Drives the `sqldiff-mcp` binary over stdio like an MCP client would.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

fn fixture(name: &str) -> String {
    format!("{}/../../tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// Send each message and collect one response per request (not per notification).
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
        if m.get("id").is_some() {
            let mut line = String::new();
            out.read_line(&mut line).unwrap();
            responses.push(serde_json::from_str(&line).unwrap());
        }
    }
    drop(stdin);
    assert!(child.wait().unwrap().success());
    responses
}

fn call(id: u64, name: &str, args: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": args } })
}

#[test]
fn handshake_list_and_conversion_review() {
    let r = session(&[
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "2025-06-18", "capabilities": {},
                            "clientInfo": { "name": "test", "version": "0" } } }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        call(
            3,
            "diff",
            json!({
                "a": { "path": fixture("movestock.tsql.sql"), "dialect": "tsql" },
                "b": { "path": fixture("movestock.db2i.sql"), "dialect": "db2i" },
            }),
        ),
    ]);
    assert_eq!(r[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(r[0]["result"]["serverInfo"]["name"], "sqldiff");

    let names: Vec<&str> = r[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "diff",
            "check",
            "finding",
            "queue",
            "assumptions",
            "set_assumption",
            "comment_text",
            "open_in_gui"
        ]
    );

    let res = &r[2]["result"];
    assert_eq!(res["isError"], false);
    let s = &res["structuredContent"];
    assert_eq!(s["verdict"], "different");
    let cards = s["findings"].as_array().unwrap();
    assert!(cards.iter().any(|c| c["class"] == "transactional"));
    // Most severe first.
    assert_eq!(cards[0]["severity"], "critical");
    assert!(res["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Verdict: DIFFERENT"));
}

#[test]
fn reformat_is_equal_and_finding_expands() {
    let reformatted = session(&[call(
        1,
        "diff",
        json!({
            "a": { "path": fixture("movestock.tsql.sql"), "dialect": "tsql" },
            "b": { "path": fixture("movestock_reformatted.tsql.sql"), "dialect": "tsql" },
        }),
    )]);
    let s = &reformatted[0]["result"]["structuredContent"];
    assert!(matches!(
        s["verdict"].as_str(),
        Some("equal" | "equal_under_assumptions")
    ));
    assert_eq!(s["finding_count"], 0);

    // Expand a card from an earlier call in the same session; inline text works too.
    let text = |f: &str| std::fs::read_to_string(fixture(f)).unwrap();
    let r = session(&[call(
        1,
        "diff",
        json!({
            "a": { "text": text("movestock.tsql.sql"), "dialect": "tsql", "name": "movestock.tsql.sql" },
            "b": { "text": text("movestock.pg.sql"), "dialect": "postgres", "name": "movestock.pg.sql" },
            "max_findings": 1,
        }),
    )]);
    let s = &r[0]["result"]["structuredContent"];
    assert_eq!(s["findings"].as_array().unwrap().len(), 1);
    let fp = s["findings"][0]["fingerprint"].clone();
    let r = session(&[
        call(
            1,
            "diff",
            json!({
                "a": { "text": text("movestock.tsql.sql"), "dialect": "tsql", "name": "movestock.tsql.sql" },
                "b": { "text": text("movestock.pg.sql"), "dialect": "postgres", "name": "movestock.pg.sql" },
            }),
        ),
        call(2, "finding", json!({ "fingerprint": fp })),
        call(3, "finding", json!({ "fingerprint": "nope" })),
    ]);
    assert_eq!(
        r[1]["result"]["structuredContent"]["finding"]["fingerprint"],
        fp
    );
    assert_eq!(r[2]["result"]["isError"], true);
}

#[test]
fn check_and_errors() {
    let r = session(&[
        call(
            1,
            "check",
            json!({ "artifact": { "path": fixture("catch_rethrow.tsql.sql"), "dialect": "tsql" } }),
        ),
        call(
            2,
            "check",
            json!({ "artifact": { "path": "x.sql", "text": "x", "dialect": "tsql" } }),
        ),
        call(
            3,
            "check",
            json!({ "artifact": { "text": "select 1", "dialect": "oracle" } }),
        ),
        json!({ "jsonrpc": "2.0", "id": 4, "method": "no/such" }),
        call(5, "nope", json!({})),
    ]);
    let s = &r[0]["result"]["structuredContent"];
    assert!(s["findings"].as_array().unwrap().iter().any(|c| c["rule"]
        .as_str()
        .unwrap()
        .contains("terminator")
        || c["witness"].as_str().unwrap().contains("THROW")));
    assert_eq!(r[1]["result"]["isError"], true);
    assert_eq!(r[2]["result"]["isError"], true);
    assert_eq!(r[3]["error"]["code"], -32601);
    assert_eq!(r[4]["error"]["code"], -32602);
}

#[test]
fn parse_error_and_ping() {
    let mut server = sqldiff_mcp::Server::new();
    let out: Value = serde_json::from_str(&server.handle_line("{not json").unwrap()).unwrap();
    assert_eq!(out["error"]["code"], -32700);
    let out: Value = serde_json::from_str(
        &server
            .handle_line(r#"{"jsonrpc":"2.0","id":"p","method":"ping"}"#)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(out["result"], json!({}));
    assert!(server
        .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#)
        .is_none());
}

fn text(r: &Value) -> &str {
    r["result"]["content"][0]["text"].as_str().unwrap()
}

#[test]
fn queue_flip_comment_and_link() {
    let fixtures = fixture("");
    let r = session(&[
        call(1, "queue", json!({ "source": fixtures })),
        call(
            2,
            "queue",
            json!({ "source": fixtures, "min_severity": "loud" }),
        ),
        call(
            3,
            "queue",
            json!({ "source": fixtures, "classes": ["semantic"], "limit": 1 }),
        ),
    ]);
    let filtered = &r[2]["result"]["structuredContent"];
    assert_eq!(filtered["pairs"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["omitted_pairs"], 2);
    let s = &r[0]["result"]["structuredContent"];
    let pairs = s["pairs"].as_array().unwrap();
    assert_eq!(pairs.len(), 3);
    // Most severe first; the T-SQL ↔ PG pair hinges on one assumption.
    assert_eq!(pairs[0]["highest_severity"], "critical");
    let pg = pairs
        .iter()
        .find(|p| p["b"] == "movestock.pg.sql" && p["a"] == "movestock.tsql.sql")
        .unwrap();
    assert_eq!(pg["verdict_flipping"], json!(["ASM-CALLER-CHECKS-RC"]));
    assert_eq!(pg["review"]["unreviewed"], 2);
    assert_eq!(r[1]["result"]["isError"], true);

    let pid = pg["id"].as_str().unwrap();
    let r = session(&[
        call(1, "queue", json!({ "source": fixtures })),
        call(2, "assumptions", json!({ "pair": pid })),
        call(
            3,
            "set_assumption",
            json!({ "id": "ASM-CALLER-CHECKS-RC", "holds": false }),
        ),
        call(
            4,
            "queue",
            json!({ "source": fixtures, "verdicts": ["unknown"] }),
        ),
        call(
            5,
            "set_assumption",
            json!({ "id": "ASM-CALLER-CHECKS-RC", "holds": null }),
        ),
        call(6, "assumptions", json!({ "pair": pid })),
    ]);
    let ledger = &r[1]["result"]["structuredContent"]["assumptions"];
    assert_eq!(ledger[0]["id"], "ASM-CALLER-CHECKS-RC");
    assert_eq!(ledger[0]["status"], "open");
    assert_eq!(ledger[0]["flips_verdict"], true);
    assert_eq!(ledger[0]["verdict_if_flipped"], "unknown");
    let changed = &r[2]["result"]["structuredContent"]["changed"];
    assert!(changed
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["pair_id"] == pid && c["from"] == "different" && c["to"] == "unknown"));
    let unknown = r[3]["result"]["structuredContent"]["pairs"]
        .as_array()
        .unwrap();
    assert_eq!(unknown.len(), 1);
    assert_eq!(unknown[0]["id"], pid);
    assert_eq!(r[5]["result"]["structuredContent"]["verdict"], "different");

    // Comment text is deterministic and cites the finding; the link selects it.
    let ask = |id| {
        session(&[
            call(1, "queue", json!({ "source": fixtures })),
            call(2, "finding", json!({ "fingerprint": "06ccfd05e423182d" })),
            call(
                3,
                "comment_text",
                json!({ "finding_ids": ["06ccfd05e423182d"] }),
            ),
            call(
                4,
                "open_in_gui",
                json!({ "finding_id": "06ccfd05e423182d", "gui_url": "http://h:1" }),
            ),
            call(id, "comment_text", json!({ "finding_ids": ["nope"] })),
        ])
    };
    let (x, y) = (ask(5), ask(5));
    assert_eq!(text(&x[2]), text(&y[2]));
    assert!(text(&x[2]).starts_with("SQLDiff found 1 issue to review."));
    assert!(text(&x[2]).contains("(sqldiff 06ccfd05e423182d)"));
    let url = x[3]["result"]["structuredContent"]["url"].as_str().unwrap();
    assert!(url.starts_with("http://h:1/?a="), "{url}");
    assert!(url.ends_with("#f=06ccfd05e423182d"), "{url}");
    assert!(x[3]["result"]["structuredContent"]["command"]
        .as_str()
        .unwrap()
        .starts_with("sqldiff-gui serve --dialect-a tsql"));
    assert_eq!(x[4]["result"]["isError"], true);
}

#[test]
fn inline_text_has_no_gui_link() {
    let r = session(&[call(
        1,
        "diff",
        json!({ "a": { "text": "select 1;", "dialect": "tsql" }, "b": { "text": "select 2;", "dialect": "tsql" } }),
    )]);
    let pid = r[0]["result"]["structuredContent"]["pair_id"].clone();
    let r = session(&[
        call(
            1,
            "diff",
            json!({ "a": { "text": "select 1;", "dialect": "tsql" }, "b": { "text": "select 2;", "dialect": "tsql" } }),
        ),
        call(2, "open_in_gui", json!({ "pair": pid })),
    ]);
    assert_eq!(r[1]["result"]["isError"], true);
}

#[test]
fn inline_pairs_do_not_replace_each_other() {
    let inline = |t: &str| json!({ "text": t, "dialect": "tsql" });
    let r = session(&[
        call(
            1,
            "diff",
            json!({ "a": inline("select 1;"), "b": inline("select 2;\nTHROW 50000, 'x', 1") }),
        ),
        call(
            2,
            "diff",
            json!({ "a": inline("select 3;"), "b": inline("select 4;\nTHROW 50000, 'y', 1") }),
        ),
    ]);
    let (p1, p2) = (
        &r[0]["result"]["structuredContent"]["pair_id"],
        &r[1]["result"]["structuredContent"]["pair_id"],
    );
    assert_ne!(p1, p2);
    let r = session(&[
        call(
            1,
            "diff",
            json!({ "a": inline("select 1;"), "b": inline("select 2;\nTHROW 50000, 'x', 1") }),
        ),
        call(
            2,
            "diff",
            json!({ "a": inline("select 3;"), "b": inline("select 4;\nTHROW 50000, 'y', 1") }),
        ),
        call(3, "assumptions", json!({ "pair": p1 })),
    ]);
    assert_eq!(r[2]["result"]["isError"], false, "{}", text(&r[2]));
}

#[test]
fn single_file_check_has_no_gui_pair() {
    let r = session(&[call(
        1,
        "check",
        json!({ "artifact": { "path": fixture("catch_rethrow.tsql.sql"), "dialect": "tsql" } }),
    )]);
    let pid = r[0]["result"]["structuredContent"]["pair_id"].clone();
    let r = session(&[
        call(
            1,
            "check",
            json!({ "artifact": { "path": fixture("catch_rethrow.tsql.sql"), "dialect": "tsql" } }),
        ),
        call(2, "open_in_gui", json!({ "pair": pid })),
    ]);
    assert_eq!(r[1]["result"]["isError"], true);
    assert!(text(&r[1]).contains("single-file"));
}
