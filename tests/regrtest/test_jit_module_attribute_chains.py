"""Borrow module chains while respecting owners, mutations, and callbacks."""

import gc
import math
import sys
import types
import weakref


def child(value):
    result = types.ModuleType("child")
    result.value = value
    return result


def integer_read(n, root):
    total = 0
    for iteration in range(n):
        total += root._weavepy_chain_child.value
    return total


def object_read(n, root):
    result = None
    for iteration in range(n):
        result = root._weavepy_chain_child.value
    return result


def allocated_read(n):
    root = types.ModuleType("allocated")
    root._weavepy_chain_child = child(3)
    total = 0
    for iteration in range(n):
        total += root._weavepy_chain_child.value
    return total


python_root = types.ModuleType("root")
native_root = sys
roots = (python_root, native_root, math)
for root in roots:
    assert not hasattr(root, "_weavepy_chain_child")
    root._weavepy_chain_child = child(3)
    for repeat in range(2):
        assert integer_read(20000, root) == 60000
        assert object_read(3000, root) == 3
assert allocated_read(20000) == 60000

# MODULE CHAIN MUTATIONS: native counters and direct borrow checks run here.

# The same sites must check both module identity and the key at a cached index.
for index, root in enumerate(roots):
    del root._weavepy_chain_child
    root._weavepy_chain_padding = "reorder"
    root._weavepy_chain_child = child(7 + index)
    assert integer_read(31, root) == 31 * (7 + index)
    tail = root._weavepy_chain_child
    del tail.value
    tail.padding = "reorder"
    tail.value = 11 + index
    assert integer_read(29, root) == 29 * (11 + index)
    root._weavepy_chain_child = child(17 + index)
    assert integer_read(23, root) == 23 * (17 + index)
    # Changing only a value does not change the dictionary's layout stamp.
    root._weavepy_chain_child.value = 19 + index
    assert integer_read(19, root) == 19 * (19 + index)

# Integer guards must decline before committing the result or the addition.
for root in roots:
    for value in (True, 1.5, 2 ** 80, -(2 ** 80)):
        root._weavepy_chain_child.value = value
        assert integer_read(13, root) == 13 * value
    payload = object()
    root._weavepy_chain_child.value = payload
    assert object_read(29, root) is payload
    root._weavepy_chain_child.value = None
    assert object_read(29, root) is None
    del root._weavepy_chain_child.value
    try:
        integer_read(1, root)
    except AttributeError as exc:
        assert "value" in str(exc)
        tb = exc.__traceback__
        while tb.tb_next is not None:
            tb = tb.tb_next
        assert tb.tb_frame.f_code is integer_read.__code__
    else:
        raise AssertionError("missing module field did not raise")
    root._weavepy_chain_child.value = 3

# Module-level __getattr__ runs once for each missing final field.
seen = []


def missing_value(name):
    frame = sys._getframe(1)
    seen.append((name, frame.f_code.co_name, frame.f_locals["root"]))
    return 5


tail = python_root._weavepy_chain_child
del tail.value
tail.__getattr__ = missing_value
assert integer_read(7, python_root) == 35
assert seen == [("value", "integer_read", python_root)] * 7
del tail.__getattr__
tail.value = 3

# A Python-created module's class change must revoke its plain lookup.
class ObservedModule(types.ModuleType):
    def __getattribute__(self, name):
        if name == "_weavepy_chain_child":
            frame = sys._getframe(1)
            seen.append((frame.f_code.co_name, frame.f_locals["root"]))
        return types.ModuleType.__getattribute__(self, name)


for root in (python_root,):
    assert integer_read(3000, root) == 9000
    seen.clear()
    root.__class__ = ObservedModule
    try:
        assert integer_read(7, root) == 21
        assert seen == [("integer_read", root)] * 7
    finally:
        root.__class__ = types.ModuleType


class Child:
    pass


def value_descriptor(self):
    frame = sys._getframe(1)
    seen.append((frame.f_code.co_name, frame.f_locals["root"]))
    return 7


tail = Child()
tail.value = 3
python_root._weavepy_chain_child = tail
assert integer_read(3000, python_root) == 9000
seen.clear()
Child.value = property(value_descriptor)
assert integer_read(7, python_root) == 49
assert seen == [("integer_read", python_root)] * 7
del Child.value

# A completed read keeps its final result alive, but releases intermediates.
class Payload:
    pass


for root in roots:
    value = Payload()
    leaf = child(value)
    leaf_ref = weakref.ref(leaf)
    value_ref = weakref.ref(value)
    root._weavepy_chain_child = leaf
    result = object_read(3000, root)
    assert result is value
    del leaf, value
    root._weavepy_chain_child = child(3)
    gc.collect()
    assert leaf_ref() is None
    assert value_ref() is result
    del result
    gc.collect()
    assert value_ref() is None

# Native modules can also appear inside the chain, behind an ordinary owner.
interior = types.ModuleType("interior")
interior._weavepy_chain_child = sys
sys.value = 23
try:
    assert integer_read(20000, interior) == 460000
    sys.value = 29
    assert integer_read(31, interior) == 899
finally:
    del sys.value

for root in roots:
    del root._weavepy_chain_child
    del root._weavepy_chain_padding
