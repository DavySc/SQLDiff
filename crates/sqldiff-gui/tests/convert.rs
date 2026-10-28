use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use sqldiff_core::Dialect;
use sqldiff_gui::convert::{page, run, serve, Setup, TemplateSetup, SAMPLE};
use sqldiff_source::compose::Placeholders;
use sqldiff_source::config::{self, Sources};
use sqldiff_source::Resolved;

fn setup(text: &str) -> Setup {
    Setup {
        from: Dialect::Tsql,
        to: Dialect::Postgres,
        template: None,
        text: text.to_string(),
        name: "sample.sql".into(),
    }
}

#[test]
fn sample_converts_and_parses() {
    let p = run(&setup(SAMPLE), SAMPLE, false);
    let c = p.conversion.expect("supported pair");
    assert!(c.output.contains("CREATE OR REPLACE PROCEDURE MoveStock"));
    assert!(sqldiff_syntax::parse(&c.output, Dialect::Postgres)
        .diagnostics
        .is_empty());
    assert_eq!(c.counts.not_converted, 0, "{}", c.output);
}

#[test]
fn page_embeds_the_payload_safely() {
    let text = "SELECT '</script>' AS x;";
    let html = page(&run(&setup(text), text, false));
    assert!(!html.contains("/*__SQLDIFF_PAYLOAD__*/null"));
    assert!(!html.contains("'</script>'"));
}

#[test]
fn a_pair_without_rules_shows_statements_as_not_converted() {
    let s = Setup {
        from: Dialect::Db2i,
        ..setup("SELECT 1")
    };
    let p = run(&s, "SELECT 1;", false);
    assert!(!p.rules);
    let c = p.conversion.expect("a conversion, not an error");
    assert_eq!(c.counts.not_converted, 1);
    assert!(p.with_rules.contains(&("tsql", "postgres")));
}

fn post(addr: std::net::SocketAddr, path: &str, body: &serde_json::Value) -> serde_json::Value {
    let body = body.to_string();
    let mut s = TcpStream::connect(addr).unwrap();
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    serde_json::from_str(resp.split("\r\n\r\n").nth(1).unwrap()).unwrap()
}

#[test]
fn the_target_and_the_mode_are_chosen_per_request() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || serve(listener, setup("SELECT 1;"), &[]));
    // A pair without rules yet: every statement not converted.
    let v = post(
        addr,
        "/api/convert",
        &serde_json::json!({ "text": "SELECT 1;\nSELECT 2;", "from": "postgres", "to": "db2i" }),
    );
    assert_eq!(v["to"], "db2i");
    assert_eq!(v["rules"], false);
    assert_eq!(v["conversion"]["counts"]["not_converted"], 2);
    // Compare: the engine's diff of two scratch texts in any two dialects.
    let v = post(
        addr,
        "/api/compare",
        &serde_json::json!({ "a": "UPDATE t SET a = 1 WHERE id = 2;", "b": "UPDATE t SET a = 2 WHERE id = 2;", "from": "tsql", "to": "postgres" }),
    );
    assert_ne!(v["report"]["verdict"], "equal", "{v}");
    assert!(!v["report"]["findings"].as_array().unwrap().is_empty());
}

#[test]
fn server_converts_posted_text() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || serve(listener, setup("SELECT 1;"), &[]));
    let body = r#"{"text":"SELECT TOP 3 a FROM dbo.t;"}"#;
    let mut s = TcpStream::connect(addr).unwrap();
    write!(
        s,
        "POST /api/convert HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    let json = resp.split("\r\n\r\n").nth(1).unwrap();
    let v: serde_json::Value = serde_json::from_str(json).unwrap();
    assert_eq!(v["conversion"]["output"], "SELECT a FROM t\nLIMIT 3;\n");

    let mut s = TcpStream::connect(addr).unwrap();
    write!(s, "GET / HTTP/1.1\r\nHost: {addr}\r\n\r\n").unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    assert!(resp.contains("SQLDiff convert"));
}

#[test]
fn cross_origin_posts_are_refused() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || serve(listener, setup("SELECT 1;"), &[]));
    let body = r#"{"text":"SELECT 1;"}"#;
    let post = |origin: &str| {
        let mut s = TcpStream::connect(addr).unwrap();
        write!(
            s,
            "POST /api/convert HTTP/1.1\r\nHost: {addr}\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut resp = String::new();
        s.read_to_string(&mut resp).unwrap();
        resp
    };
    assert!(post("https://evil.example").starts_with("HTTP/1.1 403"));
    assert!(post(&format!("http://{addr}")).starts_with("HTTP/1.1 200"));
}

fn root(p: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(p)
}

#[test]
fn the_example_template_converts_with_the_example_pack() {
    let cfg = Resolved::load(&Sources {
        project: None,
        user: None,
        isolated: true,
        cwd: None,
        packs: vec![root("docs/examples/pack")],
        cli: config::cli_layer(&[], &[], None).unwrap(),
    })
    .unwrap();
    let text =
        std::fs::read_to_string(root("docs/examples/templates/save-entity.tpl.sql")).unwrap();
    let s = Setup {
        template: Some(TemplateSetup {
            placeholders: Placeholders::new(cfg.policy.template.as_ref().unwrap()).unwrap(),
            parameters: None,
            packs: sqldiff_gui::packs::loaded(&cfg),
        }),
        ..setup(&text)
    };
    let p = run(&s, &text, false);
    // The page confirms the `--pack` it loaded, a private pack, by path only.
    let packs = p
        .template_packs
        .as_ref()
        .expect("template mode lists its packs");
    assert_eq!(packs.len(), 1);
    assert!(packs[0].private && packs[0].name.is_none() && packs[0].version.is_none());
    assert!(packs[0].path.ends_with("pack.toml"), "{}", packs[0].path);
    assert!(run(&setup(&text), &text, false).template_packs.is_none());
    let c = p.conversion.expect("converts");
    let info = c.template.as_ref().unwrap();
    assert_eq!(info.parameters, ["NAME", "SCHEMA", "ENTITY", "TABLE"]);
    assert_eq!(info.special_forms, ["include"]);
    for want in [
        "<%@include header.sql %>",
        "CREATE OR REPLACE PROCEDURE <%SCHEMA%>.Save<%ENTITY%>(",
        "UPDATE <%SCHEMA%>.<%TABLE%>",
        "INSERT INTO <%SCHEMA%>.<%TABLE%>_log",
    ] {
        assert!(c.output.contains(want), "{want}:\n{}", c.output);
    }
    assert_eq!(c.counts.not_converted, 0, "{}", c.output);
    assert!(
        !c.output.to_ascii_lowercase().contains("tpl_"),
        "{}",
        c.output
    );
}
