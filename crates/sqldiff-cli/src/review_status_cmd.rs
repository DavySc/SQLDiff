//! `sqldiff review-status`: the done gate of a converted tree, for CI.
//!
//! The engine is `sqldiff_convert::gate`; this command converts the source
//! again in memory with the ledger in the output directory, reads the
//! instance report, and prints the verdict. It writes nothing.

use std::path::PathBuf;
use std::process::ExitCode;

use sqldiff_convert::{gate, queue, review, tree};

pub struct Args {
    pub tree: tree::Args,
    pub out: PathBuf,
    pub instances: Option<PathBuf>,
    pub require_complete: bool,
    pub json: bool,
}

pub fn run(a: Args) -> Result<ExitCode, String> {
    if !a.out.join("conversion-report.json").exists() {
        return Err(format!(
            "{}: no converted tree (conversion-report.json): run `sqldiff convert` first",
            a.out.display()
        ));
    }
    let inst = match &a.instances {
        Some(p) => Some(queue::read_instances(p)?),
        None => None,
    };
    let (inputs, unreadable) = tree::read(&a.tree)?;
    let mut t = tree::convert(&a.tree, &inputs, unreadable, Some(&a.out))?;
    let rejected = tree::review_in(&mut t, &inputs, &a.out)?;
    if t.report.ledger.is_none() {
        tree::review(&mut t, &inputs, &review::Ledger::default(), "(none)");
    }
    let g = gate::check(&t, &a.tree, &a.out, inst.as_ref())?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&g).unwrap());
    } else {
        print!("{}", gate::summary(&g, 50));
    }
    for w in rejected {
        eprintln!("{}: not applied: {w}", review::LEDGER_FILE);
    }
    Ok(ExitCode::from(u8::from(a.require_complete && !g.complete)))
}
