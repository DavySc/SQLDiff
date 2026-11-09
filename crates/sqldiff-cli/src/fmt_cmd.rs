//! `sqldiff fmt`: lay out SQL files, checked by the engine.
//!
//! Layout comes from the `[format]` section of the loaded config and packs
//! (with `[format.tsql]`, `[format.postgres]`, `[format.db2i]`), then from
//! the command-line flags. Statements the check rejects stay as written and
//! are listed on stderr. The files are laid out by
//! `sqldiff_source::format_run`, which the app's format runs run too.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args as ClapArgs, ValueEnum};
use sqldiff_core::format::{self, FormatKeys, IndentStyle, KeywordCase, LineEnding};
use sqldiff_core::hooks::Hooks;
use sqldiff_core::Dialect;
use sqldiff_source::format_run;

use crate::source_cmd::ConfigArgs;
use crate::stdin_input;

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
    /// Files to format; a folder stands for its SQL files (`.sql` and what
    /// `dialect_map` names), which need --write or --check. `-` reads
    /// standard input and writes the result to standard output.
    #[arg(required = true)]
    files: Vec<PathBuf>,
    /// Dialect (default: from `dialect_map` or the path convention).
    #[arg(short, long)]
    dialect: Option<Dialect>,
    /// With `-`: the path standard input stands for. It decides the dialect
    /// (`dialect_map`, path convention) and where the project config is
    /// searched from; the file need not exist.
    #[arg(long, value_name = "PATH")]
    stdin_filepath: Option<PathBuf>,
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

/// `sqldiff fmt -`: standard input in, laid-out text on standard output.
fn run_stdin(a: &Args) -> Result<ExitCode, String> {
    if a.write {
        return Err("--write needs a file; `-` writes to standard output".into());
    }
    let path = a.stdin_filepath.as_deref();
    let cfg = a.cfg.resolve_from(stdin_input::config_dir(path))?;
    let flags = format::FormatConfig {
        all: a.keys(),
        ..Default::default()
    };
    flags.validate()?;
    let mut config = cfg.policy.format.clone();
    config.merge(&flags);
    let text = stdin_input::read_text()?;
    let art = stdin_input::artifact(text, path, a.dialect, &cfg)?;
    let shown = art.path.clone();
    let file = format_run::laid_out(Path::new(&shown), art, &config, &cfg);
    if let Some(line) = file.skipped_line() {
        eprintln!("{line}");
        return Ok(ExitCode::from(2));
    }
    for line in file.kept_lines() {
        eprintln!("{line}");
    }
    let text = file.text.as_deref().unwrap_or_default();
    if a.check {
        return Ok(if file.changed {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        });
    }
    // Bytes as they are: no newline translation on Windows.
    let mut out = std::io::stdout().lock();
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .map_err(|e| format!("<stdout>: {e}"))?;
    Ok(ExitCode::SUCCESS)
}

pub fn run(a: Args) -> Result<ExitCode, String> {
    let dash = a.files.iter().any(|f| stdin_input::is_stdin(f));
    if dash {
        if a.files.len() > 1 {
            return Err("`-` (standard input) cannot be combined with other files".into());
        }
        return run_stdin(&a);
    }
    if a.stdin_filepath.is_some() {
        return Err("--stdin-filepath needs `-` as the file".into());
    }
    if !a.write && !a.check && (a.files.len() > 1 || a.files.iter().any(|f| f.is_dir())) {
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
    let files = format_run::check(&a.files, a.dialect, &config, &cfg, &Hooks::default())?;
    let mut unformatted = 0;
    let mut errors = 0;
    for f in &files {
        if let Some(line) = f.skipped_line() {
            eprintln!("{line}");
            errors += 1;
            continue;
        }
        for line in f.kept_lines() {
            eprintln!("{line}");
        }
        if a.check {
            if f.changed {
                println!("{}", f.shown);
                unformatted += 1;
            }
        } else if a.write {
            if f.write()? {
                println!("formatted {}", f.shown);
            }
        } else if let Some(t) = &f.text {
            print!("{t}");
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
