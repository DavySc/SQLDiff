//! Optional encryption at rest for the review store and the cache
//! (design section 5.5).
//!
//! The key is named by reference, like adapter credentials: `env:NAME`
//! (the variable holds the key) or `file:PATH` (the file holds it). The
//! secret should be random, for example 32 bytes of hex; it is hashed to a
//! 256-bit key. Records are sealed with XChaCha20-Poly1305 and a random
//! nonce, as `enc1:` followed by hex of nonce and ciphertext.

use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha256};
use sqldiff_core::model::hex;
use sqldiff_source::{Error, Result};

const PREFIX: &str = "enc1:";
const NONCE: usize = 24;

#[derive(Clone)]
pub struct DataKey {
    cipher: XChaCha20Poly1305,
}

impl std::fmt::Debug for DataKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DataKey(..)")
    }
}

impl DataKey {
    /// Resolves `env:NAME` or `file:PATH`.
    pub fn from_ref(r: &str) -> Result<DataKey> {
        let secret = match r.split_once(':') {
            Some(("env", name)) if !name.is_empty() => std::env::var(name)
                .map_err(|_| Error::new(format!("data key `{r}`: variable {name} is not set")))?,
            Some(("file", path)) if !path.is_empty() => std::fs::read_to_string(path)
                .map_err(|e| Error::new(format!("data key `{r}`: {e}")))?,
            _ => {
                return Err(Error::new(format!(
                    "data key `{r}`: expected env:NAME or file:PATH"
                )))
            }
        };
        let secret = secret.trim();
        if secret.len() < 16 {
            return Err(Error::new(format!(
                "data key `{r}`: the key must be at least 16 characters"
            )));
        }
        Ok(DataKey::from_secret(secret.as_bytes()))
    }

    pub fn from_secret(secret: &[u8]) -> DataKey {
        let mut h = Sha256::new();
        h.update(b"sqldiff-data-key-v1\0");
        h.update(secret);
        let key = h.finalize();
        DataKey {
            cipher: XChaCha20Poly1305::new(&key),
        }
    }

    pub fn seal(&self, plain: &[u8]) -> String {
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ct = self
            .cipher
            .encrypt(&nonce, plain)
            .expect("encryption does not fail");
        format!("{PREFIX}{}{}", hex(&nonce), hex(&ct))
    }

    pub fn open(&self, sealed: &str) -> Result<Vec<u8>> {
        let bytes = sealed
            .trim()
            .strip_prefix(PREFIX)
            .and_then(unhex)
            .filter(|b| b.len() > NONCE)
            .ok_or_else(|| Error::new("not an encrypted record"))?;
        let (nonce, ct) = bytes.split_at(NONCE);
        self.cipher
            .decrypt(XNonce::from_slice(nonce), ct)
            .map_err(|_| Error::new("cannot decrypt record: wrong data key or corrupt data"))
    }
}

pub fn is_sealed(s: &str) -> bool {
    s.trim_start().starts_with(PREFIX)
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

/// Seals `plain` when a key is given, else returns it as UTF-8 text.
pub(crate) fn encode(key: Option<&DataKey>, plain: &str) -> String {
    match key {
        Some(k) => k.seal(plain.as_bytes()),
        None => plain.to_string(),
    }
}

/// Opens a record written by [`encode`]. Plain records need no key;
/// sealed records need one.
pub(crate) fn decode(key: Option<&DataKey>, record: &str) -> Result<String> {
    if !is_sealed(record) {
        return Ok(record.to_string());
    }
    let k = key.ok_or_else(|| Error::new("encrypted data and no data key configured"))?;
    String::from_utf8(k.open(record)?).map_err(|_| Error::new("decrypted record is not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open() {
        let k = DataKey::from_secret(b"0123456789abcdef0123456789abcdef");
        let s = k.seal(b"hello");
        assert!(is_sealed(&s));
        assert!(!s.contains("hello"));
        assert_eq!(k.open(&s).unwrap(), b"hello");
        let other = DataKey::from_secret(b"another key entirely, 32 chars!!");
        assert!(other.open(&s).is_err());
        assert_ne!(k.seal(b"hello"), s, "nonces are random");
    }

    #[test]
    fn refs() {
        assert!(DataKey::from_ref("vault:x").is_err());
        std::env::set_var("SQLDIFF_TEST_KEY_SHORT", "short");
        assert!(DataKey::from_ref("env:SQLDIFF_TEST_KEY_SHORT").is_err());
        std::env::set_var("SQLDIFF_TEST_KEY_OK", "0123456789abcdef0123");
        assert!(DataKey::from_ref("env:SQLDIFF_TEST_KEY_OK").is_ok());
    }
}
