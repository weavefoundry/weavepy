"""Exercise weak-reference cache changes and finalization enrollment."""
import gc
import threading
import weakref


def churn():
    for index in range(256):
        temporary = [index]
        temporary.clear()


def callback_changes():
    callbacks = []

    class Node:
        pass

    node = Node()
    # Register watchers before the first cycle-bearing mutation.
    watchers = [weakref.ref(node, lambda ref, tag=tag: callbacks.append(tag))
                for tag in range(3)]
    del watchers[-1]
    node.link = node
    aliases = [node] * 40
    churn()
    for generation in (0, 1, 2):
        gc.collect(generation)
        assert all(ref() is node for ref in watchers)
        assert not callbacks
    watchers.append(weakref.ref(node, lambda ref: callbacks.append(99)))
    aliases.clear()
    churn()
    assert all(ref() is node for ref in watchers)
    assert not callbacks
    del node
    gc.collect(2)
    assert all(ref() is None for ref in watchers)
    assert sorted(callbacks) == [0, 1, 99], callbacks
    gc.collect(2)
    assert sorted(callbacks) == [0, 1, 99], callbacks


def removed_callbacks_with_finalizer():
    callbacks = []
    finalized = []

    class Node:
        def __del__(self):
            finalized.append(self.tag)

    node = Node()
    node.tag = 'finalized once'
    aliases = [node] * 40
    watchers = [weakref.ref(node, lambda ref: callbacks.append('unexpected'))
                for _ in range(4)]
    churn()
    watchers.clear()
    aliases.clear()
    churn()
    assert not finalized and not callbacks
    node.link = node
    del node
    for _ in range(3):
        gc.collect(2)
    assert finalized == ['finalized once'], finalized
    assert not callbacks, callbacks


def concurrent_enrollment():
    callbacks = []
    watchers = []
    lock = threading.Lock()
    ready = threading.Barrier(3)

    class Node:
        pass

    node = Node()

    def register(worker):
        ready.wait()
        local = [weakref.ref(node, lambda ref, tag=(worker, i): callbacks.append(tag))
                 for i in range(32)]
        with lock:
            watchers.extend(local)

    workers = [threading.Thread(target=register, args=(worker,)) for worker in range(2)]
    for worker in workers:
        worker.start()
    ready.wait()
    for worker in workers:
        worker.join()
    node.link = node
    churn()
    gc.collect(2)
    assert len(watchers) == 64
    assert all(ref() is node for ref in watchers)
    assert not callbacks
    del node
    gc.collect(2)
    assert all(ref() is None for ref in watchers)
    assert sorted(callbacks) == [(worker, i) for worker in range(2) for i in range(32)]


was_enabled = gc.isenabled()
gc.disable()
try:
    callback_changes()
    removed_callbacks_with_finalizer()
    concurrent_enrollment()
finally:
    gc.collect(2)
    if was_enabled:
        gc.enable()
print('finalization cache lifecycle: ok')
