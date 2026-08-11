/*
 * demo.c — RFC 0062 WS2 source-build proof fixture.
 *
 * Deliberately exercises the *non-limited* API surface a typical
 * hand-written C extension uses, so a successful compile+import
 * proves the installed header tree and the ABI underneath it:
 *
 *   - a classic static PyTypeObject with tp_members/tp_methods
 *     (structmember.h — the legacy spelling real sdists include),
 *   - PyArg_ParseTupleAndKeywords with keyword arguments,
 *   - PEP 393 direct unicode access (PyUnicode_KIND/DATA/READ/WRITE —
 *     the markupsafe pattern; inline macros over PyASCIIObject),
 *   - the datetime C-API capsule (PyDateTime_IMPORT + the capsule
 *     constructors + the inlined PyDateTime_GET_* field macros).
 */
#define PY_SSIZE_T_CLEAN
#include <Python.h>
#include <structmember.h>
#include <datetime.h>

typedef struct {
    PyObject_HEAD
    Py_ssize_t count;
    PyObject *label;
} CounterObject;

static int
Counter_init(CounterObject *self, PyObject *args, PyObject *kwds)
{
    static char *kwlist[] = {"label", "start", NULL};
    PyObject *label = NULL;
    Py_ssize_t start = 0;
    if (!PyArg_ParseTupleAndKeywords(args, kwds, "|Un", kwlist, &label, &start))
        return -1;
    self->count = start;
    if (label == NULL)
        label = PyUnicode_FromString("counter");
    else
        Py_INCREF(label);
    Py_XSETREF(self->label, label);
    return 0;
}

static void
Counter_dealloc(CounterObject *self)
{
    Py_XDECREF(self->label);
    Py_TYPE(self)->tp_free((PyObject *)self);
}

static PyObject *
Counter_bump(CounterObject *self, PyObject *args)
{
    Py_ssize_t by = 1;
    if (!PyArg_ParseTuple(args, "|n", &by))
        return NULL;
    self->count += by;
    return PyLong_FromSsize_t(self->count);
}

static PyObject *
Counter_repr(CounterObject *self)
{
    return PyUnicode_FromFormat("<Counter %U at %zd>", self->label, self->count);
}

static PyMemberDef Counter_members[] = {
    {"count", T_PYSSIZET, offsetof(CounterObject, count), 0, "current count"},
    {"label", T_OBJECT_EX, offsetof(CounterObject, label), READONLY, "label"},
    {NULL}
};

static PyMethodDef Counter_methods[] = {
    {"bump", (PyCFunction)Counter_bump, METH_VARARGS, "increment"},
    {NULL}
};

static PyTypeObject CounterType = {
    PyVarObject_HEAD_INIT(NULL, 0)
    .tp_name = "weavepy_cext_demo._demo.Counter",
    .tp_basicsize = sizeof(CounterObject),
    .tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE,
    .tp_new = PyType_GenericNew,
    .tp_init = (initproc)Counter_init,
    .tp_dealloc = (destructor)Counter_dealloc,
    .tp_repr = (reprfunc)Counter_repr,
    .tp_members = Counter_members,
    .tp_methods = Counter_methods,
};

/* PEP 393 direct access — the markupsafe pattern. */
static PyObject *
shout(PyObject *self, PyObject *arg)
{
    if (!PyUnicode_Check(arg)) {
        PyErr_SetString(PyExc_TypeError, "expected str");
        return NULL;
    }
    Py_ssize_t n = PyUnicode_GET_LENGTH(arg);
    int kind = PyUnicode_KIND(arg);
    const void *data = PyUnicode_DATA(arg);
    PyObject *out = PyUnicode_New(n, PyUnicode_MAX_CHAR_VALUE(arg));
    if (out == NULL)
        return NULL;
    for (Py_ssize_t i = 0; i < n; i++) {
        Py_UCS4 ch = PyUnicode_READ(kind, data, i);
        if (ch >= 'a' && ch <= 'z')
            ch = ch - 'a' + 'A';
        PyUnicode_WRITE(PyUnicode_KIND(out), PyUnicode_DATA(out), i, ch);
    }
    return out;
}

/* Capsule constructor + inlined field-macro readback. */
static PyObject *
make_date(PyObject *self, PyObject *args)
{
    int y, m, d;
    if (!PyArg_ParseTuple(args, "iii", &y, &m, &d))
        return NULL;
    PyObject *date = PyDateTimeAPI->Date_FromDate(
        y, m, d, PyDateTimeAPI->DateType);
    if (date == NULL)
        return NULL;
    if (!PyDate_Check(date)) {
        Py_DECREF(date);
        PyErr_SetString(PyExc_AssertionError, "PyDate_Check failed");
        return NULL;
    }
    if (PyDateTime_GET_YEAR(date) != y || PyDateTime_GET_MONTH(date) != m ||
        PyDateTime_GET_DAY(date) != d) {
        Py_DECREF(date);
        PyErr_SetString(PyExc_AssertionError,
                        "PyDateTime_GET_* macro readback mismatch");
        return NULL;
    }
    return date;
}

static PyMethodDef demo_methods[] = {
    {"shout", shout, METH_O, "uppercase ASCII via PEP 393 access"},
    {"make_date", make_date, METH_VARARGS, "datetime C-API round-trip"},
    {NULL}
};

static struct PyModuleDef demo_module = {
    PyModuleDef_HEAD_INIT, "weavepy_cext_demo._demo",
    "RFC 0062 source-build proof extension", -1, demo_methods
};

PyMODINIT_FUNC
PyInit__demo(void)
{
    PyDateTime_IMPORT;
    if (PyDateTimeAPI == NULL)
        return NULL;
    if (PyType_Ready(&CounterType) < 0)
        return NULL;
    PyObject *m = PyModule_Create(&demo_module);
    if (m == NULL)
        return NULL;
    Py_INCREF(&CounterType);
    PyModule_AddObject(m, "Counter", (PyObject *)&CounterType);
    return m;
}
