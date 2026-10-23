//! Git reference adapter: the tree at one revision, read through the `git`
//! program (no working-tree access), with history from `git log`.
//!
//! Object ids are paths relative to the subdirectory. `PATH@REV` reads the
//! object at another revision; history entries carry such ids.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use crate::protocol::{
    ArtifactData, Describe, HistoryEntry, ObjectFilter, ObjectInfo, Pair, PairScope, Provenance,
};
use crate::source::Source;
use crate::walk::{is_candidate, object_info};
use crate::{pairing, Error, Resolved, Result};

pub struct GitSource {
    repo: PathBuf,
    rev: String,
    commit: String,
    subdir: Option<String>,
    config: Resolved,
    /// rel path -> blob id
    tree: BTreeMap<String, String>,
    last_changed: OnceLock<BTreeMap<String, String>>,
}

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::new(format!("git: {e}")))?;
    if !out.status.success() {
        return Err(Error::new(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    String::from_utf8(out.stdout).map_err(|_| Error::new("git: output is not UTF-8"))
}

impl GitSource {
    pub fn new(
        repo: &Path,
        rev: &str,
        subdir: Option<&str>,
        config: &Resolved,
    ) -> Result<GitSource> {
        let commit = git(
            repo,
            &["rev-parse", "--verify", &format!("{rev}^{{commit}}")],
        )?
        .trim()
        .to_string();
        let subdir = subdir
            .map(|s| s.trim_matches('/').to_string())
            .filter(|s| !s.is_empty());
        let mut args = vec!["ls-tree", "-r", "-z", "--full-tree", commit.as_str()];
        if let Some(s) = &subdir {
            args.push("--");
            args.push(s);
        }
        let listing = git(repo, &args)?;
        let mut src = GitSource {
            repo: repo.to_path_buf(),
            rev: rev.to_string(),
            commit,
            subdir,
            config: config.clone(),
            tree: BTreeMap::new(),
            last_changed: OnceLock::new(),
        };
        for rec in listing.split('\0').filter(|r| !r.is_empty()) {
            let Some((meta, path)) = rec.split_once('\t') else {
                continue;
            };
            let mut f = meta.split(' ');
            let (_mode, kind, blob) = (f.next(), f.next(), f.next());
            if kind != Some("blob") {
                continue;
            }
            let Some(rel) = src.rel(path) else { continue };
            if is_candidate(&rel, config) {
                src.tree.insert(rel, blob.unwrap_or_default().to_string());
            }
        }
        Ok(src)
    }

    pub fn commit(&self) -> &str {
        &self.commit
    }

    fn rel(&self, full: &str) -> Option<String> {
        match &self.subdir {
            None => Some(full.to_string()),
            Some(s) => full
                .strip_prefix(s.as_str())?
                .strip_prefix('/')
                .map(String::from),
        }
    }

    fn full(&self, rel: &str) -> String {
        match &self.subdir {
            None => rel.to_string(),
            Some(s) => format!("{s}/{rel}"),
        }
    }

    /// Reads many blobs with one `git cat-file --batch`.
    fn blobs(&self, ids: &[&str]) -> Result<Vec<String>> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| Error::new(format!("git: {e}")))?;
        let mut stdin = child.stdin.take().expect("piped");
        let input: String = ids.iter().map(|i| format!("{i}\n")).collect();
        let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
        let mut out = BufReader::new(child.stdout.take().expect("piped"));
        let mut texts = Vec::with_capacity(ids.len());
        for id in ids {
            let mut header = String::new();
            out.read_line(&mut header)?;
            let size: usize = header
                .split(' ')
                .nth(2)
                .and_then(|s| s.trim().parse().ok())
                .ok_or_else(|| Error::new(format!("git cat-file: {id}: {}", header.trim())))?;
            let mut buf = vec![0u8; size + 1];
            out.read_exact(&mut buf)?;
            buf.pop();
            texts.push(
                String::from_utf8(buf).map_err(|_| Error::new(format!("{id}: not UTF-8 text")))?,
            );
        }
        writer
            .join()
            .map_err(|_| Error::new("git cat-file: writer panicked"))??;
        child.wait()?;
        Ok(texts)
    }

    /// Time of the last commit touching each path, from one `git log` walk.
    fn last_changed(&self) -> &BTreeMap<String, String> {
        self.last_changed.get_or_init(|| {
            let mut args = vec![
                "log",
                "--format=%x1e%cI",
                "--name-only",
                self.commit.as_str(),
            ];
            if let Some(s) = &self.subdir {
                args.push("--");
                args.push(s);
            }
            let mut map = BTreeMap::new();
            let Ok(log) = git(&self.repo, &args) else {
                return map;
            };
            for rec in log.split('\x1e').filter(|r| !r.trim().is_empty()) {
                let mut lines = rec.lines();
                let time = lines.next().unwrap_or("").trim().to_string();
                for path in lines.filter(|l| !l.is_empty()) {
                    if let Some(rel) = self.rel(path) {
                        map.entry(rel).or_insert_with(|| time.clone());
                    }
                }
            }
            map
        })
    }

    /// Splits `PATH@REV`; plain ids read at the source revision.
    fn locate(&self, id: &str) -> Result<(String, Option<String>)> {
        if self.tree.contains_key(id) {
            return Ok((id.to_string(), None));
        }
        match id.rsplit_once('@') {
            Some((path, rev)) if !rev.is_empty() => Ok((path.to_string(), Some(rev.to_string()))),
            _ => Err(Error::new(format!("{id}: no such object at {}", self.rev))),
        }
    }
}

impl Source for GitSource {
    fn describe(&self) -> Result<Describe> {
        Ok(Describe {
            name: "git".to_string(),
            version: sqldiff_core::TOOL_VERSION.to_string(),
            protocol: crate::protocol::PROTOCOL_VERSION,
            capabilities: ["list_objects", "get_artifact", "get_pairs", "get_history"]
                .map(String::from)
                .to_vec(),
            source: format!(
                "{}@{} ({}){}",
                self.repo.display(),
                self.rev,
                &self.commit[..self.commit.len().min(12)],
                self.subdir
                    .as_ref()
                    .map(|s| format!(" //{s}"))
                    .unwrap_or_default()
            ),
        })
    }

    fn list_objects(&self, filter: &ObjectFilter) -> Result<Vec<ObjectInfo>> {
        let ids: Vec<&str> = self.tree.values().map(String::as_str).collect();
        let texts = self.blobs(&ids)?;
        let changed = self.last_changed();
        Ok(self
            .tree
            .keys()
            .zip(texts)
            .map(|(rel, text)| object_info(rel, &text, &self.config, changed.get(rel).cloned()))
            .filter(|o| filter.matches(o))
            .collect())
    }

    fn get_artifact(&self, id: &str) -> Result<ArtifactData> {
        let (rel, rev) = self.locate(id)?;
        let commit = match &rev {
            Some(r) => git(
                &self.repo,
                &["rev-parse", "--verify", &format!("{r}^{{commit}}")],
            )?
            .trim()
            .to_string(),
            None => self.commit.clone(),
        };
        let full = self.full(&rel);
        let blob = match (&rev, self.tree.get(&rel)) {
            (None, Some(b)) => b.clone(),
            _ => git(
                &self.repo,
                &["rev-parse", "--verify", &format!("{commit}:{full}")],
            )?
            .trim()
            .to_string(),
        };
        let text = self.blobs(&[&blob])?.remove(0);
        let last = git(
            &self.repo,
            &[
                "log",
                "-1",
                "--format=%H%x1f%cI%x1f%an",
                &commit,
                "--",
                &full,
            ],
        )?;
        let mut f = last.trim().split('\x1f');
        let (revision, time, author) = (f.next(), f.next(), f.next());
        let mut info = object_info(&rel, &text, &self.config, time.map(String::from));
        info.id = id.to_string();
        Ok(ArtifactData {
            info,
            text,
            provenance: Provenance {
                author_kind: "unknown".to_string(),
                author: author.map(String::from),
                time: time.map(String::from),
                revision: revision.map(String::from),
            },
            metadata: BTreeMap::from([("commit".to_string(), commit)]),
        })
    }

    fn get_pairs(&self, scope: &PairScope) -> Result<Vec<Pair>> {
        let objects = self.list_objects(&ObjectFilter::default())?;
        Ok(pairing::pair(&objects, &self.config, scope)?.pairs)
    }

    fn get_history(&self, id: &str) -> Result<Vec<HistoryEntry>> {
        let (rel, rev) = self.locate(id)?;
        let start = rev.unwrap_or_else(|| self.commit.clone());
        let log = git(
            &self.repo,
            &[
                "log",
                "--follow",
                "--name-only",
                "--format=%x1e%H%x1f%cI%x1f%an%x1f%s",
                &start,
                "--",
                &self.full(&rel),
            ],
        )?;
        let mut out = Vec::new();
        for rec in log.split('\x1e').filter(|r| !r.trim().is_empty()) {
            let mut lines = rec.lines();
            let head = lines.next().unwrap_or("");
            let mut f = head.split('\x1f');
            let (Some(h), time, author, subject) = (f.next(), f.next(), f.next(), f.next()) else {
                continue;
            };
            let path = lines.find(|l| !l.is_empty()).and_then(|p| self.rel(p));
            let description = subject.unwrap_or("").to_string();
            out.push(HistoryEntry {
                revision: h.to_string(),
                time: time.map(String::from),
                author: author.map(String::from),
                work_items: self.config.work_items(&description),
                description,
                artifact_id: path.map(|p| format!("{p}@{h}")),
            });
        }
        Ok(out)
    }
}
