# `threading` — cooperative shim: Threads run synchronously, Locks /
# Events / Conditions / Semaphores all "work" because there's no
# concurrent contention in a single-threaded interpreter.

import threading


def worker(out, value):
    out.append(value)


out = []
threads = [threading.Thread(target=worker, args=(out, i)) for i in range(3)]
for t in threads:
    t.start()
for t in threads:
    t.join()
print(out)


lock = threading.Lock()
with lock:
    print("inside lock")
print("locked:", lock.locked())


rlock = threading.RLock()
rlock.acquire()
rlock.acquire()
rlock.release()
rlock.release()
print("rlock state:", rlock.locked())


ev = threading.Event()
print("set?", ev.is_set())
ev.set()
print("set?", ev.is_set())
ev.clear()
print("set?", ev.is_set())


sem = threading.Semaphore(2)
sem.acquire()
sem.acquire()
print("sem depleted")
sem.release()
sem.release()
print("sem refilled")


me = threading.current_thread()
print("main name:", me.name)
print("active:", threading.active_count())
print("ident type:", type(threading.get_ident()).__name__)
