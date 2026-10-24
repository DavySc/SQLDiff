//! A change set between two git revisions (`sqldiff changes`, the pull
//! request workflow): only the SQL files that changed, paired base to head
//! by path, then by git rename detection, then by the one schema-qualified
//! object a file defines. Files that did not change are never read.
//!
//! Nothing is dropped silently: a changed file is either paired, added,
//! removed, without a dialect, unreadable, or ignored by config, and each
//! list is reported.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sqldiff_core::{Artifact, Dialect};

use crate::git::{cat_blob_bytes, git};
use crate::walk::{classify, defined_name};
use crate::{tree, Error, Resolved, Result};

/// One side of a changed file.
#[derive(Debug, Clone, Serialize)]
pub struct Version {
    /// Path relative to the repository root, as SARIF and GitHub expect.
    pub path: String,
    pub blob: String,
    #[serde(skip)]
    pub artifact: Artifact,
}

/// How a base file was matched to a head file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum PairedBy {
    /// Same path at both revisions.
    Path,
    /// Git rename detection, with its similarity in percent.
    Rename { similarity: u32 },
    /// One file removed and one added, each defining the same single
    /// object (`relation:schema.name` or `routine:schema.name`).
    Name { name: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct ChangedPair {
    pub base: Version,
    pub head: Version,
    pub paired_by: PairedBy,
}

/// A removed or added file that could not be paired by name because
/// several files on one side define the same object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ambiguity {
    pub name: String,
    pub removed: Vec<String>,
    pub added: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ChangeSet {
    pub base: String,
    pub head: String,
    /// The diff runs from here to `head`: what the head branch changed, as a
    /// pull request shows it.
    pub merge_base: String,
    pub pairs: Vec<ChangedPair>,
    /// Head files with no base counterpart.
    pub added: Vec<Version>,
    /// Base files with no head counterpart.
    pub removed: Vec<Version>,
    /// Name pairing candidates left unpaired because they are ambiguous;
    /// their files are also in `added` and `removed`.
    pub ambiguous: Vec<Ambiguity>,
    /// Changed SQL files whose dialect is unknown (path at head, else base).
    pub no_dialect: Vec<String>,
    /// Changed SQL files that could not be read as text, with the reason.
    pub unreadable: Vec<(String, String)>,
    /// Changed SQL files the config's `ignore` excludes.
    pub ignored: Vec<String>,
    /// Changed files that configure SQLDiff (`sqldiff.toml`, `pack.toml`,
    /// or a loaded config or pack file): the results use the head's config.
    pub config_changed: Vec<String>,
    /// Every changed path, SQL or not.
    pub changed_files: usize,
}

/// What to compare.
pub struct ChangeScope<'a> {
    pub repo: &'a Path,
    pub base: &'a str,
    pub head: &'a str,
    /// Dialect of every changed file, overriding `dialect_map` and the path
    /// convention.
    pub dialect: Option<Dialect>,
    /// Git pathspecs limiting the change set; empty for the whole tree.
    pub paths: &'a [String],
}

fn commit(repo: &Path, rev: &str) -> Result<String> {
    Ok(git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{rev}^{{commit}}"),
        ],
    )?
    .trim()
    .to_string())
}

/// One record of `git diff --raw -z`.
struct Raw {
    mode_a: String,
    mode_b: String,
    blob_a: String,
    blob_b: String,
    status: char,
    score: u32,
    path_a: String,
    path_b: String,
}

fn parse_raw(out: &str) -> Result<Vec<Raw>> {
    let mut fields = out.split('\0');
    let mut recs = Vec::new();
    while let Some(meta) = fields.next() {
        if meta.is_empty() {
            continue;
        }
        let bad = || Error::new(format!("git diff: unexpected record `{meta}`"));
        let m: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
        let [mode_a, mode_b, blob_a, blob_b, st] = m[..] else {
            return Err(bad());
        };
        let status = st.chars().next().ok_or_else(bad)?;
        let score = st[1..].parse().unwrap_or(0);
        let path_a = fields.next().ok_or_else(bad)?.to_string();
        let path_b = if matches!(status, 'R' | 'C') {
            fields.next().ok_or_else(bad)?.to_string()
        } else {
            path_a.clone()
        };
        recs.push(Raw {
            mode_a: mode_a.to_string(),
            mode_b: mode_b.to_string(),
            blob_a: blob_a.to_string(),
            blob_b: blob_b.to_string(),
            status,
            score,
            path_a,
            path_b,
        });
    }
    Ok(recs)
}

/// A regular file (not a symlink or submodule).
fn regular(mode: &str) -> bool {
    mode.starts_with("100")
}

/// Text of a blob: UTF-8 (a BOM is kept, so lines and columns match the
/// file), or UTF-16 with a BOM, as SQL Server tools often save it.
pub fn decode(bytes: Vec<u8>) -> std::result::Result<String, String> {
    let utf16 = |be: bool| {
        let body = &bytes[2..];
        if body.len() % 2 != 0 {
            return Err("odd length for UTF-16 text".to_string());
        }
        let units: Vec<u16> = body
            .chunks_exact(2)
            .map(|c| {
                if be {
                    u16::from_be_bytes([c[0], c[1]])
                } else {
                    u16::from_le_bytes([c[0], c[1]])
                }
            })
            .collect();
        String::from_utf16(&units).map_err(|_| "invalid UTF-16 text".to_string())
    };
    match bytes.as_slice() {
        [0xFF, 0xFE, ..] => utf16(false),
        [0xFE, 0xFF, ..] => utf16(true),
        _ => String::from_utf8(bytes).map_err(|_| "not UTF-8 or UTF-16 text".to_string()),
    }
}

/// Files that configure SQLDiff: by name anywhere, or one the config loaded.
fn configures(path: &str, repo: &Path, loaded: &[PathBuf]) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name == "sqldiff.toml" || name == "pack.toml" {
        return true;
    }
    let Ok(full) = repo.join(path).canonicalize() else {
        return false;
    };
    loaded.contains(&full)
}

fn loaded_config_files(cfg: &Resolved) -> Vec<PathBuf> {
    cfg.packs
        .iter()
        .map(|p| p.path.clone())
        .chain(cfg.layers.iter().map(|l| PathBuf::from(&l.name)))
        .filter_map(|p| p.canonicalize().ok())
        .filter(|p| p.is_file())
        .collect()
}

/// The SQL files changed between the merge base of `base` and `head`, and
/// `head`, read and paired.
pub fn changes(scope: &ChangeScope<'_>, cfg: &Resolved) -> Result<ChangeSet> {
    let repo = scope.repo;
    let base = commit(repo, scope.base)?;
    let head = commit(repo, scope.head)?;
    let merge_base = git(repo, &["merge-base", &base, &head])
        .map_err(|e| {
            e.context(format!(
                "no common ancestor of {} and {}",
                scope.base, scope.head
            ))
        })?
        .trim()
        .to_string();
    let mut args = vec![
        "diff",
        "--raw",
        "-z",
        "--no-abbrev",
        "--no-ext-diff",
        "--no-textconv",
        "--find-renames",
        "--no-relative",
        merge_base.as_str(),
        head.as_str(),
        "--",
    ];
    args.extend(scope.paths.iter().map(String::as_str));
    let recs = parse_raw(&git(repo, &args)?)?;

    let mut set = ChangeSet {
        base,
        head,
        merge_base,
        changed_files: recs.len(),
        ..ChangeSet::default()
    };
    let loaded = loaded_config_files(cfg);
    for r in &recs {
        for p in [&r.path_a, &r.path_b] {
            if configures(p, repo, &loaded) && !set.config_changed.contains(p) {
                set.config_changed.push(p.clone());
            }
        }
    }

    // Which sides of each record are in scope.
    let sql_like =
        |p: &str| p.to_ascii_lowercase().ends_with(".sql") || cfg.mapped_dialect(p).is_some();
    let mut ignored = Vec::new();
    let mut wanted = |p: &str, mode: &str| {
        if !regular(mode) || !sql_like(p) {
            return false;
        }
        if cfg.is_ignored(p) {
            ignored.push(p.to_string());
            return false;
        }
        true
    };
    // (record, base side in scope, head side in scope)
    let mut kept: Vec<(&Raw, bool, bool)> = Vec::new();
    for r in &recs {
        let (a, b) = match r.status {
            'A' | 'C' => (false, wanted(&r.path_b, &r.mode_b)),
            'D' => (wanted(&r.path_a, &r.mode_a), false),
            _ => (wanted(&r.path_a, &r.mode_a), wanted(&r.path_b, &r.mode_b)),
        };
        if a || b {
            kept.push((r, a, b));
        }
    }
    ignored.sort();
    ignored.dedup();
    set.ignored = ignored;

    // Read every needed blob at once.
    let mut ids: Vec<&str> = Vec::new();
    for (r, a, b) in &kept {
        if *a {
            ids.push(&r.blob_a);
        }
        if *b {
            ids.push(&r.blob_b);
        }
    }
    ids.sort();
    ids.dedup();
    let texts: BTreeMap<&str, std::result::Result<String, String>> = ids
        .iter()
        .copied()
        .zip(cat_blob_bytes(repo, &ids)?.into_iter().map(decode))
        .collect();

    let version = |path: &str, blob: &str, set: &mut ChangeSet| -> Option<Version> {
        let text = match &texts[blob] {
            Ok(t) => t.clone(),
            Err(e) => {
                set.unreadable.push((path.to_string(), e.clone()));
                return None;
            }
        };
        if scope.dialect.is_none() && classify(path, cfg).0.is_none() {
            set.no_dialect.push(path.to_string());
            return None;
        }
        let artifact = match tree::artifact(path.to_string(), path, scope.dialect, text, cfg) {
            Ok(a) => a,
            Err(_) => {
                set.no_dialect.push(path.to_string());
                return None;
            }
        };
        Some(Version {
            path: path.to_string(),
            blob: blob.to_string(),
            artifact,
        })
    };

    for (r, a, b) in kept {
        let va = if a {
            version(&r.path_a, &r.blob_a, &mut set)
        } else {
            None
        };
        let vb = if b {
            version(&r.path_b, &r.blob_b, &mut set)
        } else {
            None
        };
        match (va, vb) {
            (Some(base), Some(head)) => set.pairs.push(ChangedPair {
                base,
                head,
                paired_by: if r.status == 'R' {
                    PairedBy::Rename {
                        similarity: r.score,
                    }
                } else {
                    PairedBy::Path
                },
            }),
            (Some(base), None) => {
                // A modified file whose head side is unreadable or has no
                // dialect is reported there, not as removed.
                if !b {
                    set.removed.push(base);
                }
            }
            // A base side that has no dialect or is unreadable is listed
            // there; the head file is still checked on its own.
            (None, Some(head)) => set.added.push(head),
            (None, None) => {}
        }
    }
    pair_by_name(&mut set);
    set.no_dialect.sort();
    set.no_dialect.dedup();
    Ok(set)
}

/// Pairs removed and added files that define the same single object, when
/// exactly one file on each side does.
fn pair_by_name(set: &mut ChangeSet) {
    let names = |vs: &[Version]| -> BTreeMap<String, Vec<usize>> {
        let mut m: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, v) in vs.iter().enumerate() {
            if let Some(n) = defined_name(&v.artifact.text, v.artifact.dialect) {
                m.entry(n).or_default().push(i);
            }
        }
        m
    };
    let removed = names(&set.removed);
    let added = names(&set.added);
    let mut take: Vec<(usize, usize, String)> = Vec::new();
    for (name, rs) in &removed {
        let Some(adds) = added.get(name) else {
            continue;
        };
        if rs.len() == 1 && adds.len() == 1 {
            take.push((rs[0], adds[0], name.clone()));
        } else {
            set.ambiguous.push(Ambiguity {
                name: name.clone(),
                removed: rs.iter().map(|&i| set.removed[i].path.clone()).collect(),
                added: adds.iter().map(|&i| set.added[i].path.clone()).collect(),
            });
        }
    }
    let mut removed_out = vec![false; set.removed.len()];
    let mut added_out = vec![false; set.added.len()];
    for (r, a, name) in take {
        removed_out[r] = true;
        added_out[a] = true;
        set.pairs.push(ChangedPair {
            base: set.removed[r].clone(),
            head: set.added[a].clone(),
            paired_by: PairedBy::Name { name },
        });
    }
    let keep = |vs: &mut Vec<Version>, out: &[bool]| {
        let mut i = 0;
        vs.retain(|_| {
            i += 1;
            !out[i - 1]
        });
    };
    keep(&mut set.removed, &removed_out);
    keep(&mut set.added, &added_out);
}

/// Where a base line is at head, from a line diff of the two texts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadLine {
    /// The line is unchanged at this head line.
    Same(usize),
    /// The line was removed or replaced. `after` is the head line before
    /// the gap (0 at the start); `replaced` says head has new lines there.
    Gap { after: usize, replaced: bool },
}

/// Maps base lines to head lines (1-based), from a Myers line diff.
#[derive(Debug, Clone)]
pub struct LineMap {
    /// Per base line: the head line it is matched to, if any.
    matched: Vec<Option<usize>>,
    head_lines: usize,
}

/// The diff keeps `2d + 1` entries per edit step `d`, so memory grows with
/// the square of the edit distance. Past this many entries (128 MB, about
/// 2 800 inserted plus deleted lines) it stops: a file rewritten this much
/// has no useful line correspondence, and the caller says so.
const MAX_TRACE: usize = 16 << 20;

impl LineMap {
    /// `None` when the texts differ too much to map (see [`MAX_TRACE`]).
    pub fn new(base: &str, head: &str) -> Option<LineMap> {
        let a: Vec<&str> = base.lines().collect();
        let b: Vec<&str> = head.lines().collect();
        let mut matched = vec![None; a.len()];
        let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
        let suf = a[pre..]
            .iter()
            .rev()
            .zip(b[pre..].iter().rev())
            .take_while(|(x, y)| x == y)
            .count();
        for (i, m) in matched.iter_mut().enumerate().take(pre) {
            *m = Some(i + 1);
        }
        for k in 0..suf {
            matched[a.len() - 1 - k] = Some(b.len() - k);
        }
        let (ma, mb) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
        for (x, y) in myers(ma, mb)? {
            matched[pre + x] = Some(pre + y + 1);
        }
        Some(LineMap {
            matched,
            head_lines: b.len(),
        })
    }

    pub fn head_lines(&self) -> usize {
        self.head_lines
    }

    /// Where base line `line` (1-based) is at head.
    pub fn map(&self, line: usize) -> HeadLine {
        let i = line.saturating_sub(1).min(self.matched.len());
        if let Some(Some(n)) = self.matched.get(i) {
            return HeadLine::Same(*n);
        }
        let after = self.matched[..i].iter().rev().find_map(|m| *m).unwrap_or(0);
        let next = self.matched[i..]
            .iter()
            .find_map(|m| *m)
            .unwrap_or(self.head_lines + 1);
        HeadLine::Gap {
            after,
            replaced: next > after + 1,
        }
    }
}

/// Matched line index pairs of a shortest edit script (Myers 1986).
fn myers(a: &[&str], b: &[&str]) -> Option<Vec<(usize, usize)>> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m) as usize;
    if max == 0 {
        return Some(Vec::new());
    }
    let off = max as isize + 1;
    let mut v = vec![0isize; 2 * max + 3];
    // trace[d] holds v[k] for k in -d..=d as it was before step d.
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut kept = 0usize;
    let mut found = None;
    'outer: for d in 0..=max as isize {
        kept += 2 * d as usize + 1;
        if kept > MAX_TRACE {
            return None;
        }
        trace.push(v[(off - d) as usize..=(off + d) as usize].to_vec());
        let mut k = -d;
        while k <= d {
            let mut x =
                if k == -d || (k != d && v[(off + k - 1) as usize] < v[(off + k + 1) as usize]) {
                    v[(off + k + 1) as usize]
                } else {
                    v[(off + k - 1) as usize] + 1
                };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[(off + k) as usize] = x;
            if x >= n && y >= m {
                found = Some(d);
                break 'outer;
            }
            k += 2;
        }
    }
    let mut out = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (1..=found?).rev() {
        let t = &trace[d as usize];
        let at = |k: isize| t[(k + d) as usize];
        let k = x - y;
        let pk = if k == -d || (k != d && at(k - 1) < at(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let px = at(pk);
        let py = px - pk;
        // The diagonal that followed the edit at step d.
        while x > px && y > py {
            x -= 1;
            y -= 1;
            out.push((x as usize, y as usize));
        }
        x = px;
        y = py;
    }
    // Leading diagonal at d = 0.
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        out.push((x as usize, y as usize));
    }
    out.reverse();
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(a: &str, b: &str) {
        let la: Vec<&str> = a.lines().collect();
        let lb: Vec<&str> = b.lines().collect();
        let map = LineMap::new(a, b).unwrap();
        let mut last = 0;
        let mut kept = 0;
        for (i, m) in map.matched.iter().enumerate() {
            if let Some(n) = m {
                assert_eq!(la[i], lb[n - 1], "line {} maps to an unequal line", i + 1);
                assert!(*n > last, "matches out of order");
                last = *n;
                kept += 1;
            }
        }
        // Optimal: as many matches as the longest common subsequence.
        let mut dp = vec![vec![0usize; lb.len() + 1]; la.len() + 1];
        for i in (0..la.len()).rev() {
            for j in (0..lb.len()).rev() {
                dp[i][j] = if la[i] == lb[j] {
                    dp[i + 1][j + 1] + 1
                } else {
                    dp[i + 1][j].max(dp[i][j + 1])
                };
            }
        }
        assert_eq!(kept, dp[0][0], "{a:?} -> {b:?}");
    }

    #[test]
    fn line_map_is_a_longest_common_subsequence() {
        check("a\nb\nc\nd\n", "a\nc\nd\n");
        check("a\nb\nc\n", "x\na\nb\ny\nc\nz\n");
        check("", "a\nb\n");
        check("a\nb\n", "");
        check("a\nb\nc\na\nb\nb\na\n", "c\nb\na\nb\na\nc\n");
        check("x\ny\nz\n", "p\nq\n");
        // Deterministic pseudo-random cases over a small alphabet.
        let mut s = 7u64;
        let mut next = || {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (s >> 33) as usize
        };
        for _ in 0..300 {
            let (la, lb) = (next() % 12, next() % 12);
            let a: String = (0..la)
                .map(|_| ["a\n", "b\n", "c\n", "d\n"][next() % 4])
                .collect();
            let b: String = (0..lb)
                .map(|_| ["a\n", "b\n", "c\n", "d\n"][next() % 4])
                .collect();
            check(&a, &b);
        }
    }

    #[test]
    fn removed_and_replaced_lines() {
        let m = LineMap::new("a\nb\nc\nd\n", "a\nd\n").unwrap();
        assert_eq!(m.map(1), HeadLine::Same(1));
        assert_eq!(
            m.map(2),
            HeadLine::Gap {
                after: 1,
                replaced: false
            }
        );
        assert_eq!(m.map(4), HeadLine::Same(2));
        let m = LineMap::new("a\nb\nc\n", "a\nx\nc\n").unwrap();
        assert_eq!(
            m.map(2),
            HeadLine::Gap {
                after: 1,
                replaced: true
            }
        );
        let m = LineMap::new("a\nb\n", "b\n").unwrap();
        assert_eq!(
            m.map(1),
            HeadLine::Gap {
                after: 0,
                replaced: false
            }
        );
    }

    #[test]
    fn a_full_rewrite_gives_up_quickly() {
        let a: String = (0..9_000).map(|i| format!("a{i}\n")).collect();
        let b: String = (0..9_000).map(|i| format!("b{i}\n")).collect();
        let t = std::time::Instant::now();
        assert!(LineMap::new(&a, &b).is_none());
        assert!(t.elapsed() < std::time::Duration::from_secs(5));
        // Small edits in a large file still map.
        let c = a.replacen("a4500\n", "", 1);
        assert_eq!(
            LineMap::new(&a, &c).unwrap().map(4502),
            HeadLine::Same(4501)
        );
    }

    #[test]
    fn decodes_utf16_with_bom() {
        let mut b = vec![0xFF, 0xFE];
        for u in "SELECT 1;\n".encode_utf16() {
            b.extend(u.to_le_bytes());
        }
        assert_eq!(decode(b).unwrap(), "SELECT 1;\n");
        assert!(decode(vec![0xC3, 0x28]).is_err());
    }

    #[test]
    fn raw_records() {
        let out = ":100644 100644 aa bb M\0x.sql\0:100644 100644 cc dd R087\0old.sql\0new.sql\0:000000 100644 0000 ee A\0n.sql\0";
        let r = parse_raw(out).unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!((r[1].status, r[1].score), ('R', 87));
        assert_eq!(
            (r[1].path_a.as_str(), r[1].path_b.as_str()),
            ("old.sql", "new.sql")
        );
        assert_eq!(r[2].path_b, "n.sql");
    }
}
