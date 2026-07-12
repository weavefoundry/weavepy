"""RFC 0050 WS6 — stdio error-handler matrix + WTF-8 argv round-trip.

Spawns child interpreters (sys.executable) to pin the PEP 540/538
startup surface WS5 implemented: PYTHONUTF8 precedence, the stdio
error-handler defaults, PYTHONIOENCODING normalization, and PEP 383
surrogateescape argv round-tripping of non-UTF-8 bytes.
"""

import os
import subprocess
import sys

PY = sys.executable


def run(args, env_extra=None, expect_fail=False):
    env = dict(os.environ)
    env.pop("PYTHONUTF8", None)
    env.pop("PYTHONIOENCODING", None)
    if env_extra:
        env.update(env_extra)
    proc = subprocess.run(
        [PY, *args], env=env, capture_output=True, text=True, timeout=60
    )
    if expect_fail:
        assert proc.returncode != 0, (args, proc.returncode, proc.stderr)
        return proc.stderr.strip()
    assert proc.returncode == 0, (args, proc.returncode, proc.stderr)
    return proc.stdout.strip()


FLAG = "import sys; print(sys.flags.utf8_mode)"
STDIO = (
    "import sys;"
    "print(sys.stdin.encoding, sys.stdin.errors,"
    " sys.stdout.encoding, sys.stdout.errors,"
    " sys.stderr.encoding, sys.stderr.errors)"
)

# PYTHONUTF8 drives sys.flags.utf8_mode; -X utf8 wins; -E ignores the env.
assert run(["-c", FLAG], {"PYTHONUTF8": "1"}) == "1"
assert run(["-c", FLAG], {"PYTHONUTF8": "0"}) == "0"
assert run(["-X", "utf8=0", "-c", FLAG], {"PYTHONUTF8": "1"}) == "0"
assert run(["-X", "utf8", "-c", FLAG], {"PYTHONUTF8": "0"}) == "1"
err = run(["-c", FLAG], {"PYTHONUTF8": "bogus"}, expect_fail=True)
assert "invalid PYTHONUTF8 environment variable value" in err, err
# The C/POSIX locale enables the mode by default (PEP 540).
assert run(["-c", FLAG], {"LC_ALL": "C", "PYTHONUTF8": ""}) == "1"

# Stdio error handlers under UTF-8 mode: stdin/stdout surrogateescape,
# stderr always backslashreplace.
out = run(["-X", "utf8", "-c", STDIO], {"PYTHONIOENCODING": ""})
assert out.split() == [
    "utf-8", "surrogateescape",
    "utf-8", "surrogateescape",
    "utf-8", "backslashreplace",
], out

# PYTHONIOENCODING beats UTF-8 mode; the encoding half is reported under
# its canonical codec name and forces strict on stdin/stdout — stderr
# stays backslashreplace.
out = run(["-X", "utf8", "-c", STDIO], {"PYTHONIOENCODING": "latin1"})
assert out.split() == [
    "iso8859-1", "strict",
    "iso8859-1", "strict",
    "iso8859-1", "backslashreplace",
], out

# An errors-only spec (":handler") applies to stdin/stdout only.
out = run(["-X", "utf8", "-c", STDIO], {"PYTHONIOENCODING": ":namereplace"})
assert out.split() == [
    "utf-8", "namereplace",
    "utf-8", "namereplace",
    "utf-8", "backslashreplace",
], out

# ---------------------------------------------------------------------------
# WTF-8 / PEP 383: a non-UTF-8 argv byte round-trips through the child's
# sys.argv (as a lone surrogate) back to the same OS bytes on stdout.
# ---------------------------------------------------------------------------

env = dict(os.environ)
proc = subprocess.run(
    [PY.encode(), b"-c",
     b"import sys; sys.stdout.buffer.write(sys.argv[1].encode('utf-8', 'surrogateescape'))",
     b"a\xffb"],
    env=env, capture_output=True, timeout=60,
)
assert proc.returncode == 0, proc.stderr
assert proc.stdout == b"a\xffb", proc.stdout

# The same escape must be visible as a lone surrogate inside the child.
proc = subprocess.run(
    [PY.encode(), b"-c", b"import sys; print(ascii(sys.argv[1]))", b"a\xffb"],
    env=env, capture_output=True, timeout=60,
)
assert proc.returncode == 0, proc.stderr
assert proc.stdout.decode().strip() == "'a\\udcffb'", proc.stdout

print("stdio matrix + wtf8 argv round-trip ok")
