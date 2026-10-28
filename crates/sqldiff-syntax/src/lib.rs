//! Lossless syntax layer for SQLDiff.
//!
//! `parse(text, dialect)` produces a concrete syntax tree in which every byte
//! of the input is owned by exactly one token, so `tree.text() == text`.
//! Structure goes down to statements, including procedural compound
//! statements (blocks, IF, loops, handlers, TRY/CATCH, PL/pgSQL exception
//! sections). PostgreSQL routine bodies in dollar-quoted strings are parsed
//! as nested PL/pgSQL or SQL with offsets in the outer artifact.

pub mod abstract_syntax;
pub mod ast;
pub mod dialect;
pub mod kind;
pub mod lexer;
mod parser;
pub mod print;
pub mod template;

pub use dialect::{Dialect, IdentFolding};
pub use kind::{SqlLanguage, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};
pub use parser::{parse, Parse};

/// A lexing or parsing problem at a byte offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub offset: usize,
    pub message: String,
}

/// Maps byte offsets to 1-based line and column numbers.
#[derive(Debug, Clone)]
pub struct LineIndex {
    line_starts: Vec<usize>,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        LineIndex { line_starts }
    }

    /// 1-based (line, column); the column counts bytes.
    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        let line = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        (line + 1, offset - self.line_starts[line] + 1)
    }

    /// Byte offset where 1-based `line` starts; past the last line, the
    /// start of the last line.
    pub fn line_start(&self, line: usize) -> usize {
        let i = line.max(1) - 1;
        self.line_starts[i.min(self.line_starts.len() - 1)]
    }
}
