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
retained = [Node(i) for i in range(3000)]
watchers = []

def bench(n):
    global watchers
    for _ in range(n):
        watchers.clear()
        gc.collect(2)
        watchers = [weakref.ref(node, on_collect) for node in retained]
    return len(watchers)
