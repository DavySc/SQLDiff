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
    if let Some((c, path)) = &w.certificates {
        let (confirmed, all) = c.counts();
        println!("certificates: {confirmed} of {all} routines confirmed by the proof checker");
        eprintln!("wrote {}", path.display());
    }
    let [json, md, qjson, qmd] = &w.reports;
    eprintln!(
        "wrote {}, {}, {} and {}",
        json.display(),
        md.display(),
        qjson.display(),
        qmd.display()
    );
    if a.proof.mode == sqldiff_core::proof::Mode::Require {
        let why = match &w.certificates {
            Some((c, _)) => c.gate(&w, &a.tree.deployment),
            None => vec!["no certificates".to_string()],
        };
        if why.is_empty() {
            println!("release gate: passed (every routine confirmed, every assumption accepted)");
        } else {
            println!("release gate: a review is still needed");
            for y in &why {
                println!("  {y}");
            }
            return Ok(ExitCode::from(1));
        }
    }
    Ok(ExitCode::from(if w.tree.report.ok() { 0 } else { 1 }))
}
