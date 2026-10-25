//! Convert mode: type SQL on the left, see it converted and checked on the right.
//!
//! `GET /` (or `/convert` beside the pair view) serves the page with the
//! initial text converted; `POST /api/convert` with `{"text", "from"?, "to"?}`
//! converts again, for any source and target (a pair without rules comes back
//! with every statement not converted). `POST /api/compare` with
//! `{"a", "b", "from", "to"}` diffs two scratch texts (the page's compare
//! mode). Every unit of the output carries a badge from `sqldiff_convert`
//! (rules + the engine's diff of input against output). Nothing is written
//! to disk.

use std::collections::BTreeSet;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sqldiff_convert::Conversion;
use sqldiff_core::Dialect;
use sqldiff_source::compose::Placeholders;

use crate::server::{
    allowed_hosts, bad, json, not_found, read_request, respond, Request, Response,
};

/// The convert page; the payload is injected at `/*__SQLDIFF_PAYLOAD__*/null`.
pub const CONVERT_HTML: &str = include_str!("../assets/convert.html");

/// Shown when no file is given.
pub const SAMPLE: &str = "\
-- Type or paste T-SQL here; the PostgreSQL version on the right follows as you type.
CREATE TABLE dbo.Stock (
    ItemId INT IDENTITY(1,1) NOT NULL PRIMARY KEY,
    [Name] NVARCHAR(100) NOT NULL,
    Qty INT NOT NULL DEFAULT 0,
    UpdatedAt DATETIME2 NOT NULL DEFAULT SYSUTCDATETIME()
);
GO
CREATE PROCEDURE dbo.MoveStock @Item int, @Qty int AS
BEGIN
  SET NOCOUNT ON;
  SET XACT_ABORT ON;
  DECLARE @name NVARCHAR(100);
  SELECT @name = [Name] FROM dbo.Stock WHERE ItemId = @Item;
  IF @name IS NULL
    THROW 50001, 'Unknown item', 1;
  BEGIN TRY
    BEGIN TRAN;
    UPDATE dbo.Stock SET Qty = Qty - @Qty, UpdatedAt = GETDATE() WHERE ItemId = @Item;
    INSERT INTO dbo.StockLog (ItemId, Delta, Note) VALUES (@Item, -@Qty, 'moved ' + @name);
    COMMIT;
  END TRY
  BEGIN CATCH
    IF @@TRANCOUNT > 0 ROLLBACK;
    THROW;
  END CATCH
END
GO
SELECT TOP 10 ItemId, ISNULL([Name], '?') AS Name
FROM dbo.Stock WITH (NOLOCK)
WHERE [Name] LIKE 'A%'
ORDER BY Qty DESC;
";

/// Template input: the pack's placeholder rules and, when given, the
/// parameter names that decide what is a placeholder.
#[derive(Debug, Clone)]
pub struct TemplateSetup {
    pub placeholders: Placeholders,
    pub parameters: Option<BTreeSet<String>>,
}

#[derive(Debug, Clone)]
pub struct Setup {
    pub from: Dialect,
    pub to: Dialect,
    /// Set when the input is a template.
    pub template: Option<TemplateSetup>,
    /// Initial input text.
    pub text: String,
    /// Display name of the input.
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DialectInfo {
    pub id: &'static str,
    pub name: &'static str,
    /// What the page's syntax highlighter needs from the dialect's lexer.
    pub lex: LexInfo,
}

/// The parts of a dialect's [`sqldiff_syntax::dialect::LexicalProfile`] that
/// decide where comments, strings, identifiers and variables start and end,
/// so the editors highlight the way the engine lexes.
#[derive(Debug, Clone, Serialize)]
pub struct LexInfo {
    pub nested_block_comments: bool,
    pub bracket_identifiers: bool,
    pub at_variables: bool,
    pub dollar_quoting: bool,
    pub go_batches: bool,
    pub question_param: bool,
    pub string_prefixes: &'static [&'static str],
    pub extra_ident_start: String,
    pub extra_ident_continue: String,
}

impl LexInfo {
    fn of(d: Dialect) -> Self {
        let p = d.lexical();
        LexInfo {
            nested_block_comments: p.nested_block_comments,
            bracket_identifiers: p.bracket_identifiers,
            at_variables: p.at_variables,
            dollar_quoting: p.dollar_quoting,
            go_batches: p.go_batches,
            question_param: p.question_param,
            string_prefixes: p.string_prefixes,
            extra_ident_start: p.extra_ident_start.iter().collect(),
            extra_ident_continue: p.extra_ident_continue.iter().collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ConvertPayload {
    pub name: String,
    pub input: String,
    /// Dialect ids of this conversion.
    pub from: String,
    pub to: String,
    /// Whether the pair has conversion rules (else every statement comes back
    /// not converted).
    pub rules: bool,
    /// Every dialect, for the source and target pickers.
    pub dialects: Vec<DialectInfo>,
    /// The (from, to) pairs that have conversion rules.
    pub with_rules: Vec<(&'static str, &'static str)>,
    /// Words the editors style as keywords (upper case).
    pub keywords: Vec<&'static str>,
    pub conversion: Option<Conversion>,
    pub error: Option<String>,
    /// Conversion plus checking time, milliseconds.
    pub ms: f64,
    /// Whether a server is behind the page.
    pub live: bool,
    /// Whether the server also has the pair view (compare files) at `/`.
    pub pair_view: bool,
}

/// The engine's diff of two scratch texts (compare mode).
#[derive(Debug, Clone, Serialize)]
pub struct ComparePayload {
    pub report: sqldiff_core::Report,
    pub ms: f64,
}

/// Convert and check `text`, timing it.
pub fn run(setup: &Setup, text: &str, live: bool) -> ConvertPayload {
    run_for(setup, text, setup.from, setup.to, live)
}

/// [`run`] for another source or target dialect.
pub fn run_for(
    setup: &Setup,
    text: &str,
    from: Dialect,
    to: Dialect,
    live: bool,
) -> ConvertPayload {
    let t = Instant::now();
    let r = match &setup.template {
        Some(ts) => {
            let (t, basis) = ts.placeholders.parse(text, from, ts.parameters.as_ref());
            let delimiters = ts.placeholders.delimiters();
            sqldiff_convert::convert_template(&t, from, to, basis, delimiters)
        }
        None => sqldiff_convert::convert(text, from, to),
    };
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    let (conversion, error) = match r {
        Ok(c) => (Some(c), None),
        Err(e) => (None, Some(e)),
    };
    ConvertPayload {
        name: setup.name.clone(),
        input: text.to_string(),
        from: from.id().to_string(),
        to: to.id().to_string(),
        rules: sqldiff_convert::supported(from, to),
        dialects: Dialect::ALL
            .iter()
            .map(|d| DialectInfo {
                id: d.id(),
                name: d.display_name(),
                lex: LexInfo::of(*d),
            })
            .collect(),
        with_rules: Dialect::ALL
            .iter()
            .flat_map(|&f| Dialect::ALL.iter().map(move |&t| (f, t)))
            .filter(|&(f, t)| sqldiff_convert::supported(f, t))
            .map(|(f, t)| (f.id(), t.id()))
            .collect(),
        keywords: crate::styled_keywords(),
        conversion,
        error,
        ms,
        live,
        pair_view: false,
    }
}

/// Diff two scratch texts with the engine, timing it.
pub fn compare(a: &str, b: &str, from: Dialect, to: Dialect) -> ComparePayload {
    let t = Instant::now();
    let a = sqldiff_core::Artifact::new("left.sql", from, a);
    let b = sqldiff_core::Artifact::new("right.sql", to, b);
    let report = sqldiff_core::diff(&a, &b, &sqldiff_core::Options::default());
    ComparePayload {
        report,
        ms: t.elapsed().as_secs_f64() * 1000.0,
    }
}

/// The page with a payload embedded.
pub fn page(payload: &ConvertPayload) -> String {
    let json = serde_json::to_string(payload)
        .unwrap()
        .replace("</", "<\\/");
    CONVERT_HTML.replace(
        "/*__SQLDIFF_PAYLOAD__*/null",
        &format!("/*__SQLDIFF_PAYLOAD__*/{json}"),
    )
}

#[derive(Deserialize)]
struct Edit {
    text: String,
    /// Dialect ids; the setup's when absent.
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
}

#[derive(Deserialize)]
struct Compare {
    a: String,
    b: String,
    from: String,
    to: String,
}

fn dialect(id: Option<&str>, default: Dialect) -> Result<Dialect, Response> {
    match id {
        None => Ok(default),
        Some(id) => id
            .parse()
            .map_err(|_| bad(format!("unknown dialect `{id}`"))),
    }
}

/// The convert page and its API, shared by this server (page at `/`) and the
/// pair view's (page at `/convert`, `pair_view` set).
pub(crate) fn route(setup: &Setup, req: &Request, page_at: &str) -> Option<Response> {
    let pair_view = page_at != "/";
    Some(match (req.method.as_str(), req.path.as_str()) {
        ("GET", p) if p == page_at => {
            let mut payload = run(setup, &setup.text, true);
            payload.pair_view = pair_view;
            ("200 OK", "text/html; charset=utf-8", page(&payload))
        }
        ("POST", "/api/convert") => match serde_json::from_slice::<Edit>(&req.body) {
            Ok(e) => {
                let pick = dialect(e.from.as_deref(), setup.from)
                    .and_then(|f| dialect(e.to.as_deref(), setup.to).map(|t| (f, t)));
                match pick {
                    Ok((from, to)) => {
                        let mut payload = run_for(setup, &e.text, from, to, true);
                        payload.pair_view = pair_view;
                        json(&payload)
                    }
                    Err(r) => r,
                }
            }
            Err(e) => bad(e),
        },
        ("POST", "/api/compare") => match serde_json::from_slice::<Compare>(&req.body) {
            Ok(c) => match (c.from.parse(), c.to.parse()) {
                (Ok(from), Ok(to)) => json(&compare(&c.a, &c.b, from, to)),
                _ => bad("unknown dialect"),
            },
            Err(e) => bad(e),
        },
        _ => return None,
    })
}

pub fn serve(listener: TcpListener, setup: Setup, extra_hosts: &[String]) -> std::io::Result<()> {
    let setup = Arc::new(setup);
    let hosts: Arc<Vec<String>> = Arc::new(allowed_hosts(&listener, extra_hosts)?);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let setup = Arc::clone(&setup);
        let hosts = Arc::clone(&hosts);
        std::thread::spawn(move || {
            let _ = handle(stream, &setup, &hosts);
        });
    }
    Ok(())
}

fn handle(mut stream: TcpStream, setup: &Setup, hosts: &[String]) -> std::io::Result<()> {
    let req = match read_request(&stream, hosts)? {
        Ok(r) => r,
        Err(res) => return respond(&mut stream, res),
    };
    let res = route(setup, &req, "/").unwrap_or_else(not_found);
    respond(&mut stream, res)
}
