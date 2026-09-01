"""Ecosystem probe: celery (RFC 0076 WS5) — task round-trip over the
in-process `memory://` transport with the `cache+memory://` result
backend. Defines tasks, starts a real worker (`--pool=solo`) in a
thread, submits work through `delay()`/`apply_async`, asserts result
round-trips (including a raising task's exception surface), and shuts
the worker down cleanly. Pure-Python but concurrency-shaped: kombu's
event loop, billiard's shims, and vine promises all run."""

import faulthandler
import sys
import threading
import time

faulthandler.enable()


def stage(name: str) -> None:
    print(f"[stage] {name}", file=sys.stderr, flush=True)


stage("import")
import celery
from celery import Celery
from celery.result import allow_join_result

stage("app")
app = Celery(
    "weavepy_probe",
    broker="memory://",
    backend="cache+memory://",
)
app.conf.update(
    broker_connection_retry_on_startup=True,
    worker_hijack_root_logger=False,
    worker_redirect_stdouts=False,
    task_always_eager=False,
)


@app.task(name="probe.add")
def add(x, y):
    return x + y


@app.task(name="probe.boom")
def boom():
    raise ValueError("expected-boom")


# --- eager sanity first (no worker involved) ----------------------------------
stage("eager")
assert add.apply(args=(2, 3)).get() == 5

# --- real worker in a thread over the memory transport --------------------------
stage("worker-start")
worker = app.Worker(
    pool="solo",
    concurrency=1,
    loglevel="ERROR",
    without_heartbeat=True,
    without_gossip=True,
    without_mingle=True,
)
worker_thread = threading.Thread(target=worker.start, daemon=True)
worker_thread.start()

# Readiness: poll until a trivial task round-trips (the worker needs a
# moment to build its consumer over the memory channel).
#
# Every `.get()` below runs under `allow_join_result()`: the in-process
# worker's Pool component sets the *process-global*
# `_task_join_will_block` flag when it starts (solo pool blocks on
# join), after which a bare `result.get()` anywhere in the process —
# main thread included — raises "Never call result.get() within a
# task!". Celery's own testing harness
# (`celery.contrib.testing.worker.start_worker`) resets the same flag
# for exactly this worker-in-a-thread shape; CPython behaves
# identically.
stage("first-result")
deadline = time.monotonic() + 60
result = None
while time.monotonic() < deadline:
    try:
        with allow_join_result():
            result = add.delay(19, 23).get(timeout=10)
        break
    except Exception:  # noqa: BLE001 - readiness poll
        time.sleep(0.5)
assert result == 42, result

# --- a burst of tasks ------------------------------------------------------------
stage("burst")
asyncs = [add.apply_async(args=(i, i * 2)) for i in range(10)]
with allow_join_result():
    values = [r.get(timeout=30) for r in asyncs]
assert values == [i * 3 for i in range(10)], values

# --- exception round-trip through the result backend -----------------------------
stage("raising-task")
failed = boom.delay()
try:
    with allow_join_result():
        failed.get(timeout=30)
    raise AssertionError("raising task did not raise")
except ValueError as e:
    assert "expected-boom" in str(e), e
assert failed.state == "FAILURE", failed.state

# --- clean shutdown ---------------------------------------------------------------
stage("shutdown")
worker.stop()
worker_thread.join(timeout=30)
assert not worker_thread.is_alive(), "worker thread did not stop"

stage("done")
print("celery ok", celery.__version__)
