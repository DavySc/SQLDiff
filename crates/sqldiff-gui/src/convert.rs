//! Convert mode: type T-SQL on the left, see checked PostgreSQL on the right.
//!
//! `GET /` serves the page with the initial text converted; `POST /api/convert`
//! with `{"text": "..."}` converts again. Every unit of the output carries a
//! badge from `sqldiff_convert` (rules + the engine's diff of input against
//! output). Nothing is written to disk. Own server so it can run beside the
//! pair view without sharing its state.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sqldiff_convert::Conversion;
use sqldiff_core::Dialect;

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

#[derive(Debug, Clone)]
pub struct Setup {
    pub from: Dialect,
    pub to: Dialect,
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
    let r = sqldiff_convert::convert(text, setup.from, setup.to);
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

/// Largest request body accepted.
const MAX_BODY: usize = 16 * 1024 * 1024;

pub fn serve(listener: TcpListener, setup: Setup) -> std::io::Result<()> {
    let setup = Arc::new(setup);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let setup = Arc::clone(&setup);
        std::thread::spawn(move || {
            let _ = handle(stream, &setup);
        });
    }
    Ok(())
}

fn handle(mut stream: TcpStream, setup: &Setup) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let path = path.split('?').next().unwrap_or("");
    let (status, ctype, body) = match (method.as_str(), path) {
        ("GET", "/") => (
            "200 OK",
            "text/html; charset=utf-8",
            page(&run(setup, &setup.text, true)),
        ),
        ("POST", "/api/convert") if content_length <= MAX_BODY => {
            let mut buf = vec![0; content_length];
            reader.read_exact(&mut buf)?;
            match serde_json::from_slice::<Edit>(&buf) {
                Ok(e) => (
                    "200 OK",
                    "application/json",
                    serde_json::to_string(&run(setup, &e.text, true)).unwrap(),
                ),
                Err(e) => ("400 Bad Request", "text/plain", e.to_string()),
            }
        }
        ("POST", "/api/convert") => ("413 Payload Too Large", "text/plain", String::new()),
        _ => ("404 Not Found", "text/plain", "not found".to_string()),
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}
