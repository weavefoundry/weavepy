/*
 * _stockarray — the RFC 0045 (binary-ABI inline storage + the numpy
 * array C-API surface, wave 3) hermetic proof.
 *
 * Like `_stockabi.c` / `_stocktype.c`, this module is compiled against
 * the host's **stock CPython 3.13 headers** with the *full* (non-limited)
 * API, so it sees the genuine 416-byte `PyTypeObject`, the real
 * `PyMemberDef` layout + `T_*` codes (`<structmember.h>`), the inlined
 * head macros, and the real `PyCapsule_*` surface. Where `_stockabi`
 * proved object *mirrors* and `_stocktype` proved the *type* machinery
 * (with state kept in `__dict__`), this fixture proves the piece wave 2
 * explicitly deferred: a stock type that reads its own fields **inline**,
 * `((StockArrayObject *)self)->field`, at fixed `tp_basicsize` offsets —
 * the `PyArrayObject` shape — and the numpy array C-API surface that
 * rides on it.
 *
 * It exercises, all against WeavePy's faithful inline instance body:
 *
 *   - **Inline `tp_basicsize` storage**: `StockArray(n)` writes its
 *     fields in `tp_init`; a *later, separate* C call (`sum()`) reads
 *     them back — proof that the body is the *same* block across
 *     crossings (a fresh per-crossing box would read zeros / crash).
 *   - **`tp_members`**: `nd` / `length` (READONLY) and `typenum`
 *     (writable) project the inline fields to/from Python at their
 *     declared `offsetof`, reading the very bytes `tp_init` wrote.
 *   - **A faithful `tp_dealloc`** that frees `self->data` and then calls
 *     `PyObject_Free(self)` — the canonical stock shape; WeavePy absorbs
 *     the `tp_free` on an instance body (the body is owned by the native
 *     instance) and frees the buffer.
 *   - **Array interchange**: `__array_interface__` (a dict) and
 *     `__array_struct__` (a `PyCapsule` wrapping a `PyArrayInterface`),
 *     both reading the inline `data` pointer.
 *   - **The array-C-API *capsule* pattern**: the module installs a
 *     `void **` function table at the well-known dotted name
 *     `_stockarray._ARRAY_API`; `capi_roundtrip()` re-imports it the way
 *     `import_array()` does — `PyCapsule_Import("_stockarray._ARRAY_API")`
 *     → a `void **` table → call through `table[i]` — and builds a fresh
 *     array through it.
 */

#define PY_SSIZE_T_CLEAN
#include <Python.h>
#include <structmember.h>

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* ================================================================== */
/* StockArray — a PyArrayObject-shaped inline-storage type.           */
/* ================================================================== */

typedef struct {
    PyObject_HEAD
    int nd;            /* number of dimensions (always 1 here)       */
    Py_ssize_t length; /* element count                               */
    double *data;      /* malloc'd buffer of `length` doubles         */
    int typenum;       /* dtype sentinel (writable, like numpy dtype) */
} StockArrayObject;

/* Live-instance counter so the test can observe that `tp_dealloc`
 * (and therefore the faithful buffer free) actually runs. */
static long g_live = 0;

/* Monotonic total-`tp_dealloc` counter. Unlike `g_live` (which other,
 * concurrently-running tests in the same process push up and down through
 * this same shared `.so`), this only ever increases, so a death test can
 * prove *its* instance was collected with a race-free `after >= before + 1`
 * — concurrent deallocs by other tests can only push it higher. */
static long g_deallocs = 0;

static PyTypeObject StockArray_Type; /* forward */

static int StockArray_init(PyObject *self, PyObject *args, PyObject *kwds) {
    (void)kwds;
    Py_ssize_t n = 0;
    if (!PyArg_ParseTuple(args, "n", &n)) {
        return -1;
    }
    if (n < 0) {
        PyErr_SetString(PyExc_ValueError, "StockArray: length must be >= 0");
        return -1;
    }
    StockArrayObject *a = (StockArrayObject *)self;
    double *buf = (double *)malloc((size_t)(n > 0 ? n : 1) * sizeof(double));
    if (!buf) {
        PyErr_NoMemory();
        return -1;
    }
    for (Py_ssize_t i = 0; i < n; i++) {
        buf[i] = (double)i; /* [0, 1, 2, ... n-1] */
    }
    /* Write straight into the inline fields of our own body. */
    a->nd = 1;
    a->length = n;
    a->data = buf;
    a->typenum = 12; /* sentinel for "float64" */
    g_live += 1;
    return 0;
}

static void StockArray_dealloc(PyObject *self) {
    StockArrayObject *a = (StockArrayObject *)self;
    free(a->data);
    a->data = NULL;
    g_live -= 1;
    g_deallocs += 1;
    /* The canonical stock tail. Under CPython this releases the object
     * storage; under WeavePy the body is owned by the native instance,
     * so this `tp_free`-equivalent is absorbed (and the block is freed
     * when the instance is collected). */
    PyObject_Free(self);
}

/* sum() — read the inline `data`/`length` fields (written by a *prior*
 * `tp_init` call) and total them. The headline inline-storage proof. */
static PyObject *StockArray_sum(PyObject *self, PyObject *ignored) {
    (void)ignored;
    StockArrayObject *a = (StockArrayObject *)self;
    double acc = 0.0;
    for (Py_ssize_t i = 0; i < a->length; i++) {
        acc += a->data[i];
    }
    return PyFloat_FromDouble(acc);
}

/* fill(value) — write every element inline, so a following sum()
 * observes the mutation through the same stable body. */
static PyObject *StockArray_fill(PyObject *self, PyObject *value) {
    double v = PyFloat_AsDouble(value);
    if (v == -1.0 && PyErr_Occurred()) {
        return NULL;
    }
    StockArrayObject *a = (StockArrayObject *)self;
    for (Py_ssize_t i = 0; i < a->length; i++) {
        a->data[i] = v;
    }
    Py_RETURN_NONE;
}

/* data_addr() — expose the inline `data` pointer so the test can assert
 * it is *stable* across crossings (same address every call). */
static PyObject *StockArray_data_addr(PyObject *self, PyObject *ignored) {
    (void)ignored;
    StockArrayObject *a = (StockArrayObject *)self;
    return PyLong_FromLongLong((long long)(intptr_t)a->data);
}

static PyMethodDef StockArray_methods[] = {
    {"sum", StockArray_sum, METH_NOARGS, "sum the elements (reads inline fields)"},
    {"fill", StockArray_fill, METH_O, "set every element to value (writes inline)"},
    {"data_addr", StockArray_data_addr, METH_NOARGS, "address of the inline data buffer"},
    {NULL, NULL, 0, NULL},
};

/* tp_members: project inline fields at their real offsets. `nd` and
 * `length` are read-only; `typenum` is writable (a member-set proof). */
static PyMemberDef StockArray_members[] = {
    {"nd", T_INT, offsetof(StockArrayObject, nd), READONLY, "number of dimensions"},
    {"length", T_PYSSIZET, offsetof(StockArrayObject, length), READONLY, "element count"},
    {"typenum", T_INT, offsetof(StockArrayObject, typenum), 0, "dtype code (writable)"},
    {NULL, 0, 0, 0, NULL},
};

/* ------------------------------------------------------------------ */
/* Array interchange: __array_interface__ + __array_struct__.         */
/* ------------------------------------------------------------------ */

/* The documented numpy `PyArrayInterface` (array interface v3). Defined
 * locally because this fixture is built without numpy's headers — the
 * layout is the ABI contract every `__array_struct__` consumer reads. */
typedef struct {
    int two;              /* sanity check: always 2                    */
    int nd;               /* number of dimensions                      */
    char typekind;        /* 'f', 'i', ...                             */
    int itemsize;         /* bytes per element                         */
    int flags;            /* interpretation flags                      */
    Py_intptr_t *shape;   /* length-nd shape                           */
    Py_intptr_t *strides; /* length-nd strides                         */
    void *data;           /* first element                             */
    PyObject *descr;      /* optional, NULL here                       */
} PyArrayInterface;

static PyObject *StockArray_get_array_interface(PyObject *self, void *closure) {
    (void)closure;
    StockArrayObject *a = (StockArrayObject *)self;
    PyObject *shape = Py_BuildValue("(n)", a->length);
    if (!shape) {
        return NULL;
    }
    /* data is (address:int, read_only:bool) per the array interface. */
    PyObject *data = Py_BuildValue("(LO)", (long long)(intptr_t)a->data, Py_False);
    if (!data) {
        Py_DECREF(shape);
        return NULL;
    }
    PyObject *dict = Py_BuildValue("{s:i, s:N, s:s, s:N}",
                                   "version", 3,
                                   "shape", shape,
                                   "typestr", "<f8",
                                   "data", data);
    return dict;
}

static void array_struct_destructor(PyObject *capsule) {
    PyArrayInterface *iface = (PyArrayInterface *)PyCapsule_GetPointer(capsule, NULL);
    if (iface) {
        free(iface->shape);
        free(iface->strides);
        free(iface);
    }
}

static PyObject *StockArray_get_array_struct(PyObject *self, void *closure) {
    (void)closure;
    StockArrayObject *a = (StockArrayObject *)self;
    PyArrayInterface *iface = (PyArrayInterface *)calloc(1, sizeof(PyArrayInterface));
    if (!iface) {
        return PyErr_NoMemory();
    }
    iface->shape = (Py_intptr_t *)malloc(sizeof(Py_intptr_t));
    iface->strides = (Py_intptr_t *)malloc(sizeof(Py_intptr_t));
    if (!iface->shape || !iface->strides) {
        free(iface->shape);
        free(iface->strides);
        free(iface);
        return PyErr_NoMemory();
    }
    iface->two = 2;
    iface->nd = a->nd;
    iface->typekind = 'f';
    iface->itemsize = (int)sizeof(double);
    iface->flags = 0;
    iface->shape[0] = (Py_intptr_t)a->length;
    iface->strides[0] = (Py_intptr_t)sizeof(double);
    iface->data = a->data;
    iface->descr = NULL;
    /* The protocol uses a NULL-named capsule. */
    return PyCapsule_New((void *)iface, NULL, array_struct_destructor);
}

static PyGetSetDef StockArray_getset[] = {
    {"__array_interface__", StockArray_get_array_interface, NULL, "numpy array interface v3", NULL},
    {"__array_struct__", StockArray_get_array_struct, NULL, "numpy C array interface capsule", NULL},
    {NULL, NULL, NULL, NULL, NULL},
};

static PyTypeObject StockArray_Type = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "_stockarray.StockArray",
    .tp_basicsize = sizeof(StockArrayObject),
    .tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE,
    .tp_doc = "fixed 1-D float64 array with inline tp_basicsize storage",
    .tp_new = PyType_GenericNew,
    .tp_init = StockArray_init,
    .tp_dealloc = StockArray_dealloc,
    .tp_methods = StockArray_methods,
    .tp_members = StockArray_members,
    .tp_getset = StockArray_getset,
};

/* ================================================================== */
/* The array C-API capsule (`import_array()` shape).                  */
/* ================================================================== */

/* `StockArray_FromLength(n)` — a C-level constructor exported through
 * the API table (the analogue of `PyArray_SimpleNew`). Builds an array
 * by calling the readied type object, so it drives the same inline-body
 * `tp_new`/`tp_init` path. */
static PyObject *StockArray_FromLength(Py_ssize_t n) {
    return PyObject_CallFunction((PyObject *)&StockArray_Type, "n", n);
}

/* The exported function table. Index 0 is the type object, index 1 is
 * the constructor — the same "array of `void *`" shape numpy publishes
 * as `PyArray_API`. */
enum {
    STOCKARRAY_API_TYPE = 0,
    STOCKARRAY_API_FROMLENGTH = 1,
    STOCKARRAY_API_NUMPOINTERS = 2,
};
static void *StockArray_API[STOCKARRAY_API_NUMPOINTERS];

/* capi_roundtrip(n) — the consumer side of `import_array()`: resolve the
 * well-known capsule, recover the `void **` table, and call through it
 * to build a fresh array. Proves the whole import-capsule round trip. */
static PyObject *sa_capi_roundtrip(PyObject *self, PyObject *args) {
    (void)self;
    Py_ssize_t n = 0;
    if (!PyArg_ParseTuple(args, "n", &n)) {
        return NULL;
    }
    void **api = (void **)PyCapsule_Import("_stockarray._ARRAY_API", 0);
    if (!api) {
        return NULL;
    }
    PyObject *(*from_length)(Py_ssize_t) =
        (PyObject * (*)(Py_ssize_t)) api[STOCKARRAY_API_FROMLENGTH];
    return from_length(n);
}

/* read_array_struct(arr) — a consumer of the `__array_struct__` capsule:
 * pull the `PyArrayInterface` back out and report a few fields, proving
 * the C array-interchange struct round-trips with the right layout. */
static PyObject *sa_read_array_struct(PyObject *self, PyObject *arr) {
    (void)self;
    PyObject *cap = PyObject_GetAttrString(arr, "__array_struct__");
    if (!cap) {
        return NULL;
    }
    PyArrayInterface *iface = (PyArrayInterface *)PyCapsule_GetPointer(cap, NULL);
    if (!iface) {
        Py_DECREF(cap);
        return NULL;
    }
    /* (two, nd, typekind_as_int, length, data_addr) */
    PyObject *out = Py_BuildValue("(iiinL)",
                                  iface->two,
                                  iface->nd,
                                  (int)iface->typekind,
                                  (Py_ssize_t)iface->shape[0],
                                  (long long)(intptr_t)iface->data);
    Py_DECREF(cap);
    return out;
}

/* live_count() — number of live StockArray instances (dealloc proof). */
static PyObject *sa_live_count(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    return PyLong_FromLong(g_live);
}

/* dealloc_count() — monotonic count of `tp_dealloc` runs (race-free
 * dealloc proof; see `g_deallocs`). */
static PyObject *sa_dealloc_count(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    return PyLong_FromLong(g_deallocs);
}

static PyMethodDef sa_methods[] = {
    {"capi_roundtrip", sa_capi_roundtrip, METH_VARARGS,
     "import the array C-API capsule and build an array through it"},
    {"read_array_struct", sa_read_array_struct, METH_O,
     "read back fields from an array's __array_struct__ capsule"},
    {"live_count", sa_live_count, METH_NOARGS, "live StockArray instance count"},
    {"dealloc_count", sa_dealloc_count, METH_NOARGS,
     "monotonic count of tp_dealloc runs"},
    {NULL, NULL, 0, NULL},
};

static struct PyModuleDef sa_module = {
    PyModuleDef_HEAD_INIT,
    "_stockarray",
    "RFC 0045 wave-3 stock-CPython-3.13 inline-storage + array C-API proof.",
    -1,
    sa_methods,
    NULL,
    NULL,
    NULL,
    NULL,
};

PyMODINIT_FUNC PyInit__stockarray(void) {
    PyObject *m = PyModule_Create(&sa_module);
    if (!m) {
        return NULL;
    }
    if (PyType_Ready(&StockArray_Type) < 0) {
        Py_DECREF(m);
        return NULL;
    }
    Py_INCREF(&StockArray_Type);
    if (PyModule_AddObject(m, "StockArray", (PyObject *)&StockArray_Type) < 0) {
        Py_DECREF(&StockArray_Type);
        Py_DECREF(m);
        return NULL;
    }

    /* Publish the array C-API function table as a capsule, exactly as a
     * numpy-like producer does (`numpy.core.multiarray._ARRAY_API`). */
    StockArray_API[STOCKARRAY_API_TYPE] = (void *)&StockArray_Type;
    StockArray_API[STOCKARRAY_API_FROMLENGTH] = (void *)StockArray_FromLength;
    PyObject *c_api = PyCapsule_New((void *)StockArray_API, "_stockarray._ARRAY_API", NULL);
    if (!c_api) {
        Py_DECREF(m);
        return NULL;
    }
    if (PyModule_AddObject(m, "_ARRAY_API", c_api) < 0) {
        Py_DECREF(c_api);
        Py_DECREF(m);
        return NULL;
    }

    PyModule_AddStringConstant(m, "ABI", "cp313");
    return m;
}
