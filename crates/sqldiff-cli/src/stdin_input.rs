//! Reading one artifact from standard input (`-` as the file), for editors:
//! the buffer comes in on stdin and `--stdin-filepath` says where it lives, so
//! the dialect (`dialect_map`, path convention), the project config and the
//! path shown in findings are those of the real file.
//!
//! Standard input is read as bytes and must be UTF-8; line endings (CRLF
//! included) pass through untouched.

use std::io::Read;
use std::path::{Path, PathBuf};

use sqldiff_core::{Artifact, Dialect};
use sqldiff_source::{tree, Resolved};

/// The file argument that stands for standard input.
pub fn is_stdin(p: &Path) -> bool {
    p.as_os_str() == "-"
}

/// All of standard input as UTF-8 text.
pub fn read_text() -> Result<String, String> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .read_to_end(&mut bytes)
        .map_err(|e| format!("<stdin>: {e}"))?;
    String::from_utf8(bytes).map_err(|e| format!("<stdin>: not valid UTF-8 ({e})"))
}

/// Where the project config is looked for: the directory of the file
/// `--stdin-filepath` names, made absolute so the search reaches the parents.
pub fn config_dir(filepath: Option<&Path>) -> Option<PathBuf> {
    let parent = filepath?.parent()?;
    if parent.is_absolute() {
        return Some(parent.to_path_buf());
    }
    std::env::current_dir().ok().map(|c| c.join(parent))
}

/// The path as messages show it.
pub fn shown(filepath: Option<&Path>) -> String {
    filepath.map_or_else(|| "<stdin>".to_string(), |p| p.display().to_string())
}

/// An artifact for `text` as if it were the file `filepath` (or no file).
pub fn artifact(
    text: String,
    filepath: Option<&Path>,
    dialect: Option<Dialect>,
    cfg: &Resolved,
) -> Result<Artifact, String> {
    let rel = filepath.map(tree::slashed).unwrap_or_default();
    tree::artifact(shown(filepath), &rel, dialect, text, cfg).map_err(|e| e.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dash_is_stdin() {
        assert!(is_stdin(Path::new("-")));
        assert!(!is_stdin(Path::new("a.sql")));
    }

    #[test]
    fn config_dir_is_absolute() {
        assert!(config_dir(None).is_none());
        assert!(config_dir(Some(Path::new("db/a.sql")))
            .unwrap()
            .is_absolute());
    }
}
