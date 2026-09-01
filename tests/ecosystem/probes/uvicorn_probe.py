"""Ecosystem probe: uvicorn (RFC 0076 WS5) — the deployment twin of
the gunicorn-gevent capstone: launch `uvicorn` as a real process
serving a FastAPI app (graduating the RFC 0060 row from `TestClient`
to the shape people actually deploy), poll readiness, drive
concurrent HTTP over loopback through the h11 protocol stack, assert
responses (JSON echo incl. a pydantic-validated POST), then SIGTERM
and assert a clean exit."""

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
import urllib.error
import urllib.request

scratch = tempfile.mkdtemp(prefix="weavepy_uvicorn_")

app_py = textwrap.dedent(
    """
    import os

    from fastapi import FastAPI
    from pydantic import BaseModel

    app = FastAPI()


    class Item(BaseModel):
        name: str
        qty: int


    @app.get("/ping")
    def ping():
        return {"pid": os.getpid(), "ok": True}


    @app.post("/items")
    def create(item: Item):
        return {"name": item.name, "qty": item.qty * 2}


    @app.get("/async-ping")
    async def async_ping():
        return {"ok": True, "mode": "async"}
    """
)
with open(os.path.join(scratch, "probeapp.py"), "w", encoding="utf-8") as f:
    f.write(app_py)

probe_sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
probe_sock.bind(("127.0.0.1", 0))
port = probe_sock.getsockname()[1]
probe_sock.close()

server = subprocess.Popen(
    [
        sys.executable,
        "-m",
        "uvicorn",
        "--host",
        "127.0.0.1",
        "--port",
        str(port),
        # info: the shutdown assertion below greps the "Shutting down" /
        # "Finished server process" INFO lines (the exit code alone can't
        # distinguish graceful shutdown from an unhandled SIGTERM — both
        # are -15 since uvicorn 0.29 re-raises the captured signal).
        "--log-level",
        "info",
        "probeapp:app",
    ],
    cwd=scratch,
    stdout=subprocess.PIPE,
    stderr=subprocess.STDOUT,
)

try:
    # --- readiness poll ------------------------------------------------------
    url = f"http://127.0.0.1:{port}/ping"
    deadline = time.monotonic() + 120
    last_err = None
    while time.monotonic() < deadline:
        if server.poll() is not None:
            out = server.stdout.read().decode(errors="replace")
            raise AssertionError(
                f"uvicorn exited early rc={server.returncode}:\n{out}"
            )
        try:
            with urllib.request.urlopen(url, timeout=5) as resp:
                assert resp.status == 200
                break
        except Exception as e:  # noqa: BLE001 - readiness poll
            last_err = e
            time.sleep(0.25)
    else:
        raise AssertionError(f"uvicorn never became ready: {last_err!r}")

    # --- concurrent GETs through the h11 stack --------------------------------
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

    # --- pydantic-validated POST + async endpoint ------------------------------
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}/items",
        data=json.dumps({"name": "bolt", "qty": 21}).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        body = json.loads(resp.read().decode())
    assert body == {"name": "bolt", "qty": 42}, body

    # A validation failure must answer 422, not crash the worker.
    bad = urllib.request.Request(
        f"http://127.0.0.1:{port}/items",
        data=json.dumps({"name": "bolt", "qty": "not-a-number"}).encode(),
        headers={"Content-Type": "application/json"},
    )
    try:
        urllib.request.urlopen(bad, timeout=30)
        raise AssertionError("invalid POST did not 422")
    except urllib.error.HTTPError as e:
        assert e.code == 422, e.code

    with urllib.request.urlopen(
        f"http://127.0.0.1:{port}/async-ping", timeout=30
    ) as resp:
        body = json.loads(resp.read().decode())
    assert body == {"ok": True, "mode": "async"}, body

    # --- graceful shutdown ------------------------------------------------------
    # uvicorn 0.29+ deliberately *re-raises* the captured signal after
    # restoring the default handler (Server.capture_signals), so a
    # SIGTERM'd server exits with signal status -SIGTERM — on CPython
    # too. Graceful shutdown is proven by the "Shutting down" /
    # "Finished server process" log lines, not the exit code.
    server.send_signal(signal.SIGTERM)
    rc = server.wait(timeout=60)
    assert rc == -signal.SIGTERM, f"uvicorn SIGTERM exit code {rc}"
    out = server.stdout.read().decode()
    assert "Shutting down" in out and "Finished server process" in out, (
        f"no graceful shutdown in output:\n{out}"
    )
finally:
    if server.poll() is None:
        server.kill()
        server.wait(timeout=30)

print("uvicorn ok")
