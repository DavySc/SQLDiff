//! S-expression reader and writer for rule files.
//!
//! Atoms are runs of characters other than whitespace, parentheses, `"` and
//! `;`. A double-quoted string is one atom (`\"` and `\\` escape). `;` starts
//! a comment that runs to the end of the line.

use crate::equiv::term::Term;

/// Reads every top-level form of `src`.
pub fn read_all(src: &str) -> Result<Vec<Term>, String> {
    let mut r = Reader {
        s: src.as_bytes(),
        i: 0,
        src,
    };
    let mut out = Vec::new();
    loop {
        r.skip();
        if r.i >= r.s.len() {
            return Ok(out);
        }
        out.push(r.form()?);
    }
}

/// Reads exactly one form.
pub fn read(src: &str) -> Result<Term, String> {
    let mut v = read_all(src)?;
    match v.len() {
        1 => Ok(v.remove(0)),
        n => Err(format!("expected one form, found {n}")),
    }
}

struct Reader<'a> {
    s: &'a [u8],
    i: usize,
    src: &'a str,
}

impl Reader<'_> {
    fn skip(&mut self) {
        while self.i < self.s.len() {
            match self.s[self.i] {
                b';' => {
                    while self.i < self.s.len() && self.s[self.i] != b'\n' {
                        self.i += 1;
                    }
                }
                c if c.is_ascii_whitespace() => self.i += 1,
                _ => break,
            }
        }
    }

    fn line(&self) -> usize {
        self.src[..self.i.min(self.src.len())].matches('\n').count() + 1
    }

    fn form(&mut self) -> Result<Term, String> {
        self.skip();
        match self.s.get(self.i) {
            None => Err("unexpected end of input".into()),
            Some(b'(') => {
                let start = self.line();
                self.i += 1;
                let mut kids = Vec::new();
                loop {
                    self.skip();
                    match self.s.get(self.i) {
                        None => return Err(format!("unclosed `(` from line {start}")),
                        Some(b')') => {
                            self.i += 1;
                            break;
                        }
                        Some(_) => kids.push(self.form()?),
                    }
                }
                let mut it = kids.into_iter();
                match it.next() {
                    Some(Term::A(h)) => Ok(Term::N(h, it.collect())),
                    Some(Term::N(..)) => {
                        Err(format!("line {start}: a list must start with an atom"))
                    }
                    None => Err(format!("line {start}: empty list")),
                }
            }
            Some(b')') => Err(format!("line {}: unexpected `)`", self.line())),
            Some(b'"') => {
                self.i += 1;
                let mut out = Vec::new();
                loop {
                    match self.s.get(self.i) {
                        None => return Err("unclosed string".into()),
                        Some(b'"') => {
                            self.i += 1;
                            break;
                        }
                        Some(b'\\') if self.i + 1 < self.s.len() => {
                            out.push(self.s[self.i + 1]);
                            self.i += 2;
                        }
                        Some(&c) => {
                            out.push(c);
                            self.i += 1;
                        }
                    }
                }
                Ok(Term::A(String::from_utf8(out).map_err(|e| e.to_string())?))
            }
            Some(_) => {
                let start = self.i;
                while self.i < self.s.len()
                    && !matches!(self.s[self.i], b'(' | b')' | b'"' | b';')
                    && !self.s[self.i].is_ascii_whitespace()
                {
                    self.i += 1;
                }
                Ok(Term::A(self.src[start..self.i].to_string()))
            }
        }
    }
}

/// Writes a term so that `read` gives it back.
pub fn write(t: &Term) -> String {
    let mut s = String::new();
    write_into(t, &mut s);
    s
}

fn write_into(t: &Term, out: &mut String) {
    match t {
        Term::A(a) => {
            let plain = !a.is_empty()
                && !a.bytes().any(|c| {
                    matches!(c, b'(' | b')' | b'"' | b';' | b'\\') || c.is_ascii_whitespace()
                });
            if plain {
                out.push_str(a);
            } else {
                out.push('"');
                for c in a.chars() {
                    if c == '"' || c == '\\' {
                        out.push('\\');
                    }
                    out.push(c);
                }
                out.push('"');
            }
        }
        Term::N(h, kids) => {
            out.push('(');
            write_into(&Term::A(h.clone()), out);
            for k in kids {
                out.push(' ');
                write_into(k, out);
            }
            out.push(')');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let src = r#"(lift x ; comment
            (tsql (call isnull ?a "it's \"q\"")) (str ""))"#;
        let t = read(src).unwrap();
        assert_eq!(read(&write(&t)).unwrap(), t);
        assert_eq!(
            t.kids()[1].kids()[0].kids()[2],
            Term::A("it's \"q\"".into())
        );
    }

    #[test]
    fn errors_have_lines() {
        assert!(read("(a\n(b)").unwrap_err().contains("line 1"));
        assert!(read("((a))").is_err());
    }
}
