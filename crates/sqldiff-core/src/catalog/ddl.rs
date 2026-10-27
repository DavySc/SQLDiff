//! Reads catalog facts from DDL text in any of the three dialects:
//! `CREATE TABLE`, `CREATE [UNIQUE] INDEX`, `ALTER TABLE ... ADD/DROP COLUMN`,
//! `CREATE VIEW`, `CREATE SCHEMA`, `CREATE PROCEDURE` / `FUNCTION` and
//! Db2 `DECLARE GLOBAL TEMPORARY TABLE`.

use sqldiff_syntax::{parse, Dialect, SyntaxKind, SyntaxNode};

use super::{
    Catalog, Column, DataType, Ident, Index, Key, Param, ParamMode, Routine, RoutineKind, Table,
    TableKind,
};
use crate::tokens::{self, close_paren, find_kw, qualified, split_commas, Tok};

/// Loads every top-level definition in `text`.
pub fn load(text: &str, dialect: Dialect, origin: &str) -> Catalog {
    let mut cat = Catalog {
        origin: origin.to_string(),
        ..Catalog::default()
    };
    let root = parse(text, dialect).syntax();
    load_tree(&mut cat, &root, dialect);
    cat
}

/// Adds the definitions found at the top level of a parsed tree.
pub fn load_tree(cat: &mut Catalog, root: &SyntaxNode, dialect: Dialect) {
    for node in root.descendants() {
        let top = node
            .parent()
            .is_some_and(|p| matches!(p.kind(), SyntaxKind::SOURCE_FILE | SyntaxKind::BATCH));
        if !top {
            continue;
        }
        match node.kind() {
            SyntaxKind::STATEMENT => apply_statement(cat, &tokens::of_node(&node), dialect),
            SyntaxKind::ROUTINE => {
                if let Some(h) = routine_header(&node, dialect) {
                    match h.view {
                        Some(cols) => {
                            let body: Vec<Tok> = node
                                .children()
                                .filter(|c| c.kind() == SyntaxKind::ROUTINE_BODY)
                                .flat_map(|b| tokens::of_node(&b))
                                .collect();
                            let t = view_table(cat, dialect, h.name.clone(), cols, &body);
                            cat.put_table(dialect, t);
                        }
                        None => cat.put_routine(dialect, h.routine),
                    }
                }
            }
            _ => {}
        }
    }
}

const TABLE_PREFIXES: &[&str] = &[
    "GLOBAL",
    "LOCAL",
    "TEMPORARY",
    "TEMP",
    "UNLOGGED",
    "OR",
    "REPLACE",
];

/// Applies one top-level statement.
pub fn apply_statement(cat: &mut Catalog, t: &[Tok], dialect: Dialect) {
    let Some(first) = t.first() else { return };
    if first.is_kw("CREATE") {
        let mut i = 1;
        while t.get(i).is_some_and(|x| x.is_any_kw(TABLE_PREFIXES)) {
            i += 1;
        }
        match t.get(i).map(Tok::upper).as_deref() {
            Some("TABLE") => {
                if let Some(tb) = create_table(t, dialect) {
                    cat.put_table(dialect, tb);
                }
            }
            Some("UNIQUE" | "INDEX" | "CLUSTERED" | "NONCLUSTERED") => {
                create_index(cat, t, i, dialect)
            }
            Some("VIEW") => {
                let (name, j) = qualified(t, i + 1);
                let mut cols = None;
                let mut j = j;
                if t.get(j).is_some_and(|x| x.is(SyntaxKind::L_PAREN)) {
                    let c = close_paren(t, j);
                    cols = Some(
                        split_commas(&t[j + 1..c])
                            .iter()
                            .filter_map(|e| e.first().map(Tok::ident))
                            .collect(),
                    );
                    j = c + 1;
                }
                if let Some(as_at) = find_kw(t, j, "AS") {
                    let tb = view_table(cat, dialect, name, cols, &t[as_at + 1..]);
                    cat.put_table(dialect, tb);
                }
            }
            Some("SCHEMA") => {
                if let Some(n) = t.get(i + 1).filter(|x| x.is_name()) {
                    cat.schemas.push(n.ident().stored(dialect));
                }
            }
            _ => {}
        }
    } else if first.is_kw("DECLARE") && t.get(1).is_some_and(|x| x.is_kw("GLOBAL")) {
        if let Some(tb) = create_table(t, dialect) {
            cat.put_table(dialect, tb);
        }
    } else if first.is_kw("ALTER") && t.get(1).is_some_and(|x| x.is_kw("TABLE")) {
        alter_table(cat, t, dialect);
    }
}

fn stored_name(parts: &[Ident], dialect: Dialect) -> Option<(Option<String>, String)> {
    let name = parts.last()?.stored(dialect);
    let schema = (parts.len() >= 2).then(|| parts[parts.len() - 2].stored(dialect));
    Some((schema, name))
}

/// `CREATE [TEMP] TABLE name (...)`, `DECLARE GLOBAL TEMPORARY TABLE name (...)`.
pub fn create_table(t: &[Tok], dialect: Dialect) -> Option<Table> {
    let at = t.iter().position(|x| x.is_kw("TABLE"))?;
    let temp = t[..at]
        .iter()
        .any(|x| x.is_any_kw(&["TEMPORARY", "TEMP", "GLOBAL"]));
    let mut i = at + 1;
    if t.get(i).is_some_and(|x| x.is_kw("IF")) {
        i += 3; // IF NOT EXISTS
    }
    let (parts, j) = qualified(t, i);
    let (schema, name) = stored_name(&parts, dialect)?;
    let hash_temp = dialect == Dialect::Tsql && name.starts_with('#');
    let kind = if temp || hash_temp {
        TableKind::Temporary
    } else {
        TableKind::Table
    };
    let mut tb = Table::new(schema, name, kind);
    if t.get(j).is_some_and(|x| x.is(SyntaxKind::L_PAREN)) {
        let c = close_paren(t, j);
        column_defs(&mut tb, &t[j + 1..c], dialect);
    } else {
        // `AS SELECT`, `LIKE other`, `AS (subquery) WITH DATA`.
        tb.columns_known = false;
    }
    Some(tb)
}

const COLUMN_OPTIONS: &[&str] = &[
    "NOT",
    "NULL",
    "PRIMARY",
    "UNIQUE",
    "DEFAULT",
    "IDENTITY",
    "GENERATED",
    "COLLATE",
    "REFERENCES",
    "CHECK",
    "CONSTRAINT",
    "AS",
    "ALLOCATE",
    "CCSID",
    "FOR",
    "IMPLICITLY",
    "SPARSE",
    "ROWGUIDCOL",
    "FILESTREAM",
    "MASKED",
    "ENCRYPTED",
    "COMPRESSION",
    "STORAGE",
    "HIDDEN",
    "WITH",
    "OUTPUT",
    "OUT",
    "READONLY",
];

const TABLE_CONSTRAINTS: &[&str] = &[
    "CONSTRAINT",
    "PRIMARY",
    "UNIQUE",
    "FOREIGN",
    "CHECK",
    "INDEX",
    "EXCLUDE",
    "PERIOD",
    "LIKE",
];

/// Whether a table element is a constraint (or index, period, LIKE) rather
/// than a column whose name happens to be one of those words (`period int`).
fn is_constraint(el: &[Tok], dialect: Dialect) -> bool {
    let Some(first) = el.first() else {
        return false;
    };
    if first.kind != SyntaxKind::IDENT || !first.is_any_kw(TABLE_CONSTRAINTS) {
        return false;
    }
    let next = el.get(1);
    let paren = |x: Option<&Tok>| x.is_some_and(|x| x.is(SyntaxKind::L_PAREN));
    match first.upper().as_str() {
        "CONSTRAINT" => el.get(2).is_some_and(|x| {
            x.is_any_kw(&[
                "PRIMARY", "UNIQUE", "FOREIGN", "CHECK", "EXCLUDE", "DEFAULT",
            ])
        }),
        "PRIMARY" | "FOREIGN" => next.is_some_and(|x| x.is_kw("KEY")),
        "UNIQUE" => {
            paren(next)
                || next.is_some_and(|x| {
                    x.is_any_kw(&["KEY", "INDEX", "NULLS", "CLUSTERED", "NONCLUSTERED"])
                })
        }
        "CHECK" => paren(next) || next.is_some_and(|x| x.is_kw("NOT")),
        "EXCLUDE" => paren(next) || next.is_some_and(|x| x.is_kw("USING")),
        "PERIOD" => next.is_some_and(|x| x.is_kw("FOR")),
        // T-SQL inline index: `INDEX ix (a, b)` or `INDEX ix CLUSTERED ...`;
        // `index varchar(10)` is a column with a length argument.
        "INDEX" => match el.get(2) {
            Some(x) if x.is(SyntaxKind::L_PAREN) => el[3..].first().is_some_and(|y| y.is_name()),
            Some(x) => x.is_any_kw(&["CLUSTERED", "NONCLUSTERED", "UNIQUE", "COLUMNSTORE"]),
            None => false,
        },
        "LIKE" => next.is_some_and(|x| {
            x.is_name()
                && super::builtins::of(dialect).data_type(&x.upper(), "")
                    == super::builtins::Availability::Unknown
        }),
        _ => false,
    }
}

/// Reads the element list of a table definition (between the parentheses).
pub fn column_defs(tb: &mut Table, inner: &[Tok], dialect: Dialect) {
    for el in split_commas(inner) {
        let Some(first) = el.first() else { continue };
        if is_constraint(el, dialect) {
            if first.is_kw("LIKE") {
                // PostgreSQL `(LIKE other)`: the columns come from another table.
                tb.columns_known = false;
            }
            table_constraint(tb, el, dialect);
            continue;
        }
        if !first.is_name() {
            continue;
        }
        let name = first.ident().stored(dialect);
        let ty_end = type_end(el, 1);
        let data_type = DataType::parse(&tokens::texts(&el[1..ty_end]));
        let mut col = Column {
            name: name.clone(),
            data_type,
            nullable: true,
            identity: false,
            has_default: false,
            generated: false,
            collation: None,
        };
        let opts = &el[ty_end..];
        let mut k = 0;
        while k < opts.len() {
            let o = &opts[k];
            match o.upper().as_str() {
                "NOT" if opts.get(k + 1).is_some_and(|x| x.is_kw("NULL")) => {
                    col.nullable = false;
                    k += 1;
                }
                "PRIMARY" => {
                    col.nullable = false;
                    tb.keys.push(Key {
                        name: None,
                        primary: true,
                        columns: vec![name.clone()],
                    });
                }
                "UNIQUE" => tb.keys.push(Key {
                    name: None,
                    primary: false,
                    columns: vec![name.clone()],
                }),
                "IDENTITY" => {
                    col.identity = true;
                    col.nullable = false;
                }
                "GENERATED" => {
                    if opts[k..].iter().any(|x| x.is_kw("IDENTITY")) {
                        col.identity = true;
                        col.nullable = false;
                    } else {
                        col.has_default = true;
                        // GENERATED ALWAYS AS ROW START / END, AS (expr) STORED.
                        col.generated = opts.get(k + 1).is_some_and(|x| x.is_kw("ALWAYS"));
                    }
                }
                "DEFAULT" => col.has_default = true,
                "COLLATE" => {
                    col.collation = opts.get(k + 1).map(|x| x.ident().text);
                    k += 1;
                }
                "AS" if k == 0 => {
                    // T-SQL computed column: `c AS (expr)`.
                    col.has_default = true;
                    col.generated = true;
                }
                _ => {}
            }
            k += 1;
        }
        tb.columns
            .retain(|c| super::key(dialect, &c.name) != super::key(dialect, &name));
        tb.columns.push(col);
    }
}

/// End of the type words in a column or parameter definition starting at `from`.
pub fn type_end(el: &[Tok], from: usize) -> usize {
    let mut k = from;
    while k < el.len() {
        let x = &el[k];
        match x.kind {
            SyntaxKind::L_PAREN => {
                if k == from {
                    break;
                }
                k = close_paren(el, k) + 1;
                continue;
            }
            SyntaxKind::L_BRACKET | SyntaxKind::R_BRACKET => {}
            SyntaxKind::IDENT => {
                let with_tz = x.is_any_kw(&["WITH", "WITHOUT"])
                    && el
                        .get(k + 1)
                        .is_some_and(|n| n.is_any_kw(&["TIME", "LOCAL"]));
                if !with_tz && x.is_any_kw(COLUMN_OPTIONS) {
                    break;
                }
            }
            SyntaxKind::QUOTED_IDENT | SyntaxKind::DOT => {}
            SyntaxKind::OP if x.text == "%" => {}
            _ => break,
        }
        k += 1;
    }
    k.min(el.len())
}

fn paren_names(t: &[Tok], dialect: Dialect) -> (Vec<String>, usize) {
    let Some(open) = t.iter().position(|x| x.is(SyntaxKind::L_PAREN)) else {
        return (Vec::new(), t.len());
    };
    let c = close_paren(t, open);
    let names = split_commas(&t[open + 1..c])
        .iter()
        .filter_map(|e| e.first().filter(|x| x.is_name()))
        .map(|x| x.ident().stored(dialect))
        .collect();
    (names, c + 1)
}

fn table_constraint(tb: &mut Table, el: &[Tok], dialect: Dialect) {
    let mut i = 0;
    let mut name = None;
    if el[0].is_kw("CONSTRAINT") {
        name = el.get(1).map(|x| x.ident().stored(dialect));
        i = 2;
    }
    let Some(head) = el.get(i) else { return };
    let primary = head.is_kw("PRIMARY");
    if !(primary || head.is_kw("UNIQUE")) {
        return;
    }
    let (cols, _) = paren_names(&el[i..], dialect);
    if primary {
        for c in &mut tb.columns {
            if cols
                .iter()
                .any(|k| super::key(dialect, k) == super::key(dialect, &c.name))
            {
                c.nullable = false;
            }
        }
    }
    tb.keys.push(Key {
        name,
        primary,
        columns: cols,
    });
}

/// `CREATE [UNIQUE] [CLUSTERED|NONCLUSTERED] INDEX name ON table (cols)`.
fn create_index(cat: &mut Catalog, t: &[Tok], from: usize, dialect: Dialect) {
    let unique = t[from..].iter().take(3).any(|x| x.is_kw("UNIQUE"));
    let Some(ix) = t.iter().position(|x| x.is_kw("INDEX")) else {
        return;
    };
    let mut i = ix + 1;
    if t.get(i).is_some_and(|x| x.is_kw("CONCURRENTLY")) {
        i += 1;
    }
    if t.get(i).is_some_and(|x| x.is_kw("IF")) {
        i += 3;
    }
    let name = t
        .get(i)
        .filter(|x| x.is_name() && !x.is_kw("ON"))
        .map(|x| x.ident().stored(dialect))
        .unwrap_or_default();
    let Some(on) = find_kw(t, i, "ON") else {
        return;
    };
    let mut j = on + 1;
    if t.get(j).is_some_and(|x| x.is_kw("ONLY")) {
        j += 1;
    }
    let (parts, mut after) = qualified(t, j);
    // T-SQL / PG `USING method` before the column list.
    if t.get(after).is_some_and(|x| x.is_kw("USING")) {
        after += 2;
    }
    let (cols, _) = paren_names(&t[after..], dialect);
    // Only a plain column list without a filter is a key: an expression
    // element or a WHERE clause (partial, filtered index) is not.
    let plain = t.get(after).is_some_and(|x| x.is(SyntaxKind::L_PAREN)) && {
        let close = close_paren(t, after);
        split_commas(&t[after + 1..close]).iter().all(|el| {
            matches!(el, [c] | [c, _] if c.is_name()
                && el.get(1).map_or(true, |o| o.is_any_kw(&["ASC", "DESC"])))
        }) && !t[close..].iter().any(|x| x.is_kw("WHERE"))
    };
    let unique = unique && plain && !cols.is_empty();
    if let Some(i) = super::table_position(dialect, cat, &parts) {
        cat.tables[i].indexes.push(Index {
            name,
            unique,
            columns: cols,
        });
    }
}

/// `ALTER TABLE t ADD [COLUMN] c type ...`, `DROP [COLUMN] c`.
fn alter_table(cat: &mut Catalog, t: &[Tok], dialect: Dialect) {
    let mut i = 2;
    if t.get(i).is_some_and(|x| x.is_kw("IF")) {
        i += 2;
    }
    if t.get(i).is_some_and(|x| x.is_kw("ONLY")) {
        i += 1;
    }
    let (parts, j) = qualified(t, i);
    let Some(i) = super::table_position(dialect, cat, &parts) else {
        return;
    };
    let tb = &mut cat.tables[i];
    // T-SQL lists several items after one verb: `ADD a int, b int`,
    // `DROP COLUMN a, b`; an item without a verb continues the last one.
    #[derive(Clone, Copy, PartialEq)]
    enum Verb {
        Add,
        DropColumn,
        Other,
    }
    let mut last = Verb::Other;
    let verbs = [
        "ADD", "DROP", "ALTER", "ENABLE", "DISABLE", "SET", "RENAME", "WITH", "NOCHECK", "OWNER",
        "VALIDATE",
    ];
    for action in split_commas(&t[j..]) {
        let Some(verb) = action.first() else { continue };
        let (kind, mut k) = if verb.is_kw("ADD") {
            (Verb::Add, 1)
        } else if verb.is_kw("DROP") {
            let column = action.get(1).is_some_and(|x| x.is_kw("COLUMN"))
                || (dialect != Dialect::Tsql
                    && action.get(1).is_some_and(|x| {
                        x.is_name()
                            && !x.is_any_kw(&[
                                "CONSTRAINT",
                                "PRIMARY",
                                "UNIQUE",
                                "FOREIGN",
                                "CHECK",
                                "INDEX",
                                "PERIOD",
                            ])
                    }));
            (
                if column {
                    Verb::DropColumn
                } else {
                    Verb::Other
                },
                1,
            )
        } else if verb.is_any_kw(&verbs) {
            (Verb::Other, 1)
        } else {
            (last, 0)
        };
        last = kind;
        match kind {
            Verb::Add => {
                if action.get(k).is_some_and(|x| x.is_kw("COLUMN")) {
                    k += 1;
                }
                if action.get(k).is_some_and(|x| x.is_kw("IF")) {
                    k += 3; // IF NOT EXISTS
                }
                let item = &action[k.min(action.len())..];
                if is_constraint(item, dialect) {
                    table_constraint(tb, item, dialect);
                } else {
                    column_defs(tb, item, dialect);
                }
            }
            Verb::DropColumn => {
                if action.get(k).is_some_and(|x| x.is_kw("COLUMN")) {
                    k += 1;
                }
                if action.get(k).is_some_and(|x| x.is_kw("IF")) {
                    k += 2;
                }
                if let Some(c) = action.get(k).filter(|x| x.is_name()) {
                    let ck = c.ident().key(dialect);
                    tb.columns.retain(|x| super::key(dialect, &x.name) != ck);
                }
            }
            Verb::Other => {}
        }
    }
}

/// One output column of a query's select list.
#[derive(Debug, Clone)]
pub struct OutputCol {
    pub name: Option<Ident>,
    /// The item is a plain column reference (`a`, `t.a`, `s.t.a`).
    pub source: Option<Vec<Ident>>,
}

/// Output columns of a query starting at its `SELECT` (or `WITH`); `None`
/// when they cannot be listed (`*`, no select list).
pub fn select_outputs(t: &[Tok]) -> Option<Vec<OutputCol>> {
    let mut depth = 0usize;
    let sel = t.iter().enumerate().find_map(|(i, x)| {
        match x.kind {
            SyntaxKind::L_PAREN => depth += 1,
            SyntaxKind::R_PAREN => depth = depth.saturating_sub(1),
            _ if depth == 0 && x.is_kw("SELECT") => return Some(i),
            _ => {}
        }
        None
    })?;
    let mut i = sel + 1;
    while let Some(x) = t.get(i) {
        if x.is_any_kw(&["DISTINCT", "ALL"]) {
            i += 1;
        } else if x.is_kw("ON") && t.get(i + 1).is_some_and(|n| n.is(SyntaxKind::L_PAREN)) {
            i = close_paren(t, i + 1) + 1; // DISTINCT ON (...)
        } else if x.is_kw("TOP") {
            i += 1;
            if t.get(i).is_some_and(|n| n.is(SyntaxKind::L_PAREN)) {
                i = close_paren(t, i) + 1;
            } else {
                i += 1;
            }
            if t.get(i).is_some_and(|n| n.is_kw("PERCENT")) {
                i += 1;
            }
            if t.get(i).is_some_and(|n| n.is_kw("WITH")) {
                i += 2;
            }
        } else {
            break;
        }
    }
    const ENDS: &[&str] = &[
        "FROM",
        "INTO",
        "WHERE",
        "GROUP",
        "HAVING",
        "ORDER",
        "UNION",
        "EXCEPT",
        "INTERSECT",
        "LIMIT",
        "OFFSET",
        "FETCH",
        "FOR",
        "WINDOW",
    ];
    let mut end = i;
    let mut depth = 0usize;
    while end < t.len() {
        let x = &t[end];
        match x.kind {
            SyntaxKind::L_PAREN => depth += 1,
            SyntaxKind::R_PAREN => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            SyntaxKind::SEMICOLON if depth == 0 => break,
            _ if depth == 0 && x.is_any_kw(ENDS) => break,
            _ => {}
        }
        end += 1;
    }
    let mut out = Vec::new();
    for item in split_commas(&t[i..end]) {
        if item.is_empty() {
            continue;
        }
        if item.last().is_some_and(|x| x.is_op("*")) {
            return None;
        }
        // T-SQL `alias = expr`.
        if item.len() >= 3 && item[0].is_name() && item[1].is_op("=") {
            out.push(OutputCol {
                name: Some(item[0].ident()),
                source: None,
            });
            continue;
        }
        let (parts, after) = qualified(item, 0);
        let plain = !parts.is_empty() && after == item.len();
        let n = item.len();
        // `col AS x` and `col x` still copy the column's type.
        let aliased = !parts.is_empty()
            && ((after + 2 == n && item[after].is_kw("AS")) || (after + 1 == n && after > 0))
            && item[n - 1].is_name();
        let source = (plain || aliased).then(|| parts.clone());
        let name = if n >= 2 && item[n - 2].is_kw("AS") && item[n - 1].is_name() {
            Some(item[n - 1].ident())
        } else if plain {
            parts.last().cloned()
        } else if n >= 2
            && item[n - 1].is_name()
            && !item[n - 1].is_any_kw(&["END", "NULL", "TRUE", "FALSE"])
            && matches!(
                item[n - 2].kind,
                SyntaxKind::IDENT
                    | SyntaxKind::QUOTED_IDENT
                    | SyntaxKind::R_PAREN
                    | SyntaxKind::NUMBER
                    | SyntaxKind::STRING
                    | SyntaxKind::VARIABLE
            )
        {
            Some(item[n - 1].ident())
        } else {
            None
        };
        out.push(OutputCol { name, source });
    }
    Some(out)
}

/// A view as a table: output names, with types copied from plain column
/// references to tables already in the catalog.
fn view_table(
    cat: &Catalog,
    dialect: Dialect,
    name: Vec<Ident>,
    cols: Option<Vec<Ident>>,
    query: &[Tok],
) -> Table {
    let (schema, vname) = stored_name(&name, dialect).unwrap_or((None, String::new()));
    let mut tb = Table::new(schema, vname, TableKind::View);
    let outputs = select_outputs(query);
    // Tables in the top-level FROM, for typing plain column references.
    let mut from_tables: Vec<(Option<Ident>, &Table)> = Vec::new();
    if let Some(f) = find_kw(query, 0, "FROM") {
        let end = ["WHERE", "GROUP", "HAVING", "ORDER", "UNION"]
            .iter()
            .filter_map(|k| find_kw(query, f, k))
            .min()
            .unwrap_or(query.len());
        for item in split_commas(&query[f + 1..end]) {
            let (parts, after) = qualified(item, 0);
            if let Some(t) = super::table_position(dialect, cat, &parts).map(|i| &cat.tables[i]) {
                let alias = item
                    .get(after)
                    .filter(|x| x.is_kw("AS"))
                    .map(|_| after + 1)
                    .or(Some(after))
                    .and_then(|k| item.get(k))
                    .filter(|x| x.is_name() && !x.is_any_kw(&["JOIN", "INNER", "LEFT", "WITH"]))
                    .map(Tok::ident);
                from_tables.push((alias, t));
            }
        }
    }
    // NOT NULL survives only a plain inner query: outer joins, set operations
    // and grouping extensions can produce NULL in any column.
    let keeps_not_null = !query.iter().any(|x| {
        x.is_any_kw(&[
            "LEFT",
            "RIGHT",
            "FULL",
            "OUTER",
            "APPLY",
            "UNION",
            "EXCEPT",
            "INTERSECT",
            "MINUS",
            "ROLLUP",
            "CUBE",
            "GROUPING",
        ])
    });
    match (outputs, cols) {
        (Some(outs), explicit) => {
            for (k, o) in outs.iter().enumerate() {
                let Some(n) = explicit
                    .as_ref()
                    .and_then(|c| c.get(k).cloned())
                    .or_else(|| o.name.clone())
                else {
                    continue;
                };
                let src = o.source.as_ref().and_then(|parts| {
                    let col = parts.last()?;
                    from_tables
                        .iter()
                        .filter(|(alias, t)| match parts.len() {
                            1 => true,
                            _ => {
                                let q = &parts[parts.len() - 2];
                                alias
                                    .as_ref()
                                    .is_some_and(|a| a.key(dialect) == q.key(dialect))
                                    || super::key(dialect, &t.name) == q.key(dialect)
                            }
                        })
                        .find_map(|(_, t)| t.column(dialect, col))
                });
                tb.columns.push(Column {
                    name: n.stored(dialect),
                    data_type: src
                        .map(|c| c.data_type.clone())
                        .unwrap_or_else(DataType::unknown),
                    nullable: !keeps_not_null || src.map(|c| c.nullable).unwrap_or(true),
                    identity: false,
                    has_default: false,
                    generated: false,
                    collation: src.and_then(|c| c.collation.clone()),
                });
            }
        }
        (None, Some(explicit)) => {
            for n in explicit {
                tb.columns.push(Column {
                    name: n.stored(dialect),
                    data_type: DataType::unknown(),
                    nullable: true,
                    identity: false,
                    has_default: false,
                    generated: false,
                    collation: None,
                });
            }
        }
        (None, None) => tb.columns_known = false,
    }
    tb
}

/// The header of a routine definition.
#[derive(Debug, Clone)]
pub struct RoutineHeader {
    pub name: Vec<Ident>,
    pub routine: Routine,
    /// Byte range of each parameter's name, parallel to `routine.params`.
    pub param_spans: Vec<(usize, usize)>,
    pub trigger: Option<TriggerInfo>,
    /// `Some` for a T-SQL view (`CREATE VIEW` parses as a routine): explicit column names.
    pub view: Option<Option<Vec<Ident>>>,
}

#[derive(Debug, Clone, Default)]
pub struct TriggerInfo {
    pub table: Vec<Ident>,
    /// Db2 `REFERENCING NEW [ROW] AS n`: (`NEW`/`OLD`/`NEW TABLE`/`OLD TABLE`, alias).
    pub referencing: Vec<(String, Ident)>,
}

const PG_ROUTINE_OPTIONS: &[&str] = &[
    "LANGUAGE",
    "AS",
    "IMMUTABLE",
    "STABLE",
    "VOLATILE",
    "STRICT",
    "CALLED",
    "SECURITY",
    "SET",
    "COST",
    "ROWS",
    "PARALLEL",
    "LEAKPROOF",
    "WINDOW",
    "BEGIN",
    "RETURN",
    "TRANSFORM",
    "SUPPORT",
    "NOT",
    "EXTERNAL",
    "DETERMINISTIC",
    "READS",
    "MODIFIES",
    "CONTAINS",
    "NO",
    "SPECIFIC",
    "RESULT",
    "DYNAMIC",
    "PROGRAM",
    "FENCED",
    "NOT",
    "WITH",
    "INHERIT",
    "PARAMETER",
    "NEW",
    "OLD",
    "ALLOW",
    "DISALLOW",
    "CARDINALITY",
    "FINAL",
    "SCRATCHPAD",
    "DBINFO",
    "CONCURRENT",
];

/// Reads the name, parameters and return type of a ROUTINE node.
pub fn routine_header(node: &SyntaxNode, dialect: Dialect) -> Option<RoutineHeader> {
    let t = tokens::header(node);
    let kw = t
        .iter()
        .position(|x| x.is_any_kw(&["PROCEDURE", "PROC", "FUNCTION", "TRIGGER", "VIEW"]))?;
    let kind_word = t[kw].upper();
    let (name, mut i) = qualified(&t, kw + 1);
    let (schema, rname) = stored_name(&name, dialect)?;
    let kind = match kind_word.as_str() {
        "FUNCTION" => RoutineKind::Function,
        "TRIGGER" => RoutineKind::Trigger,
        _ => RoutineKind::Procedure,
    };
    let mut h = RoutineHeader {
        name: name.clone(),
        routine: Routine {
            schema,
            name: rname,
            kind,
            params: Vec::new(),
            returns: None,
        },
        param_spans: Vec::new(),
        trigger: None,
        view: None,
    };
    if kind_word == "VIEW" {
        let cols = t.get(i).filter(|x| x.is(SyntaxKind::L_PAREN)).map(|_| {
            let c = close_paren(&t, i);
            split_commas(&t[i + 1..c])
                .iter()
                .filter_map(|e| e.first().map(Tok::ident))
                .collect()
        });
        h.view = Some(cols);
        return Some(h);
    }
    if kind == RoutineKind::Trigger {
        h.trigger = Some(trigger_info(&t, i));
        return Some(h);
    }
    // T-SQL `;n` procedure numbers.
    if t.get(i).is_some_and(|x| x.is(SyntaxKind::SEMICOLON)) {
        i += 2;
    }
    let params: &[Tok] = if t.get(i).is_some_and(|x| x.is(SyntaxKind::L_PAREN)) {
        let c = close_paren(&t, i);
        let p = &t[i + 1..c];
        i = c + 1;
        p
    } else if dialect == Dialect::Tsql {
        // Unparenthesised T-SQL procedure parameters run up to WITH / FOR / AS.
        let end = (i..t.len())
            .find(|&k| {
                t[k].is_any_kw(&["AS", "WITH", "FOR", "RETURNS"])
                    && t.get(k.wrapping_sub(1))
                        .map_or(true, |p| !p.is(SyntaxKind::VARIABLE) || t[k].is_kw("WITH"))
            })
            .unwrap_or(t.len());
        let p = &t[i..end];
        i = end;
        p
    } else {
        &[]
    };
    for el in split_commas(params) {
        if let Some((p, span)) = param(el, dialect, kind) {
            h.routine.params.push(p);
            h.param_spans.push(span);
        }
    }
    if let Some(r) = find_kw(&t, i, "RETURNS") {
        let mut k = r + 1;
        if t.get(k).is_some_and(|x| x.is(SyntaxKind::VARIABLE)) {
            k += 1; // RETURNS @t TABLE (...)
        }
        if t.get(k).is_some_and(|x| x.is_kw("SETOF")) {
            k += 1;
        }
        let end = (k..t.len())
            .find(|&e| {
                (e > k && t[e].is_any_kw(PG_ROUTINE_OPTIONS))
                    || t[e].kind == SyntaxKind::DOLLAR_STRING
                    || t[e].is(SyntaxKind::SEMICOLON)
            })
            .unwrap_or(t.len());
        let end = type_end(&t[..end], k).max(k + 1).min(t.len());
        h.routine.returns = Some(DataType::parse(&tokens::texts(&t[k..end])));
    }
    Some(h)
}

fn param(el: &[Tok], dialect: Dialect, kind: RoutineKind) -> Option<(Param, (usize, usize))> {
    let mut k = 0;
    let mut mode = ParamMode::In;
    if let Some(m) = el.first().filter(|x| x.kind == SyntaxKind::IDENT) {
        let explicit = match m.upper().as_str() {
            "IN" => Some(ParamMode::In),
            "OUT" => Some(ParamMode::Out),
            "INOUT" => Some(ParamMode::InOut),
            "VARIADIC" => Some(ParamMode::Variadic),
            _ => None,
        };
        if let Some(md) = explicit {
            // `IN OUT` is a PostgreSQL synonym of INOUT.
            mode = md;
            k = 1;
            if md == ParamMode::In && el.get(1).is_some_and(|x| x.is_kw("OUT")) {
                mode = ParamMode::InOut;
                k = 2;
            }
        }
    }
    let rest = &el[k..];
    let first = rest.first()?;
    // Default: `DEFAULT expr` or `= expr`.
    let def_at = rest.iter().position(|x| x.is_kw("DEFAULT") || x.is_op("="));
    let body = &rest[..def_at.unwrap_or(rest.len())];
    let has_default = def_at.is_some();
    // The default's text, without a trailing T-SQL OUTPUT or READONLY.
    let default = def_at.map(|at| {
        let mut end = rest.len();
        while end > at + 1 && rest[end - 1].is_any_kw(&["OUTPUT", "OUT", "READONLY"]) {
            end -= 1;
        }
        tokens::texts(&rest[at + 1..end]).join(" ")
    });
    let named = match dialect {
        Dialect::Tsql => first.kind == SyntaxKind::VARIABLE,
        _ => first.kind == SyntaxKind::QUOTED_IDENT || (body.len() >= 2 && !is_type_start(body)),
    };
    let (name, span, ty_from) = if named {
        let n = match first.kind {
            SyntaxKind::VARIABLE => first.text.clone(),
            _ => first.ident().stored(dialect),
        };
        (n, (first.start, first.end), 1)
    } else {
        (String::new(), (first.start, first.start), 0)
    };
    let mut ty_from = ty_from;
    if body.get(ty_from).is_some_and(|x| x.is_kw("AS")) {
        ty_from += 1;
    }
    let ty_end = type_end(body, ty_from);
    let data_type = DataType::parse(&tokens::texts(&body[ty_from.min(body.len())..ty_end]));
    // T-SQL `@p int OUTPUT` / `OUT`.
    if dialect == Dialect::Tsql
        && rest.iter().any(|x| x.is_any_kw(&["OUTPUT", "OUT"]))
        && kind == RoutineKind::Procedure
    {
        mode = ParamMode::InOut;
    }
    Some((
        Param {
            name,
            data_type,
            mode,
            has_default,
            default,
        },
        span,
    ))
}

/// Whether a parameter element starts with a type rather than a name
/// (PostgreSQL allows unnamed parameters).
fn is_type_start(body: &[Tok]) -> bool {
    let Some(second) = body.get(1) else {
        return true;
    };
    let multi = second.is_any_kw(&["VARYING", "PRECISION", "WITH", "WITHOUT", "LARGE"])
        || second.is(SyntaxKind::L_PAREN)
        || second.is(SyntaxKind::L_BRACKET);
    multi
        && body[0].is_any_kw(&[
            "CHARACTER",
            "CHAR",
            "DOUBLE",
            "TIMESTAMP",
            "TIME",
            "BIT",
            "NATIONAL",
            "VARCHAR",
            "NUMERIC",
            "DECIMAL",
            "INTERVAL",
            "BINARY",
            "INTEGER",
            "INT",
            "TEXT",
            "BIGINT",
        ])
}

/// PostgreSQL `CREATE [OR REPLACE] [CONSTRAINT] TRIGGER … ON table …
/// [REFERENCING …] … EXECUTE {FUNCTION|PROCEDURE} f(…)`: the function's
/// name parts and the trigger's table and transition tables.
pub fn pg_trigger_statement(t: &[Tok]) -> Option<(Vec<Ident>, TriggerInfo)> {
    let kw = t.iter().position(|x| x.is_kw("TRIGGER"))?;
    if !t[..kw]
        .iter()
        .all(|x| x.is_any_kw(&["CREATE", "OR", "REPLACE", "CONSTRAINT"]))
        || !t.first()?.is_kw("CREATE")
    {
        return None;
    }
    let ex = (kw..t.len()).find(|&i| {
        t[i].is_kw("EXECUTE")
            && t.get(i + 1)
                .is_some_and(|x| x.is_any_kw(&["FUNCTION", "PROCEDURE"]))
    })?;
    let (name, _) = qualified(t, ex + 2);
    if name.is_empty() {
        return None;
    }
    Some((name, trigger_info(&t[..ex], kw)))
}

fn trigger_info(t: &[Tok], from: usize) -> TriggerInfo {
    let mut info = TriggerInfo::default();
    if let Some(on) = find_kw(t, from, "ON") {
        info.table = qualified(t, on + 1).0;
    }
    if let Some(r) = find_kw(t, from, "REFERENCING") {
        let mut k = r + 1;
        while let Some(x) = t.get(k) {
            if !x.is_any_kw(&["NEW", "OLD", "NEW_TABLE", "OLD_TABLE"]) {
                break;
            }
            let mut which = x.upper();
            k += 1;
            if t.get(k).is_some_and(|y| y.is_any_kw(&["ROW", "TABLE"])) {
                if t[k].is_kw("TABLE") {
                    which.push_str(" TABLE");
                }
                k += 1;
            }
            if t.get(k).is_some_and(|y| y.is_kw("AS")) {
                k += 1;
            }
            if let Some(alias) = t.get(k).filter(|y| y.is_name()) {
                info.referencing.push((which, alias.ident()));
                k += 1;
            }
        }
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{CatalogView, Lookup};

    fn corpus(name: &str) -> String {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../corpus/catalog/");
        std::fs::read_to_string(format!("{p}{name}")).unwrap()
    }

    #[test]
    fn corpus_catalogs_load_in_every_dialect() {
        for (file, d) in [
            ("tsql.sql", Dialect::Tsql),
            ("pg.sql", Dialect::Postgres),
            ("db2i.sql", Dialect::Db2i),
        ] {
            let c = load(&corpus(file), d, file);
            assert_eq!(c.tables.len(), 10, "{file}: {:?}", c.tables);
            let v = CatalogView::new(d, &[&c]);
            let Lookup::Found(cust) = v.table(&[Ident::plain("customer")]) else {
                panic!("{file}: customer")
            };
            let first = cust.column(d, &Ident::plain("first_name")).unwrap();
            assert_eq!(first.data_type.char_length(), Some(50), "{file}");
            assert!(first.nullable);
            let email = cust.column(d, &Ident::plain("email")).unwrap();
            assert!(!email.nullable);
            assert!(
                cust.is_unique_on(d, &[&Ident::plain("email").stored(d)]),
                "{file}"
            );
            let Lookup::Found(log) = v.table(&[Ident::plain("stock_log")]) else {
                panic!()
            };
            assert!(log.column(d, &Ident::plain("log_id")).unwrap().identity);
            assert!(
                log.column(d, &Ident::plain("logged_at"))
                    .unwrap()
                    .has_default
            );
            let Lookup::Found(ol) = v.table(&[Ident::plain("order_line")]) else {
                panic!()
            };
            assert!(ol.keys.iter().any(|k| k.primary && k.columns.len() == 2));
            let Lookup::Found(r) = v.routines(&[Ident::plain("log_stock_change")]) else {
                panic!("{file}: routine")
            };
            assert_eq!(r[0].params.len(), 3, "{file}: {:?}", r[0]);
            assert!(r[0].params.iter().all(|p| !p.has_default));
        }
    }

    #[test]
    fn tsql_signature_details() {
        let c = load(
            "CREATE PROCEDURE dbo.p @a int, @b varchar(10) = 'x', @c int OUTPUT AS SELECT 1\nGO\nCREATE FUNCTION dbo.f(@x int) RETURNS varchar(20) AS BEGIN RETURN 'a' END",
            Dialect::Tsql,
            "t",
        );
        let p = &c.routines[0];
        assert_eq!(p.params.len(), 3);
        assert!(p.params[1].has_default);
        assert_eq!(p.params[2].mode, ParamMode::InOut);
        let f = &c.routines[1];
        assert_eq!(f.kind, RoutineKind::Function);
        assert_eq!(f.returns.as_ref().unwrap().char_length(), Some(20));
    }

    #[test]
    fn pg_signature_details() {
        let c = load(
            "CREATE FUNCTION f(integer, b text DEFAULT 'x', OUT c int) RETURNS SETOF record LANGUAGE sql AS $$ SELECT 1 $$;",
            Dialect::Postgres,
            "t",
        );
        let f = &c.routines[0];
        assert_eq!(f.params.len(), 3);
        assert_eq!(f.params[0].name, "");
        assert_eq!(f.params[0].data_type.base, "INTEGER");
        assert!(f.params[1].has_default);
        assert_eq!(f.call_params(Dialect::Postgres).len(), 2);
    }

    #[test]
    fn alter_and_views() {
        let c = load(
            "CREATE TABLE t (a int, b varchar(5));\nALTER TABLE t DROP COLUMN b;\nALTER TABLE t ADD COLUMN c date;\nCREATE VIEW v AS SELECT a, c AS d FROM t;",
            Dialect::Postgres,
            "t",
        );
        let v = CatalogView::new(Dialect::Postgres, &[&c]);
        let Lookup::Found(t) = v.table(&[Ident::plain("t")]) else {
            panic!()
        };
        let names: Vec<_> = t.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["a", "c"]);
        let Lookup::Found(vw) = v.table(&[Ident::plain("v")]) else {
            panic!()
        };
        assert_eq!(vw.kind, TableKind::View);
        assert_eq!(vw.columns[1].name, "d");
        assert_eq!(vw.columns[1].data_type.base, "DATE");
    }

    fn columns(c: &Catalog, d: Dialect, t: &str) -> Vec<String> {
        let v = CatalogView::new(d, &[c]);
        let Lookup::Found(t) = v.table(&[Ident::plain(t)]) else {
            panic!("{t} missing")
        };
        t.columns.iter().map(|c| c.name.clone()).collect()
    }

    #[test]
    fn tsql_alter_lists() {
        let c = load(
            "CREATE TABLE t (a int, b int, c int);\nALTER TABLE t ADD x int, y varchar(5) NULL, CONSTRAINT pk PRIMARY KEY (x);\nALTER TABLE t DROP COLUMN a, b;",
            Dialect::Tsql,
            "t",
        );
        assert_eq!(columns(&c, Dialect::Tsql, "t"), ["c", "x", "y"]);
    }

    #[test]
    fn keyword_column_names() {
        let c = load(
            "CREATE TABLE t (period int, index varchar(10), check date, unique_code int, like text, PRIMARY KEY (period), CHECK (period > 0));",
            Dialect::Postgres,
            "t",
        );
        assert_eq!(
            columns(&c, Dialect::Postgres, "t"),
            ["period", "index", "check", "unique_code", "like"]
        );
        let c = load(
            "CREATE TABLE t (id int, INDEX ix_id (id), period int);",
            Dialect::Tsql,
            "t",
        );
        assert_eq!(columns(&c, Dialect::Tsql, "t"), ["id", "period"]);
    }
}
