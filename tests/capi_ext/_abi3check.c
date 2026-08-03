/*
 * _abi3check — the RFC 0056 WS5 limited-API (abi3) proof.
 *
 * Compiled against the host's stock CPython 3.13 headers with
 * `Py_LIMITED_API = 0x030D0000`, so — unlike the `_stock*` fixtures —
 * every object access goes through *exported functions*, never inlined
 * macros. This is exactly the surface a PyO3 `abi3-py313` wheel binds
 * (pydantic-core, cryptography's `_rust`, orjson under abi3):
 *
 *   - PEP 489 multiphase init: `PyModuleDef_Init` + `Py_mod_exec` slot
 *     (PyO3's `#[pymodule]` two-phase path).
 *   - `PyType_FromSpec` heap type with `Py_tp_new`/`Py_tp_methods`
 *     (PyO3's `#[pyclass]`).
 *   - `PyObject_Vectorcall` under its limited-API spelling (3.12+).
 *   - `PyGILState_Ensure`/`Release`, `PyInterpreterState_Get`,
 *     `Py_EnterRecursiveCall`/`Py_LeaveRecursiveCall` — the runtime
 *     state surface `pyo3-ffi` imports at module init.
 *
 * The paired integration test (`crates/weavepy-capi/tests/
 * capi_abi3check.rs`) dlopens the artifact into WeavePy and drives it.
 */

#define Py_LIMITED_API 0x030D0000
#define PY_SSIZE_T_CLEAN
#include <Python.h>

/* ----- PyType_FromSpec heap type (the #[pyclass] shape) ----- */

typedef struct {
    PyObject_HEAD
    long count;
} CounterObject;

static PyObject *counter_incr(PyObject *self, PyObject *Py_UNUSED(ignored)) {
    CounterObject *c = (CounterObject *)self;
    c->count++;
    return PyLong_FromLong(c->count);
}

static PyObject *counter_value(PyObject *self, PyObject *Py_UNUSED(ignored)) {
    return PyLong_FromLong(((CounterObject *)self)->count);
}

static PyMethodDef counter_methods[] = {
    {"incr", counter_incr, METH_NOARGS, "bump and return the count"},
    {"value", counter_value, METH_NOARGS, "current count"},
    {NULL, NULL, 0, NULL},
};

static PyType_Slot counter_slots[] = {
    {Py_tp_new, PyType_GenericNew},
    {Py_tp_methods, counter_methods},
    {0, NULL},
};

static PyType_Spec counter_spec = {
    .name = "_abi3check.Counter",
    .basicsize = sizeof(CounterObject),
    .itemsize = 0,
    .flags = Py_TPFLAGS_DEFAULT,
    .slots = counter_slots,
};

/* ----- runtime-state surface (what pyo3-ffi binds at init) ----- */

/* GILState round-trip: Ensure/Release from an already-attached thread
 * (PyO3's `Python::with_gil` on the main thread). */
static PyObject *ac_gil_roundtrip(PyObject *Py_UNUSED(self),
                                  PyObject *Py_UNUSED(ignored)) {
    PyGILState_STATE st = PyGILState_Ensure();
    PyGILState_Release(st);
    Py_RETURN_TRUE;
}

/* `PyInterpreterState_Get()` must return a non-NULL handle (PyO3 keys
 * its per-interpreter module state off it). */
static PyObject *ac_interp_alive(PyObject *Py_UNUSED(self),
                                 PyObject *Py_UNUSED(ignored)) {
    return PyBool_FromLong(PyInterpreterState_Get() != NULL);
}

/* Recursion-guard pair, balanced. */
static PyObject *ac_recursion_guard(PyObject *Py_UNUSED(self),
                                    PyObject *Py_UNUSED(ignored)) {
    if (Py_EnterRecursiveCall(" in _abi3check") != 0) {
        return NULL;
    }
    Py_LeaveRecursiveCall();
    Py_RETURN_TRUE;
}

/* ----- PyObject_Vectorcall under the abi3 spelling ----- */

/* vectorcall_call(f, a, b) -> f(a, b): builds the flat args array and
 * dispatches through the exported (not macro) `PyObject_Vectorcall`. */
static PyObject *ac_vectorcall_call(PyObject *Py_UNUSED(self), PyObject *args) {
    PyObject *f, *a, *b;
    if (!PyArg_ParseTuple(args, "OOO", &f, &a, &b)) {
        return NULL;
    }
    PyObject *stack[2] = {a, b};
    return PyObject_Vectorcall(f, stack, 2, NULL);
}

/* Function-call (no macro) object access: sum a sequence of ints via
 * `PySequence_GetItem` + `PyLong_AsLong` — the limited-API idiom where
 * full-API code would use `PyList_GET_ITEM`. */
static PyObject *ac_sum_ints(PyObject *Py_UNUSED(self), PyObject *seq) {
    Py_ssize_t n = PySequence_Size(seq);
    if (n < 0) {
        return NULL;
    }
    long total = 0;
    for (Py_ssize_t i = 0; i < n; i++) {
        PyObject *item = PySequence_GetItem(seq, i);
        if (item == NULL) {
            return NULL;
        }
        long v = PyLong_AsLong(item);
        Py_DECREF(item);
        if (v == -1 && PyErr_Occurred()) {
            return NULL;
        }
        total += v;
    }
    return PyLong_FromLong(total);
}

static PyMethodDef abi3check_methods[] = {
    {"gil_roundtrip", ac_gil_roundtrip, METH_NOARGS, NULL},
    {"interp_alive", ac_interp_alive, METH_NOARGS, NULL},
    {"recursion_guard", ac_recursion_guard, METH_NOARGS, NULL},
    {"vectorcall_call", ac_vectorcall_call, METH_VARARGS, NULL},
    {"sum_ints", ac_sum_ints, METH_O, NULL},
    {NULL, NULL, 0, NULL},
};

/* ----- PEP 489 multiphase init (PyO3's two-phase module path) ----- */

static int abi3check_exec(PyObject *module) {
    /* Prove the exec slot ran and `PyModule_AddIntConstant` works. */
    if (PyModule_AddIntConstant(module, "EXEC_RAN", 1) < 0) {
        return -1;
    }
    if (PyModule_AddStringConstant(module, "ABI", "abi3") < 0) {
        return -1;
    }
    PyObject *counter_type = PyType_FromSpec(&counter_spec);
    if (counter_type == NULL) {
        return -1;
    }
    if (PyModule_AddObject(module, "Counter", counter_type) < 0) {
        Py_DECREF(counter_type);
        return -1;
    }
    return 0;
}

static PyModuleDef_Slot abi3check_slots[] = {
    {Py_mod_exec, abi3check_exec},
    {0, NULL},
};

static struct PyModuleDef abi3check_module = {
    PyModuleDef_HEAD_INIT,
    .m_name = "_abi3check",
    .m_doc = "WeavePy limited-API (abi3) regression fixture (RFC 0056 WS5).",
    .m_size = 0,
    .m_methods = abi3check_methods,
    .m_slots = abi3check_slots,
};

PyMODINIT_FUNC PyInit__abi3check(void) {
    return PyModuleDef_Init(&abi3check_module);
}
