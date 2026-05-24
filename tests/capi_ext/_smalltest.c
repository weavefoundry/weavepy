/*
 * _smalltest — a tiny CPython-style extension module used by
 * `tests/capi_loader.rs` to exercise the WeavePy C-API end-to-end.
 *
 * The module exposes:
 *   - `add(a, b)`        → a + b
 *   - `concat(s1, s2)`   → s1 + s2
 *   - `make_pair(x, y)`  → (x, y)
 *   - `oops(msg)`        → raises ValueError(msg)
 *   - `Counter` class with `Counter().tick()` returning the count
 *   - module-level `VERSION` (a str) and `MAGIC` (an int)
 *
 * This file *only* uses the documented C-API surface (Py_LIMITED_API
 * style). It must be loadable into both real CPython (with cpython
 * 3.13) and WeavePy unchanged.
 */

#include "../../crates/weavepy-capi/include/Python.h"

#include <stdlib.h>
#include <string.h>

/* ---------- module-level functions ---------- */

static PyObject *st_add(PyObject *self, PyObject *args) {
    (void)self;
    long a = 0, b = 0;
    if (!PyArg_ParseTuple(args, "ll", &a, &b)) {
        return NULL;
    }
    return PyLong_FromLong(a + b);
}

static PyObject *st_concat(PyObject *self, PyObject *args) {
    (void)self;
    const char *s1 = NULL;
    const char *s2 = NULL;
    if (!PyArg_ParseTuple(args, "ss", &s1, &s2)) {
        return NULL;
    }
    size_t n1 = strlen(s1);
    size_t n2 = strlen(s2);
    char *buf = (char *)PyMem_Malloc(n1 + n2 + 1);
    memcpy(buf, s1, n1);
    memcpy(buf + n1, s2, n2);
    buf[n1 + n2] = 0;
    PyObject *out = PyUnicode_FromString(buf);
    PyMem_Free(buf);
    return out;
}

static PyObject *st_make_pair(PyObject *self, PyObject *args) {
    (void)self;
    PyObject *x = NULL;
    PyObject *y = NULL;
    if (!PyArg_ParseTuple(args, "OO", &x, &y)) {
        return NULL;
    }
    return Py_BuildValue("(OO)", x, y);
}

static PyObject *st_oops(PyObject *self, PyObject *args) {
    (void)self;
    const char *msg = NULL;
    if (!PyArg_ParseTuple(args, "s", &msg)) {
        return NULL;
    }
    PyErr_SetString(PyExc_ValueError, msg);
    return NULL;
}

static PyObject *st_identity(PyObject *self, PyObject *o) {
    (void)self;
    Py_INCREF(o);
    return o;
}

static PyObject *st_dict_new(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    PyObject *d = PyDict_New();
    PyDict_SetItemString(d, "alpha", PyLong_FromLong(1));
    PyDict_SetItemString(d, "beta", PyLong_FromLong(2));
    return d;
}

/* ---------- Counter class ---------- */

typedef struct {
    PyObject_HEAD
    long count;
} CounterObject;

static PyObject *Counter_tick(PyObject *self, PyObject *args) {
    (void)args;
    CounterObject *c = (CounterObject *)self;
    c->count += 1;
    return PyLong_FromLong(c->count);
}

static PyMethodDef Counter_methods[] = {
    {"tick", Counter_tick, METH_NOARGS, "increment and return the new count"},
    {NULL, NULL, 0, NULL},
};

static PyType_Slot Counter_slots[] = {
    {Py_tp_doc, (void *)"A trivial counter."},
    {Py_tp_methods, Counter_methods},
    {0, NULL},
};

static PyType_Spec Counter_spec = {
    "_smalltest.Counter",
    sizeof(CounterObject),
    0,
    Py_TPFLAGS_DEFAULT,
    Counter_slots,
};

/* ---------- module init ---------- */

static PyMethodDef _smalltest_methods[] = {
    {"add", st_add, METH_VARARGS, "add two longs"},
    {"concat", st_concat, METH_VARARGS, "concatenate two strings"},
    {"make_pair", st_make_pair, METH_VARARGS, "build a 2-tuple"},
    {"oops", st_oops, METH_VARARGS, "raise ValueError"},
    {"identity", st_identity, METH_O, "return arg unchanged"},
    {"dict_new", st_dict_new, METH_NOARGS, "build a sample dict"},
    {NULL, NULL, 0, NULL},
};

static struct PyModuleDef _smalltest_def = {
    PyModuleDef_HEAD_INIT
    "_smalltest",
    "Tiny WeavePy C-API smoke test extension",
    -1,
    _smalltest_methods,
    NULL, NULL, NULL, NULL,
};

PyObject *PyInit__smalltest(void);

PyObject *PyInit__smalltest(void) {
    PyObject *m = PyModule_Create(&_smalltest_def);
    if (!m) return NULL;

    PyObject *type = PyType_FromSpec(&Counter_spec);
    if (!type) {
        Py_DECREF(m);
        return NULL;
    }
    if (PyModule_AddObject(m, "Counter", type) != 0) {
        Py_DECREF(type);
        Py_DECREF(m);
        return NULL;
    }

    PyModule_AddStringConstant(m, "VERSION", "1.0");
    PyModule_AddIntConstant(m, "MAGIC", 0xC0DE);

    return m;
}
