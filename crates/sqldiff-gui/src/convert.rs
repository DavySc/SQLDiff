//! Convert mode: type T-SQL on the left, see checked PostgreSQL on the right.
//!
//! `GET /` serves the page with the initial text converted; `POST /api/convert`
//! with `{"text": "..."}` converts again. Every unit of the output carries a
//! badge from `sqldiff_convert` (rules + the engine's diff of input against
//! output). Nothing is written to disk. Own server (sharing the request handling in
//! `server`) so it can run beside the pair view without sharing its state.

use std::collections::BTreeSet;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sqldiff_convert::Conversion;
use sqldiff_core::Dialect;
use sqldiff_source::compose::Placeholders;

use crate::server::{allowed_hosts, bad, json, not_found, read_request, respond};

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
pub struct ConvertPayload {
    pub name: String,
    pub input: String,
    pub conversion: Option<Conversion>,
    pub error: Option<String>,
    /// Conversion plus checking time, milliseconds.
    pub ms: f64,
    /// Whether a server is behind the page.
    pub live: bool,
}

/// Convert and check `text`, timing it.
pub fn run(setup: &Setup, text: &str, live: bool) -> ConvertPayload {
    let t = Instant::now();
    let r = match &setup.template {
        Some(ts) => {
            let (t, _) = ts
                .placeholders
                .parse(text, setup.from, ts.parameters.as_ref());
            sqldiff_convert::convert_template(&t, setup.from, setup.to)
        }
        None => sqldiff_convert::convert(text, setup.from, setup.to),
    };
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    let (conversion, error) = match r {
        Ok(c) => (Some(c), None),
        Err(e) => (None, Some(e)),
    };
    ConvertPayload {
        name: setup.name.clone(),
        input: text.to_string(),
        conversion,
        error,
        ms,
        live,
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
    let res = match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => (
            "200 OK",
            "text/html; charset=utf-8",
            page(&run(setup, &setup.text, true)),
        ),
        ("POST", "/api/convert") => match serde_json::from_slice::<Edit>(&req.body) {
            Ok(e) => json(&run(setup, &e.text, true)),
            Err(e) => bad(e),
        },
        _ => not_found(),
    };
    respond(&mut stream, res)
}
