//! `sqldiff convert`: convert a whole source tree into another dialect.
//!
//! The engine is `sqldiff_convert::tree`, shared with the MCP server, and
//! `sqldiff_convert::job` writes the output tree, its report and its review
//! queue, as the GUI's runs do; this command prints the summary.

use std::process::ExitCode;

use sqldiff_convert::tree;
pub use sqldiff_convert::tree::TemplateMode;

pub use sqldiff_convert::job::Job as Args;

/// `convert --plan`: writes the plan and prints the groups in order.
pub fn plan(tree: &tree::Args, out: &std::path::Path) -> Result<ExitCode, String> {
    let (plan, path) = sqldiff_convert::job::plan_into(tree, out)?;
    let cyclic = plan.groups.iter().filter(|g| g.cyclic).count();
    println!(
        "{} files in {} groups ({} cyclic); what a file uses comes first",
        plan.files.len(),
        plan.groups.len(),
        cyclic
    );
    for (i, g) in plan.groups.iter().enumerate().take(20) {
        let names: Vec<&str> = g
            .files
            .iter()
            .map(|&f| plan.files[f].path.as_str())
            .collect();
        let shown = if names.len() > 3 {
            format!("{}, … ({} files)", names[..3].join(", "), names.len())
        } else {
            names.join(", ")
        };
        println!("  {:>4}. {shown}", i + 1);
    }
    if plan.groups.len() > 20 {
        println!("  … and {} more groups", plan.groups.len() - 20);
    }
    eprintln!("wrote {}", path.display());
    Ok(ExitCode::SUCCESS)
}

pub fn run(a: Args) -> Result<ExitCode, String> {
    let w = sqldiff_convert::job::convert_into(&a, &tree::Hooks::default())?;
    for n in &w.not_applied {
        eprintln!("{}: not applied: {n}", sqldiff_convert::review::LEDGER_FILE);
    }
    print!("{}", tree::summary(&w.tree.report));
    print!("{}", sqldiff_convert::queue::summary(&w.queue));
    // The proof counts are in the summary above (`proof: R of S routines
    // confirmed`), printed once.
    if let Some((_, path)) = &w.certificates {
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
            // The gate is the exit code: where the engine did not compare,
            // the proof stands.
            return Ok(ExitCode::SUCCESS);
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
