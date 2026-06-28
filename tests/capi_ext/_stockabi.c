/*
 * _stockabi — the RFC 0043 wave-1 hermetic proof.
 *
 * Unlike every other in-tree fixture (`_smalltest`, `_ndarray`,
 * `_numpylike`), this module is compiled against the **stock CPython
 * 3.13 headers** (`#include <Python.h>` resolved via the host's real
 * include directory) with the *full* (non-limited) API. That means the
 * compiler inlines CPython's hot-path macros directly into this object
 * file:
 *
 *   - `Py_INCREF`/`Py_DECREF`  → poke `op->ob_refcnt` (+ immortal check)
 *   - `Py_TYPE`/`Py_SIZE`/`Py_REFCNT` → read the object head
 *   - `PyFloat_AS_DOUBLE(op)`   → `*(double*)((char*)op + 16)`
 *   - `PyTuple_GET_ITEM(op, i)` → `((PyTupleObject*)op)->ob_item[i]`
 *   - `Py_TYPE(o) == &PyFloat_Type` → compares against the host's
 *      exported static type symbol
 *
 * When WeavePy loads this `.so` and calls its functions, those inlined
 * reads land on WeavePy's *layout-faithful mirrors* (RFC 0043 WS2). If
 * the mirror bytes match CPython's structs, the wheel "just works"; if
 * they don't, it reads garbage. This is the first time an artifact
 * compiled against stock CPython headers — rather than WeavePy's own
 * `Python.h` — runs under WeavePy.
 *
 * The functions are grouped by which ABI property they prove.
 */

#define PY_SSIZE_T_CLEAN
#include <Python.h>

/* ----- inlined head ops: refcount poke + ownership transfer ----- */

/* `Py_INCREF(o)` is inlined here (pokes ob_refcnt); returning `o`
 * transfers the new reference back to the caller. Proves the faithful
 * 16-byte head + the immortal-refcount sentinel. */
static PyObject *sa_roundtrip(PyObject *self, PyObject *o) {
    (void)self;
    Py_INCREF(o);
    return o;
}

/* ----- inlined type identity across the boundary ----- */

/* `Py_TYPE(o)` reads ob_type at offset 8 and compares against the
 * host's exported `&PyFloat_Type`. Proves type-object identity. */
static PyObject *sa_is_float(PyObject *self, PyObject *o) {
    (void)self;
    return PyBool_FromLong(Py_TYPE(o) == &PyFloat_Type);
}

static PyObject *sa_is_long(PyObject *self, PyObject *o) {
    (void)self;
    return PyBool_FromLong(Py_TYPE(o) == &PyLong_Type);
}

static PyObject *sa_type_name(PyObject *self, PyObject *o) {
    (void)self;
    /* tp_name lives at the faithful offset 24. */
    return PyUnicode_FromString(Py_TYPE(o)->tp_name);
}

/* ----- inlined concrete-field reads (the core of the thesis) ----- */

/* `PyFloat_AS_DOUBLE(o)` is inlined to read `ob_fval` at offset 16. */
static PyObject *sa_double_it(PyObject *self, PyObject *o) {
    (void)self;
    double x = PyFloat_AS_DOUBLE(o);
    return PyFloat_FromDouble(x * 2.0);
}

/* `Py_SIZE(o)` is inlined to read `ob_size` at offset 16 of the var
 * head. Works for tuples/bytes (immutable, filled once at mirror time). */
static PyObject *sa_size(PyObject *self, PyObject *o) {
    (void)self;
    return PyLong_FromSsize_t(Py_SIZE(o));
}

/* `PyTuple_GET_ITEM(o, i)` is inlined to read `ob_item[i]`. Returns a
 * new reference to the first element. */
static PyObject *sa_tuple_first(PyObject *self, PyObject *o) {
    (void)self;
    if (Py_SIZE(o) == 0) {
        Py_RETURN_NONE;
    }
    PyObject *first = PyTuple_GET_ITEM(o, 0);
    Py_INCREF(first);
    return first;
}

/* Sum a tuple by inlined `PyTuple_GET_ITEM` + the function-API
 * `PyLong_AsLong`. Proves the faithful `ob_item[]` tail end-to-end. */
static PyObject *sa_tuple_sum(PyObject *self, PyObject *o) {
    (void)self;
    Py_ssize_t n = Py_SIZE(o);
    long total = 0;
    for (Py_ssize_t i = 0; i < n; i++) {
        PyObject *it = PyTuple_GET_ITEM(o, i); /* borrowed */
        long v = PyLong_AsLong(it);
        if (v == -1 && PyErr_Occurred()) {
            return NULL;
        }
        total += v;
    }
    return PyLong_FromLong(total);
}

/* ----- function-API constructors / parsing ----- */

static PyObject *sa_add(PyObject *self, PyObject *args) {
    (void)self;
    long a = 0, b = 0;
    if (!PyArg_ParseTuple(args, "ll", &a, &b)) {
        return NULL;
    }
    return PyLong_FromLong(a + b);
}

static PyObject *sa_add_doubles(PyObject *self, PyObject *args) {
    (void)self;
    double a = 0, b = 0;
    if (!PyArg_ParseTuple(args, "dd", &a, &b)) {
        return NULL;
    }
    return PyFloat_FromDouble(a + b);
}

static PyObject *sa_echo_str(PyObject *self, PyObject *args) {
    (void)self;
    const char *s = NULL;
    Py_ssize_t n = 0;
    if (!PyArg_ParseTuple(args, "s#", &s, &n)) {
        return NULL;
    }
    return PyUnicode_FromStringAndSize(s, n);
}

static PyObject *sa_make_pair(PyObject *self, PyObject *args) {
    (void)self;
    PyObject *x = NULL, *y = NULL;
    if (!PyArg_ParseTuple(args, "OO", &x, &y)) {
        return NULL;
    }
    return Py_BuildValue("(OO)", x, y);
}

/* Sum a list via the function-call container API (lists are not
 * inline-read in wave 1). */
static PyObject *sa_list_sum(PyObject *self, PyObject *o) {
    (void)self;
    Py_ssize_t n = PyList_Size(o);
    if (n < 0) {
        return NULL;
    }
    long total = 0;
    for (Py_ssize_t i = 0; i < n; i++) {
        PyObject *it = PyList_GetItem(o, i); /* borrowed */
        if (!it) {
            return NULL;
        }
        long v = PyLong_AsLong(it);
        if (v == -1 && PyErr_Occurred()) {
            return NULL;
        }
        total += v;
    }
    return PyLong_FromLong(total);
}

/* ----- C-side allocation + last-ref drop (exercises tp_dealloc) ----- */

/* Build a temporary, then `Py_DECREF` it to zero entirely inside C.
 * The inlined `Py_DECREF` calls the external `_Py_Dealloc`, which reads
 * `Py_TYPE(tmp)->tp_dealloc` at offset 48 and frees the WeavePy mirror.
 * Returns the value the temporary held, proving no corruption. */
static PyObject *sa_alloc_free_cycle(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    long sum = 0;
    for (int i = 0; i < 100; i++) {
        PyObject *tmp = PyLong_FromLong(i);
        if (!tmp) {
            return NULL;
        }
        sum += PyLong_AsLong(tmp);
        Py_DECREF(tmp); /* drops to zero → _Py_Dealloc → tp_dealloc */
    }
    return PyLong_FromLong(sum);
}

/* ----- module definition (static, single-phase) ----- */

static PyMethodDef sa_methods[] = {
    {"roundtrip", sa_roundtrip, METH_O, "Py_INCREF + return (head poke)"},
    {"is_float", sa_is_float, METH_O, "Py_TYPE(o) == &PyFloat_Type"},
    {"is_long", sa_is_long, METH_O, "Py_TYPE(o) == &PyLong_Type"},
    {"type_name", sa_type_name, METH_O, "Py_TYPE(o)->tp_name"},
    {"double_it", sa_double_it, METH_O, "PyFloat_AS_DOUBLE (inlined)"},
    {"size", sa_size, METH_O, "Py_SIZE (inlined)"},
    {"tuple_first", sa_tuple_first, METH_O, "PyTuple_GET_ITEM[0] (inlined)"},
    {"tuple_sum", sa_tuple_sum, METH_O, "sum via PyTuple_GET_ITEM (inlined)"},
    {"add", sa_add, METH_VARARGS, "a + b (long)"},
    {"add_doubles", sa_add_doubles, METH_VARARGS, "a + b (double)"},
    {"echo_str", sa_echo_str, METH_VARARGS, "echo a str"},
    {"make_pair", sa_make_pair, METH_VARARGS, "Py_BuildValue (OO)"},
    {"list_sum", sa_list_sum, METH_O, "sum a list via function API"},
    {"alloc_free_cycle", sa_alloc_free_cycle, METH_NOARGS, "C-side alloc + Py_DECREF to zero"},
    {NULL, NULL, 0, NULL},
};

static struct PyModuleDef sa_module = {
    PyModuleDef_HEAD_INIT,
    "_stockabi",
    "RFC 0043 wave-1 stock-CPython-3.13-ABI proof extension.",
    -1,
    sa_methods,
    NULL,
    NULL,
    NULL,
    NULL,
};

PyMODINIT_FUNC PyInit__stockabi(void) {
    PyObject *m = PyModule_Create(&sa_module);
    if (!m) {
        return NULL;
    }
    PyModule_AddIntConstant(m, "ANSWER", 42);
    PyModule_AddStringConstant(m, "ABI", "cp313");
    return m;
}
