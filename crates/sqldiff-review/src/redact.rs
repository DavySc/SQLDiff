//! Redacted export (design section 5.5): pack-derived identifiers (object
//! names, schema names, paths, work-item ids, helper and limit names, pack
//! names) and deny-listed terms are replaced by stable placeholders, so
//! findings can be shared outside the environment they came from.
//!
//! A placeholder is `KIND_XXXXXXXX`, a keyed hash of the term: the same term
//! gets the same placeholder in every export made with the same salt, and
//! the term cannot be recovered by guessing without the salt. The salt lives
//! in the data directory (`redact.salt`), created on first use.
//!
//! Identifiers are replaced as whole words (SQL identifier characters are
//! `[A-Za-z0-9_@#$]`), deny-listed terms anywhere, case-insensitively.
//! [`Redactor::leaks`] re-checks an output; exports refuse to write one
//! that still contains a term.

use std::collections::HashMap;

use regex::{Regex, RegexBuilder};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqldiff_core::model::hex;
use sqldiff_core::Report;
use sqldiff_source::{Error, Resolved, Result};

pub const SALT_FILE: &str = "redact.salt";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Object,
    Schema,
    Path,
    WorkItem,
    Helper,
    Rule,
    Pack,
    /// A deny-listed term; matched anywhere, not only as a whole word.
    Term,
}

impl Kind {
    fn prefix(self) -> &'static str {
        match self {
            Kind::Object => "OBJ",
            Kind::Schema => "SCHEMA",
            Kind::Path => "PATH",
            Kind::WorkItem => "WI",
            Kind::Helper => "HELPER",
            Kind::Rule => "RULE",
            Kind::Pack => "PACK",
            Kind::Term => "TERM",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Remove source excerpts instead of redacting them.
    pub drop_excerpts: bool,
}

#[derive(Debug, Clone)]
struct Entry {
    placeholder: String,
    whole_word: bool,
}

#[derive(Debug, Clone)]
pub struct Redactor {
    salt: Vec<u8>,
    /// Lower-cased term to its entry.
    terms: HashMap<String, Entry>,
    work_items: Vec<Regex>,
    matcher: Option<Regex>,
    pub options: Options,
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '@' | '#' | '$')
}

/// Strips `"x"`, `[x]` and `` `x` `` quoting from one identifier part.
fn unquote(s: &str) -> &str {
    let s = s.trim();
    for (o, c) in [('"', '"'), ('[', ']'), ('`', '`')] {
        if let Some(inner) = s.strip_prefix(o).and_then(|x| x.strip_suffix(c)) {
            return inner;
        }
    }
    s
}

impl Redactor {
    /// A redactor seeded with the configuration's pack-derived terms.
    pub fn new(cfg: &Resolved, salt: &[u8]) -> Redactor {
        let mut r = Redactor {
            salt: salt.to_vec(),
            terms: HashMap::new(),
            work_items: Vec::new(),
            matcher: None,
            options: Options::default(),
        };
        for t in &cfg.policy.deny_list {
            r.add(Kind::Term, t);
        }
        for h in &cfg.policy.helpers {
            r.add(Kind::Helper, &h.name);
        }
        for l in &cfg.policy.limits {
            r.add(Kind::Rule, &l.id);
        }
        for p in &cfg.packs {
            r.add(Kind::Pack, &p.meta.name);
            r.add(Kind::Path, &p.path.display().to_string());
            if let Some(dir) = p.path.parent() {
                r.add(Kind::Path, &dir.display().to_string());
            }
        }
        for l in &cfg.layers {
            if l.name.contains('/') || l.name.contains('\\') {
                r.add(Kind::Path, &l.name);
            }
        }
        for c in &cfg.policy.change_patterns {
            if let Ok(re) = Regex::new(&c.pattern) {
                r.work_items.push(re);
            }
        }
        r
    }

    /// A redactor whose salt is kept in the configured data directory.
    pub fn from_config(cfg: &Resolved) -> Result<Redactor> {
        Ok(Redactor::new(cfg, &salt(&cfg.data_dir)?))
    }

    /// Adds a term. Terms shorter than two characters are ignored.
    pub fn add(&mut self, kind: Kind, term: &str) {
        let term = term.trim();
        if term.chars().count() < 2 {
            return;
        }
        let lower = term.to_lowercase();
        if self.terms.contains_key(&lower) {
            return;
        }
        let mut h = Sha256::new();
        h.update(&self.salt);
        h.update(kind.prefix().as_bytes());
        h.update([0]);
        h.update(lower.as_bytes());
        let mut placeholder = format!(
            "{}_{}",
            kind.prefix(),
            hex(&h.finalize()[..4]).to_uppercase()
        );
        if kind == Kind::Path {
            if let Some(ext) = term
                .rsplit_once('.')
                .map(|(_, e)| e)
                .filter(|e| e.len() <= 4 && !e.contains(['/', '\\']))
            {
                placeholder.push('.');
                placeholder.push_str(&ext.to_lowercase());
            }
        }
        self.terms.insert(
            lower,
            Entry {
                placeholder,
                whole_word: kind != Kind::Term && kind != Kind::Path,
            },
        );
        self.matcher = None;
    }

    /// Adds a possibly qualified object name: every part is a term, the
    /// last as an object, the others as schemas (or databases).
    pub fn add_object(&mut self, name: &str) {
        let parts: Vec<&str> = name
            .split('.')
            .map(unquote)
            .filter(|p| !p.is_empty())
            .collect();
        if parts.len() > 1 {
            self.add(Kind::Object, &parts.join("."));
        }
        for (i, p) in parts.iter().enumerate() {
            let kind = if i + 1 == parts.len() {
                Kind::Object
            } else {
                Kind::Schema
            };
            self.add(kind, p);
        }
    }

    /// Adds a path, and its file stem as an object name.
    pub fn add_path(&mut self, path: &str) {
        self.add(Kind::Path, path);
        let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
        if let Some(stem) = file.split('.').next() {
            self.add(Kind::Object, stem);
        }
    }

    /// Adds the work items the pack change patterns find in a text.
    pub fn add_work_items(&mut self, text: &str) {
        let mut found = Vec::new();
        for re in &self.work_items {
            for c in re.captures_iter(text) {
                if let Some(m) = c.get(1).or_else(|| c.get(0)) {
                    found.push(m.as_str().to_string());
                }
            }
        }
        for w in found {
            self.add(Kind::WorkItem, &w);
        }
    }

    /// Adds everything identifying in a report: objects, schemas, paths,
    /// and work items mentioned in any of its text.
    pub fn add_report(&mut self, r: &Report) {
        for info in std::iter::once(&r.a).chain(r.b.as_ref()) {
            self.add_object(&info.object);
            self.add_path(&info.path);
        }
        let v = serde_json::to_value(r).expect("report serializes");
        let mut texts = Vec::new();
        collect_strings(&v, &mut texts);
        for t in texts {
            self.add_work_items(&t);
        }
    }

    fn matcher(&mut self) -> Option<&Regex> {
        if self.matcher.is_none() && !self.terms.is_empty() {
            let mut keys: Vec<&String> = self.terms.keys().collect();
            // Longest first, so a qualified name or a path wins over its parts.
            keys.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
            let alt = keys
                .iter()
                .map(|k| regex::escape(k))
                .collect::<Vec<_>>()
                .join("|");
            self.matcher = Some(
                RegexBuilder::new(&alt)
                    .case_insensitive(true)
                    .size_limit(64 << 20)
                    .build()
                    .expect("escaped alternation compiles"),
            );
        }
        self.matcher.as_ref()
    }

    /// Matches of registered terms, as (start, end, placeholder).
    fn matches(&mut self, s: &str) -> Vec<(usize, usize, String)> {
        let Some(re) = self.matcher().cloned() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut pos = 0;
        while pos <= s.len() {
            let Some(m) = re.find_at(s, pos) else { break };
            let e = &self.terms[&m.as_str().to_lowercase()];
            let before = s[..m.start()].chars().next_back();
            let after = s[m.end()..].chars().next();
            let bounded =
                !e.whole_word || (!before.is_some_and(is_ident) && !after.is_some_and(is_ident));
            if bounded {
                out.push((m.start(), m.end(), e.placeholder.clone()));
                pos = m.end();
            } else {
                pos = m.start() + s[m.start()..].chars().next().map_or(1, char::len_utf8);
            }
            if m.end() == m.start() {
                pos += 1;
            }
        }
        out
    }

    pub fn redact_str(&mut self, s: &str) -> String {
        let ms = self.matches(s);
        if ms.is_empty() {
            return s.to_string();
        }
        let mut out = String::with_capacity(s.len());
        let mut last = 0;
        for (a, b, p) in ms {
            out.push_str(&s[last..a]);
            out.push_str(&p);
            last = b;
        }
        out.push_str(&s[last..]);
        out
    }

    /// Redacts every string in a JSON value, keys included.
    pub fn redact_value(&mut self, v: &mut Value) {
        match v {
            Value::String(s) => *s = self.redact_str(s),
            Value::Array(a) => a.iter_mut().for_each(|x| self.redact_value(x)),
            Value::Object(o) => {
                let old = std::mem::take(o);
                for (k, mut x) in old {
                    if self.options.drop_excerpts && k == "excerpt" {
                        x = Value::Array(Vec::new());
                    }
                    self.redact_value(&mut x);
                    o.insert(self.redact_str(&k), x);
                }
            }
            _ => {}
        }
    }

    /// A redacted copy of a report. Registers the report's own terms first.
    pub fn redact_report(&mut self, r: &Report) -> Report {
        self.add_report(r);
        let mut v = serde_json::to_value(r).expect("report serializes");
        self.redact_value(&mut v);
        serde_json::from_value(v).expect("redacted report deserializes")
    }

    /// Registered terms still present in `s` (lower-cased, sorted).
    pub fn leaks(&mut self, s: &str) -> Vec<String> {
        let mut found: Vec<String> = self
            .matches(s)
            .into_iter()
            .map(|(a, b, _)| s[a..b].to_lowercase())
            .collect();
        found.sort();
        found.dedup();
        found
    }

    /// Errors if `s` still contains a registered term. The error names how
    /// many, not which, so it can be logged.
    pub fn verify(&mut self, s: &str) -> Result<()> {
        let n = self.leaks(s).len();
        if n > 0 {
            return Err(Error::new(format!(
                "redacted export still contains {n} identifying term(s); not written"
            )));
        }
        Ok(())
    }

    pub fn term_count(&self) -> usize {
        self.terms.len()
    }
}

fn collect_strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| collect_strings(x, out)),
        Value::Object(o) => o.values().for_each(|x| collect_strings(x, out)),
        _ => {}
    }
}

/// The redaction salt of a data directory, created on first use.
pub fn salt(data_dir: &std::path::Path) -> Result<Vec<u8>> {
    use chacha20poly1305::aead::rand_core::RngCore;
    let path = data_dir.join(SALT_FILE);
    match std::fs::read(&path) {
        Ok(b) if b.len() >= 16 => return Ok(b),
        Ok(_) => return Err(Error::new(format!("{}: salt too short", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::new(format!("{}: {e}", path.display()))),
    }
    let mut b = vec![0u8; 32];
    chacha20poly1305::aead::OsRng.fill_bytes(&mut b);
    crate::store::write_atomic(&path, &b)?;
    Ok(b)
}
