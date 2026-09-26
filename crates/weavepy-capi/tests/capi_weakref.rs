//! Weakref identities, saved methods, and referent lifetimes through the C API.
use std::ffi::CString;
use std::ptr;
use weavepy_capi::abi313::{PyWeakref_GetRef, PyWeakref_NewRef};
use weavepy_capi::abi314::PyUnstable_Object_ClearWeakRefsNoCallbacks;
use weavepy_capi::abstract_::{PyObject_CallNoArgs, PyObject_GetAttrString};
use weavepy_capi::containers::PyDict_SetItemString;
use weavepy_capi::lifecycle::{Py_FinalizeEx, Py_Initialize};
use weavepy_capi::module::{PyImport_AddModule, PyModule_GetDict};
use weavepy_capi::object::{clone_object, into_owned, PyObject, Py_DecRef};
use weavepy_capi::pythonrun::{PyRun_SimpleString, PyRun_String, Py_eval_input};

fn run(source: &str) {
    let source = CString::new(source).unwrap();
    assert_eq!(unsafe { PyRun_SimpleString(source.as_ptr()) }, 0);
}

unsafe fn eval(source: &str, globals: *mut PyObject) -> *mut PyObject {
    let source = CString::new(source).unwrap();
    let object = unsafe { PyRun_String(source.as_ptr(), Py_eval_input, globals, globals) };
    assert!(!object.is_null(), "C API evaluation failed");
    object
}

#[test]
fn weakref_identity_methods_and_referent_lifetime() {
    // Keep one interpreter lifecycle in this integration process.
    unsafe { Py_Initialize() };
    run("import gc, weakref\nclass Node: pass\nroot = Node()\nreference = weakref.ref(root)\nclass Ref(weakref.ref):\n    __slots__ = ('label',)\nsubreference = Ref(root)\nsubreference.label = 47\n");
    unsafe {
        let name = CString::new("__main__").unwrap();
        let module = PyImport_AddModule(name.as_ptr());
        assert!(!module.is_null());
        let globals = PyModule_GetDict(module);
        assert!(!globals.is_null());
        let target = eval("root", globals);
        let reference = eval("reference", globals);
        let subclass = eval("subreference", globals);
        let roundtrip = into_owned(clone_object(reference));
        assert_eq!(roundtrip, reference);
        Py_DecRef(roundtrip);
        let from_c = PyWeakref_NewRef(target, ptr::null_mut());
        assert!(!from_c.is_null());
        let name = CString::new("c_reference").unwrap();
        assert_eq!(PyDict_SetItemString(globals, name.as_ptr(), from_c), 0);
        let call_name = CString::new("__call__").unwrap();
        let saved_call = PyObject_GetAttrString(from_c, call_name.as_ptr());
        assert!(!saved_call.is_null());
        let name = CString::new("saved_call").unwrap();
        assert_eq!(PyDict_SetItemString(globals, name.as_ptr(), saved_call), 0);
        run("assert saved_call.__self__ is c_reference\n");
        let result = PyObject_CallNoArgs(saved_call);
        assert_eq!(result, target);
        Py_DecRef(result);
        for wrapper in [reference, subclass, from_c] {
            let mut referent = ptr::null_mut();
            assert_eq!(PyWeakref_GetRef(wrapper, &raw mut referent), 1);
            assert_eq!(referent, target);
            Py_DecRef(referent);
            let boxed_again = into_owned(clone_object(wrapper));
            assert_eq!(boxed_again, wrapper);
            Py_DecRef(boxed_again);
        }
        run("assert reference() is subreference() is c_reference() is root\nassert subreference.label == 47\nassert any(r is c_reference for r in weakref.getweakrefs(root))\n");
        Py_DecRef(target);
        run("del root\ngc.collect()\nassert reference() is subreference() is c_reference() is saved_call() is None\nassert subreference.label == 47\n");
        let result = PyObject_CallNoArgs(saved_call);
        assert!(!result.is_null());
        assert!(matches!(
            clone_object(result),
            weavepy_vm::object::Object::None
        ));
        Py_DecRef(result);
        for wrapper in [reference, subclass, from_c] {
            let mut referent = ptr::null_mut();
            assert_eq!(PyWeakref_GetRef(wrapper, &raw mut referent), 0);
            assert!(referent.is_null());
            Py_DecRef(wrapper);
        }
        Py_DecRef(saved_call);
        run("clear_root = Node()\nclear_events = []\nclear_refs = [weakref.ref(clear_root, lambda ref, i=i: clear_events.append(i)) for i in range(6)]\nclear_plain = weakref.ref(clear_root)\n");
        let target = eval("clear_root", globals);
        PyUnstable_Object_ClearWeakRefsNoCallbacks(target);
        run("assert all(ref() is None for ref in clear_refs)\nassert clear_plain() is None\nassert weakref.getweakrefs(clear_root) == []\ngc.collect()\nassert clear_events == []\n");
        Py_DecRef(target);
        run("del clear_root\ngc.collect()\nassert clear_events == []\n");
        run(include_str!(
            "../../../tests/regrtest/test_weakref_no_callback_clear.py"
        ));
        run(include_str!(
            "../../../tests/regrtest/test_weakref_saved_methods.py"
        ));
        assert_eq!(Py_FinalizeEx(), 0);
    }
}
