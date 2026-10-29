//! The settings page's server side: layers and where each key comes from,
//! the preview, saving into the workspace `sqldiff.toml`, and what stays
//! out of reach (private config, other files, other origins).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;

use serde_json::{json, Value};
use sqldiff_core::format;
use sqldiff_core::Dialect;
use sqldiff_gui::app::{serve, App};
use sqldiff_gui::convert::{Setup, SAMPLE};
use sqldiff_gui::settings::KEYS;
use sqldiff_source::config::Sources;
use sqldiff_source::Resolved;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let d = std::env::temp_dir().join(format!("sqldiff-gui-set-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    /// `rel` (written with `/`) below the folder, joined by component so the
    /// path has the separators the server prints on every platform.
    fn p(&self, rel: &str) -> PathBuf {
        rel.split('/').fold(self.0.clone(), |p, c| p.join(c))
    }

    fn write(&self, rel: &str, text: &str) -> PathBuf {
        let p = self.p(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        p
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.p(rel)).unwrap()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn app(isolated: bool) -> App {
    App {
        convert: Setup {
            from: Dialect::Tsql,
            to: Dialect::Postgres,
            template: None,
            text: SAMPLE.to_string(),
            name: String::new(),
        },
        compare: None,
        review: None,
        data_dir: None,
        isolated,
        packs: Vec::new(),
    }
}

fn start(isolated: bool) -> SocketAddr {
    start_with(app(isolated))
}

fn start_with(app: App) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || serve(listener, app, &[]));
    addr
}

fn request(addr: SocketAddr, method: &str, path: &str, body: &str) -> (String, String) {
    let mut s = TcpStream::connect(addr).unwrap();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();
    let (head, body) = resp.split_once("\r\n\r\n").unwrap();
    let status = head.lines().next().unwrap();
    (status["HTTP/1.1 ".len()..].to_string(), body.to_string())
}

/// The token the settings page was served with.
fn token(addr: SocketAddr) -> String {
    let (status, page) = request(addr, "GET", "/settings", "");
    assert_eq!(status, "200 OK");
    let at = page.find(r#""token":""#).expect("token in the page") + 9;
    page[at..at + page[at..].find('"').unwrap()].to_string()
}

/// `p` as a query value: everything but unreserved characters percent-encoded
/// (Windows paths have backslashes, colons and often spaces).
fn encode(p: &std::path::Path) -> String {
    p.display()
        .to_string()
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b => format!("%{b:02X}"),
        })
        .collect()
}

fn settings(addr: SocketAddr, dir: &std::path::Path) -> (String, Value) {
    let (status, body) = request(
        addr,
        "GET",
        &format!("/api/settings?dir={}", encode(dir)),
        "",
    );
    let v = serde_json::from_str(&body).unwrap_or(Value::String(body));
    (status, v)
}

fn post(addr: SocketAddr, path: &str, mut body: Value) -> (String, Value) {
    body["token"] = json!(token(addr));
    let (status, text) = request(addr, "POST", path, &body.to_string());
    let v = serde_json::from_str(&text).unwrap_or(Value::String(text));
    (status, v)
}

const WORKSPACE: &str = "# Our SQL layout.
[format]
keyword_case = \"lower\"   # we like it quiet
indent = 2

[format.postgres]
one_item_per_line = true
";

const SHARED_PACK: &str = "[pack]
name = \"team-style\"
version = \"1\"

[format]
indent = 3
max_blank_lines = 0

[format.tsql]
keyword_case = \"upper\"
";

const QUERY: &str = "select a,b from t where a=1 and b=2;\n";

#[test]
fn key_table_lists_every_format_key_in_the_documented_order() {
    let names: Vec<_> = KEYS.iter().map(|k| k.name).collect();
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/formatter.md"
    ))
    .unwrap();
    let block = doc
        .split("```toml")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    let documented: Vec<_> = block
        .lines()
        .filter(|l| !l.starts_with('['))
        .filter_map(|l| l.split(" = ").next())
        .filter(|k| {
            !k.is_empty() && !k.starts_with('#') && !k.starts_with("keyword_case = \"lower\"")
        })
        .collect();
    // The documented block lists each key once, then `[format.postgres]`'s example.
    assert_eq!(names, documented[..names.len()]);
    let all = sqldiff_core::format::FormatKeys::from_options(&Default::default()).names();
    let mut sorted = names.clone();
    sorted.sort();
    let mut all: Vec<_> = all.iter().map(String::as_str).collect();
    all.sort();
    assert_eq!(sorted, all);
}

#[test]
fn the_page_carries_the_token_and_sits_in_the_rail() {
    let addr = start(true);
    let (status, html) = request(addr, "GET", "/settings", "");
    assert_eq!(status, "200 OK");
    assert!(
        !html.contains("<!--__SQLDIFF_SHELL__-->") && !html.contains("<!--__SQLDIFF_PICKER__-->")
    );
    assert!(html.contains(r#"<a class="on" href="/settings""#), "{html}");
    let t = token(addr);
    assert_eq!(t.len(), 32);
    // The same launch gives the same token; another launch another one.
    assert_eq!(t, token(addr));
    assert_ne!(t, token(start(true)));
}

#[test]
fn layers_and_the_source_of_each_key() {
    let d = Dir::new("layers");
    d.write("packs/team.toml", SHARED_PACK);
    d.write(
        "ws/sqldiff.toml",
        &format!("packs = [\"../packs/team.toml\"]\n{WORKSPACE}"),
    );
    let addr = start(true);
    let (status, v) = settings(addr, &d.0.join("ws"));
    assert_eq!(status, "200 OK", "{v}");
    let kinds: Vec<_> = v["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["builtin", "shared_pack", "project"]);
    assert_eq!(v["layers"][1]["label"], "Pack team-style");
    assert_eq!(v["layers"][1]["format_keys"], 3);
    assert_eq!(
        v["layers"][2]["path"],
        d.p("ws/sqldiff.toml").display().to_string()
    );
    assert_eq!(v["exists"], true);
    assert_eq!(v["draft"]["indent"], 2);
    assert_eq!(v["draft"]["postgres"]["one_item_per_line"], true);
    let e = &v["effective"];
    // The workspace outranks the pack for [format] keys ...
    assert_eq!(
        e["all"]["indent"],
        json!({"value": 2, "hidden": false, "layer": 2, "dialect_table": false})
    );
    // ... the pack's own max_blank_lines shows through ...
    assert_eq!(e["all"]["max_blank_lines"]["value"], 0);
    assert_eq!(e["all"]["max_blank_lines"]["layer"], 1);
    // ... a built-in default has layer 0 ...
    assert_eq!(
        e["all"]["final_newline"],
        json!({"value": true, "hidden": false, "layer": 0, "dialect_table": false})
    );
    // ... and the pack's [format.tsql] beats the workspace's [format] for T-SQL only.
    assert_eq!(
        e["tsql"]["keyword_case"],
        json!({"value": "upper", "hidden": false, "layer": 1, "dialect_table": true})
    );
    assert_eq!(e["all"]["keyword_case"]["value"], "lower");
    assert_eq!(e["postgres"]["keyword_case"]["value"], "lower");
    assert_eq!(e["postgres"]["one_item_per_line"]["layer"], 2);
    assert_eq!(e["postgres"]["one_item_per_line"]["dialect_table"], true);
    // Each effective value is what the library computes for the dialect.
    let cfg = Resolved::load(&Sources {
        isolated: true,
        project: Some(d.p("ws/sqldiff.toml")),
        ..Sources::default()
    })
    .unwrap();
    for dialect in Dialect::ALL {
        let o = serde_json::to_value(format::FormatKeys::from_options(
            &cfg.policy.format.options(dialect),
        ))
        .unwrap();
        for k in KEYS {
            assert_eq!(
                e[dialect.id()][k.name]["value"],
                o[k.name],
                "{dialect:?} {}",
                k.name
            );
        }
    }
}

#[test]
fn a_folder_without_a_config_starts_from_the_defaults() {
    let d = Dir::new("empty");
    // A config in a folder above is not this workspace's.
    d.write("sqldiff.toml", WORKSPACE);
    std::fs::create_dir_all(d.0.join("deeper")).unwrap();
    let (status, v) = settings(start(true), &d.0.join("deeper"));
    assert_eq!(status, "200 OK", "{v}");
    assert_eq!(v["exists"], false);
    assert_eq!(v["hash"], Value::Null);
    assert_eq!(v["draft"], json!({}));
    assert_eq!(v["layers"][1]["kind"], "project");
    assert_eq!(v["effective"]["all"]["indent"]["layer"], 0);
}

#[test]
fn private_config_is_shown_by_path_and_never_read_into_the_page() {
    let d = Dir::new("private");
    d.write(
        "secret/pack.toml",
        "deny_list = [\"zz-secret-term\"]\n\n[pack]\nname = \"zz-private-name\"\nversion = \"1\"\n\n[format]\nindent = 7\nkeyword_case = \"preserve\"\n",
    );
    d.write(
        "ws/sqldiff.toml",
        "private_packs = [\"../secret/pack.toml\"]\n[format]\nindent = 2\n",
    );
    let addr = start(true);
    let (status, v) = settings(addr, &d.0.join("ws"));
    assert_eq!(status, "200 OK", "{v}");
    let body = v.to_string();
    for leak in [
        "zz-private-name",
        "zz-secret-term",
        "\"preserve\"",
        ": 7",
        ":7",
    ] {
        assert!(!body.contains(leak), "{leak} reached the page: {body}");
    }
    let l = &v["layers"][1];
    assert_eq!(l["kind"], "private_pack");
    assert_eq!(l["shown"], false);
    assert_eq!(l["format_keys"], Value::Null);
    assert_eq!(l["path"], d.p("secret/pack.toml").display().to_string());
    // The key it sets is marked as set by it, with no value.
    assert_eq!(
        v["effective"]["all"]["keyword_case"],
        json!({"value": null, "hidden": true, "layer": 1, "dialect_table": false})
    );
    // The preview still formats with the real values: the pack keeps the case
    // (the default would upper-case), and its indent 7 loses to the draft's.
    let (status, p) = post(
        addr,
        "/api/settings/preview",
        json!({"dir": d.0.join("ws"), "draft": {"indent": 2}, "dialect": "tsql", "text": "select a from t;\n"}),
    );
    assert_eq!(status, "200 OK", "{p}");
    assert_eq!(p["text"], "select a\nfrom t;\n");
    assert!(!p.to_string().contains("zz-"));
    // An error in a private pack is not passed on: it can quote the pack.
    d.write("secret/pack.toml", "zz-secret-line = = broken\n");
    let (status, v) = settings(addr, &d.0.join("ws"));
    assert_eq!(status, "400 Bad Request");
    assert!(!v.to_string().contains("zz-secret"), "{v}");
    // An error in the workspace file is shown: it is the user's own.
    d.write("secret/pack.toml", "[pack]\nname=\"x\"\nversion=\"1\"\n");
    d.write("ws/sqldiff.toml", "[format\nindent = 2\n");
    let (status, v) = settings(addr, &d.0.join("ws"));
    assert_eq!(status, "400 Bad Request");
    assert!(v.to_string().contains("sqldiff.toml"), "{v}");
}

/// Set in the child process that runs the user-config scenario.
const CHILD: &str = "SQLDIFF_SETTINGS_TEST_DIR";

#[test]
fn the_user_config_is_path_only_and_is_never_a_save_target() {
    // The user config is found through the environment, which a test cannot
    // change for itself while the other tests read it: run in a child process.
    if let Some(dir) = std::env::var_os(CHILD) {
        return user_config_scenario(&Dir(PathBuf::from(dir)));
    }
    let d = Dir::new("user");
    let cfg = d.write("home/sqldiff.toml", USER_CONFIG);
    d.write("ws/sqldiff.toml", "[format]\nindent = 2\n");
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "the_user_config_is_path_only_and_is_never_a_save_target",
            "--test-threads=1",
        ])
        .env(CHILD, &d.0)
        .env("SQLDIFF_CONFIG", &cfg)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // The child really ran the scenario (a filtered-out test also succeeds).
    assert!(String::from_utf8_lossy(&out.stdout).contains("1 passed"));
}

const USER_CONFIG: &str = "[format]\nkeyword_case = \"preserve\"\nindent = 9\n";

fn user_config_scenario(d: &Dir) {
    let cfg = d.p("home/sqldiff.toml");
    // Not isolated: the user config is read, here named by SQLDIFF_CONFIG.
    let addr = start(false);
    let (status, v) = settings(addr, &d.0.join("ws"));
    assert_eq!(status, "200 OK", "{v}");
    let kinds: Vec<_> = v["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["builtin", "project", "user"]);
    let user = &v["layers"][2];
    assert_eq!(user["shown"], false);
    assert_eq!(user["path"], cfg.display().to_string());
    // It outranks the workspace, so the workspace's indent is not in effect.
    assert_eq!(
        v["effective"]["all"]["indent"],
        json!({"value": null, "hidden": true, "layer": 2, "dialect_table": false})
    );
    assert!(!v.to_string().contains("preserve"), "{v}");
    // As the workspace, the same file is neither shown nor saved to.
    let (status, v) = settings(addr, &d.0.join("home"));
    assert_eq!(status, "400 Bad Request", "{v}");
    assert!(!v.to_string().contains("preserve"), "{v}");
    let (status, body) = post(
        addr,
        "/api/settings/save",
        json!({"dir": d.0.join("home"), "draft": {"indent": 4}, "hash": null}),
    );
    assert_eq!(status, "400 Bad Request", "{body}");
    assert!(!body.to_string().contains("preserve"), "{body}");
    assert_eq!(d.read("home/sqldiff.toml"), USER_CONFIG);
    // Nor is a private pack the user config loads that is itself a
    // `sqldiff.toml`.
    d.write(
        "packdir/sqldiff.toml",
        "[pack]\nname = \"zz-private-file\"\nversion = \"1\"\n[format]\nindent = 5\n",
    );
    d.write(
        "home/sqldiff.toml",
        "private_packs = [\"../packdir/sqldiff.toml\"]\n",
    );
    let (status, v) = settings(addr, &d.0.join("packdir"));
    assert_eq!(status, "400 Bad Request", "{v}");
    assert!(!v.to_string().contains("zz-"), "{v}");
    // When the other layers cannot be loaded, the file may still be one of
    // their packs, so it is not read either.
    d.write(
        "home/sqldiff.toml",
        "private_packs = [\"../packdir/sqldiff.toml\", \"../missing.toml\"]\n",
    );
    let (status, v) = settings(addr, &d.0.join("packdir"));
    assert_eq!(status, "400 Bad Request", "{v}");
    assert!(!v.to_string().contains("zz-"), "{v}");
    assert!(!v.to_string().contains("unknown field"), "{v}");
}

#[test]
fn a_launch_pack_is_never_the_workspace_file() {
    let d = Dir::new("launch-pack");
    let pack = "[pack]\nname = \"zz-launch-name\"\nversion = \"1\"\n\n[format]\nindent = 6\nkeyword_case = \"preserve\"\n";
    d.write("ws/sqldiff.toml", pack);
    let mut a = app(true);
    // Named by another spelling of the same file.
    a.packs = vec![d.p("ws/../ws/sqldiff.toml")];
    let addr = start_with(a);
    let leaks = |v: &Value| {
        let t = v.to_string();
        assert!(!t.contains("zz-"), "{t}");
        assert!(!t.contains("preserve"), "{t}");
        assert!(!t.contains("unknown field"), "{t}");
    };
    for dir in [d.p("ws"), d.p("ws/sqldiff.toml")] {
        let (status, v) = settings(addr, &dir);
        assert_eq!(status, "400 Bad Request", "{v}");
        leaks(&v);
    }
    let (status, v) = post(
        addr,
        "/api/settings/preview",
        json!({"dir": d.p("ws"), "draft": {}, "dialect": "postgres", "text": QUERY}),
    );
    assert_eq!(status, "400 Bad Request", "{v}");
    leaks(&v);
    let (status, v) = post(
        addr,
        "/api/settings/save",
        json!({"dir": d.p("ws"), "draft": {"indent": 4}, "hash": null}),
    );
    assert_eq!(status, "400 Bad Request", "{v}");
    leaks(&v);
    assert_eq!(d.read("ws/sqldiff.toml"), pack);
    // A launch pack that cannot be loaded may be any file: nothing is read.
    let mut a = app(true);
    a.packs = vec![d.p("missing/pack.toml")];
    let addr = start_with(a);
    d.write("other/sqldiff.toml", "[format]\nindent = 2\n");
    let (status, v) = settings(addr, &d.p("other"));
    assert_eq!(status, "400 Bad Request", "{v}");
}

#[test]
fn the_preview_runs_the_code_sqldiff_fmt_runs() {
    let d = Dir::new("preview");
    d.write("packs/team.toml", SHARED_PACK);
    let ws = d.write(
        "ws/sqldiff.toml",
        &format!("packs = [\"../packs/team.toml\"]\n{WORKSPACE}"),
    );
    let addr = start(true);
    let cfg = Resolved::load(&Sources {
        isolated: true,
        project: Some(ws),
        ..Sources::default()
    })
    .unwrap();
    for (dialect, text) in [
        ("tsql", QUERY.to_string()),
        ("postgres", QUERY.to_string()),
        // CRLF in, CRLF out (line_ending = keep).
        ("tsql", QUERY.replace('\n', "\r\n")),
    ] {
        let (_, v) = settings(addr, &d.0.join("ws"));
        let (status, p) = post(
            addr,
            "/api/settings/preview",
            json!({"dir": d.0.join("ws"), "draft": v["draft"], "dialect": dialect, "text": text}),
        );
        assert_eq!(status, "200 OK", "{p}");
        let dialect: Dialect = dialect.parse().unwrap();
        let want = format::format(&text, dialect, &cfg.policy.format.options(dialect));
        assert_eq!(p["text"], want.text, "{dialect:?}");
        assert_eq!(p["changed"], want.changed(&text));
    }
    // The pack's [format.tsql] upper-cases; the workspace's [format] lowers.
    let (_, p) = post(
        addr,
        "/api/settings/preview",
        json!({"dir": d.0.join("ws"), "draft": {"keyword_case": "lower"}, "dialect": "tsql", "text": QUERY}),
    );
    assert!(p["text"].as_str().unwrap().starts_with("SELECT"), "{p}");
    let (_, p) = post(
        addr,
        "/api/settings/preview",
        json!({"dir": d.0.join("ws"), "draft": {"keyword_case": "lower"}, "dialect": "db2i", "text": QUERY}),
    );
    assert!(p["text"].as_str().unwrap().starts_with("select"), "{p}");
    // The draft's effective values come back with it.
    assert_eq!(p["effective"]["db2i"]["keyword_case"]["layer"], 2);
    // A psql meta-command line is script-client text and stays as written;
    // the statements kept for failing a check come back in `kept`.
    let (_, p) = post(
        addr,
        "/api/settings/preview",
        json!({"dir": d.0.join("ws"), "draft": {}, "dialect": "postgres", "text": "\\set  x   1\nselect a from t;\n"}),
    );
    assert!(
        p["text"].as_str().unwrap().starts_with("\\set  x   1\n"),
        "{p}"
    );
    assert!(p["kept"].as_array().unwrap().is_empty(), "{p}");
    // Bad input is an error, not a panic.
    for body in [
        json!({"dir": d.0.join("ws"), "draft": {"indent": 99}, "dialect": "tsql", "text": QUERY}),
        json!({"dir": d.0.join("ws"), "draft": {"nope": 1}, "dialect": "tsql", "text": QUERY}),
        json!({"dir": d.0.join("ws"), "draft": {}, "dialect": "oracle", "text": QUERY}),
    ] {
        assert_eq!(
            post(addr, "/api/settings/preview", body).0,
            "400 Bad Request"
        );
    }
}

#[test]
fn posts_need_this_launchs_token() {
    let d = Dir::new("token");
    d.write("ws/sqldiff.toml", WORKSPACE);
    let addr = start(true);
    let body = |t: Option<&str>| {
        let mut b = json!({"dir": d.0.join("ws"), "draft": {"indent": 8}, "hash": null});
        if let Some(t) = t {
            b["token"] = json!(t);
        }
        b.to_string()
    };
    for t in [
        None,
        Some(""),
        Some("0".repeat(32).as_str()),
        Some(token(start(true)).as_str()),
    ] {
        let (status, text) = request(addr, "POST", "/api/settings/save", &body(t));
        assert!(
            status == "400 Bad Request" || status == "403 Forbidden",
            "{status} {text}"
        );
    }
    let (status, text) = request(addr, "POST", "/api/settings/preview", &body(Some("x")));
    assert!(
        status == "400 Bad Request" || status == "403 Forbidden",
        "{status} {text}"
    );
    assert_eq!(d.read("ws/sqldiff.toml"), WORKSPACE);
}

#[test]
fn saving_changes_the_format_keys_and_keeps_the_rest() {
    let d = Dir::new("save");
    let original = "# Our SQL layout.\nexcerpt_lines = 5\n\n[format]\nkeyword_case = \"lower\"   # we like it quiet\nindent = 2\n\n[format.postgres]\none_item_per_line = true\n";
    d.write("ws/sqldiff.toml", original);
    let addr = start(true);
    let (_, v) = settings(addr, &d.0.join("ws"));
    let hash = v["hash"].clone();
    let (status, saved) = post(
        addr,
        "/api/settings/save",
        json!({"dir": d.0.join("ws"), "hash": hash,
               "draft": {"keyword_case": "lower", "indent": 4, "join_lines": true, "tsql": {"indent_style": "tabs"}}}),
    );
    assert_eq!(status, "200 OK", "{saved}");
    let now = d.read("ws/sqldiff.toml");
    assert_eq!(
        now,
        "# Our SQL layout.\nexcerpt_lines = 5\n\n[format]\nkeyword_case = \"lower\"   # we like it quiet\nindent = 4\njoin_lines = true\n\n[format.tsql]\nindent_style = \"tabs\"\n"
    );
    // The reply is the page's new state.
    assert_eq!(saved["draft"]["indent"], 4);
    assert_ne!(saved["hash"], v["hash"]);
    assert_eq!(saved["effective"]["tsql"]["indent_style"]["value"], "tabs");
    // Nothing else was left in the folder.
    let names: Vec<_> = std::fs::read_dir(d.0.join("ws"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["sqldiff.toml"]);
    // The file reads back as a config.
    let cfg = Resolved::load(&Sources {
        isolated: true,
        project: Some(d.p("ws/sqldiff.toml")),
        ..Sources::default()
    })
    .unwrap();
    assert_eq!(cfg.excerpt_lines, 5);
    assert_eq!(cfg.policy.format.all.indent, Some(4));
}

#[test]
fn saving_keeps_crlf_and_creates_a_missing_file() {
    let d = Dir::new("crlf");
    let crlf = "# Windows editors\r\n[format]\r\nindent = 2 # two\r\n".to_string();
    d.write("ws/sqldiff.toml", &crlf);
    let addr = start(true);
    let (_, v) = settings(addr, &d.0.join("ws"));
    let (status, _) = post(
        addr,
        "/api/settings/save",
        json!({"dir": d.0.join("ws"), "hash": v["hash"], "draft": {"indent": 3, "clause_breaks": false}}),
    );
    assert_eq!(status, "200 OK");
    assert_eq!(
        d.read("ws/sqldiff.toml"),
        "# Windows editors\r\n[format]\r\nindent = 3 # two\r\nclause_breaks = false\r\n"
    );
    // A folder with no sqldiff.toml gets one; the folder must exist.
    std::fs::create_dir_all(d.0.join("new")).unwrap();
    let (status, _) = post(
        addr,
        "/api/settings/save",
        json!({"dir": d.0.join("new"), "hash": null, "draft": {"indent": 8}}),
    );
    assert_eq!(status, "200 OK");
    assert_eq!(
        d.read("new/sqldiff.toml").replace("\r\n", "\n"),
        "[format]\nindent = 8\n"
    );
    let (status, _) = post(
        addr,
        "/api/settings/save",
        json!({"dir": d.0.join("missing"), "hash": null, "draft": {"indent": 8}}),
    );
    assert_eq!(status, "400 Bad Request");
    assert!(!d.0.join("missing").exists());
}

#[test]
fn saving_refuses_a_file_that_changed_since_it_was_read() {
    let d = Dir::new("stale");
    d.write("ws/sqldiff.toml", WORKSPACE);
    let addr = start(true);
    let (_, v) = settings(addr, &d.0.join("ws"));
    let edited = format!("{WORKSPACE}\n# edited elsewhere\n");
    d.write("ws/sqldiff.toml", &edited);
    let (status, body) = post(
        addr,
        "/api/settings/save",
        json!({"dir": d.0.join("ws"), "hash": v["hash"], "draft": {"indent": 8}}),
    );
    assert_eq!(status, "409 Conflict", "{body}");
    assert_eq!(d.read("ws/sqldiff.toml"), edited);
    // A file made in the meantime counts as a change too.
    std::fs::create_dir_all(d.0.join("fresh")).unwrap();
    d.write("fresh/sqldiff.toml", "[format]\nindent = 2\n");
    let (status, _) = post(
        addr,
        "/api/settings/save",
        json!({"dir": d.0.join("fresh"), "hash": null, "draft": {"indent": 8}}),
    );
    assert_eq!(status, "409 Conflict");
}

#[test]
fn saving_writes_only_a_file_named_sqldiff_toml() {
    let d = Dir::new("target");
    let user = d.write("home/config.toml", "[format]\nindent = 9\n");
    d.write("ws/notes.txt", "keep\n");
    let addr = start(true);
    for target in [
        user.clone(),
        d.p("ws/notes.txt"),
        d.p("ws/sqldiff.toml.bak"),
    ] {
        let (status, body) = post(
            addr,
            "/api/settings/save",
            json!({"dir": target, "hash": null, "draft": {"indent": 4}}),
        );
        assert_eq!(status, "400 Bad Request", "{target:?} {body}");
    }
    assert_eq!(d.read("home/config.toml"), "[format]\nindent = 9\n");
    assert_eq!(d.read("ws/notes.txt"), "keep\n");
    // Naming the file itself works the same as naming its folder.
    let file = d.write("ws/sqldiff.toml", "[format]\nindent = 2\n");
    let (_, v) = settings(addr, &file);
    assert_eq!(v["file"], file.display().to_string());
    // A link is not written through.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            &user,
            d.p("ln/sqldiff.toml")
                .parent()
                .map(|p| {
                    std::fs::create_dir_all(p).unwrap();
                    d.p("ln/sqldiff.toml")
                })
                .unwrap(),
        )
        .unwrap();
        let (_, v) = settings(addr, &d.0.join("ln"));
        let (status, body) = post(
            addr,
            "/api/settings/save",
            json!({"dir": d.0.join("ln"), "hash": v["hash"], "draft": {"indent": 4}}),
        );
        assert_eq!(status, "403 Forbidden", "{body}");
        assert_eq!(d.read("home/config.toml"), "[format]\nindent = 9\n");
    }
}

#[test]
fn settings_answer_only_on_loopback() {
    let d = Dir::new("lan");
    d.write("ws/sqldiff.toml", WORKSPACE);
    let listener = TcpListener::bind("0.0.0.0:0").unwrap();
    let addr: SocketAddr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port())
        .parse()
        .unwrap();
    std::thread::spawn(move || serve(listener, app(true), &[]));
    let (status, body) = request(
        addr,
        "GET",
        &format!("/api/settings?dir={}", encode(&d.0.join("ws"))),
        "",
    );
    assert_eq!(status, "403 Forbidden", "{body}");
    assert!(!body.contains("indent"));
    let t = token(addr);
    for path in ["/api/settings/preview", "/api/settings/save"] {
        let b = json!({"token": t, "dir": d.0.join("ws"), "draft": {"indent": 8}, "hash": null,
                       "dialect": "tsql", "text": QUERY});
        let (status, _) = request(addr, "POST", path, &b.to_string());
        assert_eq!(status, "403 Forbidden", "{path}");
    }
    assert_eq!(d.read("ws/sqldiff.toml"), WORKSPACE);
    // The page says why instead of offering a form.
    let (_, html) = request(addr, "GET", "/settings", "");
    assert!(html.contains(r#""no_picking":"picking files and starting runs work only"#));
}
