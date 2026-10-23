//! Print one generated program: `cargo run -p sqldiff-corpus --example fuzz_sample -- pg 42`.

use sqldiff_corpus::fuzz::{measure, program, Lcg};
use sqldiff_syntax::Dialect;

fn main() {
    let mut args = std::env::args().skip(1);
    let d: Dialect = args
        .next()
        .unwrap_or_else(|| "pg".into())
        .parse()
        .expect("dialect");
    let seed: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    let text = program(&mut Lcg(seed), d);
    print!("{text}");
    eprintln!("{:?}", measure(&text, d));
}
