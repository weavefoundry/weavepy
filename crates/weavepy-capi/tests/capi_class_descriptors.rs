//! Exercise C attribute APIs without depending on platform ctypes support.

use std::ffi::CString;
use std::ptr;
use weavepy_capi::abstract_::{
    PyObject_GetAttr, PyObject_GetAttrString, _PyObject_LookupAttr, _PyObject_LookupAttrId,
};
use weavepy_capi::containers::PyDict_SetItemString;
use weavepy_capi::errors::{PyErr_GetRaisedException, PyErr_Occurred};
use weavepy_capi::lifecycle::{Py_FinalizeEx, Py_Initialize};
use weavepy_capi::module::{PyImport_AddModule, PyModule_GetDict};
use weavepy_capi::object::{PyObject, Py_DecRef};
use weavepy_capi::pythonrun::{PyRun_SimpleString, PyRun_String, Py_eval_input};
use weavepy_capi::wave4::PyObject_GetOptionalAttr;
use weavepy_capi::wave5::PyObject_GetOptionalAttrString;

fn run(source: &str) {
    let source = CString::new(source).unwrap();
    assert_eq!(unsafe { PyRun_SimpleString(source.as_ptr()) }, 0);
}

unsafe fn eval(source: &str, globals: *mut PyObject) -> *mut PyObject {
    let source = CString::new(source).unwrap();
    let result = unsafe { PyRun_String(source.as_ptr(), Py_eval_input, globals, globals) };
    assert!(!result.is_null(), "C API evaluation failed");
    result
}

// Transfer a new C reference into Python globals, then check its Python value.
unsafe fn check_result(result: *mut PyObject, globals: *mut PyObject, assertion: &str) {
    assert!(!result.is_null());
    assert!(unsafe { PyErr_Occurred() }.is_null());
    unsafe {
        assert_eq!(PyDict_SetItemString(globals, c"result".as_ptr(), result), 0);
        Py_DecRef(result);
    }
    run(assertion);
    run("del result");
}

unsafe fn check_error(globals: *mut PyObject) {
    assert!(!unsafe { PyErr_Occurred() }.is_null());
    let error = unsafe { PyErr_GetRaisedException() };
    unsafe {
        check_result(
            error,
            globals,
            "assert type(result) is ValueError\nassert str(result) == 'descriptor failed'",
        );
    }
}

#[derive(Clone, Copy, Debug)]
enum Lookup {
    Object,
    String,
    OptionalObject,
    OptionalString,
    PrivateObject,
    PrivateString,
}

impl Lookup {
    unsafe fn get(
        self,
        cls: *mut PyObject,
        name: &str,
        globals: *mut PyObject,
        result: *mut *mut PyObject,
    ) -> i32 {
        let string_name = CString::new(name).unwrap();
        let object_name = unsafe { eval(&format!("{name:?}"), globals) };
        let status = unsafe {
            match self {
                Self::Object | Self::String => {
                    *result = if matches!(self, Self::Object) {
                        PyObject_GetAttr(cls, object_name)
                    } else {
                        PyObject_GetAttrString(cls, string_name.as_ptr())
                    };
                    if (*result).is_null() {
                        -1
                    } else {
                        1
                    }
                }
                Self::OptionalObject => PyObject_GetOptionalAttr(cls, object_name, result),
                Self::OptionalString => {
                    PyObject_GetOptionalAttrString(cls, string_name.as_ptr(), result)
                }
                Self::PrivateObject => _PyObject_LookupAttr(cls, object_name, result),
                // This legacy WeavePy helper takes a string, not CPython's
                // _Py_Identifier structure. Test its existing ABI directly.
                Self::PrivateString => _PyObject_LookupAttrId(cls, string_name.as_ptr(), result),
            }
        };
        unsafe { Py_DecRef(object_name) };
        status
    }
}

fn exercise_descriptors() {
    unsafe { Py_Initialize() };
    run(r"
calls = []
class Descriptor:
    def __get__(self, instance, owner):
        calls.append((instance, owner))
        return owner.__name__
class Meta(type): pass
class Base(metaclass=Meta): value = Descriptor()
class Child(Base): pass
class Hybrid:
    def __get__(self, instance, owner):
        return lambda value: (owner, instance, value)
Base.method = Hybrid()
class Raising:
    def __get__(self, instance, owner):
        raise ValueError('descriptor failed')
Base.bad = Raising()
class Missing:
    def __get__(self, instance, owner):
        raise AttributeError('missing through descriptor')
Base.missing_descriptor = Missing()
");
    unsafe {
        let module = PyImport_AddModule(c"__main__".as_ptr());
        assert!(!module.is_null());
        let globals = PyModule_GetDict(module);
        assert!(!globals.is_null());
        for lookup in [
            Lookup::Object,
            Lookup::String,
            Lookup::OptionalObject,
            Lookup::OptionalString,
            Lookup::PrivateObject,
            Lookup::PrivateString,
        ] {
            run("Descriptor.__get__ = lambda self, instance, owner: (calls.append((instance, owner)), owner.__name__)[1]");
            for class_name in ["Base", "Child"] {
                run(&format!("cls = {class_name}\ncalls.clear()"));
                let cls = eval("cls", globals);
                let mut result = ptr::null_mut();
                assert_eq!(
                    lookup.get(cls, "value", globals, &raw mut result),
                    1,
                    "{lookup:?}"
                );
                check_result(
                    result,
                    globals,
                    "assert result == cls.__name__\nassert calls == [(None, cls)]",
                );
                assert_eq!(lookup.get(cls, "method", globals, &raw mut result), 1);
                check_result(result, globals, "assert result(17) == (cls, None, 17)");
                assert_eq!(lookup.get(cls, "bad", globals, &raw mut result), -1);
                assert!(result.is_null());
                check_error(globals);
                if !matches!(lookup, Lookup::Object | Lookup::String) {
                    for missing in ["absent", "missing_descriptor"] {
                        // Failure must clear an existing output, not leave it stale.
                        result = cls;
                        assert_eq!(lookup.get(cls, missing, globals, &raw mut result), 0);
                        assert!(result.is_null());
                        assert!(PyErr_Occurred().is_null());
                    }
                }
                Py_DecRef(cls);
            }
            run("Descriptor.__get__ = lambda self, instance, owner: ('changed', owner)");
            let child = eval("Child", globals);
            let mut result = ptr::null_mut();
            assert_eq!(lookup.get(child, "value", globals, &raw mut result), 1);
            check_result(result, globals, "assert result == ('changed', Child)");
            if matches!(lookup, Lookup::PrivateObject | Lookup::PrivateString) {
                assert_eq!(lookup.get(child, "value", globals, ptr::null_mut()), 1);
                assert!(PyErr_Occurred().is_null());
            }
            Py_DecRef(child);
        }
        assert_eq!(Py_FinalizeEx(), 0);
    }
}

#[test]
fn class_descriptors_follow_python_lookup() {
    weavepy_capi::force_link();
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(exercise_descriptors)
        .expect("spawn descriptor test")
        .join()
        .expect("descriptor test panicked");
}
