/*
 * _stocktype — the RFC 0044 (binary-ABI type suite, wave 2) hermetic
 * proof.
 *
 * Like `_stockabi.c`, this module is compiled against the host's **stock
 * CPython 3.13 headers** with the *full* (non-limited) API, so it sees
 * the genuine 416-byte `PyTypeObject`, the real method-suite structs
 * (`PyNumberMethods`, `PySequenceMethods`, `PyMappingMethods`), and the
 * inlined head macros. Where `_stockabi` proved WeavePy's object
 * *mirrors*, this fixture proves WeavePy's *type* machinery:
 *
 *   - the classic **static `PyTypeObject` + `PyType_Ready`** pattern
 *     (NOT `PyType_FromSpec`), with method suites referenced by direct
 *     pointer — the shape every hand-written C extension and every
 *     Cython-pre-3.0 wheel still uses;
 *   - number (`nb_add`/`nb_subtract`) + rich comparison (`tp_richcompare`),
 *     including *constructing a readied type by calling it through the
 *     C call protocol* (`PyObject_CallFunction((PyObject *)&Type, …)`),
 *     both at top level and re-entrantly from inside a slot;
 *   - sequence (`sq_length`/`sq_item`) + mapping (`mp_length`/
 *     `mp_subscript`) + iteration (`tp_iter`/`tp_iternext`);
 *   - calling (`tp_call`);
 *   - the descriptor protocol (`tp_descr_get`/`tp_descr_set`);
 *   - the async protocol (`am_await`/`am_aiter`/`am_anext`), as a
 *     hermetic dispatch proof;
 *   - custom attribute access (`tp_getattro`/`tp_setattro`);
 *   - a `Py_TPFLAGS_HAVE_GC` type whose children live in **C-managed
 *     memory** and are surfaced/broken only through `tp_traverse` /
 *     `tp_clear`, allocated via `PyObject_GC_New` and enrolled with
 *     `PyObject_GC_Track`.
 *
 * ## Storage model
 *
 * WeavePy stores a readied type's instance state in the instance
 * `__dict__`, not in inline C struct fields (those are not yet stable
 * across the C boundary — a wave-3 concern). So, exactly like
 * `_ndarray.c`, each instance side-allocates its state in a malloc'd
 * `*Core` block and stashes a `PyLong`-encoded pointer to it in
 * `self.__dict__["_core_addr"]`; every slot reads the address back out.
 * This keeps the C-held child pointers (the whole point of the GC
 * proof) invisible to WeavePy's dict walker, so only `tp_traverse` can
 * reveal them.
 */

#define PY_SSIZE_T_CLEAN
#include <Python.h>

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

/* ================================================================== */
/* Shared helpers: stash/fetch a side-allocated core pointer in the   */
/* instance dict (the `_ndarray` pattern).                            */
/* ================================================================== */

static int set_core_addr(PyObject *self, void *core) {
    PyObject *addr = PyLong_FromLongLong((long long)(intptr_t)core);
    if (!addr) {
        return -1;
    }
    int rc = PyObject_SetAttrString(self, "_core_addr", addr);
    Py_DECREF(addr);
    return rc;
}

/* Fetch the core pointer; returns NULL *without* setting an error when
 * the attribute is missing (used from `tp_dealloc`, where the dict may
 * already be torn down). */
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
/* Vec2 — number protocol (nb_add / nb_subtract) + tp_richcompare.    */
/* ================================================================== */

typedef struct {
    long x;
    long y;
} Vec2Core;

static PyTypeObject Vec2_Type; /* forward */

static Vec2Core *vec2_core(PyObject *self) {
    void *p = core_addr_noerr(self);
    if (!p) {
        if (!PyErr_Occurred()) {
            PyErr_SetString(PyExc_RuntimeError, "Vec2: missing core");
        }
        return NULL;
    }
    return (Vec2Core *)p;
}

static int Vec2_init(PyObject *self, PyObject *args, PyObject *kwds) {
    (void)kwds;
    long x = 0, y = 0;
    if (!PyArg_ParseTuple(args, "ll", &x, &y)) {
        return -1;
    }
    Vec2Core *core = (Vec2Core *)malloc(sizeof(Vec2Core));
    if (!core) {
        PyErr_NoMemory();
        return -1;
    }
    core->x = x;
    core->y = y;
    if (set_core_addr(self, core) != 0) {
        free(core);
        return -1;
    }
    return 0;
}

/* Build a fresh Vec2 by *calling the readied type object through the C
 * call protocol* — the natural extension idiom
 * (`PyObject_CallFunction((PyObject *)&SomeType, ...)`) and the RFC
 * 0044 proof that a static type finalised by `PyType_Ready` is callable
 * from C: this drives `tp_new` (`PyType_GenericNew`) + `tp_init`
 * (`Vec2_init`). Note this fires *re-entrantly* from inside `nb_add` /
 * `nb_subtract`, which are themselves running under a VM dispatch — so
 * it also exercises nested `call_object` from a C slot. */
static PyObject *vec2_build(long x, long y) {
    return PyObject_CallFunction((PyObject *)&Vec2_Type, "ll", x, y);
}

static PyObject *Vec2_add(PyObject *a, PyObject *b) {
    Vec2Core *ca = vec2_core(a);
    Vec2Core *cb = vec2_core(b);
    if (!ca || !cb) {
        return NULL;
    }
    return vec2_build(ca->x + cb->x, ca->y + cb->y);
}

static PyObject *Vec2_sub(PyObject *a, PyObject *b) {
    Vec2Core *ca = vec2_core(a);
    Vec2Core *cb = vec2_core(b);
    if (!ca || !cb) {
        return NULL;
    }
    return vec2_build(ca->x - cb->x, ca->y - cb->y);
}

static PyObject *Vec2_richcompare(PyObject *a, PyObject *b, int op) {
    if (op != Py_EQ && op != Py_NE) {
        Py_RETURN_NOTIMPLEMENTED;
    }
    /* Only Vec2 == Vec2 is defined; anything else is NotImplemented so
     * the VM can fall back to identity. */
    if (Py_TYPE(b) != &Vec2_Type) {
        Py_RETURN_NOTIMPLEMENTED;
    }
    Vec2Core *ca = vec2_core(a);
    Vec2Core *cb = vec2_core(b);
    if (!ca || !cb) {
        return NULL;
    }
    int eq = (ca->x == cb->x) && (ca->y == cb->y);
    if (op == Py_NE) {
        eq = !eq;
    }
    if (eq) {
        Py_RETURN_TRUE;
    }
    Py_RETURN_FALSE;
}

static PyObject *Vec2_repr(PyObject *self) {
    Vec2Core *core = vec2_core(self);
    if (!core) {
        return NULL;
    }
    char buf[64];
    snprintf(buf, sizeof(buf), "Vec2(%ld, %ld)", core->x, core->y);
    return PyUnicode_FromString(buf);
}

static PyNumberMethods Vec2_as_number = {
    .nb_add = Vec2_add,
    .nb_subtract = Vec2_sub,
};

static PyTypeObject Vec2_Type = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "_stocktype.Vec2",
    .tp_basicsize = sizeof(PyObject),
    .tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE,
    .tp_doc = "2D integer vector (number + richcompare proof)",
    .tp_new = PyType_GenericNew,
    .tp_init = Vec2_init,
    .tp_repr = Vec2_repr,
    .tp_richcompare = Vec2_richcompare,
    .tp_as_number = &Vec2_as_number,
};

/* ================================================================== */
/* Seq — sequence (sq_length/sq_item) + mapping (mp_*) + iteration.   */
/* A read-only view over [0, n); iterates itself with a cursor.       */
/* ================================================================== */

typedef struct {
    Py_ssize_t n;
    Py_ssize_t cursor;
} SeqCore;

static PyTypeObject Seq_Type; /* forward */

static SeqCore *seq_core(PyObject *self) {
    void *p = core_addr_noerr(self);
    if (!p) {
        if (!PyErr_Occurred()) {
            PyErr_SetString(PyExc_RuntimeError, "Seq: missing core");
        }
        return NULL;
    }
    return (SeqCore *)p;
}

static int Seq_init(PyObject *self, PyObject *args, PyObject *kwds) {
    (void)kwds;
    Py_ssize_t n = 0;
    if (!PyArg_ParseTuple(args, "n", &n)) {
        return -1;
    }
    if (n < 0) {
        PyErr_SetString(PyExc_ValueError, "Seq: n must be >= 0");
        return -1;
    }
    SeqCore *core = (SeqCore *)malloc(sizeof(SeqCore));
    if (!core) {
        PyErr_NoMemory();
        return -1;
    }
    core->n = n;
    core->cursor = 0;
    if (set_core_addr(self, core) != 0) {
        free(core);
        return -1;
    }
    return 0;
}

static Py_ssize_t Seq_length(PyObject *self) {
    SeqCore *core = seq_core(self);
    if (!core) {
        return -1;
    }
    return core->n;
}

static PyObject *Seq_item(PyObject *self, Py_ssize_t i) {
    SeqCore *core = seq_core(self);
    if (!core) {
        return NULL;
    }
    if (i < 0 || i >= core->n) {
        PyErr_SetString(PyExc_IndexError, "Seq index out of range");
        return NULL;
    }
    return PyLong_FromSsize_t(i);
}

static PyObject *Seq_subscript(PyObject *self, PyObject *key) {
    if (!PyLong_Check(key)) {
        PyErr_SetString(PyExc_TypeError, "Seq indices must be integers");
        return NULL;
    }
    Py_ssize_t i = PyLong_AsSsize_t(key);
    if (i == -1 && PyErr_Occurred()) {
        return NULL;
    }
    return Seq_item(self, i);
}

static PyObject *Seq_iter(PyObject *self) {
    /* Self-iterator: reset the cursor and hand back a new reference. */
    SeqCore *core = seq_core(self);
    if (!core) {
        return NULL;
    }
    core->cursor = 0;
    Py_INCREF(self);
    return self;
}

static PyObject *Seq_iternext(PyObject *self) {
    SeqCore *core = seq_core(self);
    if (!core) {
        return NULL;
    }
    if (core->cursor >= core->n) {
        /* Exhausted: the canonical "raise StopIteration" is to return
         * NULL with no error set (CPython's `tp_iternext` protocol). */
        return NULL;
    }
    Py_ssize_t v = core->cursor;
    core->cursor += 1;
    return PyLong_FromSsize_t(v);
}

static PySequenceMethods Seq_as_sequence = {
    .sq_length = Seq_length,
    .sq_item = Seq_item,
};

static PyMappingMethods Seq_as_mapping = {
    .mp_length = Seq_length,
    .mp_subscript = Seq_subscript,
};

static PyTypeObject Seq_Type = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "_stocktype.Seq",
    .tp_basicsize = sizeof(PyObject),
    .tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE,
    .tp_doc = "read-only [0,n) view (sequence + mapping + iter proof)",
    .tp_new = PyType_GenericNew,
    .tp_init = Seq_init,
    .tp_iter = Seq_iter,
    .tp_iternext = Seq_iternext,
    .tp_as_sequence = &Seq_as_sequence,
    .tp_as_mapping = &Seq_as_mapping,
};

/* ================================================================== */
/* Adder — tp_call. `Adder(base)(x) == base + x`.                     */
/* ================================================================== */

static PyTypeObject Adder_Type; /* forward */

static int Adder_init(PyObject *self, PyObject *args, PyObject *kwds) {
    (void)kwds;
    long base = 0;
    if (!PyArg_ParseTuple(args, "l", &base)) {
        return -1;
    }
    long *core = (long *)malloc(sizeof(long));
    if (!core) {
        PyErr_NoMemory();
        return -1;
    }
    *core = base;
    if (set_core_addr(self, core) != 0) {
        free(core);
        return -1;
    }
    return 0;
}

static PyObject *Adder_call(PyObject *self, PyObject *args, PyObject *kwds) {
    (void)kwds;
    long *core = (long *)core_addr_noerr(self);
    if (!core) {
        if (!PyErr_Occurred()) {
            PyErr_SetString(PyExc_RuntimeError, "Adder: missing core");
        }
        return NULL;
    }
    long x = 0;
    if (!PyArg_ParseTuple(args, "l", &x)) {
        return NULL;
    }
    return PyLong_FromLong(*core + x);
}

static PyTypeObject Adder_Type = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "_stocktype.Adder",
    .tp_basicsize = sizeof(PyObject),
    .tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE,
    .tp_doc = "callable accumulator (tp_call proof)",
    .tp_new = PyType_GenericNew,
    .tp_init = Adder_init,
    .tp_call = Adder_call,
};

/* ================================================================== */
/* Const — descriptor protocol (tp_descr_get / tp_descr_set).         */
/* `__get__` returns the stored constant; `__set__` records the last  */
/* value it was handed in a module global so the test can observe it. */
/* ================================================================== */

static long g_last_descr_set = 0;

static PyTypeObject Const_Type; /* forward */

static int Const_init(PyObject *self, PyObject *args, PyObject *kwds) {
    (void)kwds;
    long val = 0;
    if (!PyArg_ParseTuple(args, "l", &val)) {
        return -1;
    }
    long *core = (long *)malloc(sizeof(long));
    if (!core) {
        PyErr_NoMemory();
        return -1;
    }
    *core = val;
    if (set_core_addr(self, core) != 0) {
        free(core);
        return -1;
    }
    return 0;
}

static PyObject *Const_descr_get(PyObject *self, PyObject *obj, PyObject *type) {
    (void)obj;
    (void)type;
    long *core = (long *)core_addr_noerr(self);
    if (!core) {
        if (!PyErr_Occurred()) {
            PyErr_SetString(PyExc_RuntimeError, "Const: missing core");
        }
        return NULL;
    }
    return PyLong_FromLong(*core);
}

static int Const_descr_set(PyObject *self, PyObject *obj, PyObject *value) {
    (void)self;
    (void)obj;
    if (value == NULL) {
        g_last_descr_set = -1; /* deletion */
        return 0;
    }
    long v = PyLong_AsLong(value);
    if (v == -1 && PyErr_Occurred()) {
        return -1;
    }
    g_last_descr_set = v;
    return 0;
}

static PyTypeObject Const_Type = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "_stocktype.Const",
    .tp_basicsize = sizeof(PyObject),
    .tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE,
    .tp_doc = "data descriptor (tp_descr_get / tp_descr_set proof)",
    .tp_new = PyType_GenericNew,
    .tp_init = Const_init,
    .tp_descr_get = Const_descr_get,
    .tp_descr_set = Const_descr_set,
};

/* ================================================================== */
/* Aw — async protocol (am_await / am_aiter / am_anext).              */
/*                                                                    */
/* A hermetic *dispatch* proof: no event loop is involved, so the     */
/* awaitables are stand-in integer sentinels. The point is only to    */
/* show that the VM's synthesised `__await__` / `__aiter__` /         */
/* `__anext__` dunders are harvested from `tp_as_async` and routed to */
/* the genuine `PyAsyncMethods` slots:                                */
/*   - `am_await`  → returns 11   (sentinel for "await dispatched");   */
/*   - `am_aiter`  → returns self (the async-iterator is itself);      */
/*   - `am_anext`  → returns 7    (sentinel) and bumps a counter.      */
/* ================================================================== */

static long g_aw_anext_calls = 0;

static PyObject *Aw_await(PyObject *self) {
    (void)self;
    return PyLong_FromLong(11);
}

static PyObject *Aw_aiter(PyObject *self) {
    Py_INCREF(self);
    return self;
}

static PyObject *Aw_anext(PyObject *self) {
    (void)self;
    g_aw_anext_calls += 1;
    return PyLong_FromLong(7);
}

static PyAsyncMethods Aw_as_async = {
    .am_await = Aw_await,
    .am_aiter = Aw_aiter,
    .am_anext = Aw_anext,
};

static PyTypeObject Aw_Type = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "_stocktype.Aw",
    .tp_basicsize = sizeof(PyObject),
    .tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE,
    .tp_doc = "async protocol (am_await/am_aiter/am_anext proof)",
    .tp_new = PyType_GenericNew,
    .tp_as_async = &Aw_as_async,
};

/* ================================================================== */
/* Proxy — custom attribute access (tp_getattro / tp_setattro).       */
/*                                                                    */
/* `getattr(p, "magic")` is synthesised in C (returns 4242); every    */
/* other name falls back to the *generic* instance-dict lookup        */
/* (`PyObject_GenericGetAttr`, which does NOT re-enter `getattro`, so  */
/* there is no recursion). `setattr` records the (name, value) in     */
/* module globals and then stores normally, so a written value round- */
/* trips back out through `getattro`.                                 */
/* ================================================================== */

static char g_last_setattr_name[64] = {0};
static long g_last_setattr_value = 0;

static PyObject *Proxy_getattro(PyObject *self, PyObject *name) {
    const char *n = PyUnicode_AsUTF8(name);
    if (n && strcmp(n, "magic") == 0) {
        return PyLong_FromLong(4242);
    }
    return PyObject_GenericGetAttr(self, name);
}

static int Proxy_setattro(PyObject *self, PyObject *name, PyObject *value) {
    const char *n = PyUnicode_AsUTF8(name);
    if (n) {
        strncpy(g_last_setattr_name, n, sizeof(g_last_setattr_name) - 1);
        g_last_setattr_name[sizeof(g_last_setattr_name) - 1] = '\0';
    }
    if (value && PyLong_Check(value)) {
        g_last_setattr_value = PyLong_AsLong(value);
    }
    return PyObject_GenericSetAttr(self, name, value);
}

static PyTypeObject Proxy_Type = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "_stocktype.Proxy",
    .tp_basicsize = sizeof(PyObject),
    .tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE,
    .tp_doc = "custom attribute access (tp_getattro/tp_setattro proof)",
    .tp_new = PyType_GenericNew,
    .tp_getattro = Proxy_getattro,
    .tp_setattro = Proxy_setattro,
};

/* ================================================================== */
/* Node — Py_TPFLAGS_HAVE_GC. Holds one child reference in C-managed   */
/* memory (the side core), invisible to WeavePy's dict walker, and    */
/* surfaces/breaks it only through tp_traverse / tp_clear.            */
/* ================================================================== */

typedef struct {
    PyObject *child; /* strong ref, or NULL */
} NodeCore;

/* Observability counters for the test. */
static long g_node_traverses = 0;
static long g_node_clears = 0;
static long g_node_live = 0;

static PyTypeObject Node_Type; /* forward */

static NodeCore *node_core_noerr(PyObject *self) {
    return (NodeCore *)core_addr_noerr(self);
}

/* tp_new: allocate via the GC allocator, initialise the core, then
 * enrol with the cycle collector. The classic stock GC-type pattern. */
static PyObject *Node_new(PyTypeObject *type, PyObject *args, PyObject *kwds) {
    (void)args;
    (void)kwds;
    PyObject *self = _PyObject_GC_New(type);
    if (!self) {
        return NULL;
    }
    NodeCore *core = (NodeCore *)calloc(1, sizeof(NodeCore));
    if (!core) {
        PyObject_GC_Del(self);
        PyErr_NoMemory();
        return NULL;
    }
    core->child = NULL;
    if (set_core_addr(self, core) != 0) {
        free(core);
        PyObject_GC_Del(self);
        return NULL;
    }
    g_node_live += 1;
    PyObject_GC_Track(self);
    return self;
}

static int Node_traverse(PyObject *self, visitproc visit, void *arg) {
    g_node_traverses += 1;
    NodeCore *core = node_core_noerr(self);
    if (core && core->child) {
        Py_VISIT(core->child);
    }
    return 0;
}

static int Node_clear(PyObject *self) {
    g_node_clears += 1;
    NodeCore *core = node_core_noerr(self);
    if (core) {
        Py_CLEAR(core->child);
    }
    return 0;
}

static void Node_dealloc(PyObject *self) {
    PyObject_GC_UnTrack(self);
    NodeCore *core = node_core_noerr(self);
    if (core) {
        Py_CLEAR(core->child);
        /* The core block itself is intentionally leaked (a small, fixed
         * allocation), exactly as `_ndarray.c` leaks its cores. */
    }
    g_node_live -= 1;
    PyObject_GC_Del(self);
}

/* Node.set_child(other) — replace the C-held child reference. */
static PyObject *Node_set_child(PyObject *self, PyObject *other) {
    NodeCore *core = node_core_noerr(self);
    if (!core) {
        PyErr_SetString(PyExc_RuntimeError, "Node: missing core");
        return NULL;
    }
    PyObject *old = core->child;
    if (other == Py_None) {
        core->child = NULL;
    } else {
        Py_INCREF(other);
        core->child = other;
    }
    Py_XDECREF(old);
    Py_RETURN_NONE;
}

/* Node.get_child() — return a new reference to the C-held child. */
static PyObject *Node_get_child(PyObject *self, PyObject *ignored) {
    (void)ignored;
    NodeCore *core = node_core_noerr(self);
    if (!core) {
        PyErr_SetString(PyExc_RuntimeError, "Node: missing core");
        return NULL;
    }
    if (!core->child) {
        Py_RETURN_NONE;
    }
    Py_INCREF(core->child);
    return core->child;
}

static PyMethodDef Node_methods[] = {
    {"set_child", Node_set_child, METH_O, "store a child reference in C memory"},
    {"get_child", Node_get_child, METH_NOARGS, "return the C-held child or None"},
    {NULL, NULL, 0, NULL},
};

static PyTypeObject Node_Type = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "_stocktype.Node",
    .tp_basicsize = sizeof(PyObject),
    .tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | Py_TPFLAGS_HAVE_GC,
    .tp_doc = "GC node holding a child in C memory (tp_traverse/tp_clear proof)",
    .tp_new = Node_new,
    .tp_dealloc = Node_dealloc,
    .tp_traverse = Node_traverse,
    .tp_clear = Node_clear,
    .tp_methods = Node_methods,
};

/* ================================================================== */
/* Module-level helpers for the GC proof.                             */
/* ================================================================== */

/* Return (traverses, clears, live_nodes) so the test can observe that
 * the collector reached the C `tp_traverse`/`tp_clear` slots and that
 * the nodes were reclaimed. */
static PyObject *st_gc_counters(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    return Py_BuildValue("(lll)", g_node_traverses, g_node_clears, g_node_live);
}

/* Return the last value handed to `Const.__set__`. */
static PyObject *st_last_descr_set(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    return PyLong_FromLong(g_last_descr_set);
}

/* make_vec2(x, y) — construct a Vec2 by calling the readied type object
 * through the C call protocol, at the *top level* of a C entry point
 * (the non-re-entrant counterpart to `vec2_build`'s in-slot use). */
static PyObject *st_make_vec2(PyObject *self, PyObject *args) {
    (void)self;
    long x = 0, y = 0;
    if (!PyArg_ParseTuple(args, "ll", &x, &y)) {
        return NULL;
    }
    return vec2_build(x, y);
}

/* Return (last_name, last_value) handed to `Proxy.__setattr__`. */
static PyObject *st_last_setattr(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    return Py_BuildValue("(sl)", g_last_setattr_name, g_last_setattr_value);
}

/* Number of times `Aw.__anext__` (am_anext) reached the C slot. */
static PyObject *st_aw_anext_calls(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    return PyLong_FromLong(g_aw_anext_calls);
}

static PyMethodDef st_methods[] = {
    {"gc_counters", st_gc_counters, METH_NOARGS, "(traverses, clears, live_nodes)"},
    {"last_descr_set", st_last_descr_set, METH_NOARGS, "last value passed to Const.__set__"},
    {"make_vec2", st_make_vec2, METH_VARARGS, "construct a Vec2 by calling the type from C"},
    {"last_setattr", st_last_setattr, METH_NOARGS, "(name, value) last passed to Proxy.__setattr__"},
    {"aw_anext_calls", st_aw_anext_calls, METH_NOARGS, "times Aw.__anext__ reached the C slot"},
    {NULL, NULL, 0, NULL},
};

static struct PyModuleDef st_module = {
    PyModuleDef_HEAD_INIT,
    "_stocktype",
    "RFC 0044 wave-2 stock-CPython-3.13 static-type-suite proof.",
    -1,
    st_methods,
    NULL,
    NULL,
    NULL,
    NULL,
};

/* Ready a type and add it to the module, decref'ing on the error path. */
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

PyMODINIT_FUNC PyInit__stocktype(void) {
    PyObject *m = PyModule_Create(&st_module);
    if (!m) {
        return NULL;
    }
    if (add_type(m, &Vec2_Type, "Vec2") < 0 || add_type(m, &Seq_Type, "Seq") < 0 ||
        add_type(m, &Adder_Type, "Adder") < 0 || add_type(m, &Const_Type, "Const") < 0 ||
        add_type(m, &Aw_Type, "Aw") < 0 || add_type(m, &Proxy_Type, "Proxy") < 0 ||
        add_type(m, &Node_Type, "Node") < 0) {
        Py_DECREF(m);
        return NULL;
    }
    PyModule_AddStringConstant(m, "ABI", "cp313");
    return m;
}
