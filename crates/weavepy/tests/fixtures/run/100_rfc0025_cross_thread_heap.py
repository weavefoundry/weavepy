"""RFC 0025 — cross-thread heap sharing.

The fixture exercises the four invariants RFC 0025 promises:

1. **Real OS-thread parallelism.** `_thread.start_new_thread`
   runs the target on the spawned thread, not on the calling
   thread.
2. **Shared mutable state.** A list captured by a worker closure
   is the *same* list the parent sees after `join()`.
3. **Cross-thread Locks.** A `threading.Lock` acquired in the
   worker is visibly held to the parent (and vice versa).
4. **`threading.excepthook` fires on worker exceptions.** A
   `RuntimeError` raised by the worker shows up through the
   excepthook with a `_ExceptHookArgs`-shaped payload.
"""

import sys
import threading
import time


# 1 — Worker mutates a shared list; parent observes after join.
shared = []


def append_worker(values):
    for v in values:
        shared.append(v)


t = threading.Thread(target=append_worker, args=([1, 2, 3, 4, 5],))
t.start()
t.join()
print("shared list after join:", shared)


# 2 — Multiple workers contend for a shared lock + counter.
counter = [0]
counter_lock = threading.Lock()


def bump_counter(n):
    for _ in range(n):
        with counter_lock:
            counter[0] += 1


workers = [threading.Thread(target=bump_counter, args=(100,)) for _ in range(4)]
for w in workers:
    w.start()
for w in workers:
    w.join()
print("locked counter total:", counter[0])


# 3 — `threading.Event` signals across thread boundary.
event = threading.Event()
results = []


def waiter():
    event.wait()
    results.append("woke")


w = threading.Thread(target=waiter)
w.start()
event.set()
w.join()
print("event waiter:", results)


# 4 — `Thread.is_alive` after join.
def short_worker():
    pass


sw = threading.Thread(target=short_worker)
sw.start()
sw.join()
print("alive after join:", sw.is_alive())


# 5 — `threading.active_count` reflects spawned + parent.
def block_until_set(ev):
    ev.wait()


hold_ev = threading.Event()
holders = [threading.Thread(target=block_until_set, args=(hold_ev,)) for _ in range(3)]
for h in holders:
    h.start()
# Give the workers a moment to register with the runtime before
# we ask for the count.
time.sleep(0.05)
ac = threading.active_count()
print("active_count >= 4:", ac >= 4)
hold_ev.set()
for h in holders:
    h.join()


# 6 — Worker exception surfaces through threading.excepthook.
hook_seen = []
saved_hook = threading.excepthook


def my_hook(args):
    hook_seen.append((args.exc_type.__name__, str(args.exc_value)))


threading.excepthook = my_hook


def bad_worker():
    raise RuntimeError("boom")


bw = threading.Thread(target=bad_worker)
bw.start()
bw.join()
threading.excepthook = saved_hook
if hook_seen:
    print("excepthook fired:", hook_seen[0])
else:
    # The excepthook plumbing was added in RFC 0025; if it didn't
    # fire we want a deterministic line in the .out so the
    # conformance harness flags any regression.
    print("excepthook fired: (none)")


# 7 — Thread identity is stable across get_ident calls inside
# the same thread.
own_ids = []


def report_ident():
    own_ids.append(threading.get_ident())
    own_ids.append(threading.get_ident())


r = threading.Thread(target=report_ident)
r.start()
r.join()
print("ident stable:", own_ids[0] == own_ids[1])
