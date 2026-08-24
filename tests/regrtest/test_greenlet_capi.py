"""RFC 0072 WS1 — the greenlet C-API capsule, end-to-end.

Compiles the checked-in ``tests/capi_ext/_greenletconsumer.c`` fixture
at test time (against the installed header tree plus the vendored
upstream ``greenlet/greenlet.h``) and drives it the way gevent's
compiled Cython modules drive the real capsule:

  - ``PyGreenlet_Import()`` resolves ``greenlet._C_API``
  - the imported class's ``tp_basicsize`` matches ``sizeof(PyGreenlet)``
    (the ``__Pyx_ImportType`` contract) and the capsule's slot-0 type is
    the *same object* as ``greenlet.greenlet``
  - ``PyGreenlet_New`` / ``Switch`` / ``Throw`` / ``GetCurrent`` /
    ``Get``/``SetParent`` and the ``MAIN``/``STARTED``/``ACTIVE``
    predicates work from C frames
  - a static C subclass with a cdef-shaped field at
    ``sizeof(PyGreenlet)`` constructs through the inherited ``tp_new``
    chain and round-trips direct C-field reads/writes
  - a greenlet's Python ``run`` can call back into C and switch from
    inside that C frame (the gevent hub shape), parking the native
    stack until control returns

The same fixture also runs as a Rust integration test
(``crates/weavepy-capi/tests/capi_greenletconsumer.rs``); this bundled
copy keeps the surface guarded by the regrtest sweep and CI's
default lanes.
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

CAPI_EXT = os.path.normpath(
    os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "capi_ext")
)
SRC = os.path.join(CAPI_EXT, "_greenletconsumer.c")
if not os.path.isfile(SRC):
    raise unittest.SkipTest("fixture source tree not present (installed layout)")
assert os.path.isfile(os.path.join(CAPI_EXT, "greenlet", "greenlet.h"))


def run(cmd):
    proc = subprocess.run(cmd, capture_output=True, text=True)
    assert proc.returncode == 0, "%r failed:\n%s\n%s" % (
        cmd,
        proc.stdout,
        proc.stderr,
    )


tmp = tempfile.mkdtemp(prefix="weavepy-greenlet-capi-")
try:
    cflags = sysconfig.get_config_var("CFLAGS") or ""
    ccshared = sysconfig.get_config_var("CCSHARED") or ""
    ldshared = sysconfig.get_config_var("LDSHARED") or ""
    ext_suffix = sysconfig.get_config_var("EXT_SUFFIX") or ".so"
    assert ldshared, "LDSHARED unset"

    obj = os.path.join(tmp, "_greenletconsumer.o")
    run(
        shlex.split(CC)
        + shlex.split(cflags)
        + shlex.split(ccshared)
        + ["-I", includepy, "-I", CAPI_EXT, "-c", SRC, "-o", obj]
    )
    mod_path = os.path.join(tmp, "_greenletconsumer" + ext_suffix)
    run(shlex.split(ldshared) + [obj, "-o", mod_path])

    sys.path.insert(0, tmp)
    import greenlet
    import _greenletconsumer as m

    # ---- capsule + type identity ----
    assert m.imported == 1
    assert m.header_sizeof == m.imported_basicsize, (
        m.header_sizeof,
        m.imported_basicsize,
    )
    assert m.capsule_basicsize == m.imported_basicsize
    assert m.types_match == 1
    assert m.sub_field_offset == m.header_sizeof

    # Capsule exception slots are the Python-visible classes.
    assert m.exc_greenlet_error() is greenlet.error
    assert m.exc_greenlet_exit() is greenlet.GreenletExit

    # ---- GetCurrent / predicates on main ----
    cur = m.get_current()
    assert cur is greenlet.getcurrent()
    assert m.current_is_main() == 1
    assert m.type_check(cur) == 1
    assert m.type_check(object()) == 0
    assert m.predicates(cur) == (1, 1, 1)  # main: MAIN, STARTED, ACTIVE
    assert m.get_parent(cur) is None  # main has no parent

    # ---- PyGreenlet_New + switch from a C frame ----
    seen = []

    def child(*args, **kwargs):
        seen.append((args, kwargs))
        return "child-done"

    g = m.new_greenlet(child)
    assert m.type_check(g) == 1
    assert m.predicates(g) == (0, 0, 0)
    assert m.get_parent(g) is greenlet.getcurrent()
    r = m.switch_to(g, (1, 2), {"k": 3})
    assert r == "child-done", r
    assert seen == [((1, 2), {"k": 3})], seen
    assert m.predicates(g) == (0, 1, 0)  # started, no longer active
    assert g.dead

    # ---- Throw from C ----
    caught = []

    def sleeper():
        try:
            greenlet.getcurrent().parent.switch("parked")
        except ValueError as e:
            caught.append(str(e))
            return "unwound"

    g2 = m.new_greenlet(sleeper)
    assert m.switch_to(g2, None, None) == "parked"
    assert m.throw_into(g2, ValueError, "boom") == "unwound"
    assert caught == ["boom"]
    assert g2.dead

    # Throw into an unstarted greenlet: it dies without running.
    g3 = m.new_greenlet(child)
    m.throw_into(g3, greenlet.GreenletExit, None)
    assert g3.dead

    # ---- SetParent ----
    order = []

    def outer(*args):
        # A dying child continues in its parent, passing its result.
        order.append("outer")

    def inner():
        order.append("inner")

    go = m.new_greenlet(outer)
    gi = m.new_greenlet(inner)
    m.set_parent(gi, go)  # inner's death continues in outer
    assert m.get_parent(gi) is go
    m.switch_to(gi, None, None)
    assert order == ["inner", "outer"], order

    # ---- the C subclass (gevent's TrackedRawGreenlet shape) ----
    sub_seen = []

    def sub_run():
        sub_seen.append(greenlet.getcurrent().get_tag())
        return "sub-done"

    s = m.SubGreenlet(sub_run)
    assert isinstance(s, greenlet.greenlet)
    assert m.type_check(s) == 1
    assert s.get_tag() is None
    s.set_tag({"loop": 1})  # direct C-field write at sizeof(PyGreenlet)
    assert s.get_tag() == {"loop": 1}
    assert s.switch() == "sub-done"
    assert sub_seen == [{"loop": 1}]
    assert s.dead
    assert s.get_tag() == {"loop": 1}  # field survives the run

    # ---- switch-under-C-frame from inside a running greenlet ----
    # The greenlet's Python run calls C (`switch_to`), and the switch
    # parks the whole native stack, C frame included — the gevent hub
    # shape.
    hops = []

    def pingpong():
        hops.append("in")
        got = m.switch_to(greenlet.getcurrent().parent, ("mid",), None)
        hops.append(got)
        return "pp-done"

    gp = m.new_greenlet(pingpong)
    # Single-argument switches unpack (upstream greenlet semantics).
    assert m.switch_to(gp, None, None) == "mid"
    assert m.switch_to(gp, ("back",), None) == "pp-done"
    assert hops == ["in", "back"], hops

    print("greenlet C-API capsule matrix ok")
finally:
    shutil.rmtree(tmp, ignore_errors=True)
