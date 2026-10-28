//! The printer's contract (docs/verified-conversion.md, C0 items 1 and 4):
//! for every PostgreSQL statement the abstract syntax reads, printing it and
//! reading the text back gives the same tree. A statement the printer
//! refuses is allowed (it is counted); a printed text that reads back as a
//! different tree is a failure.

use std::path::{Path, PathBuf};

use sqldiff_syntax::abstract_syntax::{parse_stmt_ext, tokens, Expr, Stmt};
use sqldiff_syntax::{parse, print, Dialect, SyntaxKind};

const D: Dialect = Dialect::Postgres;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn postgres_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if p.is_dir() {
            if name != "target" && !name.starts_with('.') {
                postgres_files(&p, out);
            }
        } else if name.ends_with(".sql")
            && (name.starts_with("pg") || name.contains(".pg.") || name.contains("postgres"))
        {
            out.push(p);
        }
    }
}

/// Reads one statement's text back.
fn reread(text: &str) -> Result<Stmt, String> {
    let p = parse(text, D);
    parse_stmt_ext(&tokens(&p.syntax()), D)
}

/// Statements of a file the abstract syntax reads, with their source text.
fn statements(text: &str) -> Vec<(String, Stmt)> {
    let p = parse(text, D);
    p.syntax()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::STATEMENT)
        .filter_map(|n| {
            let s = parse_stmt_ext(&tokens(&n), D).ok()?;
            Some((n.text().to_string(), s))
        })
        .collect()
}

#[test]
fn every_postgres_statement_prints_and_reads_back_the_same() {
    let mut files = Vec::new();
    for d in ["corpus", "eval", "tests", "crates"] {
        postgres_files(&root().join(d), &mut files);
    }
    files.sort();
    assert!(files.len() >= 20, "found only {} files", files.len());
    let (mut read, mut printed, mut refused) = (0, 0, Vec::new());
    let mut wrong = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        for (src, s) in statements(&text) {
            read += 1;
            match print::stmt(&s, D) {
                Err(e) => refused.push(format!("{}: {e}: {}", f.display(), src.trim())),
                Ok(out) => {
                    printed += 1;
                    match reread(&out) {
                        Ok(back) if back == s => {}
                        Ok(back) => wrong.push(format!(
                            "{}\n  source:  {}\n  printed: {out}\n  tree:    {s:?}\n  back:    {back:?}",
                            f.display(),
                            src.trim()
                        )),
                        Err(e) => wrong.push(format!(
                            "{}\n  source:  {}\n  printed: {out}\n  does not read back: {e}",
                            f.display(),
                            src.trim()
                        )),
                    }
                }
            }
        }
    }
    eprintln!(
        "{} files, {read} statements read, {printed} printed, {} refused",
        files.len(),
        refused.len()
    );
    for r in &refused {
        eprintln!("refused: {r}");
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n\n"));
    assert!(printed * 10 >= read * 9, "printed only {printed} of {read}");
}

#[test]
fn awkward_shapes_read_back() {
    for sql in [
        "SELECT a || b + c FROM t",
        "SELECT - - 5, -(a + 1), NOT (a = 1 OR b = 2) FROM t",
        "SELECT \"Order\".\"Id\" AS \"Id\", \"select\" FROM \"Order\"",
        "SELECT x FROM t WHERE a NOT BETWEEN 1 + 1 AND 3 AND b NOT IN (1, 2) AND c NOT LIKE 'a''b%'",
        "SELECT CASE WHEN a IS NOT NULL THEN 1 ELSE 2 END, CAST(a AS numeric(10, 2)), EXTRACT(DAY FROM d) FROM t",
        "SELECT string_agg(DISTINCT x, ',' ORDER BY y DESC NULLS LAST) FROM t GROUP BY z HAVING count(*) > 1",
        "SELECT a FROM t UNION ALL SELECT b FROM u ORDER BY 1 LIMIT 10 OFFSET 5",
        "INSERT INTO t (a, b) VALUES (1, 'x') ON CONFLICT (a) DO UPDATE SET b = EXCLUDED.b RETURNING a INTO v",
        "UPDATE t AS x SET (a, b) = (SELECT 1, 2), c = c + 1 FROM u WHERE u.id = x.id",
        "DELETE FROM t AS x USING u WHERE u.id = x.id",
        "SELECT * FROM t WHERE EXISTS (SELECT 1 FROM u WHERE u.a = t.a) AND NOT EXISTS (SELECT 1 FROM v)",
        "SELECT qty INTO STRICT v FROM stock WHERE item_id = p FOR UPDATE",
    ] {
        let s = reread(sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
        let out = print::stmt(&s, D).unwrap_or_else(|e| panic!("{sql}: {e}"));
        assert_eq!(reread(&out).as_ref(), Ok(&s), "{sql}\nprinted: {out}");
    }
}

/// PostgreSQL ranks `||` below `+` and `-` and above comparisons, `LIKE`
/// and `BETWEEN`.
#[test]
fn postgres_concatenation_binds_looser_than_addition() {
    let bin = |op: &str, l: Expr, r: Expr| Expr::Bin(op.into(), Box::new(l), Box::new(r));
    let n = |s: &str| Expr::Num(s.into());
    let e = |sql: &str| match reread(&format!("SELECT {sql}")).unwrap() {
        Stmt::Select(q) => q.items[0].expr.clone(),
        s => panic!("{s:?}"),
    };
    assert_eq!(
        e("'a' || 1 + 2"),
        bin("||", Expr::Str("a".into()), bin("+", n("1"), n("2")))
    );
    assert_eq!(
        e("x LIKE 'a' || '%'"),
        Expr::Like(
            Box::new(Expr::Name(vec!["x".into()])),
            Box::new(bin("||", Expr::Str("a".into()), Expr::Str("%".into()))),
            false,
            false
        )
    );
    assert_ne!(e("'a' || 1 + 2"), e("('a' || 1) + 2"));
}

#[test]
fn what_postgres_cannot_spell_is_refused() {
    let t = |sql: &str| {
        let p = parse(sql, Dialect::Tsql);
        let s = parse_stmt_ext(&tokens(&p.syntax()), Dialect::Tsql).unwrap();
        print::stmt(&s, D)
    };
    assert!(t("SELECT TOP 1 a FROM t").is_err());
    assert!(t("SELECT @v = a FROM t").is_err());
    assert!(t("SELECT a FROM t WITH (NOLOCK)").is_err());
    assert!(t("SET @v = 1").is_err());
}
