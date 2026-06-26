"""RFC 0041 regression guard - the C-accelerator numeric / data tower.

Locks in the behaviour of the accelerators and numeric-tower fixes landed by
RFC 0041 so CI catches regressions without a full CPython `Lib/test/`
checkout. Each section maps to a workstream in the RFC. Plain `assert`s only -
the file exits 0 iff every behaviour matches CPython 3.13.
"""

# ===========================================================================
# WS-math - faithful mathmodule.c + cross-cutting numeric fixes
# ===========================================================================
import math
from fractions import Fraction
from decimal import Decimal

# fma / sumprod (3.12/3.13 surface) and the high-accuracy reductions.
assert math.fma(2.0, 3.0, 4.0) == 10.0
assert math.sumprod([1, 2, 3], [4, 5, 6]) == 32
assert math.fsum([0.1] * 10) == 1.0
assert math.dist((0, 0), (3, 4)) == 5.0
assert math.comb(10, 3) == 120 and math.perm(5, 2) == 20
assert math.isclose(math.gamma(6), 120.0)

# sum() accumulates through binary dispatch, so reflected __radd__ fires:
# a list of Fractions starting at int 0 sums to a Fraction, not a TypeError.
assert sum([Fraction(1, 3), Fraction(1, 6)]) == Fraction(1, 2)
assert isinstance(sum([Fraction(1, 3)]), Fraction)

# float(int) overflow is an OverflowError, not a silent inf.
try:
    float(10 ** 400)
except OverflowError:
    pass
else:
    raise AssertionError("float(huge int) should raise OverflowError")

# 0 ** -n raises ZeroDivisionError (int fast path).
try:
    0 ** -1
except ZeroDivisionError:
    pass
else:
    raise AssertionError("0 ** -1 should raise ZeroDivisionError")

# Correctly-rounded large int/int true division (the harmonic_mean edge):
# pre-rounding each operand to f64 would give 47.99999999999999.
assert 576460752303423488 / 12009599006321323 == 48.0

# ===========================================================================
# WS-json - json package over the native _json accelerator
# ===========================================================================
import json
import _json

# The C accelerator exists and exposes the five symbols Lib/json imports.
for _sym in ("make_scanner", "make_encoder", "scanstring",
             "encode_basestring", "encode_basestring_ascii"):
    assert hasattr(_json, _sym), _sym

# Round-trips, ordering, and ensure_ascii / separators / sort_keys.
assert json.loads('{"a": [1, 2.5, true, null, "x"]}') == {
    "a": [1, 2.5, True, None, "x"]}
assert json.dumps({"b": 1, "a": 2}, sort_keys=True) == '{"a": 2, "b": 1}'
assert json.dumps(["x", "y"], separators=(",", ":")) == '["x","y"]'
assert json.dumps("\u00e9") == '"\\u00e9"'
assert json.dumps("\u00e9", ensure_ascii=False) == '"\u00e9"'
assert _json.scanstring('"ab\\u0041"', 1) == ("abA", 10)

# JSONDecodeError carries msg/pos/lineno/colno.
try:
    json.loads("[1,\n2,]")
except json.JSONDecodeError as e:
    assert e.lineno == 2 and e.pos > 0 and isinstance(e.msg, str)
else:
    raise AssertionError("expected JSONDecodeError")

# ===========================================================================
# WS-csv - faithful _csv accelerator
# ===========================================================================
import csv
import _csv
import io

# Writer accepts any iterable and quotes per dialect; QUOTE_NONNUMERIC coerces.
_buf = io.StringIO()
_w = csv.writer(_buf, quoting=csv.QUOTE_NONNUMERIC)
_w.writerow(["a,b", 3, 'he said "hi"'])
assert _buf.getvalue() == '"a,b",3,"he said ""hi"""\r\n'

# Reader DFA: embedded newlines in quotes, quote doubling, NONNUMERIC floats.
_rows = list(csv.reader(io.StringIO('1,"x\ny",2\r\n'), ))
assert _rows == [["1", "x\ny", "2"]]
_rows = list(csv.reader(io.StringIO('1,2,3\r\n'), quoting=csv.QUOTE_NONNUMERIC))
assert _rows == [[1.0, 2.0, 3.0]]

# field_size_limit returns the prior limit and is enforced.
_old = csv.field_size_limit()
try:
    csv.field_size_limit(10)
    assert csv.field_size_limit() == 10
    try:
        list(csv.reader(io.StringIO("x" * 50)))
    except _csv.Error:
        pass
    else:
        raise AssertionError("field_size_limit not enforced")
finally:
    csv.field_size_limit(_old)

# _csv.Error lives in the _csv module.
assert _csv.Error.__module__ == "_csv"

# ===========================================================================
# WS-statistics - native _statistics + numeric-tower interop
# ===========================================================================
import statistics
import _statistics

# The native inverse-CDF helper backs NormalDist.inv_cdf.
assert math.isclose(_statistics._normal_dist_inv_cdf(0.5, 0.0, 1.0), 0.0,
                    abs_tol=1e-12)
_nd = statistics.NormalDist(0.0, 1.0)
assert math.isclose(_nd.inv_cdf(0.975), 1.959963984540054, rel_tol=1e-12)

# Fraction / Decimal interop in mean, and the weighted harmonic_mean form.
assert statistics.mean([Fraction(1, 2), Fraction(1, 2)]) == Fraction(1, 2)
assert statistics.mean([Decimal("0.5"), Decimal("1.5")]) == Decimal("1.0")
assert statistics.harmonic_mean([40, 60]) == 48.0

# NormalDist is __slots__-only: vars() raises TypeError, no instance __dict__.
try:
    vars(_nd)
except TypeError:
    pass
else:
    raise AssertionError("vars(NormalDist) should raise TypeError")

# COMPARE_OP returns the raw __eq__ result (CPython), not its truthiness.
class _Ten:
    def __eq__(self, other):
        return 10
assert (statistics.NormalDist(0, 1) == _Ten()) == 10

# ===========================================================================
# WS-containers - _heapq / _bisect accelerators
# ===========================================================================
import heapq
import _heapq
import bisect
import _bisect

_data = [5, 1, 4, 2, 8, 0, 3]
_h = list(_data)
heapq.heapify(_h)
assert [heapq.heappop(_h) for _ in range(len(_h))] == sorted(_data)
assert heapq.nsmallest(3, _data) == [0, 1, 2]
assert heapq.nlargest(3, _data) == [8, 5, 4]
assert hasattr(_heapq, "heappush") and hasattr(_bisect, "bisect_left")

# bisect: search + insort with a key, overflow-safe midpoint.
_s = [1, 3, 5, 7, 9]
assert bisect.bisect_left(_s, 5) == 2 and bisect.bisect_right(_s, 5) == 3
_keyed = [("a", 1), ("c", 3)]
bisect.insort(_keyed, ("b", 2), key=lambda kv: kv[1])
assert _keyed == [("a", 1), ("b", 2), ("c", 3)]

# sorted/min/max treat key=None as identity (not "call None").
assert sorted([3, 1, 2], key=None) == [1, 2, 3]
assert min([3, 1, 2], key=None) == 1

# ===========================================================================
# WS-datetime - _pydatetime split + native time/array edges
# ===========================================================================
import datetime
import time
import array

# The pure-Python implementation is selectable (datetime shim falls back to
# _pydatetime when _datetime is unavailable).
from test.support import import_fresh_module
_pydt = import_fresh_module("datetime", fresh=["datetime", "_pydatetime"],
                            blocked=["_datetime"])
assert _pydt is not None
assert _pydt.date(2024, 1, 2).isoformat() == "2024-01-02"

# strftime passes lone surrogates through (the WStr -> PUA bridge round-trip).
assert datetime.date(2002, 3, 22).strftime("%y\ud800%m") == "02\ud80003"
# %c accepts an int code point in the surrogate range (un-bridged correctly).
assert "%c" % 0xD800 == "\ud800"
assert "%c" % 0x41 == "A"
assert "%d" % 0xD800 == "55296"

# ctime + the libc asctime layout (space-padded day).
assert datetime.datetime(2002, 3, 22, 18, 3, 5).ctime() == "Fri Mar 22 18:03:05 2002"
assert hasattr(time, "ctime") and hasattr(time, "asctime")

# struct_time carries the hidden tz extras read by _local_timezone, but they
# don't leak into the 9-element sequence view.
_lt = time.localtime(0)
assert hasattr(_lt, "tm_gmtoff") and hasattr(_lt, "tm_zone")
assert len(_lt) == 9

# An out-of-range timestamp is an OverflowError, not a TypeError.
for _insane in (-1e200, 1e200):
    try:
        datetime.datetime.fromtimestamp(_insane)
    except OverflowError:
        pass
    else:
        raise AssertionError("fromtimestamp(%r) should raise OverflowError" % _insane)

# array.byteswap reverses each item's bytes in place (tzfile parsing).
_a = array.array("i", [1])
_a.byteswap()
assert _a[0] == 1 << 24
_a.byteswap()
assert _a[0] == 1

print("ok")
