//! Set removal through the C API preserves ownership and avoids pop callbacks.
use std::ffi::CString;
use weavepy_capi::containers::{PyDict_SetItemString, PySet_Discard, PySet_Pop, PySet_Size};
use weavepy_capi::errors::{PyErr_Clear, PyErr_Occurred};
use weavepy_capi::lifecycle::{Py_FinalizeEx, Py_Initialize};
use weavepy_capi::module::{PyImport_AddModule, PyModule_GetDict};
use weavepy_capi::object::{PyObject, Py_DecRef};
use weavepy_capi::pythonrun::{PyRun_SimpleString, PyRun_String, Py_eval_input};

fn run(source: &str) {
    let source = CString::new(source).unwrap();
    assert_eq!(unsafe { PyRun_SimpleString(source.as_ptr()) }, 0);
}

unsafe fn eval(source: &str, globals: *mut PyObject) -> *mut PyObject {
    let source = CString::new(source).unwrap();
    let value = unsafe { PyRun_String(source.as_ptr(), Py_eval_input, globals, globals) };
    assert!(!value.is_null(), "C API evaluation failed");
    value
}

#[test]
fn set_pop_and_discard_preserve_native_ownership() {
    // Keep initialization and finalization in one test, as for embedding.
    unsafe { Py_Initialize() };
    run(include_str!("../../../tests/regrtest/test_set_removals.py"));
    run("hashes = []\nclass Key:\n    def __init__(self, value): self.value = value\n    def __hash__(self):\n        hashes.append(self.value)\n        return 7\n    def __eq__(self, other): return self.value == other.value\nowners = [Key(i) for i in range(12)]\nvalues = set(owners)\nhashes.clear()\nseen = []\n");
    unsafe {
        let name = CString::new("__main__").unwrap();
        let module = PyImport_AddModule(name.as_ptr());
        assert!(!module.is_null());
        let globals = PyModule_GetDict(module);
        assert!(!globals.is_null());
        let values = eval("values", globals);
        let popped_name = CString::new("c_popped").unwrap();
        for remaining in (0..12).rev() {
            let popped = PySet_Pop(values);
            assert!(!popped.is_null());
            assert_eq!(PySet_Size(values), remaining);
            assert!(PyErr_Occurred().is_null());
            assert_eq!(
                PyDict_SetItemString(globals, popped_name.as_ptr(), popped),
                0
            );
            Py_DecRef(popped);
            run("assert not hashes\nassert any(c_popped is owner for owner in owners)\nassert all(c_popped is not previous for previous in seen)\nseen.append(c_popped)\n");
        }
        assert!(PySet_Pop(values).is_null());
        assert!(!PyErr_Occurred().is_null());
        PyErr_Clear();
        Py_DecRef(values);
        run("values = set(range(97))\n");
        let values = eval("values", globals);
        for i in 0..97 {
            let key = eval(&i.to_string(), globals);
            assert_eq!(PySet_Discard(values, key), 1);
            assert_eq!(PySet_Size(values), 96 - i);
            assert_eq!(PySet_Discard(values, key), 0);
            assert!(PyErr_Occurred().is_null());
            Py_DecRef(key);
        }
        run("assert not values\nassert len(seen) == 12\n");
        Py_DecRef(values);
        assert_eq!(Py_FinalizeEx(), 0);
    }
}
