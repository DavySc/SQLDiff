//! A routine body that a generator wraps in a routine header it writes itself
//! is converted inside a stand-in procedure that declares the parameters the
//! tree lists for it (`template-parameters.toml`), so the names it uses bind
//! and read as the target's parameters.

use std::path::PathBuf;

use sqldiff_convert::tree::{self, TemplateMode, Tree};
use sqldiff_convert::Status;
use sqldiff_core::Dialect;

const BODY: &str = "IF @mode = 1 AND @code IS NOT NULL\nBEGIN\n    UPDATE <%T%> SET a = @code WHERE b = @mode;\nEND\n";

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str, files: &[(&str, &str)]) -> Fixture {
        let dir = std::env::temp_dir().join(format!("sqldiff-rp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (p, text) in files {
            let f = dir.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, text).unwrap();
        }
        Fixture { dir }
    }

    fn convert(&self) -> Result<Tree, String> {
        let def: sqldiff_source::pack::TemplateSyntax =
            serde_json::from_value(serde_json::json!({ "open": "<%", "close": "%>" })).unwrap();
        let a = tree::Args {
            src: self.dir.clone(),
            from: Dialect::Tsql,
            to: Dialect::Postgres,
            jobs: Some(1),
            no_catalog: true,
            template: Some(TemplateMode::new(&def, None).unwrap()),
            extensions: tree::extensions(Vec::new()),
            helpers: Vec::new(),
            deployment: Default::default(),
            catalogs: Vec::new(),
            catalogs_b: Vec::new(),
        };
        let (inputs, unreadable) = tree::read(&a).unwrap();
        tree::convert(&a, &inputs, unreadable, None)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn rules(t: &Tree) -> Vec<String> {
    t.conversions[0]
        .units
        .iter()
        .flat_map(|u| u.notes.iter().map(|n| n.rule.to_string()))
        .chain(
            t.conversions[0]
                .output_check
                .findings
                .iter()
                .map(|f| f.rule.clone()),
        )
        .collect()
}

#[test]
fn listed_parameters_bind_and_become_p_names() {
    let fx = Fixture::new(
        "binds",
        &[
            ("procs/body.sql", BODY),
            (
                "template-parameters.toml",
                "[\"procs/body.sql\"]\nparameters = [\"@mode int\", \"@code varchar(20) output\"]\n",
            ),
        ],
    );
    let t = fx.convert().unwrap();
    let c = &t.conversions[0];
    assert!(
        c.output.contains("p_mode") && c.output.contains("p_code"),
        "{}",
        c.output
    );
    assert!(
        !c.output.contains("v_mode") && !c.output.contains("v_code"),
        "{}",
        c.output
    );
    let r = rules(&t);
    assert!(!r.iter().any(|r| r == "bind.unknown-variable"), "{r:?}");
    let fragment = c
        .units
        .iter()
        .flat_map(|u| &u.notes)
        .find(|n| n.rule == "ASM-TEMPLATE-FRAGMENT")
        .expect("the fragment assumption stays");
    assert!(fragment.text.contains("2 parameters"), "{}", fragment.text);
}

#[test]
fn without_the_file_the_same_body_has_unknown_variables() {
    let fx = Fixture::new("baseline", &[("procs/body.sql", BODY)]);
    let t = fx.convert().unwrap();
    assert!(
        rules(&t).iter().any(|r| r == "bind.unknown-variable"),
        "{:?}",
        rules(&t)
    );
    assert_ne!(t.conversions[0].units[0].status, Status::Converted);
}

#[test]
fn a_name_the_list_leaves_out_stays_unknown() {
    let fx = Fixture::new(
        "partial",
        &[
            ("procs/body.sql", BODY),
            (
                "template-parameters.toml",
                "[\"procs/body.sql\"]\nparameters = [\"@mode int\"]\n",
            ),
        ],
    );
    let t = fx.convert().unwrap();
    let c = &t.conversions[0];
    assert!(
        rules(&t).iter().any(|r| r == "bind.unknown-variable"),
        "{:?}",
        rules(&t)
    );
    let finding = c
        .output_check
        .findings
        .iter()
        .find(|f| f.rule == "bind.unknown-variable")
        .unwrap();
    assert!(finding.witness.contains("code"), "{}", finding.witness);
    assert!(!finding.witness.contains("p_mode"), "{}", finding.witness);
}

#[test]
fn only_the_listed_template_gets_a_procedure_around_it() {
    let fx = Fixture::new(
        "scope",
        &[
            ("procs/body.sql", BODY),
            ("other/plain.sql", "SELECT 1;\n"),
            (
                "template-parameters.toml",
                "[\"procs/body.sql\"]\nparameters = [\"@mode int\", \"@code varchar(20)\"]\n",
            ),
        ],
    );
    let t = fx.convert().unwrap();
    assert_eq!(t.conversions.len(), 2);
    let plain = t
        .conversions
        .iter()
        .find(|c| c.output.contains("SELECT 1"))
        .unwrap();
    assert!(!plain.output.contains("p_"), "{}", plain.output);
}

#[test]
fn a_bad_file_is_an_error_naming_the_entry() {
    for (toml, needle) in [
        (
            "[\"a.sql\"]\nparameters = [\"mode int\"]\n",
            "not a parameter declaration",
        ),
        (
            "[\"a.sql\"]\nparameters = [\"@a int; drop table t\"]\n",
            "not a parameter declaration",
        ),
        (
            "[\"a.sql\"]\nparameters = [\"@a int -- note\"]\n",
            "not a parameter declaration",
        ),
        // Text that would end the stand-in header early or hide the body.
        (
            "[\"a.sql\"]\nparameters = [\"@a int /*\"]\n",
            "not a parameter declaration",
        ),
        (
            "[\"a.sql\"]\nparameters = [\"@a int /* note */\"]\n",
            "not a parameter declaration",
        ),
        (
            "[\"a.sql\"]\nparameters = [\"@a int) AS BEGIN SELECT 1 END /*\"]\n",
            "not a parameter declaration",
        ),
        (
            "[\"a.sql\"]\nparameters = [\"@a varchar(20\"]\n",
            "not a parameter declaration",
        ),
        (
            "[\"a.sql\"]\nparameters = [\"@a varchar(5) = 'x\"]\n",
            "not a parameter declaration",
        ),
        (
            "[\"a.sql\"]\nparameters = [\"@a int, @b int\"]\n",
            "not a parameter declaration",
        ),
        (
            "[\"a.sql\"]\nparameters = [\"@a int\", \"@A bigint\"]\n",
            "listed before",
        ),
        ("[\"a.sql\"]\nparameters = \"@a int\"\n", "list of strings"),
        ("[\"a.sql\"]\nparameter = [\"@a int\"]\n", "unknown key"),
        ("\"a.sql\" = 3\n", "expected a table"),
    ] {
        let fx = Fixture::new(
            "bad",
            &[("a.sql", "SELECT 1;\n"), ("template-parameters.toml", toml)],
        );
        let e = fx.convert().err().expect(toml);
        assert!(e.contains(needle) && e.contains("a.sql"), "{toml}: {e}");
    }
}

#[test]
fn a_windows_path_in_the_file_still_finds_the_template() {
    let fx = Fixture::new(
        "slashes",
        &[
            ("procs/body.sql", BODY),
            (
                "template-parameters.toml",
                "['procs\\body.sql']\nparameters = [\"@mode int\", \"@code varchar(20)\"]\n",
            ),
        ],
    );
    let t = fx.convert().unwrap();
    assert!(
        t.conversions[0].output.contains("p_mode"),
        "{}",
        t.conversions[0].output
    );
}

#[test]
fn declarations_with_defaults_and_inner_commas_are_read() {
    let fx = Fixture::new(
        "forms",
        &[
            ("procs/body.sql", BODY),
            (
                "template-parameters.toml",
                "[\"procs/body.sql\"]\nparameters = [\"@mode AS decimal(10, 2) = -1\", \"@code varchar(20) = 'a,b;c' OUTPUT\"]\n",
            ),
        ],
    );
    let t = fx.convert().unwrap();
    let c = &t.conversions[0];
    assert!(
        c.output.contains("p_mode") && c.output.contains("p_code"),
        "{}",
        c.output
    );
}

/// A stand-in that swallows the body (here a declaration that opens a
/// comment, which the file check refuses before it gets here) leaves the
/// template not converted, never a "converted" file of unchanged T-SQL.
#[test]
fn a_body_the_stand_in_swallows_is_not_converted() {
    use sqldiff_convert::{check_template, rewrite_template_with, FragmentKind, StandIns};
    use sqldiff_core::composition::PlaceholderBasis;
    use sqldiff_syntax::template::{Compiled, Template, TemplateSyntax};
    let syntax = Compiled::new(&TemplateSyntax {
        open: "<%".into(),
        close: "%>".into(),
        name_pattern: "[A-Z][A-Z0-9_.]*".into(),
        special_forms: Vec::new(),
        ignore_case: false,
    })
    .unwrap();
    let t = Template::parse(BODY, &syntax, None);
    let stand = StandIns::new([&t]);
    for parameters in [
        vec!["@mode int /*".to_string(), "@code varchar(20)".to_string()],
        vec!["@mode int) AS BEGIN SELECT 1 END /*".to_string()],
    ] {
        let d = rewrite_template_with(
            &t,
            FragmentKind::RoutineBody,
            &parameters,
            Dialect::Tsql,
            Dialect::Postgres,
            &stand,
            None,
        );
        let c = check_template(
            &t,
            d,
            &stand,
            &sqldiff_core::Options::default(),
            PlaceholderBasis::DeclaredPattern,
            ("<%", "%>"),
        );
        assert!(!c.units.is_empty(), "{parameters:?}: {}", c.output);
        assert!(
            c.units.iter().all(|u| u.status > Status::Assumptions),
            "{parameters:?}: {:#?}",
            c.units
        );
    }
}
