"""Clearing weakrefs without callbacks handles mixed and proxy-only watchers."""
import gc
import sys
import threading
import weakref

if sys.implementation.name == 'weavepy':
    from _testcapi import pyobject_clear_weakrefs_no_callbacks as clear
else:
    import ctypes
    clear = ctypes.pythonapi.PyUnstable_Object_ClearWeakRefsNoCallbacks
    clear.argtypes = [ctypes.py_object]
    clear.restype = None


class Target:
    value = 17


class CallableTarget(Target):
    def __call__(self):
        return self.value


class Ref(weakref.ref):
    pass


def exercise(proxy_only, callable_target, on_thread):
    target = CallableTarget() if callable_target else Target()
    events = []
    references = []
    proxies = []
    completed = []

    def prepare():
        if not proxy_only:
            references.append(weakref.ref(target))
            for index in range(6):
                factory = Ref if index % 2 else weakref.ref
                references.append(factory(target, lambda ref, index=index: events.append(index)))
        proxies.append(weakref.proxy(target, lambda ref: events.append('proxy')))
        assert proxies[0].value == 17
        if callable_target:
            assert proxies[0]() == 17
        completed.append(True)

    if on_thread:
        worker = threading.Thread(target=prepare)
        worker.start()
        worker.join()
    else:
        prepare()
    assert completed == [True]
    assert weakref.getweakrefcount(target) == len(references) + len(proxies)
    saved = [ref.__call__ for ref in references]
    clear(target)
    assert all(ref() is None for ref in references)
    assert all(method() is None for method in saved)
    for proxy in proxies:
        try:
            proxy.value
        except ReferenceError:
            pass
        else:
            raise AssertionError('proxy retained a cleared target')
    assert weakref.getweakrefs(target) == []
    assert weakref.getweakrefcount(target) == 0
    gc.collect()
    assert events == [], events
    # Repeated clearing is harmless, and the surviving target can be watched again.
    clear(target)
    renewed = weakref.ref(target, lambda ref: events.append('renewed'))
    assert renewed() is target
    del target
    gc.collect()
    assert renewed() is None
    assert events == ['renewed'], events
    assert all(method() is None for method in saved)


for proxy_only in (False, True):
    for callable_target in (False, True):
        for on_thread in (False, True):
            exercise(proxy_only, callable_target, on_thread)
clear(object())
clear(None)
print('mixed and proxy-only weakrefs clear without callbacks')
