//! The convert page highlights its editors in the browser, with a lexer of
//! its own (convert.html, `lexSql`). This runs that lexer under Node on the
//! corpus, in every dialect and with tricky fragments spliced in, and checks
//! that it colours every character the way the engine's tokens are styled
//! (`token_styles`, the pair view's colouring).

use std::path::{Path, PathBuf};
use std::process::Command;

use sqldiff_core::Dialect;
use sqldiff_gui::convert::{run, Setup, SAMPLE};
use sqldiff_gui::token_styles;

const PAGE: &str = include_str!("../assets/convert.html");

/// Fragments that start or end comments, strings, identifiers, variables and
/// routine bodies.
const FRAGMENTS: &[&str] = &[
    "'",
    "\"",
    "[",
    "]",
    "]]",
    "''",
    "/*",
    "*/",
    "/* /* */",
    "--",
    "\n",
    "$$",
    "$fn$",
    "$1",
    "$",
    "@",
    "@@x",
    "@1",
    "N'",
    "E'a\\'b'",
    "X'0F'",
    "U&'",
    "U&\"",
    "\\",
    "GO\n",
    "\nGO 2\n",
    "\n  go -- x\n",
    "\nGO;\n",
    "?",
    "0x1F",
    "1.5e-3",
    "1e",
    ".5",
    "1..2",
    "1_000",
    "#t",
    "a$b",
    "é",
    "😀",
    "\r\n",
    ";",
    "::",
    ":=",
    "LANGUAGE plpython3u",
    " AS $$ select 1 $$",
    " DO $$ begin end $$",
    "LANGUAGE sql",
    "+--",
    "*/*",
    "-=-",
];

fn sql_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            sql_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "sql") {
            out.push(p);
        }
    }
}

fn texts() -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    sql_files(&root.join("corpus"), &mut files);
    sql_files(&root.join("tests/fixtures"), &mut files);
    files.sort();
    let mut base: Vec<String> = files
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect();
    base.push(SAMPLE.to_string());
    base.extend(
        [
            "SELECT [a]]b], N'it''s', @x, @@ROWCOUNT FROM #t\nGO 2\nSELECT 1",
            "/* outer /* inner */ still comment */ SELECT 1",
            "SELECT 'line one\nline two' AS s;",
            "CREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN 1; /* x */ END $$ LANGUAGE plpgsql;",
            "CREATE FUNCTION f() RETURNS int LANGUAGE plpython3u AS $$ return 1 $$;",
            "CREATE FUNCTION f() RETURNS int AS $body$ SELECT 'a$$b' $body$ LANGUAGE sql;",
            "DO $$ DECLARE x int := 1; BEGIN RAISE NOTICE '%', x; END $$;",
            "SELECT $1, E'a\\'b', U&'d\\0061', x::int FROM t WHERE a ~* 'b' AND c?d",
            "SELECT ? FROM SYSIBM.SYSDUMMY1 WHERE X'0F' <> GX'00' AND @a#$ = 1",
            // CRLF line endings (a Windows checkout); a lone CR is no line end.
            "-- note\r\nSELECT 1; -- tail\r\n--\r\n-- lone\r",
        ]
        .map(String::from),
    );
    // Deterministic splices of the fragments into every text.
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n.max(1) as u64) as usize
    };
    let mut out = base.clone();
    for t in &base {
        for _ in 0..12 {
            let mut s = t.clone();
            for _ in 0..4 {
                let cuts: Vec<usize> = s.char_indices().map(|(i, _)| i).chain([s.len()]).collect();
                let at = cuts[next(cuts.len())];
                s.insert_str(at, FRAGMENTS[next(FRAGMENTS.len())]);
            }
            out.push(s);
        }
    }
    out
}

/// One style per UTF-16 unit, as the page names them ("" for plain).
fn engine_styles(text: &str, d: Dialect) -> Vec<&'static str> {
    let mut by_byte = vec![""; text.len()];
    for (s, e, st) in token_styles(text, d) {
        let st = match st {
            "kw" | "str" | "cmt" | "var" | "num" => st,
            _ => "",
        };
        by_byte[s..e].fill(st);
    }
    let mut out = Vec::new();
    for (i, c) in text.char_indices() {
        out.extend(std::iter::repeat(by_byte[i]).take(c.len_utf16()));
    }
    out
}

#[test]
fn page_highlighter_matches_the_engine() {
    let node = Command::new("node").arg("--version").output();
    if node.is_err() {
        assert!(
            std::env::var_os("CI").is_none(),
            "node is needed for this test on CI"
        );
        eprintln!("skipped: node not found");
        return;
    }
    let start = PAGE
        .find("const KEYWORDS = ")
        .expect("lexer start in convert.html");
    let end = PAGE
        .find("// The lines of text[from, to)")
        .expect("lexer end");
    let setup = Setup {
        from: Dialect::Tsql,
        to: Dialect::Postgres,
        template: None,
        text: String::new(),
        name: String::new(),
    };
    let initial = serde_json::to_string(&run(&setup, "", false)).unwrap();
    let cases: Vec<(String, Dialect)> = texts()
        .into_iter()
        .flat_map(|t| Dialect::ALL.map(|d| (t.clone(), d)))
        .collect();
    let input: Vec<_> = cases
        .iter()
        .map(|(t, d)| serde_json::json!([t, d.id()]))
        .collect();
    let dir = std::env::temp_dir().join(format!("sqldiff-hl-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("cases.json"),
        serde_json::to_string(&input).unwrap(),
    )
    .unwrap();
    let script = format!(
        "const INITIAL = {initial};\n{}\n\
         const cases = JSON.parse(require('fs').readFileSync(process.argv[2], 'utf8'));\n\
         process.stdout.write(JSON.stringify(cases.map(([t, d]) => lexSql(t, d))));\n",
        &PAGE[start..end]
    );
    std::fs::write(dir.join("lex.js"), script).unwrap();
    let out = Command::new("node")
        .arg(dir.join("lex.js"))
        .arg(dir.join("cases.json"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let runs: Vec<Vec<(usize, usize, String)>> = serde_json::from_slice(&out.stdout).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    let mut bad = Vec::new();
    for ((text, d), runs) in cases.iter().zip(runs) {
        let want = engine_styles(text, *d);
        let mut got = vec![""; want.len()];
        for (s, e, st) in &runs {
            for g in &mut got[*s..*e] {
                *g = match st.as_str() {
                    "kw" => "kw",
                    "str" => "str",
                    "cmt" => "cmt",
                    "var" => "var",
                    "num" => "num",
                    _ => "?",
                };
            }
        }
        if let Some(k) = (0..want.len()).find(|&k| want[k] != got[k]) {
            let units: Vec<u16> = text.encode_utf16().collect();
            let around =
                String::from_utf16_lossy(&units[k.saturating_sub(120)..(k + 60).min(units.len())]);
            bad.push(format!(
                "{}: at {k} engine `{}`, page `{}` in …{around:?}…",
                d.id(),
                want[k],
                got[k]
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} texts differ:\n{}",
        bad.len(),
        cases.len(),
        bad.iter().take(15).cloned().collect::<Vec<_>>().join("\n")
    );
}
