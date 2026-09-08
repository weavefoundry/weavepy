"""RFC 0077 Pillar I (performance wave 12) — engine fixes, one canary each.

1. `del list[a:b]` is one pass, and every slice shape (contiguous,
   negative, extended, empty, out of range) matches CPython. The old
   element-wise form was O(k * n): `del data[:40000]` on 80k elements
   took 660 ms and made every `del buf[:n]` consumer loop quadratic.

2. `collections.deque` end operations are amortized O(1) in both
   directions (`appendleft`/`popleft` used `list.insert(0)`/`list.pop(0)`),
   with the semantics the head-offset rewrite must preserve: indexing
   through a consumed prefix, `maxlen` eviction on both ends, `rotate`,
   `__reduce__`, comparisons, iteration invalidation.

3. A never-awaited coroutine whose last holder is a traceback-pinned
   frame released by `assertRaises` (unittest clears frames on exit) is
   finalized *inside* the enclosing `assertWarns` block, as on CPython.
   The WS2 hot/cold finalizable gate had demoted the coroutine to cold
   during the exception's construction, deferring the warning by up to
   a cold stride; `gc_trace::mark_bulk_drop` re-grades after an
   exceptional unwind and after `frame.clear()`.

4. `-X importtime` / `PYTHONPROFILEIMPORTTIME` print CPython's per-load
   lines to stderr (previously a documented no-op).

5. The tier-2 JIT models `COPY n` at depth n. It read `COPY 2` as a
   plain dup, so once the WS9 bytecode switch admitted CPython 3.14's
   chained-comparison shape (`SWAP 2; COPY 2; COMPARE_OP`), a hot
   `lo <= x <= hi` compared `lo` with itself (the shift_jis decoder's
   single-byte test misfired after ~50 calls).
"""

import os
import subprocess
import sys
import time
import unittest
import warnings
from collections import deque

# ------------- 1. slice deletion: one pass, CPython semantics -------------


def _ref_del(n, sl):
    ref = list(range(n))
    idx = list(range(n))[sl]
    keep = [v for v in ref if v not in set(idx)]
    return keep


for n in (0, 1, 7, 64):
    for sl in (
        slice(None),
        slice(2, 5),
        slice(-3, None),
        slice(None, -2),
        slice(None, None, 2),
        slice(None, None, -1),
        slice(1, -1, 3),
        slice(5, 2),
        slice(100, 200),
        slice(-200, 2),
        slice(None, None, -3),
        slice(0, 0),
    ):
        data = list(range(n))
        del data[sl]
        assert data == _ref_del(n, sl), (n, sl, data)

# Linear, not quadratic: doubling n must not quadruple the time. Generous
# ratios so a loaded CI host cannot flake it; the old code was 4x per
# doubling (42 ms, 165 ms, 662 ms at 20k/40k/80k).
_big = list(range(200_000))
_t0 = time.perf_counter()
del _big[: len(_big) // 2]
_t_half = time.perf_counter() - _t0
assert len(_big) == 100_000
assert _t_half < 0.5, f"del list[:n/2] took {_t_half:.3f}s on 200k elements"

_ext = list(range(200_000))
_t0 = time.perf_counter()
del _ext[::2]
assert len(_ext) == 100_000 and _ext[0] == 1 and _ext[-1] == 199_999
assert time.perf_counter() - _t0 < 0.5

# ------------- 2. deque: O(1) ends and head-offset semantics -------------

_d = deque(range(10))
for _ in range(4):
    _d.popleft()
assert list(_d) == [4, 5, 6, 7, 8, 9]
assert (_d[0], _d[-1], _d[2], len(_d)) == (4, 9, 6, 6)
_d[0] = 40
assert _d[0] == 40 and _d.popleft() == 40
_d.appendleft(3)
_d.appendleft(2)
assert list(_d) == [2, 3, 5, 6, 7, 8, 9]
del _d[1]
assert list(_d) == [2, 5, 6, 7, 8, 9]
_d.rotate(2)
assert list(_d) == [8, 9, 2, 5, 6, 7]
_d.rotate(-2)
assert list(_d) == [2, 5, 6, 7, 8, 9]
assert repr(_d) == "deque([2, 5, 6, 7, 8, 9])"
assert _d == deque([2, 5, 6, 7, 8, 9]) and _d != deque([2, 5])
assert deque([1, 2]) < deque([1, 3])
_cls, _args, *_rest = _d.__reduce__()
assert _cls is deque and _args == () or _args == ([2, 5, 6, 7, 8, 9],), (_cls, _args)
_it = iter(_d)
next(_it)
_d.append(10)
try:
    next(_it)
except RuntimeError:
    pass
else:
    raise AssertionError("deque iterator did not detect mutation")

_m = deque(maxlen=3)
for i in range(5):
    _m.append(i)
assert list(_m) == [2, 3, 4]
_m.appendleft(9)
assert list(_m) == [9, 2, 3]
_m.extendleft([7, 8])
assert list(_m) == [8, 7, 9]
_m.extend([1, 2, 3, 4])
assert list(_m) == [2, 3, 4]
assert _m.maxlen == 3

_e = deque()
for exc_case in (lambda: _e.pop(), lambda: _e.popleft(), lambda: _e[0]):
    try:
        exc_case()
    except IndexError:
        pass
    else:
        raise AssertionError("empty deque did not raise IndexError")

# Amortized O(1): 200k pushes then 200k pops per end. The old form was
# O(n) per left-side op (the full 200k took ~50 s; now ~2 s interp-only).
_q = deque()
_t0 = time.perf_counter()
for i in range(200_000):
    _q.append(i)
while _q:
    _q.popleft()
for i in range(200_000):
    _q.appendleft(i)
while _q:
    _q.pop()
for i in range(100_000):
    _q.appendleft(i)
    _q.append(i)
    _q.popleft()
    _q.pop()
_dt = time.perf_counter() - _t0
assert _dt < 20.0, f"deque end operations took {_dt:.1f}s (quadratic?)"

# ------------- 3. never-awaited warning inside assertWarns -------------


class _NeverAwaited(unittest.TestCase):
    def test_traceback_pinned_coroutine_finalizes_promptly(self):
        import asyncio

        loop = asyncio.new_event_loop()
        try:

            async def coro1():
                await asyncio.sleep(0)

            async def coro2():
                loop.run_until_complete(coro1())

            with self.assertWarnsRegex(RuntimeWarning, r"coroutine \S+ was never awaited"):
                self.assertRaises(RuntimeError, loop.run_until_complete, coro2())
        finally:
            loop.close()


for _ in range(5):
    _suite = unittest.TestLoader().loadTestsFromTestCase(_NeverAwaited)
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", ResourceWarning)
        _result = unittest.TextTestRunner(stream=open(os.devnull, "w"), verbosity=0).run(_suite)
    assert _result.wasSuccessful(), (_result.failures, _result.errors)

# ------------- 4. -X importtime prints per-load lines -------------

_out = subprocess.run(
    [sys.executable, "-X", "importtime", "-c", "import json"],
    capture_output=True,
    text=True,
    check=True,
)
_lines = [ln for ln in _out.stderr.splitlines() if ln.startswith("import time:")]
assert _lines and _lines[0].startswith("import time: self [us] | cumulative | imported package"), _out.stderr[:400]
assert any(ln.rstrip().endswith("| json") for ln in _lines), _out.stderr[:400]
assert any("json.decoder" in ln for ln in _lines), _out.stderr[:400]
_out2 = subprocess.run(
    [sys.executable, "-c", "import json"],
    capture_output=True,
    text=True,
    check=True,
    env={**os.environ, "PYTHONPROFILEIMPORTTIME": "1"},
)
assert "import time:" in _out2.stderr

# ------------- 5. JIT: COPY n at depth n (chained comparisons) -------------


def _between(c):
    return 0xA1 <= c <= 0xDF


def _strict(c):
    return 10 < c < 20


def _bounded(c, lo, hi):
    return lo <= c <= hi


def _eq3(a, b, c):
    return a == b == c


def _single_byte(c):
    if 0xA1 <= c <= 0xDF:
        return chr(0xFEC0 + c)
    return None


# Past the tier-2 threshold (50 calls), each function runs natively.
for _ in range(200):
    _between(0xC0)
    _strict(15)
    _bounded(15, 10, 20)
    _eq3(1, 1, 1)
    _single_byte(0xC0)
assert [_between(c) for c in (0x41, 0x81, 0xA1, 0xC0, 0xDF, 0xE0, 0xF1)] == [
    False, False, True, True, True, False, False,
]
assert [_strict(c) for c in (5, 10, 11, 15, 19, 20, 25)] == [
    False, False, True, True, True, False, False,
]
assert [_bounded(c, 10, 20) for c in (5, 10, 11, 15, 19, 20, 25)] == [
    False, True, True, True, True, True, False,
]
assert [_eq3(*t) for t in ((1, 1, 1), (1, 2, 1), (1, 1, 2), (2, 1, 1))] == [
    True, False, False, False,
]
assert _single_byte(0x81) is None
assert _single_byte(0xB1) == "\uff71"
assert b"\x82\xf1".decode("shift_jis") == "\u3093"

print("rfc0077-floor-regressions: ok")
