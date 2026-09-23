"""A native version guard doesn't own an otherwise unreachable class."""
import gc
import weakref

SOURCES = {
    'dict': 'def driver(obj, n):\n    total = 0\n    for _ in range(n):\n        total += obj.value\n    return total\n',
    'slots': 'def driver(obj, n):\n    total = 0\n    for _ in range(n):\n        total += obj.value\n    return total\n',
    'store': 'def driver(obj, n):\n    for i in range(n):\n        obj.value = i\n',
    'method': 'def driver(obj, n):\n    total = 0\n    for _ in range(n):\n        total += obj.read()\n    return total\n',
}
callbacks = []


def make(kind):
    if kind == 'slots':
        class Temporary:
            __slots__ = ('value', '__weakref__')
    elif kind == 'method':
        class Temporary:
            def read(self):
                # Isolate the method guard from attribute guards in its callee.
                return 7
    else:
        class Temporary:
            pass
    obj = Temporary()
    obj.value = 7
    namespace = {}
    exec(SOURCES[kind], namespace)
    driver = namespace['driver']
    if kind == 'store':
        driver(obj, 10000)
        assert obj.value == 9999
    else:
        if kind == 'method':
            for _ in range(80):
                assert obj.read() == 7
        # Stay below the native driver's call-density retirement poll for
        # the tiny method. The lifetime check must retain its MethodEntry.
        count = 800 if kind == 'method' else 10000
        assert driver(obj, count) == 7 * count
    type_ref = weakref.ref(Temporary, lambda ref: callbacks.append(kind))
    return driver, type_ref, weakref.ref(obj)


# Independent, retained drivers ensure each case owns a compiled guard.
# Sharing one reader across cases would make later cases deopt immediately.
drivers = {}
for kind in SOURCES:
    driver, type_ref, instance_ref = make(kind)
    drivers[kind] = driver
    for _ in range(3):
        gc.collect()
    assert instance_ref() is None, kind
    assert type_ref() is None, kind
    assert callbacks[-1] == kind
assert callbacks == list(SOURCES)

# New classes can reuse old allocation addresses, but never guard tokens.
for kind, driver in drivers.items():
    for value in range(1, 9):
        class Replacement:
            def read(self):
                return self.value
        obj = Replacement()
        obj.padding = -1000
        obj.value = value
        if kind == 'store':
            driver(obj, 300)
            assert obj.value == 299
            assigned = []
            Replacement.value = property(lambda self: 101,
                                         lambda self, value: assigned.append(value))
            driver(obj, 300)
            assert assigned == list(range(300))
        else:
            assert driver(obj, 300) == 300 * value
            Replacement.value = property(lambda self: 101)
            assert driver(obj, 300) == 30300
            if kind == 'method':
                Replacement.read = lambda self: 103
                assert driver(obj, 300) == 30900

print('native class guard lifetime: ok')
