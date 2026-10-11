//! A fast, unkeyed hasher for the compiler's name sets and binding maps.
//!
//! Scope analysis builds many small sets of identifiers per code object.
//! The standard library's keyed SipHash costs several times more per short
//! string than this multiply-rotate hash (rustc's `FxHasher`), and the
//! compiler never hashes attacker-chosen keys at a scale where hash
//! flooding matters: a source's names are bounded by its own size.

use std::hash::{BuildHasherDefault, Hasher};

/// rustc's `FxHasher`: one rotate, xor and multiply per word.
#[derive(Default, Clone, Copy)]
pub(crate) struct FxHasher {
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
    fn write(&mut self, mut bytes: &[u8]) {
        while let Some((head, rest)) = bytes.split_first_chunk::<8>() {
            self.add(u64::from_le_bytes(*head));
            bytes = rest;
        }
        if let Some((head, rest)) = bytes.split_first_chunk::<4>() {
            self.add(u64::from(u32::from_le_bytes(*head)));
            bytes = rest;
        }
        if let Some((head, rest)) = bytes.split_first_chunk::<2>() {
            self.add(u64::from(u16::from_le_bytes(*head)));
            bytes = rest;
        }
        if let Some(&b) = bytes.first() {
            self.add(u64::from(b));
        }
    }

    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.add(u64::from(i));
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

pub(crate) type FxBuild = BuildHasherDefault<FxHasher>;

/// A `HashSet` over [`FxHasher`]; build one with `HashSet::default()`.
pub(crate) type HashSet<T> = std::collections::HashSet<T, FxBuild>;

/// A `HashMap` over [`FxHasher`]; build one with `HashMap::default()`.
pub(crate) type HashMap<K, V> = std::collections::HashMap<K, V, FxBuild>;

/// An `IndexMap` over [`FxHasher`]; build one with `IndexMap::default()`.
pub(crate) type IndexMap<K, V> = indexmap::IndexMap<K, V, FxBuild>;
