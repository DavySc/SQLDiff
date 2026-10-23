//! Path globs used by packs: `*` (within a segment), `**` (any depth), `?`.
//! Paths always use `/`. Matching is case-sensitive unless the glob starts
//! with `(?i)`.

use regex::Regex;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Glob {
    pub pattern: String,
    re: Regex,
}

impl Glob {
    pub fn new(pattern: &str) -> Result<Self> {
        let (ci, body) = match pattern.strip_prefix("(?i)") {
            Some(rest) => (true, rest),
            None => (false, pattern),
        };
        let mut re = String::from(if ci { "(?i)^" } else { "^" });
        let chars: Vec<char> = body.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '*' if chars.get(i + 1) == Some(&'*') => {
                    if chars.get(i + 2) == Some(&'/') {
                        re.push_str("(?:.*/)?");
                        i += 3;
                    } else {
                        re.push_str(".*");
                        i += 2;
                    }
                    continue;
                }
                '*' => re.push_str("[^/]*"),
                '?' => re.push_str("[^/]"),
                c => re.push_str(&regex::escape(&c.to_string())),
            }
            i += 1;
        }
        re.push('$');
        Ok(Glob {
            pattern: pattern.to_string(),
            re: Regex::new(&re)?,
        })
    }

    pub fn is_match(&self, path: &str) -> bool {
        self.re.is_match(path)
    }
}

/// True if any glob matches.
pub fn any(globs: &[Glob], path: &str) -> bool {
    globs.iter().any(|g| g.is_match(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        let g = Glob::new("mssql/**/*.sql").unwrap();
        assert!(g.is_match("mssql/a.sql"));
        assert!(g.is_match("mssql/x/y/a.sql"));
        assert!(!g.is_match("pg/a.sql"));
        assert!(Glob::new("**/*.sql").unwrap().is_match("a.sql"));
        assert!(!Glob::new("*.sql").unwrap().is_match("x/a.sql"));
        assert!(Glob::new("(?i)*.SQL").unwrap().is_match("a.sql"));
        assert!(Glob::new("a?.sql").unwrap().is_match("ab.sql"));
        assert!(Glob::new("common/**").unwrap().is_match("common/x/y.sql"));
    }
}
