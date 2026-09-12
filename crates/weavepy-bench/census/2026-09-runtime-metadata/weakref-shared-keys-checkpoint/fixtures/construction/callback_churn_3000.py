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

def bench(n):
    before = callbacks
    for _ in range(n):
        nodes = [Node(i) for i in range(3000)]
        refs = [weakref.ref(node, on_collect) for node in nodes]
        nodes.clear()
        gc.collect(2)
        assert refs[0]() is None and refs[-1]() is None
    return callbacks - before
