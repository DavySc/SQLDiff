//! Tables of the tree whose CREATE TABLE the rules refused, standing in the
//! target catalog for binding only.
//!
//! A table refused for one part of it (a computed column with no exact
//! target expression, `DURABILITY = SCHEMA_ONLY`, a memory-optimized
//! system-versioned table) leaves every statement that uses it unbound on
//! side B: "table … does not exist". When the rest of the table converts,
//! that rest stands in the target catalog: the CREATE TABLE without the
//! refused parts, converted by the same rules. The refused statement stays
//! as it was (not converted, kept as a comment); the stand-in is never
//! written and never counts as converted.
//!
//! Every unit that names a stand-in table carries `ASM-TABLE-STAND-IN`: it
//! holds once the target table is created with the stand-in's columns and
//! types. A unit that may touch a refused column (its name appears, a `*`
//! that is not `COUNT(*)`, an INSERT without a column list) is not proven:
//! the column's target form is not known. Names are matched by text, as for
//! base catalogs: a false alarm, never a miss; names inside dynamic SQL
//! strings are not seen.

use sqldiff_core::catalog::{ddl, key, Catalog, Column, DataType, Ident};
use sqldiff_core::tokens::{close_paren, of_node, qualified, split_commas, texts, Tok};
use sqldiff_core::Dialect;
use sqldiff_syntax::SyntaxKind;

use crate::base::{nodes, refs_at, refs_in, Key, Node};
use crate::{Conversion, Note, Status};

/// The rule a unit naming a stand-in table carries.
pub const STAND_IN: &str = "ASM-TABLE-STAND-IN";
/// The rule a unit that may touch a refused column carries.
pub const REFUSED_COLUMN: &str = "tree.refused-column";

/// One refused table and what was left out of its stand-in.
struct StandIn {
    key: Key,
    /// `schema.name` as written.
    name: String,
    /// The defining statement: file index and byte offset.
    file: usize,
    at: usize,
    /// `path line N`.
    place: String,
    /// The rules' reason for refusing the CREATE TABLE.
    reason: String,
    /// Refused columns, as stored in the source dialect.
    columns: Vec<String>,
    /// Table options left out (`DURABILITY = SCHEMA_ONLY`, …).
    options: Vec<String>,
    /// CHECK constraints and indexes left out.
    others: Vec<String>,
    /// Indexes the tree puts on it: what names one binds through it too.
    indexes: Vec<Key>,
    /// A view of the tree over a stand-in (`reason` names it), not a
    /// refused table.
    view: bool,
    /// A view that may show a refused column: every use may touch one.
    exposes: bool,
}

#[derive(Default)]
pub struct StandIns {
    tables: Vec<StandIn>,
}

/// What is left of a CREATE TABLE without its refused parts.
struct Relaxed {
    text: String,
    /// Refused columns and their place among the column definitions.
    columns: Vec<(usize, String)>,
    options: Vec<String>,
    /// Table-level CHECK constraints and non-unique inline indexes left out.
    others: Vec<String>,
}

/// The CREATE TABLE `t` (the statement's tokens, offsets into `text`)
/// without its computed columns and without the memory-optimized and
/// durability table options; with `more`, also without its table-level CHECK
/// constraints and non-unique inline indexes, which the catalog does not
/// hold anyway. `None` when nothing is left out, nothing would be left, or it
/// is not a plain CREATE TABLE.
fn relax(text: &str, t: &[Tok], d: Dialect, more: bool) -> Option<Relaxed> {
    if !(t.first()?.is_kw("CREATE") && t.get(1)?.is_kw("TABLE")) {
        return None;
    }
    let (_, open) = qualified(t, 2);
    if !t.get(open)?.is(SyntaxKind::L_PAREN) {
        return None;
    }
    let close = close_paren(t, open);
    if close >= t.len() {
        return None;
    }
    let piece = |x: &[Tok]| &text[x[0].start..x[x.len() - 1].end];
    let mut columns = Vec::new();
    let mut kept = Vec::new();
    let mut others = Vec::new();
    let mut position = 0;
    for item in split_commas(&t[open + 1..close]) {
        let Some(first) = item.first() else { continue };
        let constraint = first.is_any_kw(CONSTRAINT_STARTS);
        let check = first.is_kw("CHECK")
            || first.is_kw("CONSTRAINT") && item.get(2).is_some_and(|x| x.is_kw("CHECK"));
        let index = first.is_kw("INDEX") && !item.iter().any(|x| x.is_kw("UNIQUE"));
        if !constraint && first.is_name() && item.get(1).is_some_and(|x| x.is_kw("AS")) {
            columns.push((position, first.ident().stored(d)));
        } else if more && (check || index) {
            let name = match (check, item.get(1)) {
                (true, Some(n)) if first.is_kw("CONSTRAINT") => {
                    format!("CHECK constraint {}", n.text)
                }
                (true, _) => "a CHECK constraint".to_string(),
                (false, Some(n)) => format!("index {}", n.text),
                (false, None) => "an index".to_string(),
            };
            others.push(name);
        } else {
            kept.push(piece(item));
        }
        if !constraint {
            position += 1;
        }
    }
    if position == columns.len() {
        return None;
    }
    let mut out = format!("{}\n    {}\n)", &text[..t[open].end], kept.join(",\n    "));
    // `WITH (option, …)` after the column list.
    let mut options = Vec::new();
    let mut rest = close + 1;
    if t.get(rest).is_some_and(|x| x.is_kw("WITH"))
        && t.get(rest + 1).is_some_and(|x| x.is(SyntaxKind::L_PAREN))
    {
        let o_close = close_paren(t, rest + 1);
        if o_close < t.len() {
            let mut keep = Vec::new();
            for o in split_commas(&t[rest + 2..o_close]) {
                if o.is_empty() {
                    continue;
                }
                if o[0].is_any_kw(&["MEMORY_OPTIMIZED", "DURABILITY"]) {
                    options.push(texts(o).join(" "));
                } else {
                    keep.push(piece(o));
                }
            }
            if !options.is_empty() {
                if !keep.is_empty() {
                    out.push_str(&format!("\nWITH ({})", keep.join(", ")));
                }
                rest = o_close + 1;
            }
        }
    }
    if columns.is_empty() && options.is_empty() || more && others.is_empty() {
        return None;
    }
    match t.get(rest) {
        Some(x) => {
            out.push('\n');
            out.push_str(&text[x.start..]);
        }
        None => out.push(';'),
    }
    Some(Relaxed {
        text: out,
        columns,
        options,
        others,
    })
}

const CONSTRAINT_STARTS: &[&str] = &[
    "CONSTRAINT",
    "PRIMARY",
    "UNIQUE",
    "FOREIGN",
    "CHECK",
    "INDEX",
    "PERIOD",
];

impl StandIns {
    /// Finds the tree's refused tables (`files`: path and source text; the
    /// rules' drafts, `outputs`, gave `target`, the target catalog) and adds
    /// the stand-in of each one whose rest converts to `target`, with what
    /// the outputs put on it (indexes, keys added by ALTER TABLE).
    pub fn build<'o>(
        files: &[(&str, &str)],
        outputs: impl Iterator<Item = &'o str>,
        target: &mut Catalog,
        from: Dialect,
        to: Dialect,
        source: Option<&Catalog>,
    ) -> StandIns {
        let named = |c: &Catalog, k: &Key| {
            c.tables.iter().any(|t| {
                key(to, &t.name) == k.name
                    && match (&t.schema, &k.schema) {
                        (Some(s), Some(ks)) => key(to, s) == *ks,
                        _ => true,
                    }
            })
        };
        let mut out = StandIns::default();
        let mut add = Catalog::default();
        let all: Vec<Vec<Node>> = files.iter().map(|(_, text)| nodes(text, from)).collect();
        for (fi, (path, text)) in files.iter().enumerate() {
            for n in &all[fi] {
                let [o] = n.defines.as_slice() else { continue };
                if o.key.routine || named(target, &o.key) || named(&add, &o.key) {
                    continue;
                }
                let stmt = &text[n.start..n.end];
                let root = sqldiff_syntax::parse(stmt, from).syntax();
                let Some(node) = root
                    .descendants()
                    .find(|x| x.kind() == SyntaxKind::STATEMENT)
                else {
                    continue;
                };
                let t = of_node(&node);
                // Why the rules refused it.
                let whole = crate::rewrite_in(stmt, from, to, source);
                let Some(reason) = whole.worst_note(Status::NotConverted) else {
                    continue;
                };
                let Some((r, draft)) = [false, true].into_iter().find_map(|more| {
                    let r = relax(stmt, &t, from, more)?;
                    // The history table of a system-versioned table holds
                    // the left-out columns too: without them on both sides.
                    let src = source.map(|c| without_columns(c, from, &o.key, &r.columns));
                    let draft = crate::rewrite_in(&r.text, from, to, src.as_ref());
                    draft
                        .worst_note(Status::NotConverted)
                        .is_none()
                        .then_some((r, draft))
                }) else {
                    continue;
                };
                let mut c = Catalog::default();
                let root = sqldiff_syntax::parse(draft.output(), to).syntax();
                ddl::load_tree(&mut c, &root, to);
                let Some(mut table) = c
                    .tables
                    .into_iter()
                    .find(|x| key(to, &x.name) == o.key.name)
                else {
                    continue;
                };
                for (pos, name) in &r.columns {
                    let col = Column {
                        name: name.to_ascii_lowercase(),
                        data_type: DataType::unknown(),
                        nullable: true,
                        identity: false,
                        has_default: false,
                        generated: true,
                        collation: None,
                    };
                    let at = (*pos).min(table.columns.len());
                    table.columns.insert(at, col);
                }
                add.put_table(to, table);
                out.tables.push(StandIn {
                    key: o.key.clone(),
                    name: o.name.clone(),
                    file: fi,
                    at: n.start,
                    place: format!("{path} line {}", n.line),
                    reason,
                    columns: r.columns.into_iter().map(|(_, c)| c).collect(),
                    options: r.options,
                    others: r.others,
                    indexes: Vec::new(),
                    view: false,
                    exposes: false,
                });
            }
        }
        if out.tables.is_empty() {
            return out;
        }
        // The outputs' statements on the stand-ins, which found no table
        // when the target catalog was built.
        let mut on = add.clone();
        for text in outputs {
            let root = sqldiff_syntax::parse(text, to).syntax();
            ddl::load_tree(&mut on, &root, to);
        }
        for t in &mut add.tables {
            if let Some(x) = on
                .tables
                .iter()
                .find(|x| x.name == t.name && x.schema == t.schema)
            {
                *t = x.clone();
            }
            let Some(s) = out
                .tables
                .iter_mut()
                .find(|s| key(to, &t.name) == s.key.name)
            else {
                continue;
            };
            s.indexes = t
                .indexes
                .iter()
                .map(|i| Key::new(to, false, t.schema.as_deref(), &i.name))
                .collect();
        }
        target.extend(to, &add);
        // A view over a stand-in binds through it (its columns are the
        // stand-in's): what uses the view is flagged too, until nothing
        // more changes.
        loop {
            let mut grew = false;
            for (fi, (path, text)) in files.iter().enumerate() {
                for n in &all[fi] {
                    let [o] = n.defines.as_slice() else { continue };
                    if o.kind != "view" || out.tables.iter().any(|s| s.key == o.key) {
                        continue;
                    }
                    let stmt = &text[n.start..n.end];
                    let via: Vec<usize> = refs_at(stmt, from)
                        .iter()
                        .filter_map(|(_, r)| out.tables.iter().position(|s| s.key.same_name(r)))
                        .collect();
                    let Some(&first) = via.first() else { continue };
                    let toks = of_node(&sqldiff_syntax::parse(stmt, from).syntax());
                    let exposes = via
                        .iter()
                        .any(|&i| touches(&toks, &out.tables[i], from).is_some());
                    let reason = format!("a view over `{}`", out.tables[first].name);
                    out.tables.push(StandIn {
                        key: o.key.clone(),
                        name: o.name.clone(),
                        file: fi,
                        at: n.start,
                        place: format!("{path} line {}", n.line),
                        reason,
                        columns: Vec::new(),
                        options: Vec::new(),
                        others: Vec::new(),
                        indexes: Vec::new(),
                        view: true,
                        exposes,
                    });
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
        out
    }

    /// Flags every unit of `convs` (converted from `files`) that names a
    /// stand-in table, on the innermost unit around each name.
    pub fn flag<'c>(
        &self,
        files: &[(&str, &str)],
        convs: impl Iterator<Item = &'c mut Conversion>,
        from: Dialect,
        to: Dialect,
    ) {
        if self.tables.is_empty() {
            return;
        }
        for (fi, ((_, text), c)) in files.iter().zip(convs).enumerate() {
            // Names in the source and in the output: a name the source
            // holds in a string (sp_addextendedproperty) is a name in the
            // output (COMMENT ON). Names in SQL held in a string (dynamic
            // SQL whose text is known, which the engine compares) count too.
            let (src_toks, out_toks) = (tokens(text, from, 0), tokens(&c.output, to, 0));
            let mut hits: Vec<Vec<usize>> = vec![Vec::new(); c.units.len()];
            let found = refs_in(&src_toks, from)
                .into_iter()
                .map(|(at, r)| (innermost(c, at, false), r))
                .chain(
                    refs_in(&out_toks, to)
                        .into_iter()
                        .map(|(at, r)| (innermost(c, at, true), r)),
                );
            for (ui, r) in found {
                let Some(ui) = ui else { continue };
                for (si, s) in self.tables.iter().enumerate() {
                    let named = s.key.same_name(&r) || s.indexes.iter().any(|i| i.same_name(&r));
                    if !named || hits[ui].contains(&si) {
                        continue;
                    }
                    // The refused statement itself stays as it is.
                    if s.file == fi && c.units[ui].src.start <= s.at && s.at < c.units[ui].src.end {
                        continue;
                    }
                    hits[ui].push(si);
                }
            }
            let mut changed = false;
            for (ui, his) in hits.into_iter().enumerate() {
                if his.is_empty() {
                    continue;
                }
                let u = &mut c.units[ui];
                let within = |t: &[Tok], (a, b): (usize, usize)| -> std::ops::Range<usize> {
                    t.partition_point(|x| x.start < a)..t.partition_point(|x| x.start < b)
                };
                let src_span = &src_toks[within(&src_toks, (u.src.start, u.src.end))];
                let out_span = &out_toks[within(&out_toks, (u.out.start, u.out.end))];
                let mut parts = Vec::new();
                let mut touched = Vec::new();
                for si in his {
                    let s = &self.tables[si];
                    if let Some(why) =
                        touches(src_span, s, from).or_else(|| touches(out_span, s, to))
                    {
                        touched.push(format!("`{}` {why}", s.name));
                    }
                    if s.view {
                        parts.push(format!("`{}` ({}: {})", s.name, s.place, s.reason));
                        continue;
                    }
                    let mut left = Vec::new();
                    if !s.columns.is_empty() {
                        left.push(format!("computed column(s) {}", s.columns.join(", ")));
                    }
                    if !s.options.is_empty() {
                        left.push(format!("option(s) {}", s.options.join(", ")));
                    }
                    left.extend(s.others.iter().cloned());
                    parts.push(format!(
                        "`{}` ({}: {}; checked against the rest of it, converted by the same rules, without {})",
                        s.name,
                        s.place,
                        s.reason,
                        left.join(" and ")
                    ));
                }
                u.notes.push(Note {
                    status: Status::Assumptions,
                    rule: STAND_IN.into(),
                    text: format!(
                        "Uses tables whose CREATE TABLE was not converted, or views over them: {}. The statement holds once the target table is created with the stand-in's columns and types; accept this when it is (a column or alias named like the table also counts).",
                        parts.join("; ")
                    ),
                });
                if !touched.is_empty() {
                    let at = u.notes.partition_point(|n| n.status >= Status::Unproven);
                    u.notes.insert(
                        at,
                        Note {
                            status: Status::Unproven,
                            rule: REFUSED_COLUMN.into(),
                            text: format!(
                                "May use a refused column, whose target form is not known, so this is not compared: {}.",
                                touched.join("; ")
                            ),
                        },
                    );
                    u.status = u.status.max(Status::Unproven);
                } else {
                    u.status = u.status.max(Status::Assumptions);
                }
                changed = true;
            }
            if changed {
                crate::recount(c);
            }
        }
    }
}

/// `c` with the columns `cols` left out of `table` and, when it is
/// system-versioned, of its history table, as the relaxed CREATE TABLE
/// leaves them out: the versioning rule compares the two.
fn without_columns(c: &Catalog, d: Dialect, table: &Key, cols: &[(usize, String)]) -> Catalog {
    let mut c = c.clone();
    let is = |t: &sqldiff_core::catalog::Table, k: &Key| {
        key(d, &t.name) == k.name
            && (k.schema.is_none() || t.schema.as_deref().map(|s| key(d, s)) == k.schema)
    };
    let history = c
        .tables
        .iter()
        .find(|t| is(t, table))
        .and_then(|t| t.versioning.as_ref()?.history.clone())
        .and_then(|h| {
            let parts: Vec<Ident> = h.iter().map(|p| Ident::plain(p.clone())).collect();
            Key::of_parts(d, &parts)
        });
    for t in &mut c.tables {
        if is(t, table) || history.as_ref().is_some_and(|h| is(t, h)) {
            t.columns
                .retain(|x| !cols.iter().any(|(_, n)| key(d, n) == key(d, &x.name)));
        }
    }
    c
}

/// The tokens of `text`, each string literal followed by the tokens of the
/// SQL it may hold (at the literal's place), to a few levels of nesting.
fn tokens(text: &str, d: Dialect, depth: usize) -> Vec<Tok> {
    let mut out = Vec::new();
    for t in of_node(&sqldiff_syntax::parse(text, d).syntax()) {
        let inner = (depth < 3 && matches!(t.kind, SyntaxKind::STRING | SyntaxKind::DOLLAR_STRING))
            .then(|| literal(&t.text))
            .flatten();
        let (start, end) = (t.start, t.end);
        out.push(t);
        if let Some(sql) = inner {
            out.extend(tokens(&sql, d, depth + 1).into_iter().map(|mut x| {
                x.start = start;
                x.end = end;
                x
            }));
        }
    }
    out
}

/// A string literal's text: `N'…'`, `E'…'`, `'…'` with doubled quotes, or
/// `$tag$…$tag$`.
fn literal(raw: &str) -> Option<String> {
    if let Some(rest) = raw.strip_prefix('$') {
        let tag_end = rest.find('$')? + 2;
        let tag = &raw[..tag_end];
        return raw[tag_end..].strip_suffix(tag).map(str::to_string);
    }
    let open = raw.find('\'')?;
    let body = raw[open + 1..].strip_suffix('\'')?;
    Some(body.replace("''", "'"))
}

/// The innermost unit whose source (or output) holds byte `at`.
fn innermost(c: &Conversion, at: usize, out: bool) -> Option<usize> {
    let span = |u: &crate::Unit| if out { u.out } else { u.src };
    c.units
        .iter()
        .enumerate()
        .filter(|(_, u)| span(u).start <= at && at < span(u).end.max(span(u).start + 1))
        .min_by_key(|(_, u)| span(u).end - span(u).start)
        .map(|(i, _)| i)
}

/// Whether the unit's tokens may touch a refused column of `s`, and how.
fn touches(t: &[Tok], s: &StandIn, d: Dialect) -> Option<String> {
    if s.exposes {
        return Some("(the view may show a refused column)".into());
    }
    if s.columns.is_empty() {
        return None;
    }
    // Letter case ignored: the output folds what the source kept.
    for x in t {
        let n = x.ident().key(d).to_lowercase();
        if x.is_name() && s.columns.iter().any(|c| c.to_lowercase() == n) {
            return Some(format!("(names column {})", x.text));
        }
    }
    for (i, x) in t.iter().enumerate() {
        if x.text == "*" && !(i > 0 && t[i - 1].is(SyntaxKind::L_PAREN)) {
            return Some("(a `*` may include the refused columns)".into());
        }
    }
    // An INSERT into the table without a column list fills its columns by
    // position (in a MERGE, `INSERT VALUES`, whatever the target).
    let names_it = |j: usize| {
        let (parts, next) = qualified(t, j);
        let tail = &parts[parts.len().saturating_sub(2)..];
        (
            Key::of_parts(d, tail).is_some_and(|k| s.key.same_name(&k)),
            next,
        )
    };
    let merge = t.iter().any(|x| x.is_kw("MERGE"));
    for (i, x) in t.iter().enumerate() {
        if !x.is_kw("INSERT") {
            continue;
        }
        let mut j = i + 1;
        if t.get(j).is_some_and(|y| y.is_kw("INTO")) {
            j += 1;
        }
        let positional = if t
            .get(j)
            .is_some_and(|y| y.is_any_kw(&["VALUES", "DEFAULT"]))
        {
            merge
        } else {
            let (hit, next) = names_it(j);
            hit && !t.get(next).is_some_and(|y| y.is(SyntaxKind::L_PAREN))
        };
        if positional {
            return Some("(an INSERT without a column list fills columns by position)".into());
        }
    }
    None
}
