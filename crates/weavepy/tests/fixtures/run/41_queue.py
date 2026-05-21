# `queue` — FIFO / LIFO / priority queues used as plain collections.

import queue


# FIFO
q = queue.Queue()
for i in range(5):
    q.put(i)
print("qsize:", q.qsize())
out = []
while not q.empty():
    out.append(q.get())
print(out)


# LIFO
lq = queue.LifoQueue()
for c in "abcd":
    lq.put(c)
out = []
while not lq.empty():
    out.append(lq.get())
print(out)


# Priority
pq = queue.PriorityQueue()
for v in (5, 1, 3, 2, 4):
    pq.put(v)
out = []
while not pq.empty():
    out.append(pq.get())
print(out)


# SimpleQueue
sq = queue.SimpleQueue()
sq.put("x")
sq.put("y")
print(sq.qsize(), sq.get(), sq.get(), sq.empty())


# Empty / Full on non-blocking variants
empty_q = queue.Queue()
try:
    empty_q.get_nowait()
except queue.Empty:
    print("empty ok")

full_q = queue.Queue(maxsize=1)
full_q.put(1)
try:
    full_q.put_nowait(2)
except queue.Full:
    print("full ok")
