//! `cargo run -p sqldiff-convert --example convert -- file.sql` prints the
//! PostgreSQL conversion of a T-SQL file followed by the unit badges.

fn main() {
    let path = std::env::args().nth(1).expect("usage: convert FILE.sql");
    let text = std::fs::read_to_string(&path).expect("readable file");
    let c = sqldiff_convert::convert(
        &text,
        sqldiff_core::Dialect::Tsql,
        sqldiff_core::Dialect::Postgres,
    )
    .expect("supported pair");
    println!("{}", c.output);
    println!("-- verdict: {}", c.report.verdict.label());
    for u in &c.units {
        println!(
            "-- in {}-{} → out {}-{}: {}",
            u.src.line,
            u.src.end_line,
            u.out.line,
            u.out.end_line,
            u.status.label()
        );
        for n in &u.notes {
            println!("--     [{}] {}: {}", n.status.label(), n.rule, n.text);
        }
        for &f in &u.findings {
            let f = &c.report.findings[f];
            println!(
                "--     finding {} ({}): {}",
                f.rule,
                f.verdict.label(),
                f.witness
            );
        }
    }
}
