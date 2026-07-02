/*
 * _stockdatetime — the RFC 0029 (wave 5) faithful-datetime ABI proof.
 *
 * Compiled against the host's **stock CPython 3.13 headers**, including
 * the real `datetime.h`. That means the compiler inlines CPython's
 * datetime accessor macros directly into this object file — exactly the
 * way Cython's `cimport datetime` does inside pandas' `tslibs`:
 *
 *   - `PyDateTime_IMPORT`             → PyCapsule_Import("datetime.datetime_CAPI")
 *   - `PyDateTime_GET_YEAR(o)`        → ((PyDateTime_Date*)o)->data[0..1] (big-endian)
 *   - `PyDateTime_DATE_GET_HOUR(o)`   → ((PyDateTime_DateTime*)o)->data[4]
 *   - `PyDateTime_DELTA_GET_DAYS(o)`  → ((PyDateTime_Delta*)o)->days
 *   - `PyDate_Check(o)`               → PyObject_TypeCheck(o, PyDateTimeAPI->DateType)
 *   - `PyDate_FromDate(y,m,d)`        → PyDateTimeAPI->Date_FromDate(...)
 *
 * When WeavePy loads this `.so`, those inlined reads land on WeavePy's
 * byte-faithful datetime instance bodies (RFC 0029), and the capsule's
 * type slots must report the CPython `tp_basicsize` (date 32, datetime
 * 48, time 40, timedelta 40). If the layout is right, a real Cython
 * datetime consumer "just works"; if not, it reads garbage / size-checks
 * fail (`datetime.datetime size changed`).
 */

#define PY_SSIZE_T_CLEAN
#include <Python.h>
#include <datetime.h>

/* ---- weavepy -> C read path (what pandas does to our datetimes) ---- */

static PyObject *sd_read_date(PyObject *self, PyObject *o) {
    (void)self;
    return Py_BuildValue("(iii)", PyDateTime_GET_YEAR(o), PyDateTime_GET_MONTH(o),
                         PyDateTime_GET_DAY(o));
}

static PyObject *sd_read_datetime(PyObject *self, PyObject *o) {
    (void)self;
    return Py_BuildValue("(iiiiiiii)", PyDateTime_GET_YEAR(o), PyDateTime_GET_MONTH(o),
                         PyDateTime_GET_DAY(o), PyDateTime_DATE_GET_HOUR(o),
                         PyDateTime_DATE_GET_MINUTE(o), PyDateTime_DATE_GET_SECOND(o),
                         PyDateTime_DATE_GET_MICROSECOND(o), PyDateTime_DATE_GET_FOLD(o));
}

static PyObject *sd_read_time(PyObject *self, PyObject *o) {
    (void)self;
    return Py_BuildValue("(iiiii)", PyDateTime_TIME_GET_HOUR(o), PyDateTime_TIME_GET_MINUTE(o),
                         PyDateTime_TIME_GET_SECOND(o), PyDateTime_TIME_GET_MICROSECOND(o),
                         PyDateTime_TIME_GET_FOLD(o));
}

static PyObject *sd_read_delta(PyObject *self, PyObject *o) {
    (void)self;
    return Py_BuildValue("(iii)", PyDateTime_DELTA_GET_DAYS(o), PyDateTime_DELTA_GET_SECONDS(o),
                         PyDateTime_DELTA_GET_MICROSECONDS(o));
}

/* `PyDateTime_DATE_GET_TZINFO` — naive returns Py_None; aware returns the
 * tzinfo. Returns 1 when the macro yields Py_None, else 0. */
static PyObject *sd_datetime_tz_is_none(PyObject *self, PyObject *o) {
    (void)self;
    PyObject *tz = PyDateTime_DATE_GET_TZINFO(o);
    return PyBool_FromLong(tz == Py_None);
}

/* ---- C construct-then-read path (the capsule constructors) ---- */

static PyObject *sd_construct_date(PyObject *self, PyObject *args) {
    (void)self;
    int y, mo, d;
    if (!PyArg_ParseTuple(args, "iii", &y, &mo, &d)) {
        return NULL;
    }
    PyObject *o = PyDate_FromDate(y, mo, d);
    if (!o) {
        return NULL;
    }
    PyObject *r = Py_BuildValue("(iii)", PyDateTime_GET_YEAR(o), PyDateTime_GET_MONTH(o),
                                PyDateTime_GET_DAY(o));
    Py_DECREF(o);
    return r;
}

static PyObject *sd_construct_datetime(PyObject *self, PyObject *args) {
    (void)self;
    int y, mo, d, hh, mi, ss, us;
    if (!PyArg_ParseTuple(args, "iiiiiii", &y, &mo, &d, &hh, &mi, &ss, &us)) {
        return NULL;
    }
    PyObject *o = PyDateTime_FromDateAndTime(y, mo, d, hh, mi, ss, us);
    if (!o) {
        return NULL;
    }
    PyObject *r = Py_BuildValue(
        "(iiiiiii)", PyDateTime_GET_YEAR(o), PyDateTime_GET_MONTH(o), PyDateTime_GET_DAY(o),
        PyDateTime_DATE_GET_HOUR(o), PyDateTime_DATE_GET_MINUTE(o), PyDateTime_DATE_GET_SECOND(o),
        PyDateTime_DATE_GET_MICROSECOND(o));
    Py_DECREF(o);
    return r;
}

static PyObject *sd_construct_delta(PyObject *self, PyObject *args) {
    (void)self;
    int days, secs, us;
    if (!PyArg_ParseTuple(args, "iii", &days, &secs, &us)) {
        return NULL;
    }
    PyObject *o = PyDelta_FromDSU(days, secs, us);
    if (!o) {
        return NULL;
    }
    PyObject *r = Py_BuildValue("(iii)", PyDateTime_DELTA_GET_DAYS(o),
                                PyDateTime_DELTA_GET_SECONDS(o), PyDateTime_DELTA_GET_MICROSECONDS(o));
    Py_DECREF(o);
    return r;
}

/* ---- type checks via the capsule type slots ---- */

static PyObject *sd_checks(PyObject *self, PyObject *o) {
    (void)self;
    return Py_BuildValue("(iiiii)", PyDate_Check(o), PyDate_CheckExact(o), PyDateTime_Check(o),
                         PyDateTime_CheckExact(o), PyDelta_Check(o));
}

/* ---- the __Pyx_ImportType size-check path ----
 * Reads `Py_TYPE`-style `tp_basicsize` straight off the class objects the
 * `datetime` module exports, exactly as Cython's generated
 * `__Pyx_ImportType("datetime", "datetime", sizeof(PyDateTime_DateTime))`
 * does before erroring with "datetime.datetime size changed". */
static PyObject *sd_module_basicsizes(PyObject *self, PyObject *args) {
    (void)self;
    (void)args;
    PyObject *m = PyImport_ImportModule("datetime");
    if (!m) {
        return NULL;
    }
    Py_ssize_t bs_date = -1, bs_dt = -1, bs_time = -1, bs_delta = -1;
    PyObject *c;
    if ((c = PyObject_GetAttrString(m, "date"))) {
        bs_date = ((PyTypeObject *)c)->tp_basicsize;
        Py_DECREF(c);
    }
    if ((c = PyObject_GetAttrString(m, "datetime"))) {
        bs_dt = ((PyTypeObject *)c)->tp_basicsize;
        Py_DECREF(c);
    }
    if ((c = PyObject_GetAttrString(m, "time"))) {
        bs_time = ((PyTypeObject *)c)->tp_basicsize;
        Py_DECREF(c);
    }
    if ((c = PyObject_GetAttrString(m, "timedelta"))) {
        bs_delta = ((PyTypeObject *)c)->tp_basicsize;
        Py_DECREF(c);
    }
    Py_DECREF(m);
    return Py_BuildValue("(nnnn)", bs_date, bs_dt, bs_time, bs_delta);
}

/* ---- module definition (static, single-phase) ---- */

static PyMethodDef sd_methods[] = {
    {"read_date", sd_read_date, METH_O, "read a date via PyDateTime_GET_* macros"},
    {"read_datetime", sd_read_datetime, METH_O, "read a datetime via inlined macros"},
    {"read_time", sd_read_time, METH_O, "read a time via PyDateTime_TIME_GET_* macros"},
    {"read_delta", sd_read_delta, METH_O, "read a timedelta via PyDateTime_DELTA_GET_* macros"},
    {"datetime_tz_is_none", sd_datetime_tz_is_none, METH_O, "PyDateTime_DATE_GET_TZINFO == Py_None"},
    {"construct_date", sd_construct_date, METH_VARARGS, "PyDate_FromDate then read back"},
    {"construct_datetime", sd_construct_datetime, METH_VARARGS, "PyDateTime_FromDateAndTime + read"},
    {"construct_delta", sd_construct_delta, METH_VARARGS, "PyDelta_FromDSU + read"},
    {"checks", sd_checks, METH_O, "PyDate_Check/PyDateTime_Check/PyDelta_Check via capsule"},
    {"module_basicsizes", sd_module_basicsizes, METH_NOARGS, "tp_basicsize size-check path"},
    {NULL, NULL, 0, NULL},
};

static struct PyModuleDef sd_module = {
    PyModuleDef_HEAD_INIT,
    "_stockdatetime",
    "RFC 0029 (wave 5) faithful-datetime stock-ABI proof extension.",
    -1,
    sd_methods,
    NULL,
    NULL,
    NULL,
    NULL,
};

PyMODINIT_FUNC PyInit__stockdatetime(void) {
    PyObject *m = PyModule_Create(&sd_module);
    if (!m) {
        return NULL;
    }
    /* The headline call: import the datetime C-API capsule. Expands to
     * `PyDateTimeAPI = PyCapsule_Import("datetime.datetime_CAPI", 0)`. */
    PyDateTime_IMPORT;
    PyModule_AddIntConstant(m, "imported", PyDateTimeAPI != NULL ? 1 : 0);
    if (PyDateTimeAPI != NULL) {
        PyModule_AddIntConstant(m, "cap_date_basicsize",
                                (long)PyDateTimeAPI->DateType->tp_basicsize);
        PyModule_AddIntConstant(m, "cap_datetime_basicsize",
                                (long)PyDateTimeAPI->DateTimeType->tp_basicsize);
        PyModule_AddIntConstant(m, "cap_time_basicsize",
                                (long)PyDateTimeAPI->TimeType->tp_basicsize);
        PyModule_AddIntConstant(m, "cap_delta_basicsize",
                                (long)PyDateTimeAPI->DeltaType->tp_basicsize);
    }
    return m;
}
