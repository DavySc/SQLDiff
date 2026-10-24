//! Filesystem reference adapter: a directory tree, dialects from
//! `dialect_map` or path conventions, pairing by pack rules.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::protocol::{
    ArtifactData, Describe, ObjectFilter, ObjectInfo, Pair, PairScope, Provenance,
};
use crate::source::Source;
use crate::walk::{is_candidate, object_text, objects, whole_info};
use crate::{pairing, Error, Resolved, Result};

/// Directories never descended into.
const SKIP_DIRS: &[&str] = &[".git", ".hg", ".svn", "target", "node_modules"];

pub struct FsSource {
    root: PathBuf,
    config: Resolved,
    /// Dialect of every file, instead of `dialect_map` and path conventions.
    dialect: Option<sqldiff_core::Dialect>,
}

impl FsSource {
    pub fn new(root: &Path, config: &Resolved) -> Result<FsSource> {
        if !root.is_dir() {
            return Err(Error::new(format!("{}: not a directory", root.display())));
        }
        Ok(FsSource {
            root: root.to_path_buf(),
            config: config.clone(),
            dialect: None,
        })
    }

    /// Every file read in dialect `d` (`--dialect` on tree commands).
    pub fn with_dialect(mut self, d: Option<sqldiff_core::Dialect>) -> FsSource {
        self.dialect = d;
        self
    }

    /// Relative paths of candidate files, sorted, with `/` separators.
    fn files(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(d) = stack.pop() {
            for entry in
                std::fs::read_dir(&d).map_err(|e| Error::new(format!("{}: {e}", d.display())))?
            {
                let entry = entry?;
                let p = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if entry.file_type()?.is_dir() {
                    if !SKIP_DIRS.contains(&name.as_str()) {
                        stack.push(p);
                    }
                    continue;
                }
                let rel = self.rel(&p);
                if is_candidate(&rel, &self.config) {
                    out.push(rel);
                }
            }
        }
        out.sort();
        Ok(out)
    }

    fn rel(&self, p: &Path) -> String {
        p.strip_prefix(&self.root)
            .unwrap_or(p)
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    }

    fn path_of(&self, id: &str) -> Result<PathBuf> {
        if id.split('/').any(|s| s == ".." || s.is_empty()) || Path::new(id).is_absolute() {
            return Err(Error::new(format!("{id}: not an object id")));
        }
        Ok(self.root.join(id))
    }

    fn changed(&self, rel: &str) -> Option<String> {
        let p = self.path_of(rel).ok()?;
        std::fs::metadata(&p)
            .and_then(|m| m.modified())
            .ok()
            .map(rfc3339)
    }

    fn file(&self, rel: &str) -> Result<String> {
        let p = self.path_of(rel)?;
        std::fs::read_to_string(&p).map_err(|e| Error::new(format!("{rel}: {e}")))
    }

    /// One object by id: a file, or `file#name` for one object of a file.
    fn read(&self, id: &str) -> Result<(ObjectInfo, String)> {
        // A file whose name contains `#` is still a file.
        let (rel, frag) = if self.path_of(id).is_ok_and(|p| p.is_file()) {
            (id, None)
        } else {
            crate::split::split_id(id)
        };
        let text = self.file(rel)?;
        let changed = self.changed(rel);
        if frag.is_none() {
            return Ok((
                whole_info(rel, &text, &self.config, changed, self.dialect),
                text,
            ));
        }
        let (info, text) = object_text(id, &self.config, self.dialect, |_| Ok(text.clone()))
            .and_then(|(_, t)| {
                objects(rel, &text, &self.config, changed, self.dialect)
                    .into_iter()
                    .find(|o| o.id == id)
                    .map(|o| (o, t))
                    .ok_or_else(|| Error::new(format!("{id}: no such object")))
            })?;
        Ok((info, text))
    }
}

impl Source for FsSource {
    fn describe(&self) -> Result<Describe> {
        Ok(Describe {
            name: "fs".to_string(),
            version: sqldiff_core::TOOL_VERSION.to_string(),
            protocol: crate::protocol::PROTOCOL_VERSION,
            capabilities: ["list_objects", "get_artifact", "get_pairs"]
                .map(String::from)
                .to_vec(),
            source: self.root.display().to_string(),
        })
    }

    fn list_objects(&self, filter: &ObjectFilter) -> Result<Vec<ObjectInfo>> {
        let mut out = Vec::new();
        for rel in self.files()? {
            let text = self.file(&rel)?;
            for info in objects(&rel, &text, &self.config, self.changed(&rel), self.dialect) {
                if filter.matches(&info) {
                    out.push(info);
                }
            }
        }
        Ok(out)
    }

    fn get_artifact(&self, id: &str) -> Result<ArtifactData> {
        let (info, text) = self.read(id)?;
        Ok(ArtifactData {
            provenance: Provenance {
                author_kind: "unknown".to_string(),
                time: info.last_changed.clone(),
                ..Provenance::default()
            },
            info,
            text,
            metadata: Default::default(),
            template: None,
        })
    }

    fn get_pairs(&self, scope: &PairScope) -> Result<Vec<Pair>> {
        let objects = self.list_objects(&ObjectFilter::default())?;
        Ok(pairing::pair(&objects, &self.config, scope)?.pairs)
    }
}

/// UTC RFC 3339 timestamp, seconds precision.
pub fn rfc3339(t: SystemTime) -> String {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_761_229_160);
        assert_eq!(rfc3339(t), "2025-10-23T14:19:20Z");
        assert_eq!(rfc3339(SystemTime::UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }
}
