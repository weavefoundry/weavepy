//! Interned identifiers for code objects' name tables.
//!
//! A module's code objects repeat the same few identifiers (`self`,
//! `__name__`, `len`, a class's attribute names) across their `co_names`,
//! `co_varnames`, `co_freevars` and `co_cellvars`: importing `asyncio`
//! decodes about 38,000 entries, of which about 6,300 are distinct. A
//! [`Name`] is a shared, immutable string, and every name the compiler or
//! the code cache produces goes through one process-wide pool, so equal
//! identifiers share one allocation (as CPython interns them).
//!
//! The pool holds only names some code object still uses: when it has
//! doubled since it was last swept, the names no one else holds are
//! dropped from it.

use std::borrow::Borrow;
use std::collections::HashSet;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::sync::{Arc, Mutex};

/// An identifier in a code object's name tables (see the module docs).
/// It reads as a `str`.
#[derive(Clone)]
pub struct Name(Arc<str>);

impl Name {
    /// The pooled name equal to `s`.
    pub fn new(s: &str) -> Name {
        POOL.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .intern(s)
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `a` and `b` share one allocation (pooled names always do
    /// when equal).
    #[inline]
    pub fn ptr_eq(a: &Name, b: &Name) -> bool {
        Arc::ptr_eq(&a.0, &b.0)
    }
}

/// The pool's hasher: FNV-1a, which is quick over short identifiers.
struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut h = self.0;
        for &b in bytes {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
        self.0 = h;
    }
}

struct Pool {
    names: Option<HashSet<Arc<str>, BuildHasherDefault<Fnv>>>,
    /// The size past which the next insertion sweeps unused names.
    sweep_at: usize,
}

static POOL: Mutex<Pool> = Mutex::new(Pool {
    names: None,
    sweep_at: 4096,
});

impl Pool {
    fn intern(&mut self, s: &str) -> Name {
        let names = self.names.get_or_insert_with(HashSet::default);
        if let Some(name) = names.get(s) {
            return Name(name.clone());
        }
        if names.len() >= self.sweep_at {
            names.retain(|name| Arc::strong_count(name) > 1);
            self.sweep_at = (names.len() * 2).max(4096);
        }
        let name: Arc<str> = Arc::from(s);
        names.insert(name.clone());
        Name(name)
    }
}

impl Default for Name {
    fn default() -> Self {
        Name::new("")
    }
}

impl std::ops::Deref for Name {
    type Target = str;

    #[inline]
    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Name {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for Name {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl PartialEq for Name {
    #[inline]
    fn eq(&self, other: &Name) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0
    }
}

impl Eq for Name {}

impl PartialEq<str> for Name {
    fn eq(&self, other: &str) -> bool {
        &*self.0 == other
    }
}

impl PartialEq<&str> for Name {
    fn eq(&self, other: &&str) -> bool {
        &*self.0 == *other
    }
}

impl PartialEq<String> for Name {
    fn eq(&self, other: &String) -> bool {
        *self.0 == **other
    }
}

impl PartialEq<Name> for str {
    fn eq(&self, other: &Name) -> bool {
        self == &*other.0
    }
}

impl PartialEq<Name> for &str {
    fn eq(&self, other: &Name) -> bool {
        *self == &*other.0
    }
}

impl PartialEq<Name> for String {
    fn eq(&self, other: &Name) -> bool {
        **self == *other.0
    }
}

impl PartialOrd for Name {
    fn partial_cmp(&self, other: &Name) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Name {
    fn cmp(&self, other: &Name) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl Hash for Name {
    // As a `str`, which `Borrow<str>` requires.
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl std::fmt::Debug for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        (*self.0).fmt(f)
    }
}

impl std::fmt::Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        (*self.0).fmt(f)
    }
}

impl From<&str> for Name {
    fn from(s: &str) -> Name {
        Name::new(s)
    }
}

impl From<String> for Name {
    fn from(s: String) -> Name {
        Name::new(&s)
    }
}

impl From<&String> for Name {
    fn from(s: &String) -> Name {
        Name::new(s)
    }
}

impl From<&Name> for Name {
    fn from(s: &Name) -> Name {
        s.clone()
    }
}

impl From<Name> for String {
    fn from(s: Name) -> String {
        String::from(&*s.0)
    }
}

impl From<&Name> for String {
    fn from(s: &Name) -> String {
        String::from(&*s.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_names_share_one_allocation() {
        let a = Name::new("pooled_name_a");
        let b = Name::from(String::from("pooled_name_a"));
        assert!(Name::ptr_eq(&a, &b));
        assert_eq!(a, "pooled_name_a");
        assert_eq!(a.as_str(), "pooled_name_a");
        assert_ne!(a, Name::new("pooled_name_b"));
    }

    #[test]
    fn sweeping_keeps_names_in_use() {
        let kept = Name::new("kept_through_sweeps");
        for i in 0..20_000 {
            let _ = Name::new(&format!("transient_{i}"));
        }
        let again = Name::new("kept_through_sweeps");
        assert!(Name::ptr_eq(&kept, &again));
        let pool = POOL.lock().unwrap();
        assert!(pool.names.as_ref().unwrap().len() < 20_000);
    }
}
