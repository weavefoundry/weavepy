"""Native list temporaries die promptly, even with cyclic GC disabled."""

import gc
import weakref


def convert_bytes(data):
    out = []
    for i, value in enumerate(data):
        out.append((i ^ value) & 255)
    return bytes(out)


def returned_list(n):
    out = []
    for i in range(n):
        out.append(i)
    return out


references = []


class Leaf:
    pass


def make_leaf():
    leaf = Leaf()
    references.append(weakref.ref(leaf))
    return leaf


def discard_objects(n):
    out = []
    for i in range(n):
        out.append(make_leaf())
    return None


def list_count():
    return sum(1 for obj in gc.get_objects() if type(obj) is list and len(obj) == 1024)


data = bytes(range(256)) * 4
expected = bytes(1024)
for _ in range(60):
    assert convert_bytes(data) == expected
    assert returned_list(12) == list(range(12))
    discard_objects(3)
references.clear()
gc.collect()
was_enabled = gc.isenabled()
gc.disable()
try:
    before = list_count()
    for _ in range(200):
        assert convert_bytes(data) == expected
    after = list_count()
    assert after <= before, (before, after)

    # Cleanup must retain lists that escape the native activation.
    result = returned_list(1024)
    alias = result
    assert gc.is_tracked(result)
    assert result == list(range(1024))
    result[0] = -1
    assert alias[0] == -1
    del result, alias
    assert list_count() <= before

    # A temporary list can be the last owner of weak-referenceable
    # children. Its cleanup must release those children at frame exit.
    discard_objects(3)
    assert len(references) == 3
    assert all(ref() is None for ref in references)
finally:
    if was_enabled:
        gc.enable()
print("ok")
