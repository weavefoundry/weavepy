/*
 * _numpylike — a numpy-shaped C extension that exercises the
 * end-to-end stack required to support real-world scientific
 * Python (RFC 0029).
 *
 * This is intentionally larger and rougher than `_ndarray.c`. The
 * goal isn't a fast linear-algebra kernel — it's exhaustive
 * coverage of the C-API surface that a "production" numpy-style
 * extension touches at import time and on every common operation:
 *
 *   - Heap-type registration with rich slot tables.
 *   - Multi-dtype arrays (int8, int32, int64, float32, float64,
 *     complex128).
 *   - Strided (non-contiguous) buffer export via the PEP 3118
 *     buffer protocol.
 *   - Element-wise ufuncs with both unary and binary signatures,
 *     plus broadcasting against scalars and other arrays.
 *   - Fancy indexing (slicing, lists of integer indices, bool
 *     masks).
 *   - Structured / "record" dtype: an array of compound elements.
 *   - Capsule export of an internal C-API table so a sibling
 *     extension could consume it without going through Python.
 *   - PyArg_ParseTupleAndKeywords with mixed positional and
 *     keyword bindings.
 *   - datetime C-API consumption (build a datetime, read its
 *     fields back out, return the year diff).
 *
 * Storage model: like `_ndarray.c`, we side-allocate per-instance
 * state via `PyMem_Calloc` and stash a `PyLong` pointer in
 * `self.__dict__["_state"]`. This keeps the harness compatible
 * with WeavePy's instance representation (which is opaque to the
 * extension).
 */

#include "../../crates/weavepy-capi/include/Python.h"

#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* -------------------- Dtype enumeration -------------------- */

typedef enum {
    DT_INT8    = 0,
    DT_INT32   = 1,
    DT_INT64   = 2,
    DT_FLOAT32 = 3,
    DT_FLOAT64 = 4,
    DT_COMPLEX = 5,  /* pair of float64 */
    DT_RECORD  = 6,  /* {int64 i, float64 f} */
} DType;

static const char *dt_name(DType d) {
    switch (d) {
        case DT_INT8:    return "i8";
        case DT_INT32:   return "i32";
        case DT_INT64:   return "i64";
        case DT_FLOAT32: return "f32";
        case DT_FLOAT64: return "f64";
        case DT_COMPLEX: return "c128";
        case DT_RECORD:  return "rec";
        default:         return "?";
    }
}

static Py_ssize_t dt_itemsize(DType d) {
    switch (d) {
        case DT_INT8:    return 1;
        case DT_INT32:   return 4;
        case DT_INT64:   return 8;
        case DT_FLOAT32: return 4;
        case DT_FLOAT64: return 8;
        case DT_COMPLEX: return 16;
        case DT_RECORD:  return 16; /* int64 + float64 */
        default:         return 0;
    }
}

/* Buffer-protocol format string for `dt`. Matches PEP 3118 and is
 * the same alphabet numpy uses. */
static const char *dt_format(DType d) {
    switch (d) {
        case DT_INT8:    return "b";
        case DT_INT32:   return "i";
        case DT_INT64:   return "q";
        case DT_FLOAT32: return "f";
        case DT_FLOAT64: return "d";
        case DT_COMPLEX: return "Zd";
        case DT_RECORD:  return "T{q:i:d:f:}";
        default:         return "B";
    }
}

/* -------------------- Per-instance storage -------------------- */

typedef struct {
    Py_ssize_t ndim;
    Py_ssize_t shape[4];
    Py_ssize_t strides[4]; /* element-stride per axis (in bytes) */
    Py_ssize_t total_bytes;
    DType dtype;
    char *data;
    int writeable;
    int exporter_count;
} NDState;

static int put_state(PyObject *self, NDState *st) {
    PyObject *addr = PyLong_FromLongLong((long long)(intptr_t)st);
    if (!addr) return -1;
    int rc = PyObject_SetAttrString(self, "_state", addr);
    Py_DECREF(addr);
    return rc;
}

static NDState *get_state(PyObject *self) {
    PyObject *attr = PyObject_GetAttrString(self, "_state");
    if (!attr) return NULL;
    long long v = PyLong_AsLongLong(attr);
    Py_DECREF(attr);
    if (v == -1 && PyErr_Occurred()) return NULL;
    NDState *st = (NDState *)(intptr_t)v;
    if (!st) {
        PyErr_SetString(PyExc_RuntimeError, "ND: state is NULL");
        return NULL;
    }
    return st;
}

static Py_ssize_t total_elements(NDState *st) {
    Py_ssize_t n = 1;
    for (Py_ssize_t i = 0; i < st->ndim; i++) {
        n *= st->shape[i];
    }
    return n;
}

static void compute_contiguous_strides(NDState *st) {
    Py_ssize_t s = dt_itemsize(st->dtype);
    for (Py_ssize_t i = st->ndim - 1; i >= 0; i--) {
        st->strides[i] = s;
        s *= st->shape[i];
    }
}

/* Read one element as a double (used by ufuncs). */
static double read_as_double(const char *p, DType d) {
    switch (d) {
        case DT_INT8:    return (double)(*(const int8_t *)p);
        case DT_INT32:   return (double)(*(const int32_t *)p);
        case DT_INT64:   return (double)(*(const int64_t *)p);
        case DT_FLOAT32: return (double)(*(const float *)p);
        case DT_FLOAT64: return *(const double *)p;
        case DT_COMPLEX: return *(const double *)p; /* real part */
        case DT_RECORD:  return *(const double *)(p + 8);
        default:         return 0.0;
    }
}

/* Write one element from a double. */
static void write_from_double(char *p, DType d, double v) {
    switch (d) {
        case DT_INT8:    *(int8_t *)p  = (int8_t)v; break;
        case DT_INT32:   *(int32_t *)p = (int32_t)v; break;
        case DT_INT64:   *(int64_t *)p = (int64_t)v; break;
        case DT_FLOAT32: *(float *)p   = (float)v; break;
        case DT_FLOAT64: *(double *)p  = v; break;
        case DT_COMPLEX:
            *(double *)p          = v;     /* real */
            *(double *)(p + 8)    = 0.0;   /* imag */
            break;
        case DT_RECORD:
            *(int64_t *)p         = (int64_t)v;
            *(double *)(p + 8)    = v;
            break;
        default: break;
    }
}

/* -------------------- DType helper class -------------------- */
/*
 * Mimics numpy's `dtype` objects in shape: a small immutable
 * value with `kind`, `itemsize`, and `name` attributes. We use
 * `PyType_FromSpec` and a custom getset table so the WeavePy
 * descriptor machinery is exercised on attribute access.
 */

static PyTypeObject *DTypeType_obj = NULL;

static PyObject *dtype_new(DType d) {
    if (!DTypeType_obj) {
        PyErr_SetString(PyExc_RuntimeError, "DType type not initialised");
        return NULL;
    }
    /* Build by calling the type — we pass the integer code as the
     * sole positional argument and rely on `__init__` to stash it. */
    PyObject *args = Py_BuildValue("(i)", (int)d);
    if (!args) return NULL;
    PyObject *out = PyObject_Call((PyObject *)DTypeType_obj, args, NULL);
    Py_DECREF(args);
    return out;
}

static int DType_init(PyObject *self, PyObject *args, PyObject *kwargs) {
    static char *kw[] = { "code", NULL };
    int code = 0;
    if (!PyArg_ParseTupleAndKeywords(args, kwargs, "i", kw, &code)) return -1;
    PyObject *icode = PyLong_FromLong(code);
    if (!icode) return -1;
    int rc = PyObject_SetAttrString(self, "_code", icode);
    Py_DECREF(icode);
    return rc;
}

static DType dtype_of(PyObject *o) {
    PyObject *attr = PyObject_GetAttrString(o, "_code");
    if (!attr) return DT_FLOAT64;
    long v = PyLong_AsLong(attr);
    Py_DECREF(attr);
    return (DType)v;
}

static PyObject *DType_get_name(PyObject *self, void *cls_unused) {
    (void)cls_unused;
    return PyUnicode_FromString(dt_name(dtype_of(self)));
}

static PyObject *DType_get_itemsize(PyObject *self, void *cls_unused) {
    (void)cls_unused;
    return PyLong_FromSsize_t(dt_itemsize(dtype_of(self)));
}

static PyObject *DType_get_kind(PyObject *self, void *cls_unused) {
    (void)cls_unused;
    DType d = dtype_of(self);
    const char *k = "?";
    switch (d) {
        case DT_INT8: case DT_INT32: case DT_INT64: k = "i"; break;
        case DT_FLOAT32: case DT_FLOAT64:           k = "f"; break;
        case DT_COMPLEX:                            k = "c"; break;
        case DT_RECORD:                             k = "V"; break;
        default: break;
    }
    return PyUnicode_FromString(k);
}

static PyGetSetDef DType_getsets[] = {
    {"name",     DType_get_name,     NULL, "dtype name", NULL},
    {"itemsize", DType_get_itemsize, NULL, "bytes per element", NULL},
    {"kind",     DType_get_kind,     NULL, "kind char", NULL},
    {NULL, NULL, NULL, NULL, NULL},
};

static PyObject *DType_repr(PyObject *self) {
    char buf[64];
    snprintf(buf, sizeof(buf), "dtype('%s')", dt_name(dtype_of(self)));
    return PyUnicode_FromString(buf);
}

static PyType_Slot DType_slots[] = {
    {Py_tp_init, (void *)DType_init},
    {Py_tp_repr, (void *)DType_repr},
    {Py_tp_str,  (void *)DType_repr},
    {Py_tp_getset, (void *)DType_getsets},
    {0, NULL},
};

static PyType_Spec DType_spec = {
    .name      = "_numpylike.dtype",
    .basicsize = 0,
    .itemsize  = 0,
    .flags     = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE,
    .slots     = DType_slots,
};

/* -------------------- NDArray heap type -------------------- */

static PyTypeObject *NDArrayType_obj = NULL;

static int parse_shape(PyObject *shape_obj, Py_ssize_t *out_shape, Py_ssize_t *out_ndim) {
    if (PyLong_Check(shape_obj)) {
        Py_ssize_t v = PyLong_AsSsize_t(shape_obj);
        if (v == -1 && PyErr_Occurred()) return -1;
        if (v < 0) {
            PyErr_SetString(PyExc_ValueError, "shape entries must be >= 0");
            return -1;
        }
        out_shape[0] = v;
        *out_ndim = 1;
        return 0;
    }
    if (PyTuple_Check(shape_obj)) {
        Py_ssize_t n = PyTuple_Size(shape_obj);
        if (n < 1 || n > 4) {
            PyErr_SetString(PyExc_ValueError, "shape must have 1..4 entries");
            return -1;
        }
        for (Py_ssize_t i = 0; i < n; i++) {
            PyObject *item = PyTuple_GetItem(shape_obj, i);
            if (!item) return -1;
            Py_ssize_t v = PyLong_AsSsize_t(item);
            if (v == -1 && PyErr_Occurred()) return -1;
            if (v < 0) {
                PyErr_SetString(PyExc_ValueError, "shape entries must be >= 0");
                return -1;
            }
            out_shape[i] = v;
        }
        *out_ndim = n;
        return 0;
    }
    if (PyList_Check(shape_obj)) {
        Py_ssize_t n = PyList_Size(shape_obj);
        if (n < 1 || n > 4) {
            PyErr_SetString(PyExc_ValueError, "shape must have 1..4 entries");
            return -1;
        }
        for (Py_ssize_t i = 0; i < n; i++) {
            PyObject *item = PyList_GetItem(shape_obj, i);
            if (!item) return -1;
            Py_ssize_t v = PyLong_AsSsize_t(item);
            if (v == -1 && PyErr_Occurred()) return -1;
            out_shape[i] = v;
        }
        *out_ndim = n;
        return 0;
    }
    PyErr_SetString(PyExc_TypeError, "shape must be int, tuple, or list");
    return -1;
}

static int NDArray_init(PyObject *self, PyObject *args, PyObject *kwargs) {
    static char *kw[] = { "shape", "dtype", "writeable", NULL };
    PyObject *shape_obj = NULL;
    int dtype_code = (int)DT_FLOAT64;
    int writeable = 1;
    if (!PyArg_ParseTupleAndKeywords(args, kwargs, "O|ip", kw,
                                     &shape_obj, &dtype_code, &writeable)) {
        return -1;
    }
    NDState *st = (NDState *)PyMem_Calloc(1, sizeof(NDState));
    if (!st) { PyErr_NoMemory(); return -1; }
    if (parse_shape(shape_obj, st->shape, &st->ndim) != 0) {
        PyMem_Free(st);
        return -1;
    }
    st->dtype = (DType)dtype_code;
    st->writeable = writeable ? 1 : 0;
    compute_contiguous_strides(st);
    Py_ssize_t total = total_elements(st) * dt_itemsize(st->dtype);
    st->total_bytes = total;
    if (total > 0) {
        st->data = (char *)PyMem_Calloc(1, (size_t)total);
        if (!st->data) {
            PyMem_Free(st);
            PyErr_NoMemory();
            return -1;
        }
    }
    return put_state(self, st);
}

static PyObject *NDArray_repr(PyObject *self) {
    NDState *st = get_state(self);
    if (!st) return NULL;
    char buf[128];
    if (st->ndim == 1) {
        snprintf(buf, sizeof(buf), "<ND shape=(%ld,) dtype=%s>",
                 (long)st->shape[0], dt_name(st->dtype));
    } else if (st->ndim == 2) {
        snprintf(buf, sizeof(buf), "<ND shape=(%ld,%ld) dtype=%s>",
                 (long)st->shape[0], (long)st->shape[1], dt_name(st->dtype));
    } else {
        snprintf(buf, sizeof(buf), "<ND ndim=%ld dtype=%s>",
                 (long)st->ndim, dt_name(st->dtype));
    }
    return PyUnicode_FromString(buf);
}

/* ---------- Buffer protocol ---------- */

static int NDArray_getbuffer(PyObject *self, Py_buffer *view, int flags) {
    NDState *st = get_state(self);
    if (!st) return -1;
    view->buf = st->data;
    view->obj = self;
    Py_INCREF(self);
    view->len = st->total_bytes;
    view->itemsize = dt_itemsize(st->dtype);
    view->readonly = st->writeable ? 0 : 1;
    view->ndim = (int)st->ndim;
    view->format = (char *)dt_format(st->dtype);
    if (flags & PyBUF_ND) {
        view->shape = st->shape;
    } else {
        view->shape = NULL;
    }
    if (flags & PyBUF_STRIDES) {
        view->strides = st->strides;
    } else {
        view->strides = NULL;
    }
    view->suboffsets = NULL;
    view->internal = NULL;
    st->exporter_count++;
    return 0;
}

static void NDArray_releasebuffer(PyObject *self, Py_buffer *view) {
    (void)view;
    NDState *st = get_state(self);
    if (!st) return;
    if (st->exporter_count > 0) st->exporter_count--;
}

/* Buffer slots are registered via Py_bf_* in the slot table below;
 * no separate `PyBufferProcs` definition is needed when using
 * `PyType_FromSpec`. */

/* ---------- Indexing ---------- */

static PyObject *NDArray_subscript(PyObject *self, PyObject *idx) {
    NDState *st = get_state(self);
    if (!st) return NULL;

    /* Fast path: 1-D integer index. */
    if (st->ndim == 1 && PyLong_Check(idx)) {
        Py_ssize_t i = PyLong_AsSsize_t(idx);
        if (i == -1 && PyErr_Occurred()) return NULL;
        if (i < 0) i += st->shape[0];
        if (i < 0 || i >= st->shape[0]) {
            PyErr_SetString(PyExc_IndexError, "index out of range");
            return NULL;
        }
        const char *p = st->data + i * st->strides[0];
        return PyFloat_FromDouble(read_as_double(p, st->dtype));
    }

    /* 2-D integer-tuple index. */
    if (st->ndim == 2 && PyTuple_Check(idx) && PyTuple_Size(idx) == 2) {
        PyObject *a = PyTuple_GetItem(idx, 0);
        PyObject *b = PyTuple_GetItem(idx, 1);
        if (PyLong_Check(a) && PyLong_Check(b)) {
            Py_ssize_t i = PyLong_AsSsize_t(a);
            Py_ssize_t j = PyLong_AsSsize_t(b);
            if (i < 0) i += st->shape[0];
            if (j < 0) j += st->shape[1];
            if (i < 0 || i >= st->shape[0] || j < 0 || j >= st->shape[1]) {
                PyErr_SetString(PyExc_IndexError, "index out of range");
                return NULL;
            }
            const char *p = st->data + i * st->strides[0] + j * st->strides[1];
            return PyFloat_FromDouble(read_as_double(p, st->dtype));
        }
    }

    /* Slice. */
    if (PySlice_Check(idx) && st->ndim == 1) {
        Py_ssize_t start = 0, stop = st->shape[0], step = 1;
        Py_ssize_t length = 0;
        if (PySlice_GetIndicesEx(idx, st->shape[0], &start, &stop, &step, &length) < 0) {
            return NULL;
        }
        /* Build a list — production numpy would return an ND view,
         * but we don't need view semantics for the tests. */
        PyObject *lst = PyList_New(length);
        if (!lst) return NULL;
        Py_ssize_t k = 0;
        for (Py_ssize_t i = start; (step > 0 ? i < stop : i > stop); i += step) {
            const char *p = st->data + i * st->strides[0];
            PyObject *v = PyFloat_FromDouble(read_as_double(p, st->dtype));
            if (!v) { Py_DECREF(lst); return NULL; }
            PyList_SetItem(lst, k++, v);
        }
        return lst;
    }

    /* Fancy indexing: list of indices or array-of-bool mask. */
    if (PyList_Check(idx) && st->ndim == 1) {
        Py_ssize_t n = PyList_Size(idx);
        PyObject *lst = PyList_New(n);
        if (!lst) return NULL;
        for (Py_ssize_t k = 0; k < n; k++) {
            PyObject *item = PyList_GetItem(idx, k);
            if (PyBool_Check(item)) {
                PyErr_SetString(PyExc_TypeError, "use mask= for boolean indexing");
                Py_DECREF(lst);
                return NULL;
            }
            Py_ssize_t i = PyLong_AsSsize_t(item);
            if (i == -1 && PyErr_Occurred()) { Py_DECREF(lst); return NULL; }
            if (i < 0) i += st->shape[0];
            if (i < 0 || i >= st->shape[0]) {
                PyErr_SetString(PyExc_IndexError, "fancy index out of range");
                Py_DECREF(lst);
                return NULL;
            }
            const char *p = st->data + i * st->strides[0];
            PyList_SetItem(lst, k, PyFloat_FromDouble(read_as_double(p, st->dtype)));
        }
        return lst;
    }

    PyErr_SetString(PyExc_TypeError, "unsupported index");
    return NULL;
}

static int NDArray_ass_subscript(PyObject *self, PyObject *idx, PyObject *value) {
    NDState *st = get_state(self);
    if (!st) return -1;
    if (!st->writeable) {
        PyErr_SetString(PyExc_TypeError, "array is read-only");
        return -1;
    }
    double v = 0.0;
    if (PyFloat_Check(value)) v = PyFloat_AsDouble(value);
    else if (PyLong_Check(value)) v = (double)PyLong_AsLongLong(value);
    else if (PyBool_Check(value)) v = (double)(value == Py_True ? 1 : 0);
    else {
        PyErr_SetString(PyExc_TypeError, "scalar value required");
        return -1;
    }
    if (st->ndim == 1 && PyLong_Check(idx)) {
        Py_ssize_t i = PyLong_AsSsize_t(idx);
        if (i < 0) i += st->shape[0];
        if (i < 0 || i >= st->shape[0]) {
            PyErr_SetString(PyExc_IndexError, "index out of range");
            return -1;
        }
        write_from_double(st->data + i * st->strides[0], st->dtype, v);
        return 0;
    }
    if (st->ndim == 2 && PyTuple_Check(idx) && PyTuple_Size(idx) == 2) {
        Py_ssize_t i = PyLong_AsSsize_t(PyTuple_GetItem(idx, 0));
        Py_ssize_t j = PyLong_AsSsize_t(PyTuple_GetItem(idx, 1));
        if (i < 0) i += st->shape[0];
        if (j < 0) j += st->shape[1];
        if (i < 0 || i >= st->shape[0] || j < 0 || j >= st->shape[1]) {
            PyErr_SetString(PyExc_IndexError, "index out of range");
            return -1;
        }
        write_from_double(st->data + i * st->strides[0] + j * st->strides[1],
                          st->dtype, v);
        return 0;
    }
    PyErr_SetString(PyExc_TypeError, "unsupported index assignment");
    return -1;
}

static Py_ssize_t NDArray_length(PyObject *self) {
    NDState *st = get_state(self);
    if (!st) return -1;
    return st->ndim > 0 ? st->shape[0] : 0;
}

/* ---------- Properties ---------- */

static PyObject *NDArray_get_shape(PyObject *self, void *_) {
    (void)_;
    NDState *st = get_state(self);
    if (!st) return NULL;
    PyObject *t = PyTuple_New(st->ndim);
    for (Py_ssize_t i = 0; i < st->ndim; i++) {
        PyTuple_SetItem(t, i, PyLong_FromSsize_t(st->shape[i]));
    }
    return t;
}

static PyObject *NDArray_get_strides(PyObject *self, void *_) {
    (void)_;
    NDState *st = get_state(self);
    if (!st) return NULL;
    PyObject *t = PyTuple_New(st->ndim);
    for (Py_ssize_t i = 0; i < st->ndim; i++) {
        PyTuple_SetItem(t, i, PyLong_FromSsize_t(st->strides[i]));
    }
    return t;
}

static PyObject *NDArray_get_dtype(PyObject *self, void *_) {
    (void)_;
    NDState *st = get_state(self);
    if (!st) return NULL;
    return dtype_new(st->dtype);
}

static PyObject *NDArray_get_size(PyObject *self, void *_) {
    (void)_;
    NDState *st = get_state(self);
    if (!st) return NULL;
    return PyLong_FromSsize_t(total_elements(st));
}

static PyObject *NDArray_get_nbytes(PyObject *self, void *_) {
    (void)_;
    NDState *st = get_state(self);
    if (!st) return NULL;
    return PyLong_FromSsize_t(st->total_bytes);
}

static PyObject *NDArray_get_ndim(PyObject *self, void *_) {
    (void)_;
    NDState *st = get_state(self);
    if (!st) return NULL;
    return PyLong_FromSsize_t(st->ndim);
}

static PyObject *NDArray_get_writeable(PyObject *self, void *_) {
    (void)_;
    NDState *st = get_state(self);
    if (!st) return NULL;
    return PyBool_FromLong(st->writeable);
}

static PyGetSetDef NDArray_getsets[] = {
    {"shape",     NDArray_get_shape,     NULL, "shape tuple",       NULL},
    {"strides",   NDArray_get_strides,   NULL, "byte strides",      NULL},
    {"dtype",     NDArray_get_dtype,     NULL, "dtype instance",    NULL},
    {"size",      NDArray_get_size,      NULL, "total elements",    NULL},
    {"nbytes",    NDArray_get_nbytes,    NULL, "total bytes",       NULL},
    {"ndim",      NDArray_get_ndim,      NULL, "number of dims",    NULL},
    {"writeable", NDArray_get_writeable, NULL, "is writeable",      NULL},
    {NULL, NULL, NULL, NULL, NULL},
};

/* ---------- Methods ---------- */

static PyObject *NDArray_tolist(PyObject *self, PyObject *unused) {
    (void)unused;
    NDState *st = get_state(self);
    if (!st) return NULL;
    Py_ssize_t n = total_elements(st);
    PyObject *out = PyList_New(n);
    if (!out) return NULL;
    Py_ssize_t k = 0;
    if (st->ndim == 1) {
        for (Py_ssize_t i = 0; i < st->shape[0]; i++) {
            const char *p = st->data + i * st->strides[0];
            PyList_SetItem(out, k++, PyFloat_FromDouble(read_as_double(p, st->dtype)));
        }
    } else if (st->ndim == 2) {
        Py_DECREF(out);
        out = PyList_New(st->shape[0]);
        if (!out) return NULL;
        for (Py_ssize_t i = 0; i < st->shape[0]; i++) {
            PyObject *row = PyList_New(st->shape[1]);
            for (Py_ssize_t j = 0; j < st->shape[1]; j++) {
                const char *p = st->data + i * st->strides[0] + j * st->strides[1];
                PyList_SetItem(row, j, PyFloat_FromDouble(read_as_double(p, st->dtype)));
            }
            PyList_SetItem(out, i, row);
        }
    }
    return out;
}

static PyObject *NDArray_fill(PyObject *self, PyObject *args) {
    double v = 0.0;
    if (!PyArg_ParseTuple(args, "d", &v)) return NULL;
    NDState *st = get_state(self);
    if (!st) return NULL;
    if (!st->writeable) {
        PyErr_SetString(PyExc_TypeError, "array is read-only");
        return NULL;
    }
    Py_ssize_t n = total_elements(st);
    Py_ssize_t is = dt_itemsize(st->dtype);
    for (Py_ssize_t i = 0; i < n; i++) {
        write_from_double(st->data + i * is, st->dtype, v);
    }
    Py_INCREF(Py_None);
    return Py_None;
}

static PyObject *NDArray_sum(PyObject *self, PyObject *unused) {
    (void)unused;
    NDState *st = get_state(self);
    if (!st) return NULL;
    Py_ssize_t n = total_elements(st);
    Py_ssize_t is = dt_itemsize(st->dtype);
    double acc = 0.0;
    for (Py_ssize_t i = 0; i < n; i++) {
        acc += read_as_double(st->data + i * is, st->dtype);
    }
    return PyFloat_FromDouble(acc);
}

static PyObject *NDArray_argmax(PyObject *self, PyObject *unused) {
    (void)unused;
    NDState *st = get_state(self);
    if (!st) return NULL;
    Py_ssize_t n = total_elements(st);
    Py_ssize_t is = dt_itemsize(st->dtype);
    if (n == 0) {
        PyErr_SetString(PyExc_ValueError, "argmax of empty");
        return NULL;
    }
    Py_ssize_t best = 0;
    double best_v = read_as_double(st->data, st->dtype);
    for (Py_ssize_t i = 1; i < n; i++) {
        double v = read_as_double(st->data + i * is, st->dtype);
        if (v > best_v) { best_v = v; best = i; }
    }
    return PyLong_FromSsize_t(best);
}

static PyObject *NDArray_mean(PyObject *self, PyObject *unused) {
    (void)unused;
    NDState *st = get_state(self);
    if (!st) return NULL;
    Py_ssize_t n = total_elements(st);
    if (n == 0) { return PyFloat_FromDouble(0.0); }
    Py_ssize_t is = dt_itemsize(st->dtype);
    double acc = 0.0;
    for (Py_ssize_t i = 0; i < n; i++) {
        acc += read_as_double(st->data + i * is, st->dtype);
    }
    return PyFloat_FromDouble(acc / (double)n);
}

static PyObject *NDArray_reshape(PyObject *self, PyObject *args) {
    PyObject *shape_obj = NULL;
    if (!PyArg_ParseTuple(args, "O", &shape_obj)) return NULL;
    NDState *st = get_state(self);
    if (!st) return NULL;
    Py_ssize_t new_shape[4];
    Py_ssize_t new_ndim = 0;
    if (parse_shape(shape_obj, new_shape, &new_ndim) != 0) return NULL;
    Py_ssize_t new_total = 1;
    for (Py_ssize_t i = 0; i < new_ndim; i++) new_total *= new_shape[i];
    if (new_total != total_elements(st)) {
        PyErr_SetString(PyExc_ValueError, "reshape: total mismatch");
        return NULL;
    }
    st->ndim = new_ndim;
    for (Py_ssize_t i = 0; i < new_ndim; i++) st->shape[i] = new_shape[i];
    compute_contiguous_strides(st);
    Py_INCREF(self);
    return self;
}

static PyObject *NDArray_astype(PyObject *self, PyObject *args) {
    int dtype_code = (int)DT_FLOAT64;
    if (!PyArg_ParseTuple(args, "i", &dtype_code)) return NULL;
    NDState *st = get_state(self);
    if (!st) return NULL;

    /* Create a fresh array with the new dtype. */
    PyObject *shape_tuple = NDArray_get_shape(self, NULL);
    if (!shape_tuple) return NULL;
    PyObject *call_args = Py_BuildValue("(Oi)", shape_tuple, dtype_code);
    Py_DECREF(shape_tuple);
    if (!call_args) return NULL;
    PyObject *new_array = PyObject_Call((PyObject *)NDArrayType_obj, call_args, NULL);
    Py_DECREF(call_args);
    if (!new_array) return NULL;
    NDState *ds = get_state(new_array);
    if (!ds) { Py_DECREF(new_array); return NULL; }

    Py_ssize_t n = total_elements(st);
    Py_ssize_t src_is = dt_itemsize(st->dtype);
    Py_ssize_t dst_is = dt_itemsize(ds->dtype);
    for (Py_ssize_t i = 0; i < n; i++) {
        double v = read_as_double(st->data + i * src_is, st->dtype);
        write_from_double(ds->data + i * dst_is, ds->dtype, v);
    }
    return new_array;
}

/* ---------- Ufuncs ---------- */

/* Binary ufunc on two arrays. The result is always float64 to
 * avoid lossy down-casting in the tests. */
static PyObject *apply_binary(PyObject *a, PyObject *b,
                              double (*op)(double, double),
                              const char *err) {
    NDState *sa = get_state(a);
    if (!sa) return NULL;
    /* Scalar broadcast. */
    if (PyFloat_Check(b) || PyLong_Check(b)) {
        double sv = PyFloat_Check(b) ? PyFloat_AsDouble(b)
                                     : (double)PyLong_AsLongLong(b);
        PyObject *shape_tuple = NDArray_get_shape(a, NULL);
        if (!shape_tuple) return NULL;
        PyObject *call_args = Py_BuildValue("(Oi)", shape_tuple, (int)DT_FLOAT64);
        Py_DECREF(shape_tuple);
        PyObject *out = PyObject_Call((PyObject *)NDArrayType_obj, call_args, NULL);
        Py_DECREF(call_args);
        if (!out) return NULL;
        NDState *so = get_state(out);
        if (!so) { Py_DECREF(out); return NULL; }
        Py_ssize_t n = total_elements(sa);
        Py_ssize_t sa_is = dt_itemsize(sa->dtype);
        for (Py_ssize_t i = 0; i < n; i++) {
            double va = read_as_double(sa->data + i * sa_is, sa->dtype);
            write_from_double(so->data + i * 8, DT_FLOAT64, op(va, sv));
        }
        return out;
    }
    NDState *sb = get_state(b);
    if (!sb) {
        PyErr_SetString(PyExc_TypeError, err);
        return NULL;
    }
    if (sa->ndim != sb->ndim) {
        PyErr_SetString(PyExc_ValueError, "shape mismatch in ufunc");
        return NULL;
    }
    for (Py_ssize_t i = 0; i < sa->ndim; i++) {
        if (sa->shape[i] != sb->shape[i]) {
            PyErr_SetString(PyExc_ValueError, "shape mismatch in ufunc");
            return NULL;
        }
    }
    PyObject *shape_tuple = NDArray_get_shape(a, NULL);
    if (!shape_tuple) return NULL;
    PyObject *call_args = Py_BuildValue("(Oi)", shape_tuple, (int)DT_FLOAT64);
    Py_DECREF(shape_tuple);
    PyObject *out = PyObject_Call((PyObject *)NDArrayType_obj, call_args, NULL);
    Py_DECREF(call_args);
    if (!out) return NULL;
    NDState *so = get_state(out);
    if (!so) { Py_DECREF(out); return NULL; }
    Py_ssize_t n = total_elements(sa);
    Py_ssize_t sa_is = dt_itemsize(sa->dtype);
    Py_ssize_t sb_is = dt_itemsize(sb->dtype);
    for (Py_ssize_t i = 0; i < n; i++) {
        double va = read_as_double(sa->data + i * sa_is, sa->dtype);
        double vb = read_as_double(sb->data + i * sb_is, sb->dtype);
        write_from_double(so->data + i * 8, DT_FLOAT64, op(va, vb));
    }
    return out;
}

static double op_add(double a, double b) { return a + b; }
static double op_sub(double a, double b) { return a - b; }
static double op_mul(double a, double b) { return a * b; }
static double op_div(double a, double b) { return b == 0.0 ? 0.0 : a / b; }
static double op_max(double a, double b) { return a > b ? a : b; }
static double op_min(double a, double b) { return a < b ? a : b; }

static PyObject *uf_add(PyObject *self, PyObject *args) {
    PyObject *a, *b;
    if (!PyArg_ParseTuple(args, "OO", &a, &b)) return NULL;
    (void)self;
    return apply_binary(a, b, op_add, "add: incompatible types");
}

static PyObject *uf_sub(PyObject *self, PyObject *args) {
    PyObject *a, *b;
    if (!PyArg_ParseTuple(args, "OO", &a, &b)) return NULL;
    (void)self;
    return apply_binary(a, b, op_sub, "sub: incompatible types");
}

static PyObject *uf_mul(PyObject *self, PyObject *args) {
    PyObject *a, *b;
    if (!PyArg_ParseTuple(args, "OO", &a, &b)) return NULL;
    (void)self;
    return apply_binary(a, b, op_mul, "mul: incompatible types");
}

static PyObject *uf_div(PyObject *self, PyObject *args) {
    PyObject *a, *b;
    if (!PyArg_ParseTuple(args, "OO", &a, &b)) return NULL;
    (void)self;
    return apply_binary(a, b, op_div, "div: incompatible types");
}

static PyObject *uf_max(PyObject *self, PyObject *args) {
    PyObject *a, *b;
    if (!PyArg_ParseTuple(args, "OO", &a, &b)) return NULL;
    (void)self;
    return apply_binary(a, b, op_max, "max: incompatible types");
}

static PyObject *uf_min(PyObject *self, PyObject *args) {
    PyObject *a, *b;
    if (!PyArg_ParseTuple(args, "OO", &a, &b)) return NULL;
    (void)self;
    return apply_binary(a, b, op_min, "min: incompatible types");
}

/* Unary ufuncs. */
static PyObject *apply_unary(PyObject *a, double (*op)(double)) {
    NDState *sa = get_state(a);
    if (!sa) return NULL;
    PyObject *shape_tuple = NDArray_get_shape(a, NULL);
    if (!shape_tuple) return NULL;
    PyObject *call_args = Py_BuildValue("(Oi)", shape_tuple, (int)DT_FLOAT64);
    Py_DECREF(shape_tuple);
    PyObject *out = PyObject_Call((PyObject *)NDArrayType_obj, call_args, NULL);
    Py_DECREF(call_args);
    if (!out) return NULL;
    NDState *so = get_state(out);
    if (!so) { Py_DECREF(out); return NULL; }
    Py_ssize_t n = total_elements(sa);
    Py_ssize_t sa_is = dt_itemsize(sa->dtype);
    for (Py_ssize_t i = 0; i < n; i++) {
        double v = read_as_double(sa->data + i * sa_is, sa->dtype);
        write_from_double(so->data + i * 8, DT_FLOAT64, op(v));
    }
    return out;
}

static double op_sqrt(double a) { return sqrt(a); }
static double op_abs(double a)  { return fabs(a); }
static double op_neg(double a)  { return -a; }
static double op_log(double a)  { return log(a); }
static double op_exp(double a)  { return exp(a); }
static double op_sin(double a)  { return sin(a); }
static double op_cos(double a)  { return cos(a); }

static PyObject *uf_sqrt(PyObject *self, PyObject *args) {
    PyObject *a;
    if (!PyArg_ParseTuple(args, "O", &a)) return NULL;
    (void)self;
    return apply_unary(a, op_sqrt);
}

static PyObject *uf_abs(PyObject *self, PyObject *args) {
    PyObject *a;
    if (!PyArg_ParseTuple(args, "O", &a)) return NULL;
    (void)self;
    return apply_unary(a, op_abs);
}

static PyObject *uf_neg(PyObject *self, PyObject *args) {
    PyObject *a;
    if (!PyArg_ParseTuple(args, "O", &a)) return NULL;
    (void)self;
    return apply_unary(a, op_neg);
}

static PyObject *uf_log(PyObject *self, PyObject *args) {
    PyObject *a;
    if (!PyArg_ParseTuple(args, "O", &a)) return NULL;
    (void)self;
    return apply_unary(a, op_log);
}

static PyObject *uf_exp(PyObject *self, PyObject *args) {
    PyObject *a;
    if (!PyArg_ParseTuple(args, "O", &a)) return NULL;
    (void)self;
    return apply_unary(a, op_exp);
}

static PyObject *uf_sin(PyObject *self, PyObject *args) {
    PyObject *a;
    if (!PyArg_ParseTuple(args, "O", &a)) return NULL;
    (void)self;
    return apply_unary(a, op_sin);
}

static PyObject *uf_cos(PyObject *self, PyObject *args) {
    PyObject *a;
    if (!PyArg_ParseTuple(args, "O", &a)) return NULL;
    (void)self;
    return apply_unary(a, op_cos);
}

/* Boolean mask filter. */
static PyObject *mask_select(PyObject *self, PyObject *args) {
    PyObject *a, *mask;
    if (!PyArg_ParseTuple(args, "OO", &a, &mask)) return NULL;
    (void)self;
    NDState *sa = get_state(a);
    if (!sa || sa->ndim != 1) {
        PyErr_SetString(PyExc_ValueError, "mask_select: array must be 1-D");
        return NULL;
    }
    if (!PyList_Check(mask)) {
        PyErr_SetString(PyExc_TypeError, "mask must be a list of bools");
        return NULL;
    }
    Py_ssize_t n = PyList_Size(mask);
    if (n != sa->shape[0]) {
        PyErr_SetString(PyExc_ValueError, "mask length != array length");
        return NULL;
    }
    PyObject *out = PyList_New(0);
    if (!out) return NULL;
    Py_ssize_t is = dt_itemsize(sa->dtype);
    for (Py_ssize_t i = 0; i < n; i++) {
        PyObject *flag = PyList_GetItem(mask, i);
        int truthy = PyObject_IsTrue(flag);
        if (truthy < 0) { Py_DECREF(out); return NULL; }
        if (truthy) {
            const char *p = sa->data + i * sa->strides[0];
            PyObject *v = PyFloat_FromDouble(read_as_double(p, sa->dtype));
            if (PyList_Append(out, v) != 0) {
                Py_DECREF(v); Py_DECREF(out); return NULL;
            }
            Py_DECREF(v);
        }
    }
    return out;
}

/* Range constructor. */
static PyObject *arange(PyObject *self, PyObject *args, PyObject *kwargs) {
    (void)self;
    static char *kw[] = { "n", "start", "step", "dtype", NULL };
    Py_ssize_t n = 0;
    double start = 0.0;
    double step = 1.0;
    int dtype_code = (int)DT_FLOAT64;
    if (!PyArg_ParseTupleAndKeywords(args, kwargs, "n|ddi", kw,
                                     &n, &start, &step, &dtype_code)) {
        return NULL;
    }
    PyObject *shape_obj = PyLong_FromSsize_t(n);
    if (!shape_obj) return NULL;
    PyObject *call_args = Py_BuildValue("(Oi)", shape_obj, dtype_code);
    Py_DECREF(shape_obj);
    PyObject *arr = PyObject_Call((PyObject *)NDArrayType_obj, call_args, NULL);
    Py_DECREF(call_args);
    if (!arr) return NULL;
    NDState *st = get_state(arr);
    if (!st) { Py_DECREF(arr); return NULL; }
    Py_ssize_t is = dt_itemsize(st->dtype);
    for (Py_ssize_t i = 0; i < n; i++) {
        write_from_double(st->data + i * is, st->dtype, start + (double)i * step);
    }
    return arr;
}

/* Pure-C dot product — used to verify buffer export round-trips. */
static PyObject *dot1d(PyObject *self, PyObject *args) {
    PyObject *a, *b;
    if (!PyArg_ParseTuple(args, "OO", &a, &b)) return NULL;
    (void)self;
    NDState *sa = get_state(a);
    NDState *sb = get_state(b);
    if (!sa || !sb || sa->ndim != 1 || sb->ndim != 1 || sa->shape[0] != sb->shape[0]) {
        PyErr_SetString(PyExc_ValueError, "dot1d: shape mismatch");
        return NULL;
    }
    Py_ssize_t n = sa->shape[0];
    Py_ssize_t sa_is = dt_itemsize(sa->dtype);
    Py_ssize_t sb_is = dt_itemsize(sb->dtype);
    double acc = 0.0;
    for (Py_ssize_t i = 0; i < n; i++) {
        acc += read_as_double(sa->data + i * sa_is, sa->dtype)
             * read_as_double(sb->data + i * sb_is, sb->dtype);
    }
    return PyFloat_FromDouble(acc);
}

/* ---------- Methods table ---------- */
static PyMethodDef NDArray_methods[] = {
    {"tolist",  (PyCFunction)NDArray_tolist,  METH_NOARGS,  "flatten to list"},
    {"fill",    (PyCFunction)NDArray_fill,    METH_VARARGS, "fill with scalar"},
    {"sum",     (PyCFunction)NDArray_sum,     METH_NOARGS,  "sum all elements"},
    {"mean",    (PyCFunction)NDArray_mean,    METH_NOARGS,  "mean of elements"},
    {"argmax",  (PyCFunction)NDArray_argmax,  METH_NOARGS,  "index of max"},
    {"reshape", (PyCFunction)NDArray_reshape, METH_VARARGS, "reshape in place"},
    {"astype",  (PyCFunction)NDArray_astype,  METH_VARARGS, "cast to new dtype"},
    {NULL, NULL, 0, NULL},
};

static PyType_Slot NDArray_slots[] = {
    {Py_tp_init,            (void *)NDArray_init},
    {Py_tp_repr,            (void *)NDArray_repr},
    {Py_tp_str,             (void *)NDArray_repr},
    {Py_tp_methods,         (void *)NDArray_methods},
    {Py_tp_getset,          (void *)NDArray_getsets},
    {Py_mp_length,          (void *)NDArray_length},
    {Py_mp_subscript,       (void *)NDArray_subscript},
    {Py_mp_ass_subscript,   (void *)NDArray_ass_subscript},
    {Py_bf_getbuffer,       (void *)NDArray_getbuffer},
    {Py_bf_releasebuffer,   (void *)NDArray_releasebuffer},
    {0, NULL},
};

static PyType_Spec NDArray_spec = {
    .name      = "_numpylike.ndarray",
    .basicsize = 0,
    .itemsize  = 0,
    .flags     = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE,
    .slots     = NDArray_slots,
};

/* ---------- Datetime probe (consumes the datetime C-API) ---------- */
static PyObject *datetime_year_diff(PyObject *self, PyObject *args) {
    (void)self;
    if (!PyTuple_Check(args) || PyTuple_Size(args) != 6) {
        PyErr_SetString(PyExc_TypeError, "datetime_year_diff: expected 6 ints");
        return NULL;
    }
    long long y1 = PyLong_AsLongLong(PyTuple_GetItem(args, 0));
    long long m1 = PyLong_AsLongLong(PyTuple_GetItem(args, 1));
    long long d1 = PyLong_AsLongLong(PyTuple_GetItem(args, 2));
    long long y2 = PyLong_AsLongLong(PyTuple_GetItem(args, 3));
    long long m2 = PyLong_AsLongLong(PyTuple_GetItem(args, 4));
    long long d2 = PyLong_AsLongLong(PyTuple_GetItem(args, 5));
    if (PyErr_Occurred()) return NULL;
    PyObject *a = PyDate_FromDate((int)y1, (int)m1, (int)d1);
    if (!a) return NULL;
    PyObject *b = PyDate_FromDate((int)y2, (int)m2, (int)d2);
    if (!b) { Py_DECREF(a); return NULL; }
    int year_a = PyDateTime_GET_YEAR(a);
    int year_b = PyDateTime_GET_YEAR(b);
    Py_DECREF(a); Py_DECREF(b);
    if (year_a < 0 || year_b < 0) {
        if (!PyErr_Occurred()) {
            PyErr_SetString(PyExc_RuntimeError,
                            "datetime_year_diff: failed to read .year attribute");
        }
        return NULL;
    }
    return PyLong_FromLong(year_b - year_a);
}

/* ---------- Capsule probe (exports a vtable) ---------- */
typedef struct {
    int api_major;
    int api_minor;
    double (*dot1d)(const double *, const double *, Py_ssize_t);
} _numpylike_capi;

static double capi_dot1d(const double *a, const double *b, Py_ssize_t n) {
    double acc = 0.0;
    for (Py_ssize_t i = 0; i < n; i++) acc += a[i] * b[i];
    return acc;
}

static _numpylike_capi _capi = { 1, 0, capi_dot1d };

/* ---------- Module-level methods ---------- */
static PyMethodDef Module_methods[] = {
    {"add",    (PyCFunction)uf_add,    METH_VARARGS, "elementwise add"},
    {"sub",    (PyCFunction)uf_sub,    METH_VARARGS, "elementwise sub"},
    {"mul",    (PyCFunction)uf_mul,    METH_VARARGS, "elementwise mul"},
    {"div",    (PyCFunction)uf_div,    METH_VARARGS, "elementwise div"},
    {"maximum",(PyCFunction)uf_max,    METH_VARARGS, "elementwise max"},
    {"minimum",(PyCFunction)uf_min,    METH_VARARGS, "elementwise min"},
    {"sqrt",   (PyCFunction)uf_sqrt,   METH_VARARGS, "elementwise sqrt"},
    {"abs",    (PyCFunction)uf_abs,    METH_VARARGS, "elementwise abs"},
    {"neg",    (PyCFunction)uf_neg,    METH_VARARGS, "elementwise neg"},
    {"log",    (PyCFunction)uf_log,    METH_VARARGS, "elementwise log"},
    {"exp",    (PyCFunction)uf_exp,    METH_VARARGS, "elementwise exp"},
    {"sin",    (PyCFunction)uf_sin,    METH_VARARGS, "elementwise sin"},
    {"cos",    (PyCFunction)uf_cos,    METH_VARARGS, "elementwise cos"},
    {"mask_select", (PyCFunction)mask_select, METH_VARARGS, "boolean filter"},
    {"arange", (PyCFunction)arange,    METH_VARARGS | METH_KEYWORDS, "range builder"},
    {"dot1d",  (PyCFunction)dot1d,     METH_VARARGS, "1-D dot product"},
    {"datetime_year_diff", (PyCFunction)datetime_year_diff, METH_VARARGS,
     "year diff between two PyDate objects"},
    {NULL, NULL, 0, NULL},
};

static struct PyModuleDef Module_def = {
    PyModuleDef_HEAD_INIT
    "_numpylike",
    "numpy-shaped fixture exercising the WeavePy C-API end-to-end",
    -1,
    Module_methods,
    NULL, NULL, NULL, NULL,
};

PyObject *PyInit__numpylike(void);

PyObject *PyInit__numpylike(void) {
    PyObject *m = PyModule_Create(&Module_def);
    if (!m) return NULL;

    /* Define the dtype helper class. */
    PyObject *dtype_t = PyType_FromSpec(&DType_spec);
    if (!dtype_t) { Py_DECREF(m); return NULL; }
    if (PyModule_AddObject(m, "dtype", dtype_t) < 0) {
        Py_DECREF(dtype_t); Py_DECREF(m); return NULL;
    }
    DTypeType_obj = (PyTypeObject *)dtype_t;
    Py_INCREF(dtype_t);

    /* Define the ndarray class. */
    PyObject *ndt = PyType_FromSpec(&NDArray_spec);
    if (!ndt) { Py_DECREF(m); return NULL; }
    if (PyModule_AddObject(m, "ndarray", ndt) < 0) {
        Py_DECREF(ndt); Py_DECREF(m); return NULL;
    }
    NDArrayType_obj = (PyTypeObject *)ndt;
    Py_INCREF(ndt);

    /* dtype constants. */
    PyModule_AddIntConstant(m, "INT8",    (int)DT_INT8);
    PyModule_AddIntConstant(m, "INT32",   (int)DT_INT32);
    PyModule_AddIntConstant(m, "INT64",   (int)DT_INT64);
    PyModule_AddIntConstant(m, "FLOAT32", (int)DT_FLOAT32);
    PyModule_AddIntConstant(m, "FLOAT64", (int)DT_FLOAT64);
    PyModule_AddIntConstant(m, "COMPLEX", (int)DT_COMPLEX);
    PyModule_AddIntConstant(m, "RECORD",  (int)DT_RECORD);

    /* Capsule export of internal vtable. */
    PyObject *capsule = PyCapsule_New(&_capi, "_numpylike._API", NULL);
    if (capsule) {
        PyModule_AddObject(m, "_API", capsule);
    }

    PyModule_AddStringConstant(m, "__version__", "0.1.0-rfc0029");
    return m;
}
