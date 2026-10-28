//! `sqldiff-check` as a subprocess (decision 2): started once per run on
//! first use, kept for every pair, one request line per comparison. Every
//! failure, from a missing binary to a timeout or an answer that does not
//! parse, is "unconfirmed", and a failed process is not reused.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Mutex;
use std::time::Duration;

use sqldiff_core::proof::{Answer, Checker, MirrorAnswer, MirrorRequest, Request, MIRROR_PROTOCOL};

/// How long one answer may take.
const TIMEOUT: Duration = Duration::from_secs(10);

pub struct Subprocess {
    path: PathBuf,
    name: String,
    next: AtomicU64,
    conn: Mutex<Option<Conn>>,
}

struct Conn {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Drop for Conn {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The name and version (`--version`) plus a hash of the binary, so a run
/// fingerprint changes when the checker does.
impl std::fmt::Debug for Subprocess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Subprocess({})", self.name)
    }
}

/// Where the checker is: `$SQLDIFF_CHECK`, else beside the running
/// executable (`sqldiff-check`, or `sqldiff_check` as Lake names it).
pub fn find() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SQLDIFF_CHECK") {
        return Some(PathBuf::from(p));
    }
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    ["sqldiff-check", "sqldiff_check"]
        .iter()
        .map(|n| dir.join(format!("{n}{}", std::env::consts::EXE_SUFFIX)))
        .find(|p| p.is_file())
}

fn binary_hash(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(h.finalize()[..6]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

impl Subprocess {
    pub fn new(path: PathBuf) -> Result<Self, String> {
        let out = Command::new(&path)
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.status.success() || !version.contains(sqldiff_core::proof::PROTOCOL) {
            return Err(format!(
                "{}: not a checker for {} (`--version` said `{version}`)",
                path.display(),
                sqldiff_core::proof::PROTOCOL
            ));
        }
        let name = format!("{version} sha256:{}", binary_hash(&path)?);
        Ok(Subprocess {
            path,
            name,
            next: AtomicU64::new(1),
            conn: Mutex::new(None),
        })
    }

    fn start(&self) -> Result<Conn, String> {
        let mut child = Command::new(&self.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("starting {}: {e}", self.path.display()))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Conn {
            child,
            stdin,
            lines,
        })
    }

    fn ask(&self, line: impl Fn(u64) -> Option<String>) -> Result<MirrorAnswer, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let line = line(id).ok_or("a field contains a tab or line break")?;
        let mut guard = self.conn.lock().map_err(|_| "checker lock poisoned")?;
        if guard.is_none() {
            *guard = Some(self.start()?);
        }
        let conn = guard.as_mut().expect("started above");
        let sent = writeln!(conn.stdin, "{line}").and_then(|_| conn.stdin.flush());
        let reply = sent
            .map_err(|e| format!("writing to the checker: {e}"))
            .and_then(|_| {
                conn.lines
                    .recv_timeout(TIMEOUT)
                    .map_err(|_| format!("no answer within {}s", TIMEOUT.as_secs()))
            })
            .and_then(|reply| parse(&reply, id));
        if reply.is_err() {
            // A checker that failed once is not trusted with the next request.
            *guard = None;
        }
        reply
    }
}

/// The answer to request `id`, with the mirror check's extra fields when
/// it has them; anything else is an error.
fn parse(reply: &str, id: u64) -> Result<MirrorAnswer, String> {
    let v: serde_json::Value =
        serde_json::from_str(reply).map_err(|e| format!("unreadable answer: {e}"))?;
    if v["id"].as_str() != Some(id.to_string().as_str()) {
        return Err(format!("answer for another request: {reply}"));
    }
    let answer = match (
        v["proof"].as_str(),
        v["theorem"].as_str(),
        v["reason"].as_str(),
    ) {
        (Some("confirmed"), Some(t), _) => Answer::Confirmed {
            theorem: t.to_string(),
        },
        (Some("unconfirmed"), _, Some(r)) => Answer::Unconfirmed {
            reason: r.to_string(),
        },
        _ => return Err(format!("unexpected answer: {reply}")),
    };
    let residual = match &v["residual"] {
        serde_json::Value::Null => Vec::new(),
        serde_json::Value::Array(xs) => xs
            .iter()
            .map(|x| x.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| format!("unexpected residual: {reply}"))?,
        _ => return Err(format!("unexpected residual: {reply}")),
    };
    Ok(MirrorAnswer {
        answer,
        scalars: v["scalars"].as_str().map(str::to_string),
        residual,
        statements: v["statements"].as_str().map(str::to_string),
    })
}

impl Checker for Subprocess {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn check(&self, req: &Request) -> Answer {
        self.ask(|id| req.line(id))
            .map(|a| a.answer)
            .unwrap_or_else(|reason| Answer::Unconfirmed {
                reason: format!("checker failed: {reason}"),
            })
    }

    fn check_mirror(&self, req: &MirrorRequest) -> MirrorAnswer {
        if !self.name.contains(MIRROR_PROTOCOL) {
            return MirrorAnswer::from(Answer::Unconfirmed {
                reason: format!("the checker does not speak {MIRROR_PROTOCOL}"),
            });
        }
        self.ask(|id| req.line(id)).unwrap_or_else(|reason| {
            MirrorAnswer::from(Answer::Unconfirmed {
                reason: format!("checker failed: {reason}"),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_matching_well_formed_answer_counts() {
        assert_eq!(
            parse(r#"{"id":"3","proof":"confirmed","theorem":"T"}"#, 3).map(|a| a.answer),
            Ok(Answer::Confirmed {
                theorem: "T".into()
            })
        );
        assert!(matches!(
            parse(r#"{"id":"3","proof":"unconfirmed","reason":"r"}"#, 3).map(|a| a.answer),
            Ok(Answer::Unconfirmed { .. })
        ));
        let m = parse(
            r#"{"id":"3","proof":"confirmed","theorem":"T","scalars":"c2a","residual":["ASM-X"],"statements":"c3a"}"#,
            3,
        )
        .unwrap();
        assert_eq!(m.scalars.as_deref(), Some("c2a"));
        assert_eq!(m.statements.as_deref(), Some("c3a"));
        assert_eq!(m.residual, ["ASM-X"]);
        for bad in [
            r#"{"id":"4","proof":"confirmed","theorem":"T"}"#,
            r#"{"id":"3","proof":"confirmed"}"#,
            r#"{"id":"3","proof":"yes","theorem":"T"}"#,
            r#"{"id":3,"proof":"confirmed","theorem":"T"}"#,
            r#"{"id":"3","proof":"confirmed","theorem":"T","residual":[1]}"#,
            "confirmed",
        ] {
            assert!(parse(bad, 3).is_err(), "{bad}");
        }
    }
}
