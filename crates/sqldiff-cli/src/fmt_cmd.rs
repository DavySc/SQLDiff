//! `sqldiff fmt`: lay out SQL files, checked by the engine.
//!
//! Layout comes from the `[format]` section of the loaded config and packs
//! (with `[format.tsql]`, `[format.postgres]`, `[format.db2i]`), then from
//! the command-line flags. Statements the check rejects stay as written and
//! are listed on stderr.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args as ClapArgs, ValueEnum};
use sqldiff_core::format::{self, FormatKeys, IndentStyle, KeywordCase, LineEnding};
use sqldiff_core::Dialect;

use crate::source_cmd::ConfigArgs;

#[derive(Clone, Copy, ValueEnum)]
pub enum Case {
    Upper,
    Lower,
    Preserve,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Ending {
    Keep,
    Lf,
    Crlf,
}

#[derive(ClapArgs)]
pub struct Args {
    /// Files to format.
    #[arg(required = true)]
    files: Vec<PathBuf>,
    /// Dialect (default: from `dialect_map` or the path convention).
    #[arg(short, long)]
    dialect: Option<Dialect>,
    /// Write the result back to each file.
    #[arg(long, conflicts_with = "check")]
    write: bool,
    /// Change nothing; exit 1 if a file is not formatted.
    #[arg(long)]
    check: bool,
    /// Spaces per indent level.
    #[arg(long, value_name = "N")]
    indent: Option<usize>,
    /// Indent with tabs.
    #[arg(long)]
    tabs: bool,
    #[arg(long, value_enum)]
    keyword_case: Option<Case>,
    /// Keep clauses (`FROM`, `WHERE`, `JOIN`, ...) where they are.
    #[arg(long)]
    no_clause_breaks: bool,
    /// One select-list, SET-list or ORDER BY item per line.
    #[arg(long)]
    one_item_per_line: bool,
    /// Join lines broken inside a statement.
    #[arg(long)]
    join_lines: bool,
    /// Also lay out PostgreSQL routine bodies between dollar quotes.
    #[arg(long)]
    postgres_bodies: bool,
    #[arg(long, value_enum)]
    line_ending: Option<Ending>,
    /// Blank lines kept in a row.
    #[arg(long, value_name = "N")]
    max_blank_lines: Option<usize>,
    #[command(flatten)]
    cfg: ConfigArgs,
}

impl Args {
    /// The flags as a config layer above the files.
    fn keys(&self) -> FormatKeys {
        let on = |b: bool| b.then_some(true);
        FormatKeys {
            indent: self.indent,
            indent_style: self.tabs.then_some(IndentStyle::Tabs),
            keyword_case: self.keyword_case.map(|c| match c {
                Case::Upper => KeywordCase::Upper,
                Case::Lower => KeywordCase::Lower,
                Case::Preserve => KeywordCase::Preserve,
            }),
            clause_breaks: self.no_clause_breaks.then_some(false),
            one_item_per_line: on(self.one_item_per_line),
            join_lines: on(self.join_lines),
            postgres_bodies: on(self.postgres_bodies),
            line_ending: self.line_ending.map(|e| match e {
                Ending::Keep => LineEnding::Keep,
                Ending::Lf => LineEnding::Lf,
                Ending::Crlf => LineEnding::Crlf,
            }),
            max_blank_lines: self.max_blank_lines,
            ..FormatKeys::default()
        }
    }
}

pub fn run(a: Args) -> Result<ExitCode, String> {
    if !a.write && !a.check && a.files.len() > 1 {
        return Err("several files: give --write or --check".into());
    }
    let cfg = a.cfg.resolve()?;
    let flags = format::FormatConfig {
        all: a.keys(),
        ..Default::default()
    };
    flags.validate()?;
    let mut config = cfg.policy.format.clone();
    config.merge(&flags);
    let mut unformatted = 0;
    let mut errors = 0;
    for path in &a.files {
        let art = match crate::read(path, a.dialect, &cfg) {
            Ok(art) => art,
            Err(e) => {
                eprintln!("sqldiff: {e}");
                errors += 1;
                continue;
            }
        };
        // A generator template is not SQL; its placeholders are outside what
        // the check can prove.
        if let Some(t) = &cfg.policy.template {
            if art.text.contains(&t.open) {
                eprintln!(
                    "{}: not formatted: a template (contains `{}`)",
                    art.path, t.open
                );
                errors += 1;
                continue;
            }
        }
        let out = format::format(&art.text, art.dialect, &config.options(art.dialect));
        for k in &out.kept {
            eprintln!(
                "{}:{}-{}: kept as written: {}",
                art.path, k.start_line, k.end_line, k.reason
            );
        }
        let changed = out.changed(&art.text);
        if a.check {
            if changed {
                println!("{}", art.path);
                unformatted += 1;
            }
        } else if a.write {
            if changed {
                std::fs::write(path, &out.text).map_err(|e| format!("{}: {e}", path.display()))?;
                println!("formatted {}", art.path);
            }
        } else {
            print!("{}", out.text);
        }
    }
    Ok(if errors > 0 {
        ExitCode::from(2)
    } else if unformatted > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}
