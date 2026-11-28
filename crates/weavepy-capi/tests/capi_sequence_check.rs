//! `PySequence_Check` and the sequence calls on an `array.array`.
//!
//! The `array` class is written in Python with its item methods served
//! natively. To C code it is what CPython's C type is: a sequence, with a
//! size, items, and an iterator (Pillow's `putdata(array(...))` asks
//! `PySequence_Check` first and refuses anything else).
use std::ffi::CString;
use weavepy_capi::abstract_::{
    PyIter_Next, PyObject_GetIter, PySequence_Check, PySequence_GetItem, PySequence_Size,
};
use weavepy_capi::errors::PyErr_Occurred;
use weavepy_capi::lifecycle::{Py_FinalizeEx, Py_Initialize};

use weavepy_capi::module::{PyImport_AddModule, PyModule_GetDict};
use weavepy_capi::numbers::PyLong_AsLong;
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
fn an_array_is_a_sequence_to_c_code() {
    // Keep initialization and finalization in one test, as for embedding.
    unsafe { Py_Initialize() };
    run("import array\n\
         ints = array.array('i', [10, 20, 30])\n\
         floats = array.array('f', [0.0]) * 100\n\
         class Sub(array.array):\n    pass\n\
         sub = Sub('B', b'abc')\n\
         class Items:\n    def __getitem__(self, i):\n        return i\n\
         items = Items()\n");
    unsafe {
        let name = CString::new("__main__").unwrap();
        let module = PyImport_AddModule(name.as_ptr());
        assert!(!module.is_null());
        let globals = PyModule_GetDict(module);
        assert!(!globals.is_null());
        for (source, want) in [
            ("ints", 1),
            ("floats", 1),
            ("sub", 1),
            ("items", 1),
            ("[1, 2]", 1),
            ("(1, 2)", 1),
            ("'text'", 1),
            ("b'bytes'", 1),
            ("range(3)", 1),
            ("{1: 2}", 0),
            ("{1, 2}", 0),
            ("5", 0),
            ("None", 0),
        ] {
            let value = eval(source, globals);
            assert_eq!(PySequence_Check(value), want, "PySequence_Check({source})");
            Py_DecRef(value);
        }
        let ints = eval("ints", globals);
        assert_eq!(PySequence_Size(ints), 3);
        for (i, want) in [10, 20, 30].into_iter().enumerate() {
            let item = PySequence_GetItem(ints, i as isize);
            assert!(!item.is_null());
            assert_eq!(PyLong_AsLong(item), want);
            Py_DecRef(item);
        }
        let last = PySequence_GetItem(ints, -1);
        assert!(!last.is_null());
        assert_eq!(PyLong_AsLong(last), 30);
        Py_DecRef(last);
        let it = PyObject_GetIter(ints);
        assert!(!it.is_null());
        let mut total = 0;
        loop {
            let item = PyIter_Next(it);
            if item.is_null() {
                break;
            }
            total += PyLong_AsLong(item);
            Py_DecRef(item);
        }
        assert!(PyErr_Occurred().is_null());
        assert_eq!(total, 60);
        Py_DecRef(it);
        Py_DecRef(ints);
        let floats = eval("floats", globals);
        assert_eq!(PySequence_Size(floats), 100);
        Py_DecRef(floats);
        assert_eq!(Py_FinalizeEx(), 0);
    }
}
