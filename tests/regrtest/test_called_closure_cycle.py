"""A cycle through the cells of a closure that has been called is collectable.

The lean call paths cache a closure's cells on the function as a frame
``cells`` vector, a second strong handle on each cell. The collector has to
count that handle as the function's own, or every cell of a closure that
ever ran looks externally held and no cycle through one is collected.
attrs' slotted ``_ClassBuilder`` is the real-world shape: it stores hooks
that close over ``self`` and calls them while building the class, and the
cycle pinned the class it replaced (attrs' ``test_no_references_to_original``).
"""

import gc
import weakref


class Target:
    pass


class Builder:
    __slots__ = ("cls", "hooks")

    def __init__(self, cls):
        self.cls = cls
        self.hooks = []

    def add(self):
        def hook(namespace):
            namespace["cls"] = self.cls

        self.hooks.append(hook)

    def build(self):
        namespace = {}
        for hook in self.hooks:
            hook(namespace)
        return namespace


def called_cycle():
    target = Target()
    builder = Builder(target)
    builder.add()
    builder.build()
    return weakref.ref(target)


def uncalled_cycle():
    target = Target()
    builder = Builder(target)
    builder.add()
    return weakref.ref(target)


def closure_self_cycle():
    target = Target()

    def f():
        return target, f

    f()
    f()
    return weakref.ref(target)


for make in (uncalled_cycle, called_cycle, closure_self_cycle):
    ref = make()
    gc.collect()
    assert ref() is None, f"{make.__name__}: cycle through closure cells survived gc.collect()"


# A closure still running during the collection keeps its cells: the frame
# shares the cached vector, and its handles are external roots.
def running_closure():
    box = [Target()]
    ref = weakref.ref(box[0])

    def inner():
        gc.collect()
        return box[0]

    assert inner() is not None
    assert ref() is not None
    return ref


ref = running_closure()
gc.collect()
assert ref() is None

print("ok")
