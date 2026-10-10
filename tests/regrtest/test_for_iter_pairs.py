"""`for a, b in ...` loops in compiled code, whose `FOR_ITER` hands the pair
straight to the stack (dict items, `enumerate`, `zip`, lists and tuples of
pairs) and `for` loops over dicts and their views: values, unpacking errors,
mutation during iteration, and exhaustion. Each loop runs enough times for
the compiled loop to take it."""
from collections import OrderedDict

ROUNDS = 400


def items_sum(d):
    t = 0
    for k, v in d.items():
        t += v
    return t


def keys_list(d):
    out = []
    for k in d:
        out.append(k)
    return out


def values_sum(d):
    t = 0
    for v in d.values():
        t += v
    return t


def pairs_of(seq):
    out = []
    for a, b in seq:
        out.append((b, a))
    return out


def enum_pairs(seq):
    out = []
    for i, x in enumerate(seq):
        out.append(i * 10 + x)
    return out


def zip_pairs(xs, ys):
    out = []
    for a, b in zip(xs, ys):
        out.append(a - b)
    return out


d = {"k%d" % i: i for i in range(30)}
od = OrderedDict((str(i), i) for i in range(10))
for _ in range(ROUNDS):
    assert items_sum(d) == sum(range(30))
    assert keys_list(d) == ["k%d" % i for i in range(30)]
    assert values_sum(d) == sum(range(30))
    assert items_sum(od) == 45
    assert pairs_of([(1, 2), (3, 4)]) == [(2, 1), (4, 3)]
    assert pairs_of(((1, "a"), (2, "b"))) == [("a", 1), ("b", 2)]
    assert pairs_of(["ab", "cd"]) == [("b", "a"), ("d", "c")]
    assert pairs_of([[1, 2], (3, 4)]) == [(2, 1), (4, 3)]
    assert enum_pairs([5, 6, 7]) == [5, 16, 27]
    assert zip_pairs([5, 6, 7], [1, 1]) == [4, 5]
    assert items_sum({}) == 0

    # A wrong-sized item raises from the unpacking.
    for bad in ([(1, 2), (1, 2, 3)], [(1, 2), 5], ((1, 2), (3,))):
        try:
            pairs_of(bad)
        except (ValueError, TypeError):
            pass
        else:
            raise AssertionError("bad unpack not detected: %r" % (bad,))

    # Changing a dict's size during iteration raises.
    m = dict(d)
    try:
        for k, v in m.items():
            if v == 5:
                m["new"] = 1
    except RuntimeError as e:
        assert "changed size during iteration" in str(e), e
    else:
        raise AssertionError("dict size change not detected")
    m = dict(d)
    try:
        for k in m:
            if k == "k3":
                del m["k4"]
    except RuntimeError:
        pass
    else:
        raise AssertionError("dict deletion not detected")
    # Replacing values in place is allowed.
    m = dict(d)
    for k, v in m.items():
        m[k] = v + 1
    assert items_sum(m) == sum(range(30)) + 30

    # A list mutated mid-loop: the iteration sees the change.
    seq = [(1, 2), (3, 4)]
    out = []
    for a, b in seq:
        out.append(a)
        if len(seq) < 4:
            seq.append((a + 10, b))
    assert out == [1, 3, 11, 13], out

    # An exhausted items iterator stays exhausted.
    it = iter(d.items())
    for k, v in it:
        pass
    assert list(it) == []
print("for-iter pairs ok")
