//! `sqldiff convert`: convert a whole source tree into another dialect.
//!
//! The engine is `sqldiff_convert::tree`, shared with the MCP server, and
//! `sqldiff_convert::job` writes the output tree, its report and its review
//! queue, as the GUI's runs do; this command prints the summary.

use std::process::ExitCode;

use sqldiff_convert::tree;
pub use sqldiff_convert::tree::TemplateMode;

pub use sqldiff_convert::job::Job as Args;

pub fn run(a: Args) -> Result<ExitCode, String> {
    let w = sqldiff_convert::job::convert_into(&a, &tree::Hooks::default())?;
    for n in &w.not_applied {
        eprintln!("{}: not applied: {n}", sqldiff_convert::review::LEDGER_FILE);
    }
    print!("{}", tree::summary(&w.tree.report));
    print!("{}", sqldiff_convert::queue::summary(&w.queue));
    let [json, md, qjson, qmd] = &w.reports;
    eprintln!(
        "wrote {}, {}, {} and {}",
        json.display(),
        md.display(),
        qjson.display(),
        qmd.display()
    );
    Ok(ExitCode::from(if w.tree.report.ok() { 0 } else { 1 }))
}
