//! Texts many units carry the same of: a note's rule id and its text. A
//! tree's notes hold a few hundred distinct texts half a million times
//! over, so each is kept once and shared ([`Pool`]). A [`Shared`] reads,
//! compares and serializes as the string it holds.

use std::collections::HashSet;
use std::fmt;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Conversion;

/// A string that equal copies share once pooled.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Shared(Arc<str>);

impl Shared {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for Shared {
    fn default() -> Shared {
        Shared(Arc::from(""))
    }
}

impl Deref for Shared {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Shared {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for Shared {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.0, f)
    }
}

impl fmt::Display for Shared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&*self.0, f)
    }
}

impl From<String> for Shared {
    fn from(s: String) -> Shared {
        Shared(Arc::from(s))
    }
}

impl From<&str> for Shared {
    fn from(s: &str) -> Shared {
        Shared(Arc::from(s))
    }
}

impl From<&String> for Shared {
    fn from(s: &String) -> Shared {
        Shared(Arc::from(s.as_str()))
    }
}

impl From<Shared> for String {
    fn from(s: Shared) -> String {
        s.0.to_string()
    }
}

impl PartialEq<str> for Shared {
    fn eq(&self, o: &str) -> bool {
        *self.0 == *o
    }
}

impl PartialEq<&str> for Shared {
    fn eq(&self, o: &&str) -> bool {
        *self.0 == **o
    }
}

impl PartialEq<String> for Shared {
    fn eq(&self, o: &String) -> bool {
        *self.0 == **o
    }
}

impl PartialEq<Shared> for String {
    fn eq(&self, o: &Shared) -> bool {
        **self == *o.0
    }
}

impl PartialEq<Shared> for str {
    fn eq(&self, o: &Shared) -> bool {
        *self == *o.0
    }
}

impl PartialEq<Shared> for &str {
    fn eq(&self, o: &Shared) -> bool {
        **self == *o.0
    }
}

impl Serialize for Shared {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Shared {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Shared, D::Error> {
        String::deserialize(d).map(Shared::from)
    }
}

/// Equal texts, kept once. One pool serves one conversion of a tree and
/// is dropped with it: nothing outlives the texts in use.
#[derive(Default)]
pub struct Pool(Mutex<HashSet<Arc<str>>>);

impl Pool {
    /// `s`, as the copy the pool keeps of its text.
    pub fn share(&self, s: &mut Shared) {
        let mut set = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match set.get(&*s.0) {
            Some(kept) => s.0 = Arc::clone(kept),
            None => {
                set.insert(Arc::clone(&s.0));
            }
        }
    }

    /// Every note of `c`'s units, shared.
    pub fn share_notes(&self, c: &mut Conversion) {
        for u in &mut c.units {
            for n in &mut u.notes {
                self.share(&mut n.rule);
                self.share(&mut n.text);
            }
        }
    }
}
