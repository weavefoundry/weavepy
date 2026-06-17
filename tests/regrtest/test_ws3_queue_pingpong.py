"""RFC 0039 WS3 — blocking `queue.Queue` ping-pong across OS threads.

`Queue.get`/`put` block on real `Condition`s (they no longer raise
`Empty`/`Full` under the old single-threaded shim), so a bounded queue can
back-pressure a producer against a consumer and `join`/`task_done` settle to
zero outstanding tasks.
"""

import queue
import threading


# ---------------------------------------------------------------------------
# Bounded queue: producer outruns the consumer, so `put` must block on a full
# queue until the consumer drains it. All items arrive in order.
# ---------------------------------------------------------------------------

N = 50
q = queue.Queue(maxsize=4)
received = []


def consumer():
    while True:
        item = q.get()
        if item is None:
            q.task_done()
            break
        received.append(item)
        q.task_done()


c = threading.Thread(target=consumer)
c.start()

for i in range(N):
    q.put(i)
q.put(None)  # sentinel

q.join()  # blocks until every task_done() has landed
c.join()

assert received == list(range(N)), received


# ---------------------------------------------------------------------------
# Two-way ping-pong: bounded request/response queues bounce a token back and
# forth a fixed number of times.
# ---------------------------------------------------------------------------

req = queue.Queue(maxsize=1)
resp = queue.Queue(maxsize=1)
ROUNDS = 20


def responder():
    while True:
        token = req.get()
        if token is None:
            break
        resp.put(token + 1)


r = threading.Thread(target=responder)
r.start()

value = 0
for _ in range(ROUNDS):
    req.put(value)
    value = resp.get()
req.put(None)
r.join()
assert value == ROUNDS, value


# ---------------------------------------------------------------------------
# LifoQueue / PriorityQueue ordering + SimpleQueue basics.
# ---------------------------------------------------------------------------

lifo = queue.LifoQueue()
for i in range(5):
    lifo.put(i)
assert [lifo.get() for _ in range(5)] == [4, 3, 2, 1, 0]

pq = queue.PriorityQueue()
for item in (3, 1, 4, 1, 5, 9, 2):
    pq.put(item)
assert [pq.get() for _ in range(7)] == [1, 1, 2, 3, 4, 5, 9]

sq = queue.SimpleQueue()
sq.put("a")
sq.put("b")
assert sq.get() == "a"
assert sq.get() == "b"
assert sq.empty()


# ---------------------------------------------------------------------------
# Non-blocking get on an empty queue raises Empty; non-blocking put on a full
# queue raises Full.
# ---------------------------------------------------------------------------

empty_q = queue.Queue()
try:
    empty_q.get_nowait()
    raise AssertionError("get_nowait on empty queue should raise Empty")
except queue.Empty:
    pass

full_q = queue.Queue(maxsize=1)
full_q.put(1)
try:
    full_q.put_nowait(2)
    raise AssertionError("put_nowait on full queue should raise Full")
except queue.Full:
    pass


print("WS3 blocking queue ping-pong ok")
