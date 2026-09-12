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
retained = [Node(i) for i in range(30000)]
watchers = [weakref.ref(node) for node in retained]

def bench(n):
    for _ in range(n):
        gc.collect(2)
    return n

import json
for stage in range(2):
    if stage:
        bench(5)
    assert bench(2) == 2
    assert len(retained) == 30000
    assert all(watchers[i]() is retained[i] for i in (0, 15000, 29999))
    assert callbacks == 0
    print(json.dumps([len(retained), len(watchers), retained[0].value, retained[15000].value, retained[-1].value, callbacks]))
