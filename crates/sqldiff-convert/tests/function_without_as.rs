//! A T-SQL function without `AS` before its body converts like the same
//! function with it, and a routine that has no body at all is refused for that
//! reason: neither may stop the conversion of the other files of a tree.

use sqldiff_convert::{rewrite, Status};
use sqldiff_core::Dialect;

const WITH_AS: &str = "CREATE FUNCTION dbo.IsOk()\nRETURNS bit\nAS\nBEGIN\n    RETURN 1\nEND\nGO\n";
const NO_AS: &str = "CREATE FUNCTION dbo.IsOk()\nRETURNS bit\nBEGIN\n    RETURN 1\nEND\nGO\n";

#[test]
fn a_function_without_as_converts_like_one_with_it() {
    for to in [Dialect::Postgres, Dialect::Db2i] {
        let a = rewrite(WITH_AS, Dialect::Tsql, to);
        let b = rewrite(NO_AS, Dialect::Tsql, to);
        assert_eq!(a.output(), b.output(), "{to:?}");
        assert!(
            !b.output().contains("[not converted]"),
            "{to:?}: {}",
            b.output()
        );
    }
}

#[test]
fn a_routine_without_a_body_is_not_converted_with_a_reason() {
    let src = "CREATE PROCEDURE dbo.Broken\nGO\n";
    let d = rewrite(src, Dialect::Tsql, Dialect::Postgres);
    assert!(d.output().contains("[not converted]"), "{}", d.output());
    assert!(d.output().contains("without a body"), "{}", d.output());
}

#[test]
fn statuses_are_not_converted_not_a_crash() {
    use sqldiff_convert::check;
    use sqldiff_core::Options;
    let src = "CREATE PROCEDURE dbo.Broken\nGO\n";
    let c = check(
        src,
        rewrite(src, Dialect::Tsql, Dialect::Postgres),
        &Options::default(),
    );
    assert!(
        c.units.iter().all(|u| u.status == Status::NotConverted),
        "{:?}",
        c.units.iter().map(|u| u.status).collect::<Vec<_>>()
    );
}
