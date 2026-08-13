//! Insert-only atomic bloom pre-filters — RFC 0065 (WS4).
//!
//! Three registries (the weakref registry, the GC tracked-object
//! index, the prompt-reap suspect map) sit behind `GilCell`/mutex
//! borrows but are consulted on *usually-miss* probes from the
//! interpreter's drop paths: "is anything watching this object?",
//! "is this object tracked?", "is this id enrolled as a suspect?".
//! RFC 0061 measured the borrow machinery of those misses at ~5%
//! of drop-heavy fixtures. An [`AtomicBloom`] answers the miss with
//! two relaxed loads, no lock, no TLS.
//!
//! # Discipline
//!
//! - **Insert-only.** Bits are set when an id is registered and never
//!   cleared (registries shrink, the filter saturates). A stale bit is
//!   a false *positive*: the caller falls through to the precise,
//!   locked path it used to take unconditionally — never a correctness
//!   change.
//! - **False negatives are impossible** for registered ids: `insert`
//!   publishes with the same relaxed atomics the readers use, and
//!   every producer inserts *before* the registration becomes
//!   observable to the code that probes (both run under the GIL; for
//!   the rare non-GIL Rust-drop probes, the GIL hand-off's lock is a
//!   full barrier, the same visibility contract as `hot_gates`).
//! - `k = 2` probe bits derived from one 64-bit mix keep the false-
//!   positive rate low until well past 10⁴ distinct ids per filter
//!   (each filter is 8 KiB / 65 536 bits).

use std::sync::atomic::{AtomicU64, Ordering};

const WORDS: usize = 1024; // 65 536 bits

/// An insert-only concurrent bloom filter keyed by object id.
pub struct AtomicBloom {
    bits: [AtomicU64; WORDS],
}

impl std::fmt::Debug for AtomicBloom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let set: u32 = self
            .bits
            .iter()
            .map(|w| w.load(Ordering::Relaxed).count_ones())
            .sum();
        f.debug_struct("AtomicBloom")
            .field("bits_set", &set)
            .field("bits_total", &(WORDS * 64))
            .finish()
    }
}

impl AtomicBloom {
    #[allow(clippy::new_without_default)]
    pub const fn new() -> Self {
        #[allow(clippy::declare_interior_mutable_const)]
        const ZERO: AtomicU64 = AtomicU64::new(0);
        Self {
            bits: [ZERO; WORDS],
        }
    }

    /// Two probe positions from one Fibonacci mix of the id.
    #[inline]
    fn probes(id: u64) -> ((usize, u64), (usize, u64)) {
        let h = id.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let p1 = (h >> 48) as usize & (WORDS * 64 - 1);
        let p2 = (h >> 32) as usize & (WORDS * 64 - 1);
        ((p1 / 64, 1u64 << (p1 % 64)), (p2 / 64, 1u64 << (p2 % 64)))
    }

    /// Mark `id` as possibly-present. Never cleared.
    #[inline]
    pub fn insert(&self, id: u64) {
        let ((w1, b1), (w2, b2)) = Self::probes(id);
        self.bits[w1].fetch_or(b1, Ordering::Relaxed);
        self.bits[w2].fetch_or(b2, Ordering::Relaxed);
    }

    /// `false` means *definitely absent* (for every id that went
    /// through [`insert`](Self::insert)); `true` means "take the
    /// precise path".
    #[inline]
    pub fn may_contain(&self, id: u64) -> bool {
        let ((w1, b1), (w2, b2)) = Self::probes(id);
        self.bits[w1].load(Ordering::Relaxed) & b1 != 0
            && self.bits[w2].load(Ordering::Relaxed) & b2 != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserted_ids_are_found() {
        let f = AtomicBloom::new();
        for id in (0..1000u64).map(|n| n * 0x1000 + 7) {
            f.insert(id);
        }
        for id in (0..1000u64).map(|n| n * 0x1000 + 7) {
            assert!(f.may_contain(id));
        }
    }

    #[test]
    fn fresh_filter_rejects() {
        let f = AtomicBloom::new();
        let mut fp = 0;
        for id in (0..1000u64).map(|n| n * 0x2000 + 13) {
            if f.may_contain(id) {
                fp += 1;
            }
        }
        assert_eq!(fp, 0);
    }
}
