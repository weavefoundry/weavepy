/*
 * _greenletconsumer — the RFC 0072 WS1 greenlet C-API proof.
 *
 * Compiled against the stock CPython 3.13 headers plus the vendored
 * upstream `greenlet/greenlet.h` (greenlet 3.2 line). It consumes the
 * `greenlet._C_API` capsule exactly the way gevent's compiled Cython
 * modules do:
 *
 *   - `PyGreenlet_Import()`   → PyCapsule_Import("greenlet._C_API", 0)
 *   - the Python-visible class imported and size-checked against
 *     `sizeof(PyGreenlet)` (the `__Pyx_ImportType` contract from
 *     `ctypedef class greenlet.greenlet [object PyGreenlet]`)
 *   - a static C subclass with `tp_base` = the imported type and a cdef
 *     field at offset `sizeof(PyGreenlet)` (gevent's
 *     `SwitchOutGreenletWithLoop.loop` shape), constructed through the
 *     inherited `tp_new` chain
 *   - `PyGreenlet_GetCurrent` / `PyGreenlet_Switch(g, NULL, NULL)` /
 *     `PyGreenlet_Throw` / `PyGreenlet_SetParent` and the four accessor
 *     entries (`MAIN`/`STARTED`/`ACTIVE`/`GET_PARENT`)
 *   - **switch-from-inside-a-C-frame**: `switch_to` is a C function a
 *     greenlet's `run` can call, parking the whole native stack — C
 *     frame included — which is the shape gevent's hub switches have
 *     (and the fixture RFC 0066 promised).
 */

#define PY_SSIZE_T_CLEAN
#include <Python.h>
#include <stddef.h>
#include "greenlet/greenlet.h"

/* The `__Pyx_ImportType` result: the Python-visible greenlet class. */
static PyTypeObject *ImportedGreenlet = NULL;
static int gc_imported = 0;

/* ---- the C subclass (gevent's TrackedRawGreenlet shape) ---- */

typedef struct {
    PyGreenlet base;
    PyObject *tag; /* offset sizeof(PyGreenlet) == 40 on LP64 */
} SubGreenlet;

static PyObject *sub_tp_new(PyTypeObject *type, PyObject *args, PyObject *kwds) {
    /* Cython chains straight through the imported base's tp_new. */
    newfunc base_new = ImportedGreenlet->tp_new;
    PyObject *o;
    if (base_new == NULL) {
        PyErr_SetString(PyExc_RuntimeError, "imported greenlet type has no tp_new");
        return NULL;
    }
    o = base_new(type, args, kwds);
    if (o != NULL) {
        ((SubGreenlet *)o)->tag = NULL;
    }
    return o;
}

static int sub_tp_init(PyObject *self, PyObject *args, PyObject *kwds) {
    /* gevent's TrackedRawGreenlet.__init__ chains
     * greenlet.__init__(self, run, parent). */
    PyObject *run = NULL;
    PyObject *base_init, *r;
    (void)kwds;
    if (!PyArg_ParseTuple(args, "|O:SubGreenlet", &run)) {
        return -1;
    }
    base_init = PyObject_GetAttrString((PyObject *)ImportedGreenlet, "__init__");
    if (base_init == NULL) {
        return -1;
    }
    if (run != NULL) {
        r = PyObject_CallFunctionObjArgs(base_init, self, run, NULL);
    } else {
        r = PyObject_CallFunctionObjArgs(base_init, self, NULL);
    }
    Py_DECREF(base_init);
    if (r == NULL) {
        return -1;
    }
    Py_DECREF(r);
    return 0;
}

static PyObject *sub_set_tag(PyObject *self, PyObject *v) {
    SubGreenlet *g = (SubGreenlet *)self;
    PyObject *old = g->tag;
    Py_INCREF(v);
    g->tag = v; /* direct C-field write at offset 40 */
    Py_XDECREF(old);
    Py_RETURN_NONE;
}

static PyObject *sub_get_tag(PyObject *self, PyObject *ignored) {
    SubGreenlet *g = (SubGreenlet *)self;
    (void)ignored;
    if (g->tag == NULL) {
        Py_RETURN_NONE;
    }
    Py_INCREF(g->tag);
    return g->tag;
}

static PyMethodDef sub_methods[] = {
    {"set_tag", (PyCFunction)sub_set_tag, METH_O, "store a value in the C field"},
    {"get_tag", (PyCFunction)sub_get_tag, METH_NOARGS, "read the C field"},
    {NULL, NULL, 0, NULL},
};

static PyTypeObject SubGreenlet_Type = {
    PyVarObject_HEAD_INIT(NULL, 0)
    "_greenletconsumer.SubGreenlet",     /* tp_name */
    sizeof(SubGreenlet),                 /* tp_basicsize */
    0,                                   /* tp_itemsize */
    0,                                   /* tp_dealloc */
    0,                                   /* tp_vectorcall_offset */
    0, 0, 0,                             /* tp_getattr, tp_setattr, tp_as_async */
    0,                                   /* tp_repr */
    0, 0, 0,                             /* tp_as_number, tp_as_sequence, tp_as_mapping */
    0, 0, 0, 0, 0,                       /* tp_hash, tp_call, tp_str, tp_getattro, tp_setattro */
    0,                                   /* tp_as_buffer */
    Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE, /* tp_flags */
    "C subclass of greenlet with a field at sizeof(PyGreenlet)", /* tp_doc */
    0, 0, 0,                             /* tp_traverse, tp_clear, tp_richcompare */
    0,                                   /* tp_weaklistoffset */
    0, 0,                                /* tp_iter, tp_iternext */
    sub_methods,                         /* tp_methods */
    0, 0,                                /* tp_members, tp_getset */
    0,                                   /* tp_base — set at module init */
    0, 0, 0,                             /* tp_dict, tp_descr_get, tp_descr_set */
    0,                                   /* tp_dictoffset */
    sub_tp_init,                         /* tp_init */
    0,                                   /* tp_alloc */
    sub_tp_new,                          /* tp_new */
};

/* ---- module functions over the capsule table ---- */

static PyObject *gc_get_current(PyObject *self, PyObject *ignored) {
    (void)self;
    (void)ignored;
    return (PyObject *)PyGreenlet_GetCurrent();
}

static PyObject *gc_current_is_main(PyObject *self, PyObject *ignored) {
    PyGreenlet *cur;
    int m;
    (void)self;
    (void)ignored;
    cur = PyGreenlet_GetCurrent();
    if (cur == NULL) {
        return NULL;
    }
    m = PyGreenlet_MAIN(cur);
    Py_DECREF(cur);
    if (m < 0) {
        return NULL;
    }
    return PyLong_FromLong(m);
}

static PyObject *gc_new_greenlet(PyObject *self, PyObject *run) {
    (void)self;
    return (PyObject *)PyGreenlet_New(run, NULL);
}

/* The switch-under-C-frame entry: any greenlet's Python code can call
 * this, and the whole native stack — this C frame included — parks
 * until control comes back. args/kwargs may be None. */
static PyObject *gc_switch_to(PyObject *self, PyObject *args) {
    PyObject *g, *sargs = NULL, *skwargs = NULL;
    (void)self;
    if (!PyArg_ParseTuple(args, "O|OO:switch_to", &g, &sargs, &skwargs)) {
        return NULL;
    }
    if (!PyGreenlet_Check(g)) {
        PyErr_SetString(PyExc_TypeError, "switch_to: not a greenlet");
        return NULL;
    }
    if (sargs == Py_None) {
        sargs = NULL;
    }
    if (skwargs == Py_None) {
        skwargs = NULL;
    }
    return PyGreenlet_Switch((PyGreenlet *)g, sargs, skwargs);
}

static PyObject *gc_throw_into(PyObject *self, PyObject *args) {
    PyObject *g, *typ, *val = NULL;
    (void)self;
    if (!PyArg_ParseTuple(args, "OO|O:throw_into", &g, &typ, &val)) {
        return NULL;
    }
    if (!PyGreenlet_Check(g)) {
        PyErr_SetString(PyExc_TypeError, "throw_into: not a greenlet");
        return NULL;
    }
    if (val == Py_None) {
        val = NULL;
    }
    return PyGreenlet_Throw((PyGreenlet *)g, typ, val, NULL);
}

static PyObject *gc_predicates(PyObject *self, PyObject *g) {
    int m, s, a;
    (void)self;
    if (!PyGreenlet_Check(g)) {
        PyErr_SetString(PyExc_TypeError, "predicates: not a greenlet");
        return NULL;
    }
    m = PyGreenlet_MAIN((PyGreenlet *)g);
    s = PyGreenlet_STARTED((PyGreenlet *)g);
    a = PyGreenlet_ACTIVE((PyGreenlet *)g);
    if (m < 0 || s < 0 || a < 0) {
        return NULL;
    }
    return Py_BuildValue("(iii)", m, s, a);
}

static PyObject *gc_get_parent(PyObject *self, PyObject *g) {
    PyGreenlet *p;
    (void)self;
    if (!PyGreenlet_Check(g)) {
        PyErr_SetString(PyExc_TypeError, "get_parent: not a greenlet");
        return NULL;
    }
    p = PyGreenlet_GetParent((PyGreenlet *)g);
    if (p == NULL) {
        if (PyErr_Occurred()) {
            return NULL;
        }
        Py_RETURN_NONE; /* main: NULL without an exception */
    }
    return (PyObject *)p;
}

static PyObject *gc_set_parent(PyObject *self, PyObject *args) {
    PyObject *g, *np;
    (void)self;
    if (!PyArg_ParseTuple(args, "OO:set_parent", &g, &np)) {
        return NULL;
    }
    if (PyGreenlet_SetParent((PyGreenlet *)g, (PyGreenlet *)np) < 0) {
        return NULL;
    }
    Py_RETURN_NONE;
}

static PyObject *gc_exc_greenlet_error(PyObject *self, PyObject *ignored) {
    (void)self;
    (void)ignored;
    Py_INCREF(PyExc_GreenletError);
    return PyExc_GreenletError;
}

static PyObject *gc_exc_greenlet_exit(PyObject *self, PyObject *ignored) {
    (void)self;
    (void)ignored;
    Py_INCREF(PyExc_GreenletExit);
    return PyExc_GreenletExit;
}

static PyObject *gc_type_check(PyObject *self, PyObject *o) {
    (void)self;
    return PyLong_FromLong(PyGreenlet_Check(o) ? 1 : 0);
}

static PyMethodDef gc_methods[] = {
    {"get_current", gc_get_current, METH_NOARGS, "PyGreenlet_GetCurrent()"},
    {"current_is_main", gc_current_is_main, METH_NOARGS, "PyGreenlet_MAIN(getcurrent())"},
    {"new_greenlet", gc_new_greenlet, METH_O, "PyGreenlet_New(run, NULL)"},
    {"switch_to", gc_switch_to, METH_VARARGS, "PyGreenlet_Switch — from a C frame"},
    {"throw_into", gc_throw_into, METH_VARARGS, "PyGreenlet_Throw"},
    {"predicates", gc_predicates, METH_O, "(MAIN, STARTED, ACTIVE)"},
    {"get_parent", gc_get_parent, METH_O, "PyGreenlet_GetParent"},
    {"set_parent", gc_set_parent, METH_VARARGS, "PyGreenlet_SetParent"},
    {"exc_greenlet_error", gc_exc_greenlet_error, METH_NOARGS, "capsule slot 1"},
    {"exc_greenlet_exit", gc_exc_greenlet_exit, METH_NOARGS, "capsule slot 2"},
    {"type_check", gc_type_check, METH_O, "PyGreenlet_Check"},
    {NULL, NULL, 0, NULL},
};

static struct PyModuleDef gc_module = {
    PyModuleDef_HEAD_INIT,
    "_greenletconsumer",
    "gevent-shaped consumer of the greenlet C-API capsule",
    -1,
    gc_methods,
    NULL,
    NULL,
    NULL,
    NULL,
};

/* The `__Pyx_ImportType` contract: import the class, check basicsize.
 * Errors when smaller than the header struct (Cython would refuse the
 * import); accepts equal (the clean case this fixture asserts
 * separately through `imported_basicsize`). */
static PyTypeObject *import_greenlet_type(void) {
    PyObject *module, *cls;
    PyTypeObject *t;
    module = PyImport_ImportModule("greenlet");
    if (module == NULL) {
        return NULL;
    }
    cls = PyObject_GetAttrString(module, "greenlet");
    Py_DECREF(module);
    if (cls == NULL) {
        return NULL;
    }
    if (!PyType_Check(cls)) {
        Py_DECREF(cls);
        PyErr_SetString(PyExc_TypeError, "greenlet.greenlet is not a type");
        return NULL;
    }
    t = (PyTypeObject *)cls;
    if (t->tp_basicsize < (Py_ssize_t)sizeof(PyGreenlet)) {
        PyErr_Format(PyExc_ValueError,
                     "greenlet.greenlet size %zd smaller than sizeof(PyGreenlet) %zu",
                     t->tp_basicsize, sizeof(PyGreenlet));
        Py_DECREF(cls);
        return NULL;
    }
    return t; /* owns the reference for the process lifetime */
}

PyMODINIT_FUNC PyInit__greenletconsumer(void) {
    PyObject *m;

    PyGreenlet_Import();
    gc_imported = (_PyGreenlet_API != NULL) ? 1 : 0;
    if (!gc_imported) {
        return NULL;
    }

    ImportedGreenlet = import_greenlet_type();
    if (ImportedGreenlet == NULL) {
        return NULL;
    }

    SubGreenlet_Type.tp_base = ImportedGreenlet;
    if (PyType_Ready(&SubGreenlet_Type) < 0) {
        return NULL;
    }

    m = PyModule_Create(&gc_module);
    if (m == NULL) {
        return NULL;
    }

    PyModule_AddIntConstant(m, "imported", gc_imported);
    PyModule_AddIntConstant(m, "header_sizeof", (long)sizeof(PyGreenlet));
    PyModule_AddIntConstant(m, "imported_basicsize", (long)ImportedGreenlet->tp_basicsize);
    PyModule_AddIntConstant(m, "capsule_basicsize",
                            (long)((PyTypeObject *)_PyGreenlet_API[PyGreenlet_Type_NUM])
                                ->tp_basicsize);
    PyModule_AddIntConstant(
        m, "types_match",
        ((PyTypeObject *)_PyGreenlet_API[PyGreenlet_Type_NUM] == ImportedGreenlet) ? 1 : 0);
    PyModule_AddIntConstant(m, "sub_field_offset", (long)offsetof(SubGreenlet, tag));

    Py_INCREF((PyObject *)&SubGreenlet_Type);
    PyModule_AddObject(m, "SubGreenlet", (PyObject *)&SubGreenlet_Type);

    return m;
}
