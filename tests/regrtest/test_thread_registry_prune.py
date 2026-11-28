"""Many short threads: the registry keeps only the ones still running.

Finished, joined threads leave the runtime's thread registry, so starting
thousands of threads doesn't make each start and join slower. Counts,
late joins, unjoined finished threads and daemons behave as before.
"""

import _thread
import threading
import time


def work(out, i):
    out.append(i)


out = []
for batch in range(20):
    ts = [threading.Thread(target=work, args=(out, batch * 50 + k)) for k in range(50)]
    for t in ts:
        t.start()
    for t in ts:
        t.join()
        t.join()
        assert not t.is_alive()
assert sorted(out) == list(range(1000))

# A thread nobody joins finishes and is reaped at the next join.
loose = threading.Thread(target=work, args=(out, -1))
loose.start()
while loose.is_alive():
    time.sleep(0.001)
later = threading.Thread(target=work, args=(out, -2))
later.start()
later.join()
loose.join()
assert out[-2:] in ([-1, -2], [-2, -1])

stop = threading.Event()
daemon = threading.Thread(target=stop.wait, daemon=True)
daemon.start()
assert _thread._count() >= 1 and threading.active_count() >= 2
stop.set()
daemon.join()
assert threading.active_count() == 1
print("ok")
