//! `sqldiff convert`: convert a whole source tree into another dialect.
//!
//! The engine is `sqldiff_convert::tree`, shared with the MCP server; this
//! command reads the tree, checks the output directory, writes the
//! annotated output tree and its report, and prints the summary.

use std::path::PathBuf;
use std::process::ExitCode;

use sqldiff_convert::tree;
pub use sqldiff_convert::tree::TemplateMode;
pub(crate) use sqldiff_convert::tree::{catalog, collect, decode, parallel};

pub struct Args {
    pub tree: tree::Args,
    pub out: PathBuf,
    pub overwrite: bool,
    /// `instance-report.json`, or the directory holding it.
    pub instances: Option<PathBuf>,
    /// Tier 6 in the queue too.
    pub queue_all: bool,
}

pub fn run(a: Args) -> Result<ExitCode, String> {
    // Read before anything is written, so a bad path fails early.
    let inst = match &a.instances {
        Some(p) => Some(sqldiff_convert::queue::read_instances(p)?),
        None => None,
    };
    let (inputs, unreadable) = tree::read(&a.tree)?;
    tree::prepare_out(&a.tree.src, &a.out, a.overwrite)?;
    let mut t = tree::convert(&a.tree, &inputs, unreadable, Some(&a.out))?;
    // Review decisions committed next to the converted tree.
    for w in tree::review_in(&mut t, &inputs, &a.out)? {
        eprintln!("{}: not applied: {w}", sqldiff_convert::review::LEDGER_FILE);
    }
    let (json, md) = tree::write(&t, &a.out)?;
    let q = sqldiff_convert::queue::build(&t.report, inst.as_ref(), a.queue_all);
    let (qjson, qmd) = sqldiff_convert::queue::write(&q, &a.out)?;
    print!("{}", tree::summary(&t.report));
    print!("{}", sqldiff_convert::queue::summary(&q));
    eprintln!(
        "wrote {}, {}, {} and {}",
        json.display(),
        md.display(),
        qjson.display(),
        qmd.display()
    );
    Ok(ExitCode::from(if t.report.ok() { 0 } else { 1 }))
}
