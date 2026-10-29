//! `sqldiff fmt` over files and folders: what the command runs, and what the
//! app's format runs run, so both lay out the same files the same way.
//!
//! A folder stands for the SQL files a tree source lists there (`.sql` and
//! what `dialect_map` names, without ignored paths or tool folders). Each
//! file is laid out by [`format::format`], which keeps a statement as
//! written when its check fails. [`check`] writes nothing; a caller writes
//! the changed files afterwards ([`File::write`]), so a cancelled run has
//! written nothing.

use std::path::{Path, PathBuf};

use sqldiff_core::format::{self, FormatConfig, Kept};
use sqldiff_core::hooks::Hooks;
use sqldiff_core::Dialect;

use crate::fs::FsSource;
use crate::Resolved;

/// Why a file was not laid out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skipped {
    /// Not read: missing, not text, or not in a known dialect.
    Unreadable(String),
    /// A generator template: its placeholders are outside what the check
    /// can prove. Holds the `[template]` opening delimiter.
    Template(String),
    /// A symbolic link found in a folder: it may point outside the folder.
    Link,
}

/// One file, laid out in memory.
#[derive(Debug, Clone)]
pub struct File {
    /// The file on disk.
    pub file: PathBuf,
    /// The path as messages show it.
    pub shown: String,
    /// The laid-out text, unless the file was skipped.
    pub text: Option<String>,
    /// Laid out differently from what is on disk.
    pub changed: bool,
    /// Statements kept as written.
    pub kept: Vec<Kept>,
    pub skipped: Option<Skipped>,
}

impl File {
    /// Writes the laid-out text over the file if it changed; true if written.
    pub fn write(&self) -> Result<bool, String> {
        match (&self.text, self.changed) {
            (Some(t), true) => std::fs::write(&self.file, t)
                .map(|()| true)
                .map_err(|e| format!("{}: {e}", self.file.display())),
            _ => Ok(false),
        }
    }

    /// The line `sqldiff fmt` prints on stderr for a skipped file.
    pub fn skipped_line(&self) -> Option<String> {
        self.skipped.as_ref().map(|s| match s {
            Skipped::Unreadable(e) => format!("sqldiff: {e}"),
            Skipped::Template(open) => {
                format!(
                    "{}: not formatted: a template (contains `{open}`)",
                    self.shown
                )
            }
            Skipped::Link => format!("{}: not formatted: a symbolic link", self.shown),
        })
    }

    /// The lines `sqldiff fmt` prints on stderr for the statements kept.
    pub fn kept_lines(&self) -> Vec<String> {
        self.kept
            .iter()
            .map(|k| {
                format!(
                    "{}:{}-{}: kept as written: {}",
                    self.shown, k.start_line, k.end_line, k.reason
                )
            })
            .collect()
    }
}

/// The files `paths` name: a file as given, a folder as the SQL files a
/// tree source lists there, in order. The flag says the file was found in a
/// folder.
pub fn files(paths: &[PathBuf], cfg: &Resolved) -> Result<Vec<(PathBuf, bool)>, String> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            let src = FsSource::new(p, cfg).map_err(|e| e.0)?;
            let rels = src.list().map_err(|e| e.0)?;
            if rels.is_empty() {
                return Err(format!("{}: no SQL files", p.display()));
            }
            out.extend(
                rels.iter()
                    .map(|r| (r.split('/').fold(p.clone(), |d, c| d.join(c)), true)),
            );
        } else {
            out.push((p.clone(), false));
        }
    }
    Ok(out)
}

/// Lays out every file `paths` name with `config` (the config's `[format]`
/// merged with any flags) and reads each in `dialect`, or the dialect its
/// path says. Writes nothing.
pub fn check(
    paths: &[PathBuf],
    dialect: Option<Dialect>,
    config: &FormatConfig,
    cfg: &Resolved,
    hooks: &Hooks,
) -> Result<Vec<File>, String> {
    let files = files(paths, cfg)?;
    let total = files.len();
    let mut out = Vec::with_capacity(total);
    for (i, (path, in_folder)) in files.into_iter().enumerate() {
        hooks.stop_if_cancelled()?;
        out.push(one(&path, in_folder, dialect, config, cfg));
        hooks.report(i + 1, total);
    }
    Ok(out)
}

fn one(
    path: &Path,
    in_folder: bool,
    dialect: Option<Dialect>,
    config: &FormatConfig,
    cfg: &Resolved,
) -> File {
    let skipped = |shown: String, why: Skipped| File {
        file: path.to_path_buf(),
        shown,
        text: None,
        changed: false,
        kept: Vec::new(),
        skipped: Some(why),
    };
    let shown = path.display().to_string();
    if in_folder && path.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return skipped(shown, Skipped::Link);
    }
    let art = match crate::tree::read(path, dialect, cfg) {
        Ok(art) => art,
        Err(e) => return skipped(shown, Skipped::Unreadable(e.0)),
    };
    if let Some(t) = &cfg.policy.template {
        if art.text.contains(&t.open) {
            return skipped(art.path, Skipped::Template(t.open.clone()));
        }
    }
    let f = format::format(&art.text, art.dialect, &config.options(art.dialect));
    File {
        file: path.to_path_buf(),
        changed: f.changed(&art.text),
        shown: art.path,
        text: Some(f.text),
        kept: f.kept,
        skipped: None,
    }
}
