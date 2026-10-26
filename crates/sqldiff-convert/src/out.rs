//! Output text with the units written into it.

use crate::Note;

/// A unit before verification: byte ranges in input and output.
#[derive(Debug, Clone)]
pub(crate) struct RawUnit {
    pub src: (usize, usize),
    pub out: (usize, usize),
    pub notes: Vec<Note>,
    /// Set when a person or an agent wrote this unit's output by hand.
    pub written: Option<crate::Written>,
}

#[derive(Debug, Default)]
pub(crate) struct Out {
    pub text: String,
    pub units: Vec<RawUnit>,
}

impl Out {
    pub fn push(&mut self, s: &str) {
        self.text.push_str(s);
    }

    /// Writes `text` indented by `indent` spaces (every line), newline-terminated,
    /// and records it as a unit for `src`.
    pub fn unit(&mut self, src: (usize, usize), indent: usize, text: &str, notes: Vec<Note>) {
        let start = self.text.len();
        self.lines(indent, text);
        let end = self.text.len().saturating_sub(1).max(start);
        self.units.push(RawUnit {
            src,
            out: (start, end),
            notes,
            written: None,
        });
    }

    /// Writes `text` indented, newline-terminated, without a unit.
    pub fn lines(&mut self, indent: usize, text: &str) {
        let pad = " ".repeat(indent);
        for line in text.lines() {
            if !line.trim().is_empty() {
                self.text.push_str(&pad);
                self.text.push_str(line);
            }
            self.text.push('\n');
        }
    }

    /// Appends another buffer, shifting its units.
    pub fn append(&mut self, other: Out) {
        let base = self.text.len();
        self.text.push_str(&other.text);
        self.units.extend(other.units.into_iter().map(|mut u| {
            u.out = (u.out.0 + base, u.out.1 + base);
            u
        }));
    }
}
