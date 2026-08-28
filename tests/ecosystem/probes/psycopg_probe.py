"""Ecosystem probe: psycopg v3 + psycopg-binary (PyPI wheels) —
serverless but behavior-asserting (RFC 0072 WS4): proves the Cython
binary implementation is live (not the pure-Python fallback), sanity-
checks libpq, composes SQL, round-trips the adaptation transformer,
parses conninfo, and drives real libpq to a connection-refused
OperationalError against a closed loopback port."""

import faulthandler
import socket
import sys

# Diagnostics for native-crash triage (the 2026-08 ubuntu-only
# `free(): invalid pointer` glibc abort): faulthandler dumps the
# Python stack on SIGABRT/SIGSEGV, and the flushed stage markers
# survive into the harness's failure reason (it keeps the last ~12
# output lines), pinpointing the dying block.
faulthandler.enable()


def stage(name: str) -> None:
    print(f"[stage] {name}", file=sys.stderr, flush=True)


stage("import")
import psycopg
from psycopg import pq, sql
from psycopg.adapt import PyFormat, Transformer
from psycopg.conninfo import conninfo_to_dict, make_conninfo

# The binary (Cython/libpq) implementation must be the one loaded.
stage("impl+libpq-version")
assert pq.__impl__ == "binary", pq.__impl__
assert pq.version() >= 100000, pq.version()

# SQL composition to string
stage("sql-compose")
composed = sql.SQL("select {}, {} from {}").format(
    sql.Literal(42),
    sql.Identifier("na me"),
    sql.Identifier("tbl"),
)
text = composed.as_string(None)
assert text == 'select 42, "na me" from "tbl"', text

# Adaptation-layer round-trip: dump python values to postgres wire
# format and load them back.
stage("transformer")
t = Transformer()
for value, expected_text in [(42, b"42"), (1.5, b"1.5"), ("weavepy", b"weavepy")]:
    dumper = t.get_dumper(value, PyFormat.TEXT)
    assert dumper.dump(value) == expected_text, (value, dumper.dump(value))
loader = t.get_loader(psycopg.postgres.types["int4"].oid, pq.Format.TEXT)
assert loader.load(b"123") == 123
loader = t.get_loader(psycopg.postgres.types["text"].oid, pq.Format.TEXT)
assert loader.load(b"abc") == "abc"

# conninfo assembly and parsing
stage("conninfo")
info = make_conninfo("dbname=app", user="alice", port=5433)
parsed = conninfo_to_dict(info)
assert parsed["dbname"] == "app" and parsed["user"] == "alice", parsed
assert parsed["port"] == "5433", parsed

# Real libpq exercise: a closed loopback port must raise
# OperationalError (not crash, not hang).
stage("connect-refused")
probe = socket.socket()
probe.bind(("127.0.0.1", 0))
closed_port = probe.getsockname()[1]
probe.close()
try:
    psycopg.connect(
        host="127.0.0.1", port=closed_port, dbname="nope", connect_timeout=1
    )
    raise AssertionError("connect to closed port did not raise")
except psycopg.OperationalError:
    pass

stage("done")
print("psycopg ok", psycopg.__version__, "libpq", pq.version())
