"""Code that compiles while machine code is generated off the main thread
gives the same results before and after its code is ready, and a fork
while compiles are in flight leaves a child that compiles and runs its
own, with no warning about forking a multi-threaded process (the JIT's
worker isn't one of the program's threads)."""

import os
import sys
import warnings


def make(k):
    # Distinct code objects, so each compiles on its own.
    src = f"""
def f{k}(n):
    d = {{}}
    for i in range(n):
        key = str((i * {k}) % 7)
        d[key] = d.get(key, 0) + 1
    return sorted(d.items())
"""
    ns = {}
    exec(src, ns)
    return ns[f"f{k}"]


def expected(k, n):
    d = {}
    for i in range(n):
        key = str((i * k) % 7)
        d[key] = d.get(key, 0) + 1
    return sorted(d.items())


funcs = [make(k) for k in range(1, 40)]

# Each function gets hot at a different point; check every result while
# compiles are submitted and published.
for rep in range(30):
    for k, f in enumerate(funcs, 1):
        assert f(200) == expected(k, 200), (k, rep)

if hasattr(os, "fork") and sys.platform != "win32":
    later = [make(k) for k in range(100, 130)]
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        # Start compiles, then fork with some likely still in flight.
        for f in later:
            for _ in range(40):
                f(50)
        pid = os.fork()
        if pid == 0:
            ok = True
            try:
                for rep in range(40):
                    for k, f in enumerate(later, 100):
                        if f(300) != expected(k, 300):
                            ok = False
                fresh = [make(k) for k in range(200, 210)]
                for rep in range(60):
                    for k, f in enumerate(fresh, 200):
                        if f(300) != expected(k, 300):
                            ok = False
            except BaseException:
                ok = False
            os._exit(0 if ok else 1)
        _, status = os.waitpid(pid, 0)
    assert os.waitstatus_to_exitcode(status) == 0, status
    threaded = [w for w in caught if "multi-threaded" in str(w.message)]
    assert not threaded, [str(w.message) for w in threaded]
    for rep in range(40):
        for k, f in enumerate(later, 100):
            assert f(300) == expected(k, 300)
print("ok")
