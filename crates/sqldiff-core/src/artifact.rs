//! Artifacts (layer 0 input) and their parsed form.

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
}

impl Artifact {
    pub fn new(path: impl Into<String>, dialect: Dialect, text: impl Into<String>) -> Self {
        Artifact {
            path: path.into(),
            dialect,
            version: dialect.default_version().to_string(),
            text: text.into(),
        }
    }
}

/// Content hash with line endings, BOM and trailing whitespace normalized.
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
}

impl<'a> Analyzed<'a> {
    pub fn new(artifact: &'a Artifact, side: Side, excerpt_lines: usize) -> Self {
        let parse = parse(&artifact.text, artifact.dialect);
        let root = parse.syntax();
        let object = root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::ROUTINE)
            .and_then(|r| ast::routine_name(&r))
            .unwrap_or_else(|| file_stem(&artifact.path));
        Analyzed {
            artifact,
            side,
            lines: LineIndex::new(&artifact.text),
            parse,
            root,
            object,
            excerpt_lines,
        }
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
        let (start_line, start_col) = self.lines.line_col(start);
        let (end_line, end_col) = self.lines.line_col(end);
        let excerpt = self.artifact.text[start..end]
            .lines()
            .take(self.excerpt_lines.max(1))
            .map(|l| l.trim_end().to_string())
            .collect();
        Location {
            side: self.side,
            path: self.artifact.path.clone(),
            start_line,
            start_col,
            end_line,
            end_col,
            byte_start: start,
            byte_end: end,
            excerpt,
        }
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
