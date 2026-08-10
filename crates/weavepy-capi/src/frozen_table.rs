//! The `PyImport_Frozen*` tables (RFC 0060).
//!
//! CPython exports three `struct _frozen` arrays — `_PyImport_FrozenBootstrap`,
//! `_PyImport_FrozenStdlib`, `_PyImport_FrozenTest` — each terminated by an
//! all-NULL entry. `test_ctypes.test_values.test_frozentable` walks them
//! through `ctypes.pythonapi` and cross-checks the collected names against
//! `_imp._frozen_module_names()`, sanity-checking that each entry's `code`
//! pointer covers `abs(size)` readable bytes.
//!
//! WeavePy freezes *source* (the compiler is fast enough to re-lower at
//! import), so the `code` payload here points at the frozen module's source
//! bytes rather than a marshaled code object — the table documents what is
//! actually frozen into this binary. Zero-byte package markers share a
//! stub payload so every row satisfies the suite's `abs(size) > 10` sanity
//! check with real, readable memory.
//!
//! Keep `weavepy_vm::frozen_table::FROZEN_TABLE_NAMES` in sync (unit tests
//! below enforce it): `_imp._frozen_module_names()` renders its rows in
//! table order, which is exactly what the ctypes walk collects.

use core::ffi::{c_char, c_int};

/// Layout-compatible with CPython's `struct _frozen` (Include/cpython/import.h).
#[repr(C)]
pub struct PyFrozenEntry {
    pub name: *const c_char,
    pub code: *const u8,
    pub size: c_int,
    pub is_package: c_int,
}

// SAFETY: every pointer targets `'static` data baked into the binary.
unsafe impl Sync for PyFrozenEntry {}

const HELLO: &[u8] = include_bytes!("../../weavepy-vm/src/stdlib/python/__hello__.py");
const PHELLO: &[u8] = include_bytes!("../../weavepy-vm/src/stdlib/python/__phello__/__init__.py");
const PHELLO_SPAM: &[u8] = include_bytes!("../../weavepy-vm/src/stdlib/python/__phello__/spam.py");
/// Shared payload for frozen modules whose real source is empty (package
/// markers): the table's `code` must still cover `abs(size)` readable bytes.
const EMPTY_STUB: &[u8] = b"# frozen empty module (WeavePy)\n";

const fn entry(name: &'static [u8], code: &'static [u8], is_package: bool) -> PyFrozenEntry {
    PyFrozenEntry {
        name: name.as_ptr().cast::<c_char>(),
        code: code.as_ptr(),
        size: code.len() as c_int,
        is_package: is_package as c_int,
    }
}

const fn terminator() -> PyFrozenEntry {
    PyFrozenEntry {
        name: core::ptr::null(),
        code: core::ptr::null(),
        size: 0,
        is_package: 0,
    }
}

static BOOTSTRAP_ENTRIES: [PyFrozenEntry; 1] = [terminator()];

static STDLIB_ENTRIES: [PyFrozenEntry; 1] = [terminator()];

static TEST_ENTRIES: [PyFrozenEntry; 9] = [
    entry(b"__hello__\0", HELLO, false),
    entry(b"__hello_alias__\0", HELLO, false),
    entry(b"__phello_alias__\0", HELLO, true),
    entry(b"__phello_alias__.spam\0", HELLO, false),
    entry(b"__phello__\0", PHELLO, true),
    entry(b"__phello__.ham\0", EMPTY_STUB, true),
    entry(b"__phello__.ham.eggs\0", EMPTY_STUB, false),
    entry(b"__phello__.spam\0", PHELLO_SPAM, false),
    terminator(),
];

// CPython declares these as `const struct _frozen *` *pointer variables*
// (Include/internal/pycore_import.h), so ctypes' `POINTER(...).in_dll`
// reads a pointer value from the symbol's address — export pointers, not
// the arrays themselves.
#[no_mangle]
pub static _PyImport_FrozenBootstrap: &[PyFrozenEntry; 1] = &BOOTSTRAP_ENTRIES;

#[no_mangle]
pub static _PyImport_FrozenStdlib: &[PyFrozenEntry; 1] = &STDLIB_ENTRIES;

#[no_mangle]
pub static _PyImport_FrozenTest: &[PyFrozenEntry; 9] = &TEST_ENTRIES;

#[cfg(test)]
mod tests {
    use super::*;

    /// The exported C arrays and the canonical `weavepy-vm` list (which
    /// backs `_imp._frozen_module_names()`) must stay in lockstep —
    /// `test_ctypes.test_values.test_frozentable` asserts exactly this
    /// through `ctypes.pythonapi`.
    #[test]
    fn tables_match_canonical_names() {
        let mut exported = Vec::new();
        for table in [
            _PyImport_FrozenBootstrap.as_slice(),
            _PyImport_FrozenStdlib.as_slice(),
            _PyImport_FrozenTest.as_slice(),
        ] {
            for row in table {
                if row.name.is_null() {
                    break;
                }
                // SAFETY: non-terminator rows point at static NUL-terminated
                // names baked into this binary.
                let name = unsafe { core::ffi::CStr::from_ptr(row.name) };
                exported.push(name.to_str().expect("ascii name").to_owned());
                assert!(!row.code.is_null(), "{name:?} has a NULL code payload");
                assert!(
                    row.size.unsigned_abs() > 10,
                    "{name:?} payload too small for the suite's sanity check"
                );
            }
        }
        let canonical = weavepy_vm::frozen_table::frozen_module_names();
        assert_eq!(exported, canonical);
    }

    /// Package-ness in the C table matches the canonical rows.
    #[test]
    fn package_flags_match() {
        let (boot, std_, test) = weavepy_vm::frozen_table::FROZEN_TABLE_NAMES;
        let canonical: Vec<_> = boot.iter().chain(std_).chain(test).collect();
        let mut i = 0;
        for table in [
            _PyImport_FrozenBootstrap.as_slice(),
            _PyImport_FrozenStdlib.as_slice(),
            _PyImport_FrozenTest.as_slice(),
        ] {
            for row in table {
                if row.name.is_null() {
                    break;
                }
                assert_eq!(
                    row.is_package != 0,
                    canonical[i].1,
                    "package flag mismatch for {}",
                    canonical[i].0
                );
                i += 1;
            }
        }
        assert_eq!(i, canonical.len());
    }
}
