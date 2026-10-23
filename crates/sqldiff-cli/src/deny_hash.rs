//! Hashed deny-list for the leak scan (docs/private-overlay.md).
//!
//! CI holds only salted SHA-256 hashes of the deny-listed terms, so the terms
//! themselves never leave the machine that has the private packs. A scan
//! hashes every substring of each listed length and reports where a hash
//! matches, never which term it was.
//!
//! Format, one entry per line:
//!
//! ```text
//! sqldiff-deny-hashes 1
//! salt <hex>
//! <byte length> <hex SHA-256 of salt + folded term>
//! ```
//!
//! Terms and text are folded with ASCII lower-casing, as `grep -i` does for
//! ASCII; other characters match exactly.

use std::collections::{BTreeSet, HashSet};
use std::hash::{BuildHasher, Hasher};

use sha2::{Digest, Sha256};
use sqldiff_core::model::hex;

const HEADER: &str = "sqldiff-deny-hashes 1";

pub struct DenyHashes {
    salt: Vec<u8>,
    /// Ascending, so a scan can stop at the first length longer than a line.
    lengths: Vec<usize>,
    hashes: HashSet<[u8; 32]>,
}

fn digest(salt: &[u8], folded: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(salt);
    h.update(folded);
    h.finalize().into()
}

/// 128 bits from the standard library's randomly keyed hasher, which is seeded
/// from the operating system. The salt only has to be unique per list, not
/// secret, so this avoids a dependency on a random-number crate.
fn random_salt() -> Vec<u8> {
    let mut salt = Vec::with_capacity(16);
    for i in 0..2u8 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u8(i);
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        salt.extend(h.finish().to_le_bytes());
    }
    salt
}

/// The hashed form of `terms` under a fresh salt. Entries are sorted, so their
/// order says nothing about the list they came from.
pub fn render(terms: &[String]) -> String {
    let salt = random_salt();
    let entries: BTreeSet<(usize, String)> = terms
        .iter()
        .map(|t| t.trim().as_bytes().to_ascii_lowercase())
        .filter(|t| !t.is_empty())
        .map(|t| (t.len(), hex(&digest(&salt, &t))))
        .collect();
    let mut out = format!("{HEADER}\nsalt {}\n", hex(&salt));
    for (len, h) in entries {
        out.push_str(&format!("{len} {h}\n"));
    }
    out
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

impl DenyHashes {
    pub fn parse(text: &str) -> Result<DenyHashes, String> {
        let mut lines = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'));
        if lines.next() != Some(HEADER) {
            return Err(format!(
                "not a hashed deny-list: the first line must be `{HEADER}`"
            ));
        }
        let salt = lines
            .next()
            .and_then(|l| l.strip_prefix("salt "))
            .and_then(|s| unhex(s.trim()))
            .ok_or("hashed deny-list: the second line must be `salt HEX`")?;
        let mut lengths = BTreeSet::new();
        let mut hashes = HashSet::new();
        for (i, l) in lines.enumerate() {
            let bad = || format!("hashed deny-list: entry {} is not `LENGTH HEX`", i + 1);
            let (len, h) = l.split_once(' ').ok_or_else(bad)?;
            let len: usize = len.parse().ok().filter(|&n| n > 0).ok_or_else(bad)?;
            let h: [u8; 32] = unhex(h.trim())
                .and_then(|v| v.try_into().ok())
                .ok_or_else(bad)?;
            lengths.insert(len);
            hashes.insert(h);
        }
        Ok(DenyHashes {
            salt,
            lengths: lengths.into_iter().collect(),
            hashes,
        })
    }

    pub fn len(&self) -> usize {
        self.hashes.len()
    }

    /// One-based numbers of the lines of `text` that contain a listed term.
    pub fn lines_with_terms(&self, text: &[u8]) -> Vec<usize> {
        let folded = text.to_ascii_lowercase();
        folded
            .split(|&b| b == b'\n')
            .enumerate()
            .filter(|(_, line)| self.line_has_term(line))
            .map(|(i, _)| i + 1)
            .collect()
    }

    fn line_has_term(&self, line: &[u8]) -> bool {
        let mut salted = Sha256::new();
        salted.update(&self.salt);
        for &len in &self.lengths {
            if len > line.len() {
                break;
            }
            for w in line.windows(len) {
                let mut h = salted.clone();
                h.update(w);
                if self.hashes.contains(&<[u8; 32]>::from(h.finalize())) {
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(terms: &[&str]) -> DenyHashes {
        let terms: Vec<String> = terms.iter().map(|t| t.to_string()).collect();
        DenyHashes::parse(&render(&terms)).unwrap()
    }

    #[test]
    fn rendered_list_holds_no_term() {
        let terms = vec!["zq-secret-term".to_string(), "Corp Example".to_string()];
        let text = render(&terms);
        assert!(!text.to_ascii_lowercase().contains("zq-secret"));
        assert!(!text.to_ascii_lowercase().contains("corp"));
        assert_eq!(DenyHashes::parse(&text).unwrap().len(), 2);
    }

    #[test]
    fn salts_differ_between_renders() {
        let terms = vec!["zq-secret-term".to_string()];
        assert_ne!(render(&terms), render(&terms));
    }

    #[test]
    fn finds_terms_anywhere_ignoring_ascii_case() {
        let d = list(&["zq-secret", "corp example", "x_"]);
        let text = b"clean line\nprefix ZQ-SECRET-suffix\nnothing\nthe Corp Example ltd\nend";
        assert_eq!(d.lines_with_terms(text), vec![2, 4]);
        assert_eq!(d.lines_with_terms(b"call x_send()\n"), vec![1]);
        assert!(d.lines_with_terms(b"zq-secre\ncorp  example\n").is_empty());
    }

    #[test]
    fn rejects_malformed_lists() {
        assert!(DenyHashes::parse("zq-secret-term\n").is_err());
        assert!(DenyHashes::parse("sqldiff-deny-hashes 1\n").is_err());
        assert!(DenyHashes::parse("sqldiff-deny-hashes 1\nsalt 00ff\n3 abc\n").is_err());
        assert!(DenyHashes::parse("sqldiff-deny-hashes 1\nsalt 00ff\n").is_ok());
    }
}
