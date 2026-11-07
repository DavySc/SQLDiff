//! `abstract_syntax::tokens_shared` is a cache of `tokens`: whatever was
//! asked before, it returns the tokens `tokens` returns for that node.

use sqldiff_syntax::abstract_syntax::{clear_shared_tokens, tokens, tokens_shared, Tok};
use sqldiff_syntax::{parse, Dialect, SyntaxKind};

fn plain(t: &[Tok]) -> Vec<(String, String, usize, String)> {
    t.iter()
        .map(|t| (format!("{:?}", t.kind), t.text.clone(), t.off, t.up.clone()))
        .collect()
}

fn check(text: &str, d: Dialect) {
    let root = parse(text, d).syntax();
    // Twice over every node: the first call fills the cache, the second
    // reads it, in a different order each time.
    let nodes: Vec<_> = root.descendants().collect();
    for pass in 0..2 {
        let order: Box<dyn Iterator<Item = &_>> = if pass == 0 {
            Box::new(nodes.iter())
        } else {
            Box::new(nodes.iter().rev())
        };
        for n in order {
            assert_eq!(
                plain(&tokens_shared(n)),
                plain(&tokens(n)),
                "{:?} in {text:?} ({d:?}, pass {pass})",
                n.kind()
            );
        }
    }
}

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

#[test]
fn the_shared_tokens_are_the_tokens() {
    let samples = [
        "",
        "SELECT 1; SELECT 2;",
        "CREATE PROCEDURE dbo.p AS\nBEGIN\n  IF 1 = 1 SELECT 1\n  ELSE SELECT 2\nEND\n",
        "DECLARE @x int\nSET @x = 1\nWHILE @x < 3 SET @x += 1\n",
    ];
    for d in [Dialect::Tsql, Dialect::Postgres, Dialect::Db2i] {
        for s in samples {
            check(s, d);
        }
    }
    for dir in ["../../corpus", "../../eval"] {
        for text in corpus(dir).iter().take(200) {
            check(text, Dialect::Tsql);
        }
    }
}

#[test]
fn a_full_cache_still_gives_the_same_tokens() {
    // Far more tokens than the cache keeps: it is emptied along the way and
    // a node asked for again is read back right either way.
    let stmt = "SELECT a, b, c, d, e, f, g, h FROM t WHERE a = 1 AND b = 2;\n";
    let text = stmt.repeat(12000);
    let root = parse(&text, Dialect::Tsql).syntax();
    let nodes: Vec<_> = root
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::STATEMENT)
        .collect();
    assert!(nodes.len() >= 10000);
    clear_shared_tokens();
    for round in 0..2 {
        for n in &nodes {
            assert_eq!(plain(&tokens_shared(n)), plain(&tokens(n)), "round {round}");
        }
    }
    clear_shared_tokens();
}

#[test]
fn a_tree_that_is_dropped_does_not_leave_a_stale_entry() {
    // The cache holds the node it keys by, so a new tree can never take the
    // key of a dropped one: parse many different texts one after another.
    clear_shared_tokens();
    for i in 0..300 {
        let text = format!("SELECT {i} AS n{i}, 'v{i}' FROM t{i}");
        let root = parse(&text, Dialect::Tsql).syntax();
        for n in root.descendants() {
            assert_eq!(plain(&tokens_shared(&n)), plain(&tokens(&n)), "{text}");
        }
    }
    clear_shared_tokens();
}
