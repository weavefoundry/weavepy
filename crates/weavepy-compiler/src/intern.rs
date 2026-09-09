//! Temporary indexes for the compiler's append-only constant and name pools.
//!
//! Small pools use a linear scan without allocating an index. Larger pools
//! keep collision chains of slot numbers, so indexing never clones a string,
//! tuple, or nested code object. Equality remains the pool's existing rule.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use crate::Constant;

#[derive(Default)]
pub(crate) struct InternIndex {
    heads: HashMap<u64, u32>,
    next: Vec<Option<u32>>,
}

impl InternIndex {
    pub(crate) fn intern<T: PartialEq>(
        &mut self,
        values: &mut Vec<T>,
        value: T,
        hash: impl Fn(&T) -> u64,
    ) -> u32 {
        const INDEX_THRESHOLD: usize = 32;
        if values.len() < INDEX_THRESHOLD {
            if let Some(i) = values.iter().position(|v| v == &value) {
                return i as u32;
            }
            let i = values.len() as u32;
            values.push(value);
            return i;
        }
        let key = hash(&value);
        if let Some(i) = self.find(values, &hash, key, |v| v == &value) {
            return i;
        }
        let i = values.len() as u32;
        self.next.push(self.heads.insert(key, i));
        values.push(value);
        i
    }

    pub(crate) fn intern_name(&mut self, values: &mut Vec<String>, name: &str) -> u32 {
        if values.len() < 32 {
            if let Some(i) = values.iter().position(|v| v == name) {
                return i as u32;
            }
        } else {
            let key = name_hash(name);
            if let Some(i) = self.find(values, &|v| name_hash(v), key, |v| v == name) {
                return i;
            }
            self.next.push(self.heads.insert(key, values.len() as u32));
        }
        let i = values.len() as u32;
        values.push(name.to_owned());
        i
    }

    fn find<T>(
        &mut self,
        values: &[T],
        hash: &impl Fn(&T) -> u64,
        key: u64,
        equal: impl Fn(&T) -> bool,
    ) -> Option<u32> {
        // Callers may seed the pool before the first intern operation.
        for (i, v) in values.iter().enumerate().skip(self.next.len()) {
            self.next.push(self.heads.insert(hash(v), i as u32));
        }
        debug_assert_eq!(self.next.len(), values.len());
        let mut slot = self.heads.get(&key).copied();
        // Preserve the first equal slot, including in a preseeded pool.
        let mut found = None;
        while let Some(i) = slot {
            if equal(&values[i as usize]) {
                found = Some(i);
            }
            slot = self.next[i as usize];
        }
        found
    }
}

fn name_hash(name: &str) -> u64 {
    let mut state = DefaultHasher::new();
    name.hash(&mut state);
    state.finish()
}

pub(crate) fn constant_hash(value: &Constant) -> u64 {
    fn hash(value: &Constant, state: &mut impl Hasher) {
        std::mem::discriminant(value).hash(state);
        match value {
            Constant::Bool(v) => v.hash(state),
            Constant::Int(v) => v.hash(state),
            Constant::BigInt(v) => v.hash(state),
            Constant::Float(v) => v.to_bits().hash(state),
            Constant::Complex(r, i) => {
                r.to_bits().hash(state);
                i.to_bits().hash(state);
            }
            Constant::Str(v) => v.hash(state),
            Constant::WStr(v) => v.hash(state),
            Constant::Bytes(v) => v.hash(state),
            Constant::Tuple(v) => {
                v.len().hash(state);
                for item in v {
                    hash(item, state);
                }
            }
            // Equality ignores order and, for equal lengths, multiplicity.
            // Keep the coarse key and resolve these uncommon constants by
            // equality rather than introducing a different equivalence rule.
            Constant::FrozenSet(v) => v.len().hash(state),
            Constant::Code(v) => {
                v.name.hash(state);
                v.qualname.hash(state);
                v.instructions.len().hash(state);
            }
            Constant::Slice(v) => {
                hash(&v.0, state);
                hash(&v.1, state);
                hash(&v.2, state);
            }
            _ => {}
        }
    }
    let mut state = DefaultHasher::new();
    hash(value, &mut state);
    state.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collisions_and_threshold_keep_first_slot() {
        let mut index = InternIndex::default();
        let mut values = Vec::new();
        for i in 0..100 {
            assert_eq!(index.intern(&mut values, i, |_| 0), i);
        }
        for i in (0..100).rev() {
            assert_eq!(index.intern(&mut values, i, |_| 0), i);
        }
        assert_eq!(values.len(), 100);
        let mut seeded = vec![1; 40];
        assert_eq!(InternIndex::default().intern(&mut seeded, 1, |_| 0), 0);
    }

    #[test]
    fn names_cross_threshold_without_changing_order() {
        let mut index = InternIndex::default();
        let mut names = Vec::new();
        for i in 0..100 {
            assert_eq!(index.intern_name(&mut names, &format!("name_{i}")), i);
        }
        for i in (0..100).rev() {
            assert_eq!(index.intern_name(&mut names, &format!("name_{i}")), i);
        }
        assert_eq!(names.len(), 100);
    }

    #[test]
    fn constant_keys_preserve_python_pool_equality() {
        let mut index = InternIndex::default();
        let mut values: Vec<_> = (0..40).map(Constant::Int).collect();
        let cases = [
            Constant::Bool(true),
            Constant::Float(0.0),
            Constant::Float(-0.0),
            Constant::Complex(0.0, -0.0),
            Constant::Str("abc".into()),
            Constant::Bytes(b"abc".to_vec()),
            Constant::Tuple(vec![Constant::Float(-0.0)]),
            Constant::FrozenSet(vec![Constant::Int(1), Constant::Int(2)]),
            Constant::Slice(Box::new((Constant::Int(1), Constant::None, Constant::None))),
        ];
        for c in cases {
            let slot = values.len() as u32;
            assert_eq!(index.intern(&mut values, c.clone(), constant_hash), slot);
            assert_eq!(index.intern(&mut values, c, constant_hash), slot);
        }
        let reversed = Constant::FrozenSet(vec![Constant::Int(2), Constant::Int(1)]);
        assert_eq!(index.intern(&mut values, reversed, constant_hash), 47);
        let nan = Constant::Float(f64::NAN);
        let a = index.intern(&mut values, nan.clone(), constant_hash);
        let b = index.intern(&mut values, nan, constant_hash);
        assert_ne!(a, b);
    }
}
