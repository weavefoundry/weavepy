/*
 * _stockcython — the RFC 0047 (binary-ABI, wave 5) hermetic proof.
 *
 * Compiled against the host's **stock CPython 3.13 headers** (full,
 * non-limited API) like `_stockabi`/`_stocktype`/`_stockarray`, this
 * fixture stands in for a **Cython-generated** extension (the shape
 * pandas and the wider Cython ecosystem ship). It proves the two
 * load-bearing wave-5 capabilities:
 *
 *   1. **Faithful `inherit_slots`.** A base type (`CyBase`) defines a
 *      number suite (`nb_add`), a sequence suite (`sq_length`),
 *      `tp_repr`, `tp_hash`, and `tp_richcompare`. Two subclasses
 *      define (almost) nothing:
 *        - `CySub`  — a *pure* behaviour subclass (no slots of its own);
 *        - `CySub2` — a *partial-override* subclass: its own `tp_repr`
 *          and a number suite carrying only `nb_subtract`, inheriting
 *          `nb_add` into that same suite.
 *      The proof reads the slots **directly off `Py_TYPE(instance)`** —
 *      the inlined idiom Cython emits everywhere
 *      (`Py_TYPE(o)->tp_as_number->nb_add(...)`), with no MRO walk — and
 *      invokes them. Before wave-5 those reads were NULL on a subclass;
 *      `probe_slots` asserts they now resolve to the inherited (or, for
 *      `CySub2.tp_repr`/`nb_subtract`, the own) function.
 *
 *   2. **The Cython C-API runtime tail.** `cython_runtime_surface`
 *      exercises the leaf helpers the Cython runtime links that wave 5
 *      adds (`_PyObject_GetDictPtr`, `PyObject_GetOptionalAttrString`,
 *      `_PyObject_GetMethod`, `PyObject_CallMethodOneArg`,
 *      `_PyDict_NewPresized`, `PyMapping_GetOptionalItemString`,
 *      `PyLong_AsInt`).
 *
 * ## Storage model
 *
 * Like `_stocktype.c`, each instance stashes its state in a malloc'd
 * `*Core` block whose pointer lives in `self.__dict__["_core_addr"]`
 * (the `_ndarray` idiom); `tp_basicsize == sizeof(PyObject)`, so this is
 * the dict-backed path, *not* wave-3 inline storage. The whole point is
 * the slot *inheritance*, which is orthogonal to instance layout.
 */

#define PY_SSIZE_T_CLEAN
#include <Python.h>

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

/* Cython-generated code vendors its own `extern` declarations for the
 * private CPython helpers it links (they are not all in the public
 * headers). We mirror that here so the fixture is a faithful stand-in:
 * `_PyObject_GetMethod` and `_PyDict_NewPresized` are internal and not
 * declared by `Python.h`. Their signatures have been stable across the
 * 3.x series. */
#ifdef __cplusplus
extern "C" {
#endif
extern int _PyObject_GetMethod(PyObject *obj, PyObject *name, PyObject **method);
extern PyObject *_PyDict_NewPresized(Py_ssize_t minused);
#ifdef __cplusplus
}
#endif

/* ================================================================== */
/* Shared helpers.                                                    */
/* ================================================================== */

static void dict_set_int(PyObject *d, const char *k, long v) {
    PyObject *o = PyLong_FromLong(v);
    if (o) {
        PyDict_SetItemString(d, k, o);
        Py_DECREF(o);
    }
}

static int set_core_addr(PyObject *self, void *core) {
    PyObject *addr = PyLong_FromLongLong((long long)(intptr_t)core);
    if (!addr) {
        return -1;
    }
    int rc = PyObject_SetAttrString(self, "_core_addr", addr);
    Py_DECREF(addr);
    return rc;
}

static void *core_addr_noerr(PyObject *self) {
    PyObject *attr = PyObject_GetAttrString(self, "_core_addr");
    if (!attr) {
        PyErr_Clear();
        return NULL;
    }
    long long v = PyLong_AsLongLong(attr);
    Py_DECREF(attr);
    if (v == -1 && PyErr_Occurred()) {
        PyErr_Clear();
        return NULL;
    }
    return (void *)(intptr_t)v;
}

/* ================================================================== */
/* CyBase — the base whose slots get inherited.                       */
/* ================================================================== */

typedef struct {
    long value;
} CyBaseCore;

static CyBaseCore *cybase_core(PyObject *self) {
    void *p = core_addr_noerr(self);
    if (!p) {
        if (!PyErr_Occurred()) {
            PyErr_SetString(PyExc_RuntimeError, "CyBase: missing core");
        }
        return NULL;
    }
    return (CyBaseCore *)p;
}

static int CyBase_init(PyObject *self, PyObject *args, PyObject *kwds) {
    (void)kwds;
    long value = 0;
    if (!PyArg_ParseTuple(args, "l", &value)) {
        return -1;
    }
    CyBaseCore *core = (CyBaseCore *)malloc(sizeof(CyBaseCore));
    if (!core) {
        PyErr_NoMemory();
        return -1;
    }
    core->value = value;
    if (set_core_addr(self, core) != 0) {
        free(core);
        return -1;
    }
    return 0;
}

static PyObject *CyBase_repr(PyObject *self) {
    CyBaseCore *core = cybase_core(self);
    if (!core) {
        return NULL;
    }
    char buf[64];
    snprintf(buf, sizeof(buf), "CyBase(%ld)", core->value);
    return PyUnicode_FromString(buf);
}

static Py_hash_t CyBase_hash(PyObject *self) {
    CyBaseCore *core = cybase_core(self);
    if (!core) {
        return -1;
    }
    return (Py_hash_t)core->value;
}

static PyObject *CyBase_add(PyObject *a, PyObject *b) {
    CyBaseCore *ca = cybase_core(a);
    CyBaseCore *cb = cybase_core(b);
    if (!ca || !cb) {
        return NULL;
    }
    return PyLong_FromLong(ca->value + cb->value);
}

static Py_ssize_t CyBase_length(PyObject *self) {
    CyBaseCore *core = cybase_core(self);
    if (!core) {
        return -1;
    }
    return (Py_ssize_t)core->value;
}

static PyObject *CyBase_richcompare(PyObject *a, PyObject *b, int op) {
    if (op != Py_EQ && op != Py_NE) {
        Py_RETURN_NOTIMPLEMENTED;
    }
    CyBaseCore *ca = cybase_core(a);
    if (!ca) {
        return NULL;
    }
    CyBaseCore *cb = cybase_core(b);
    if (!cb) {
        PyErr_Clear();
        Py_RETURN_NOTIMPLEMENTED;
    }
    int eq = (ca->value == cb->value);
    if (op == Py_NE) {
        eq = !eq;
    }
    if (eq) {
        Py_RETURN_TRUE;
    }
    Py_RETURN_FALSE;
}

static PyNumberMethods CyBase_as_number = {
    .nb_add = CyBase_add,
};

static PySequenceMethods CyBase_as_sequence = {
    .sq_length = CyBase_length,
};

static PyTypeObject CyBase_Type = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "_stockcython.CyBase",
    .tp_basicsize = sizeof(PyObject),
    .tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE,
    .tp_doc = "base type whose slots are inherited (inherit_slots proof)",
    .tp_new = PyType_GenericNew,
    .tp_init = CyBase_init,
    .tp_repr = CyBase_repr,
    .tp_hash = CyBase_hash,
    .tp_richcompare = CyBase_richcompare,
    .tp_as_number = &CyBase_as_number,
    .tp_as_sequence = &CyBase_as_sequence,
};

/* ================================================================== */
/* CySub — pure behaviour subclass: defines NOTHING of its own.       */
/* Every slot it dispatches must come from CyBase via inherit_slots.  */
/* ================================================================== */

static PyTypeObject CySub_Type = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "_stockcython.CySub",
    .tp_basicsize = sizeof(PyObject),
    .tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE,
    .tp_doc = "pure subclass (inherits every slot from CyBase)",
    .tp_base = &CyBase_Type,
};

/* ================================================================== */
/* CySub2 — partial-override subclass: its own tp_repr + a number     */
/* suite carrying ONLY nb_subtract. inherit_slots must (a) keep its   */
/* own tp_repr / nb_subtract, and (b) fill nb_add into its *existing* */
/* suite from CyBase (the in-place suite merge path).                 */
/* ================================================================== */

static PyObject *CySub2_repr(PyObject *self) {
    CyBaseCore *core = cybase_core(self);
    if (!core) {
        return NULL;
    }
    char buf[64];
    snprintf(buf, sizeof(buf), "CySub2(%ld)", core->value);
    return PyUnicode_FromString(buf);
}

static PyObject *CySub2_sub(PyObject *a, PyObject *b) {
    CyBaseCore *ca = cybase_core(a);
    CyBaseCore *cb = cybase_core(b);
    if (!ca || !cb) {
        return NULL;
    }
    return PyLong_FromLong(ca->value - cb->value);
}

static PyNumberMethods CySub2_as_number = {
    .nb_subtract = CySub2_sub,
};

static PyTypeObject CySub2_Type = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "_stockcython.CySub2",
    .tp_basicsize = sizeof(PyObject),
    .tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE,
    .tp_doc = "partial subclass (own tp_repr + nb_subtract; inherits the rest)",
    .tp_base = &CyBase_Type,
    .tp_repr = CySub2_repr,
    .tp_as_number = &CySub2_as_number,
};

/* ================================================================== */
/* probe_slots(obj) — read the slots DIRECTLY off Py_TYPE(obj) (the   */
/* inlined Cython idiom) and invoke them, returning a result dict.    */
/* ================================================================== */

static PyObject *probe_slots(PyObject *self, PyObject *arg) {
    (void)self;
    PyTypeObject *t = Py_TYPE(arg);
    PyObject *res = PyDict_New();
    if (!res) {
        return NULL;
    }

    int has_repr = (t->tp_repr != NULL);
    int has_hash = (t->tp_hash != NULL);
    int has_nb_add = (t->tp_as_number != NULL && t->tp_as_number->nb_add != NULL);
    int has_nb_sub = (t->tp_as_number != NULL && t->tp_as_number->nb_subtract != NULL);
    int has_sq_len = (t->tp_as_sequence != NULL && t->tp_as_sequence->sq_length != NULL);
    int has_cmp = (t->tp_richcompare != NULL);
    dict_set_int(res, "has_repr", has_repr);
    dict_set_int(res, "has_hash", has_hash);
    dict_set_int(res, "has_nb_add", has_nb_add);
    dict_set_int(res, "has_nb_sub", has_nb_sub);
    dict_set_int(res, "has_sq_len", has_sq_len);
    dict_set_int(res, "has_cmp", has_cmp);

    if (has_repr) {
        PyObject *r = t->tp_repr(arg);
        if (r) {
            PyDict_SetItemString(res, "repr", r);
            Py_DECREF(r);
        } else {
            PyErr_Clear();
        }
    }
    if (has_hash) {
        dict_set_int(res, "hash", (long)t->tp_hash(arg));
    }
    if (has_sq_len) {
        dict_set_int(res, "len", (long)t->tp_as_sequence->sq_length(arg));
    }
    if (has_nb_add) {
        PyObject *s = t->tp_as_number->nb_add(arg, arg);
        if (s) {
            dict_set_int(res, "add", PyLong_AsLong(s));
            Py_DECREF(s);
        } else {
            PyErr_Clear();
        }
    }
    if (has_nb_sub) {
        PyObject *s = t->tp_as_number->nb_subtract(arg, arg);
        if (s) {
            dict_set_int(res, "sub", PyLong_AsLong(s));
            Py_DECREF(s);
        } else {
            PyErr_Clear();
        }
    }
    return res;
}

/* ================================================================== */
/* cython_runtime_surface(obj) — exercise the wave-5 Cython tail.     */
/* ================================================================== */

static PyObject *cython_runtime_surface(PyObject *self, PyObject *obj) {
    (void)self;
    PyObject *res = PyDict_New();
    if (!res) {
        return NULL;
    }

    /* _PyObject_GetDictPtr → NULL (no tp_dictoffset); the Cython idiom
     * then falls back to generic getattr. */
    PyObject **dictptr = _PyObject_GetDictPtr(obj);
    dict_set_int(res, "dictptr_null", dictptr == NULL);

    /* PyObject_GetOptionalAttrString: present vs. missing. */
    PyObject *present = NULL;
    dict_set_int(res, "opt_present",
                 PyObject_GetOptionalAttrString(obj, "__class__", &present));
    Py_XDECREF(present);
    PyObject *absent = NULL;
    dict_set_int(res, "opt_absent",
                 PyObject_GetOptionalAttrString(obj, "no_such_attr_zzz", &absent));
    Py_XDECREF(absent);

    /* _PyObject_GetMethod: resolve a method handle. */
    PyObject *mname = PyUnicode_FromString("__class__");
    PyObject *method = NULL;
    int gm = _PyObject_GetMethod(obj, mname, &method);
    Py_XDECREF(mname);
    dict_set_int(res, "get_method_rc", gm);
    dict_set_int(res, "get_method_ok", method != NULL);
    Py_XDECREF(method);

    /* PyObject_CallMethodOneArg: obj.__eq__(obj) is truthy. */
    PyObject *eqname = PyUnicode_FromString("__eq__");
    PyObject *eqres = PyObject_CallMethodOneArg(obj, eqname, obj);
    Py_XDECREF(eqname);
    if (eqres) {
        dict_set_int(res, "call_eq_true", PyObject_IsTrue(eqres));
        Py_DECREF(eqres);
    } else {
        PyErr_Clear();
        dict_set_int(res, "call_eq_true", -1);
    }

    /* _PyDict_NewPresized + PyMapping_GetOptionalItemString. */
    PyObject *d = _PyDict_NewPresized(8);
    PyObject *v = PyLong_FromLong(99);
    PyDict_SetItemString(d, "k", v);
    Py_DECREF(v);
    PyObject *got = NULL;
    dict_set_int(res, "map_present", PyMapping_GetOptionalItemString(d, "k", &got));
    dict_set_int(res, "map_value", got ? PyLong_AsLong(got) : -1);
    Py_XDECREF(got);
    PyObject *got2 = NULL;
    dict_set_int(res, "map_absent", PyMapping_GetOptionalItemString(d, "missing", &got2));
    Py_XDECREF(got2);
    Py_DECREF(d);

    /* PyLong_AsInt. */
    PyObject *n = PyLong_FromLong(4242);
    dict_set_int(res, "as_int", PyLong_AsInt(n));
    Py_DECREF(n);

    return res;
}

/* ================================================================== */
/* Module.                                                            */
/* ================================================================== */

static PyMethodDef cy_methods[] = {
    {"probe_slots", probe_slots, METH_O,
     "read Py_TYPE(obj)'s slots directly and invoke them (inherit_slots proof)"},
    {"cython_runtime_surface", cython_runtime_surface, METH_O,
     "exercise the wave-5 Cython C-API tail"},
    {NULL, NULL, 0, NULL},
};

static struct PyModuleDef cy_module = {
    PyModuleDef_HEAD_INIT,
    "_stockcython",
    "RFC 0047 wave-5 stock-CPython-3.13 Cython-surface proof.",
    -1,
    cy_methods,
    NULL,
    NULL,
    NULL,
    NULL,
};

static int add_type(PyObject *m, PyTypeObject *t, const char *name) {
    if (PyType_Ready(t) < 0) {
        return -1;
    }
    Py_INCREF(t);
    if (PyModule_AddObject(m, name, (PyObject *)t) < 0) {
        Py_DECREF(t);
        return -1;
    }
    return 0;
}

PyMODINIT_FUNC PyInit__stockcython(void) {
    PyObject *m = PyModule_Create(&cy_module);
    if (!m) {
        return NULL;
    }
    /* Ready the base first so the subclasses' tp_base resolves to an
     * already-flattened type (inherit_slots copies one level). */
    if (add_type(m, &CyBase_Type, "CyBase") < 0 ||
        add_type(m, &CySub_Type, "CySub") < 0 ||
        add_type(m, &CySub2_Type, "CySub2") < 0) {
        Py_DECREF(m);
        return NULL;
    }
    PyModule_AddStringConstant(m, "ABI", "cp313");
    return m;
}
