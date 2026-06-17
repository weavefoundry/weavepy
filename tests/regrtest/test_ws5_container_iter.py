"""RFC 0039 WS5 — container + iterator fidelity (list/tuple/iterator).

Pins the CPython-faithful container behaviour that retired the
`test_list`/`test_tuple` `gc.collect()` reachable-hang: `PyObject_Rich-
CompareBool` identity-first membership, `list_richcompare` with a live-length
re-read under a mutating `__eq__` (bpo-38588), recursive-repr cycle detection,
`list.sort` list-modified detection, list-iterator detach-on-exhaustion, and
shared-store iterator pickling. All deterministic — no collector timing.
"""

import pickle


# ---------------------------------------------------------------------------
# Identity-first membership / count / index (PyObject_RichCompareBool): an
# object that is unequal to everything is still found by identity, and a NaN
# is found in a container by identity even though `nan != nan`.
# ---------------------------------------------------------------------------

class AlwaysUnequal:
    def __eq__(self, other):
        return False

    __hash__ = object.__hash__


u = AlwaysUnequal()
assert u in [1, 2, u, 3]
assert [1, u, u].count(u) == 2
assert [1, u, 2].index(u) == 1

nan = float("nan")
assert nan in [1, nan, 2]
assert (1, nan, 2).count(nan) == 1


# ---------------------------------------------------------------------------
# list_richcompare under a mutating __eq__ (bpo-38588): comparing two lists
# whose element `__eq__` clears the *other* list mid-compare must not crash;
# the result follows CPython's live-length re-read (the first element compares
# equal, then the now-empty operand makes the lengths differ → not equal).
# ---------------------------------------------------------------------------

class Clears:
    def __init__(self):
        self.other = None

    def __eq__(self, other):
        del self.other[:]
        return True

    __hash__ = None


a = [Clears()]
b = [Clears()]
a[0].other = b  # comparing a[0] == b[0] clears b
b[0].other = a
assert (a == b) is False  # CPython: lengths diverge to 1 vs 0 after the clear
assert b == [] and len(a) == 1


# ---------------------------------------------------------------------------
# Recursive repr cycle detection: a self-referential list/tuple/dict renders
# the CPython ellipsis sentinel rather than recursing forever.
# ---------------------------------------------------------------------------

L = [1, 2]
L.append(L)
assert repr(L) == "[1, 2, [...]]", repr(L)

D = {}
D["self"] = D
assert repr(D) == "{'self': {...}}", repr(D)

# A list inside a tuple inside the list still terminates.
inner = []
T = (inner,)
inner.append(T)
assert repr(inner) == "[([...],)]", repr(inner)


# ---------------------------------------------------------------------------
# list.sort detects mutation during the sort (ValueError), and rejects
# positional args; METH_NOARGS arity is enforced for clear/copy/reverse/pop.
# ---------------------------------------------------------------------------

class Vicious:
    def __init__(self, sink):
        self.sink = sink

    def __lt__(self, other):
        self.sink.append(len(self.sink))
        return True


sink = []
sink.extend([Vicious(sink), Vicious(sink), Vicious(sink)])
try:
    sink.sort()
    raise AssertionError("sort of a self-mutating list should raise ValueError")
except ValueError:
    pass

for bad in (
    lambda: [1].sort(None),       # positional arg
    lambda: [1].clear(1),          # METH_NOARGS
    lambda: [1].reverse(1),
    lambda: [1].copy(1),
    lambda: [1].pop(1, 2),
):
    try:
        bad()
        raise AssertionError("expected TypeError for bad arity")
    except TypeError:
        pass


# ---------------------------------------------------------------------------
# list iterator detaches on exhaustion: a StopIteration'd iterator does not
# resurrect when the backing list grows again.
# ---------------------------------------------------------------------------

data = [1, 2, 3]
it = iter(data)
assert list(it) == [1, 2, 3]
data.append(4)
assert list(it) == [], "exhausted iterator must not resurrect"


# ---------------------------------------------------------------------------
# Shared-store iterator pickling: co-pickling (iter(xs), xs) round-trips to a
# single shared list so the unpickled iterator tracks later mutations.
# ---------------------------------------------------------------------------

xs = [10, 20, 30, 40]
src = iter(xs)
assert next(src) == 10
clone_it, clone_xs = pickle.loads(pickle.dumps((src, xs)))
clone_xs.append(50)
assert list(clone_it) == [20, 30, 40, 50], "iterator did not track shared store"

# reversed() over a list shares the store too.
rev = reversed([1, 2, 3])
rclone_it, = pickle.loads(pickle.dumps((rev,)))
assert list(rclone_it) == [3, 2, 1]


# ---------------------------------------------------------------------------
# Tuple identity short-circuits: `t * 1 is t` and `tuple(t) is t` return the
# same object (tuples are immutable).
# ---------------------------------------------------------------------------

t = (1, 2, 3)
assert t * 1 is t
assert tuple(t) is t


print("WS5 container/iterator fidelity ok")
