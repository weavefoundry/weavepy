"""RFC 0062 WS2 — compile a C extension against the installed headers.

The daily-driver contract for source builds: `sysconfig` names a real
compiler (`CC`), real flags (`CFLAGS`/`CCSHARED`/`LDSHARED`), and an
`INCLUDEPY` directory that actually contains the CPython 3.13 header
tree (materialized by the stdlib tree writer). This test drives that
surface exactly the way setuptools' ccompiler does — compile, link,
import — without needing setuptools itself, so it runs offline and
deterministically. The pip/PEP 517 flavor of the same path is proven
by the ecosystem lane's `--no-binary` rows (markupsafe, wrapt) and by
`weavepy-dist check`.
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
assert os.path.isfile(os.path.join(includepy, "pyconfig.h")), (
    "installed header tree missing pyconfig.h in %r" % includepy
)
# Spot-check the satellite headers real sdists include directly.
for name in ("structmember.h", "datetime.h", "cpython/unicodeobject.h"):
    assert os.path.isfile(os.path.join(includepy, name)), (
        "installed header tree missing %r" % name
    )

SOURCE = r"""
#define PY_SSIZE_T_CLEAN
#include <Python.h>
#include <structmember.h>

/* PEP 393 direct access -- the markupsafe pattern. */
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

static PyObject *
add(PyObject *self, PyObject *args)
{
    Py_ssize_t a, b;
    if (!PyArg_ParseTuple(args, "nn", &a, &b))
        return NULL;
    return PyLong_FromSsize_t(a + b);
}

static PyMethodDef methods[] = {
    {"shout", shout, METH_O, "uppercase ASCII via PEP 393 access"},
    {"add", add, METH_VARARGS, "add two ints"},
    {NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT, "_weave_cext_smoke", NULL, -1, methods
};

PyMODINIT_FUNC
PyInit__weave_cext_smoke(void)
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


tmp = tempfile.mkdtemp(prefix="weavepy-cext-smoke-")
try:
    src = os.path.join(tmp, "_weave_cext_smoke.c")
    with open(src, "w") as f:
        f.write(SOURCE)

    cflags = sysconfig.get_config_var("CFLAGS") or ""
    ccshared = sysconfig.get_config_var("CCSHARED") or ""
    ldshared = sysconfig.get_config_var("LDSHARED") or ""
    ext_suffix = sysconfig.get_config_var("EXT_SUFFIX") or ".so"
    assert ldshared, "LDSHARED unset"

    obj = os.path.join(tmp, "_weave_cext_smoke.o")
    run(
        shlex.split(CC)
        + shlex.split(cflags)
        + shlex.split(ccshared)
        + ["-I", includepy, "-c", src, "-o", obj]
    )
    mod_path = os.path.join(tmp, "_weave_cext_smoke" + ext_suffix)
    run(shlex.split(ldshared) + [obj, "-o", mod_path])

    sys.path.insert(0, tmp)
    import _weave_cext_smoke

    assert _weave_cext_smoke.add(20, 22) == 42
    assert _weave_cext_smoke.shout("hello weavepy!") == "HELLO WEAVEPY!"
    assert _weave_cext_smoke.shout("héllo") == "HéLLO"
    assert _weave_cext_smoke.__file__ == mod_path
    print("cext-build ok")
finally:
    shutil.rmtree(tmp, ignore_errors=True)
