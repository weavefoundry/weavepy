//! Integration test: the RFC 0072 WS1 greenlet C-API proof.
//!
//! `crates/weavepy-capi/build.rs` compiles
//! `tests/capi_ext/_greenletconsumer.c` against the vendored stock
//! CPython 3.13 headers plus the vendored upstream
//! `greenlet/greenlet.h`, and exports
//! `WEAVEPY_CAPI_GREENLETCONSUMER_EXTENSION`. The fixture consumes the
//! `greenlet._C_API` capsule exactly the way gevent's compiled Cython
//! modules do: `PyGreenlet_Import()`, the `__Pyx_ImportType` size
//! check against `sizeof(PyGreenlet)`, a static C subclass with a cdef
//! field at offset `sizeof(PyGreenlet)`, and the full 12-slot table
//! (`New` / `GetCurrent` / `Switch` / `Throw` / `SetParent` /
//! `MAIN` / `STARTED` / `ACTIVE` / `GET_PARENT`).
//!
//! Here we stage the `.so` on `sys.path` and drive it from Python,
//! asserting the capsule resolves, the shell type is byte-faithful
//! (40-byte `tp_basicsize`), switching works **from inside a C
//! frame**, and the C subclass behaves like gevent's
//! `TrackedRawGreenlet`.
//!
//! Skipped (passes) when the env var is unset (no `cc` on the build
//! host), so CI on a bare machine still passes.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use weavepy::{run_source_with_options, InterpreterFlags, RunOptions};

/// One extension load at a time — the fixture keeps C-global state
/// (`_PyGreenlet_API`, the imported type pointer, the readied static
/// subclass), so concurrent interpreters importing it race.
fn serialize() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn extension_path() -> Option<PathBuf> {
    option_env!("WEAVEPY_CAPI_GREENLETCONSUMER_EXTENSION").map(PathBuf::from)
}

/// Render `s` as a Python single-quoted literal.
fn py_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// Stage the built `.so` under a temp dir as `_greenletconsumer.so`
/// and run `driver_body` with that dir prepended to `sys.path`.
fn run_with_consumer(driver_body: &str) {
    let Some(ext) = extension_path() else {
        eprintln!("WEAVEPY_CAPI_GREENLETCONSUMER_EXTENSION not set — skipping");
        return;
    };
    if !ext.is_file() {
        eprintln!("extension path missing: {} — skipping", ext.display());
        return;
    }
    let _guard = serialize();
    let tmp = tempfile::tempdir().expect("mktemp");
    let staged = tmp.path().join("_greenletconsumer.so");
    std::fs::copy(&ext, &staged).expect("staging extension");
    let p_dir = py_quote(&tmp.path().display().to_string());
    let driver = format!("import sys\nsys.path.insert(0, {p_dir})\n{driver_body}");
    let opts = RunOptions::new("<greenletconsumer-test>").with_flags(InterpreterFlags::default());
    if let Err(err) = run_source_with_options(&driver, &opts) {
        let formatted = err.format(&driver, "<greenletconsumer-test>");
        panic!("_greenletconsumer driver failed:\n{formatted}");
    }
}

#[test]
fn greenletconsumer_skipped_when_extension_missing() {
    if extension_path().is_none() {
        eprintln!(
            "WEAVEPY_CAPI_GREENLETCONSUMER_EXTENSION not set — skipping greenlet C-API proof"
        );
    }
}

/// `PyGreenlet_Import()` resolves the capsule; the type in slot 0 is
/// the same object as `greenlet.greenlet` and reports the faithful
/// `sizeof(PyGreenlet)` (40 on LP64) — the `__Pyx_ImportType`
/// contract gevent's compiled modules enforce.
#[test]
fn greenletconsumer_capsule_and_sizes() {
    run_with_consumer(
        "
import _greenletconsumer as gc
import greenlet
assert gc.imported == 1, 'PyGreenlet_Import failed (capsule NULL)'
assert gc.header_sizeof == 40, gc.header_sizeof
assert gc.imported_basicsize == gc.header_sizeof, gc.imported_basicsize
assert gc.capsule_basicsize == gc.header_sizeof, gc.capsule_basicsize
assert gc.types_match == 1, 'capsule type slot is not greenlet.greenlet'
assert gc.sub_field_offset == gc.header_sizeof, gc.sub_field_offset
assert greenlet._C_API is not None, 'facade _C_API placeholder not replaced'
",
    );
}

/// `PyGreenlet_GetCurrent` returns the same object as
/// `greenlet.getcurrent()`; the main greenlet answers MAIN / STARTED /
/// ACTIVE through the table.
#[test]
fn greenletconsumer_getcurrent_and_predicates() {
    run_with_consumer(
        "
import _greenletconsumer as gc
import greenlet
cur = gc.get_current()
assert cur is greenlet.getcurrent(), (cur, greenlet.getcurrent())
assert gc.current_is_main() == 1
assert gc.predicates(cur) == (1, 1, 1), gc.predicates(cur)
assert gc.type_check(cur) == 1
assert gc.type_check(42) == 0
",
    );
}

/// `PyGreenlet_New` + `PyGreenlet_Switch` drive a full lifecycle with
/// value round-trips; the predicates track unstarted → active → dead.
#[test]
fn greenletconsumer_new_and_switch_lifecycle() {
    run_with_consumer(
        "
import _greenletconsumer as gc
import greenlet

log = []
def run(x, y):
    log.append(('enter', x, y))
    got = greenlet.getcurrent().parent.switch('mid')
    log.append(('resumed', got))
    return 'done'

g = gc.new_greenlet(run)
assert isinstance(g, greenlet.greenlet), type(g)
assert gc.type_check(g) == 1
assert gc.predicates(g) == (0, 0, 0), gc.predicates(g)
r1 = gc.switch_to(g, (1, 2))
assert r1 == 'mid', r1
assert gc.predicates(g) == (0, 1, 1), gc.predicates(g)
r2 = gc.switch_to(g, ('back',))
assert r2 == 'done', r2
assert gc.predicates(g) == (0, 1, 0), gc.predicates(g)
assert log == [('enter', 1, 2), ('resumed', 'back')], log
",
    );
}

/// The RFC 0066 promise: switching away **from inside a C frame**
/// parks the whole native stack (C frame included) and resumes it —
/// the shape of gevent's hub switches.
#[test]
fn greenletconsumer_switch_under_c_frame() {
    run_with_consumer(
        "
import _greenletconsumer as gc
import greenlet

main = greenlet.getcurrent()
seq = []
def run():
    seq.append('in-g')
    got = gc.switch_to(main, ('from-c',))
    seq.append(('back-in-g', got))
    return 'g-done'

g = greenlet.greenlet(run)
first = g.switch()
assert first == 'from-c', first
seq.append('in-main')
second = g.switch('resume')
assert second == 'g-done', second
assert seq == ['in-g', 'in-main', ('back-in-g', 'resume')], seq
",
    );
}

/// `PyGreenlet_Throw` raises inside the target; the capsule's
/// exception slots are the same objects as the Python-level names.
#[test]
fn greenletconsumer_throw_and_exceptions() {
    run_with_consumer(
        "
import _greenletconsumer as gc
import greenlet

assert gc.exc_greenlet_exit() is greenlet.GreenletExit
assert gc.exc_greenlet_error() is greenlet.error

state = []
def run():
    try:
        greenlet.getcurrent().parent.switch()
    except ValueError as e:
        state.append(('caught', str(e)))
        return 'handled'

g = greenlet.greenlet(run)
g.switch()
r = gc.throw_into(g, ValueError, 'boom')
assert r == 'handled', r
assert state == [('caught', 'boom')], state
assert gc.predicates(g) == (0, 1, 0), gc.predicates(g)

def run2():
    greenlet.getcurrent().parent.switch()
g2 = greenlet.greenlet(run2)
g2.switch()
gc.throw_into(g2, gc.exc_greenlet_exit())
assert gc.predicates(g2) == (0, 1, 0), gc.predicates(g2)
",
    );
}

/// `PyGreenlet_GetParent` (NULL-without-exception for main) and
/// `PyGreenlet_SetParent` reparent through the table.
#[test]
fn greenletconsumer_parent_get_set() {
    run_with_consumer(
        "
import _greenletconsumer as gc
import greenlet

main = greenlet.getcurrent()
assert gc.get_parent(main) is None
g = greenlet.greenlet(lambda: None)
assert gc.get_parent(g) is main
other = greenlet.greenlet(lambda: None)
gc.set_parent(other, g)
assert other.parent is g
",
    );
}

/// The gevent shape end-to-end: a static C subclass of the imported
/// greenlet type, with a cdef-style field at `sizeof(PyGreenlet)`,
/// constructed through the inherited `tp_new` chain and the chained
/// `greenlet.__init__`. The C field must survive a full run.
#[test]
fn greenletconsumer_c_subclass() {
    run_with_consumer(
        "
import _greenletconsumer as gc
import greenlet

Sub = gc.SubGreenlet
assert issubclass(Sub, greenlet.greenlet)

vals = []
def run(*a):
    vals.append(a)
    return 'sub-done'

s = Sub(run)
assert isinstance(s, greenlet.greenlet), type(s)
assert gc.type_check(s) == 1
assert s.get_tag() is None
s.set_tag({'loop': 1})
assert s.get_tag() == {'loop': 1}, s.get_tag()
r = s.switch(7)
assert r == 'sub-done', r
assert vals == [(7,)], vals
assert gc.predicates(s) == (0, 1, 0), gc.predicates(s)
assert s.get_tag() == {'loop': 1}, 'C field did not survive the run'
",
    );
}
