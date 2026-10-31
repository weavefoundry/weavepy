"""`gc.callbacks` around the collector's automatic collections.

CPython calls `gc.callbacks` with `start` and `stop` around every collection,
automatic ones included, each with a fresh `info` dict holding `generation`,
`collected` and `uncollectable`. Each case below reports what a callback saw
in a shape that doesn't depend on when the two collectors choose to run (how
many collections there are differs, as does the generation an old pass picks,
and WeavePy doesn't count an instance that holds nothing but scalars until it
could be part of a cycle, so the survivors here hold a list): that callbacks
fired, that the phases alternate, and what `info` held. The output must be CPython's in fresh interpreters with every tier on, with a low
compile threshold, with a low frame-JIT threshold, with the method tier off
and with the compilers off.

Running this file under CPython checks the expected output itself.
"""

import os
import subprocess
import sys
import unittest

PRELUDE = r'''
import gc

events = []


def record(phase, info):
    events.append((phase, dict(info)))


def report(tag, found=True):
    starts = [e for e in events if e[0] == "start"]
    stops = [e for e in events if e[0] == "stop"]
    print(
        tag,
        "fired", len(starts) > 0,
        "alternates", all(e[0] == ("start" if i % 2 == 0 else "stop") for i, e in enumerate(events)),
        "keys", sorted({k for e in events for k in e[1]}),
        "young", any(e[1]["generation"] == 0 for e in events),
        "known generations", all(e[1]["generation"] in (0, 1, 2) for e in events),
        "start counts zero", all(e[1]["collected"] == e[1]["uncollectable"] == 0 for e in starts),
        "same generation", all(a[1]["generation"] == b[1]["generation"] for a, b in zip(starts, stops)),
        "collected", found and sum(e[1]["collected"] for e in stops) > 0,
        "uncollectable", sum(e[1]["uncollectable"] for e in stops),
    )
    events.clear()


class N:
    def __init__(self):
        self.held = []


COUNT = 20000
'''

CASES = {}

CASES["survivors"] = r'''
gc.callbacks.append(record)
keep = [N() for _ in range(COUNT)]
report("instances")
del keep
gc.collect()
events.clear()
keep = []
for i in range(COUNT):
    keep.append([i])
report("lists")
del keep
gc.collect()
events.clear()
keep = []
for i in range(COUNT):
    keep.append({"i": i})
report("dicts")
del keep
gc.collect()
events.clear()
def g():
    yield 1
keep = []
for i in range(COUNT):
    it = g()
    next(it)
    keep.append(it)
report("suspended generators")
del keep, it
gc.collect()
events.clear()
def f(x):
    def h():
        return x
    return h
keep = [f(i) for i in range(COUNT)]
report("closures")
'''

CASES["cycles"] = r'''
gc.callbacks.append(record)
for i in range(COUNT):
    a = N()
    b = N()
    a.o = b
    b.o = a
report("cycles")
'''

CASES["asyncio"] = r'''
import asyncio
gc.callbacks.append(record)
async def leaf(i):
    await asyncio.sleep(0)
    return i
async def tree(d):
    if d == 0:
        return await leaf(d)
    r = await asyncio.gather(*[tree(d - 1) for _ in range(5)])
    return sum(r)
print(asyncio.run(tree(5)))
# (What this finds differs: CPython's reference counts free more of it.)
report("task tree", found=False)
'''

# What a collection's callback may do while the collection is on.
CASES["reentrancy"] = r'''
results = []
def collects(phase, info):
    results.append(gc.collect())
gc.callbacks.append(collects)
for i in range(COUNT):
    a = N()
    b = N()
    a.o = b
    b.o = a
print("collect in a callback of an automatic one", len(results) > 0, set(results))
gc.callbacks.remove(collects)
gc.collect()
results.clear()
gc.callbacks.append(collects)
gc.collect()
gc.callbacks.remove(collects)
print("collect in a callback of an explicit one", results)

depth = [0]
deepest = [0]
def allocates(phase, info):
    depth[0] += 1
    deepest[0] = max(deepest[0], depth[0])
    keep = [N() for _ in range(5000)]
    depth[0] -= 1
gc.callbacks.append(allocates)
keep = [N() for _ in range(COUNT)]
gc.callbacks.remove(allocates)
print("allocating callback nests", deepest[0])
del keep
gc.collect()

fired = []
def first(phase, info):
    fired.append("first")
    if second in gc.callbacks:
        gc.callbacks.remove(second)
def second(phase, info):
    fired.append("second")
gc.callbacks.extend([first, second])
keep = [N() for _ in range(COUNT)]
print("snapshot of the list", fired[:2])
gc.callbacks.remove(first)
del keep
gc.collect()

hooked = []
sys_hook = __import__("sys").unraisablehook
__import__("sys").unraisablehook = lambda u: hooked.append(type(u.exc_value).__name__)
def fails(phase, info):
    raise ValueError("no")
gc.callbacks.append(fails)
gc.callbacks.append(record)
keep = [N() for _ in range(COUNT)]
print("a failing callback goes on", hooked[:1], len(events) > 0)
gc.callbacks.remove(fails)
gc.callbacks.remove(record)
__import__("sys").unraisablehook = sys_hook
'''

# Finalizers of garbage an automatic collection finds run between its phases.
CASES["finalizers"] = r'''
log = []
class F:
    def __del__(self):
        log.append("del")
def note(phase, info):
    log.append(phase)
gc.callbacks.append(note)
for i in range(COUNT):
    a = F()
    b = F()
    a.o = b
    b.o = a
# (Only the shape: the first collection that found this kind of garbage.)
first_del = log.index("del")
print(log[first_del - 1], log.count("start") == log.count("stop") or log[-1] == "start")
stop_after = log.index("stop", first_del)
print("del" in log[first_del:stop_after], log[stop_after:stop_after + 1])
'''

CASES["switched_off"] = r'''
gc.callbacks.append(record)
gc.disable()
keep = [N() for _ in range(COUNT)]
print("disabled", len(events))
gc.enable()
del keep
gc.collect()
events.clear()
gc.set_threshold(0)
keep = [N() for _ in range(COUNT)]
print("threshold zero", len(events))
gc.set_threshold(2000, 10, 10)
del keep
gc.collect()
events.clear()
gc.callbacks.remove(record)
keep = [N() for _ in range(COUNT)]
print("removed", len(events))
'''

# An uncollectable object's count and `gc.garbage` after an automatic pass.
CASES["saved_garbage"] = r'''
gc.collect()
gc.set_debug(gc.DEBUG_SAVEALL)
gc.callbacks.append(record)
for i in range(COUNT):
    a = N()
    a.o = a
del a
stops = [e for e in events if e[0] == "stop"]
print("saved", len(gc.garbage) > 0, isinstance(gc.garbage[0], N))
print("uncollectable", sum(e[1]["uncollectable"] for e in stops))
print("collected", sum(e[1]["collected"] for e in stops) > 0)
gc.callbacks.remove(record)
gc.set_debug(0)
gc.garbage.clear()
'''

ENVS = [
    {},
    {"WEAVEPY_JIT_THRESHOLD": "2"},
    {"WEAVEPY_FRAME_JIT_HOT": "50", "WEAVEPY_JIT_SYNC": "1"},
    {"WEAVEPY_TIER2": "0"},
    {"WEAVEPY_JIT": "0"},
]

# CPython 3.14's output for each case.
EXPECTED = {
    'survivors': "instances fired True alternates True keys ['collected', 'generation', 'uncollectable'] young True known generations True start counts zero True same generation True collected False uncollectable 0\nlists fired True alternates True keys ['collected', 'generation', 'uncollectable'] young True known generations True start counts zero True same generation True collected False uncollectable 0\ndicts fired True alternates True keys ['collected', 'generation', 'uncollectable'] young True known generations True start counts zero True same generation True collected False uncollectable 0\nsuspended generators fired True alternates True keys ['collected', 'generation', 'uncollectable'] young True known generations True start counts zero True same generation True collected False uncollectable 0\nclosures fired True alternates True keys ['collected', 'generation', 'uncollectable'] young True known generations True start counts zero True same generation True collected False uncollectable 0\n",
    'cycles': "cycles fired True alternates True keys ['collected', 'generation', 'uncollectable'] young True known generations True start counts zero True same generation True collected True uncollectable 0\n",
    'asyncio': "0\ntask tree fired True alternates True keys ['collected', 'generation', 'uncollectable'] young True known generations True start counts zero True same generation True collected False uncollectable 0\n",
    'reentrancy': "collect in a callback of an automatic one True {0}\ncollect in a callback of an explicit one [0, 0]\nallocating callback nests 1\nsnapshot of the list ['first', 'first']\na failing callback goes on ['ValueError'] True\n",
    'finalizers': "start True\nTrue ['stop']\n",
    'switched_off': 'disabled 0\nthreshold zero 0\nremoved 0\n',
    'saved_garbage': 'saved True True\nuncollectable 0\ncollected True\n',
}


def source(name):
    return PRELUDE + CASES[name]


def run_case(name, extra=None):
    env = dict(os.environ, **(extra or {}))
    result = subprocess.run(
        [sys.executable, "-c", source(name)],
        capture_output=True,
        text=True,
        env=env,
        timeout=300,
    )
    return result.returncode, result.stdout, result.stderr


class AutomaticCollectionCallbackTests(unittest.TestCase):
    pass


def _make(name):
    def test(self):
        for extra in ENVS:
            with self.subTest(env=extra):
                code, out, err = run_case(name, extra)
                self.assertEqual(code, 0, err[-3000:])
                self.assertEqual(out, EXPECTED[name])

    return test


for _name in CASES:
    setattr(AutomaticCollectionCallbackTests, "test_" + _name, _make(_name))


if __name__ == "__main__":
    unittest.main()
