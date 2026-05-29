//! RFC 0032 — the VM side of the tier-2 Cranelift JIT.
//!
//! This module is compiled only with the `jit` feature. It owns a
//! per-thread [`weavepy_jit::JitEngine`] and a hot-counter cache keyed by
//! `CodeObject` identity, decides when a frame is hot enough to compile,
//! applies the entry type-guard, marshals locals into a
//! [`weavepy_jit::JitFrame`], enters the native code, and reconstructs
//! interpreter state on a deopt side exit.
//!
//! Everything here runs under the GIL on a single thread, so the engine,
//! cache, and the raw function pointers they hand out never cross thread
//! boundaries — hence the thread-local state and the plain [`StdRc`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc as StdRc;

use weavepy_compiler::CodeObject;
use weavepy_jit::{CompiledFrame, JitEngine, JitFrame, JitStatus, JitType, SlotTag};

use crate::object::Object;
use crate::sync::Rc;

/// What happened when the VM offered a frame to the JIT.
pub(crate) enum JitEntry {
    /// The native frame ran to completion; this is its return value.
    Ran(Object),
    /// The native frame deopted; `frame.pc` / locals / stack have been
    /// rewritten and the interpreter should resume.
    Deopt,
    /// The frame was not entered (cold, not JITable, or guard failed);
    /// run the interpreter as usual.
    Skip,
}

/// Per-`CodeObject` compilation state.
enum Tier {
    Cold,
    NotJitable,
    Compiled(StdRc<CompiledFrame>),
}

struct CacheEntry {
    counter: u32,
    tier: Tier,
    /// Keeps the code object alive so its address can't be reused while
    /// this entry (and any compiled pointer keyed by it) is live.
    _code: Rc<CodeObject>,
}

/// JIT counters surfaced through `WEAVEPY_VM_STATS`.
#[derive(Default, Clone)]
pub(crate) struct JitStats {
    pub frames_seen: u64,
    pub frames_compiled: u64,
    pub frames_notjitable: u64,
    pub native_entries: u64,
    pub deopts: u64,
    pub entry_guard_failures: u64,
}

struct JitState {
    enabled: bool,
    threshold: u32,
    engine: Option<JitEngine>,
    cache: HashMap<*const CodeObject, CacheEntry>,
    stats: JitStats,
}

impl JitState {
    fn new() -> JitState {
        let enabled = match std::env::var("WEAVEPY_JIT") {
            Ok(v) => v != "0" && !v.eq_ignore_ascii_case("off") && !v.is_empty(),
            Err(_) => false,
        };
        let threshold = std::env::var("WEAVEPY_JIT_THRESHOLD")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(50);
        JitState {
            enabled,
            threshold,
            engine: None,
            cache: HashMap::new(),
            stats: JitStats::default(),
        }
    }

    /// Bump the hot counter for `code` and, once it crosses the
    /// threshold, attempt compilation. Returns the compiled frame when
    /// one is available.
    fn get_compiled(&mut self, code: &Rc<CodeObject>) -> Option<StdRc<CompiledFrame>> {
        let key = Rc::as_ptr(code).cast::<CodeObject>();
        {
            let entry = self.cache.entry(key).or_insert_with(|| CacheEntry {
                counter: 0,
                tier: Tier::Cold,
                _code: code.clone(),
            });
            match &entry.tier {
                Tier::Compiled(cf) => return Some(cf.clone()),
                Tier::NotJitable => return None,
                Tier::Cold => {
                    entry.counter += 1;
                    if entry.counter < self.threshold {
                        return None;
                    }
                }
            }
        }
        // Threshold reached: compile (engine + cache borrowed disjointly).
        if self.engine.is_none() {
            self.engine = JitEngine::new();
            if self.engine.is_none() {
                // Host ISA unavailable — disable so we stop retrying.
                self.enabled = false;
                return None;
            }
        }
        let engine = self.engine.as_mut()?;
        let (tier, out) = match engine.compile(code) {
            Ok(cf) => {
                self.stats.frames_compiled += 1;
                let rc = StdRc::new(cf);
                (Tier::Compiled(rc.clone()), Some(rc))
            }
            Err(_) => {
                self.stats.frames_notjitable += 1;
                (Tier::NotJitable, None)
            }
        };
        if let Some(entry) = self.cache.get_mut(&key) {
            entry.tier = tier;
        }
        out
    }

    fn note_backedge(&mut self, code: &Rc<CodeObject>) {
        if !self.enabled {
            return;
        }
        let key = Rc::as_ptr(code).cast::<CodeObject>();
        let entry = self.cache.entry(key).or_insert_with(|| CacheEntry {
            counter: 0,
            tier: Tier::Cold,
            _code: code.clone(),
        });
        if matches!(entry.tier, Tier::Cold) {
            entry.counter = entry.counter.saturating_add(1);
        }
    }
}

thread_local! {
    static JIT: RefCell<JitState> = RefCell::new(JitState::new());
}

/// Reconstruct an [`Object`] from a `(bits, tag)` slot.
fn unpack(bits: u64, tag: u32) -> Object {
    match SlotTag::from_raw(tag) {
        SlotTag::Int => Object::Int(bits as i64),
        SlotTag::Float => Object::Float(f64::from_bits(bits)),
        SlotTag::Bool => Object::Bool(bits != 0),
    }
}

/// Reconstruct an [`Object`] from a slot whose lane is statically known.
fn unpack_ty(bits: u64, ty: JitType) -> Object {
    match ty {
        JitType::Int => Object::Int(bits as i64),
        JitType::Float => Object::Float(f64::from_bits(bits)),
        JitType::Bool => Object::Bool(bits != 0),
        JitType::Unknown => Object::None,
    }
}

/// Pack a representable [`Object`] into its slot bits for `ty`, or `None`
/// if it doesn't match the expected lane.
fn pack(obj: &Object, ty: JitType) -> Option<u64> {
    match (ty, obj) {
        (JitType::Int, Object::Int(i)) => Some(*i as u64),
        (JitType::Bool, Object::Bool(b)) => Some(u64::from(*b)),
        (JitType::Float, Object::Float(f)) => Some(f.to_bits()),
        _ => None,
    }
}

/// Bump the back-edge hot counter for a code object (no-op when the JIT
/// is disabled).
pub(crate) fn note_backedge(code: &Rc<CodeObject>) {
    JIT.with(|cell| cell.borrow_mut().note_backedge(code));
}

/// Offer a fresh frame (pc 0, empty stack) to the JIT. See [`JitEntry`].
pub(crate) fn try_enter(frame: &mut super::Frame) -> JitEntry {
    // Phase 1: counter + compilation, holding the state borrow briefly.
    let cf = JIT.with(|cell| {
        let mut st = cell.borrow_mut();
        if !st.enabled {
            return None;
        }
        st.stats.frames_seen += 1;
        st.get_compiled(&frame.code)
    });
    let Some(cf) = cf else {
        return JitEntry::Skip;
    };

    // Phase 2: entry type-guard on the live-in locals.
    for &slot in &cf.livein {
        let ty = match cf.local_types.get(slot as usize).copied().flatten() {
            Some(t) => t,
            None => return JitEntry::Skip,
        };
        let ok = frame
            .locals
            .get(slot as usize)
            .and_then(|o| pack(o, ty))
            .is_some();
        if !ok {
            JIT.with(|cell| cell.borrow_mut().stats.entry_guard_failures += 1);
            return JitEntry::Skip;
        }
    }

    // Phase 3: marshal locals and enter native code.
    let n = cf.n_locals as usize;
    let mut locals_buf = vec![0u64; n];
    for (slot, dst) in locals_buf.iter_mut().enumerate() {
        if let Some(ty) = cf.local_types[slot] {
            *dst = frame
                .locals
                .get(slot)
                .and_then(|o| pack(o, ty))
                .unwrap_or(0);
        }
    }
    let cap = cf.max_stack as usize + 1;
    let mut spill = vec![0u64; cap];
    let mut tags = vec![0u32; cap];
    let mut jf = JitFrame {
        locals: locals_buf.as_mut_ptr(),
        n_locals: cf.n_locals,
        entry_pc: 0,
        ret_bits: 0,
        ret_tag: 0,
        deopt_pc: 0,
        stack_spill: spill.as_mut_ptr(),
        stack_tags: tags.as_mut_ptr(),
        stack_len: 0,
        stack_cap: cap as u32,
    };

    // SAFETY: `locals_buf` is `n_locals` wide and `spill`/`tags` are
    // `max_stack + 1` wide, matching what the compiled frame was built
    // to address; the engine that backs `cf` lives in this thread's
    // `JIT` thread-local for the process lifetime.
    let status = unsafe { cf.enter(&raw mut jf) };

    JIT.with(|cell| {
        let mut st = cell.borrow_mut();
        st.stats.native_entries += 1;
        if matches!(status, JitStatus::Deopt) {
            st.stats.deopts += 1;
        }
    });

    match status {
        JitStatus::Returned => JitEntry::Ran(unpack(jf.ret_bits, jf.ret_tag)),
        JitStatus::Deopt => {
            // Write back managed locals, rebuild the operand stack from
            // the spill, and resume at the deopt pc.
            for (slot, &bits) in locals_buf.iter().enumerate() {
                if let Some(ty) = cf.local_types[slot] {
                    frame.locals[slot] = unpack_ty(bits, ty);
                }
            }
            for i in 0..jf.stack_len as usize {
                frame.stack.push(unpack(spill[i], tags[i]));
            }
            frame.pc = jf.deopt_pc;
            JitEntry::Deopt
        }
    }
}

/// Test hook: force the JIT on for the current thread with a low
/// tier-up threshold, regardless of `WEAVEPY_JIT`. Compiled only in
/// test builds so it never reaches release binaries.
#[cfg(test)]
pub(crate) fn force_enable_for_test(threshold: u32) {
    JIT.with(|cell| {
        let mut st = cell.borrow_mut();
        st.enabled = true;
        st.threshold = threshold.max(1);
    });
}

/// Test hook: `(frames_compiled, native_entries, deopts)` for the
/// current thread.
#[cfg(test)]
pub(crate) fn stats_for_test() -> (u64, u64, u64) {
    JIT.with(|cell| {
        let s = &cell.borrow().stats;
        (s.frames_compiled, s.native_entries, s.deopts)
    })
}

/// Render the JIT counters as markdown rows, or `None` if the JIT was
/// never exercised on this thread.
pub(crate) fn format_stats_markdown() -> Option<String> {
    JIT.with(|cell| {
        let st = cell.borrow();
        let s = &st.stats;
        if s.frames_seen == 0 {
            return None;
        }
        Some(format!(
            "\n## Tier-2 JIT stats\n\n\
             - frames seen: **{}**\n\
             - frames compiled: **{}**\n\
             - frames not JITable: **{}**\n\
             - native entries: **{}**\n\
             - deopts: **{}**\n\
             - entry-guard failures: **{}**\n",
            s.frames_seen,
            s.frames_compiled,
            s.frames_notjitable,
            s.native_entries,
            s.deopts,
            s.entry_guard_failures,
        ))
    })
}
