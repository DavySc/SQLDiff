//! Converts the statements of a T-SQL file to PostgreSQL with the idiom
//! rules and prints the result, the surface terms and each piece's status.
//!
//! `cargo run -p sqldiff-core --example rules_convert -- FILE.sql`

use sqldiff_core::rules::{convert, sexp, surface};
use sqldiff_syntax::Dialect;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: rules_convert FILE.sql");
    let text = std::fs::read_to_string(path).unwrap();
    for u in surface::lower_text(&text, Dialect::Tsql) {
        eprintln!("surface: {}", sexp::write(&u.term));
    }
    let (out, pieces) = convert::convert_text(
        &text,
        Dialect::Tsql,
        Dialect::Postgres,
        &convert::Options::default(),
    );
    println!("{out}");
    for p in pieces {
        let asm: Vec<&str> = p.assumptions.iter().map(|x| x.0.as_str()).collect();
        eprintln!(
            "{}: rules {:?}, assumptions {:?}",
            p.status.label(),
            p.rules,
            asm
        );
        for n in &p.notes {
            eprintln!("  {} {}: {}", n.status.label(), n.rule, n.text);
        }
    }
}
