"""Ecosystem probe: gunicorn `-k gevent` capstone (RFC 0075 WS9) — the
default production topology RFC 0072 named: a pre-fork gunicorn master
(`os.fork`, SIGTERM/SIGQUIT discipline) running two gevent workers
(monkey.patch_all in the child) that serve a real Django WSGI app.

The probe writes a miniature Django project to a scratch dir, launches
`python -m gunicorn --workers 2 -k gevent` on a loopback port, polls
readiness, drives concurrent requests through the patched-socket path,
and asserts worker responses plus a clean SIGTERM shutdown."""

import json
import os
import signal
import socket
import subprocess
import sys
import tempfile
import textwrap
import threading
import time
import urllib.request

scratch = tempfile.mkdtemp(prefix="weavepy_gunicorn_")

# --- miniature Django WSGI project ------------------------------------------
# One module carries settings, a JSON view, urls, and the WSGI callable;
# the view answers with the worker pid so the assertion below can prove
# more than one pre-forked worker actually served traffic.
app_py = textwrap.dedent(
    """
    import os

    from django.conf import settings

    settings.configure(
        DEBUG=False,
        SECRET_KEY="probe-secret",
        ALLOWED_HOSTS=["*"],
        ROOT_URLCONF=__name__,
        USE_TZ=True,
    )

    import django

    django.setup()

    from django.http import JsonResponse
    from django.urls import path


    def ping(request):
        return JsonResponse({"pid": os.getpid(), "ok": True})


    urlpatterns = [path("ping/", ping)]

    from django.core.wsgi import get_wsgi_application

    application = get_wsgi_application()
    """
)
with open(os.path.join(scratch, "probeapp.py"), "w", encoding="utf-8") as f:
    f.write(app_py)

# A free loopback port (bind-then-release; gunicorn rebinds immediately).
probe_sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
probe_sock.bind(("127.0.0.1", 0))
port = probe_sock.getsockname()[1]
probe_sock.close()

env = dict(os.environ)
env["PYTHONPATH"] = scratch + os.pathsep + env.get("PYTHONPATH", "")

master = subprocess.Popen(
    [
        sys.executable,
        "-m",
        "gunicorn",
        "--workers",
        "2",
        "--worker-class",
        "gevent",
        "--bind",
        f"127.0.0.1:{port}",
        "--timeout",
        "60",
        "probeapp:application",
    ],
    env=env,
    cwd=scratch,
    stdout=subprocess.PIPE,
    stderr=subprocess.STDOUT,
)

try:
    # --- readiness poll ------------------------------------------------------
    url = f"http://127.0.0.1:{port}/ping/"
    deadline = time.monotonic() + 120
    last_err = None
    while time.monotonic() < deadline:
        if master.poll() is not None:
            out = master.stdout.read().decode(errors="replace")
            raise AssertionError(
                f"gunicorn master exited early rc={master.returncode}:\n{out}"
            )
        try:
            with urllib.request.urlopen(url, timeout=5) as resp:
                assert resp.status == 200
                break
        except Exception as e:  # noqa: BLE001 - readiness poll
            last_err = e
            time.sleep(0.25)
    else:
        raise AssertionError(f"gunicorn never became ready: {last_err!r}")

    # --- concurrent requests through the gevent workers ----------------------
    results = []
    errors = []

    def hit():
        try:
            with urllib.request.urlopen(url, timeout=30) as resp:
                body = json.loads(resp.read().decode())
                assert resp.status == 200 and body["ok"] is True, body
                results.append(body["pid"])
        except Exception as e:  # noqa: BLE001 - collected and asserted below
            errors.append(e)

    threads = [threading.Thread(target=hit) for _ in range(16)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=60)
    assert not errors, errors
    assert len(results) == 16, f"only {len(results)}/16 responses"
    worker_pids = set(results)
    assert all(pid != master.pid for pid in worker_pids), (
        "responses came from the master, not pre-forked workers"
    )

    # --- graceful shutdown ----------------------------------------------------
    master.send_signal(signal.SIGTERM)
    rc = master.wait(timeout=60)
    assert rc == 0, f"master SIGTERM exit code {rc}"
finally:
    if master.poll() is None:
        master.kill()
        master.wait(timeout=30)

print("gunicorn-gevent ok", len(worker_pids), "worker pid(s)")
