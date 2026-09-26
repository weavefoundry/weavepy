//! Native adapters must expose the iterator slots read directly by Cython.

use std::ffi::{c_void, CString};
use weavepy_capi::abstract_::PyObject_GetIter;
use weavepy_capi::containers::{PyDict_SetItemString, PyList_Append, PyList_New};
use weavepy_capi::errors::{
    PyErr_Clear, PyErr_ExceptionMatches, PyErr_GetRaisedException, PyErr_Occurred,
    PyExc_StopIteration,
};
use weavepy_capi::lifecycle::{Py_FinalizeEx, Py_Initialize};
use weavepy_capi::module::{PyImport_AddModule, PyModule_GetDict};
use weavepy_capi::object::{PyObject, Py_DecRef};
use weavepy_capi::pythonrun::{PyRun_SimpleString, PyRun_String, Py_eval_input};

type IterSlot = unsafe extern "C" fn(*mut PyObject) -> *mut PyObject;

fn run(source: &str) {
    let source = CString::new(source).unwrap();
    assert_eq!(unsafe { PyRun_SimpleString(source.as_ptr()) }, 0);
}

unsafe fn eval(source: &str, globals: *mut PyObject) -> *mut PyObject {
    let source = CString::new(source).unwrap();
    let result = unsafe { PyRun_String(source.as_ptr(), Py_eval_input, globals, globals) };
    assert!(!result.is_null());
    result
}

unsafe fn slot(pointer: *mut c_void) -> IterSlot {
    assert!(!pointer.is_null(), "iterator is missing a C slot");
    unsafe { std::mem::transmute(pointer) }
}

unsafe fn publish(result: *mut PyObject, globals: *mut PyObject) {
    assert!(!result.is_null());
    unsafe {
        assert_eq!(PyDict_SetItemString(globals, c"result".as_ptr(), result), 0);
        Py_DecRef(result);
    }
}

fn exercise_slots() {
    unsafe { Py_Initialize() };
    run("import itertools\ndef fails():\n    yield 11\n    raise ValueError('iteration failed')\ndef returns():\n    yield 11\n    return 17\n");
    unsafe {
        let module = PyImport_AddModule(c"__main__".as_ptr());
        assert!(!module.is_null());
        let globals = PyModule_GetDict(module);
        assert!(!globals.is_null());
        for (expression, expected) in [
            ("iter([11, 22])", "[11, 22]"),
            ("itertools.chain.from_iterable([[11], [22]])", "[11, 22]"),
            ("itertools.islice(range(5), 1, 4)", "[1, 2, 3]"),
            ("itertools.repeat(17, 2)", "[17, 17]"),
            ("itertools.zip_longest([1], [2, 3])", "[(1, 2), (None, 3)]"),
            ("(x for x in [11, 22])", "[11, 22]"),
            ("itertools.chain.from_iterable([])", "[]"),
        ] {
            let source = eval(expression, globals);
            let iterator = PyObject_GetIter(source);
            Py_DecRef(source);
            assert!(!iterator.is_null(), "{expression}");
            let iter = slot((*(*iterator).ob_type).tp_iter);
            let next = slot((*(*iterator).ob_type).tp_iternext);
            let same = iter(iterator);
            assert_eq!(
                same, iterator,
                "iterator slot must return a new self reference"
            );
            Py_DecRef(same);
            let values = PyList_New(0);
            assert!(!values.is_null());
            loop {
                let item = next(iterator);
                if item.is_null() {
                    break;
                }
                assert_eq!(PyList_Append(values, item), 0);
                Py_DecRef(item);
            }
            if PyErr_ExceptionMatches(PyExc_StopIteration) != 0 {
                PyErr_Clear();
            }
            assert!(PyErr_Occurred().is_null(), "{expression}");
            assert!(next(iterator).is_null(), "exhaustion must persist");
            if PyErr_ExceptionMatches(PyExc_StopIteration) != 0 {
                PyErr_Clear();
            }
            assert!(PyErr_Occurred().is_null());
            Py_DecRef(iterator);
            publish(values, globals);
            run(&format!("assert result == {expected}, result\ndel result"));
        }
        for (expression, assertion) in [
            (
                "itertools.chain.from_iterable([fails()])",
                "assert type(result) is ValueError\nassert str(result) == 'iteration failed'",
            ),
            (
                "returns()",
                "assert type(result) is StopIteration\nassert result.value == 17",
            ),
        ] {
            let source = eval(expression, globals);
            let iterator = PyObject_GetIter(source);
            Py_DecRef(source);
            assert!(!iterator.is_null());
            let next = slot((*(*iterator).ob_type).tp_iternext);
            publish(next(iterator), globals);
            run("assert result == 11\ndel result");
            assert!(next(iterator).is_null());
            assert!(!PyErr_Occurred().is_null());
            let error = PyErr_GetRaisedException();
            publish(error, globals);
            run(assertion);
            run("del result");
            Py_DecRef(iterator);
            assert!(PyErr_Occurred().is_null());
        }
        assert_eq!(Py_FinalizeEx(), 0);
    }
}

#[test]
fn native_iterators_expose_c_slots() {
    weavepy_capi::force_link();
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(exercise_slots)
        .expect("spawn iterator test")
        .join()
        .expect("iterator test panicked");
}
