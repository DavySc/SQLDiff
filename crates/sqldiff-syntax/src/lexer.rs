//! Lossless lexer. Every byte of the input belongs to exactly one token.

use crate::dialect::{Dialect, LexicalProfile};
use crate::kind::SyntaxKind::{self, *};
use crate::Diagnostic;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: SyntaxKind,
    /// Absolute byte offsets into the source.
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Default)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Lex a whole text.
pub fn tokenize(text: &str, dialect: Dialect) -> Lexed {
    tokenize_range(text, 0, text.len(), dialect)
}

/// Lex `src[start..end]`, producing absolute offsets. Used for nested languages.
pub fn tokenize_range(src: &str, start: usize, end: usize, dialect: Dialect) -> Lexed {
    let lexer = Lexer {
        src: &src[..end],
        range_start: start,
        pos: start,
        dialect,
        profile: dialect.lexical(),
        out: Lexed::default(),
    };
    lexer.run()
}

const PG_OP_CHARS: &str = "+-*/<>=~!@#%^&|`?";
const PG_OP_SPECIAL: &str = "~!@#%^&|`?";
const GENERIC_OP_CHARS: &str = "+-*/<>=~!@#%^&|`?¬";

struct Lexer<'a> {
    src: &'a str,
    range_start: usize,
    pos: usize,
    dialect: Dialect,
    profile: &'static LexicalProfile,
    out: Lexed,
}

impl<'a> Lexer<'a> {
    fn run(mut self) -> Lexed {
        while self.pos < self.src.len() {
            let start = self.pos;
            let kind = self.next_kind();
            debug_assert!(self.pos > start, "lexer made no progress");
            if self.pos == start {
                // Defensive: never loop forever.
                self.bump();
            }
            self.out.tokens.push(Token {
                kind,
                start,
                end: self.pos,
            });
        }
        self.out
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn peek_nth(&self, n: usize) -> Option<char> {
        self.rest().chars().nth(n)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn starts_with(&self, s: &str) -> bool {
        self.rest().starts_with(s)
    }

    fn diag(&mut self, offset: usize, message: impl Into<String>) {
        self.out.diagnostics.push(Diagnostic {
            offset,
            message: message.into(),
        });
    }

    fn is_ident_start(&self, c: char) -> bool {
        c.is_alphabetic() || c == '_' || self.profile.extra_ident_start.contains(&c)
    }

    fn is_ident_continue(&self, c: char) -> bool {
        c.is_alphanumeric() || c == '_' || self.profile.extra_ident_continue.contains(&c)
    }

    fn next_kind(&mut self) -> SyntaxKind {
        let start = self.pos;
        let c = self.peek().expect("not at end");

        if c.is_whitespace() {
            while matches!(self.peek(), Some(c) if c.is_whitespace()) {
                self.bump();
            }
            return WHITESPACE;
        }
        if self.starts_with("--") {
            while let Some(c) = self.peek() {
                if c == '\n' {
                    break;
                }
                self.bump();
            }
            return LINE_COMMENT;
        }
        if self.starts_with("/*") {
            self.block_comment(start);
            return BLOCK_COMMENT;
        }

        match c {
            '\'' => {
                self.quoted(start, '\'', false);
                STRING
            }
            '"' => {
                self.quoted(start, '"', false);
                QUOTED_IDENT
            }
            '[' if self.profile.bracket_identifiers => {
                self.bracket_ident(start);
                QUOTED_IDENT
            }
            '[' => {
                self.bump();
                L_BRACKET
            }
            ']' => {
                self.bump();
                R_BRACKET
            }
            '(' => {
                self.bump();
                L_PAREN
            }
            ')' => {
                self.bump();
                R_PAREN
            }
            '{' => {
                self.bump();
                L_BRACE
            }
            '}' => {
                self.bump();
                R_BRACE
            }
            ',' => {
                self.bump();
                COMMA
            }
            ';' => {
                self.bump();
                SEMICOLON
            }
            '.' if matches!(self.peek_nth(1), Some(d) if d.is_ascii_digit()) => {
                self.number();
                NUMBER
            }
            '.' => {
                self.bump();
                DOT
            }
            ':' => {
                if self.starts_with("::") && self.dialect != Dialect::Db2i {
                    self.pos += 2;
                    OP
                } else if self.starts_with(":=") && self.dialect == Dialect::Postgres {
                    self.pos += 2;
                    OP
                } else {
                    self.bump();
                    COLON
                }
            }
            '$' if self.profile.dollar_quoting => self.dollar(start),
            '@' if self.profile.at_variables
                && matches!(self.peek_nth(1), Some(n) if n == '@' || self.is_ident_continue(n)) =>
            {
                self.bump();
                while matches!(self.peek(), Some(n) if n == '@' || self.is_ident_continue(n)) {
                    self.bump();
                }
                VARIABLE
            }
            '?' if self.profile.question_param => {
                self.bump();
                PARAM
            }
            c if c.is_ascii_digit() => {
                self.number();
                NUMBER
            }
            c if self.is_ident_start(c) => self.word(start),
            c if GENERIC_OP_CHARS.contains(c) => {
                self.operator();
                OP
            }
            _ => {
                self.bump();
                self.diag(start, format!("unexpected character `{c}`"));
                ERROR_TOKEN
            }
        }
    }

    fn block_comment(&mut self, start: usize) {
        self.pos += 2;
        let mut depth = 1usize;
        loop {
            if self.profile.nested_block_comments && self.starts_with("/*") {
                self.pos += 2;
                depth += 1;
            } else if self.starts_with("*/") {
                self.pos += 2;
                depth -= 1;
                if depth == 0 {
                    break;
                }
            } else if self.bump().is_none() {
                self.diag(start, "unterminated block comment");
                break;
            }
        }
    }

    fn quoted(&mut self, start: usize, q: char, backslash_escapes: bool) {
        self.bump();
        loop {
            match self.bump() {
                None => {
                    self.diag(start, "unterminated quoted text");
                    break;
                }
                Some('\\') if backslash_escapes => {
                    self.bump();
                }
                Some(c) if c == q => {
                    if self.peek() == Some(q) {
                        self.bump();
                    } else {
                        break;
                    }
                }
                Some(_) => {}
            }
        }
    }

    fn bracket_ident(&mut self, start: usize) {
        self.bump();
        loop {
            match self.bump() {
                None => {
                    self.diag(start, "unterminated bracketed identifier");
                    break;
                }
                Some(']') => {
                    if self.peek() == Some(']') {
                        self.bump();
                    } else {
                        break;
                    }
                }
                Some(_) => {}
            }
        }
    }

    fn number(&mut self) {
        let hex_ok = matches!(self.dialect, Dialect::Tsql | Dialect::Postgres);
        if hex_ok && (self.starts_with("0x") || self.starts_with("0X")) {
            self.pos += 2;
            while matches!(self.peek(), Some(c) if c.is_ascii_hexdigit() || c == '_') {
                self.bump();
            }
            return;
        }
        let underscores = self.dialect == Dialect::Postgres;
        let digits = |l: &mut Self| {
            while matches!(l.peek(), Some(c) if c.is_ascii_digit() || (underscores && c == '_')) {
                l.bump();
            }
        };
        digits(self);
        if self.peek() == Some('.') && self.peek_nth(1) != Some('.') {
            self.bump();
            digits(self);
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let sign = matches!(self.peek_nth(1), Some('+' | '-'));
            let digit_at = if sign { 2 } else { 1 };
            if matches!(self.peek_nth(digit_at), Some(c) if c.is_ascii_digit()) {
                self.pos += digit_at;
                digits(self);
            }
        }
    }

    fn word(&mut self, start: usize) -> SyntaxKind {
        while matches!(self.peek(), Some(c) if self.is_ident_continue(c)) {
            self.bump();
        }
        let word = &self.src[start..self.pos];
        let upper = word.to_ascii_uppercase();

        // Prefixed strings: N'..', E'..', X'..', U&'..', U&"..".
        if self.peek() == Some('\'') && self.profile.string_prefixes.contains(&upper.as_str()) {
            let backslash = self.dialect == Dialect::Postgres && upper == "E";
            self.quoted(start, '\'', backslash);
            return STRING;
        }
        if self.dialect == Dialect::Postgres && upper == "U" && self.starts_with("&'") {
            self.bump();
            self.quoted(start, '\'', false);
            return STRING;
        }
        if self.dialect == Dialect::Postgres && upper == "U" && self.starts_with("&\"") {
            self.bump();
            self.quoted(start, '"', false);
            return QUOTED_IDENT;
        }

        if self.profile.go_batches && upper == "GO" && self.at_line_start(start) {
            if let Some(end) = self.go_line_end() {
                self.pos = end;
                return BATCH_SEP;
            }
        }
        IDENT
    }

    fn at_line_start(&self, start: usize) -> bool {
        let before = &self.src[self.range_start..start];
        let line = match before.rfind('\n') {
            Some(i) => &before[i + 1..],
            None => before,
        };
        line.chars().all(char::is_whitespace)
    }

    /// If the rest of the line after `GO` is `[count] [-- comment]`, return the
    /// end of `GO [count]`.
    fn go_line_end(&self) -> Option<usize> {
        let rest = self.rest();
        let bytes = rest.as_bytes();
        let mut i = 0;
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        let digits_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let end = if i > digits_start {
            self.pos + i
        } else {
            self.pos
        };
        while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }
        let tail = &rest[i..];
        if tail.is_empty()
            || tail.starts_with('\n')
            || tail.starts_with("\r\n")
            || tail.starts_with("--")
        {
            Some(end)
        } else {
            None
        }
    }

    fn dollar(&mut self, start: usize) -> SyntaxKind {
        if matches!(self.peek_nth(1), Some(c) if c.is_ascii_digit()) {
            self.bump();
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.bump();
            }
            return PARAM;
        }
        // $tag$ where tag is an identifier without `$`, or empty.
        let rest = &self.rest()[1..];
        let mut tag_len = 0;
        for (i, c) in rest.char_indices() {
            if c == '$' {
                tag_len = i;
                break;
            }
            let ok = if i == 0 {
                c.is_alphabetic() || c == '_'
            } else {
                c.is_alphanumeric() || c == '_'
            };
            if !ok {
                tag_len = usize::MAX;
                break;
            }
            tag_len = usize::MAX;
        }
        if tag_len == usize::MAX || !rest[tag_len..].starts_with('$') {
            self.bump();
            self.diag(start, "stray `$`");
            return ERROR_TOKEN;
        }
        let delim_len = tag_len + 2;
        let delim = &self.src[start..start + delim_len];
        let body_start = start + delim_len;
        match self.src[body_start..].find(delim) {
            Some(i) => self.pos = body_start + i + delim_len,
            None => {
                self.pos = self.src.len();
                self.diag(start, "unterminated dollar-quoted string");
            }
        }
        DOLLAR_STRING
    }

    fn operator(&mut self) {
        if self.profile.greedy_operators {
            let start = self.pos;
            while let Some(c) = self.peek() {
                if !PG_OP_CHARS.contains(c) {
                    break;
                }
                if self.pos > start && (self.starts_with("--") || self.starts_with("/*")) {
                    break;
                }
                self.bump();
            }
            // A multi-char operator cannot end in + or - unless it contains a special char.
            loop {
                let s = &self.src[start..self.pos];
                if s.len() > 1
                    && (s.ends_with('+') || s.ends_with('-'))
                    && !s.chars().any(|c| PG_OP_SPECIAL.contains(c))
                {
                    self.pos -= 1;
                } else {
                    break;
                }
            }
            if self.pos == start {
                self.bump();
            }
            return;
        }
        for op in self.profile.operators {
            if self.starts_with(op) {
                self.pos += op.len();
                return;
            }
        }
        self.bump();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str, d: Dialect) -> Vec<(SyntaxKind, &str)> {
        tokenize(text, d)
            .tokens
            .iter()
            .filter(|t| !t.kind.is_trivia())
            .map(|t| (t.kind, &text[t.start..t.end]))
            .collect()
    }

    #[test]
    fn tsql_tokens() {
        let k = kinds(
            "SELECT @x = [a]]b], N'it''s' FROM #t\nGO 2\n",
            Dialect::Tsql,
        );
        assert_eq!(
            k,
            vec![
                (IDENT, "SELECT"),
                (VARIABLE, "@x"),
                (OP, "="),
                (QUOTED_IDENT, "[a]]b]"),
                (COMMA, ","),
                (STRING, "N'it''s'"),
                (IDENT, "FROM"),
                (IDENT, "#t"),
                (BATCH_SEP, "GO 2"),
            ]
        );
    }

    #[test]
    fn go_only_alone_on_line() {
        let k = kinds("SELECT go FROM t", Dialect::Tsql);
        assert!(k.iter().all(|(k, _)| *k != BATCH_SEP));
    }

    #[test]
    fn pg_dollar_and_ops() {
        let text = "SELECT $1::int, E'a\\'b', $fn$ body; $fn$, a->>'k', x<<y";
        let k = kinds(text, Dialect::Postgres);
        assert_eq!(k[1], (PARAM, "$1"));
        assert_eq!(k[2], (OP, "::"));
        assert_eq!(k[5], (STRING, "E'a\\'b'"));
        assert_eq!(k[7], (DOLLAR_STRING, "$fn$ body; $fn$"));
        assert_eq!(k[10], (OP, "->>"));
        assert_eq!(k[14], (OP, "<<"));
    }

    #[test]
    fn pg_op_does_not_end_in_minus() {
        let k = kinds("a<-1", Dialect::Postgres);
        assert_eq!(k[1], (OP, "<"));
        assert_eq!(k[2], (OP, "-"));
    }

    #[test]
    fn db2_names() {
        let k = kinds(
            "SELECT * FROM MYLIB/MYFILE WHERE X'FF' = @A#",
            Dialect::Db2i,
        );
        assert!(k.contains(&(IDENT, "MYLIB")));
        assert!(k.contains(&(OP, "/")));
        assert!(k.contains(&(STRING, "X'FF'")));
        assert!(k.contains(&(IDENT, "@A#")));
    }

    #[test]
    fn unterminated_is_diagnosed_and_lossless() {
        for d in Dialect::ALL {
            let text = "SELECT 'abc /* x";
            let lexed = tokenize(text, d);
            assert!(!lexed.diagnostics.is_empty());
            let joined: String = lexed.tokens.iter().map(|t| &text[t.start..t.end]).collect();
            assert_eq!(joined, text);
        }
    }
}
