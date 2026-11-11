//! The inventory of catalog read sites (performance plan W4.3, PR 1).
//!
//! `docs/catalog-read-sites.md` lists every place the check reads a catalog
//! and the [`Query`](sqldiff_catalog::record::Query) it becomes when the
//! reads are recorded (PRs 2a and 2b). It is generated from this file and
//! checked against it:
//!
//! - the **door counts** are scanned from the non-test sources of the check
//!   crates; a new read of a catalog changes a count, fails this test, and
//!   has to be added to the table below with its query before it can land;
//! - every **site** below names an anchor (a function or a line of code) that
//!   must still exist in its file, so the table cannot drift from the code;
//! - the public methods of `CatalogView` and `Schema` are listed with what
//!   they become, and a method added to either fails the test.
//!
//! `SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test catalog_sites`
//! regenerates the document.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use regex::Regex;

fn crates_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

/// The text without `#[cfg(test)] mod name { ... }` blocks.
fn without_test_modules(text: &str) -> String {
    let start =
        Regex::new(r"(?m)^[ \t]*#\[cfg\(test\)\]\s*(pub(\(crate\))?\s+)?mod\s+\w+\s*\{").unwrap();
    let mut out = String::new();
    let mut at = 0;
    while let Some(m) = start.find_at(text, at) {
        out.push_str(&text[at..m.start()]);
        at = m.end() + block_end(&text[m.end()..]);
    }
    out.push_str(&text[at..]);
    out
}

/// The length of a Rust block's body up to and including the `}` that closes
/// it; braces in strings, character literals and comments do not count.
fn block_end(s: &str) -> usize {
    let b = s.as_bytes();
    let mut depth = 1usize;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            b'r' if matches!(b.get(i + 1), Some(b'#' | b'"'))
                && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) =>
            {
                let mut j = i + 1;
                while b.get(j) == Some(&b'#') {
                    j += 1;
                }
                if b.get(j) == Some(&b'"') {
                    let close = format!("\"{}", "#".repeat(j - i - 1));
                    let rest = &s[j + 1..];
                    i = j + 1 + rest.find(&close).expect("closed raw string") + close.len() - 1;
                }
            }
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'\'' => {
                if b.get(i + 1) == Some(&b'\\') {
                    if let Some(k) = s[i + 2..].find('\'') {
                        i += 2 + k;
                    }
                } else if b.get(i + 2) == Some(&b'\'') {
                    i += 2;
                } else if let Some(c) = s[i + 1..].chars().next() {
                    let n = c.len_utf8();
                    if b.get(i + 1 + n) == Some(&b'\'') {
                        i += 1 + n;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    panic!("unbalanced test module");
}

/// Comments cut from the text, so prose does not count as a read.
fn without_comments(text: &str) -> String {
    text.lines()
        .map(|l| match l.find("//") {
            Some(i) if !l[..i].contains('"') => &l[..i],
            _ => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Where the check reads: the crates of the check stage and of pass 1.
const CRATES: &[(&str, &str)] = &[
    ("sqldiff-core/src", "check"),
    ("sqldiff-equiv/src", "check"),
    ("sqldiff-model/src", "check"),
    ("sqldiff-draft/src", "pass 1"),
    ("sqldiff-convert/src", "driver"),
];

/// A kind of door: how the check can reach a catalog.
const DOORS: &[(&str, &str, &str)] = &[
    (
        "catalog accessor",
        r"\.catalog\(\)",
        "`Schema::catalog`: the side's `RecordingView`. `CatalogView::catalog` is gone (PR 2a): a `RecordingView` has no accessor for the catalog behind it.",
    ),
    (
        "unrecorded catalog",
        r"\.unrecorded_catalog\(\)",
        "`CatalogView::unrecorded_catalog`: engine-side only. `Schema::build` and the static `include_str!` schemas use it (no sink); the scan below fails on any other file.",
    ),
    (
        "whole catalog",
        r"\.whole\(\)",
        "`RecordingView::whole`: records `WholeCatalog`. Sound (any edit of the side re-checks the file) and the price of a read with no narrow query yet: call.rs comments (A6), the trigger scans (A7).",
    ),
    (
        "authoritative",
        r"\b(tables|routines)_authoritative\b",
        "`TablesAuthoritative`, `RoutinesAuthoritative(kind)`.",
    ),
    (
        "supplied / origins",
        r"\.supplied\(\)|\.origins\(\)|\.origin\(\)",
        "`Supplied`, `Origin`. The fields are private since PR 2a; these are the accessors.",
    ),
    (
        "view lookups",
        r"\.(table|routines)\(",
        "`Table(parts)`, `Routines(parts)` on a view; on a `Schema`, `SchemaTables(name)` (see the sites). Counts include both.",
    ),
    (
        "schema tables",
        r"\.(tables|table_exact|with_images|with_ddl|with_local)\(",
        "The equivalence schema's table set: `SchemaTables`, `ColumnNamed`, `ViewFamily`, `HiddenTables` (PR 2b).",
    ),
    (
        "options catalogs",
        r"\b(facts|catalog)_[ab]\b",
        "The `Options` door: becomes an opaque `Facts` handle only the engine opens (PR 2a).",
    ),
    (
        "triggers",
        r"\.triggers\b|\btriggers_named\b",
        "`TriggersNamed`, `TriggersOn`, `TriggersRunning`.",
    ),
    (
        "schema fields",
        r"\.(star_views|list_stars|is_star_view)\b",
        "`StarView(lower name)`. `Schema::star_views` and `list_stars` are private; a read is a read of the catalog.",
    ),
    (
        "pass 1 catalog",
        r"\bself\.catalog\b|\btype_catalog\(\)|\btable_position\(|\b(cat|c)\.(tables|routines)\b",
        "Pass 1 (`sqldiff-draft`) holds the source catalog directly. Counted in `sqldiff-draft/src` only. Sandbox-only until Phase B: `SourceReads`.",
    ),
    (
        "view type",
        r"\bCatalogView\b",
        "Where a bare `CatalogView` is held. Check crates hold a `RecordingView` or a `LocalView` after PR 2a.",
    ),
];

/// Whether a door counts in a file. The pass 1 door is about `sqldiff-draft`
/// only; its pattern is too common elsewhere.
fn door_applies(name: &str, rel: &str) -> bool {
    name != "pass 1 catalog" || rel.starts_with("sqldiff-draft/src")
}

/// One read site: where it is, what it reads, the query it becomes.
struct Site {
    id: &'static str,
    file: &'static str,
    anchor: &'static str,
    query: &'static str,
    note: &'static str,
}

const SITES: &[Site] = &[
    Site {
        id: "B1",
        file: "sqldiff-core/src/bind/query.rs",
        anchor: "fn table_ref(",
        query: "`Table(parts)`, `TablesAuthoritative`, `Origin`",
        note: "Table lookup with `NoSchema`/`NotFound` outcomes; the reason text formats the origin.",
    },
    Site {
        id: "B2",
        file: "sqldiff-core/src/bind/query.rs",
        anchor: "fn unknown_column_in(",
        query: "`Origin`",
        note: "Reason text.",
    },
    Site {
        id: "B3",
        file: "sqldiff-core/src/bind/query.rs",
        anchor: "fn resolve_call(",
        query: "`Routines(parts)`, `RoutinesAuthoritative(kind)`, `Origin`",
        note: "Overloads in catalog order; the file-local routines are a `LocalView`, not a read.",
    },
    Site {
        id: "B4",
        file: "sqldiff-core/src/bind/query.rs",
        anchor: "fn unresolved_bare(",
        query: "`Origin`",
        note: "Reason text.",
    },
    Site {
        id: "B5",
        file: "sqldiff-core/src/bind/query.rs",
        anchor: "fn pseudo_sources(",
        query: "`Table(parts)`",
        note: "The table a trigger fires on, for `inserted` and `deleted`.",
    },
    Site {
        id: "B6",
        file: "sqldiff-core/src/bind/query.rs",
        anchor: "fn lookup_table_silent(",
        query: "`Table(parts)`",
        note: "`self.input.view.table`: the same lookup as `table_ref`, without the reason text.",
    },
    Site {
        id: "B7",
        file: "sqldiff-core/src/bind/mod.rs",
        anchor: "fn lookup_table_quiet(",
        query: "`Table(parts)`",
        note: "`view.table(parts)` for a caller outside the query binder.",
    },
    Site {
        id: "S1",
        file: "sqldiff-equiv/src/equiv/canon.rs",
        anchor: "fn visible_star(",
        query: "`StarView(lower name)`, `HiddenTables` (the `list_stars` flag)",
        note: "Reads through `Schema::is_star_view` and `Schema::lists_stars`; the fields are private.",
    },
    Site {
        id: "E1",
        file: "sqldiff-core/src/engine.rs",
        anchor: "read_a.supplied()",
        query: "`Supplied` per side",
        note: "A8: field reads. Today the key tells `none` from a catalog; once digests leave the key, `supplied` stays in it.",
    },
    Site {
        id: "A1",
        file: "sqldiff-equiv/src/equiv/schema.rs",
        anchor: "pub fn hides_columns(",
        query: "`HiddenTables` per side, plus `SchemaTables` for each counterpart",
        note: "Runs for every file (`list_stars_with`, `mirrored`). If this falls back to `WholeCatalog`, the series buys nothing.",
    },
    Site {
        id: "A2",
        file: "sqldiff-equiv/src/equiv/edit.rs",
        anchor: "fn typed_class(",
        query: "`ColumnNamed(name)`",
        note: "Scans `schema.tables()` (deduplicated set) for a column of that name.",
    },
    Site {
        id: "A3",
        file: "sqldiff-equiv/src/equiv/schema.rs",
        anchor: "pub fn table_exact(",
        query: "`SchemaTables(lower name)`",
        note: "`Schema::table`, `table_exact`, `with_ddl`, `with_images`: base and temporary tables by `to_lowercase` unqualified name, last one wins, `ambiguous` across schemas. Not `Ident::key`.",
    },
    Site {
        id: "A4",
        file: "sqldiff-equiv/src/equiv/schema.rs",
        anchor: "types_from_source && !t.star_outputs",
        query: "`ViewFamily(name)`",
        note: "Views typed from a source: every non-view table of the source's name under `eq_ignore_ascii_case`, and their columns; two views of a name give an empty map. A flag digest would be too coarse.",
    },
    Site {
        id: "A5",
        file: "sqldiff-equiv/src/equiv/schema.rs",
        anchor: "if d == Dialect::Postgres {",
        query: "`Spellings(lower name)`",
        note: "PostgreSQL spellings over schemas, routine schemas and names, table schemas and names, columns, keys and indexes: seven sources.",
    },
    Site {
        id: "A6",
        file: "sqldiff-equiv/src/equiv/call.rs",
        anchor: "fn exists(target: &Target, view: RecordingView",
        query: "`SchemaKnown(part)`, `Table(parts)`, `Routines(parts)`, and the scan at the end",
        note: "Comment targets. The `tables.iter().any(..)` scan is `RecordingView::whole` (`WholeCatalog`) in PR 2a.",
    },
    Site {
        id: "C1",
        file: "sqldiff-equiv/src/equiv/call.rs",
        anchor: "fn switches(",
        query: "`Table(parts)` on both sides",
        note: "Versioning declared on the table and its history table on the other side.",
    },
    Site {
        id: "C2",
        file: "sqldiff-equiv/src/equiv/call.rs",
        anchor: "fn comments(",
        query: "`Table(parts)`, `Routines(parts)`",
        note: "COMMENT ON targets.",
    },
    Site {
        id: "C3",
        file: "sqldiff-equiv/src/equiv/call.rs",
        anchor: "fn callee<'v>(",
        query: "`Routines(parts)`",
        note: "The routine a CALL names.",
    },
    Site {
        id: "K1",
        file: "sqldiff-equiv/src/equiv/canon.rs",
        anchor: "fn user_fn(",
        query: "`Routines(parts)`",
        note: "A user function in an expression.",
    },
    Site {
        id: "K2",
        file: "sqldiff-equiv/src/equiv/canon.rs",
        anchor: "fn schema_unindexed(",
        query: "`Table(parts)`",
        note: "Whether a column set is covered by an index.",
    },
    Site {
        id: "A7",
        file: "sqldiff-equiv/src/equiv/mod.rs",
        anchor: "fn trigger_drop(",
        query: "`TriggersNamed`, `TriggersOn`, `TriggersRunning`",
        note: "Triggers by name; PostgreSQL triggers with a name on a table; any other trigger that runs the function.",
    },
    Site {
        id: "A8",
        file: "sqldiff-equiv/src/equiv/schema.rs",
        anchor: "sources: view.origins().to_vec()",
        query: "`Origin`, `Supplied`",
        note: "`Schema::catalog()` is `None` when nothing was supplied; origins reach witnesses.",
    },
    Site {
        id: "A9",
        file: "sqldiff-convert/src/check.rs",
        anchor: "fn check_with_mix(",
        query: "the union of the reads of every `check_one` inside, the losing variants included",
        note: "The result is the best of two or three checks. A catalog edit that flips which variant wins must not replay clean. The pass-1 reads inside `rebuild` are covered by rebuilding the mix against the current catalog on reuse.",
    },
    Site {
        id: "P1",
        file: "sqldiff-draft/src/tsql.rs",
        anchor: "catalog: Option<SourceReads<'a>>,",
        query: "pass 1 `SourceReads` (sandbox only until Phase B)",
        note: "`Cx` holds the whole-tree source catalog as `SourceReads`, the only way in (PR 2a, compiler-enforced).",
    },
    Site {
        id: "P3",
        file: "sqldiff-draft/src/tsql.rs",
        anchor: "fn catalog_table(",
        query: "pass 1 `SourceReads` (sandbox only until Phase B)",
        note: "a table of the source catalog by name (dynamic SQL, column owners).",
    },
    Site {
        id: "P4",
        file: "sqldiff-draft/src/tsql.rs",
        anchor: "fn column_owners(",
        query: "pass 1 `SourceReads` (sandbox only until Phase B)",
        note: "the owners of a column name among catalog tables.",
    },
    Site {
        id: "P5",
        file: "sqldiff-draft/src/tsql.rs",
        anchor: "fn routine_in<'c>(",
        query: "pass 1 `SourceReads` (sandbox only until Phase B)",
        note: "callee routine lookup (`callee`, `callee_returns_status`).",
    },
    Site {
        id: "P6",
        file: "sqldiff-draft/src/tsql.rs",
        anchor: "fn type_catalog(",
        query: "pass 1 `SourceReads` (sandbox only until Phase B)",
        note: "the catalog for type inference (`own_table`, `entry_table`).",
    },
    Site {
        id: "P7",
        file: "sqldiff-draft/src/tsql/bits.rs",
        anchor: "fn bits_possible(",
        query: "pass 1 `SourceReads` (sandbox only until Phase B)",
        note: "bit columns of catalog tables.",
    },
    Site {
        id: "P8",
        file: "sqldiff-draft/src/tsql/temporal.rs",
        anchor: "fn history_check(",
        query: "pass 1 `SourceReads` (sandbox only until Phase B)",
        note: "temporal tables of the catalog.",
    },
    Site {
        id: "P9",
        file: "sqldiff-draft/src/tsql/temporal.rs",
        anchor: "fn history_of_historyless(",
        query: "pass 1 `SourceReads` (sandbox only until Phase B)",
        note: "history table of a historyless table.",
    },
    Site {
        id: "P10",
        file: "sqldiff-draft/src/tsql/temporal.rs",
        anchor: "pub(super) fn alter_versioning(",
        query: "pass 1 `SourceReads` (sandbox only until Phase B)",
        note: "versioning of a catalog table.",
    },
    Site {
        id: "P11",
        file: "sqldiff-draft/src/tsql/paging.rs",
        anchor: "fn not_null_key(",
        query: "pass 1 `SourceReads` (sandbox only until Phase B)",
        note: "key nullability for paging.",
    },
    Site {
        id: "P12",
        file: "sqldiff-draft/src/tsql/trigger.rs",
        anchor: "pub(super) fn drop_trigger(",
        query: "pass 1 `SourceReads` (sandbox only until Phase B)",
        note: "trigger by name (`triggers_named`).",
    },
    Site {
        id: "P2",
        file: "sqldiff-draft/src/tsql.rs",
        anchor: "local: std::cell::OnceCell<sqldiff_equiv::catalog::Catalog>",
        query: "none (the file's own DDL)",
        note: "Not an external input: built from the file's text.",
    },
    Site {
        id: "T1",
        file: "sqldiff-convert/src/tree.rs",
        anchor: "let source = source.map(Arc::new);",
        query: "pass 1 handle",
        note: "Done in PR 2a: pass 1 holds the tree state's own `Arc<Catalog>` (`source`), not one read back out of `Options`, whose `Facts` handle only the engine opens.",
    },
];

/// `CatalogView` public methods: each is a builder or becomes a query.
const VIEW_METHODS: &[(&str, &str)] = &[
    ("new", "builder"),
    (
        "unrecorded_catalog",
        "engine only: `Schema::build` until PR 2b, and `RecordingView::whole`",
    ),
    ("scratch", "builder (a `LocalView`)"),
    ("put_table", "builder"),
    ("put_routine", "builder"),
    ("apply_statement", "builder"),
    ("tables_authoritative", "TablesAuthoritative"),
    ("routines_authoritative", "RoutinesAuthoritative"),
    ("supplied", "Supplied"),
    ("origins", "Origin"),
    ("origin", "Origin"),
    ("table", "Table"),
    ("routines", "Routines"),
];

/// `Schema` public methods: each is a builder or reads the catalog.
const SCHEMA_METHODS: &[(&str, &str)] = &[
    ("for_check", "per-check handle (side + sink)"),
    ("catalog", "the view door (Supplied)"),
    ("sources", "Supplied / Origin (the recorded source list)"),
    ("lists_stars", "HiddenTables (the `list_stars` flag)"),
    ("is_star_view", "StarView"),
    ("column_types", "ColumnNamed / SchemaTables"),
    (
        "ask",
        "replay: every Schema query again, through the check's own readers",
    ),
    ("table", "SchemaTables"),
    ("view_column_is_not_char", "ViewFamily"),
    ("table_exact", "SchemaTables"),
    ("with_images", "builder over SchemaTables"),
    ("with_ddl", "builder over SchemaTables"),
    ("with_local", "builder (the file's own DDL)"),
    ("case_check", "Spellings"),
    ("load", "builder"),
    ("hides_columns", "HiddenTables"),
    ("list_stars_with", "HiddenTables / SchemaTables"),
    ("from_view", "builder (Spellings, ViewFamily, StarView)"),
    (
        "from_shared_view",
        "builder (Spellings, ViewFamily, StarView)",
    ),
];

/// Every `impl` block of `ty` in the crate sources (inherent and trait
/// impls, with generics): `(file, header)` of each, in file order.
fn impl_blocks_of(ty: &str, dir: &str) -> Vec<(String, String)> {
    let re = Regex::new(&format!(
        r"(?m)^\s*impl(?:<[^>]*>)?\s+(?:[\w:<>]+\s+for\s+)?(?:&\s*)?{ty}\b[^{{]*\{{"
    ))
    .unwrap();
    let root = crates_dir();
    let mut files = Vec::new();
    rs_files(&root.join(dir), &mut files);
    let mut out = Vec::new();
    for p in files {
        let text = without_comments(&without_test_modules(&std::fs::read_to_string(&p).unwrap()));
        let rel = p
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        for m in re.find_iter(&text) {
            out.push((rel.clone(), m.as_str().trim().to_string()));
        }
    }
    out
}

/// `RecordingView` public methods: each one records a query.
const RECORDING_METHODS: &[(&str, &str)] = &[
    ("new", "builder (engine only)"),
    ("dialect", "none (a fact of the side, not of the catalog)"),
    ("supplied", "Supplied"),
    ("origin", "Origin"),
    ("tables_authoritative", "TablesAuthoritative"),
    ("routines_authoritative", "RoutinesAuthoritative"),
    ("table", "Table"),
    ("routines", "Routines"),
    (
        "ask",
        "replay: a RecordingView query again, through the check's own method",
    ),
    ("whole", "WholeCatalog"),
];

/// `SourceReads` public methods: pass 1's only door to the tree's catalog.
const SOURCE_METHODS: &[(&str, &str)] = &[
    (
        "new",
        "builder (the driver's one call; `sqldiff-draft/src/lib.rs`)",
    ),
    (
        "recording",
        "builder with a live sink (the tree run's pass 1, `sqldiff-draft/src/lib.rs`)",
    ),
    ("table", "pass 1 `Table(parts)`"),
    (
        "table_wide",
        "pass 1 `WholeCatalog` (the narrow form waits for a T3 proof)",
    ),
    (
        "triggers_named_wide",
        "pass 1 `WholeCatalog` (the narrow form waits for a T3 proof)",
    ),
    ("triggers_named", "pass 1 `TriggersNamed`"),
    ("column_types", "pass 1 `ColumnTypes`"),
    (
        "column_types_wide",
        "pass 1 `WholeCatalog` (unused; the narrow form is proven by T3)",
    ),
    ("tables", "pass 1 `WholeCatalog`"),
    ("routines", "pass 1 `WholeCatalog`"),
    (
        "ask",
        "replay: a pass-1 query again, through the draft's own method",
    ),
];

/// `LocalView` public methods: the file's own objects, no external read.
const LOCAL_METHODS: &[(&str, &str)] = &[
    ("new", "builder"),
    ("put_table", "builder"),
    ("put_routine", "builder"),
    ("apply_statement", "builder"),
    ("table", "none (the file's own DDL)"),
    ("routines", "none (the file's own DDL)"),
];

fn pub_fns_of_impl(file: &str, header: &str) -> Vec<String> {
    let text = std::fs::read_to_string(crates_dir().join(file)).unwrap();
    let start = text
        .find(header)
        .unwrap_or_else(|| panic!("no `{header}` in {file}"));
    let body = &text[start + header.len()..];
    let body = &body[..block_end(body)];
    let re = Regex::new(r"(?m)^    pub fn (\w+)").unwrap();
    re.captures_iter(body).map(|c| c[1].to_string()).collect()
}

/// A second `impl` of a catalog reader (a `Deref`, a trait impl, another
/// inherent block) is a way in the method lists above do not see.
#[test]
fn the_catalog_readers_have_no_other_impl_blocks() {
    let all = [
        "sqldiff-catalog/src",
        "sqldiff-core/src",
        "sqldiff-equiv/src",
        "sqldiff-model/src",
        "sqldiff-draft/src",
        "sqldiff-convert/src",
    ];
    for (ty, allowed) in [
        (
            "CatalogView",
            vec![("sqldiff-catalog/src/lib.rs", "impl CatalogView {")],
        ),
        (
            "RecordingView",
            vec![(
                "sqldiff-catalog/src/record.rs",
                "impl<'a> RecordingView<'a> {",
            )],
        ),
        (
            "SourceReads",
            vec![(
                "sqldiff-catalog/src/record.rs",
                "impl<'a> SourceReads<'a> {",
            )],
        ),
        (
            "LocalView",
            vec![("sqldiff-catalog/src/lib.rs", "impl LocalView {")],
        ),
        (
            "Schema",
            vec![
                ("sqldiff-equiv/src/equiv/schema.rs", "impl Schema {"),
                (
                    "sqldiff-equiv/src/rules/rewrite.rs",
                    "impl Facts for Schema {",
                ),
            ],
        ),
    ] {
        let mut got = Vec::new();
        for dir in all {
            got.extend(impl_blocks_of(ty, dir));
        }
        let want: Vec<(String, String)> = allowed
            .iter()
            .map(|(f, h)| (f.to_string(), h.to_string()))
            .collect();
        assert_eq!(
            got, want,
            "another `impl` of `{ty}`: it can open the catalog without a query; \
             classify its methods in tests/catalog_sites.rs"
        );
    }
}

#[test]
fn the_public_methods_of_the_catalog_readers_are_all_classified() {
    for (file, header, table) in [
        (
            "sqldiff-catalog/src/lib.rs",
            "impl CatalogView {",
            VIEW_METHODS,
        ),
        (
            "sqldiff-equiv/src/equiv/schema.rs",
            "impl Schema {",
            SCHEMA_METHODS,
        ),
        (
            "sqldiff-catalog/src/record.rs",
            "impl<'a> RecordingView<'a> {",
            RECORDING_METHODS,
        ),
        (
            "sqldiff-catalog/src/lib.rs",
            "impl LocalView {",
            LOCAL_METHODS,
        ),
        (
            "sqldiff-catalog/src/record.rs",
            "impl<'a> SourceReads<'a> {",
            SOURCE_METHODS,
        ),
    ] {
        let got = pub_fns_of_impl(file, header);
        let want: Vec<String> = table.iter().map(|(n, _)| n.to_string()).collect();
        assert_eq!(
            got, want,
            "{header} changed: a new public method reads (or builds) a catalog; \
             give it a query in the table in tests/catalog_sites.rs (and a `Query` variant if it reads)"
        );
    }
}

/// The check crates hold no `CatalogView` and read no unrecorded catalog:
/// only the engine, where a side is built, and the equivalence schema's
/// construction (until PR 2b) may.
#[test]
fn the_check_crates_hold_no_catalog_view_outside_the_engine() {
    let allowed = [
        "sqldiff-core/src/engine.rs",
        "sqldiff-equiv/src/equiv/schema.rs",
    ];
    let root = crates_dir();
    for dir in ["sqldiff-core/src", "sqldiff-equiv/src", "sqldiff-model/src"] {
        let mut files = Vec::new();
        rs_files(&root.join(dir), &mut files);
        for p in files {
            let rel = p
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let text =
                without_comments(&without_test_modules(&std::fs::read_to_string(&p).unwrap()));
            for needle in ["CatalogView", "unrecorded_catalog"] {
                let re = Regex::new(&format!(r"\b{needle}\b")).unwrap();
                assert!(
                    allowed.contains(&rel.as_str()) || !re.is_match(&text),
                    "{rel} names `{needle}`: a check reads an external catalog through a \
                     `RecordingView` (`Schema::catalog`, `BindInput::view`); only {allowed:?} may open one"
                );
            }
        }
    }
}

/// The unrecorded sink is named only where an unrecorded read is meant: the
/// recorder itself, the engine's `unrecorded()` helper and `Schema`'s
/// fallback for a schema with no per-check handle.
#[test]
fn the_unrecorded_sink_is_named_only_by_its_allowed_files() {
    let allowed = [
        "sqldiff-core/src/engine.rs",
        "sqldiff-equiv/src/equiv/schema.rs",
        "sqldiff-catalog/src/record.rs",
    ];
    let root = crates_dir();
    let re = Regex::new(r"\bNO_SINK\b|\bNoSink\b").unwrap();
    for dir in [
        "sqldiff-core/src",
        "sqldiff-equiv/src",
        "sqldiff-model/src",
        "sqldiff-catalog/src",
    ] {
        let mut files = Vec::new();
        rs_files(&root.join(dir), &mut files);
        for p in files {
            let rel = p
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let text =
                without_comments(&without_test_modules(&std::fs::read_to_string(&p).unwrap()));
            assert!(
                allowed.contains(&rel.as_str()) || !re.is_match(&text),
                "{rel} names the unrecorded sink; only {allowed:?} may"
            );
        }
    }
}

/// Pass 1 reads the tree's catalog only through `SourceReads`: its
/// `Cx::catalog` field has that type, and none of the `Catalog` accessors a
/// read used before is named in the crate any more.
#[test]
fn pass_1_reads_the_tree_catalog_only_through_source_reads() {
    let root = crates_dir();
    let mut files = Vec::new();
    rs_files(&root.join("sqldiff-draft/src"), &mut files);
    let mut fields = 0;
    for p in files {
        let text = without_comments(&without_test_modules(&std::fs::read_to_string(&p).unwrap()));
        for needle in [
            "table_position(",
            ".triggers_named(Dialect",
            "catalog: Option<&",
        ] {
            assert!(
                !text.contains(needle),
                "{} reads the tree's catalog without `SourceReads` (`{needle}`)",
                p.display()
            );
        }
        fields += text.matches("catalog: Option<SourceReads<").count();
    }
    assert!(
        fields >= 2,
        "the convert entry and `Cx` hold a `SourceReads`"
    );
}

#[test]
fn every_site_anchor_exists() {
    for s in SITES {
        let text = std::fs::read_to_string(crates_dir().join(s.file))
            .unwrap_or_else(|e| panic!("{}: {e}", s.file));
        assert!(
            text.contains(s.anchor),
            "site {} ({}): anchor `{}` is gone; update the inventory",
            s.id,
            s.file,
            s.anchor
        );
    }
}

/// Counts per door and file, from the non-test sources.
fn door_counts() -> BTreeMap<(String, String), usize> {
    let root = crates_dir();
    let mut counts = BTreeMap::new();
    for (dir, _) in CRATES {
        let mut files = Vec::new();
        rs_files(&root.join(dir), &mut files);
        assert!(!files.is_empty(), "no sources under {dir}");
        for p in files {
            let rel = p
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let text =
                without_comments(&without_test_modules(&std::fs::read_to_string(&p).unwrap()));
            for (name, re, _) in DOORS {
                if !door_applies(name, &rel) {
                    continue;
                }
                let n = Regex::new(re).unwrap().find_iter(&text).count();
                if n > 0 {
                    counts.insert((name.to_string(), rel.clone()), n);
                }
            }
        }
    }
    counts
}

fn markdown() -> String {
    let counts = door_counts();
    let mut out = String::new();
    out.push_str("# Catalog read sites\n\n");
    out.push_str(
        "Generated by `crates/sqldiff-core/tests/catalog_sites.rs`; do not edit by hand:\n\
         `SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test catalog_sites`.\n\n\
         Inventory for the recorded-catalog-reads series (performance plan W4.3). It lists where \
         the check reads a catalog and the query each read becomes. See \
         `perf-plan/w4-3-recorded-catalog-reads-design.md`.\n\n",
    );
    out.push_str("## Sites\n\n| id | file | anchor | becomes | note |\n|---|---|---|---|---|\n");
    for s in SITES {
        out.push_str(&format!(
            "| {} | `{}` | `{}` | {} | {} |\n",
            s.id,
            s.file,
            s.anchor.replace('|', "\\|"),
            s.query.replace('|', "\\|"),
            s.note.replace('|', "\\|")
        ));
    }
    out.push_str("\n## Doors\n\nCounts of each kind of door in the non-test sources, comments left out. A change here fails the test: add the new read to the sites above.\n\n");
    for (name, re, what) in DOORS {
        out.push_str(&format!("### {name}\n\n`{re}`: {what}\n\n"));
        out.push_str("| file | stage | count |\n|---|---|---|\n");
        for ((n, file), c) in &counts {
            if n == name {
                let stage = CRATES
                    .iter()
                    .find(|(d, _)| file.starts_with(*d))
                    .map(|(_, s)| *s)
                    .unwrap_or("");
                out.push_str(&format!("| `{file}` | {stage} | {c} |\n"));
            }
        }
        out.push('\n');
    }
    out.push_str("## `CatalogView` public methods\n\n| method | becomes |\n|---|---|\n");
    for (m, q) in VIEW_METHODS {
        out.push_str(&format!("| `{m}` | {q} |\n"));
    }
    out.push_str("\n## `Schema` public methods\n\n| method | becomes |\n|---|---|\n");
    for (m, q) in SCHEMA_METHODS {
        out.push_str(&format!("| `{m}` | {q} |\n"));
    }
    out.push_str("\n## `RecordingView` public methods\n\n| method | becomes |\n|---|---|\n");
    for (m, q) in RECORDING_METHODS {
        out.push_str(&format!("| `{m}` | {q} |\n"));
    }
    out.push_str("\n## `SourceReads` public methods\n\n| method | becomes |\n|---|---|\n");
    for (m, q) in SOURCE_METHODS {
        out.push_str(&format!("| `{m}` | {q} |\n"));
    }
    out.push_str("\n## `LocalView` public methods\n\n| method | becomes |\n|---|---|\n");
    for (m, q) in LOCAL_METHODS {
        out.push_str(&format!("| `{m}` | {q} |\n"));
    }
    out
}

#[test]
fn the_inventory_document_is_generated_from_the_sources() {
    let path = crates_dir().join("../docs/catalog-read-sites.md");
    let want = markdown();
    if std::env::var_os("SQLDIFF_BLESS").is_some() {
        std::fs::write(&path, &want).unwrap();
    }
    // A CRLF checkout (Windows) is the same file.
    let got = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        got == want,
        "{} is stale: a read of a catalog was added, moved or removed. Give it an entry in the \
         sites table and run SQLDIFF_BLESS=1 cargo test -p sqldiff-core --test catalog_sites",
        path.display()
    );
}
