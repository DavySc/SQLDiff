//! Optional template layer (design section 3, layer 1, and section 5.3).
//!
//! Some artifacts are templates: SQL interleaved with placeholders and
//! directives of a code generator. The syntax is configuration
//! ([`TemplateSyntax`], normally from a pack); this module knows no
//! generator. A template is split losslessly into segments: SQL spans,
//! placeholders, special forms and unresolved placeholder candidates.
//!
//! - [`Template::fragment`] replaces placeholders with neutral stand-ins so
//!   the SQL parser can read the template in a fragment-tolerant way;
//!   offsets map back to the template.
//! - [`Template::substitute`] fills in parameter values, giving the text a
//!   generator would emit, with an offset map template ↔ output.
//! - A placeholder that collides with SQL syntax (`[name]` in T-SQL) is
//!   resolved against the known parameters. A candidate that is not a known
//!   parameter is kept as SQL and listed as unresolved, never guessed.

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;

use crate::{parse, Diagnostic, Dialect, Parse};

/// A template syntax definition, as a pack declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateSyntax {
    pub open: String,
    pub close: String,
    /// Regex for placeholder names.
    pub name_pattern: String,
    /// Special forms (includes, conditionals, ...) by name and regex. The
    /// regex matches the whole form, delimiters included; capture groups
    /// are its arguments.
    pub special_forms: Vec<(String, String)>,
    /// Placeholder names match parameter names in any letter case, as the
    /// generator resolves them. Names are then kept in upper case.
    pub ignore_case: bool,
}

/// A compiled [`TemplateSyntax`].
#[derive(Debug, Clone)]
pub struct Compiled {
    placeholder: Regex,
    specials: Vec<(String, Regex)>,
    ignore_case: bool,
}

impl Compiled {
    pub fn new(s: &TemplateSyntax) -> Result<Compiled, String> {
        if s.open.is_empty() || s.close.is_empty() {
            return Err("template: open and close delimiters must be non-empty".into());
        }
        // With ignore_case the generator resolves names in any case, so the
        // name pattern matches in any case too (`[schema]` for SCHEMA).
        let placeholder = Regex::new(&format!(
            "{}\\s*(?P<tplname>{}{}){}\\s*{}",
            regex::escape(&s.open),
            if s.ignore_case { "(?i:" } else { "(?:" },
            s.name_pattern,
            ")",
            regex::escape(&s.close)
        ))
        .map_err(|e| format!("template.name_pattern: {e}"))?;
        let specials = s
            .special_forms
            .iter()
            .map(|(n, p)| {
                Regex::new(p)
                    .map(|r| (n.clone(), r))
                    .map_err(|e| format!("template special form `{n}`: {e}"))
            })
            .collect::<Result<_, _>>()?;
        Ok(Compiled {
            placeholder,
            specials,
            ignore_case: s.ignore_case,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentKind {
    Sql,
    Placeholder {
        name: String,
    },
    /// A special form; `args` are its capture groups.
    Special {
        form: String,
        args: Vec<String>,
    },
    /// Looks like a placeholder but names no known parameter; kept as SQL.
    Unresolved {
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub kind: SegmentKind,
    pub start: usize,
    pub end: usize,
}

/// What a match of the name pattern is, as [`Template::parse_with`] decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// A placeholder.
    Placeholder,
    /// Looks like a placeholder but is not one: kept as SQL and reported.
    Unresolved,
    /// Plain SQL (for example a quoted name), not reported.
    Sql,
}

/// Whether `name` is one of the parameter names in `list`, in any letter case
/// when `ignore_case` is set.
pub fn listed(list: &BTreeSet<String>, name: &str, ignore_case: bool) -> bool {
    list.contains(name) || (ignore_case && list.iter().any(|x| x.eq_ignore_ascii_case(name)))
}

/// One match of the name pattern, as [`Template::parse_with`] offers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate<'a> {
    /// The name as written between the delimiters.
    pub name: &'a str,
    /// The character just before the opening delimiter.
    pub prev: Option<char>,
    /// The character just after the closing delimiter.
    pub next: Option<char>,
}

impl Candidate<'_> {
    /// Directly attached to a variable sigil, to identifier characters or to
    /// another delimited name (`@[x]`, `a_[x]_b`, `[x][y]`). A quoted
    /// identifier always stands apart (separated by space, punctuation or a
    /// `.`), so a glued match can only be a placeholder.
    pub fn glued(&self, open: &str, close: &str) -> bool {
        let word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '@' | '#' | '$');
        self.prev.is_some_and(|c| word(c) || close.ends_with(c))
            || self
                .next
                .is_some_and(|c| (word(c) && c != '@') || open.starts_with(c))
    }
}

/// A template split into segments. Segments are contiguous and cover every
/// byte, so [`Template::print`] returns the input.
#[derive(Debug, Clone)]
pub struct Template {
    pub text: String,
    pub segments: Vec<Segment>,
    /// Parameter values are looked up in any letter case.
    pub ignore_case: bool,
}

/// One span of an [`OffsetMap`]: `src` in the template, `dst` in the
/// derived text. `exact` spans are copied byte for byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapSpan {
    pub src: (usize, usize),
    pub dst: (usize, usize),
    pub exact: bool,
}

/// Offsets between a template and a text derived from it (fragment or
/// output). Inside an exact span offsets map one to one; inside a
/// substituted span they map to the span's start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OffsetMap {
    pub spans: Vec<MapSpan>,
}

impl OffsetMap {
    fn map(&self, off: usize, forward: bool) -> usize {
        let pick = |s: &MapSpan| {
            if forward {
                (s.src, s.dst)
            } else {
                (s.dst, s.src)
            }
        };
        for s in &self.spans {
            let ((fs, fe), (ts, _)) = pick(s);
            if off >= fs && off < fe {
                return if s.exact { ts + (off - fs) } else { ts };
            }
            if off == fe && fs == fe {
                return ts;
            }
        }
        // At or past the end.
        self.spans.last().map_or(off, |s| {
            let ((_, fe), (_, te)) = pick(s);
            te + off.saturating_sub(fe)
        })
    }

    /// Template offset to derived-text offset.
    pub fn to_dst(&self, src: usize) -> usize {
        self.map(src, true)
    }

    /// Derived-text offset to template offset.
    pub fn to_src(&self, dst: usize) -> usize {
        self.map(dst, false)
    }

    /// The template span a derived-text span came from.
    pub fn src_range(&self, dst_start: usize, dst_end: usize) -> (usize, usize) {
        let s = self.to_src(dst_start);
        let e = if dst_end > dst_start {
            // The end is exclusive: map the last byte, then step past its segment.
            let last = dst_end - 1;
            let span = self
                .spans
                .iter()
                .find(|x| last >= x.dst.0 && last < x.dst.1);
            match span {
                Some(x) if x.exact => x.src.0 + (last - x.dst.0) + 1,
                Some(x) => x.src.1,
                None => self.to_src(dst_end),
            }
        } else {
            s
        };
        (s, e.max(s))
    }
}

/// A text derived from a template, with its offset map.
#[derive(Debug, Clone)]
pub struct Derived {
    pub text: String,
    pub map: OffsetMap,
}

impl Derived {
    /// Parse the derived text and report diagnostics at template offsets.
    pub fn parse(&self, dialect: Dialect) -> (Parse, Vec<Diagnostic>) {
        let p = parse(&self.text, dialect);
        let diags = p
            .diagnostics
            .iter()
            .map(|d| Diagnostic {
                offset: self.map.to_src(d.offset),
                message: d.message.clone(),
            })
            .collect();
        (p, diags)
    }
}

/// Why a template could not be expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpandProblem {
    MissingParameter {
        name: String,
        offset: usize,
    },
    /// Special forms are the generator's logic; the core does not run them.
    SpecialForm {
        form: String,
        offset: usize,
    },
}

impl std::fmt::Display for ExpandProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExpandProblem::MissingParameter { name, .. } => {
                write!(f, "no value for parameter `{name}`")
            }
            ExpandProblem::SpecialForm { form, .. } => {
                write!(
                    f,
                    "special form `{form}` is generator logic and is not expanded"
                )
            }
        }
    }
}

/// Stand-in identifier for a placeholder in a fragment. Starts with a
/// letter so it is an ordinary identifier in all three dialects.
pub fn stand_in(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("TPL_{clean}")
}

impl Template {
    /// Splits `text`. With `known`, a placeholder whose name is not a known
    /// parameter becomes [`SegmentKind::Unresolved`]; without, every match is
    /// a placeholder.
    pub fn parse(text: &str, syntax: &Compiled, known: Option<&BTreeSet<String>>) -> Template {
        let ignore_case = syntax.ignore_case;
        Template::parse_with(text, syntax, &|c: &Candidate| match known {
            Some(k) if !listed(k, c.name, ignore_case) => Decision::Unresolved,
            _ => Decision::Placeholder,
        })
    }

    /// Splits `text`, deciding for each match of the name pattern (by its
    /// name as written and the characters around it) whether it is a
    /// placeholder, an unresolved candidate, or plain SQL.
    pub fn parse_with(
        text: &str,
        syntax: &Compiled,
        decide: &dyn Fn(&Candidate) -> Decision,
    ) -> Template {
        let mut marks: Vec<Segment> = Vec::new();
        for (form, re) in &syntax.specials {
            for c in re.captures_iter(text) {
                let all = c.get(0).expect("group 0");
                if all.is_empty() || overlaps(&marks, all.start(), all.end()) {
                    continue;
                }
                let args = c
                    .iter()
                    .skip(1)
                    .flatten()
                    .map(|m| m.as_str().to_string())
                    .collect();
                marks.push(Segment {
                    kind: SegmentKind::Special {
                        form: form.clone(),
                        args,
                    },
                    start: all.start(),
                    end: all.end(),
                });
            }
        }
        for c in syntax.placeholder.captures_iter(text) {
            let all = c.get(0).expect("group 0");
            if overlaps(&marks, all.start(), all.end()) {
                continue;
            }
            let raw = &c["tplname"];
            let name = if syntax.ignore_case {
                raw.to_ascii_uppercase()
            } else {
                raw.to_string()
            };
            let candidate = Candidate {
                name: raw,
                prev: text[..all.start()].chars().next_back(),
                next: text[all.end()..].chars().next(),
            };
            let kind = match decide(&candidate) {
                Decision::Sql => continue,
                Decision::Unresolved => SegmentKind::Unresolved { name },
                Decision::Placeholder => SegmentKind::Placeholder { name },
            };
            marks.push(Segment {
                kind,
                start: all.start(),
                end: all.end(),
            });
        }
        marks.sort_by_key(|s| s.start);
        let mut segments = Vec::new();
        let mut at = 0;
        for m in marks {
            if m.start > at {
                segments.push(Segment {
                    kind: SegmentKind::Sql,
                    start: at,
                    end: m.start,
                });
            }
            at = m.end;
            segments.push(m);
        }
        if at < text.len() {
            segments.push(Segment {
                kind: SegmentKind::Sql,
                start: at,
                end: text.len(),
            });
        }
        Template {
            text: text.to_string(),
            segments,
            ignore_case: syntax.ignore_case,
        }
    }

    pub fn print(&self) -> String {
        self.segments
            .iter()
            .map(|s| &self.text[s.start..s.end])
            .collect()
    }

    pub fn slice(&self, s: &Segment) -> &str {
        &self.text[s.start..s.end]
    }

    /// Names of the placeholders, in order of first use.
    pub fn parameters(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        self.segments
            .iter()
            .filter_map(|s| match &s.kind {
                SegmentKind::Placeholder { name } if seen.insert(name.clone()) => {
                    Some(name.clone())
                }
                _ => None,
            })
            .collect()
    }

    pub fn unresolved(&self) -> impl Iterator<Item = &Segment> {
        self.segments
            .iter()
            .filter(|s| matches!(s.kind, SegmentKind::Unresolved { .. }))
    }

    /// The SQL fragment: placeholders become [`stand_in`] identifiers and
    /// special forms become block comments (trivia), so the SQL parser sees
    /// well-formed SQL wherever the template is.
    pub fn fragment(&self) -> Derived {
        self.derive(|s| match &s.kind {
            SegmentKind::Placeholder { name } => Some(stand_in(name)),
            SegmentKind::Special { form, .. } => Some(format!("/*{}*/", form.replace("*/", ""))),
            _ => None,
        })
    }

    /// The output for these parameter values.
    pub fn substitute(
        &self,
        params: &BTreeMap<String, String>,
    ) -> Result<Derived, Vec<ExpandProblem>> {
        let mut problems = Vec::new();
        let out = self.derive(|s| match &s.kind {
            SegmentKind::Placeholder { name } => match params.get(name).or_else(|| {
                self.ignore_case
                    .then(|| params.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)))
                    .flatten()
                    .map(|(_, v)| v)
            }) {
                Some(v) => Some(v.clone()),
                None => {
                    problems.push(ExpandProblem::MissingParameter {
                        name: name.clone(),
                        offset: s.start,
                    });
                    Some(String::new())
                }
            },
            SegmentKind::Special { form, .. } => {
                problems.push(ExpandProblem::SpecialForm {
                    form: form.clone(),
                    offset: s.start,
                });
                Some(String::new())
            }
            _ => None,
        });
        if problems.is_empty() {
            Ok(out)
        } else {
            Err(problems)
        }
    }

    /// Copies SQL and unresolved segments; `replace` gives the text for the others.
    fn derive(&self, mut replace: impl FnMut(&Segment) -> Option<String>) -> Derived {
        let mut text = String::with_capacity(self.text.len());
        let mut map = OffsetMap::default();
        for s in &self.segments {
            let start = text.len();
            let (piece, exact) = match replace(s) {
                Some(r) => (r, false),
                None => (self.slice(s).to_string(), true),
            };
            text.push_str(&piece);
            map.spans.push(MapSpan {
                src: (s.start, s.end),
                dst: (start, text.len()),
                exact,
            });
        }
        Derived { text, map }
    }
}

fn overlaps(marks: &[Segment], start: usize, end: usize) -> bool {
    marks.iter().any(|m| start < m.end && m.start < end)
}

/// How parameter sources that define the same name are reconciled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Collision {
    /// The first source in lookup order wins.
    #[default]
    First,
    /// The last source wins.
    Last,
    /// Two different values are a conflict.
    Error,
}

impl std::str::FromStr for Collision {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "first" => Ok(Collision::First),
            "last" => Ok(Collision::Last),
            "error" => Ok(Collision::Error),
            _ => Err(format!("collision `{s}`: expected first, last or error")),
        }
    }
}

/// Two sources disagree on a parameter under [`Collision::Error`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub name: String,
    pub values: Vec<(String, String)>,
}

/// Merges named parameter sources in lookup order (`order`, then any
/// sources it does not name, in the order given).
pub fn merge_parameters(
    sources: &[(&str, &BTreeMap<String, String>)],
    order: &[String],
    rule: Collision,
) -> (BTreeMap<String, String>, Vec<Conflict>) {
    let mut ordered: Vec<&(&str, &BTreeMap<String, String>)> = Vec::new();
    for o in order {
        ordered.extend(sources.iter().filter(|(n, _)| n == o));
    }
    ordered.extend(
        sources
            .iter()
            .filter(|(n, _)| !order.iter().any(|o| o == n)),
    );
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut origin: BTreeMap<String, &str> = BTreeMap::new();
    let mut conflicts: BTreeMap<String, Conflict> = BTreeMap::new();
    for (src, params) in ordered {
        for (k, v) in params.iter() {
            match out.get(k) {
                None => {
                    out.insert(k.clone(), v.clone());
                    origin.insert(k.clone(), src);
                }
                Some(old) => match rule {
                    Collision::First => {}
                    Collision::Last => {
                        out.insert(k.clone(), v.clone());
                    }
                    Collision::Error if old != v => {
                        let c = conflicts.entry(k.clone()).or_insert_with(|| Conflict {
                            name: k.clone(),
                            values: vec![(origin[k].to_string(), old.clone())],
                        });
                        c.values.push((src.to_string(), v.clone()));
                    }
                    Collision::Error => {}
                },
            }
        }
    }
    (out, conflicts.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn syntax(open: &str, close: &str) -> Compiled {
        Compiled::new(&TemplateSyntax {
            open: open.into(),
            close: close.into(),
            name_pattern: "[A-Za-z_][A-Za-z0-9_]*".into(),
            special_forms: vec![("include".into(), r"<%@include\s+(\S+)\s*%>".into())],
            ignore_case: false,
        })
        .unwrap()
    }

    #[test]
    fn round_trip_and_segments() {
        let s = syntax("<%", "%>");
        let text = "SELECT <% COL %> FROM <%TABLE%>;\n<%@include footer %>\n";
        let t = Template::parse(text, &s, None);
        assert_eq!(t.print(), text);
        assert_eq!(t.parameters(), vec!["COL", "TABLE"]);
        assert!(t
            .segments
            .iter()
            .any(|s| matches!(&s.kind, SegmentKind::Special { form, args } if form == "include" && args == &vec!["footer".to_string()])));
        for w in t.segments.windows(2) {
            assert_eq!(w[0].end, w[1].start);
        }
    }

    #[test]
    fn fragment_parses_and_maps_back() {
        let s = syntax("<%", "%>");
        let text = "UPDATE <%T%> SET qty = 1 WHERE id = <%ID%>;";
        let t = Template::parse(text, &s, None);
        let f = t.fragment();
        assert_eq!(f.text, "UPDATE TPL_T SET qty = 1 WHERE id = TPL_ID;");
        let (p, diags) = f.parse(Dialect::Postgres);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(p.syntax().text().to_string(), f.text);
        // `SET` maps to the same byte in the template; the stand-in maps to its placeholder.
        let set = f.text.find("SET").unwrap();
        assert_eq!(f.map.to_src(set), text.find("SET").unwrap());
        assert_eq!(
            f.map.to_src(f.text.find("TPL_ID").unwrap() + 2),
            text.find("<%ID").unwrap()
        );
        let id = f.text.find("TPL_ID").unwrap();
        assert_eq!(
            f.map.src_range(id, id + 6),
            (text.find("<%ID").unwrap(), text.len() - 1)
        );
    }

    #[test]
    fn substitute_with_offsets() {
        let s = syntax("{{", "}}");
        let text = "DELETE FROM {{T}} WHERE k = {{K}};";
        let t = Template::parse(text, &s, None);
        let mut p = BTreeMap::new();
        p.insert("T".to_string(), "dbo.orders".to_string());
        let e = t.substitute(&p).unwrap_err();
        assert_eq!(
            e,
            vec![ExpandProblem::MissingParameter {
                name: "K".into(),
                offset: text.find("{{K").unwrap()
            }]
        );
        p.insert("K".to_string(), "@Key".to_string());
        let out = t.substitute(&p).unwrap();
        assert_eq!(out.text, "DELETE FROM dbo.orders WHERE k = @Key;");
        let w = out.text.find("WHERE").unwrap();
        assert_eq!(out.map.to_src(w), text.find("WHERE").unwrap());
        assert_eq!(out.map.to_dst(text.find("WHERE").unwrap()), w);
    }

    #[test]
    fn collision_with_sql_syntax_resolves_against_known_parameters() {
        // Brackets are both the placeholder syntax and T-SQL quoted identifiers.
        let s = syntax("[", "]");
        let text = "SELECT [qty] FROM [TABLE];";
        let known: BTreeSet<String> = ["TABLE".to_string()].into();
        let t = Template::parse(text, &s, Some(&known));
        assert_eq!(t.parameters(), vec!["TABLE"]);
        let un: Vec<_> = t.unresolved().map(|s| t.slice(s)).collect();
        assert_eq!(un, vec!["[qty]"]);
        let f = t.fragment();
        assert_eq!(f.text, "SELECT [qty] FROM TPL_TABLE;");
        assert_eq!(t.print(), text);
    }

    #[test]
    fn glued_names_cannot_be_quoted_identifiers() {
        let at = |text: &str, name: &str| {
            let s = text.find(&format!("[{name}]")).unwrap();
            Candidate {
                name,
                prev: text[..s].chars().next_back(),
                next: text[s + name.len() + 2..].chars().next(),
            }
            .glued("[", "]")
        };
        let text = "set @[p][comma] = x_[k]_y; select [dbo].[t], ([c]) from [s] where a=[v];";
        for glued in ["p", "comma", "k"] {
            assert!(at(text, glued), "{glued}");
        }
        for apart in ["dbo", "t", "c", "s", "v"] {
            assert!(!at(text, apart), "{apart}");
        }
    }

    #[test]
    fn ignore_case_matches_parameters_in_any_case() {
        let def = |ignore_case| TemplateSyntax {
            open: "[".into(),
            close: "]".into(),
            name_pattern: "[A-Za-z_][A-Za-z0-9_]*".into(),
            special_forms: vec![],
            ignore_case,
        };
        let text = "SELECT a[comma] b FROM [Tab];";
        let known: BTreeSet<String> = ["COMMA".to_string(), "TAB".to_string()].into();
        let t = Template::parse(text, &Compiled::new(&def(true)).unwrap(), Some(&known));
        // One spelling per name, so `[comma]` and `[COMMA]` are the same stand-in.
        assert_eq!(t.parameters(), vec!["COMMA", "TAB"]);
        assert_eq!(t.unresolved().count(), 0);
        assert_eq!(t.fragment().text, "SELECT aTPL_COMMA b FROM TPL_TAB;");
        let values: BTreeMap<String, String> =
            [("comma".into(), ",".into()), ("tab".into(), "t".into())].into();
        assert_eq!(t.substitute(&values).unwrap().text, "SELECT a, b FROM t;");
        // Case-sensitive by default: neither name is known.
        let t = Template::parse(text, &Compiled::new(&def(false)).unwrap(), Some(&known));
        assert_eq!(t.unresolved().count(), 2);
    }

    #[test]
    fn special_forms_are_not_expanded() {
        let s = syntax("<%", "%>");
        let t = Template::parse("<%@include x %>SELECT 1;", &s, None);
        let e = t.substitute(&BTreeMap::new()).unwrap_err();
        assert!(matches!(&e[0], ExpandProblem::SpecialForm { form, .. } if form == "include"));
        assert_eq!(t.fragment().text, "/*include*/SELECT 1;");
    }

    #[test]
    fn parameter_collisions() {
        let a: BTreeMap<String, String> = [("X".to_string(), "1".to_string())].into();
        let b: BTreeMap<String, String> = [
            ("X".to_string(), "2".to_string()),
            ("Y".to_string(), "3".to_string()),
        ]
        .into();
        let order = vec!["b".to_string()];
        let (m, c) = merge_parameters(&[("a", &a), ("b", &b)], &order, Collision::First);
        assert_eq!(m["X"], "2");
        assert!(c.is_empty());
        let (m, _) = merge_parameters(&[("a", &a), ("b", &b)], &order, Collision::Last);
        assert_eq!(m["X"], "1");
        let (_, c) = merge_parameters(&[("a", &a), ("b", &b)], &[], Collision::Error);
        assert_eq!(c.len(), 1);
        assert_eq!(
            c[0].values,
            vec![("a".into(), "1".into()), ("b".into(), "2".into())]
        );
    }
}
