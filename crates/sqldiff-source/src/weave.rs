//! Weaving: filling templates with an instance's parameter values, as the
//! generator does, so `sqldiff instances` can check every result.
//!
//! An instance manifest lists what the generator produces: each instance's
//! templates (one, or several parts in order) and parameter values, with
//! values per dialect where a value is not the same in every dialect. The
//! weaver fills placeholders and expands the pack's include form
//! (`[template] include`); any other special form is generator logic it
//! cannot run, so the instance is reported as not woven, never guessed.
//!
//! ```toml
//! [params]                       # every instance
//! SCHEMA = "dbo"
//! [params_for.postgres]          # every instance, PostgreSQL side only
//! SCHEMA = "public"
//!
//! [[instance]]
//! name = "Sales/SaveCustomer"    # also the woven file's name
//! template = "procs/save-entity.tpl.sql"
//! output = "Sales/SaveCustomer.sql"   # the generator's own output (optional)
//! params = { ENTITY = "Customer", TABLE = "Customer" }
//!
//! [[instance]]
//! name = "Sales/Composed"
//! parts = ["header.sql", { template = "body.tpl.sql", params = { X = "1" } }]
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use sqldiff_core::Dialect;
use sqldiff_syntax::template::SegmentKind;

use crate::compose::Placeholders;
use crate::error::{Error, Result};

/// Includes nested deeper than this are reported, not followed.
const MAX_DEPTH: usize = 32;

type Values = BTreeMap<String, String>;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestFile {
    #[serde(default)]
    params: Values,
    #[serde(default)]
    params_for: BTreeMap<String, Values>,
    #[serde(default, rename = "instance")]
    instances: Vec<InstanceFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstanceFile {
    name: String,
    template: Option<String>,
    parts: Option<Vec<PartFile>>,
    output: Option<String>,
    #[serde(default)]
    params: Values,
    #[serde(default)]
    params_for: BTreeMap<String, Values>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum PartFile {
    Path(String),
    Full {
        template: String,
        #[serde(default)]
        params: Values,
        #[serde(default)]
        params_for: BTreeMap<String, Values>,
    },
}

/// Parameter values at one level: for every dialect, and per dialect.
#[derive(Debug, Clone, Default)]
pub struct Params {
    pub all: Values,
    pub by_dialect: BTreeMap<Dialect, Values>,
}

impl Params {
    fn new(all: Values, by: BTreeMap<String, Values>, at: &str) -> Result<Params> {
        let mut by_dialect = BTreeMap::new();
        for (d, v) in by {
            let d = crate::pack::parse_dialect(&d, &format!("{at}: params_for"))?;
            by_dialect.insert(d, v);
        }
        Ok(Params { all, by_dialect })
    }

    fn names(&self) -> impl Iterator<Item = &String> {
        self.all
            .keys()
            .chain(self.by_dialect.values().flat_map(|v| v.keys()))
    }

    fn fill(&self, d: Dialect, into: &mut Values, ignore_case: bool) {
        let key = |k: &String| {
            if ignore_case {
                k.to_uppercase()
            } else {
                k.clone()
            }
        };
        for (k, v) in self
            .all
            .iter()
            .chain(self.by_dialect.get(&d).into_iter().flatten())
        {
            into.insert(key(k), v.clone());
        }
    }
}

/// One template of an instance, with values for it alone.
#[derive(Debug, Clone)]
pub struct Part {
    /// Path relative to the template tree, with `/`.
    pub template: String,
    pub params: Params,
}

/// One result of the generator.
#[derive(Debug, Clone)]
pub struct Instance {
    /// Unique; a relative path without `..`, used for the woven files.
    pub name: String,
    pub parts: Vec<Part>,
    /// The generator's own output for this instance, relative to the woven
    /// output tree (default: the name plus `.sql`).
    pub output: String,
    pub params: Params,
}

#[derive(Debug, Clone, Default)]
pub struct Manifest {
    pub params: Params,
    pub instances: Vec<Instance>,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Manifest> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::new(e.to_string()).context(path.display()))?;
        Manifest::parse(&text).map_err(|e| e.context(path.display()))
    }

    pub fn parse(text: &str) -> Result<Manifest> {
        let f: ManifestFile = toml::from_str(text).map_err(|e| Error::new(e.to_string()))?;
        let mut names = BTreeSet::new();
        let mut instances = Vec::new();
        for i in f.instances {
            let at = format!("instance `{}`", i.name);
            relative(&i.name).map_err(|e| e.context(&at))?;
            if !names.insert(i.name.clone()) {
                return Err(Error::new(format!("{at}: the name is used twice")));
            }
            let parts = match (i.template, i.parts) {
                (Some(t), None) => vec![Part {
                    template: relative(&t).map_err(|e| e.context(&at))?,
                    params: Params::default(),
                }],
                (None, Some(ps)) if !ps.is_empty() => ps
                    .into_iter()
                    .map(|p| match p {
                        PartFile::Path(t) => Ok(Part {
                            template: relative(&t)?,
                            params: Params::default(),
                        }),
                        PartFile::Full {
                            template,
                            params,
                            params_for,
                        } => Ok(Part {
                            template: relative(&template)?,
                            params: Params::new(params, params_for, &template)?,
                        }),
                    })
                    .collect::<Result<Vec<_>>>()
                    .map_err(|e| e.context(&at))?,
                _ => {
                    return Err(Error::new(format!(
                        "{at}: give either `template` or a non-empty `parts`"
                    )))
                }
            };
            let output = match i.output {
                Some(o) => relative(&o).map_err(|e| e.context(&at))?,
                None => format!("{}.sql", i.name),
            };
            instances.push(Instance {
                params: Params::new(i.params, i.params_for, &at)?,
                name: i.name,
                parts,
                output,
            });
        }
        Ok(Manifest {
            params: Params::new(f.params, f.params_for, "manifest")?,
            instances,
        })
    }

    /// Every parameter name the manifest gives a value for.
    pub fn names(&self) -> BTreeSet<String> {
        let mut n: BTreeSet<String> = self.params.names().cloned().collect();
        for i in &self.instances {
            n.extend(i.params.names().cloned());
            for p in &i.parts {
                n.extend(p.params.names().cloned());
            }
        }
        n
    }

    /// The values for one part in one dialect: manifest, then instance, then
    /// part; at each level the dialect's own values win.
    pub fn values(&self, i: &Instance, p: &Part, d: Dialect, ignore_case: bool) -> Values {
        let mut v = Values::new();
        for level in [&self.params, &i.params, &p.params] {
            level.fill(d, &mut v, ignore_case);
        }
        v
    }
}

/// A relative path that stays inside its root, with `/` separators.
fn relative(p: &str) -> Result<String> {
    let path = Path::new(p);
    if p.is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(Error::new(format!(
            "`{p}`: expected a relative path inside the tree"
        )));
    }
    Ok(p.replace('\\', "/"))
}

/// A placeholder value in woven text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Value {
    pub start: usize,
    pub end: usize,
    pub name: String,
}

/// One woven instance in one dialect.
#[derive(Debug, Clone, Default)]
pub struct Woven {
    pub text: String,
    /// Where each placeholder value landed.
    pub values: Vec<Value>,
    /// Every template it used, includes too, relative to the tree.
    pub templates: BTreeSet<String>,
    /// Why the text is not what the generator would write: a missing value,
    /// a special form that is generator logic, an unreadable include.
    pub problems: Vec<String>,
    /// Delimited names kept as SQL because no instance gives them a value.
    pub unresolved: Vec<String>,
}

/// Weaves instances from one template tree.
pub struct Weaver<'a> {
    pub root: PathBuf,
    pub dialect: Dialect,
    pub placeholders: &'a Placeholders,
    /// The include form's name (`[template] include`).
    pub include: Option<String>,
    /// The parameter list for the placeholder rules (the manifest's names).
    pub list: Option<BTreeSet<String>>,
}

impl Weaver<'_> {
    pub fn weave(&self, m: &Manifest, i: &Instance) -> Woven {
        let mut w = Woven::default();
        let ci = self.placeholders.ignore_case();
        for p in &i.parts {
            let values = m.values(i, p, self.dialect, ci);
            let mut stack = Vec::new();
            self.file(&p.template, None, &values, &mut w, &mut stack);
        }
        w
    }

    /// Weaves `rel` (relative to the root, or first to `from`'s directory).
    fn file(
        &self,
        rel: &str,
        from: Option<&str>,
        values: &Values,
        w: &mut Woven,
        stack: &mut Vec<String>,
    ) {
        let Some(rel) = self.resolve(rel, from) else {
            w.problems.push(match from {
                Some(f) => format!("{f}: included file `{rel}` is not in the template tree"),
                None => format!("template `{rel}` is not in the template tree"),
            });
            return;
        };
        if stack.contains(&rel) || stack.len() >= MAX_DEPTH {
            w.problems.push(format!(
                "{}: includes `{rel}` again (include cycle or nesting deeper than {MAX_DEPTH})",
                stack.last().map(String::as_str).unwrap_or("")
            ));
            return;
        }
        let text = match std::fs::read(self.root.join(&rel))
            .map_err(|e| e.to_string())
            .and_then(crate::changes::decode)
        {
            Ok(t) => t,
            Err(e) => {
                w.problems.push(format!("{rel}: {e}"));
                return;
            }
        };
        w.templates.insert(rel.clone());
        let (t, _) = self
            .placeholders
            .parse(&text, self.dialect, self.list.as_ref());
        stack.push(rel.clone());
        let lines = sqldiff_syntax::LineIndex::new(&text);
        for s in &t.segments {
            match &s.kind {
                SegmentKind::Sql => w.text.push_str(t.slice(s)),
                SegmentKind::Unresolved { .. } => {
                    w.unresolved.push(format!("{rel}: `{}`", t.slice(s)));
                    w.text.push_str(t.slice(s));
                }
                SegmentKind::Placeholder { name } => match values.get(name) {
                    Some(v) => {
                        let start = w.text.len();
                        w.text.push_str(v);
                        w.values.push(Value {
                            start,
                            end: w.text.len(),
                            name: name.clone(),
                        });
                    }
                    None => {
                        w.problems.push(format!(
                            "{rel}:{}: no value for {}",
                            lines.line_col(s.start).0,
                            t.slice(s)
                        ));
                        w.text.push_str(t.slice(s));
                    }
                },
                SegmentKind::Special { form, args } => {
                    if self.include.as_deref() == Some(form.as_str()) {
                        match args.first() {
                            Some(path) => self.file(path, Some(&rel), values, w, stack),
                            None => w.problems.push(format!(
                                "{rel}:{}: include form without a path",
                                lines.line_col(s.start).0
                            )),
                        }
                    } else {
                        w.problems.push(format!(
                            "{rel}:{}: directive `{form}` is generator logic the weaver cannot run",
                            lines.line_col(s.start).0
                        ));
                        w.text.push_str(t.slice(s));
                    }
                }
            }
        }
        stack.pop();
    }

    /// The included path relative to the root: next to the including file
    /// first, else from the root. Never outside the root.
    fn resolve(&self, rel: &str, from: Option<&str>) -> Option<String> {
        let rel = relative(rel).ok()?;
        let mut candidates = Vec::new();
        if let Some(dir) = from.and_then(|f| Path::new(f).parent()) {
            if !dir.as_os_str().is_empty() {
                candidates.push(format!(
                    "{}/{rel}",
                    dir.to_string_lossy().replace('\\', "/")
                ));
            }
        }
        candidates.push(rel);
        candidates.into_iter().find(|c| self.root.join(c).is_file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::{NamedPattern, TemplateSyntax};

    fn placeholders() -> Placeholders {
        Placeholders::new(&TemplateSyntax {
            open: "<%".into(),
            close: "%>".into(),
            name_pattern: "[A-Z][A-Z0-9_]*".into(),
            special_forms: vec![
                NamedPattern {
                    name: "include".into(),
                    pattern: r"<%@include\s+(\S+)\s*%>".into(),
                },
                NamedPattern {
                    name: "if".into(),
                    pattern: r"<%@if\s+(\S+)\s*%>".into(),
                },
            ],
            parameter_sources: vec![],
            collision: None,
            known_parameters: vec![],
            ignore_case: false,
            placeholders: Some("pattern".into()),
            unquoted_name_pattern: None,
            include: Some("include".into()),
        })
        .unwrap()
    }

    fn tree(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sqldiff-weave-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for (p, t) in files {
            let p = d.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        }
        d
    }

    const MANIFEST: &str = r#"
[params]
SCHEMA = "dbo"
[params_for.postgres]
SCHEMA = "public"

[[instance]]
name = "a/Customer"
template = "procs/p.tpl.sql"
params = { TABLE = "Customer" }

[[instance]]
name = "b"
parts = ["procs/p.tpl.sql", { template = "x.sql", params = { TABLE = "T2" } }]
params = { TABLE = "T1" }
output = "gen/b.sql"
"#;

    #[test]
    fn manifest_levels_and_dialects() {
        let m = Manifest::parse(MANIFEST).unwrap();
        assert_eq!(m.instances[0].output, "a/Customer.sql");
        assert_eq!(m.instances[1].output, "gen/b.sql");
        let (i, p) = (&m.instances[1], &m.instances[1].parts[1]);
        let v = m.values(i, p, Dialect::Postgres, false);
        assert_eq!(v["SCHEMA"], "public");
        assert_eq!(v["TABLE"], "T2");
        let v = m.values(i, &i.parts[0], Dialect::Tsql, false);
        assert_eq!((v["SCHEMA"].as_str(), v["TABLE"].as_str()), ("dbo", "T1"));
        assert_eq!(
            m.names().into_iter().collect::<Vec<_>>(),
            ["SCHEMA", "TABLE"]
        );
    }

    #[test]
    fn manifest_errors() {
        for (bad, why) in [
            ("[[instance]]\nname = \"../x\"\ntemplate = \"t\"\n", "relative path"),
            ("[[instance]]\nname = \"x\"\n", "either"),
            (
                "[[instance]]\nname = \"x\"\ntemplate = \"t\"\n[[instance]]\nname = \"x\"\ntemplate = \"t\"\n",
                "twice",
            ),
            ("[params_for.oracle]\nX = \"1\"\n", "oracle"),
            ("[[instance]]\nname = \"x\"\ntemplate = \"/etc/t\"\n", "relative path"),
        ] {
            let e = Manifest::parse(bad).unwrap_err();
            assert!(e.0.contains(why), "{bad}: {e}");
        }
    }

    #[test]
    fn weaves_with_includes_and_records_values() {
        let root = tree(
            "includes",
            &[
                (
                    "procs/p.tpl.sql",
                    "<%@include h.sql %>UPDATE <%SCHEMA%>.<%TABLE%> SET a = 1;\n",
                ),
                ("procs/h.sql", "-- header\n"),
                ("x.sql", "DELETE FROM <%TABLE%>;\n"),
            ],
        );
        let m = Manifest::parse(MANIFEST).unwrap();
        let p = placeholders();
        let w = Weaver {
            root: root.clone(),
            dialect: Dialect::Postgres,
            placeholders: &p,
            include: Some("include".into()),
            list: None,
        };
        let a = w.weave(&m, &m.instances[0]);
        assert_eq!(a.text, "-- header\nUPDATE public.Customer SET a = 1;\n");
        assert!(a.problems.is_empty(), "{:?}", a.problems);
        assert_eq!(
            a.values
                .iter()
                .map(|v| (&a.text[v.start..v.end], v.name.as_str()))
                .collect::<Vec<_>>(),
            [("public", "SCHEMA"), ("Customer", "TABLE")]
        );
        assert_eq!(
            a.templates.iter().collect::<Vec<_>>(),
            ["procs/h.sql", "procs/p.tpl.sql"]
        );
        let b = w.weave(&m, &m.instances[1]);
        assert!(
            b.text.ends_with("SET a = 1;\nDELETE FROM T2;\n"),
            "{}",
            b.text
        );
    }

    #[test]
    fn what_cannot_be_woven_is_a_problem() {
        let root = tree(
            "problems",
            &[
                ("c.sql", "<%@include c.sql %>"),
                (
                    "l.sql",
                    "<%@if X %>SELECT <%MISSING%>;<%@include nowhere.sql %>",
                ),
            ],
        );
        let m = Manifest::parse(
            "[[instance]]\nname = \"c\"\ntemplate = \"c.sql\"\n[[instance]]\nname = \"l\"\ntemplate = \"l.sql\"\n[[instance]]\nname = \"n\"\ntemplate = \"none.sql\"\n",
        )
        .unwrap();
        let p = placeholders();
        let w = Weaver {
            root,
            dialect: Dialect::Tsql,
            placeholders: &p,
            include: Some("include".into()),
            list: None,
        };
        let c = w.weave(&m, &m.instances[0]);
        assert!(c.problems[0].contains("cycle"), "{:?}", c.problems);
        let l = w.weave(&m, &m.instances[1]);
        let all = l.problems.join("\n");
        assert!(all.contains("directive `if`"), "{all}");
        assert!(all.contains("no value for <%MISSING%>"), "{all}");
        assert!(
            all.contains("`nowhere.sql` is not in the template tree"),
            "{all}"
        );
        let n = w.weave(&m, &m.instances[2]);
        assert!(
            n.problems[0].contains("not in the template tree"),
            "{:?}",
            n.problems
        );
    }
}
