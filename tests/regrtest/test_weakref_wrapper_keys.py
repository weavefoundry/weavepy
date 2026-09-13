"""Exercise weakref wrapper access and clearing across repeated allocations."""

import gc
import threading
import weakref


class Node:
    def __init__(self, value):
        self.value = value

    def method(self):
        return self.value


class CallableNode(Node):
    def __call__(self, extra):
        return self.value + extra


class Ref(weakref.ref):
    pass


def check_references():
    calls = []

    def callback(ref):
        assert ref() is None
        calls.append(ref)

    nodes = [Node(i) for i in range(257)]
    refs = [weakref.ref(node) for node in nodes]
    notified = [weakref.ref(node, callback) for node in nodes]
    subrefs = [Ref(node, callback) for node in nodes]
    hashes = [hash(ref) for ref in refs]
    for i in range(len(nodes)):
        assert refs[i]() is nodes[i]
        assert refs[i].__call__() is nodes[i]
        assert weakref.ref(nodes[i]) is refs[i]
        assert refs[i] == notified[i] == subrefs[i]
        assert notified[i].__callback__ is callback
        assert subrefs[i].__callback__ is callback
        subrefs[i].label = i
        assert subrefs[i].label == i
        assert hash(subrefs[i]) == hashes[i]
    gc.collect()
    assert not calls
    nodes.clear()
    gc.collect()
    assert len(calls) == 514
    for i in range(len(refs)):
        assert refs[i]() is None
        assert refs[i].__call__() is None
        assert notified[i]() is None
        assert subrefs[i]() is None
        assert notified[i].__callback__ is None
        assert subrefs[i].__callback__ is None
        assert hash(refs[i]) == hashes[i]
        assert hash(subrefs[i]) == hashes[i]
        assert refs[i] != notified[i]


def check_proxies_and_methods():
    target = Node(31)
    callable_target = CallableNode(47)
    proxy = weakref.proxy(target)
    callable_proxy = weakref.proxy(callable_target)
    method = weakref.WeakMethod(target.method)
    assert type(proxy) is weakref.ProxyType
    assert type(callable_proxy) is weakref.CallableProxyType
    for _ in range(25):
        assert proxy.value == 31
        assert proxy.method() == 31
        assert callable_proxy(5) == 52
        assert method()() == 31
    del target, callable_target
    gc.collect()
    assert method() is None
    try:
        proxy.value
    except ReferenceError:
        pass
    else:
        raise AssertionError("dead proxy retained its target")
    try:
        callable_proxy(5)
    except ReferenceError:
        pass
    else:
        raise AssertionError("dead callable proxy retained its target")


def check_weak_containers():
    nodes = [Node(i) for i in range(41)]
    values = weakref.WeakValueDictionary(enumerate(nodes))
    keys = weakref.WeakKeyDictionary((nodes[i], i) for i in range(41))
    members = weakref.WeakSet(nodes)
    for i in range(41):
        assert values[i] is nodes[i]
        assert keys[nodes[i]] == i
        assert nodes[i] in members
    nodes.clear()
    gc.collect()
    assert len(values) == len(keys) == len(members) == 0


def check_thread_access():
    target = Node(73)
    ref = weakref.ref(target)
    outcomes = []

    def worker():
        # Use an existing wrapper and create independent wrappers on each thread.
        assert ref() is target
        assert ref.__call__() is target
        local = Node(19)
        local_ref = weakref.ref(local)
        assert local_ref() is local
        outcomes.append(local_ref().value)

    for _ in range(5):
        thread = threading.Thread(target=worker)
        thread.start()
        thread.join()
    assert outcomes == [19] * 5
    assert ref() is target


check_references()
check_proxies_and_methods()
check_weak_containers()
check_thread_access()
print("weakref wrapper access ok")
