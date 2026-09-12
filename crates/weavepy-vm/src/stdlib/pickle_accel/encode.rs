//! Speculative protocol-4/5 encoding for exact built-in data.
//!
//! This module never invokes Python callbacks. Unsupported graphs discard the
//! private byte buffer and return to the existing pickler. The caller must first
//! verify the pickler's methods, dispatch table, and configuration.

use std::collections::{HashMap, HashSet};

use num_traits::ToPrimitive;

use crate::builtins::object_identity;
use crate::object::Object;

const FRAME_TARGET: usize = 65_536;
const MAX_DEPTH: usize = 128;

fn extend(output: &mut Vec<u8>, data: &[u8]) -> Option<()> {
    output.try_reserve(data.len()).ok()?;
    output.extend_from_slice(data);
    Some(())
}

#[derive(Default)]
struct Framer {
    output: Vec<u8>,
    frame_start: Option<usize>,
}

impl Framer {
    fn write(&mut self, data: &[u8]) -> Option<()> {
        if self.frame_start.is_none() {
            let start = self.output.len();
            extend(&mut self.output, &[0x95, 0, 0, 0, 0, 0, 0, 0, 0])?;
            self.frame_start = Some(start);
        }
        extend(&mut self.output, data)
    }

    fn commit(&mut self, force: bool) {
        let Some(start) = self.frame_start else {
            return;
        };
        let body = start + 9;
        let len = self.output.len() - body;
        if !force && len < FRAME_TARGET {
            return;
        }
        if len >= 4 {
            self.output[start + 1..body].copy_from_slice(&(len as u64).to_le_bytes());
        } else {
            // Tiny terminal frames are emitted without a FRAME instruction.
            // At most three bytes move; ordinary frames stay in place.
            self.output.copy_within(body.., start);
            self.output.truncate(start + len);
        }
        self.frame_start = None;
    }

    fn data(&mut self, value: &[u8], unicode: bool) -> Option<()> {
        let len = value.len();
        if len <= 255 {
            self.write(&[if unicode { 0x8c } else { b'C' }, len as u8])?;
            return self.write(value);
        }
        let mut header = [0u8; 9];
        let header_len = if let Ok(len) = u32::try_from(len) {
            header[0] = if unicode { b'X' } else { b'B' };
            header[1..5].copy_from_slice(&len.to_le_bytes());
            5
        } else {
            header[0] = if unicode { 0x8d } else { 0x8e };
            header[1..].copy_from_slice(&(len as u64).to_le_bytes());
            9
        };
        if len >= FRAME_TARGET {
            self.commit(true);
            extend(&mut self.output, &header[..header_len])?;
            extend(&mut self.output, value)
        } else {
            self.write(&header[..header_len])?;
            self.write(value)
        }
    }
}

#[derive(Default)]
struct Encoder {
    writer: Framer,
    // Pins prevent another thread's container mutation from releasing an
    // already memoized value and reusing its address during this operation.
    memo: HashMap<i64, (u32, Object)>,
    active: HashSet<i64>,
    python_headroom: usize,
}

impl Encoder {
    fn memoize(&mut self, value: &Object) -> Option<()> {
        let index = u32::try_from(self.memo.len()).ok()?;
        self.memo.try_reserve(1).ok()?;
        self.memo
            .insert(object_identity(value), (index, value.clone()));
        self.writer.write(&[0x94])
    }

    fn get(&mut self, index: u32) -> Option<()> {
        if let Ok(index) = u8::try_from(index) {
            self.writer.write(&[b'h', index])
        } else {
            self.writer.write(b"j")?;
            self.writer.write(&index.to_le_bytes())
        }
    }

    fn long(&mut self, data: &[u8]) -> Option<()> {
        if let Ok(len) = u8::try_from(data.len()) {
            self.writer.write(&[0x8a, len])?;
        } else {
            let len = i32::try_from(data.len()).ok()?;
            self.writer.write(&[0x8b])?;
            self.writer.write(&len.to_le_bytes())?;
        }
        self.writer.write(data)
    }

    fn integer(&mut self, value: i64) -> Option<()> {
        if let Ok(value) = u8::try_from(value) {
            self.writer.write(&[b'K', value])
        } else if let Ok(value) = u16::try_from(value) {
            self.writer.write(b"M")?;
            self.writer.write(&value.to_le_bytes())
        } else if let Ok(value) = i32::try_from(value) {
            self.writer.write(b"J")?;
            self.writer.write(&value.to_le_bytes())
        } else {
            let bytes = value.to_le_bytes();
            let mut len = bytes.len();
            while len > 1
                && ((bytes[len - 1] == 0 && bytes[len - 2] & 0x80 == 0)
                    || (bytes[len - 1] == 0xff && bytes[len - 2] & 0x80 != 0))
            {
                len -= 1;
            }
            self.long(&bytes[..len])
        }
    }

    fn save(&mut self, value: &Object, depth: usize) -> Option<()> {
        // Leave ample room for the Python pickler's constructor/descriptor
        // calls and save/handler/batch expansion at every graph level. A
        // low recursion budget retains its existing fallback behavior.
        if depth > MAX_DEPTH || depth.saturating_mul(8).saturating_add(32) >= self.python_headroom {
            return None;
        }
        self.writer.commit(false);
        let memoized = matches!(
            value,
            Object::Str(_)
                | Object::Bytes(_)
                | Object::List(_)
                | Object::Dict(_)
                | Object::Tuple(_)
        );
        let id = if memoized { object_identity(value) } else { 0 };
        if memoized {
            // Recursive tuples require a POP/GET repair. Retain the existing
            // pickler for every cyclic graph in this first implementation.
            if self.active.contains(&id) {
                return None;
            }
            if let Some(&(index, _)) = self.memo.get(&id) {
                return self.get(index);
            }
        }
        match value {
            Object::None => self.writer.write(b"N"),
            Object::Bool(value) => self.writer.write(&[if *value { 0x88 } else { 0x89 }]),
            Object::Int(value) => self.integer(*value),
            Object::Long(value) => match value.to_i64() {
                Some(value) => self.integer(value),
                None => self.long(&value.to_signed_bytes_le()),
            },
            Object::Float(value) => {
                self.writer.write(b"G")?;
                self.writer.write(&value.to_bits().to_be_bytes())
            }
            Object::Str(value) => {
                self.writer.data(value.as_bytes(), true)?;
                self.memoize(&Object::Str(value.clone()))
            }
            Object::Bytes(value) => {
                self.writer.data(value, false)?;
                self.memoize(&Object::Bytes(value.clone()))
            }
            Object::Tuple(items) if items.is_empty() => self.writer.write(b")"),
            Object::Tuple(items) => {
                self.active.try_reserve(1).ok()?;
                self.active.insert(id);
                if items.len() > 3 {
                    self.writer.write(b"(")?;
                }
                for item in items.iter() {
                    self.save(item, depth + 1)?;
                }
                self.writer.write(&[match items.len() {
                    1 => 0x85,
                    2 => 0x86,
                    3 => 0x87,
                    _ => b't',
                }])?;
                self.memoize(value)?;
                self.active.remove(&id);
                Some(())
            }
            Object::List(items) => {
                // A bounded snapshot releases the container lock before
                // recursion without duplicating the entire list's storage.
                // Keep the original length as a bound; observed resizing
                // returns to the Python pickler before any callbacks run.
                let length = items.borrow().len();
                let mut batch = Vec::new();
                batch.try_reserve_exact(length.min(1000)).ok()?;
                self.active.try_reserve(1).ok()?;
                self.active.insert(id);
                self.writer.write(b"]")?;
                self.memoize(value)?;
                for start in (0..length).step_by(1000) {
                    batch.clear();
                    {
                        let items = items.borrow();
                        if items.len() != length {
                            return None;
                        }
                        let end = start.saturating_add(1000).min(length);
                        batch.extend(items[start..end].iter().cloned());
                    }
                    if batch.len() != 1 {
                        self.writer.write(b"(")?;
                    }
                    for item in &batch {
                        self.save(item, depth + 1)?;
                    }
                    self.writer
                        .write(if batch.len() == 1 { b"a" } else { b"e" })?;
                }
                self.active.remove(&id);
                Some(())
            }
            Object::Dict(items) => {
                let items = {
                    let items = items.borrow();
                    let mut snapshot = Vec::new();
                    snapshot.try_reserve_exact(items.len()).ok()?;
                    snapshot.extend(
                        items
                            .iter()
                            .map(|(key, value)| (key.0.clone(), value.clone())),
                    );
                    snapshot
                };
                self.active.try_reserve(1).ok()?;
                self.active.insert(id);
                self.writer.write(b"}")?;
                self.memoize(value)?;
                for batch in items.chunks(1000) {
                    if batch.len() != 1 {
                        self.writer.write(b"(")?;
                    }
                    for (key, value) in batch {
                        self.save(key, depth + 1)?;
                        self.save(value, depth + 1)?;
                    }
                    self.writer
                        .write(if batch.len() == 1 { b"s" } else { b"u" })?;
                }
                self.active.remove(&id);
                Some(())
            }
            _ => None,
        }
    }
}

pub(super) fn encode(value: &Object, protocol: u8) -> Option<Vec<u8>> {
    encode_with_headroom(
        value,
        protocol,
        crate::recursion::recursion_limit().saturating_sub(crate::recursion::current_depth()),
    )
}

fn encode_with_headroom(value: &Object, protocol: u8, python_headroom: usize) -> Option<Vec<u8>> {
    if !matches!(protocol, 4 | 5) {
        return None;
    }
    let mut encoder = Encoder {
        python_headroom,
        ..Encoder::default()
    };
    extend(&mut encoder.writer.output, &[0x80, protocol])?;
    encoder.save(value, 0)?;
    encoder.writer.write(b".")?;
    encoder.writer.commit(true);
    Some(encoder.writer.output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::Rc;
    use num_bigint::BigInt;

    #[test]
    fn list_batches_preserve_shared_children() {
        let child = Object::new_list(vec![Object::Int(7)]);
        let value = Object::new_list(vec![child; 2001]);
        let bytes = encode_with_headroom(&value, 5, 1000).unwrap();
        let Object::List(decoded) = super::super::decode(&bytes, |_| true).unwrap() else {
            panic!("expected decoded list");
        };
        let decoded = decoded.borrow();
        assert_eq!(decoded.len(), 2001);
        assert!(decoded.iter().all(|item| item.is_same(&decoded[0])));
    }

    #[test]
    fn list_encoding_tolerates_concurrent_resizing() {
        let items = Rc::new(crate::sync::RefCell::new(vec![Object::Int(7); 5001]));
        let value = Object::List(items.clone());
        let barrier = Rc::new(std::sync::Barrier::new(2));
        let worker_barrier = barrier.clone();
        let worker = std::thread::spawn(move || {
            worker_barrier.wait();
            for cycle in 0..2048 {
                items
                    .borrow_mut()
                    .resize(if cycle % 2 == 0 { 1001 } else { 5001 }, Object::Int(7));
                std::thread::yield_now();
            }
        });
        barrier.wait();
        for _ in 0..32 {
            // A resize may request fallback. Every accepted stream must be
            // complete and contain only the actual integer list elements.
            if let Some(bytes) = encode_with_headroom(&value, 5, 1000) {
                let Object::List(decoded) = super::super::decode(&bytes, |_| true).unwrap() else {
                    panic!("expected decoded list");
                };
                let decoded = decoded.borrow();
                assert!([1001, 5001].contains(&decoded.len()));
                assert!(decoded.iter().all(|item| matches!(item, Object::Int(7))));
            }
        }
        worker.join().unwrap();
        assert!(encode_with_headroom(&value, 5, 1000).is_some());
    }

    #[test]
    fn compact_integer_encoding_ignores_internal_bigint_representation() {
        for value in [
            i64::MIN,
            -2_147_483_649,
            -2_147_483_648,
            -1,
            0,
            255,
            256,
            65_535,
            65_536,
            i64::MAX,
        ] {
            assert_eq!(
                encode_with_headroom(&Object::Int(value), 5, 1000),
                encode_with_headroom(&Object::Long(Rc::new(BigInt::from(value))), 5, 1000)
            );
        }
        assert_eq!(
            encode_with_headroom(&Object::Long(Rc::new(BigInt::from(12))), 5, 1000),
            Some(vec![0x80, 5, b'K', 12, b'.'])
        );
    }

    #[test]
    fn frame_boundary_and_small_stream_bytes_match_the_reference() {
        assert_eq!(
            encode_with_headroom(&Object::None, 4, 1000),
            Some(vec![0x80, 4, b'N', b'.'])
        );
        assert_eq!(
            encode_with_headroom(&Object::Int(256), 5, 1000),
            Some(vec![
                0x80, 5, 0x95, 4, 0, 0, 0, 0, 0, 0, 0, b'M', 0, 1, b'.'
            ])
        );
        for size in [65_535usize, 65_536] {
            let value = Object::Bytes(Rc::from(vec![b'x'; size]));
            let actual = encode_with_headroom(&value, 5, 1000).unwrap();
            let mut expected = vec![0x80, 5];
            if size < FRAME_TARGET {
                expected.push(0x95);
                expected.extend_from_slice(&(size as u64 + 7).to_le_bytes());
            }
            expected.push(b'B');
            expected.extend_from_slice(&(size as u32).to_le_bytes());
            expected.extend(std::iter::repeat_n(b'x', size));
            expected.extend_from_slice(&[0x94, b'.']);
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn insufficient_python_headroom_uses_fallback() {
        assert!(encode_with_headroom(&Object::None, 5, 32).is_none());
        let value = Object::new_tuple_array([Object::None]);
        assert!(encode_with_headroom(&value, 5, 40).is_none());
        assert!(encode_with_headroom(&value, 5, 41).is_some());
        assert!(encode_with_headroom(&value, 3, 1000).is_none());
    }
}
