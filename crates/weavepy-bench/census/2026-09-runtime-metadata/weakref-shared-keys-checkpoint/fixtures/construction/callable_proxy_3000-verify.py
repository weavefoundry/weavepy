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
retained = [CallableNode(i) for i in range(3000)]
watchers = []

def bench(n):
    global watchers
    for _ in range(n):
        watchers.clear()
        gc.collect(2)
        watchers = [weakref.proxy(node) for node in retained]
    return len(watchers)

import json
for stage in range(2):
    if stage:
        bench(5)
    assert bench(2) == 3000
    assert watchers[1500].value == retained[1500].value
    assert callbacks == 0
    print(json.dumps([len(retained), len(watchers), callbacks]))
