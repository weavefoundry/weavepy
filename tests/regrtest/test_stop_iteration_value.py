"""A `StopIteration`'s value on its way through native iterators.

`map`, `filter`, `zip`, `enumerate` and most of `itertools` call their
source's `tp_iternext` directly, so a `StopIteration` a source or a callback
raises (a finished generator's return value, say) reaches `next()` and
`yield from` unchanged. `chain`, `cycle`, `tee`, `batched`, `zip_longest`,
`groupby`'s source and strict `map` and `zip` go through `PyIter_Next` or
clear it, and consumers that drain an iterator stop quietly. Each case prints
what one of these does, and repeats it in a loop long enough for every tier to
compile it, printing each distinct result (so a tier that differs shows). The
output must be CPython's in fresh interpreters with every tier on, with a low
compile threshold, with a low frame-JIT threshold, with the method tier off
and with the compilers off.

Running this file under CPython checks the expected output itself.
"""

import os
import subprocess
import sys
import unittest

PRELUDE = r'''
import itertools as it

N = 300


def gen(v=42, n=0):
    for i in range(n):
        yield i
    return v


class SI:
    def __iter__(self):
        return self

    def __next__(self):
        raise StopIteration(7)


class MyStop(StopIteration):
    pass


class MyMap(map):
    pass


class MyFilter(filter):
    pass


class MyEnum(enumerate):
    pass


class MyZip(zip):
    pass


def raiser(*a):
    raise StopIteration(9)


def raises_mine(x):
    raise MyStop(3, 4)


def fn(x):
    return x


def fn2(a, b):
    return a


def tr(x):
    return True


def result(f):
    try:
        r = f()
    except StopIteration as e:
        return ("stop", type(e).__name__, e.value, e.args)
    except Exception as e:
        return ("raised", type(e).__name__, str(e))
    return ("returned", r)


def show(f):
    seen = []
    for _ in range(N):
        r = repr(result(f))
        if r not in seen:
            seen.append(r)
    print(seen)
'''

CASES = {}

CASES["generators"] = r'''
show(lambda: next(gen()))
show(lambda: gen().__next__())
show(lambda: gen().send(None))
show(lambda: next(gen(None)))
show(lambda: next(gen(), "default"))
show(lambda: next(iter(gen())))
'''

CASES["map"] = r'''
show(lambda: next(map(next, [gen()])))
show(lambda: map(next, [gen()]).__next__())
show(lambda: next(map(fn, gen())))
show(lambda: map(fn, gen()).__next__())
show(lambda: next(map(fn, SI())))
show(lambda: next(map(raiser, [1])))
show(lambda: next(map(fn2, gen(), gen(43))))
show(lambda: next(map(fn2, [1], gen(43))))
show(lambda: next(map(fn2, gen(), gen(43), strict=True)))
show(lambda: next(map(fn, gen(), strict=True)))
show(lambda: next(map(fn2, [], gen(43), strict=True)))
show(lambda: next(map(fn2, [1], gen(43), strict=True)))
show(lambda: next(map(gen().send, [None])))
show(lambda: next(map(lambda _: gen().__next__(), [1])))
show(lambda: next(map(next, [iter([])])))
show(lambda: next(map(next, [map(next, [gen()])])))
show(lambda: next(map(raises_mine, [1])))
show(lambda: next(map(next, [gen(None)])))
show(lambda: next(map(next, [gen((1, 2))])))
show(lambda: next(MyMap(next, [gen()])))
show(lambda: next(map(next, [gen()]), "default"))
show(lambda: next(map(fn, gen()), "default"))
'''

CASES["filter_zip_enumerate"] = r'''
show(lambda: next(filter(None, gen())))
show(lambda: next(filter(tr, gen())))
show(lambda: filter(tr, gen()).__next__())
show(lambda: next(filter(raiser, [1])))
show(lambda: next(filter(tr, map(next, [gen()]))))
show(lambda: next(MyFilter(tr, gen())))
show(lambda: next(zip(gen())))
show(lambda: zip(gen()).__next__())
show(lambda: next(zip([1], gen())))
show(lambda: next(zip(gen(), [1])))
show(lambda: next(zip(SI())))
show(lambda: next(zip(gen(), [], strict=True)))
show(lambda: next(zip(gen(), strict=True)))
show(lambda: next(zip([1], gen(), strict=True)))
show(lambda: next(MyZip(gen())))
show(lambda: next(enumerate(gen())))
show(lambda: enumerate(gen()).__next__())
show(lambda: next(enumerate(SI())))
show(lambda: next(enumerate(map(next, [gen()]))))
show(lambda: next(MyEnum(gen())))
show(lambda: next(reversed([])))
show(lambda: next(iter(raiser, 1)))
'''

CASES["itertools_keep"] = r'''
show(lambda: next(it.islice(gen(), 1)))
show(lambda: next(it.islice(gen(), 1, None)))
show(lambda: it.islice(gen(), 1).__next__())
show(lambda: next(it.islice(map(next, [gen()]), 2)))
show(lambda: next(it.takewhile(tr, gen())))
show(lambda: next(it.takewhile(raiser, [1])))
show(lambda: next(it.dropwhile(tr, gen())))
show(lambda: next(it.dropwhile(raiser, [1])))
show(lambda: next(it.filterfalse(tr, gen())))
show(lambda: next(it.filterfalse(raiser, [1])))
show(lambda: next(it.starmap(fn, gen())))
show(lambda: next(it.starmap(raiser, [(1,)])))
show(lambda: next(it.starmap(fn, map(next, [gen()]))))
show(lambda: next(it.accumulate(gen())))
show(lambda: next(it.islice(it.accumulate([1, 2], raiser), 1, None)))
show(lambda: next(it.compress(gen(), [1])))
show(lambda: next(it.compress([1], gen())))
show(lambda: next(it.pairwise(gen())))
show(lambda: next(it.pairwise(gen(42, 1))))
show(lambda: next(it.groupby([1], raiser)))
def twice(make):
    i = make()
    return result(lambda: next(i)), result(lambda: next(i))
show(lambda: twice(lambda: it.pairwise(gen())))
show(lambda: twice(lambda: it.pairwise(gen(42, 1))))
show(lambda: twice(lambda: it.islice(gen(), 1)))
show(lambda: twice(lambda: it.islice(gen(), 1, None)))
show(lambda: twice(lambda: it.takewhile(tr, gen())))
show(lambda: twice(lambda: map(next, [gen()])))
show(lambda: twice(lambda: map(fn, gen())))
show(lambda: twice(lambda: zip(gen())))
show(lambda: twice(lambda: enumerate(gen())))
show(lambda: twice(gen))
'''

CASES["itertools_clear"] = r'''
show(lambda: next(it.chain(gen())))
show(lambda: next(it.chain.from_iterable([gen()])))
show(lambda: next(it.chain.from_iterable(map(next, [gen()]))))
show(lambda: next(it.zip_longest(gen())))
show(lambda: next(it.batched(gen(), 2)))
show(lambda: next(it.cycle(gen())))
show(lambda: next(it.tee(gen())[0]))
show(lambda: next(it.groupby(gen())))
'''

CASES["consumers_stop"] = r'''
show(lambda: list(map(next, [gen()])))
show(lambda: list(map(next, [gen(42, 1), gen()])))
show(lambda: list(map(fn, gen(42, 2))))
show(lambda: tuple(map(next, [gen()])))
show(lambda: set(map(next, [gen()])))
show(lambda: dict(map(next, [gen()])))
show(lambda: sorted(map(next, [gen()])))
show(lambda: sum(map(next, [gen()])))
show(lambda: any(map(next, [gen()])))
show(lambda: all(map(next, [gen()])))
show(lambda: min(map(next, [gen()]), default=5))
show(lambda: "".join(map(next, [gen()])))
show(lambda: list(filter(raiser, [1])))
show(lambda: list(filter(tr, gen(42, 2))))
show(lambda: list(zip(map(next, [gen()]))))
show(lambda: list(zip(gen(42, 2))))
show(lambda: list(zip(gen(42, 2), gen(43, 2))))
show(lambda: list(zip(gen(42, 2), gen(43, 2), strict=True)))
show(lambda: list(enumerate(map(next, [gen()]))))
show(lambda: list(enumerate(gen(42, 2))))
show(lambda: list(it.islice(gen(42, 2), 5)))
show(lambda: list(it.chain(gen(42, 2), gen(43, 1))))
show(lambda: list(it.pairwise(gen(42, 3))))
show(lambda: list(it.takewhile(tr, gen(42, 2))))
show(lambda: list(it.dropwhile(tr, gen(42, 2))))
show(lambda: list(it.filterfalse(tr, gen(42, 2))))
show(lambda: list(it.starmap(fn, map(next, [gen()]))))
show(lambda: list(it.accumulate(gen(42, 3))))
show(lambda: list(it.compress(gen(42, 2), gen(43, 2))))
show(lambda: [x for x in map(next, [gen()])])
def for_loop():
    out = []
    for x in map(fn, gen(42, 2)):
        out.append(x)
    for x in enumerate(gen(42, 1)):
        out.append(x)
    for x in zip(gen(42, 1), gen(43, 1)):
        out.append(x)
    for x in filter(tr, gen(42, 1)):
        out.append(x)
    return out
show(for_loop)
'''

CASES["generator_wrappers"] = r'''
show(lambda: next(x for x in map(next, [gen()])))
show(lambda: next(x for x in gen()))
show(lambda: next(map(fn, (x for x in gen()))))
show(lambda: next(next(gen()) for x in [1]))
show(lambda: next(map(fn, (next(gen()) for x in [1]))))
def raising():
    next(gen())
    yield
show(lambda: next(raising()))
show(lambda: next(map(fn, raising())))
def inside():
    yield from map(next, [gen()])
show(lambda: next(inside()))
def delegate(x):
    r = yield from x
    return r
show(lambda: next(delegate(map(next, [gen()]))))
show(lambda: next(delegate(map(fn, gen()))))
show(lambda: next(delegate(filter(tr, gen()))))
show(lambda: next(delegate(zip(gen()))))
show(lambda: next(delegate(enumerate(gen()))))
show(lambda: next(delegate(it.islice(gen(), 1))))
show(lambda: next(delegate(it.chain(gen()))))
show(lambda: next(delegate(gen())))
show(lambda: next(delegate(SI())))
'''

# With a callback that runs an inner iteration, the value is the source's.
CASES["nested_iterations"] = r'''
def drains(x):
    list(map(fn, gen(99)))
    return False
show(lambda: next(filter(drains, [1, 2])))
show(lambda: next(filter(drains, gen(42, 1))))
def inner_next(x):
    try:
        next(map(fn, gen(99)))
    except StopIteration as e:
        return e.value
show(lambda: list(map(inner_next, [1])))
show(lambda: next(map(inner_next, [1])))
show(lambda: next(map(inner_next, gen(42))))
class Finalized:
    def __del__(self):
        log.append("del")
log = []
def value_goes():
    list(map(fn, gen(Finalized())))
    log.append("after")
    r = list(log)
    log.clear()
    return r
show(value_goes)
def value_kept():
    try:
        next(map(fn, gen(Finalized())))
    except StopIteration as e:
        log.append(type(e.value).__name__)
    log.append("after")
    r = list(log)
    log.clear()
    return r
show(value_kept)
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
    'generators': '["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', None, ())"]\n["(\'returned\', \'default\')"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n',
    'map': '["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 7, (7,))"]\n["(\'stop\', \'StopIteration\', 9, (9,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 43, (43,))"]\n["(\'stop\', \'StopIteration\', None, ())"]\n["(\'stop\', \'StopIteration\', None, ())"]\n["(\'stop\', \'StopIteration\', None, ())"]\n["(\'raised\', \'ValueError\', \'map() argument 2 is shorter than argument 1\')"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', None, ())"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'MyStop\', 3, (3, 4))"]\n["(\'stop\', \'StopIteration\', None, ())"]\n["(\'stop\', \'StopIteration\', (1, 2), ((1, 2),))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'returned\', \'default\')"]\n["(\'returned\', \'default\')"]\n',
    'filter_zip_enumerate': '["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 9, (9,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 7, (7,))"]\n["(\'stop\', \'StopIteration\', None, ())"]\n["(\'stop\', \'StopIteration\', None, ())"]\n["(\'raised\', \'ValueError\', \'zip() argument 2 is shorter than argument 1\')"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 7, (7,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', None, ())"]\n["(\'stop\', \'StopIteration\', None, ())"]\n',
    'itertools_keep': '["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 9, (9,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 9, (9,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 9, (9,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 9, (9,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 9, (9,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 9, (9,))"]\n["(\'returned\', ((\'stop\', \'StopIteration\', 42, (42,)), (\'stop\', \'StopIteration\', None, ())))"]\n["(\'returned\', ((\'stop\', \'StopIteration\', 42, (42,)), (\'stop\', \'StopIteration\', None, ())))"]\n["(\'returned\', ((\'stop\', \'StopIteration\', 42, (42,)), (\'stop\', \'StopIteration\', None, ())))"]\n["(\'returned\', ((\'stop\', \'StopIteration\', 42, (42,)), (\'stop\', \'StopIteration\', None, ())))"]\n["(\'returned\', ((\'stop\', \'StopIteration\', 42, (42,)), (\'stop\', \'StopIteration\', None, ())))"]\n["(\'returned\', ((\'stop\', \'StopIteration\', 42, (42,)), (\'stop\', \'StopIteration\', None, ())))"]\n["(\'returned\', ((\'stop\', \'StopIteration\', 42, (42,)), (\'stop\', \'StopIteration\', None, ())))"]\n["(\'returned\', ((\'stop\', \'StopIteration\', 42, (42,)), (\'stop\', \'StopIteration\', None, ())))"]\n["(\'returned\', ((\'stop\', \'StopIteration\', 42, (42,)), (\'stop\', \'StopIteration\', None, ())))"]\n["(\'returned\', ((\'stop\', \'StopIteration\', 42, (42,)), (\'stop\', \'StopIteration\', None, ())))"]\n',
    'itertools_clear': '["(\'stop\', \'StopIteration\', None, ())"]\n["(\'stop\', \'StopIteration\', None, ())"]\n["(\'stop\', \'StopIteration\', None, ())"]\n["(\'stop\', \'StopIteration\', None, ())"]\n["(\'stop\', \'StopIteration\', None, ())"]\n["(\'stop\', \'StopIteration\', None, ())"]\n["(\'stop\', \'StopIteration\', None, ())"]\n["(\'stop\', \'StopIteration\', None, ())"]\n',
    'consumers_stop': '["(\'returned\', [])"]\n["(\'returned\', [0])"]\n["(\'returned\', [0, 1])"]\n["(\'returned\', ())"]\n["(\'returned\', set())"]\n["(\'returned\', {})"]\n["(\'returned\', [])"]\n["(\'returned\', 0)"]\n["(\'returned\', False)"]\n["(\'returned\', True)"]\n["(\'returned\', 5)"]\n["(\'returned\', \'\')"]\n["(\'returned\', [])"]\n["(\'returned\', [0, 1])"]\n["(\'returned\', [])"]\n["(\'returned\', [(0,), (1,)])"]\n["(\'returned\', [(0, 0), (1, 1)])"]\n["(\'returned\', [(0, 0), (1, 1)])"]\n["(\'returned\', [])"]\n["(\'returned\', [(0, 0), (1, 1)])"]\n["(\'returned\', [0, 1])"]\n["(\'returned\', [0, 1, 0])"]\n["(\'returned\', [(0, 1), (1, 2)])"]\n["(\'returned\', [0, 1])"]\n["(\'returned\', [])"]\n["(\'returned\', [])"]\n["(\'returned\', [])"]\n["(\'returned\', [0, 1, 3])"]\n["(\'returned\', [1])"]\n["(\'returned\', [])"]\n["(\'returned\', [0, 1, (0, 0), (0, 0), 0])"]\n',
    'generator_wrappers': '["(\'stop\', \'StopIteration\', None, ())"]\n["(\'stop\', \'StopIteration\', None, ())"]\n["(\'stop\', \'StopIteration\', None, ())"]\n["(\'raised\', \'RuntimeError\', \'generator raised StopIteration\')"]\n["(\'raised\', \'RuntimeError\', \'generator raised StopIteration\')"]\n["(\'raised\', \'RuntimeError\', \'generator raised StopIteration\')"]\n["(\'raised\', \'RuntimeError\', \'generator raised StopIteration\')"]\n["(\'stop\', \'StopIteration\', None, ())"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', None, ())"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'stop\', \'StopIteration\', 7, (7,))"]\n',
    'nested_iterations': '["(\'stop\', \'StopIteration\', None, ())"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'returned\', [99])"]\n["(\'returned\', 99)"]\n["(\'stop\', \'StopIteration\', 42, (42,))"]\n["(\'returned\', [\'del\', \'after\'])"]\n["(\'returned\', [\'Finalized\', \'del\', \'after\'])"]\n',
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


class StopIterationValueTests(unittest.TestCase):
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
    setattr(StopIterationValueTests, "test_" + _name, _make(_name))


if __name__ == "__main__":
    unittest.main()
