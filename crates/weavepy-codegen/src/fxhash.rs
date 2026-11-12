//! A fast hash for the small integer keys the code generator and the JIT's
//! analysis map (values, blocks, pcs, slots): the Firefox/rustc multiply-
//! rotate hash, a few instructions a word where the standard library's
//! SipHash takes dozens. Nothing here hashes untrusted input.

use std::hash::{BuildHasherDefault, Hasher};

/// The hasher state.
#[derive(Debug, Default, Clone, Copy)]
pub struct FxHasher {
    hash: u64,
}

const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.add(u64::from_le_bytes(word));
        }
    }

    #[inline]
    fn write_u8(&mut self, n: u8) {
        self.add(u64::from(n));
    }

    #[inline]
    fn write_u16(&mut self, n: u16) {
        self.add(u64::from(n));
    }

    #[inline]
    fn write_u32(&mut self, n: u32) {
        self.add(u64::from(n));
    }

    #[inline]
    fn write_u64(&mut self, n: u64) {
        self.add(n);
    }

    #[inline]
    fn write_usize(&mut self, n: usize) {
        self.add(n as u64);
    }

    #[inline]
    fn write_i64(&mut self, n: i64) {
        self.add(n as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

/// [`FxHasher`]'s builder.
pub type FxBuildHasher = BuildHasherDefault<FxHasher>;
/// A `HashMap` with [`FxHasher`].
pub type FxHashMap<K, V> = std::collections::HashMap<K, V, FxBuildHasher>;
/// A `HashSet` with [`FxHasher`].
pub type FxHashSet<K> = std::collections::HashSet<K, FxBuildHasher>;
