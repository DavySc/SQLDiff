//! Review store, suppression, markers, redaction, history and cache.

use std::path::{Path, PathBuf};

use sqldiff_core::model::{AssumptionSource, SuppressionKind};
use sqldiff_core::{render, Artifact, Dialect, Options, Report, Verdict};
use sqldiff_review::{
    cache, history, markers, snapshot, suppress, Cache, DataKey, Decision, Redactor, ReviewState,
    State, Store, ANY_SNAPSHOT,
};
use sqldiff_source::config::{ConfigLayer, Sources};
use sqldiff_source::protocol::HistoryEntry;
use sqldiff_source::Resolved;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqldiff-review-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name),
    )
    .unwrap()
}

const PACK: &str = r#"deny_list = ["acmecorp"]

[pack]
name = "acmecorp-pack"
version = "1"

[[author_markers]]
kind = "intentional_difference"
pattern = "--\\s*DELIBERATE:\\s*(.*)"

[[author_markers]]
kind = "assumption"
pattern = "--\\s*ASSUMES:\\s*(.*)"

[[change_patterns]]
name = "ticket"
pattern = "\\b(ACME-[0-9]+)\\b"
"#;

fn config(dir: &Path, pack: &str) -> Resolved {
    let p = dir.join("pack.toml");
    std::fs::write(&p, pack).unwrap();
    let mut cli = ConfigLayer::default();
    cli.file.data_dir = Some(dir.join("data"));
    Resolved::load(&Sources {
        isolated: true,
        packs: vec![p],
        cli,
        ..Sources::default()
    })
    .unwrap()
}

fn movestock() -> (Artifact, Artifact) {
    (
        Artifact::new(
            "acmecorp/tsql/movestock.tsql.sql",
            Dialect::Tsql,
            fixture("movestock.tsql.sql"),
        ),
        Artifact::new(
            "acmecorp/pg/movestock.pg.sql",
            Dialect::Postgres,
            fixture("movestock.pg.sql"),
        ),
    )
}

fn report() -> Report {
    let (a, b) = movestock();
    sqldiff_core::diff(&a, &b, &Options::default())
}

#[test]
fn store_round_trip_and_matching() {
    let dir = scratch("store");
    let mut s = Store::open(&dir).unwrap();
    assert_eq!(s.get("fp1", "s1"), None);
    s.put(
        "fp1",
        "s1",
        Decision::new(State::Accepted)
            .comment("on purpose")
            .author("dana"),
    )
    .unwrap();
    s.put("fp2", ANY_SNAPSHOT, Decision::new(State::Rejected))
        .unwrap();

    // Persisted, reloaded by a second handle.
    let t = Store::open(&dir).unwrap();
    assert_eq!(
        t.get("fp1", "s1").unwrap().comment.as_deref(),
        Some("on purpose")
    );
    assert_eq!(t.get("fp1", "s2"), None, "exact snapshot only");
    assert_eq!(
        t.get("fp2", "anything").unwrap().state,
        State::Rejected,
        "any snapshot"
    );

    let now = sqldiff_review::time::now();
    assert_eq!(t.status("fp1", "s1", &now).state, ReviewState::Accepted);
    assert_eq!(t.status("fp1", "s2", &now).state, ReviewState::Outdated);
    assert_eq!(t.status("fp3", "s1", &now).state, ReviewState::Unreviewed);

    // Last write wins; clear removes.
    s.put("fp1", "s1", Decision::new(State::Commented).comment("hmm"))
        .unwrap();
    assert!(s.clear("fp2", ANY_SNAPSHOT).unwrap());
    assert!(!s.clear("fp2", ANY_SNAPSHOT).unwrap());
    let mut t = Store::open(&dir).unwrap();
    assert_eq!(t.get("fp1", "s1").unwrap().state, State::Commented);
    assert_eq!(t.get("fp2", "x"), None);
    assert_eq!(t.all().len(), 1);

    // Expiry.
    let past = Decision::new(State::Accepted)
        .expires("2001-01-01")
        .unwrap();
    t.put("fp4", "s", past).unwrap();
    assert_eq!(t.status("fp4", "s", &now).state, ReviewState::Expired);
    assert_eq!(t.compact(&now).unwrap(), 1);
    let lines = std::fs::read_to_string(t.path().unwrap()).unwrap();
    assert_eq!(lines.lines().count(), 1, "compacted to live keys: {lines}");
}

#[test]
fn store_encrypted_at_rest() {
    let dir = scratch("enc");
    let key = DataKey::from_secret(b"0123456789abcdef0123456789abcdef");
    let mut s = Store::open_with(&dir, Some(key.clone())).unwrap();
    s.put(
        "fp1",
        "s1",
        Decision::new(State::Accepted).comment("secret reason"),
    )
    .unwrap();
    let raw = std::fs::read_to_string(s.path().unwrap()).unwrap();
    assert!(
        !raw.contains("secret reason") && !raw.contains("fp1"),
        "{raw}"
    );
    assert!(raw.starts_with("enc1:"));
    let t = Store::open_with(&dir, Some(key)).unwrap();
    assert_eq!(
        t.get("fp1", "s1").unwrap().comment.as_deref(),
        Some("secret reason")
    );
    assert!(Store::open(&dir).is_err(), "no key");
    let wrong = DataKey::from_secret(b"another key entirely, 32 chars!!");
    assert!(Store::open_with(&dir, Some(wrong)).is_err());
}

#[test]
fn accepted_findings_are_suppressed() {
    let mut r = report();
    assert_eq!(r.verdict, Verdict::Different);
    let n = r.findings.len();
    assert!(n >= 1);
    let fp = r.findings[0].fingerprint.clone();
    let mut store = Store::in_memory();
    let now = sqldiff_review::time::now();

    // Another snapshot's decision does not apply.
    store
        .put(&fp, "other:snapshot", Decision::new(State::Accepted))
        .unwrap();
    assert_eq!(suppress(&mut r.clone(), &store, &now), 0);
    // Rejected and commented do not suppress.
    store
        .put(&fp, &snapshot(&r), Decision::new(State::Rejected))
        .unwrap();
    assert_eq!(suppress(&mut r.clone(), &store, &now), 0);
    // Expired acceptance does not suppress.
    store
        .put(
            &fp,
            &snapshot(&r),
            Decision::new(State::Accepted)
                .expires("2001-01-01")
                .unwrap(),
        )
        .unwrap();
    assert_eq!(suppress(&mut r.clone(), &store, &now), 0);

    store
        .put(
            &fp,
            &snapshot(&r),
            Decision::new(State::Accepted)
                .comment("intended")
                .author("dana"),
        )
        .unwrap();
    assert_eq!(suppress(&mut r, &store, &now), 1);
    assert_eq!(r.findings.len(), n - 1);
    assert_eq!(r.suppressed.len(), 1);
    assert_eq!(r.suppressed[0].kind, SuppressionKind::Review);
    assert!(r.suppressed[0]
        .justification
        .contains("accepted by dana: intended"));
    assert_eq!(r.verdict_before_suppression, Some(Verdict::Different));
    assert_eq!(r.verdict, r.open_verdict(), "verdict is the open verdict");
    if r.findings.iter().all(|f| f.verdict != Verdict::Different) {
        assert_ne!(r.open_verdict(), Verdict::Different);
    }
    assert_ne!(
        r.open_verdict(),
        Verdict::Equal,
        "suppression never yields equal"
    );

    // SARIF keeps it, marked suppressed.
    let sarif: serde_json::Value = serde_json::from_str(&render::sarif(&[r.clone()])).unwrap();
    let results = sarif["runs"][0]["results"].as_array().unwrap();
    let sup: Vec<_> = results
        .iter()
        .filter(|x| x.get("suppressions").is_some())
        .collect();
    assert_eq!(sup.len(), 1);
    assert_eq!(sup[0]["suppressions"][0]["kind"], "external");
    assert_eq!(sup[0]["partialFingerprints"]["sqldiff/v1"], fp.as_str());
    assert!(render::text(&r, false).contains("1 suppressed"));

    // JSON round trip keeps suppressed findings.
    let back: Report = serde_json::from_str(&render::json(&r)).unwrap();
    assert_eq!(back.suppressed, r.suppressed);
}

#[test]
fn author_markers() {
    let dir = scratch("markers");
    let cfg = config(&dir, PACK);
    let (a, mut b) = movestock();
    let base = sqldiff_core::diff(&a, &b, &Options::default());
    let f = &base.findings[0];
    let loc = f.a.as_ref().or(f.b.as_ref()).unwrap().clone();
    let side_text = |s: &str| -> String {
        let mut lines: Vec<&str> = s.lines().collect();
        lines.insert(
            loc.start_line - 1,
            "-- DELIBERATE: error codes are mapped by the caller",
        );
        lines.join("\n") + "\n"
    };
    let mut a = a;
    match loc.side {
        sqldiff_core::model::Side::A => a.text = side_text(&a.text),
        sqldiff_core::model::Side::B => b.text = side_text(&b.text),
    }
    b.text
        .push_str("-- ASSUMES: callers never run this inside a transaction\n");
    a.text
        .push_str("-- assumes:  Callers never run this   inside a transaction\n");

    let mut r = sqldiff_core::diff(&a, &b, &Options::default());
    let before = r.findings.len();
    let sum = markers::apply(&mut r, &a.text, Some(&b.text), &cfg);
    assert!(sum.suppressed >= 1, "{:#?}", r.findings);
    assert_eq!(r.findings.len() + sum.suppressed, before);
    assert!(r
        .suppressed
        .iter()
        .all(|s| s.kind == SuppressionKind::InSource));
    assert!(r.suppressed[0]
        .justification
        .contains("error codes are mapped"));

    // The ASSUMES pattern is case-sensitive, so only side B's marker counts.
    assert_eq!(sum.assumptions.len(), 1);
    let asm = r
        .assumptions
        .iter()
        .find(|x| x.id == sum.assumptions[0])
        .unwrap();
    assert_eq!(asm.source, AssumptionSource::Author);
    assert!(asm.cite.as_deref().unwrap().starts_with("B:"));
    assert_eq!(
        markers::assumption_id("callers  never run this inside a TRANSACTION"),
        asm.id,
        "ids fold case and whitespace"
    );
    let sarif: serde_json::Value = serde_json::from_str(&render::sarif(&[r])).unwrap();
    let results = sarif["runs"][0]["results"].as_array().unwrap();
    assert!(results
        .iter()
        .any(|x| x["suppressions"][0]["kind"] == "inSource"));
}

#[test]
fn redacted_export_passes_leak_check() {
    let dir = scratch("redact");
    let cfg = config(&dir, PACK);
    let mut r = report();
    r.findings[0]
        .witness
        .push_str(" See ACME-1234 for acmecorpinternal context.");
    r.assumptions[0].text.push_str(" (dbo.MoveStock)");

    let mut red = Redactor::from_config(&cfg).unwrap();
    let out = red.redact_report(&r);
    let json = render::json(&out);
    let sarif = render::sarif(std::slice::from_ref(&out));
    let text = render::text(&out, true);
    for o in [&json, &sarif, &text] {
        let lower = o.to_lowercase();
        for term in ["movestock", "move_stock", "acmecorp", "acme-1234", "dbo."] {
            assert!(!lower.contains(term), "{term} leaked in:\n{o}");
        }
        red.verify(o).unwrap();
    }
    assert!(
        json.contains("OBJ_") && json.contains("WI_") && json.contains("TERM_"),
        "{json}"
    );
    // The unredacted report fails verification.
    assert!(red.verify(&render::json(&r)).is_err());

    // Stable: the same salt gives the same placeholders; another salt does not.
    let mut again = Redactor::from_config(&cfg).unwrap();
    assert_eq!(render::json(&again.redact_report(&r)), json);
    let mut other = Redactor::new(&cfg, b"a different salt, 16+ bytes");
    assert_ne!(render::json(&other.redact_report(&r)), json);
    // Structure survives: fingerprints and verdicts are not touched.
    assert_eq!(out.findings[0].fingerprint, r.findings[0].fingerprint);
    assert_eq!(out.verdict, r.verdict);

    // Excerpts can be dropped entirely.
    let mut drop = Redactor::from_config(&cfg).unwrap();
    drop.options.drop_excerpts = true;
    let d = drop.redact_report(&r);
    assert!(d.findings.iter().all(|f| [&f.a, &f.b]
        .into_iter()
        .flatten()
        .all(|l| l.excerpt.is_empty())));

    // The repository leak scan agrees (design section 5.6 gate).
    let out_file = dir.join("export.json");
    std::fs::write(&out_file, &json).unwrap();
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/leak-scan.sh");
    let scan = std::process::Command::new(bash())
        .arg(&script)
        .arg("--file")
        .arg(&out_file)
        .env(
            "SQLDIFF_DENYLIST",
            "acmecorp\nMoveStock\nmove_stock\nACME-1234\n",
        )
        .output()
        .unwrap();
    assert!(
        scan.status.success(),
        "{}{}",
        String::from_utf8_lossy(&scan.stdout),
        String::from_utf8_lossy(&scan.stderr)
    );
}

/// The bash that runs the leak scan. On Windows a bare `bash` resolves to the
/// WSL relay in System32 (which fails without a distro) before PATH is
/// consulted, so prefer `SQLDIFF_BASH`, then Git for Windows' bash.
fn bash() -> PathBuf {
    if let Some(p) = std::env::var_os("SQLDIFF_BASH") {
        return PathBuf::from(p);
    }
    if cfg!(windows) {
        for root in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(dir) = std::env::var_os(root) {
                let p = PathBuf::from(dir).join("Git/bin/bash.exe");
                if p.exists() {
                    return p;
                }
            }
        }
    }
    PathBuf::from("bash")
}

#[test]
fn whole_word_identifiers() {
    let dir = scratch("words");
    let cfg = config(&dir, PACK);
    let mut red = Redactor::new(&cfg, b"0123456789abcdef");
    red.add_object("[sales].[Order]");
    let s =
        red.redact_str("SELECT * FROM sales.\"order\" o JOIN orders x ON reorder = 1 -- ACMECORPX");
    assert!(s.contains(" orders ") && s.contains("reorder"), "{s}");
    assert!(
        !s.to_lowercase().contains("sales") && !s.to_lowercase().contains("\"order\""),
        "{s}"
    );
    assert!(
        s.contains("TERM_") && s.ends_with('X'),
        "deny terms match inside words: {s}"
    );
}

#[test]
fn history_groups_by_work_item() {
    let dir = scratch("history");
    let cfg = config(&dir, PACK);
    let e = |rev: &str, d: &str| HistoryEntry {
        revision: rev.into(),
        time: None,
        author: None,
        description: d.into(),
        work_items: Vec::new(),
        artifact_id: None,
    };
    let entries = vec![
        e("r1", "ACME-1 first cut"),
        e("r2", "tidy"),
        e("r3", "ACME-2 and ACME-1 follow-up"),
    ];
    let g = history::group(&entries, &cfg);
    let names: Vec<_> = g.iter().map(|g| g.work_item.clone()).collect();
    assert_eq!(
        names,
        vec![Some("ACME-1".into()), Some("ACME-2".into()), None]
    );
    assert_eq!(g[0].revisions, vec!["r1", "r3"]);
    assert_eq!(g[2].revisions, vec!["r2"]);
}

#[test]
fn cache_hits_on_same_content_only() {
    let dir = scratch("cache");
    let cfg = config(&dir, PACK);
    let (a, b) = movestock();
    let opts = Options::default();
    let c = Cache::from_config(&cfg).unwrap();
    let mut calls = 0;
    let r1 = c.report(&a, Some(&b), &opts, &cfg, || {
        calls += 1;
        sqldiff_core::diff(&a, &b, &opts)
    });
    let r2 = c.report(&a, Some(&b), &opts, &cfg, || {
        calls += 1;
        sqldiff_core::diff(&a, &b, &opts)
    });
    assert_eq!(calls, 1);
    assert_eq!(r1, r2);
    let mut b2 = b.clone();
    b2.text.push_str("\n-- changed\n");
    assert_ne!(
        cache::report_key(&a, Some(&b), &opts, &cfg),
        cache::report_key(&a, Some(&b2), &opts, &cfg)
    );
    let opts5 = Options {
        excerpt_lines: 5,
        ..Options::default()
    };
    assert_ne!(
        cache::report_key(&a, Some(&b), &opts, &cfg),
        cache::report_key(&a, Some(&b), &opts5, &cfg)
    );
    assert!(c.clear().unwrap());
    let d = Cache::disabled();
    let _ = d.report(&a, Some(&b), &opts, &cfg, || {
        calls += 1;
        sqldiff_core::diff(&a, &b, &opts)
    });
    assert_eq!(calls, 2);

    // Sealed cache entries.
    let k = DataKey::from_secret(b"0123456789abcdef0123456789abcdef");
    let sealed = Cache::open(&dir.join("sealed"), Some(k));
    sealed.put("t", "abcdef", &"move_stock");
    let file = std::fs::read_to_string(dir.join("sealed/cache/t/ab/abcdef.json")).unwrap();
    assert!(!file.contains("move_stock"));
    assert_eq!(
        sealed.get::<String>("t", "abcdef").as_deref(),
        Some("move_stock")
    );
}

#[test]
fn data_key_config() {
    let dir = scratch("datakey");
    assert!(ConfigLayer::parse("data_key = \"hunter2hunter2hunter2\"\n").is_err());
    let l = ConfigLayer::parse("data_key = \"env:SQLDIFF_TEST_DATA_KEY\"\n").unwrap();
    std::env::set_var("SQLDIFF_TEST_DATA_KEY", "0123456789abcdef0123456789abcdef");
    let mut cli = l;
    cli.file.data_dir = Some(dir.clone());
    let cfg = Resolved::load(&Sources {
        isolated: true,
        cli,
        ..Sources::default()
    })
    .unwrap();
    let mut s = Store::from_config(&cfg).unwrap();
    s.put("fp", "s", Decision::new(State::Accepted)).unwrap();
    let raw = std::fs::read_to_string(s.path().unwrap()).unwrap();
    assert!(raw.starts_with("enc1:"));
}

// Regressions from the review of PR #12.

const WORDS_PACK: &str = r#"deny_list = ["data", "obj", "06cc"]

[pack]
name = "p"
version = "1"
"#;

#[test]
fn redaction_never_touches_keys_enums_or_hashes() {
    let dir = scratch("keys");
    let cfg = config(&dir, WORDS_PACK);
    for (pa, pb) in [
        ("rule.sql", "rule2.sql"),
        ("path.sql", "text.sql"),
        ("error.sql", "info.sql"),
        ("object.sql", "different.sql"),
    ] {
        let (mut a, mut b) = movestock();
        a.path = pa.into();
        b.path = pb.into();
        let r = sqldiff_core::diff(&a, &b, &Options::default());
        let mut red = Redactor::from_config(&cfg).unwrap();
        let out = red.redact_report(&r);
        let json = render::json(&out);
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(back.verdict, r.verdict);
        assert_eq!(back.a.content_hash, r.a.content_hash);
        for (x, y) in back.findings.iter().zip(&r.findings) {
            assert_eq!(x.fingerprint, y.fingerprint);
            assert_eq!(x.class, y.class);
        }
        red.verify_report(&out).unwrap();
        // Keys and labels of the output formats ("rule", "text", "error",
        // "different") may equal a name; they are not leaks.
        let sarif: serde_json::Value =
            serde_json::from_str(&render::sarif(std::slice::from_ref(&out))).unwrap();
        red.verify_value(&sarif).unwrap();
        assert!(!json.contains(&format!("\"{pa}\"")), "{json}");
    }
}

#[test]
fn shorter_term_is_tried_when_the_longest_fails_its_boundary() {
    let dir = scratch("fallback");
    let cfg = config(&dir, WORDS_PACK);
    let mut red = Redactor::new(&cfg, b"0123456789abcdef");
    red.add_object("sales.order");
    let s = red.redact_str("SELECT * FROM sales.OrderLines JOIN sales.order o");
    assert!(!s.to_lowercase().contains("sales"), "{s}");
    assert!(s.contains("OrderLines"), "{s}");
    assert!(red.verify(&s).is_ok());
    assert!(red.verify("sales.OrderLines").is_err());
}

#[test]
fn whole_body_finding_is_not_hidden_by_an_inner_marker() {
    let dir = scratch("inner");
    let cfg = config(&dir, PACK);
    let mut text: Vec<String> = (1..=30).map(|i| format!("-- line {i}")).collect();
    text[9] = "-- DELIBERATE: this one statement".into();
    let text = text.join("\n");
    let mut r = report();
    r.findings.truncate(1);
    let mut f = r.findings[0].clone();
    let mut loc = f.a.clone().or(f.b.clone()).unwrap();
    loc.side = sqldiff_core::model::Side::A;
    loc.start_line = 1;
    loc.end_line = 30;
    f.a = Some(loc.clone());
    f.b = None;
    r.findings = vec![f.clone()];
    let mut whole = r.clone();
    assert_eq!(markers::apply(&mut whole, &text, None, &cfg).suppressed, 0);
    assert_eq!(whole.findings.len(), 1);

    loc.start_line = 11;
    loc.end_line = 12;
    f.a = Some(loc);
    r.findings = vec![f];
    assert_eq!(markers::apply(&mut r, &text, None, &cfg).suppressed, 1);
    assert_eq!(r.verdict, r.open_verdict());
    assert!(r.verdict_before_suppression.is_some());
}

#[test]
fn compact_keeps_records_appended_by_another_handle() {
    let dir = scratch("compact");
    let mut gui = Store::open(&dir).unwrap();
    gui.put("fp1", "s", Decision::new(State::Accepted)).unwrap();
    let mut cli = Store::open(&dir).unwrap();
    cli.put("fp2", "s", Decision::new(State::Rejected)).unwrap();
    gui.compact(&sqldiff_review::time::now()).unwrap();
    let fresh = Store::open(&dir).unwrap();
    assert!(fresh.get("fp1", "s").is_some());
    assert!(
        fresh.get("fp2", "s").is_some(),
        "concurrent append survived"
    );
    assert!(!dir.join("review/decisions.lock").exists());
}

#[test]
fn torn_last_line_is_skipped_with_a_warning() {
    let dir = scratch("torn");
    let mut s = Store::open(&dir).unwrap();
    s.put("fp1", "s", Decision::new(State::Accepted)).unwrap();
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(s.path().unwrap())
        .unwrap();
    f.write_all(b"{\"fingerprint\":\"fp2\",\"snap").unwrap();
    let t = Store::open(&dir).unwrap();
    assert!(t.get("fp1", "s").is_some());
    assert_eq!(t.warnings().len(), 1);
    // Sealed records without the key still fail as a whole.
    let edir = scratch("torn-enc");
    let k = DataKey::from_secret(b"0123456789abcdef0123456789abcdef");
    Store::open_with(&edir, Some(k))
        .unwrap()
        .put("fp", "s", Decision::new(State::Accepted))
        .unwrap();
    assert!(Store::open(&edir).is_err());
}

#[test]
fn time_rejects_non_ascii() {
    assert!(Decision::new(State::Accepted)
        .expires("2026-10-2é")
        .is_err());
}
