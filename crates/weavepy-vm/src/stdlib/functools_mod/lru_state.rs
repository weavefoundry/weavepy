//! An `lru_cache` wrapper's private words: its hit and miss counts, its
//! size limit and mode, and, for a bounded cache of native keys, the
//! recency links of its entries by entry position. They hold no Python
//! owners.
//!
//! The words live in a `bytearray` in one of the wrapper's slots, so
//! neither the object enum nor every instance needs a native storage
//! variant. Python reaches the buffer only through `gc.get_referents` (or
//! WeavePy's private `_functools._lru_private`), and every access is
//! bounds-checked: a tampered buffer reads as inconsistent, never as
//! unsound. The cache's lock (its dictionary's
//! cell) also serializes these words, and the two stay borrowed together
//! while either changes.

const WORD: usize = std::mem::size_of::<u64>();

/// No entry: an empty list's head and tail, and the ends' outer links.
const NONE: u64 = u64::MAX;

const HITS: usize = 0;
const MISSES: usize = 1;
/// The size limit: [`UNBOUNDED`], or the entry count (zero caches
/// nothing).
const LIMIT: usize = 2;
const FLAGS: usize = 3;
const HEAD: usize = 4;
const TAIL: usize = 5;
/// The words before the first entry's links.
const HEADER: usize = 6;

/// [`LIMIT`] of a cache with no size limit.
pub(super) const UNBOUNDED: u64 = u64::MAX;

/// The keys carry their arguments' types (`typed=True`).
pub(super) const TYPED: u64 = 1;
/// The cache admitted a key whose comparison can run Python: the entries
/// are kept in recency order (oldest first) and the links are unused.
pub(super) const FALLBACK: u64 = 2;

#[inline(always)]
fn read(bytes: &[u8], index: usize) -> Option<u64> {
    let at = index.checked_mul(WORD)?;
    let word = bytes.get(at..at + WORD)?;
    Some(u64::from_ne_bytes(word.try_into().ok()?))
}

#[inline(always)]
fn write(bytes: &mut [u8], index: usize, value: u64) -> Option<()> {
    let at = index.checked_mul(WORD)?;
    bytes
        .get_mut(at..at + WORD)?
        .copy_from_slice(&value.to_ne_bytes());
    Some(())
}

/// A new wrapper's words: no entries, no counts.
pub(super) fn new(limit: u64, flags: u64) -> Vec<u8> {
    // Room for the header and a first node, so a small cache's first
    // entries need no reallocation.
    let mut bytes = Vec::with_capacity((HEADER + 4) * WORD);
    for word in [0, 0, limit, flags, NONE, NONE] {
        bytes.extend_from_slice(&word.to_ne_bytes());
    }
    bytes
}

/// The size limit and the flags.
#[inline(always)]
pub(super) fn mode(bytes: &[u8]) -> Option<(u64, u64)> {
    Some((read(bytes, LIMIT)?, read(bytes, FLAGS)?))
}

/// Set `flag`.
pub(super) fn add_flag(bytes: &mut [u8], flag: u64) -> Option<()> {
    let flags = read(bytes, FLAGS)?;
    write(bytes, FLAGS, flags | flag)
}

/// The hit and miss counts.
pub(super) fn counts(bytes: &[u8]) -> Option<(u64, u64)> {
    Some((read(bytes, HITS)?, read(bytes, MISSES)?))
}

/// Count one more hit.
#[inline(always)]
pub(super) fn hit(bytes: &mut [u8]) -> Option<()> {
    write(bytes, HITS, read(bytes, HITS)?.wrapping_add(1))
}

/// Count one more miss.
#[inline(always)]
pub(super) fn miss(bytes: &mut [u8]) -> Option<()> {
    write(bytes, MISSES, read(bytes, MISSES)?.wrapping_add(1))
}

/// Take back one miss.
pub(super) fn unmiss(bytes: &mut [u8]) -> Option<()> {
    write(bytes, MISSES, read(bytes, MISSES)?.wrapping_sub(1))
}

/// Zero both counts.
pub(super) fn reset_counts(bytes: &mut [u8]) -> Option<()> {
    write(bytes, HITS, 0)?;
    write(bytes, MISSES, 0)
}

/// Drop every entry's links (the caller checked the buffer may resize).
pub(super) fn clear_links(bytes: &mut Vec<u8>) -> Option<()> {
    bytes.truncate(HEADER * WORD);
    write(bytes, HEAD, NONE)?;
    write(bytes, TAIL, NONE)
}

/// The recency links of a bounded cache's `len` entries, oldest at the
/// head: entry `i`'s previous and next entries are words `HEADER + 2i`
/// and `HEADER + 2i + 1`.
pub(super) struct Links<'a> {
    bytes: &'a mut Vec<u8>,
    len: usize,
}

impl<'a> Links<'a> {
    /// The links of `len` entries, if the buffer holds exactly them.
    #[inline(always)]
    pub(super) fn new(bytes: &'a mut Vec<u8>, len: usize) -> Option<Self> {
        let words = len.checked_mul(2)?.checked_add(HEADER)?;
        if bytes.len() != words.checked_mul(WORD)? {
            return None;
        }
        Some(Self { bytes, len })
    }

    /// Word `index`, which is a header word or an entry's link.
    #[inline(always)]
    fn get(&self, index: usize) -> u64 {
        debug_assert!(index < HEADER + 2 * self.len);
        // SAFETY: the buffer holds `HEADER + 2 * len` words (checked by
        // `new`, kept by `push`), and every index passed here is a header
        // word or the link of an entry below `len`; the read is unaligned.
        #[allow(clippy::cast_ptr_alignment)]
        unsafe {
            self.bytes.as_ptr().cast::<u64>().add(index).read_unaligned()
        }
    }

    /// Set word `index` (as [`Self::get`]'s).
    #[inline(always)]
    fn set(&mut self, index: usize, value: u64) {
        debug_assert!(index < HEADER + 2 * self.len);
        // SAFETY: as in `get`; the write is unaligned.
        #[allow(clippy::cast_ptr_alignment)]
        unsafe {
            self.bytes
                .as_mut_ptr()
                .cast::<u64>()
                .add(index)
                .write_unaligned(value);
        }
    }

    /// An entry position read from a link: `None` unless it names an
    /// entry.
    #[inline(always)]
    fn entry(&self, link: u64) -> Option<usize> {
        usize::try_from(link).ok().filter(|&i| i < self.len)
    }

    /// The least recent entry.
    pub(super) fn head(&self) -> Option<usize> {
        self.entry(self.get(HEAD))
    }

    /// Make entry `index` the most recent.
    #[inline]
    pub(super) fn touch(&mut self, index: usize) -> Option<()> {
        if index >= self.len {
            return None;
        }
        let tail = self.get(TAIL);
        if tail == index as u64 {
            return Some(());
        }
        let tail = self.entry(tail)?;
        let at = HEADER + 2 * index;
        let (prev, next) = (self.get(at), self.get(at + 1));
        // Unlink it (it isn't the tail, so it has a next entry)...
        let after = self.entry(next)?;
        let before = if prev == NONE { None } else { Some(self.entry(prev)?) };
        self.set(HEADER + 2 * after, prev);
        match before {
            None => self.set(HEAD, next),
            Some(before) => self.set(HEADER + 2 * before + 1, next),
        }
        // ...and append it after the tail.
        self.set(HEADER + 2 * tail + 1, index as u64);
        self.set(at, tail as u64);
        self.set(at + 1, NONE);
        self.set(TAIL, index as u64);
        Some(())
    }

    /// Link a new entry, the next position, as the most recent (the
    /// caller checked the buffer may resize). `limit` caps the growth of
    /// the buffer's reservation.
    pub(super) fn push(&mut self, limit: usize) -> Option<()> {
        let index = self.len;
        let tail = self.get(TAIL);
        let tail = if tail == NONE { None } else { Some(self.entry(tail)?) };
        let needed = (HEADER + 2 * (index + 1)).checked_mul(WORD)?;
        if needed > self.bytes.capacity() {
            let nodes = index.saturating_mul(2).max(1).min(limit.max(index + 1));
            let words = nodes.checked_mul(2)?.checked_add(HEADER)?;
            let capacity = words.checked_mul(WORD)?;
            self.bytes.reserve_exact(capacity - self.bytes.len());
        }
        let prev = tail.map_or(NONE, |t| t as u64);
        self.bytes.extend_from_slice(&prev.to_ne_bytes());
        self.bytes.extend_from_slice(&NONE.to_ne_bytes());
        self.len += 1;
        match tail {
            None => self.set(HEAD, index as u64),
            Some(tail) => self.set(HEADER + 2 * tail + 1, index as u64),
        }
        self.set(TAIL, index as u64);
        Some(())
    }

    /// The entry positions, least recent first (to leave native mode).
    pub(super) fn indices(&self) -> Option<Vec<usize>> {
        let mut result = Vec::with_capacity(self.len);
        let mut link = self.get(HEAD);
        for _ in 0..self.len {
            let index = self.entry(link)?;
            result.push(index);
            link = self.get(HEADER + 2 * index + 1);
        }
        (link == NONE).then_some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recency_matches_a_sequence_through_touches_and_evictions() {
        for limit in 1..=32 {
            let mut bytes = new(limit as u64, 0);
            let mut dense: Vec<u32> = Vec::new();
            let mut expected = Vec::new();
            let mut seed = 7_u64;
            for value in 0..4000 {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let mut links = Links::new(&mut bytes, dense.len()).unwrap();
                if !dense.is_empty() && !seed.is_multiple_of(3) {
                    // A hit refreshes an entry.
                    let index = (seed as usize) % dense.len();
                    let key = dense[index];
                    expected.retain(|item| *item != key);
                    links.touch(index).unwrap();
                    expected.push(key);
                } else if dense.len() == limit {
                    // A full cache's miss replaces its oldest entry in
                    // place, which becomes the newest.
                    let index = links.head().unwrap();
                    assert_eq!(dense[index], expected.remove(0));
                    links.touch(index).unwrap();
                    dense[index] = value;
                    expected.push(value);
                } else {
                    links.push(limit).unwrap();
                    dense.push(value);
                    expected.push(value);
                }
                let links = Links::new(&mut bytes, dense.len()).unwrap();
                let actual: Vec<_> = links.indices().unwrap().iter().map(|i| dense[*i]).collect();
                assert_eq!(actual, expected, "limit={limit} step={value}");
            }
        }
    }

    #[test]
    fn counts_and_mode_words() {
        let mut bytes = new(UNBOUNDED, TYPED);
        assert_eq!(mode(&bytes), Some((UNBOUNDED, TYPED)));
        hit(&mut bytes).unwrap();
        miss(&mut bytes).unwrap();
        miss(&mut bytes).unwrap();
        assert_eq!(counts(&bytes), Some((1, 2)));
        add_flag(&mut bytes, FALLBACK).unwrap();
        assert_eq!(mode(&bytes), Some((UNBOUNDED, TYPED | FALLBACK)));
        reset_counts(&mut bytes).unwrap();
        assert_eq!(counts(&bytes), Some((0, 0)));
    }

    #[test]
    fn malformed_buffers_are_rejected() {
        let mut short = vec![0u8; 3];
        assert!(Links::new(&mut short, 0).is_none());
        assert!(mode(&short).is_none());
        let mut bytes = new(2, 0);
        let mut links = Links::new(&mut bytes, 0).unwrap();
        links.push(2).unwrap();
        links.push(2).unwrap();
        // Point the first entry's next link at itself.
        write(&mut bytes, HEADER + 1, 0).unwrap();
        let mut links = Links::new(&mut bytes, 2).unwrap();
        assert!(links.indices().is_none());
        links.set(TAIL, 7);
        assert!(links.touch(0).is_none());
        assert!(Links::new(&mut bytes, 3).is_none());
    }
}
