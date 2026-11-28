"""Reference counts stay exact while several threads run.

With the GIL on, a thread's reference-count updates are plain (not
atomic) as long as every thread that touches an object holds the GIL:
workers take it before their first touch of an object and keep it through
their thread-local destructors, and a blocking call touches no object
while it has the GIL released. A count that lost an update would free an
object early (a crash, or another object at its address) or never; the
tests below make threads share objects through every kind of hand-off and
check each count and each object's death afterwards.

The same checks run again with `WEAVEPY_ATOMIC_COUNTS=1`, which makes
counts atomic as soon as a second thread exists.
"""

import gc
import os
import queue
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import weakref

THREADS = 6
ROUNDS = 3000


class Token:
    """A heap object with a weak reference slot and a visible death."""

    died = 0
    lock = threading.Lock()

    def __init__(self, tag):
        self.tag = tag

    def __del__(self):
        with Token.lock:
            Token.died += 1


def fast_switches(fn):
    def run(self):
        before = sys.getswitchinterval()
        sys.setswitchinterval(1e-5)
        try:
            fn(self)
        finally:
            sys.setswitchinterval(before)

    run.__name__ = fn.__name__
    return run


def run_threads(target, n=THREADS, args=()):
    errors = []

    def body(k):
        try:
            target(k, *args)
        except BaseException as e:  # reported by the starting thread
            errors.append(repr(e))

    ts = [threading.Thread(target=body, args=(k,)) for k in range(n)]
    for t in ts:
        t.start()
    for t in ts:
        t.join()
    return errors


class SharedCountTests(unittest.TestCase):
    @fast_switches
    def test_shared_objects_keep_their_counts(self):
        shared = [Token(i) for i in range(8)]
        text = "shared text %d" % len(shared)
        big = 10**30
        tup = (text, big, shared[0])
        before = [sys.getrefcount(o) for o in shared]
        before_text = sys.getrefcount(text)
        before_big = sys.getrefcount(big)
        before_tup = sys.getrefcount(tup)

        def churn(k):
            mine = []
            d = {}
            for i in range(ROUNDS):
                o = shared[(i + k) % len(shared)]
                mine.append(o)
                mine.append(text)
                mine.append(big)
                d[i % 7] = (o, tup, text)
                a, b, c = tup
                if len(mine) > 30:
                    del mine[:15]
                x = [o, a, b, c][i % 4]
                del x
            del mine, d

        self.assertEqual(run_threads(churn), [])
        self.assertEqual([sys.getrefcount(o) for o in shared], before)
        self.assertEqual(sys.getrefcount(text), before_text)
        self.assertEqual(sys.getrefcount(big), before_big)
        self.assertEqual(sys.getrefcount(tup), before_tup)

    @fast_switches
    def test_objects_die_exactly_once(self):
        with Token.lock:
            start = Token.died
        made = [0] * THREADS
        refs = [[] for _ in range(THREADS)]

        def make_and_drop(k):
            keep = []
            for i in range(ROUNDS):
                t = Token((k, i))
                made[k] += 1
                if i % 50 == 0:
                    refs[k].append(weakref.ref(t))
                keep.append(t)
                if len(keep) > 20:
                    keep.pop(0)
                del t
            del keep

        self.assertEqual(run_threads(make_and_drop), [])
        gc.collect()
        with Token.lock:
            died = Token.died - start
        self.assertEqual(died, sum(made))
        self.assertTrue(all(r() is None for rs in refs for r in rs))

    @fast_switches
    def test_queue_hand_off(self):
        q = queue.Queue(maxsize=16)
        out = queue.SimpleQueue()
        n_items = 2000
        payload = Token("payload")
        before = sys.getrefcount(payload)

        def producer(k):
            for i in range(n_items):
                q.put((k, i, payload, [payload, "item %d" % i]))

        def consumer(k):
            total = 0
            while True:
                item = q.get()
                if item is None:
                    break
                total += item[1]
                self.assertIs(item[2], payload)
                self.assertIs(item[3][0], payload)
            out.put(total)

        consumers = [threading.Thread(target=consumer, args=(k,)) for k in range(3)]
        for t in consumers:
            t.start()
        self.assertEqual(run_threads(producer, n=3), [])
        for _ in consumers:
            q.put(None)
        for t in consumers:
            t.join()
        total = sum(out.get() for _ in consumers)
        self.assertEqual(total, 3 * sum(range(n_items)))
        self.assertEqual(sys.getrefcount(payload), before)

    @fast_switches
    def test_blocking_calls_beside_churn(self):
        shared = [Token(i) for i in range(4)]
        before = [sys.getrefcount(o) for o in shared]
        stop = threading.Event()
        lock = threading.Lock()
        cond = threading.Condition()
        a, b = socket.socketpair()
        tmp = tempfile.NamedTemporaryFile(delete=False)
        tmp.close()

        def churn(k):
            mine = []
            while not stop.is_set():
                mine.extend(shared)
                if len(mine) > 40:
                    del mine[:20]

        def blockers(k):
            for i in range(150):
                o = shared[i % 4]
                with lock:
                    held = [o, o]
                time.sleep(0)
                time.sleep(0.0002)
                if k == 0:
                    a.sendall(b"x" * 64)
                    self.assertEqual(len(b.recv(64)), 64)
                with open(tmp.name, "ab") as f:
                    f.write(b"line %d\n" % i)
                with cond:
                    cond.wait(0.0001)
                    cond.notify_all()
                with open(tmp.name, "rb") as f:
                    f.read()
                del held, o

        churners = [threading.Thread(target=churn, args=(k,)) for k in range(2)]
        for t in churners:
            t.start()
        try:
            self.assertEqual(run_threads(blockers, n=4), [])
        finally:
            stop.set()
            for t in churners:
                t.join()
            a.close()
            b.close()
            os.unlink(tmp.name)
        self.assertEqual([sys.getrefcount(o) for o in shared], before)

    @fast_switches
    def test_thread_locals_and_results_released_at_exit(self):
        with Token.lock:
            start = Token.died
        local = threading.local()
        cache_hits = []

        def body(k):
            local.token = Token(("local", k))
            local.items = [Token(("item", k, i)) for i in range(5)]
            # Caches a thread keeps for itself (formats, interned names,
            # attribute memos) are filled here and released at its exit.
            import struct

            cache_hits.append(struct.pack("<%dh" % (k + 1), *range(k + 1)))
            cache_hits.append(getattr(Token, "died") >= 0)
            cache_hits.append(sys.intern("name_%d" % k))

        for wave in range(20):
            self.assertEqual(run_threads(body, n=4), [])
        gc.collect()
        with Token.lock:
            died = Token.died - start
        self.assertEqual(died, 20 * 4 * 6)

    @fast_switches
    def test_threads_started_by_threads(self):
        shared = Token("deep")
        before = sys.getrefcount(shared)
        results = []

        def leaf(depth, bag):
            bag.append(shared)
            results.append(depth)

        def node(depth):
            bag = [shared]
            kids = [
                threading.Thread(target=node if depth < 3 else leaf, args=(depth + 1,) if depth < 3 else (depth + 1, bag))
                for _ in range(2)
            ]
            for t in kids:
                t.start()
            for t in kids:
                t.join()
            del bag

        top = threading.Thread(target=node, args=(0,))
        top.start()
        top.join()
        self.assertEqual(len(results), 16)
        self.assertEqual(sys.getrefcount(shared), before)

    def test_counts_after_the_threads_are_gone(self):
        # Once every worker is joined the program runs as it did before
        # any started; a fresh round of threads starts from there again.
        shared = Token("again")
        before = sys.getrefcount(shared)
        for _ in range(5):

            def body(k):
                xs = [shared] * 100
                del xs

            self.assertEqual(run_threads(body, n=3), [])
            ys = [shared for _ in range(1000)]
            self.assertEqual(sys.getrefcount(shared), before + 1000)
            del ys
            self.assertEqual(sys.getrefcount(shared), before)


DAEMON_AT_EXIT = r"""
import sys, threading, time
shared = [object() for _ in range(50)]
started = threading.Event()
def spin():
    mine = []
    started.set()
    while True:
        mine.extend(shared)
        if len(mine) > 500:
            del mine[:250]
def sleeper():
    while True:
        time.sleep(0.001)
for _ in range(3):
    threading.Thread(target=spin, daemon=True).start()
threading.Thread(target=sleeper, daemon=True).start()
started.wait()
time.sleep(0.05)
class Loud:
    def __del__(self):
        sys.stdout.write("finalized\n")
keep = Loud()
if "raise" in sys.argv:
    raise SystemExit("exit message %d" % len(shared))
print("main done", len(shared))
"""


class ExitTests(unittest.TestCase):
    """The main thread's last steps beside threads that are still running."""

    def run_child(self, *argv, env=None):
        return subprocess.run(
            [sys.executable, "-c", DAEMON_AT_EXIT, *argv],
            capture_output=True,
            text=True,
            timeout=120,
            env=env,
        )

    def test_exit_with_daemon_threads_running(self):
        for _ in range(5):
            r = self.run_child()
            self.assertEqual(r.returncode, 0, r.stderr[-2000:])
            self.assertIn("main done 50", r.stdout)

    def test_system_exit_with_daemon_threads_running(self):
        for _ in range(5):
            r = self.run_child("raise")
            self.assertEqual(r.returncode, 1, r.stderr[-2000:])
            self.assertIn("exit message 50", r.stderr)


class AtomicCountTests(unittest.TestCase):
    """The same checks with counts atomic once a second thread exists."""

    @unittest.skipIf(os.environ.get("WEAVEPY_ATOMIC_COUNTS"), "the atomic run itself")
    def test_with_atomic_counts(self):
        env = dict(os.environ, WEAVEPY_ATOMIC_COUNTS="1")
        r = subprocess.run(
            [sys.executable, __file__, "SharedCountTests", "ExitTests"],
            env=env,
            capture_output=True,
            text=True,
            timeout=600,
        )
        self.assertEqual(r.returncode, 0, r.stderr[-3000:])
        self.assertIn("OK", r.stderr)


if __name__ == "__main__":
    unittest.main()
