"""RFC 0069 WS5 — C-API recursion accounting.

numpy's ``test_pathological_self_containing`` builds ``l = [];
l.append(l)`` and lets array coercion recurse through ``PySequence_*``.
On CPython each hop charges ``Py_EnterRecursiveCall``; WeavePy's C-API
boundary used to charge nothing, so the C recursion rode the real C
stack into SIGBUS. This test compiles a minimal extension that mimics
numpy's recursive sequence discovery — one entry point charging
``Py_EnterRecursiveCall`` like a well-behaved extension, one charging
nothing at all (the abstract-boundary headroom guard's job) — and
asserts both raise ``RecursionError`` instead of faulting.
"""

import os
import shlex
import shutil
import subprocess
import sys
import sysconfig
import tempfile
import unittest

CC = sysconfig.get_config_var("CC") or ""
if not CC or shutil.which(shlex.split(CC)[0]) is None:
    raise unittest.SkipTest("no C compiler on PATH")

includepy = sysconfig.get_config_var("INCLUDEPY")
assert includepy, "INCLUDEPY unset"
assert os.path.isfile(os.path.join(includepy, "Python.h")), (
    "installed header tree missing Python.h in %r" % includepy
)

SOURCE = r"""
#define PY_SSIZE_T_CLEAN
#include <Python.h>

/* numpy-shaped recursive sequence discovery: walk item 0 of every
   sequence with no depth cap of its own. `charge` selects whether the
   walk passes Py_EnterRecursiveCall (a well-behaved extension) or
   relies entirely on the interpreter's C-API boundary guards. */
static Py_ssize_t
discover(PyObject *o, int charge)
{
    Py_ssize_t depth = 0;
    if (charge && Py_EnterRecursiveCall(" in discover") != 0)
        return -1;
    if (PySequence_Check(o) && PySequence_Size(o) > 0) {
        PyObject *item = PySequence_GetItem(o, 0);
        if (item == NULL) {
            if (charge)
                Py_LeaveRecursiveCall();
            return -1;
        }
        Py_ssize_t sub = discover(item, charge);
        Py_DECREF(item);
        if (sub < 0) {
            if (charge)
                Py_LeaveRecursiveCall();
            return -1;
        }
        depth = 1 + sub;
    }
    if (charge)
        Py_LeaveRecursiveCall();
    return depth;
}

static PyObject *
discover_depth(PyObject *self, PyObject *arg)
{
    Py_ssize_t d = discover(arg, 1);
    if (d < 0)
        return NULL;
    return PyLong_FromSsize_t(d);
}

static PyObject *
discover_depth_raw(PyObject *self, PyObject *arg)
{
    Py_ssize_t d = discover(arg, 0);
    if (d < 0)
        return NULL;
    return PyLong_FromSsize_t(d);
}

static PyMethodDef methods[] = {
    {"discover_depth", discover_depth, METH_O,
     "recursive nesting depth, charging Py_EnterRecursiveCall"},
    {"discover_depth_raw", discover_depth_raw, METH_O,
     "recursive nesting depth, charging nothing"},
    {NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT, "_weave_cext_recursion", NULL, -1, methods
};

PyMODINIT_FUNC
PyInit__weave_cext_recursion(void)
{
    return PyModule_Create(&module);
}
"""


def run(cmd):
    proc = subprocess.run(cmd, capture_output=True, text=True)
    assert proc.returncode == 0, "%r failed:\n%s\n%s" % (
        cmd,
        proc.stdout,
        proc.stderr,
    )


tmp = tempfile.mkdtemp(prefix="weavepy-cext-recursion-")
try:
    src = os.path.join(tmp, "_weave_cext_recursion.c")
    with open(src, "w") as f:
        f.write(SOURCE)

    cflags = sysconfig.get_config_var("CFLAGS") or ""
    ccshared = sysconfig.get_config_var("CCSHARED") or ""
    ldshared = sysconfig.get_config_var("LDSHARED") or ""
    ext_suffix = sysconfig.get_config_var("EXT_SUFFIX") or ".so"
    assert ldshared, "LDSHARED unset"

    obj = os.path.join(tmp, "_weave_cext_recursion.o")
    run(
        shlex.split(CC)
        + shlex.split(cflags)
        + shlex.split(ccshared)
        + ["-I", includepy, "-c", src, "-o", obj]
    )
    mod_path = os.path.join(tmp, "_weave_cext_recursion" + ext_suffix)
    run(shlex.split(ldshared) + [obj, "-o", mod_path])

    sys.path.insert(0, tmp)
    import _weave_cext_recursion as m

    # Bounded nesting resolves exactly.
    nested = [[[[1]]]]
    assert m.discover_depth(nested) == 4, m.discover_depth(nested)
    assert m.discover_depth_raw(nested) == 4, m.discover_depth_raw(nested)

    # The pathological self-containing list: unbounded C recursion.
    # Both flavors must raise RecursionError, never fault.
    l = []
    l.append(l)
    for fn in (m.discover_depth, m.discover_depth_raw):
        try:
            fn(l)
        except RecursionError:
            pass
        else:
            raise AssertionError("%s: expected RecursionError" % fn.__name__)

    # The guard must fully unwind: the same call works again afterwards
    # (no depth leak pinning the budget), and bounded input still works.
    for fn in (m.discover_depth, m.discover_depth_raw):
        try:
            fn(l)
        except RecursionError:
            pass
        else:
            raise AssertionError("%s: expected RecursionError (retry)" % fn.__name__)
    assert m.discover_depth(nested) == 4, "budget leaked after overflow"
    assert m.discover_depth_raw(nested) == 4, "headroom guard state leaked"

    print("capi-recursion ok")
finally:
    shutil.rmtree(tmp, ignore_errors=True)
