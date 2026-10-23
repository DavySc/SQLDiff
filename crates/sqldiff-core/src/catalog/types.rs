//! Data types as written in DDL and declarations.

use serde::{Deserialize, Serialize};

/// A data type: canonical base name plus its arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataType {
    /// Upper case, synonyms folded (`INT` → `INTEGER`, `CHARACTER VARYING` → `VARCHAR`).
    /// Empty when the type is not known.
    pub base: String,
    /// Length, or precision and scale.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<u32>,
    /// `varchar(max)` and friends.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub max: bool,
    /// The type as written, whitespace collapsed.
    pub text: String,
}

impl DataType {
    pub fn unknown() -> Self {
        DataType {
            base: String::new(),
            args: Vec::new(),
            max: false,
            text: String::new(),
        }
    }

    pub fn is_known(&self) -> bool {
        !self.base.is_empty()
    }

    /// Parses a type from its words, e.g. `["character", "varying", "(", "50", ")"]`.
    pub fn parse(words: &[&str]) -> Self {
        let mut names = Vec::new();
        let mut args = Vec::new();
        let mut max = false;
        let mut in_parens = false;
        let mut array = false;
        for w in words {
            match *w {
                "(" => in_parens = true,
                ")" => in_parens = false,
                "," => {}
                "[" | "]" | "[]" => array = true,
                _ if in_parens => {
                    if w.eq_ignore_ascii_case("max") {
                        max = true;
                    } else if let Ok(n) = w.parse::<u32>() {
                        args.push(n);
                    }
                }
                _ => names.push(w.to_ascii_uppercase()),
            }
        }
        let text = render(words);
        if names.is_empty() {
            return DataType {
                text,
                ..DataType::unknown()
            };
        }
        let joined = names.join(" ");
        let mut base = canonical(&joined).to_string();
        if array {
            base.push_str("[]");
        }
        DataType {
            base,
            args,
            max,
            text,
        }
    }

    /// Parses a type written as one string, e.g. `varchar(40)`.
    pub fn parse_str(text: &str) -> Self {
        let mut words: Vec<String> = Vec::new();
        let mut cur = String::new();
        for c in text.chars() {
            if c.is_alphanumeric() || c == '_' {
                cur.push(c);
            } else {
                if !cur.is_empty() {
                    words.push(std::mem::take(&mut cur));
                }
                if matches!(c, '(' | ')' | ',' | '[' | ']') {
                    words.push(c.to_string());
                }
            }
        }
        if !cur.is_empty() {
            words.push(cur);
        }
        let refs: Vec<&str> = words.iter().map(String::as_str).collect();
        DataType::parse(&refs)
    }

    pub fn is_string(&self) -> bool {
        matches!(
            self.base.as_str(),
            "CHAR"
                | "VARCHAR"
                | "NCHAR"
                | "NVARCHAR"
                | "TEXT"
                | "NTEXT"
                | "CLOB"
                | "GRAPHIC"
                | "VARGRAPHIC"
                | "DBCLOB"
        )
    }

    /// Maximum length in characters for a string type with a declared length.
    pub fn char_length(&self) -> Option<u32> {
        if !self.is_string() || self.max {
            return None;
        }
        match self.base.as_str() {
            // `char` without a length is char(1); `varchar` without one is unbounded in
            // PostgreSQL and varchar(1) in a T-SQL declaration (30 in CAST).
            "CHAR" | "NCHAR" => Some(self.args.first().copied().unwrap_or(1)),
            "TEXT" | "NTEXT" | "CLOB" | "DBCLOB" => None,
            _ => self.args.first().copied(),
        }
    }
}

fn render(words: &[&str]) -> String {
    let mut out = String::new();
    for w in words {
        let glue = !out.is_empty()
            && !matches!(*w, "(" | ")" | "," | "[" | "]" | "[]")
            && !out.ends_with('(');
        if glue {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

/// Folds type synonyms to one spelling.
pub fn canonical(upper: &str) -> &str {
    match upper {
        "INT" | "INT4" => "INTEGER",
        "INT8" => "BIGINT",
        "INT2" => "SMALLINT",
        "CHARACTER VARYING" | "CHAR VARYING" => "VARCHAR",
        "CHARACTER" => "CHAR",
        "NATIONAL CHARACTER VARYING" | "NATIONAL CHAR VARYING" => "NVARCHAR",
        "NATIONAL CHARACTER" | "NATIONAL CHAR" => "NCHAR",
        "DEC" => "DECIMAL",
        "FLOAT8" | "DOUBLE" => "DOUBLE PRECISION",
        "FLOAT4" => "REAL",
        "BOOL" => "BOOLEAN",
        "TIMESTAMP WITHOUT TIME ZONE" => "TIMESTAMP",
        "TIMESTAMPTZ" => "TIMESTAMP WITH TIME ZONE",
        "CHARACTER LARGE OBJECT" | "CHAR LARGE OBJECT" => "CLOB",
        "BINARY LARGE OBJECT" => "BLOB",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_spellings() {
        let t = DataType::parse_str("character varying(50)");
        assert_eq!(t.base, "VARCHAR");
        assert_eq!(t.char_length(), Some(50));
        let t = DataType::parse_str("DECIMAL(12,2)");
        assert_eq!(
            (t.base.as_str(), t.args.as_slice()),
            ("DECIMAL", &[12, 2][..])
        );
        let t = DataType::parse_str("nvarchar(max)");
        assert!(t.max && t.char_length().is_none());
        assert_eq!(DataType::parse_str("int").base, "INTEGER");
        assert_eq!(DataType::parse_str("char").char_length(), Some(1));
    }
}
