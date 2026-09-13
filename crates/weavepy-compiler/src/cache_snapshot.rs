//! Coherent advisory cache snapshots without an inherited lock at fork.

// Constant target selection leaves one implementation unused on each target.
#![allow(dead_code)]

use std::sync::atomic::{fence, AtomicU64, Ordering};

#[derive(Debug, Default)]
pub(crate) struct NativeSnapshot(portable_atomic::AtomicU128);

impl NativeSnapshot {
    pub(crate) const fn new(bits: u128) -> Self {
        Self(portable_atomic::AtomicU128::new(bits))
    }

    #[inline(always)]
    pub(crate) fn load(&self) -> Option<u128> {
        Some(self.0.load(Ordering::Relaxed))
    }

    #[inline(always)]
    pub(crate) fn store(&self, bits: u128) {
        self.0.store(bits, Ordering::Relaxed);
    }
}

/// A contended or interrupted write produces a cache miss, never a wait.
/// Atomic payload words make concurrent speculative reads data-race-free.
/// The checked 64-bit epoch prevents a reader from mistaking a reused epoch
/// for a coherent snapshot, including at exhaustion.
#[derive(Debug, Default)]
pub(crate) struct FallbackSnapshot {
    epoch: AtomicU64,
    low: AtomicU64,
    high: AtomicU64,
}

impl FallbackSnapshot {
    pub(crate) const fn new(bits: u128) -> Self {
        Self {
            epoch: AtomicU64::new(0),
            low: AtomicU64::new(bits as u64),
            high: AtomicU64::new((bits >> 64) as u64),
        }
    }

    #[inline(always)]
    pub(crate) fn load(&self) -> Option<u128> {
        let before = self.epoch.load(Ordering::Acquire);
        if before & 1 != 0 {
            return None;
        }
        let low = self.low.load(Ordering::Relaxed);
        let high = self.high.load(Ordering::Relaxed);
        // If either payload load observes a later writer, this fence pairs
        // with that writer's release fence. Its odd epoch then happens before
        // our final epoch read, so the old `before` cannot validate that data.
        fence(Ordering::Acquire);
        (self.epoch.load(Ordering::Relaxed) == before)
            .then_some(u128::from(low) | (u128::from(high) << 64))
    }

    #[inline(always)]
    pub(crate) fn store(&self, bits: u128) {
        let before = self.epoch.load(Ordering::Relaxed);
        let Some(after) = before.checked_add(2) else {
            return;
        };
        if before & 1 != 0
            || self
                .epoch
                .compare_exchange(before, before + 1, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
        {
            return;
        }
        // Publish the odd epoch before any payload word. Readers observing
        // either relaxed payload store acquire this fence before validating.
        fence(Ordering::Release);
        self.low.store(bits as u64, Ordering::Relaxed);
        self.high.store((bits >> 64) as u64, Ordering::Relaxed);
        self.epoch.store(after, Ordering::Release);
    }
}

#[derive(Debug)]
pub(crate) struct Select<const NATIVE: bool>;
pub(crate) trait SelectStorage {
    type Storage;
}
impl SelectStorage for Select<true> {
    type Storage = NativeSnapshot;
}
impl SelectStorage for Select<false> {
    type Storage = FallbackSnapshot;
}

// Avoid portable-atomic's global-lock fallback. An inherited global lock can
// belong to a vanished writer after fork; advisory caches can simply miss.
pub(crate) type CacheSnapshot =
    <Select<{ portable_atomic::AtomicU128::is_always_lock_free() }> as SelectStorage>::Storage;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn interrupted_writer_never_blocks_readers_or_writers() {
        let snapshot = FallbackSnapshot::new(42);
        snapshot.epoch.store(1, Ordering::Relaxed);
        assert_eq!(snapshot.load(), None);
        snapshot.store(99);
        assert_eq!(snapshot.load(), None);
        assert_eq!(snapshot.low.load(Ordering::Relaxed), 42);
    }

    #[test]
    fn exhausted_epoch_never_wraps_or_reuses_a_stamp() {
        let snapshot = FallbackSnapshot::new(42);
        snapshot.epoch.store(u64::MAX - 3, Ordering::Relaxed);
        snapshot.store(99);
        assert_eq!(snapshot.epoch.load(Ordering::Relaxed), u64::MAX - 1);
        assert_eq!(snapshot.load(), Some(99));
        snapshot.store(101);
        assert_eq!(snapshot.epoch.load(Ordering::Relaxed), u64::MAX - 1);
        assert_eq!(snapshot.load(), Some(99));
    }

    #[test]
    fn shared_snapshots_are_complete_or_miss() {
        let a = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210;
        let b = 0xfedc_ba98_7654_3210_0123_4567_89ab_cdef;
        let snapshot = Arc::new(FallbackSnapshot::new(a));
        let barrier = Arc::new(Barrier::new(4));
        std::thread::scope(|scope| {
            for bits in [a, b, a, b] {
                let snapshot = Arc::clone(&snapshot);
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    for _ in 0..100 {
                        snapshot.store(bits);
                        if let Some(observed) = snapshot.load() {
                            assert!(observed == a || observed == b, "{observed:x}");
                        }
                    }
                });
            }
        });
    }
}
