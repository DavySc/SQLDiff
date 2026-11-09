//! Artifacts (layer 0 input) and their parsed form.

use std::cell::OnceCell;
use std::collections::HashSet;

use sha2::{Digest, Sha256};
use sqldiff_syntax::{ast, parse, Dialect, LineIndex, Parse, SyntaxKind, SyntaxNode};

use crate::model::{hex, ArtifactInfo, DiagnosticInfo, Location, Side};

/// One SQL artifact: text plus the dialect it is written in.
#[derive(Debug, Clone)]
pub struct Artifact {
    /// Display path or identifier.
    pub path: String,
    pub dialect: Dialect,
    /// Target engine version; defaults to the dialect's default version.
    pub version: String,
    pub text: String,
    /// Object identity when the text is one object of a larger file (see
    /// `sqldiff_source::split`); otherwise derived from the text or path.
    pub object: Option<String>,
}

impl Artifact {
    pub fn new(path: impl Into<String>, dialect: Dialect, text: impl Into<String>) -> Self {
        Artifact {
            path: path.into(),
            dialect,
            version: dialect.default_version().to_string(),
            text: text.into(),
            object: None,
        }
    }

    /// Sets the target version after validating it against the lifecycle
    /// data ([`crate::target::Target::parse`]); stores the canonical
    /// spelling.
    pub fn set_version(&mut self, version: &str) -> Result<(), String> {
        self.version = crate::target::Target::parse(self.dialect, version)?.canonical();
        crate::target::record_use(self.dialect, &self.version);
        Ok(())
    }
}

/// Content hash with line endings, BOM and trailing whitespace normalized.
/// The location of `text[start..end]`, with line and column and an excerpt
/// of at most `excerpt_lines` lines, as findings carry it.
pub fn location_in(
    side: Side,
    path: &str,
    text: &str,
    lines: &LineIndex,
    (start, end): (usize, usize),
    excerpt_lines: usize,
) -> Location {
    let (start_line, start_col) = lines.line_col(start);
    let (end_line, end_col) = lines.line_col(end);
    Location {
        side,
        path: path.to_string(),
        start_line,
        start_col,
        end_line,
        end_col,
        byte_start: start,
        byte_end: end,
        excerpt: text[start..end]
            .lines()
            .take(excerpt_lines.max(1))
            .map(|l| l.trim_end().to_string())
            .collect(),
    }
}

/// A hash of the exact bytes. For cache keys: [`content_hash`] ignores
/// trailing white space and line ends, which a multi-line string literal
/// can depend on.
pub fn exact_hash(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}

/// A hash of the text as a reader sees it: a byte order mark, line ends and
/// trailing white space do not count. An identity for objects and queues,
/// never a key for a cache that holds anything computed from the text.
pub fn content_hash(text: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut h = Sha256::new();
    for line in text.lines() {
        h.update(line.trim_end().as_bytes());
        h.update(b"\n");
    }
    hex(&h.finalize())
}

/// An artifact after parsing, with helpers to build locations.
pub struct Analyzed<'a> {
    pub artifact: &'a Artifact,
    pub side: Side,
    pub parse: Parse,
    pub root: SyntaxNode,
    pub lines: LineIndex,
    pub object: String,
    pub excerpt_lines: usize,
    /// Cache of `temp_table_names`: a pure function of `root`. Invariant:
    /// `root` and `parse` are never reassigned or mutated after `new` (the
    /// tree is immutable and `artifact` is borrowed), so the cached set is
    /// exactly what a fresh scan of `root` would return. Changed text
    /// (for example a blanked copy of a trigger) needs a new `Analyzed`,
    /// which gets its own cache. Keep this field private.
    temp_names: OnceCell<HashSet<String>>,
}

impl<'a> Analyzed<'a> {
    pub fn new(artifact: &'a Artifact, side: Side, excerpt_lines: usize) -> Self {
        let parse = parse(&artifact.text, artifact.dialect);
        let root = parse.syntax();
        let object = artifact
            .object
            .clone()
            .or_else(|| {
                root.descendants()
                    .find(|n| n.kind() == SyntaxKind::ROUTINE)
                    .and_then(|r| ast::routine_name(&r))
            })
            .unwrap_or_else(|| file_stem(&artifact.path));
        Analyzed {
            artifact,
            side,
            lines: LineIndex::new(&artifact.text),
            parse,
            root,
            object,
            excerpt_lines,
            temp_names: OnceCell::new(),
        }
    }

    /// Lower-case names of the temporary tables `root` names as `pg_temp.x`
    /// or `SESSION.x`: where a converted T-SQL table variable may have gone.
    /// Scans the whole tree on the first call and caches the set (see the
    /// invariant on `temp_names`); later calls are free. Callers only test
    /// membership, so the set's order never reaches an output.
    pub fn temp_table_names(&self) -> &HashSet<String> {
        self.temp_names.get_or_init(|| {
            let toks: Vec<_> = ast::sig_tokens(&self.root).collect();
            toks.windows(3)
                .filter(|w| {
                    (w[0].text().eq_ignore_ascii_case("pg_temp")
                        || w[0].text().eq_ignore_ascii_case("SESSION"))
                        && w[1].kind() == SyntaxKind::DOT
                        && w[2].kind() == SyntaxKind::IDENT
                })
                .map(|w| w[2].text().to_lowercase())
                .collect()
        })
    }

    pub fn dialect(&self) -> Dialect {
        self.artifact.dialect
    }

    pub fn info(&self) -> ArtifactInfo {
        ArtifactInfo {
            path: self.artifact.path.clone(),
            dialect: self.artifact.dialect.id().to_string(),
            dialect_version: self.artifact.version.clone(),
            object: self.object.clone(),
            content_hash: content_hash(&self.artifact.text),
        }
    }

    pub fn diagnostics(&self) -> Vec<DiagnosticInfo> {
        self.parse
            .diagnostics
            .iter()
            .map(|d| {
                let (line, col) = self.lines.line_col(d.offset);
                DiagnosticInfo {
                    side: self.side,
                    line,
                    col,
                    message: d.message.clone(),
                }
            })
            .collect()
    }

    pub fn location(&self, start: usize, end: usize) -> Location {
        location_in(
            self.side,
            &self.artifact.path,
            &self.artifact.text,
            &self.lines,
            (start, end),
            self.excerpt_lines,
        )
    }

    pub fn node_location(&self, node: &SyntaxNode) -> Location {
        let (s, e) = ast::sig_range(node);
        self.location(s, e)
    }

    pub fn line_of(&self, node: &SyntaxNode) -> usize {
        self.lines.line_col(ast::sig_range(node).0).0
    }

    /// Single-line excerpt of a node, whitespace collapsed and capped.
    pub fn snippet(&self, node: &SyntaxNode, max: usize) -> String {
        snippet_of(
            &ast::sig_tokens(node)
                .map(|t| t.text().to_string())
                .collect::<Vec<_>>(),
            max,
        )
    }
}

/// Joins token texts with single spaces and caps the length.
pub fn snippet_of(tokens: &[String], max: usize) -> String {
    let mut out = String::new();
    for (i, t) in tokens.iter().enumerate() {
        let glue = i > 0
            && !matches!(t.as_str(), "," | ")" | ";" | "." | "::")
            && !out.ends_with('(')
            && !out.ends_with('.')
            && !out.ends_with("::");
        if glue {
            out.push(' ');
        }
        out.push_str(t);
        if out.chars().count() > max {
            let cut: String = out.chars().take(max).collect();
            return format!("{}…", cut.trim_end());
        }
    }
    out
}

fn file_stem(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.split('.').next().unwrap_or(name).to_string()
}
