//! FxHash-style hashing for *internal*, pointer/integer-keyed maps.
//!
//! `std`'s default SipHash is DoS-resistant but costs ~10x an FxHash for
//! the 8-byte keys our internal registries use (object ids, `PyObject*`
//! addresses, `Rc` data pointers). None of these maps are keyed by
//! attacker-controlled data — the keys are heap addresses or VM-assigned
//! ids — so collision-flooding resistance buys nothing here, while the
//! lookups sit on the hottest paths in the runtime (every object drop
//! consults the GC index; every C crossing consults the mirror/type
//! registries). Profiling pandas workloads showed SipHash itself as a
//! top-ten CPU consumer before this switch.
//!
//! The algorithm matches the `fxhash` crate (the rustc hasher): a
//! multiply-xor fold per word. Not cryptographic; do not use for
//! user-visible `hash()` semantics.

use std::hash::{BuildHasher, Hasher};

const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

/// One-shot word-folding hasher. See module docs.
#[derive(Default, Debug, Clone, Copy)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    #[inline]
    fn fold(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let (chunks, rem) = bytes.as_chunks::<8>();
        for c in chunks {
            self.fold(u64::from_ne_bytes(*c));
        }
        if !rem.is_empty() {
            let mut tail = [0u8; 8];
            tail[..rem.len()].copy_from_slice(rem);
            self.fold(u64::from_ne_bytes(tail));
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.fold(u64::from(i));
    }
    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.fold(u64::from(i));
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.fold(u64::from(i));
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.fold(i);
    }
    #[inline]
    fn write_u128(&mut self, i: u128) {
        self.fold(i as u64);
        self.fold((i >> 64) as u64);
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.fold(i as u64);
    }
    #[inline]
    fn write_i64(&mut self, i: i64) {
        self.fold(i as u64);
    }
    #[inline]
    fn write_isize(&mut self, i: isize) {
        self.fold(i as u64);
    }
}

/// `BuildHasher` for [`FxHasher`] — usable as the `S` parameter of
/// `HashMap`/`HashSet` statics (it is `Default` + `Clone`).
#[derive(Default, Debug, Clone, Copy)]
pub struct FxBuildHasher;

impl BuildHasher for FxBuildHasher {
    type Hasher = FxHasher;
    #[inline]
    fn build_hasher(&self) -> FxHasher {
        FxHasher::default()
    }
}

/// `HashMap` with the internal fast hasher.
pub type FxHashMap<K, V> = std::collections::HashMap<K, V, FxBuildHasher>;
/// `HashSet` with the internal fast hasher.
pub type FxHashSet<K> = std::collections::HashSet<K, FxBuildHasher>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinct_keys_hash_apart() {
        let bh = FxBuildHasher;
        let a = bh.hash_one(0x1000_0000usize);
        let b = bh.hash_one(0x1000_0008usize);
        assert_ne!(a, b);
    }

    #[test]
    fn map_roundtrip() {
        let mut m: FxHashMap<usize, u32> = FxHashMap::default();
        for i in 0..1000usize {
            m.insert(i * 16, i as u32);
        }
        for i in 0..1000usize {
            assert_eq!(m.get(&(i * 16)), Some(&(i as u32)));
        }
    }
}
