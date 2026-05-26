/*
 * _ndarray — a numpy-shaped C extension fixture used to exercise
 * the full PEP 3118 buffer protocol, the descriptor + dunder
 * dispatch surface, vectorcall, and the new generic-alloc family
 * added in RFC 0028.
 *
 * ## Storage model
 *
 * WeavePy's heap-type instances are Rust-side `Object::Instance`
 * values whose payload layout differs from CPython's
 * `(PyObject_HEAD, fields)` shape. To keep the fixture portable
 * across both runtimes we sidestep the difference by malloc'ing
 * the per-instance state in an `NDArrayCore` block and stashing a
 * `PyLong`-encoded pointer to it in `self.__dict__["_core_addr"]`.
 * Every method reads the address back out, casts it to
 * `NDArrayCore *`, and operates on the raw float64 storage. This
 * keeps the buffer protocol's exported pointer stable across
 * mutations, which the WeavePy `bytearray` storage does not.
 *
 * NOTE: this fixture intentionally leaks the `NDArrayCore` blocks
 * on instance destruction; a follow-up RFC will wire `tp_dealloc`
 * back to a Rust-side finaliser.
 */

#include "../../crates/weavepy-capi/include/Python.h"

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* ---------------- The side-allocated payload struct ---------------- */

typedef struct {
    Py_ssize_t rows;
    Py_ssize_t cols;
    double *data;          /* row-major */
    int exporter_count;
} NDArrayCore;

static int set_core_addr(PyObject *self, NDArrayCore *core) {
    PyObject *addr = PyLong_FromLongLong((long long)(intptr_t)core);
    if (!addr) return -1;
    int rc = PyObject_SetAttrString(self, "_core_addr", addr);
    Py_DECREF(addr);
    return rc;
}

static NDArrayCore *core_for(PyObject *self) {
    PyObject *attr = PyObject_GetAttrString(self, "_core_addr");
    if (!attr) return NULL;
    long long v = PyLong_AsLongLong(attr);
    Py_DECREF(attr);
    if (v == -1 && PyErr_Occurred()) return NULL;
    NDArrayCore *core = (NDArrayCore *)(intptr_t)v;
    if (!core) {
        PyErr_SetString(PyExc_RuntimeError, "NDArray: _core_addr is NULL");
        return NULL;
    }
    return core;
}

static int in_bounds(Py_ssize_t i, Py_ssize_t j, Py_ssize_t rows, Py_ssize_t cols) {
    return i >= 0 && i < rows && j >= 0 && j < cols;
}

/* ---------------- Lifecycle ---------------- */

static int NDArray_init(PyObject *self, PyObject *args, PyObject *kwargs) {
    (void)kwargs;
    Py_ssize_t rows = 0, cols = 0;
    if (!PyArg_ParseTuple(args, "nn", &rows, &cols)) {
        return -1;
    }
    if (rows < 0 || cols < 0) {
        PyErr_SetString(PyExc_ValueError, "NDArray: rows/cols must be >= 0");
        return -1;
    }
    NDArrayCore *core = (NDArrayCore *)PyMem_Calloc(1, sizeof(NDArrayCore));
    if (!core) {
        PyErr_NoMemory();
        return -1;
    }
    core->rows = rows;
    core->cols = cols;
    core->data = NULL;
    core->exporter_count = 0;
    size_t total = (size_t)rows * (size_t)cols;
    if (total > 0) {
        core->data = (double *)PyMem_Calloc(total, sizeof(double));
        if (!core->data) {
            PyMem_Free(core);
            PyErr_NoMemory();
            return -1;
        }
    }
    if (set_core_addr(self, core) != 0) {
        if (core->data) PyMem_Free(core->data);
        PyMem_Free(core);
        return -1;
    }
    return 0;
}

static PyObject *NDArray_repr(PyObject *self) {
    NDArrayCore *core = core_for(self);
    if (!core) return NULL;
    char buf[96];
    snprintf(buf, sizeof(buf), "<NDArray rows=%ld cols=%ld>",
             (long)core->rows, (long)core->cols);
    return PyUnicode_FromString(buf);
}

static PyObject *NDArray_str(PyObject *self) {
    return NDArray_repr(self);
}

/* ---------------- Buffer protocol ---------------- */

typedef struct {
    Py_ssize_t shape[2];
    Py_ssize_t strides[2];
    char format[2];
} NDArrayBufInternal;

static int NDArray_getbuffer(PyObject *exporter, Py_buffer *view, int flags) {
    NDArrayCore *core = core_for(exporter);
    if (!core) return -1;

    NDArrayBufInternal *internal = (NDArrayBufInternal *)PyMem_Malloc(sizeof(*internal));
    if (!internal) {
        PyErr_NoMemory();
        return -1;
    }
    internal->shape[0] = core->rows;
    internal->shape[1] = core->cols;
    internal->strides[0] = (Py_ssize_t)(sizeof(double) * core->cols);
    internal->strides[1] = (Py_ssize_t)sizeof(double);
    internal->format[0] = 'd';
    internal->format[1] = 0;

    view->buf = (void *)core->data;
    view->obj = exporter;
    view->len = (Py_ssize_t)(core->rows * core->cols * sizeof(double));
    view->itemsize = (Py_ssize_t)sizeof(double);
    view->readonly = 0;
    view->ndim = 2;
    view->format = (flags & PyBUF_FORMAT) ? internal->format : NULL;
    view->shape = (flags & PyBUF_ND) ? internal->shape : NULL;
    view->strides = (flags & 0x10) ? internal->strides : NULL;
    view->suboffsets = NULL;
    view->internal = (void *)internal;

    Py_INCREF(exporter);
    core->exporter_count += 1;
    return 0;
}

static void NDArray_releasebuffer(PyObject *exporter, Py_buffer *view) {
    NDArrayCore *core = core_for(exporter);
    if (view->internal) {
        PyMem_Free(view->internal);
        view->internal = NULL;
    }
    if (core && core->exporter_count > 0) {
        core->exporter_count -= 1;
    }
}

/* ---------------- Number protocol ---------------- */

static PyObject *make_like(PyObject *cls_obj, Py_ssize_t rows, Py_ssize_t cols) {
    PyObject *args = Py_BuildValue("(nn)", rows, cols);
    if (!args) return NULL;
    PyObject *out = PyObject_CallObject(cls_obj, args);
    Py_DECREF(args);
    return out;
}

static PyObject *binary_op(PyObject *a_obj, PyObject *b_obj, int op) {
    NDArrayCore *a = core_for(a_obj);
    if (!a) return NULL;
    NDArrayCore *b = core_for(b_obj);
    if (!b) return NULL;
    if (a->rows != b->rows || a->cols != b->cols) {
        PyErr_SetString(PyExc_ValueError, "NDArray binary op: shape mismatch");
        return NULL;
    }
    PyObject *result = make_like((PyObject *)Py_TYPE(a_obj), a->rows, a->cols);
    if (!result) return NULL;
    NDArrayCore *r = core_for(result);
    if (!r) { Py_DECREF(result); return NULL; }
    Py_ssize_t total = a->rows * a->cols;
    for (Py_ssize_t i = 0; i < total; i++) {
        double av = a->data[i];
        double bv = b->data[i];
        switch (op) {
            case 0: r->data[i] = av + bv; break;
            case 1: r->data[i] = av - bv; break;
            case 2: r->data[i] = av * bv; break;
        }
    }
    return result;
}

static PyObject *NDArray_add(PyObject *a, PyObject *b) { return binary_op(a, b, 0); }
static PyObject *NDArray_subtract(PyObject *a, PyObject *b) { return binary_op(a, b, 1); }
static PyObject *NDArray_multiply(PyObject *a, PyObject *b) { return binary_op(a, b, 2); }

/* ---------------- Sequence + mapping protocols ---------------- */

static Py_ssize_t NDArray_length(PyObject *self) {
    NDArrayCore *core = core_for(self);
    if (!core) return -1;
    return core->rows;
}

static PyObject *NDArray_item(PyObject *self, Py_ssize_t i) {
    NDArrayCore *core = core_for(self);
    if (!core) return NULL;
    if (i < 0 || i >= core->rows) {
        PyErr_SetString(PyExc_IndexError, "NDArray index out of range");
        return NULL;
    }
    PyObject *list = PyList_New(core->cols);
    if (!list) return NULL;
    for (Py_ssize_t j = 0; j < core->cols; j++) {
        PyObject *v = PyFloat_FromDouble(core->data[i * core->cols + j]);
        PyList_SetItem(list, j, v);
    }
    return list;
}

static PyObject *NDArray_subscript(PyObject *self, PyObject *key) {
    if (PyLong_Check(key)) {
        return NDArray_item(self, PyLong_AsSsize_t(key));
    }
    NDArrayCore *core = core_for(self);
    if (!core) return NULL;
    if (!(PyTuple_Check(key) && PyTuple_Size(key) == 2)) {
        PyErr_SetString(PyExc_TypeError, "NDArray subscript must be int or (int,int)");
        return NULL;
    }
    Py_ssize_t i = PyLong_AsSsize_t(PyTuple_GetItem(key, 0));
    Py_ssize_t j = PyLong_AsSsize_t(PyTuple_GetItem(key, 1));
    if (!in_bounds(i, j, core->rows, core->cols)) {
        PyErr_SetString(PyExc_IndexError, "NDArray index out of range");
        return NULL;
    }
    return PyFloat_FromDouble(core->data[i * core->cols + j]);
}

static int NDArray_ass_subscript(PyObject *self, PyObject *key, PyObject *value) {
    NDArrayCore *core = core_for(self);
    if (!core) return -1;
    if (!PyTuple_Check(key) || PyTuple_Size(key) != 2) {
        PyErr_SetString(PyExc_TypeError, "NDArray __setitem__: key must be (int,int)");
        return -1;
    }
    Py_ssize_t i = PyLong_AsSsize_t(PyTuple_GetItem(key, 0));
    Py_ssize_t j = PyLong_AsSsize_t(PyTuple_GetItem(key, 1));
    if (!in_bounds(i, j, core->rows, core->cols)) {
        PyErr_SetString(PyExc_IndexError, "NDArray index out of range");
        return -1;
    }
    double v = PyFloat_AsDouble(value);
    core->data[i * core->cols + j] = v;
    return 0;
}

/* ---------------- Iteration protocol ---------------- */

typedef struct {
    PyObject *array;       /* strong reference */
    Py_ssize_t cursor;
} NDArrayIterCore;

static int NDArrayIter_init(PyObject *self, PyObject *args, PyObject *kwargs) {
    (void)kwargs;
    PyObject *target = NULL;
    if (!PyArg_ParseTuple(args, "O", &target)) {
        return -1;
    }
    NDArrayIterCore *core = (NDArrayIterCore *)PyMem_Calloc(1, sizeof(NDArrayIterCore));
    if (!core) {
        PyErr_NoMemory();
        return -1;
    }
    Py_INCREF(target);
    core->array = target;
    core->cursor = 0;
    PyObject *addr = PyLong_FromLongLong((long long)(intptr_t)core);
    if (!addr) {
        Py_DECREF(target);
        PyMem_Free(core);
        return -1;
    }
    int rc = PyObject_SetAttrString(self, "_iter_core", addr);
    Py_DECREF(addr);
    if (rc != 0) {
        Py_DECREF(target);
        PyMem_Free(core);
        return -1;
    }
    return 0;
}

static NDArrayIterCore *iter_core(PyObject *self) {
    PyObject *attr = PyObject_GetAttrString(self, "_iter_core");
    if (!attr) return NULL;
    long long v = PyLong_AsLongLong(attr);
    Py_DECREF(attr);
    if (v == -1 && PyErr_Occurred()) return NULL;
    return (NDArrayIterCore *)(intptr_t)v;
}

static PyObject *NDArrayIter_iter(PyObject *self) {
    Py_INCREF(self);
    return self;
}

static PyObject *NDArrayIter_next(PyObject *self) {
    NDArrayIterCore *ic = iter_core(self);
    if (!ic) return NULL;
    NDArrayCore *core = core_for(ic->array);
    if (!core) return NULL;
    if (ic->cursor >= core->rows) {
        return NULL;  /* StopIteration: dunder shim handles this. */
    }
    Py_ssize_t row = ic->cursor++;
    return NDArray_item(ic->array, row);
}

static PyType_Slot NDArrayIter_slots[] = {
    {Py_tp_doc, (void *)"NDArray row iterator."},
    {Py_tp_init, NDArrayIter_init},
    {Py_tp_iter, NDArrayIter_iter},
    {Py_tp_iternext, NDArrayIter_next},
    {0, NULL},
};

static PyType_Spec NDArrayIter_spec = {
    "_ndarray.NDArrayIter",
    0,
    0,
    Py_TPFLAGS_DEFAULT,
    NDArrayIter_slots,
};

static PyObject *g_iter_type = NULL;

static PyObject *NDArray_iter(PyObject *self) {
    if (!g_iter_type) {
        PyErr_SetString(PyExc_RuntimeError, "NDArrayIter type not initialised");
        return NULL;
    }
    PyObject *args = Py_BuildValue("(O)", self);
    if (!args) return NULL;
    PyObject *it = PyObject_CallObject(g_iter_type, args);
    Py_DECREF(args);
    return it;
}

/* ---------------- Methods ---------------- */

static PyObject *NDArray_fill(PyObject *self, PyObject *args) {
    double v = 0.0;
    if (!PyArg_ParseTuple(args, "d", &v)) return NULL;
    NDArrayCore *core = core_for(self);
    if (!core) return NULL;
    Py_ssize_t total = core->rows * core->cols;
    for (Py_ssize_t i = 0; i < total; i++) {
        core->data[i] = v;
    }
    Py_INCREF(Py_None);
    return Py_None;
}

static PyObject *NDArray_sum(PyObject *self, PyObject *args) {
    (void)args;
    NDArrayCore *core = core_for(self);
    if (!core) return NULL;
    double s = 0.0;
    Py_ssize_t total = core->rows * core->cols;
    for (Py_ssize_t i = 0; i < total; i++) {
        s += core->data[i];
    }
    return PyFloat_FromDouble(s);
}

static PyObject *NDArray_to_bytes(PyObject *self, PyObject *args) {
    (void)args;
    NDArrayCore *core = core_for(self);
    if (!core) return NULL;
    Py_ssize_t total_bytes = (Py_ssize_t)(core->rows * core->cols * sizeof(double));
    return PyBytes_FromStringAndSize((const char *)core->data, total_bytes);
}

/* ---------------- Properties (getset) ---------------- */

static PyObject *NDArray_get_shape(PyObject *self, void *closure) {
    (void)closure;
    NDArrayCore *core = core_for(self);
    if (!core) return NULL;
    PyObject *t = PyTuple_New(2);
    PyTuple_SetItem(t, 0, PyLong_FromSsize_t(core->rows));
    PyTuple_SetItem(t, 1, PyLong_FromSsize_t(core->cols));
    return t;
}

static PyObject *NDArray_get_nbytes(PyObject *self, void *closure) {
    (void)closure;
    NDArrayCore *core = core_for(self);
    if (!core) return NULL;
    return PyLong_FromSsize_t(core->rows * core->cols * (Py_ssize_t)sizeof(double));
}

static PyObject *NDArray_get_exporter_count(PyObject *self, void *closure) {
    (void)closure;
    NDArrayCore *core = core_for(self);
    if (!core) return NULL;
    return PyLong_FromLong((long)core->exporter_count);
}

static PyGetSetDef NDArray_getset[] = {
    {"shape", NDArray_get_shape, NULL, "(rows, cols)", NULL},
    {"nbytes", NDArray_get_nbytes, NULL, "Total payload size in bytes", NULL},
    {"exporter_count", NDArray_get_exporter_count, NULL, "Live Py_buffer count", NULL},
    {NULL, NULL, NULL, NULL, NULL},
};

/* ---------------- Method table ---------------- */

static PyMethodDef NDArray_methods[] = {
    {"fill", NDArray_fill, METH_VARARGS, "Fill the array with a scalar"},
    {"sum", NDArray_sum, METH_NOARGS, "Sum all elements"},
    {"to_bytes", NDArray_to_bytes, METH_NOARGS, "Return raw payload as bytes"},
    {NULL, NULL, 0, NULL},
};

/* ---------------- Slot table + spec ---------------- */

static PyType_Slot NDArray_slots[] = {
    {Py_tp_doc, (void *)"A 2-D row-major float64 array."},
    {Py_tp_init, NDArray_init},
    {Py_tp_repr, NDArray_repr},
    {Py_tp_str, NDArray_str},

    {Py_bf_getbuffer, NDArray_getbuffer},
    {Py_bf_releasebuffer, NDArray_releasebuffer},

    {Py_nb_add, NDArray_add},
    {Py_nb_subtract, NDArray_subtract},
    {Py_nb_multiply, NDArray_multiply},

    {Py_sq_length, NDArray_length},
    {Py_sq_item, NDArray_item},
    {Py_mp_length, NDArray_length},
    {Py_mp_subscript, NDArray_subscript},
    {Py_mp_ass_subscript, NDArray_ass_subscript},

    {Py_tp_iter, NDArray_iter},
    {Py_tp_methods, NDArray_methods},
    {Py_tp_getset, NDArray_getset},
    {0, NULL},
};

static PyType_Spec NDArray_spec = {
    "_ndarray.NDArray",
    0,
    0,
    Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE,
    NDArray_slots,
};

/* ---------------- Module-level helpers ---------------- */

static PyObject *nd_buffer_size(PyObject *self, PyObject *args) {
    (void)self;
    PyObject *o = NULL;
    if (!PyArg_ParseTuple(args, "O", &o)) return NULL;
    Py_buffer view;
    if (PyObject_GetBuffer(o, &view, PyBUF_FULL_RO) != 0) return NULL;
    Py_ssize_t len = view.len;
    PyBuffer_Release(&view);
    return PyLong_FromSsize_t(len);
}

static PyObject *nd_format_size(PyObject *self, PyObject *args) {
    (void)self;
    const char *fmt = NULL;
    if (!PyArg_ParseTuple(args, "s", &fmt)) return NULL;
    return PyLong_FromSsize_t(PyBuffer_SizeFromFormat(fmt));
}

static PyObject *nd_buffer_isc(PyObject *self, PyObject *args) {
    (void)self;
    PyObject *o = NULL;
    if (!PyArg_ParseTuple(args, "O", &o)) return NULL;
    Py_buffer view;
    if (PyObject_GetBuffer(o, &view, PyBUF_STRIDES) != 0) return NULL;
    int c = PyBuffer_IsContiguous(&view, 'C');
    PyBuffer_Release(&view);
    return PyBool_FromLong((long)c);
}

static PyMethodDef _ndarray_methods[] = {
    {"buffer_size", nd_buffer_size, METH_VARARGS, "Get a Py_buffer view's len"},
    {"format_size", nd_format_size, METH_VARARGS, "PyBuffer_SizeFromFormat"},
    {"buffer_is_c_contig", nd_buffer_isc, METH_VARARGS, "PyBuffer_IsContiguous(view, 'C')"},
    {NULL, NULL, 0, NULL},
};

static struct PyModuleDef _ndarray_def = {
    PyModuleDef_HEAD_INIT
    "_ndarray",
    "WeavePy buffer-protocol fixture (RFC 0028)",
    -1,
    _ndarray_methods,
    NULL, NULL, NULL, NULL,
};

PyObject *PyInit__ndarray(void);

PyObject *PyInit__ndarray(void) {
    PyObject *m = PyModule_Create(&_ndarray_def);
    if (!m) return NULL;

    PyObject *iter_type = PyType_FromSpec(&NDArrayIter_spec);
    if (!iter_type) { Py_DECREF(m); return NULL; }
    g_iter_type = iter_type;
    if (PyModule_AddObject(m, "NDArrayIter", iter_type) != 0) {
        Py_DECREF(iter_type);
        Py_DECREF(m);
        return NULL;
    }

    PyObject *type = PyType_FromSpec(&NDArray_spec);
    if (!type) {
        Py_DECREF(m);
        return NULL;
    }
    if (PyModule_AddObject(m, "NDArray", type) != 0) {
        Py_DECREF(type);
        Py_DECREF(m);
        return NULL;
    }
    PyModule_AddStringConstant(m, "VERSION", "0.1");
    PyModule_AddIntConstant(m, "DOUBLE_SIZE", (long)sizeof(double));

    return m;
}
