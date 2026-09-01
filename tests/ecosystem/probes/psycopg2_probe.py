"""Ecosystem probe: psycopg2 (RFC 0076 WS5) — the RFC 0072 deferral,
closed. The classic C extension over libpq, built from its sdist in
the offline lane (the RFC 0062 `no_binary` path drives a real
`pg_config`-configured C compile). Serverless like the psycopg v3
row: proves the compiled `_psycopg` module is live, composes SQL,
round-trips the adaptation layer, checks the error taxonomy, and
drives real libpq to an OperationalError against a closed loopback
port."""

import datetime
import faulthandler
import socket
import sys

faulthandler.enable()


def stage(name: str) -> None:
    print(f"[stage] {name}", file=sys.stderr, flush=True)


stage("import")
import psycopg2
import psycopg2.errors
import psycopg2.extensions
from psycopg2 import sql
from psycopg2.extensions import adapt

# The compiled C module must be the one live (there is no pure-Python
# fallback in psycopg2, so a successful import is itself the proof —
# but assert the C-level surface explicitly anyway).
stage("impl+libpq-version")
assert psycopg2.extensions.libpq_version() >= 90000, (
    psycopg2.extensions.libpq_version()
)
assert psycopg2._psycopg.__name__ == "psycopg2._psycopg"

# --- adaptation layer (C-implemented adapters) --------------------------------
stage("adapt")
assert adapt(42).getquoted() == b"42"
assert adapt(1.5).getquoted() == b"1.5"
assert adapt("we'avepy").getquoted() == b"'we''avepy'", (
    adapt("we'avepy").getquoted()
)
assert adapt(True).getquoted() == b"true"
assert adapt(None) is not None  # NoneAdapter exists
assert adapt(datetime.date(2026, 8, 28)).getquoted() == b"'2026-08-28'::date"
# Bare-comma join: adapter_list.c's list_quote writes `','` between
# elements (no space).
assert adapt([1, 2, 3]).getquoted() == b"ARRAY[1,2,3]", (
    adapt([1, 2, 3]).getquoted()
)

# --- SQL composition ------------------------------------------------------------
# Unlike psycopg v3, `Identifier.as_string` / `Literal.as_string` here
# require a live connection (C `quote_ident` / adapter `prepare`), so a
# serverless probe composes from the context-free pieces only.
stage("sql-compose")
composed = sql.SQL("select {}, {} from {}").format(
    sql.Placeholder(),
    sql.Placeholder("qty"),
    sql.SQL("tbl"),
)
text = composed.as_string(None)
assert text == "select %s, %(qty)s from tbl", text
ident = sql.Identifier("na me")
assert repr(ident) == "Identifier('na me')", repr(ident)
joined = sql.SQL(", ").join([sql.Placeholder(), sql.Placeholder()])
assert joined.as_string(None) == "%s, %s", joined.as_string(None)

# --- error taxonomy --------------------------------------------------------------
stage("error-taxonomy")
assert issubclass(psycopg2.errors.UniqueViolation, psycopg2.IntegrityError)
assert issubclass(psycopg2.IntegrityError, psycopg2.DatabaseError)
assert issubclass(psycopg2.OperationalError, psycopg2.DatabaseError)
assert psycopg2.errors.lookup("23505") is psycopg2.errors.UniqueViolation

# --- real libpq: connect to a closed loopback port must raise --------------------
stage("connect-refused")
probe = socket.socket()
probe.bind(("127.0.0.1", 0))
closed_port = probe.getsockname()[1]
probe.close()
try:
    psycopg2.connect(
        host="127.0.0.1", port=closed_port, dbname="nope", connect_timeout=1
    )
    raise AssertionError("connect to closed port did not raise")
except psycopg2.OperationalError:
    pass

stage("done")
print("psycopg2 ok", psycopg2.__version__)
