//! A file of thousands of routines is bound in linear time. Every call and
//! temporary-table reference used to build a view of the objects created so
//! far, so the work grew with the square of the routines (4 000 routines
//! took half a minute in a release build).

use sqldiff_core::{check, Artifact, Dialect, Options};

#[test]
fn thousands_of_routines_in_one_file_are_bound_quickly() {
    let n = 3_000;
    let text: String = (0..n)
        .map(|i| {
            format!(
                "CREATE PROCEDURE dbo.p{i} AS BEGIN\n  CREATE TABLE #t (a int);\n  INSERT #t VALUES (1);\n  EXEC dbo.p{};\n  SELECT a FROM #t;\nEND\nGO\n",
                (i + 1) % n
            )
        })
        .collect();
    let started = std::time::Instant::now();
    let r = check(
        &Artifact::new("a.sql", Dialect::Tsql, text),
        &Options::default(),
    );
    assert!(r.findings.is_empty(), "{:?}", r.findings);
    // A smoke bound, generous for a debug build; the quadratic code needed
    // minutes even in release.
    assert!(started.elapsed().as_secs() < 60, "{:?}", started.elapsed());
}
