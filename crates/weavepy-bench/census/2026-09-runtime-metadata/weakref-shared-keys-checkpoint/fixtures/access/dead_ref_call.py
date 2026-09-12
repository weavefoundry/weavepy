import gc
import weakref
gc.disable()

class Node:
    __slots__ = ('value', '__weakref__')
    def __init__(self, value):
        self.value = value

class CallableNode(Node):
    __slots__ = ()
    def __call__(self, extra):
        return self.value + extra

callbacks = 0
def on_collect(ref):
    global callbacks
    assert ref() is None
    callbacks += 1
target = Node(7)
ref = weakref.ref(target)
del target
gc.collect(2)
assert ref() is None

def bench(n):
    total = 0
    for _ in range(n):
        total += int(ref() is None)
    return total
