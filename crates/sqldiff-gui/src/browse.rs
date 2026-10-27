//! Folder listings for the app's file picker. The browser cannot tell the
//! page a real path, so the server lists folders and the page picks from
//! them. Only names and kinds are listed, never file contents.

use std::path::{Component, Path, PathBuf};

use serde::Serialize;

/// Most entries listed for one folder.
pub const MAX_ENTRIES: usize = 5000;

#[derive(Debug, Serialize)]
pub struct Listing {
    /// The folder, absolute.
    pub path: String,
    /// Its parent; `None` at a root.
    pub parent: Option<String>,
    /// Where to start over: drives on Windows, `/` elsewhere, and the home folder.
    pub roots: Vec<Root>,
    /// Folders first, then files, each by name ignoring case. Names starting
    /// with a dot are left out.
    pub entries: Vec<Entry>,
    /// Set when the folder holds more than [`MAX_ENTRIES`].
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct Root {
    pub label: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct Entry {
    pub name: String,
    pub dir: bool,
}

/// The folder at `path` (relative to the working directory), the folder
/// holding it when it is a file, or the home folder when `path` is empty.
pub fn list(path: &str) -> Result<Listing, String> {
    let mut dir = match path.trim() {
        "" => home().unwrap_or_else(|| absolute(Path::new("."))),
        p => absolute(Path::new(p)),
    };
    if dir.is_file() {
        dir.pop();
    }
    let rd = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut entries = Vec::new();
    let mut truncated = false;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if entries.len() == MAX_ENTRIES {
            truncated = true;
            break;
        }
        // Follows links: a linked folder opens like a folder.
        let dir = std::fs::metadata(e.path()).is_ok_and(|m| m.is_dir());
        entries.push(Entry { name, dir });
    }
    entries.sort_by(|a, b| {
        b.dir
            .cmp(&a.dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(Listing {
        path: dir.display().to_string(),
        parent: dir.parent().map(|p| p.display().to_string()),
        roots: roots(),
        entries,
        truncated,
    })
}

/// `p` made absolute against the working directory, with `.` and `..`
/// resolved by name (links are not followed, so `C:\` stays `C:\`).
pub fn absolute(p: &Path) -> PathBuf {
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(p)
    };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c.as_os_str()),
        }
    }
    out
}

fn home() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
}

fn roots() -> Vec<Root> {
    let tops: Vec<String> = if cfg!(windows) {
        (b'A'..=b'Z')
            .map(|c| format!("{}:\\", c as char))
            .filter(|d| Path::new(d).is_dir())
            .collect()
    } else {
        vec!["/".to_string()]
    };
    let home = home().map(|h| Root {
        label: "Home".into(),
        path: h.display().to_string(),
    });
    home.into_iter()
        .chain(tops.into_iter().map(|p| Root {
            label: p.clone(),
            path: p,
        }))
        .collect()
}
