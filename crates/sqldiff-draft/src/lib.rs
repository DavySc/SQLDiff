//! The conversion rules' first pass: the draft of a file, before any check.
//!
//! Depends on the syntax, catalog, model, equivalence and unit crates and on
//! nothing above them, so the fingerprint of this stage (`docs/crate-split.md`)
//! is that of exactly the code that writes a draft. A check never calls into
//! it except to build a draft again (`Rebuild`).

use sqldiff_syntax::parse;
use sqldiff_syntax::Dialect;
use sqldiff_units::out;
use sqldiff_units::{Draft, Note, RuleOptions, Status, Step};

pub mod table_vars;
mod tok;
pub mod tsql;

/// Apply the rules only. Nothing in a draft has been checked yet.
pub fn rewrite(text: &str, from: Dialect, to: Dialect) -> Draft {
    rewrite_in(text, from, to, None)
}

/// Apply the rules with the source catalog of the whole tree being
/// converted (`sqldiff_equiv::catalog::ddl` over every input file), so a
/// rule can rely on what other files define: a call with OUTPUT arguments
/// converts only when the called procedure's parameters are known.
pub fn rewrite_in(
    text: &str,
    from: Dialect,
    to: Dialect,
    source: Option<&sqldiff_equiv::catalog::Catalog>,
) -> Draft {
    rewrite_in_tree(text, from, to, source, None, RuleOptions::default())
}

/// [`rewrite_in`] with `options` for the rules.
pub fn rewrite_with(
    text: &str,
    from: Dialect,
    to: Dialect,
    source: Option<&sqldiff_equiv::catalog::Catalog>,
    options: RuleOptions,
) -> Draft {
    rewrite_in_tree(text, from, to, source, None, options)
}

/// [`rewrite_in`] with the temporary table names the whole tree's table
/// variables claim (`table_vars::Claims`), so a table variable of this
/// text that would take another routine's name is refused. Without
/// `claims`, the text's own routines are checked against each other.
pub fn rewrite_in_tree(
    text: &str,
    from: Dialect,
    to: Dialect,
    source: Option<&sqldiff_equiv::catalog::Catalog>,
    claims: Option<&table_vars::Claims>,
    options: RuleOptions,
) -> Draft {
    rewrite_in_tree_plain(text, from, to, source, claims, options, &[])
}

/// [`rewrite_in_tree`] with the routines starting at the source offsets
/// `plain_at` (sorted) converted without exception blocks and the rest as
/// the rules write them: one variant, no second draft kept.
pub fn rewrite_in_tree_plain(
    text: &str,
    from: Dialect,
    to: Dialect,
    source: Option<&sqldiff_equiv::catalog::Catalog>,
    claims: Option<&table_vars::Claims>,
    options: RuleOptions,
    plain_at: &[usize],
) -> Draft {
    let root = parse(text, from).syntax();
    let own;
    let claims = match claims {
        Some(c) => c,
        None => {
            let mut c = table_vars::Claims::default();
            if from == Dialect::Tsql {
                c.add_root(&root, "");
            }
            own = c;
            &own
        }
    };
    let run = || {
        guarded(&root, text, || match (from, to) {
            (Dialect::Tsql, Dialect::Postgres | Dialect::Db2i) => {
                tsql::convert(
                    &root,
                    text,
                    to,
                    // The one place the tree's catalog enters pass 1: wrapped, so
                    // that every read after it is a `SourceReads` method.
                    source.map(|c| {
                        sqldiff_equiv::catalog::record::SourceReads::new(c, Dialect::Tsql)
                    }),
                    claims,
                    options,
                )
            }
            _ => no_rules(&root, text, from, to),
        })
    };
    if !plain_at.is_empty() {
        let raw = tsql::with_plain_routines(plain_at, run);
        return Draft {
            from,
            to,
            options,
            raw,
            plain: None,
        };
    }
    let raw = run();
    let plain = raw
        .units
        .iter()
        .any(|u| u.trace.iter().any(|s| s.rule == tsql::CONTINUE_RULE))
        .then(|| {
            let raw = tsql::without_continuation(run);
            Box::new(Draft {
                from,
                to,
                options,
                raw,
                plain: None,
            })
        });
    Draft {
        from,
        to,
        options,
        raw,
        plain,
    }
}

/// Runs the rules over one file. A panic in them must not take the other files
/// of a tree with it: the file becomes "not converted", every unit with the
/// reason, and the run goes on.
fn guarded(
    root: &sqldiff_syntax::SyntaxNode,
    text: &str,
    rules: impl FnOnce() -> out::Out,
) -> out::Out {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(rules)) {
        Ok(o) => o,
        Err(e) => {
            let what = e
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| e.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown".into());
            refuse_all(
                root,
                text,
                &format!(
                    "the conversion rules stopped on this file with an internal error ({what}); nothing in it was converted. This is a bug: report it with a reduced example"
                ),
                "internal-error",
            )
        }
    }
}

/// Every top-level unit kept as a comment, for a pair without rules.
fn no_rules(root: &sqldiff_syntax::SyntaxNode, text: &str, from: Dialect, to: Dialect) -> out::Out {
    let reason = if from == to {
        format!("source and target are both {}", from.display_name())
    } else {
        format!(
            "no conversion rules from {} to {} yet",
            from.display_name(),
            to.display_name()
        )
    };
    refuse_all(root, text, &reason, "no-rules")
}

/// Every unit of the file kept as a comment, "not converted" for `reason`.
fn refuse_all(root: &sqldiff_syntax::SyntaxNode, text: &str, reason: &str, rule: &str) -> out::Out {
    let mut o = out::Out::default();
    let units = root.children().flat_map(|c| {
        if c.kind() == sqldiff_syntax::SyntaxKind::BATCH {
            c.children().filter(|u| u.kind().is_unit()).collect()
        } else if c.kind().is_unit() {
            vec![c]
        } else {
            Vec::new()
        }
    });
    for u in units {
        let (s, e) = sqldiff_syntax::ast::sig_range(&u);
        let mut t = format!("-- [not converted] {reason}\n");
        for line in text[s..e].lines() {
            t.push_str("-- ");
            t.push_str(line);
            t.push('\n');
        }
        o.unit(
            (s, e),
            0,
            &t,
            vec![Note {
                status: Status::NotConverted,
                rule: rule.into(),
                text: reason.to_string().into(),
            }],
            vec![Step {
                rule: rule.into(),
                start: s,
                end: e,
                inst: None,
            }],
        );
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_in_the_rules_makes_the_file_not_converted_not_a_crash() {
        let text = "CREATE PROCEDURE a AS SELECT 1\nGO\nCREATE PROCEDURE b AS SELECT 2\nGO\n";
        let root = parse(text, Dialect::Tsql).syntax();
        let o = guarded(&root, text, || panic!("the rules lost their place"));
        assert!(o.text.contains("[not converted]"), "{}", o.text);
        assert!(o.text.contains("the rules lost their place"), "{}", o.text);
        assert_eq!(o.units.len(), 2, "every unit is accounted for");
        assert!(o.units.iter().all(|u| u
            .notes
            .iter()
            .any(|n| n.rule == "internal-error" && n.status == Status::NotConverted)));
    }

    #[test]
    fn rules_that_do_not_panic_are_untouched() {
        let text = "CREATE PROCEDURE a AS SELECT 1\nGO\n";
        let root = parse(text, Dialect::Tsql).syntax();
        let o = guarded(&root, text, out::Out::default);
        assert!(o.text.is_empty());
    }
}
