//! Output text with the units written into it.

use crate::{Note, Step};

/// A unit before verification: byte ranges in input and output.
#[derive(Debug, Clone)]
pub(crate) struct RawUnit {
    pub src: (usize, usize),
    pub out: (usize, usize),
    pub notes: Vec<Note>,
    /// The rewrites that produced this unit's output.
    pub trace: Vec<Step>,
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
    pub fn unit(
        &mut self,
        src: (usize, usize),
        indent: usize,
        text: &str,
        notes: Vec<Note>,
        trace: Vec<Step>,
    ) {
        let start = self.text.len();
        self.lines(indent, text);
        let end = self.text.len().saturating_sub(1).max(start);
        self.units.push(RawUnit {
            src,
            out: (start, end),
            notes,
            trace,
            written: None,
        });
    }

    /// Writes `text` indented, newline-terminated, without a unit
    /// (`indented`).
    pub fn lines(&mut self, indent: usize, text: &str) {
        self.text.push_str(&indented(indent, text));
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

/// `text` with every line indented by `indent` spaces and newline-terminated;
/// blank lines are left empty. A line break inside a string literal is part
/// of its value: the line after it is kept as it is, and a carriage return
/// before it stays.
pub(crate) fn indented(indent: usize, text: &str) -> String {
    let pad = " ".repeat(indent);
    let mut out = String::new();
    let mut parts: Vec<&str> = text.split('\n').collect();
    if text.is_empty() || text.ends_with('\n') {
        parts.pop();
    }
    let quoted = quoted_breaks(text);
    for (i, part) in parts.iter().enumerate() {
        let starts_quoted = i > 0 && quoted[i - 1];
        let line = if quoted.get(i).copied().unwrap_or(false) {
            *part
        } else {
            part.strip_suffix('\r').unwrap_or(part)
        };
        if starts_quoted {
            out.push_str(line);
        } else if !line.trim().is_empty() {
            out.push_str(&pad);
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// For each line break of SQL text, in order: whether it falls inside a
/// string literal (`'…'`, `''` inside it) or a quoted name (`"…"`). Line
/// comments and nested block comments are skipped.
pub(crate) fn quoted_breaks(text: &str) -> Vec<bool> {
    #[derive(PartialEq)]
    enum In {
        Code,
        Str,
        Name,
        Line,
        Block(usize),
    }
    let mut at = In::Code;
    let mut out = Vec::new();
    let mut cs = text.chars().peekable();
    while let Some(c) = cs.next() {
        if c == '\n' {
            out.push(matches!(at, In::Str | In::Name));
            if at == In::Line {
                at = In::Code;
            }
            continue;
        }
        at = match (at, c) {
            (In::Code, '\'') => In::Str,
            (In::Code, '"') => In::Name,
            (In::Code, '-') if cs.peek() == Some(&'-') => In::Line,
            (In::Code, '/') if cs.peek() == Some(&'*') => {
                cs.next();
                In::Block(1)
            }
            (In::Str, '\'') if cs.peek() == Some(&'\'') => {
                cs.next();
                In::Str
            }
            (In::Str, '\'') => In::Code,
            (In::Name, '"') if cs.peek() == Some(&'"') => {
                cs.next();
                In::Name
            }
            (In::Name, '"') => In::Code,
            (In::Block(d), '/') if cs.peek() == Some(&'*') => {
                cs.next();
                In::Block(d + 1)
            }
            (In::Block(d), '*') if cs.peek() == Some(&'/') => {
                cs.next();
                if d == 1 {
                    In::Code
                } else {
                    In::Block(d - 1)
                }
            }
            (at, _) => at,
        };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::Out;

    #[test]
    fn a_line_break_inside_a_string_keeps_the_value() {
        let mut o = Out::default();
        o.lines(
            4,
            "v := '\r\n  a\r\n\r\n'; -- it's\r\nb := 'x''\ny';\r\n/* '\n*/ c;\n\"n\ne\";",
        );
        assert_eq!(
            o.text,
            "    v := '\r\n  a\r\n\r\n'; -- it's\n    b := 'x''\ny';\n    /* '\n    */ c;\n    \"n\ne\";\n"
        );
        let mut o = Out::default();
        o.lines(2, "a\r\n\n  \nb");
        assert_eq!(o.text, "  a\n\n\n  b\n");
    }
}
