"""Polymorphic pure getters preserve fallback and returned ownership."""

import gc
import weakref


def choose(self):
    if self.flag:
        return self.value
    return self.other


def invoke(obj):
    # Keep mutations on the same warmed call site as the initial reads.
    return choose(obj)


class First:
    selected = property(choose)


class Second:
    selected = property(choose)


class Third:
    selected = property(choose)


class Payload:
    pass


values = [Payload(), Payload(), Payload(), Payload(), Payload(), Payload()]
objects = [First(), Second(), Third()]
for index, obj in enumerate(objects):
    # Each class places the fields at different dictionary indices.
    if index:
        obj.padding = index
    if index == 2:
        obj.another_padding = index
    obj.flag = True
    obj.value = values[index * 2]
    obj.other = values[index * 2 + 1]


class Holder:
    current = None
    alternate = None


def choose_owned(container, flag):
    if flag:
        return container.current.value
    return container.alternate.other


holders = []
for obj in objects:
    class Container:
        pass
    Container.current = Container.alternate = obj
    holders.append(Container)


def warm_readers(objects, values, holders):
    for iteration in range(3000):
        for index, obj in enumerate(objects):
            obj.flag = iteration % 2 == 0
            assert obj.selected is values[index * 2 + (not obj.flag)]
            assert invoke(obj) is values[index * 2 + (not obj.flag)]
            assert choose_owned(holders[index], obj.flag) is values[index * 2 + (not obj.flag)]


warm_readers(objects, values, holders)

# PURE GETTER MUTATIONS:
obj = objects[1]
obj.flag = True
before = obj.value
del obj.value
obj.reordered = 42
obj.value = before
assert obj.selected is before
assert invoke(obj) is before

old = obj.value
seen = weakref.ref(old)
obj.value = Payload()
held = obj.selected
values[2] = None
old = before = None
gc.collect()
assert seen() is None
assert held is obj.value
assert invoke(obj) is held

calls = []


def descriptor(receiver):
    calls.append(receiver)
    return held


Second.value = property(descriptor)
assert obj.selected is held
assert calls == [obj]
calls.clear()
assert invoke(obj) is held
assert calls == [obj]
del Second.value
assert obj.selected is held
assert invoke(obj) is held

obj = objects[2]
obj.flag = False
del obj.other
try:
    obj.selected
except AttributeError:
    pass
else:
    raise AssertionError('missing selected field must raise')
try:
    invoke(obj)
except AttributeError:
    pass
else:
    raise AssertionError('missing direct field must raise')
obj.other = None
assert obj.selected is None
assert invoke(obj) is None
obj.other = 37
assert obj.selected == 37
assert invoke(obj) == 37
obj.other = b'payload'
assert obj.selected == b'payload'
assert invoke(obj) == b'payload'

# A class-owned receiver enters the evaluator's owned scratch. Its field can
# be borrowed during evaluation, but the returned object must acquire an owner.
def retained_result(container):
    result = None
    for _ in range(100):
        result = choose_owned(container, True)
    return result


owner = First()
owner.value = Payload()
Holder.current = owner
result = retained_result(Holder)
watch = weakref.ref(result)
owner.value = None
Holder.current = None
owner = None
gc.collect()
assert watch() is result
del result
gc.collect()
assert watch() is None

# Changing an attribute hook invalidates the old polymorphic shape.
obj = objects[1]
obj.flag = True
calls.clear()


def access(receiver, name):
    if name == "value":
        calls.append(receiver)
        return 101
    return object.__getattribute__(receiver, name)


Second.__getattribute__ = access
assert obj.selected == 101
assert calls == [obj]
calls.clear()
assert invoke(obj) == 101
assert calls == [obj]
del Second.__getattribute__
assert obj.selected is held
assert invoke(obj) is held

# The fallback preserves Python's caller frame and runs a descriptor once.
import sys
calls.clear()


def observing_value(receiver):
    caller = sys._getframe(1)
    calls.append((caller.f_code.co_name, caller.f_locals["self"]))
    return held


Second.value = property(observing_value)
assert obj.selected is held
assert calls == [("choose", obj)]
calls.clear()
assert invoke(obj) is held
assert calls == [("choose", obj)]
del Second.value
print("polymorphic pure-leaf borrows: ok")
