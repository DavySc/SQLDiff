//! `sqldiff-mcp`: SQLDiff as a local stdio MCP server.
//!
//! Reads newline-delimited JSON-RPC from stdin and writes responses to
//! stdout. Logs go to stderr.

use std::io::{BufRead, Write};

fn main() {
    let mut server = sqldiff_mcp::Server::new();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("sqldiff-mcp: {e}");
                break;
            }
        };
        if let Some(out) = server.handle_line(&line) {
            if writeln!(stdout, "{out}")
                .and_then(|_| stdout.flush())
                .is_err()
            {
                break;
            }
        }
    }
}
