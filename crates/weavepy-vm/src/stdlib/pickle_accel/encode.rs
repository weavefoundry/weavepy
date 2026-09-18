//! Speculative protocol-4/5 encoding for exact built-in data and for
//! instances of plain classes that use the default object reduction.
//!
//! This module never invokes Python callbacks. Unsupported graphs discard the
//! private byte buffer and return to the existing pickler. The caller must first
//! verify the pickler's methods, dispatch table, and configuration.
//!
//! An instance is encoded only when type metadata alone proves that
//! `object.__reduce_ex__` would produce `copyreg.__newobj__(cls)` plus the
//! default state, and that `save_global` would find the class. The one side
//! effect of that reduction, the `__slotnames__` cache, is applied only after
//! the whole graph has been encoded.

use std::collections::{HashMap, HashSet};

use num_traits::ToPrimitive;

use super::classes;
use crate::builtins::object_identity;
use crate::object::{DictData, DictKey, Object, StrKey};
use crate::sync::{Rc, RefCell};
use crate::types::{PyInstance, TypeObject};

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

/// The interpreter state that `save_global` and the reduction consult. It is
/// produced on demand so that built-in data never pays for it.
pub(super) struct InstanceContext {
    pub(super) modules: Rc<RefCell<DictData>>,
    pub(super) dispatch_table: Rc<RefCell<DictData>>,
}

struct ClassPlan {
    module: Object,
    qualname: Object,
    slot_names: Vec<Object>,
    has_dict: bool,
}

struct Encoder<'a> {
    writer: Framer,
    // Pins prevent another thread's container mutation from releasing an
    // already memoized value and reusing its address during this operation.
    memo: HashMap<i64, (u32, Object)>,
    // Memo positions taken by the default state's temporary tuple and
    // dictionary, to which no later object can refer.
    anonymous: u32,
    active: HashSet<i64>,
    python_headroom: usize,
    context: &'a dyn Fn() -> Option<InstanceContext>,
    resolved: Option<InstanceContext>,
    classes: HashMap<usize, Rc<ClassPlan>>,
    slot_name_caches: Vec<(Rc<TypeObject>, Vec<Object>)>,
}

impl Encoder<'_> {
    fn next_index(&self) -> Option<u32> {
        u32::try_from(self.memo.len())
            .ok()?
            .checked_add(self.anonymous)
    }

    fn memoize(&mut self, value: &Object) -> Option<()> {
        let index = self.next_index()?;
        self.memo.try_reserve(1).ok()?;
        self.memo
            .insert(object_identity(value), (index, value.clone()));
        self.writer.write(&[0x94])
    }

    fn memoize_anonymous(&mut self) -> Option<()> {
        self.next_index()?;
        self.anonymous += 1;
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
                | Object::Instance(_)
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
                self.save_items(&items, depth + 1)?;
                self.active.remove(&id);
                Some(())
            }
            Object::Instance(instance) => {
                // The instance is memoized before its state, but a graph
                // that returns to it is cyclic and keeps the full pickler.
                self.active.try_reserve(1).ok()?;
                self.active.insert(id);
                self.save_instance(value, instance, depth)?;
                self.active.remove(&id);
                Some(())
            }
            _ => None,
        }
    }

    fn save_items(&mut self, items: &[(Object, Object)], depth: usize) -> Option<()> {
        for batch in items.chunks(1000) {
            if batch.len() != 1 {
                self.writer.write(b"(")?;
            }
            for (key, value) in batch {
                self.save(key, depth)?;
                self.save(value, depth)?;
            }
            self.writer
                .write(if batch.len() == 1 { b"s" } else { b"u" })?;
        }
        Some(())
    }

    /// `save_reduce(copyreg.__newobj__, (cls,), state)` for the default
    /// `object.__reduce_ex__`, in the pickler's exact `save` order.
    fn save_instance(
        &mut self,
        value: &Object,
        instance: &Rc<PyInstance>,
        depth: usize,
    ) -> Option<()> {
        if instance.native.get().is_some() || instance.c_body.get() != 0 {
            return None;
        }
        let class = instance.cls();
        let plan = self.class_plan(&class)?;
        let dict = match instance.dict.get_shared() {
            Some(dict) => {
                let nonempty = {
                    let dict = dict.borrow();
                    // A dunder entry can shadow a reduction hook, and a
                    // non-string key never comes from attribute assignment.
                    if !dict.keys().all(|key| {
                        matches!(&key.0, Object::Str(name)
                            if !(name.starts_with("__") && name.ends_with("__")))
                    }) {
                        return None;
                    }
                    !dict.is_empty()
                };
                nonempty.then_some(dict)
            }
            None => None,
        };
        if dict.is_some() && !plan.has_dict {
            return None;
        }
        let mut slots: Vec<(Object, Object)> = Vec::new();
        if !plan.slot_names.is_empty() {
            let storage = instance.slots.borrow();
            slots.try_reserve_exact(plan.slot_names.len()).ok()?;
            for name in &plan.slot_names {
                let Object::Str(text) = name else {
                    return None;
                };
                let repeated = slots
                    .iter()
                    .any(|(seen, _)| matches!(seen, Object::Str(seen) if seen == text));
                if let (false, Some(value)) = (repeated, storage.get(text)) {
                    slots.push((name.clone(), value.clone()));
                }
            }
        }

        self.save_class(&class, &plan, depth + 1)?;
        self.save(&Object::new_tuple(Vec::new()), depth + 1)?;
        self.writer.write(&[0x81])?;
        self.memoize(value)?;
        match (dict, slots.is_empty()) {
            (None, true) => return Some(()),
            (Some(dict), true) => self.save(&Object::Dict(dict), depth + 1)?,
            (dict, false) => {
                // The state is the temporary `(dict or None, {slot: value})`.
                self.writer.commit(false);
                self.save(&dict.map_or(Object::None, Object::Dict), depth + 2)?;
                self.writer.commit(false);
                self.writer.write(b"}")?;
                self.memoize_anonymous()?;
                self.save_items(&slots, depth + 3)?;
                self.writer.write(&[0x86])?;
                self.memoize_anonymous()?;
            }
        }
        self.writer.write(b"b")
    }

    /// `save_global` for a class that [`Self::class_plan`] accepted.
    fn save_class(&mut self, class: &Rc<TypeObject>, plan: &ClassPlan, depth: usize) -> Option<()> {
        self.writer.commit(false);
        let value = Object::Type(class.clone());
        if let Some(&(index, _)) = self.memo.get(&object_identity(&value)) {
            return self.get(index);
        }
        self.save(&plan.module, depth + 1)?;
        self.save(&plan.qualname, depth + 1)?;
        self.writer.write(&[0x93])?;
        self.memoize(&value)
    }

    fn class_plan(&mut self, class: &Rc<TypeObject>) -> Option<Rc<ClassPlan>> {
        let key = Rc::as_ptr(class) as usize;
        if let Some(plan) = self.classes.get(&key) {
            return Some(plan.clone());
        }
        if self.resolved.is_none() {
            self.resolved = Some((self.context)()?);
        }
        let context = self.resolved.as_ref()?;
        let has_dict = classes::plain_layout(class, classes::ENCODE_HOOKS)?;
        if !classes::benign_metaclass(class) || class.immutable.get() {
            return None;
        }
        // `copyreg.dispatch_table` is consulted for the instance's class and,
        // when the class itself is saved, for its metaclass.
        let metaclass = class.metaclass.borrow().clone();
        for (key, _) in context.dispatch_table.borrow().iter() {
            let Object::Type(registered) = &key.0 else {
                return None;
            };
            if Rc::ptr_eq(registered, class)
                || metaclass
                    .as_ref()
                    .is_some_and(|meta| Rc::ptr_eq(registered, meta))
            {
                return None;
            }
        }
        // `getattr(cls, "__qualname__")` and `whichmodule`: both must name
        // the class itself through plain module and class dictionaries.
        let (module, own_qualname) = {
            let dict = class.dict.borrow();
            (
                dict.get(&StrKey("__module__")).cloned()?,
                dict.get(&StrKey("__qualname__")).cloned(),
            )
        };
        let Object::Str(module_name) = &module else {
            return None;
        };
        let qualname = match own_qualname {
            Some(name @ Object::Str(_)) => name,
            _ => Object::interned_str(class.qualname.borrow().as_deref().unwrap_or(&class.name)),
        };
        let Object::Str(qualified) = &qualname else {
            return None;
        };
        let found = classes::resolve_global(&context.modules, module_name, qualified)?;
        if !Rc::ptr_eq(&found, class) {
            return None;
        }
        // `copyreg._slotnames`: reuse the cached list when it is the one
        // that function would compute, else remember to publish it.
        let mut slot_names = classes::slot_names(class)?;
        let cached = class.dict.borrow().get(&StrKey("__slotnames__")).cloned();
        match cached {
            Some(Object::List(cached)) => {
                let cached = cached.borrow().clone();
                let same = cached.len() == slot_names.len()
                    && cached.iter().zip(&slot_names).all(|pair| {
                        matches!(pair, (Object::Str(left), Object::Str(right)) if left == right)
                    });
                if !same {
                    return None;
                }
                slot_names = cached;
            }
            Some(_) => return None,
            None => {
                self.slot_name_caches.try_reserve(1).ok()?;
                self.slot_name_caches
                    .push((class.clone(), slot_names.clone()));
            }
        }
        let plan = Rc::new(ClassPlan {
            module,
            qualname,
            slot_names,
            has_dict,
        });
        self.classes.try_reserve(1).ok()?;
        self.classes.insert(key, plan.clone());
        Some(plan)
    }

    /// `cls.__slotnames__ = names`, the reduction's only side effect.
    fn publish_slot_name_caches(&mut self) {
        for (class, names) in self.slot_name_caches.drain(..) {
            let mut dict = class.dict.borrow_mut();
            if dict.contains_key(&StrKey("__slotnames__")) {
                continue;
            }
            let names = Object::new_list(names);
            crate::gc_trace::track(names.clone());
            dict.insert(DictKey(Object::from_static("__slotnames__")), names);
            drop(dict);
            class.bump_attr_version();
        }
    }
}

pub(super) fn encode(
    value: &Object,
    protocol: u8,
    context: &dyn Fn() -> Option<InstanceContext>,
) -> Option<Vec<u8>> {
    encode_with_context(
        value,
        protocol,
        crate::recursion::recursion_limit().saturating_sub(crate::recursion::current_depth()),
        context,
    )
}

#[cfg(test)]
fn encode_with_headroom(value: &Object, protocol: u8, python_headroom: usize) -> Option<Vec<u8>> {
    encode_with_context(value, protocol, python_headroom, &|| None)
}

fn encode_with_context(
    value: &Object,
    protocol: u8,
    python_headroom: usize,
    context: &dyn Fn() -> Option<InstanceContext>,
) -> Option<Vec<u8>> {
    if !matches!(protocol, 4 | 5) {
        return None;
    }
    let mut encoder = Encoder {
        writer: Framer::default(),
        memo: HashMap::new(),
        anonymous: 0,
        active: HashSet::new(),
        python_headroom,
        context,
        resolved: None,
        classes: HashMap::new(),
        slot_name_caches: Vec::new(),
    };
    extend(&mut encoder.writer.output, &[0x80, protocol])?;
    encoder.save(value, 0)?;
    encoder.writer.write(b".")?;
    encoder.writer.commit(true);
    encoder.publish_slot_name_caches();
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
        let Object::List(decoded) = super::super::decode(&bytes, |_| true, &|| None).unwrap()
        else {
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
                let Object::List(decoded) =
                    super::super::decode(&bytes, |_| true, &|| None).unwrap()
                else {
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
            let value = Object::Bytes(crate::shared_value::SharedSlice::from(vec![b'x'; size]));
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
