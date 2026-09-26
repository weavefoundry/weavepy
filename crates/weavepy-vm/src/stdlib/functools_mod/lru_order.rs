//! Dense recency links containing indexes only, never Python owners.
//!
//! The wrapper keeps this buffer in an ordinary bytearray, so neither the
//! object enum nor every Python instance needs a new native storage variant.
//! The cache and this buffer must remain borrowed together during mutation.

const WORD: usize = std::mem::size_of::<usize>();
const NONE: usize = usize::MAX;
const HEAD: usize = 0;
const TAIL: usize = 1;

pub(super) fn empty() -> Vec<u8> {
    // Reserve the first node with the header. Most small caches shouldn't
    // need either a second allocation or spare space for four nodes.
    let mut bytes = Vec::with_capacity(4 * WORD);
    bytes.extend_from_slice(&[0xff; 2 * WORD]);
    bytes
}

pub(super) struct Order<'a> {
    bytes: &'a mut Vec<u8>,
    len: usize,
}

impl<'a> Order<'a> {
    pub(super) fn new(bytes: &'a mut Vec<u8>, len: usize) -> Option<Self> {
        if bytes.len() != len.checked_add(1)?.checked_mul(2 * WORD)? {
            return None;
        }
        let order = Self { bytes, len };
        let head = order.word(HEAD)?;
        let tail = order.word(TAIL)?;
        if (len == 0 && (head != NONE || tail != NONE))
            || (len != 0 && (head >= len || tail >= len))
        {
            return None;
        }
        Some(order)
    }

    fn word(&self, index: usize) -> Option<usize> {
        let start = index.checked_mul(WORD)?;
        Some(usize::from_ne_bytes(
            self.bytes
                .get(start..start.checked_add(WORD)?)?
                .try_into()
                .ok()?,
        ))
    }

    fn set_word(&mut self, index: usize, value: usize) -> Option<()> {
        let start = index.checked_mul(WORD)?;
        self.bytes
            .get_mut(start..start.checked_add(WORD)?)?
            .copy_from_slice(&value.to_ne_bytes());
        Some(())
    }

    fn node(&self, index: usize) -> Option<(usize, usize)> {
        if index >= self.len {
            return None;
        }
        Some((self.word(2 + 2 * index)?, self.word(3 + 2 * index)?))
    }

    fn detach(&mut self, index: usize) -> Option<()> {
        let (prev, next) = self.node(index)?;
        if prev == index || next == index {
            return None;
        }
        if (prev == NONE && self.word(HEAD)? != index)
            || (prev != NONE && self.node(prev)?.1 != index)
            || (next == NONE && self.word(TAIL)? != index)
            || (next != NONE && self.node(next)?.0 != index)
        {
            return None;
        }
        if prev == NONE {
            self.set_word(HEAD, next)?;
        } else {
            self.set_word(3 + 2 * prev, next)?;
        }
        if next == NONE {
            self.set_word(TAIL, prev)?;
        } else {
            self.set_word(2 + 2 * next, prev)?;
        }
        Some(())
    }

    pub(super) fn head(&self) -> Option<usize> {
        let head = self.word(HEAD)?;
        (head != NONE).then_some(head)
    }

    pub(super) fn touch(&mut self, index: usize) -> Option<()> {
        if index >= self.len {
            return None;
        }
        if self.word(TAIL)? == index {
            return Some(());
        }
        self.detach(index)?;
        let tail = self.word(TAIL)?;
        self.node(tail)?;
        self.set_word(3 + 2 * tail, index)?;
        self.set_word(2 + 2 * index, tail)?;
        self.set_word(3 + 2 * index, NONE)?;
        self.set_word(TAIL, index)
    }

    pub(super) fn push(&mut self, limit: usize) -> Option<()> {
        let index = self.len;
        if index >= limit {
            return None;
        }
        let tail = self.word(TAIL)?;
        if tail != NONE {
            self.node(tail)?;
        }
        let needed = (index + 2).checked_mul(2 * WORD)?;
        if needed > self.bytes.capacity() {
            let nodes = index.saturating_mul(2).max(1).min(limit);
            let capacity = nodes.checked_add(1)?.checked_mul(2 * WORD)?;
            self.bytes.reserve_exact(capacity - self.bytes.len());
        }
        self.bytes.extend_from_slice(&tail.to_ne_bytes());
        self.bytes.extend_from_slice(&NONE.to_ne_bytes());
        self.len += 1;
        if tail == NONE {
            self.set_word(HEAD, index)?;
        } else {
            self.set_word(3 + 2 * tail, index)?;
        }
        self.set_word(TAIL, index)
    }

    /// Match the dictionary's swap removal without shifting other nodes.
    pub(super) fn swap_remove(&mut self, index: usize) -> Option<()> {
        self.detach(index)?;
        let last = self.len - 1;
        if index != last {
            // Read after detach: the moved node may have been adjacent.
            let (prev, next) = self.node(last)?;
            if prev != NONE {
                self.node(prev)?;
            }
            if next != NONE {
                self.node(next)?;
            }
            self.set_word(2 + 2 * index, prev)?;
            self.set_word(3 + 2 * index, next)?;
            if prev == NONE {
                self.set_word(HEAD, index)?;
            } else {
                self.set_word(3 + 2 * prev, index)?;
            }
            if next == NONE {
                self.set_word(TAIL, index)?;
            } else {
                self.set_word(2 + 2 * next, index)?;
            }
        }
        self.len -= 1;
        self.bytes.truncate((self.len + 1) * 2 * WORD);
        Some(())
    }

    /// Used only when leaving scalar mode, not for ordinary cache hits.
    pub(super) fn indices(&self) -> Option<Vec<usize>> {
        let mut result = Vec::with_capacity(self.len);
        let mut index = self.word(HEAD)?;
        for _ in 0..self.len {
            result.push(index);
            index = self.node(index)?.1;
        }
        (index == NONE).then_some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dense_recency_matches_a_sequence_through_arbitrary_removals() {
        for limit in 1..=32 {
            let mut bytes = empty();
            let mut dense = Vec::new();
            let mut expected = Vec::new();
            let mut seed = 7_u64;
            for value in 0..4000 {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let mut order = Order::new(&mut bytes, dense.len()).unwrap();
                if !dense.is_empty() && !seed.is_multiple_of(3) {
                    let index = (seed as usize) % dense.len();
                    let key = dense[index];
                    expected.retain(|item| *item != key);
                    if seed & 8 == 0 {
                        order.swap_remove(index).unwrap();
                        dense.swap_remove(index);
                    } else {
                        order.touch(index).unwrap();
                        expected.push(key);
                    }
                } else {
                    if dense.len() == limit {
                        let index = order.head().unwrap();
                        assert_eq!(dense[index], expected.remove(0));
                        order.swap_remove(index).unwrap();
                        dense.swap_remove(index);
                    }
                    order.push(limit).unwrap();
                    dense.push(value);
                    expected.push(value);
                }
                let actual: Vec<_> = order.indices().unwrap().iter().map(|i| dense[*i]).collect();
                assert_eq!(actual, expected, "limit={limit} step={value}");
            }
        }
    }

    #[test]
    fn malformed_header_and_self_links_are_rejected() {
        assert!(Order::new(&mut vec![], 0).is_none());
        let mut bytes = empty();
        let mut order = Order::new(&mut bytes, 0).unwrap();
        order.push(2).unwrap();
        order.push(2).unwrap();
        order.set_word(3, 0).unwrap();
        assert!(order.touch(0).is_none());
        assert!(order.indices().is_none());
    }
}
