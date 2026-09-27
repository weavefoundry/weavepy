"""Finalizers run once across concurrent reclamation and resurrection."""

import collections
import gc
import sys
import threading
import time


def check_threads():
    entered = []
    completed = []
    finalized = []
    errors = []
    rounds = 80
    depth = 60

    def sleeper():
        try:
            yield
        finally:
            time.sleep(0.000001)

    class Node(list):
        def __init__(self, children, token):
            self.token = token
            entered.append(token)
            self[:] = children
            self.complete = True
            completed.append(token)

        def __del__(self):
            finalized.append((getattr(self, "token", None), getattr(self, "complete", False)))
            generator = sleeper()
            next(generator)

    def worker(index):
        try:
            for iteration in range(rounds):
                node = Node([], (index, iteration, 0))
                for level in range(1, depth + 1):
                    node = [Node([node], (index, iteration, level))]
                del node
        except BaseException as error:
            errors.append((type(error).__name__, str(error)))

    interval = sys.getswitchinterval()
    sys.setswitchinterval(0.00001)
    try:
        threads = [threading.Thread(target=worker, args=(index,)) for index in range(2)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
    finally:
        sys.setswitchinterval(interval)
    gc.collect()
    assert not errors, errors
    expected = 2 * rounds * (depth + 1)
    assert len(entered) == len(completed) == len(finalized) == expected
    counts = collections.Counter(entered)
    assert len(counts) == expected
    assert all(count == 1 for count in counts.values())
    assert collections.Counter(completed) == counts
    assert collections.Counter(token for token, _ in finalized) == counts
    assert all(complete for _, complete in finalized)


def check_resurrection():
    survivors = []
    calls = []

    class Node:
        def __del__(self):
            calls.append("finalized")
            survivors.append(self)

    node = Node()
    node.cycle = node
    del node
    gc.collect()
    assert calls == ["finalized"]
    assert len(survivors) == 1
    assert gc.is_finalized(survivors[0])
    survivors.clear()
    gc.collect()
    assert calls == ["finalized"]


check_threads()
check_resurrection()
print("finalizers run once")
