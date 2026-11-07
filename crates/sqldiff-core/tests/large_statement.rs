//! One very long statement is compared in bounded time and memory. The
//! token alignment used a table of (tokens of A) x (tokens of B) cells, which
//! needed gigabytes for 20 000 tokens, and the witness search listed its
//! candidate values with a quadratic duplicate check, which made an IN list
//! of a few thousand numbers run for minutes.

use sqldiff_core::{diff, Artifact, Dialect, Options, Verdict};

fn run(a: String, b: String) -> Verdict {
    diff(
        &Artifact::new("a.sql", Dialect::Tsql, a),
        &Artifact::new("b.sql", Dialect::Tsql, b),
        &Options::default(),
    )
    .verdict()
}

/// `n` items, the last one changed in B.
fn pair(n: usize, stmt: impl Fn(&dyn Fn(usize) -> usize) -> String) -> (String, String) {
    (stmt(&|i| i), stmt(&|i| if i == n - 1 { i + 1 } else { i }))
}

#[test]
fn a_flat_list_of_ten_thousand_items_is_compared() {
    let n = 10_000;
    let (a, b) = pair(n, |g| {
        format!(
            "SELECT {};\n",
            (0..n)
                .map(|i| g(i).to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    });
    assert_eq!(run(a, b), Verdict::Different);
}

#[test]
fn an_in_list_of_thousands_of_numbers_is_compared() {
    let n = 2_500;
    let (a, b) = pair(n, |g| {
        format!(
            "SELECT 1 FROM dbo.t WHERE a IN ({});\n",
            (0..n)
                .map(|i| g(i).to_string())
                .collect::<Vec<_>>()
                .join(",")
        )
    });
    assert_eq!(run(a, b), Verdict::Different);
}
