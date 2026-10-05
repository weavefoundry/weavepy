"""Time individual Python operations on two interpreters and rank the
slowest relative to CPython.

    python3 tools/pybench/microops.py --weavepy BIN [--cpython python3.14]
        [--filter SUBSTR] [--target-ms 40] [--instructions]

Each case is a (setup, statement) pair. The statement runs in a loop
inside a generated function, so locals are fast locals as in real code.
The per-iteration time of an empty loop is subtracted.

``--instructions`` instead counts instructions retired (macOS
``/usr/bin/time -l``) for processes running each loop ten thousand
and a hundred thousand times, and reports the difference per
iteration (an empty loop's subtracted), which is stable on a loaded
machine.
"""

import argparse
import json
import re
import subprocess
import sys

SETUP_COMMON = r'''
import collections, functools, itertools, operator, re, math, json, copy, dataclasses, enum, abc, io, os, decimal, string, heapq, bisect
from collections import OrderedDict, defaultdict, Counter, deque, namedtuple, ChainMap
class P:
    __slots__ = ("x", "y")
    def __init__(self, x, y): self.x = x; self.y = y
class O:
    K = 5
    def __init__(self, x): self.x = x; self.y = 2
    def meth(self, a): return a
    @property
    def prop(self): return self.x
    @staticmethod
    def sm(a): return a
    @classmethod
    def cm(cls, a): return a
    def __eq__(self, other): return self.x == other.x
    def __hash__(self): return self.x
    def __lt__(self, other): return self.x < other.x
    def __add__(self, other): return O(self.x + other.x)
    def __len__(self): return 3
    def __getitem__(self, i): return i
    def __call__(self, a): return a
    def __iter__(self): return iter((1, 2, 3))
class Sub(O):
    def __init__(self, x):
        super().__init__(x)
    def meth(self, a): return super().meth(a)
class CM:
    def __enter__(self): return self
    def __exit__(self, *a): return False
class Abc(abc.ABC):
    pass
class AbcImpl(Abc):
    pass
@dataclasses.dataclass
class DC:
    a: int
    b: str = "x"
@dataclasses.dataclass(frozen=True)
class FDC:
    a: int
    b: int = 0
class Color(enum.Enum):
    RED = 1
    GREEN = 2
NT = namedtuple("NT", "a b c")
def f0(): return 1
def f1(a): return a
def f3(a, b, c): return a
def fkw(a, b=2, *, c=3): return a
def fva(*a): return a
def fvk(*a, **k): return a
def gen3():
    yield 1; yield 2; yield 3
def genfrom(): yield from gen3()
async def coro(): return 1
def deco(fn):
    @functools.wraps(fn)
    def w(*a, **k): return fn(*a, **k)
    return w
@deco
def decorated(a): return a
o = O(1); o2 = O(2); sub = Sub(1); p = P(1, 2)
lst = list(range(100)); tup = tuple(range(10)); s100 = set(range(100)); d100 = {i: i for i in range(100)}
sd = {"k%d" % i: i for i in range(20)}
text = "The quick brown fox jumps over the lazy dog " * 4
od = OrderedDict((i, i) for i in range(50)); dd = defaultdict(int); ctr = Counter("abracadabra"); dq = deque(range(50))
nt = NT(1, 2, 3); dc = DC(1); fdc = FDC(1); cmx = CM(); abci = AbcImpl()
pat = re.compile(r"(\w+)@(\w+)\.com")
partial1 = functools.partial(f3, 1, 2)
lam = lambda a: a
big = 10 ** 30
D1 = decimal.Decimal("1.25"); D2 = decimal.Decimal("3.5")
chain = ChainMap({"a": 1}, {"b": 2})
'''

CASES = [
    # calls
    ("call_f0", "f0()"),
    ("call_f1", "f1(i)"),
    ("call_f3", "f3(i, 2, 3)"),
    ("call_kw", "fkw(i, c=4)"),
    ("call_defaults", "fkw(i)"),
    ("call_varargs", "fva(1, 2, 3)"),
    ("call_star_kw", "fvk(1, 2, x=3)"),
    ("call_star_spread", "f3(*tup[:3])"),
    ("call_lambda", "lam(i)"),
    ("call_partial", "partial1(3)"),
    ("call_decorated", "decorated(i)"),
    ("call_method", "o.meth(i)"),
    ("call_super_method", "sub.meth(i)"),
    ("call_staticmethod", "o.sm(i)"),
    ("call_classmethod", "o.cm(i)"),
    ("call_dunder_call", "o(i)"),
    ("call_builtin_len", "len(lst)"),
    ("call_builtin_abs", "abs(-i)"),
    ("call_builtin_min2", "min(i, 5)"),
    ("call_builtin_isinstance", "isinstance(o, O)"),
    ("call_isinstance_tuple", "isinstance(i, (str, int))"),
    ("call_isinstance_abc", "isinstance(abci, Abc)"),
    ("call_getattr", "getattr(o, 'x')"),
    ("call_getattr_default", "getattr(o, 'zz', 0)"),
    ("call_hasattr_miss", "hasattr(o, 'zz')"),
    ("call_closure_make", "(lambda: i)"),
    ("def_closure", "def g(a): return a + i"),
    # objects
    ("new_object", "O(i)"),
    ("new_slots", "P(i, 2)"),
    ("new_subclass_super", "Sub(i)"),
    ("new_dataclass", "DC(i)"),
    ("new_frozen_dataclass", "FDC(i)"),
    ("new_namedtuple", "NT(1, 2, 3)"),
    ("attr_read", "o.x"),
    ("attr_write", "o.x = i"),
    ("attr_class_via_inst", "o.K"),
    ("attr_property", "o.prop"),
    ("attr_slots", "p.x"),
    ("attr_namedtuple", "nt.b"),
    ("attr_dataclass", "dc.a"),
    ("attr_enum", "Color.RED"),
    ("enum_value", "Color.RED.value"),
    ("enum_lookup", "Color(1)"),
    ("dunder_eq", "o == o2"),
    ("dunder_lt", "o < o2"),
    ("dunder_add", "o + o2"),
    ("dunder_hash", "hash(o)"),
    ("dunder_len", "len(o)"),
    ("dunder_getitem", "o[1]"),
    ("dataclass_eq", "dc == dc"),
    ("dataclass_repr", "repr(dc)"),
    ("nt_replace", "nt._replace(a=5)"),
    # control flow
    ("with_cm", "with cmx: pass"),
    ("try_except", "try:\n    raise ValueError\nexcept ValueError:\n    pass"),
    ("try_noexc", "try:\n    pass\nexcept ValueError:\n    pass"),
    ("keyerror_catch", "try:\n    sd['zz']\nexcept KeyError:\n    pass"),
    ("gen_list", "list(gen3())"),
    ("gen_yield_from", "list(genfrom())"),
    ("genexp_sum", "sum(x for x in tup)"),
    ("listcomp", "[x for x in tup]"),
    ("dictcomp", "{x: x for x in tup}"),
    ("coro_send", "c = coro()\ntry:\n    c.send(None)\nexcept StopIteration:\n    pass"),
    ("for_range10", "for _ in range(10): pass"),
    ("for_list10", "for _ in tup: pass"),
    ("enumerate10", "for _ in enumerate(tup): pass"),
    ("zip10", "for _ in zip(tup, tup): pass"),
    ("unpack3", "a, b, c = 1, 2, i"),
    ("star_unpack", "a, *b = tup"),
    # builtin types
    ("int_add", "i + 1"),
    ("int_mul_big", "big * i"),
    ("float_math", "math.sqrt(i * 1.5)"),
    ("int_str", "str(i)"),
    ("int_parse", "int('12345')"),
    ("float_str", "repr(1.5 * i)"),
    ("str_concat", "'a' + 'b' * 3"),
    ("str_fstring", "f'{i}-{i:05d}'"),
    ("str_percent", "'%d-%s' % (i, 'x')"),
    ("str_format", "'{}-{}'.format(i, 'x')"),
    ("str_join", "','.join(['a', 'b', 'c', 'd'])"),
    ("str_split", "text.split()"),
    ("str_replace", "text.replace('fox', 'cat')"),
    ("str_lower", "text.lower()"),
    ("str_startswith", "text.startswith('The')"),
    ("str_strip", "'  abc  '.strip()"),
    ("str_find", "text.find('lazy')"),
    ("str_in", "'lazy' in text"),
    ("str_index_char", "text[5]"),
    ("str_slice", "text[5:20]"),
    ("str_encode", "text.encode()"),
    ("str_iter", "for _ in 'abcdefghij': pass"),
    ("str_isdigit", "'12345'.isdigit()"),
    ("list_append_pop", "lst.append(i); lst.pop()"),
    ("list_index", "lst[50]"),
    ("list_slice", "lst[10:20]"),
    ("list_sort10", "sorted(tup, reverse=True)"),
    ("list_sort_key", "sorted(tup, key=lambda v: -v)"),
    ("list_in", "99 in lst"),
    ("list_copy", "lst[:]"),
    ("list_extend", "x = []; x.extend(tup)"),
    ("tuple_build", "(i, i, i)"),
    ("tuple_hash", "hash((1, 2, i))"),
    ("dict_get", "sd.get('k5')"),
    ("dict_getitem", "sd['k5']"),
    ("dict_set", "d100[i & 63] = i"),
    ("dict_in", "'k5' in sd"),
    ("dict_items_loop", "for k, v in sd.items(): pass"),
    ("dict_build", "{'a': 1, 'b': i}"),
    ("dict_kwargs_build", "dict(a=1, b=i)"),
    ("dict_copy", "sd.copy()"),
    ("dict_update", "x = {}; x.update(sd)"),
    ("set_add_discard", "s100.add(1000); s100.discard(1000)"),
    ("set_in", "50 in s100"),
    ("set_build", "{1, 2, i}"),
    ("set_and", "s100 & {1, 2, 3}"),
    ("frozenset_build", "frozenset((1, 2, 3))"),
    ("bytes_ops", "b'abc' + b'def'"),
    # collections / stdlib
    ("od_set", "od[i & 63] = i"),
    ("od_move_to_end", "od.move_to_end(5)"),
    ("od_get", "od[5]"),
    ("od_iter", "for _ in od: pass"),
    ("defaultdict_inc", "dd[i & 31] += 1"),
    ("counter_inc", "ctr['a'] += 1"),
    ("counter_build", "Counter('abracadabra')"),
    ("deque_append_pop", "dq.append(i); dq.popleft()"),
    ("chainmap_get", "chain['b']"),
    ("heapq_push_pop", "heapq.heappush(lst, 5); heapq.heappop(lst)"),
    ("bisect", "bisect.bisect(lst, 50)"),
    ("re_search", "pat.search('mail bob@example.com now')"),
    ("re_match_group", "pat.search('bob@example.com').group(1)"),
    ("re_sub", "re.sub(r'\\d', 'x', 'a1b2c3')"),
    ("re_findall", "re.findall(r'\\w+', 'a bc def')"),
    ("json_dumps", "json.dumps({'a': [1, 2, 3], 'b': 'x'})"),
    ("json_loads", "json.loads('{\"a\": [1, 2, 3], \"b\": \"x\"}')"),
    ("copy_copy_list", "copy.copy(lst)"),
    ("deepcopy_small", "copy.deepcopy({'a': [1, 2], 'b': (3, 4)})"),
    ("itertools_chain", "list(itertools.chain(tup, tup))"),
    ("itertools_islice", "list(itertools.islice(lst, 10))"),
    ("functools_reduce", "functools.reduce(operator.add, tup)"),
    ("map_lambda", "list(map(lambda v: v, tup))"),
    ("filter_none", "list(filter(None, tup))"),
    ("any_genexp", "any(x > 8 for x in tup)"),
    ("sum_list", "sum(lst)"),
    ("max_key", "max(tup, key=lambda v: -v)"),
    ("stringio_write", "b = io.StringIO(); b.write('abc'); b.getvalue()"),
    ("decimal_add", "D1 + D2"),
    ("decimal_mul", "D1 * D2"),
    ("decimal_new", "decimal.Decimal('1.23')"),
    ("math_floor", "math.floor(2.5)"),
    ("repr_list", "repr(tup)"),
    ("str_dict", "str(sd)"),
    ("os_path_join", "os.path.join('a', 'b', 'c')"),
    ("global_read", "lst"),
    ("builtin_read", "len"),
]

RUNNER = r'''
import json, sys, time
SETUP
cases = json.loads(sys.argv[1])
target = float(sys.argv[2]) / 1000.0
def make(stmt):
    body = "\n".join("        " + line for line in stmt.splitlines())
    src = "def bench(n):\n    for i in range(n):\n" + body + "\n"
    ns = dict(globals())
    exec(src, ns)
    return ns["bench"]
def per_iter(fn):
    n = 64
    while True:
        t0 = time.perf_counter(); fn(n); dt = time.perf_counter() - t0
        if dt > target / 8 or n > 1 << 26:
            break
        n *= 4
    n = max(64, int(n * (target / max(dt, 1e-9)) / 3))
    best = None
    for _ in range(3):
        t0 = time.perf_counter(); fn(n); dt = (time.perf_counter() - t0) / n
        best = dt if best is None else min(best, dt)
    return best
empty = per_iter(make("pass"))
out = {}
for name, stmt in cases:
    try:
        out[name] = max(per_iter(make(stmt)) - empty, 1e-10) * 1e9
    except Exception as e:
        out[name] = "error: %s: %s" % (type(e).__name__, e)
print(json.dumps(out))
'''


ONE = r'''
import sys
SETUP
stmt = sys.argv[1]
body = "\n".join("        " + line for line in stmt.splitlines())
ns = dict(globals())
exec("def bench(n):\n    for i in range(n):\n" + body + "\n", ns)
ns["bench"](int(sys.argv[2]))
'''


def count_instructions(interp, stmt, n):
    src = ONE.replace("SETUP", SETUP_COMMON)
    proc = subprocess.run(["/usr/bin/time", "-l", interp, "-c", src, stmt, str(n)],
                          capture_output=True, text=True, timeout=600)
    m = re.search(r"(\d+)\s+instructions retired", proc.stderr)
    if proc.returncode != 0 or not m:
        return None
    return int(m.group(1))


def run_instructions(interp, cases, lo=10000, hi=110000):
    def per_iter(stmt):
        a = count_instructions(interp, stmt, lo)
        b = count_instructions(interp, stmt, hi)
        if a is None or b is None:
            return None
        return (b - a) / (hi - lo)

    empty = per_iter("pass")
    out = {}
    for name, stmt in cases:
        v = per_iter(stmt)
        out[name] = "error" if v is None else max(v - empty, 1.0)
    return out


def run(interp, cases, target):
    src = RUNNER.replace("SETUP", SETUP_COMMON)
    proc = subprocess.run([interp, "-c", src, json.dumps(cases), str(target)],
                          capture_output=True, text=True, timeout=3600)
    if proc.returncode != 0:
        raise SystemExit("%s failed:\n%s" % (interp, proc.stderr[-3000:]))
    return json.loads(proc.stdout.strip().splitlines()[-1])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--weavepy", required=True)
    ap.add_argument("--cpython", default="python3.14")
    ap.add_argument("--filter", action="append")
    ap.add_argument("--target-ms", type=float, default=40)
    ap.add_argument("--sort", choices=["ratio", "name"], default="ratio")
    ap.add_argument("--json")
    ap.add_argument("--instructions", action="store_true")
    args = ap.parse_args()
    cases = CASES
    if args.filter:
        cases = [c for c in cases if any(f in c[0] for f in args.filter)]
    if args.instructions:
        cp = run_instructions(args.cpython, cases)
        wp = run_instructions(args.weavepy, cases)
    else:
        cp = run(args.cpython, cases, args.target_ms)
        wp = run(args.weavepy, cases, args.target_ms)
    unit = "in" if args.instructions else "ns"
    rows = []
    for name, _ in cases:
        c, w = cp[name], wp[name]
        if isinstance(c, str) or isinstance(w, str):
            rows.append((float("inf"), name, c, w))
        else:
            rows.append((w / c, name, c, w))
    if args.sort == "ratio":
        rows.sort(key=lambda r: -r[0])
    import math
    finite = [r[0] for r in rows if r[0] != float("inf")]
    for ratio, name, c, w in rows:
        if ratio == float("inf"):
            print("%-24s cpython %s | weavepy %s" % (name, c, w))
        else:
            print("%-24s cpython %9.1f %s  weavepy %9.1f %s  ratio %6.2f" % (name, c, unit, w, unit, ratio))
    print("geomean ratio over %d ops: %.3f" % (len(finite), math.exp(sum(map(math.log, finite)) / len(finite))))
    if args.json:
        with open(args.json, "w") as fh:
            json.dump({"cpython": cp, "weavepy": wp}, fh, indent=1)


if __name__ == "__main__":
    sys.exit(main())
