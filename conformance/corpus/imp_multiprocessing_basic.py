"""RFC 0024 — `multiprocessing` module surface."""

import multiprocessing


print(isinstance(multiprocessing.cpu_count(), int))
print(multiprocessing.cpu_count() >= 1)


# Lock / RLock / Event
lock = multiprocessing.Lock()
lock.acquire()
lock.release()


rlock = multiprocessing.RLock()
rlock.acquire()
rlock.release()


event = multiprocessing.Event()
print(event.is_set())
event.set()
print(event.is_set())


# Queue
q = multiprocessing.Queue()
q.put(1)
q.put(2)
print(q.get())
print(q.get())


# Pipe
a, b = multiprocessing.Pipe()
a.send("hello")
print(b.recv())


# Pool
def double(x):
    return x * 2


with multiprocessing.Pool(2) as pool:
    print(pool.map(double, [1, 2, 3, 4]))


# Manager
m = multiprocessing.Manager()
d = m.dict()
d["a"] = 1
print(d["a"])


# current_process / active_children
print(multiprocessing.current_process().name)
print(isinstance(multiprocessing.active_children(), list))
