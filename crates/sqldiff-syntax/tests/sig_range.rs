//! `ast::sig_range` looks for the first and last significant token from the
//! ends of the subtree; it must find what walking every token finds.

use sqldiff_syntax::ast::{sig_range, sig_tokens};
use sqldiff_syntax::{parse, Dialect, SyntaxNode};

/// The definition by walking every significant token.
fn walking(node: &SyntaxNode) -> (usize, usize) {
    let mut toks = sig_tokens(node);
    let Some(first) = toks.next() else {
        let r = node.text_range();
        return (r.start().into(), r.end().into());
    };
    let last = toks.last().unwrap_or_else(|| first.clone());
    (
        first.text_range().start().into(),
        last.text_range().end().into(),
    )
}

/// The text of every `.sql` file under `dir` (empty when it is missing).
fn corpus(dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![std::path::PathBuf::from(dir)];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "sql") {
                out.extend(std::fs::read_to_string(&p));
            }
        }
    }
    out
}

fn check(text: &str, d: Dialect) {
    let root = parse(text, d).syntax();
    for n in root.descendants() {
        assert_eq!(
            sig_range(&n),
            walking(&n),
            "{:?} in {text:?} ({d:?})",
            n.kind()
        );
    }
}

#[test]
fn looking_from_both_ends_finds_what_walking_every_token_finds() {
    let samples = [
        "",
        "   \n\n",
        "-- only a comment\n",
        "/* a */ SELECT 1 /* b */ ; -- c\n",
        "SELECT a\n  FROM t -- tail\nGO\nSELECT 2\n",
        "CREATE PROCEDURE dbo.p AS\nBEGIN\n  IF 1 = 1 SELECT 1\n  ELSE SELECT 2\n  -- end\nEND\n",
        "CREATE TABLE t (a int, /* c */ b int)\n\n\nINSERT INTO t VALUES (1, 2)",
        "DECLARE @x int\nSET @x = 1\nWHILE @x < 3 SET @x += 1\nPRINT 'a;b'\n",
        "BEGIN TRY SELECT 1 END TRY BEGIN CATCH THROW END CATCH",
    ];
    for d in [Dialect::Tsql, Dialect::Postgres, Dialect::Db2i] {
        for s in samples {
            check(s, d);
        }
    }
    // Real trees when the checkout has them.
    for dir in ["../../corpus", "../../eval"] {
        for text in corpus(dir).iter().take(400) {
            check(text, Dialect::Tsql);
        }
    }
    // And any tree named by the environment (every file, all dialects).
    if let Ok(dir) = std::env::var("SQLDIFF_SIG_CORPUS") {
        for text in corpus(&dir) {
            for d in [Dialect::Tsql, Dialect::Postgres, Dialect::Db2i] {
                check(&text, d);
            }
        }
    }
}
