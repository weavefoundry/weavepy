"""Buffered file objects keep CPython's observable behavior: line reads that
take a block at a time still report and restore the right position, mix with
sized reads, seeks, writes and the `.buffer` layer, and honor every newline
mode and `readline` limit; writes held in a buffer reach the file at flush,
close, seek, a read, a line end on a line-buffered stream, and at exit."""

import io
import os
import subprocess
import sys
import tempfile

d = tempfile.mkdtemp()
p = os.path.join(d, "f.txt")


def write(data, mode="wb"):
    with open(p, mode) as f:
        f.write(data)


# Lines, positions and mixed reads.
lines = [b"line %d\n" % i for i in range(5000)]
write(b"".join(lines))

with open(p, "rb") as f:
    assert list(f) == lines
with open(p, "rb") as f:
    assert f.readlines() == lines
with open(p) as f:
    assert [l.encode() for l in f] == lines

with open(p, "rb") as f:
    offsets = []
    while True:
        offsets.append(f.tell())
        if not f.readline():
            break
    want, at = [], 0
    for line in lines:
        want.append(at)
        at += len(line)
    want.append(at)
    assert offsets == want
    assert f.tell() == os.path.getsize(p)

with open(p, "rb") as f:
    assert f.readline() == lines[0]
    assert f.read(3) == lines[1][:3]
    assert f.readline() == lines[1][3:]
    assert f.tell() == len(lines[0]) + len(lines[1])
    f.seek(len(lines[0]))
    assert f.readline() == lines[1]
    f.seek(-len(lines[-1]), 2)
    assert f.readline() == lines[-1]
    assert f.readline() == b""
    f.seek(0)
    assert f.readline() == lines[0]
    f.seek(3, 1)
    assert f.readline() == lines[1][3:]
    rest = f.read()
    assert rest == b"".join(lines[2:])
    assert f.read() == b""

with open(p, "rb") as f:
    f.readline()
    buf = bytearray(6)
    assert f.readinto(buf) == 6 and bytes(buf) == lines[1][:6]
    assert f.peek(1)[:1] == lines[1][6:7]
    assert f.read1(1) == lines[1][6:7]

# The descriptor's offset runs ahead of a buffered reader, as under CPython;
# flush on a read/write stream gives it back.
with open(p, "rb") as f:
    f.readline()
    assert os.lseek(f.fileno(), 0, os.SEEK_CUR) > len(lines[0])
    assert f.tell() == len(lines[0])
with open(p, "rb", buffering=0) as f:
    assert f.readline() == lines[0]
    assert os.lseek(f.fileno(), 0, os.SEEK_CUR) == len(lines[0])
with open(p, "r+b") as f:
    f.readline()
    f.flush()
    assert os.lseek(f.fileno(), 0, os.SEEK_CUR) == len(lines[0])

# Text positions and seeks back to them.
with open(p) as f:
    f.readline()
    pos = f.tell()
    second = f.readline()
    f.seek(pos)
    assert f.readline() == second == lines[1].decode()

# Read, then write, on one stream.
write(b"one\ntwo\nthree\n")
with open(p, "r+b") as f:
    assert f.readline() == b"one\n"
    f.write(b"TWO\n")
    assert f.readline() == b"three\n"
with open(p, "rb") as f:
    assert f.read() == b"one\nTWO\nthree\n"
with open(p, "r+") as f:
    assert f.readline() == "one\n"
    pos = f.tell()
    f.seek(pos)
    f.write("2wo\n")
    f.seek(0)
    assert f.read() == "one\n2wo\nthree\n"
with open(p, "r+b") as f:
    f.readline()
    f.truncate()
    assert f.tell() == 4
assert os.path.getsize(p) == 4

# Newline modes.
raw = b"a\nb\rc\r\nd\r\re\n\nf"
write(raw)
with open(p) as f:
    assert list(f) == ["a\n", "b\n", "c\n", "d\n", "\n", "e\n", "\n", "f"]
    assert f.newlines == ("\r", "\n", "\r\n")
with open(p, newline="") as f:
    assert list(f) == ["a\n", "b\r", "c\r\n", "d\r", "\r", "e\n", "\n", "f"]
with open(p, newline="\n") as f:
    assert list(f) == ["a\n", "b\rc\r\n", "d\r\re\n", "\n", "f"]
with open(p, newline="\r") as f:
    assert list(f) == ["a\nb\r", "c\r", "\nd\r", "\r", "e\n\nf"]
with open(p, newline="\r\n") as f:
    assert list(f) == ["a\nb\rc\r\n", "d\r\re\n\nf"]
with open(p, "rb") as f:
    assert list(f) == [b"a\n", b"b\rc\r\n", b"d\r\re\n", b"\n", b"f"]

# A `\r\n` split across two blocks is still one line end.
for size in (8190, 8191, 8192, 8193, 131071, 131072, 131073):
    body = b"x" * (size - 1) + b"\r\n" + b"tail\r\n"
    write(body)
    with open(p, newline="") as f:
        got = list(f)
    assert got == ["x" * (size - 1) + "\r\n", "tail\r\n"], size
    with open(p) as f:
        got = list(f)
    assert got == ["x" * (size - 1) + "\n", "tail\n"], size
    with open(p, "rb") as f:
        assert [len(l) for l in f] == [size + 1, 6], size

# Limits.
write(b"abcdef\nxy\r\nz")
with open(p, "rb") as f:
    assert f.readline(0) == b""
    assert f.readline(3) == b"abc"
    assert f.readline(10) == b"def\n"
    assert f.readline(2) == b"xy"
    assert f.readline(-1) == b"\r\n"
    assert f.readline(5) == b"z"
    assert f.readline(5) == b""
with open(p) as f:
    assert f.readline(3) == "abc"
    assert f.readline() == "def\n"
    assert f.readline(3) == "xy\n"
    assert f.readline() == "z"

# In-memory streams.
s = io.StringIO("a\nbb\n\nccc")
assert list(s) == ["a\n", "bb\n", "\n", "ccc"]
s.seek(2)
assert s.readline(1) == "b" and s.readline() == "b\n" and s.tell() == 5
b = io.BytesIO(b"a\nbb\r\n\nccc")
assert list(b) == [b"a\n", b"bb\r\n", b"\n", b"ccc"]
b.seek(3)
assert b.readline(10) == b"b\r\n" and b.tell() == 6
assert io.StringIO("a\r\nb\rc", newline=None).readlines() == ["a\n", "b\n", "c"]
assert io.StringIO("a\r\nb\rc", newline="").readlines() == ["a\r\n", "b\r", "c"]
s = io.StringIO("héllo\nwörld\n")
assert s.readline() == "héllo\n" and s.tell() == 6 and s.readline() == "wörld\n"

# Pipes: a buffered reader takes what's there, an unbuffered one a line.
r, w = os.pipe()
os.write(w, b"first\nsecond\nthird")
os.close(w)
with open(r, "rb") as f:
    assert f.readline() == b"first\n"
    assert f.read(3) == b"sec"
    assert f.readline() == b"ond\n"
    assert f.read() == b"third"
r, w = os.pipe()
os.write(w, b"first\nsecond\n")
with open(r, "rb", buffering=0) as f:
    assert f.readline() == b"first\n"
    assert os.read(r, 100) == b"second\n"
os.close(w)
r, w = os.pipe()
os.write(w, b"a\r")
os.write(w, b"\nb\rc")
os.close(w)
with open(r) as f:
    assert list(f) == ["a\n", "b\n", "c"]

# Writes are held: nothing reaches the file before a flush.
with open(p, "w") as f:
    f.write("held")
    assert os.path.getsize(p) == 0
    assert f.tell() == 4
    f.flush()
    assert os.path.getsize(p) == 4
    f.write("\nmore\n")
    assert os.path.getsize(p) == 4
assert os.path.getsize(p) == 10

with open(p, "w", buffering=1) as f:
    assert f.line_buffering
    f.write("partial")
    assert os.path.getsize(p) == 0
    f.write(" line\nnext")
    assert os.path.getsize(p) == 17
with open(p, "w") as f:
    assert not f.line_buffering
    f.reconfigure(line_buffering=True)
    f.write("x\n")
    assert os.path.getsize(p) == 2
    f.reconfigure(line_buffering=False, write_through=True)
    f.write("y\n")
    assert os.path.getsize(p) == 2

with open(p, "w") as f:
    big = "z" * (io.DEFAULT_BUFFER_SIZE * 2 + 17)
    f.write(big)
    assert os.path.getsize(p) >= io.DEFAULT_BUFFER_SIZE
with open(p) as f:
    assert f.read() == big

# A read or a seek on a read/write stream sees what was written.
with open(p, "w+") as f:
    f.write("abc\ndef\n")
    f.seek(0)
    assert f.read() == "abc\ndef\n"
    f.write("ghi\n")
    f.seek(4)
    assert f.readline() == "def\n"
    assert f.readline() == "ghi\n"
with open(p, "w+b") as f:
    f.write(b"0123456789")
    f.seek(2)
    f.write(b"AB")
    assert f.tell() == 4
    f.seek(0)
    assert f.read() == b"01AB456789"
with open(p, "ab") as f:
    f.write(b"+")
    assert f.tell() == 11
with open(p, "rb") as f:
    assert f.read() == b"01AB456789+"

# The binary layer under a text stream.
with open(p, "w+") as f:
    f.buffer.write(b"under\n")
    f.seek(0)
    assert f.read() == "under\n"
    f.write("text\n")
    f.flush()
    f.buffer.write(b"again\n")
    f.buffer.flush()
    f.seek(0)
    assert f.read() == "under\ntext\nagain\n"
with open(p, "w") as f:
    f.write("text ")
    f.buffer.write(b"bytes ")
with open(p) as f:
    assert sorted(f.read().split()) == ["bytes", "text"]

spool = tempfile.SpooledTemporaryFile(mode="w+", max_size=10, encoding="utf-8")
spool.write("abc\n")
spool.write("def\nxyzzy\n")
spool.seek(0)
assert spool._rolled and spool.read() == "abc\ndef\nxyzzy\n"
spool.close()

# What a program wrote and never flushed reaches its files when it exits, but
# not past `os._exit`.
script = r"""
import os, sys
d = sys.argv[1]
f = open(os.path.join(d, "global.txt"), "w"); f.write("global\n")
def local():
    h = open(os.path.join(d, "local.bin"), "wb"); h.write(b"local\n")
    return h
keep = [local()]
class Holder: pass
c = Holder(); c.me = c; c.f = open(os.path.join(d, "cycle.txt"), "w"); c.f.write("cycle\n")
print("printed")
sys.stdout.write("written")
if sys.argv[2] == "hard":
    os._exit(0)
if sys.argv[2] == "exit":
    sys.exit(3)
"""
for how, code in (("end", 0), ("exit", 3), ("hard", 0)):
    for name in ("global.txt", "local.bin", "cycle.txt"):
        try:
            os.unlink(os.path.join(d, name))
        except FileNotFoundError:
            pass
    r = subprocess.run(
        [sys.executable, "-c", script, d, how], capture_output=True, text=True
    )
    assert r.returncode == code, (how, r)
    got = {}
    for name in ("global.txt", "local.bin", "cycle.txt"):
        with open(os.path.join(d, name)) as f:
            got[name] = f.read()
    if how == "hard":
        assert r.stdout == "" and set(got.values()) == {""}, (how, r.stdout, got)
    else:
        assert r.stdout == "printed\nwritten", (how, r.stdout)
        assert got == {
            "global.txt": "global\n",
            "local.bin": "local\n",
            "cycle.txt": "cycle\n",
        }, (how, got)

# Standard output into a pipe is held too: a write to the descriptor itself
# overtakes it. `-u` writes through.
script = r"""
import os, sys
print("one")
sys.stdout.write("two\n")
os.write(1, b"raw\n")
sys.stdout.flush()
print("three", flush=True)
os.write(1, b"raw2\n")
print("four")
sys.stdout.flush()
sys.stdout.buffer.write(b"five\n")
"""
r = subprocess.run([sys.executable, "-c", script], capture_output=True, text=True)
assert r.stdout == "raw\none\ntwo\nthree\nraw2\nfour\nfive\n", r.stdout
r = subprocess.run([sys.executable, "-u", "-c", script], capture_output=True, text=True)
assert r.stdout == "one\ntwo\nraw\nthree\nraw2\nfour\nfive\n", r.stdout

script = r"""
import sys
for line in sys.stdin:
    sys.stdout.write(line.upper())
print(repr(sys.stdin.readline()))
"""
r = subprocess.run(
    [sys.executable, "-c", script], input="a\nbb\r\nccc", capture_output=True, text=True
)
assert r.stdout == "A\nBB\nCCC''\n", r.stdout
script = r"""
import sys
first = sys.stdin.readline()
second = input()
rest = sys.stdin.read()
print(repr(first), repr(second), repr(rest))
"""
r = subprocess.run(
    [sys.executable, "-c", script], input="head\nbody\nmore\n", capture_output=True, text=True
)
assert r.stdout == "'head\\n' 'body' 'more\\n'\n", r.stdout

# Hot loops, where a call site settles on the stream's in-place steps: they
# and the general path agree, whichever serves a given line or write.
N = 30000
rows = ["row %d\n" % i for i in range(N)]
with open(p, "w") as f:
    w = f.write
    for row in rows:
        assert w(row) == len(row)
    assert f.tell() == sum(map(len, rows))
with open(p, "w") as f:
    for row in rows:
        f.write(row)
with open(p) as f:
    rl = f.readline
    got = []
    while True:
        line = rl()
        if not line:
            break
        got.append(line)
    assert got == rows and rl() == ""
with open(p) as f:
    got = []
    while line := f.readline():
        got.append(line)
    assert got == rows
with open(p) as f:
    n = 0
    for line in f:
        assert line == rows[n]
        n += 1
    assert n == N
    try:
        f.tell()
    except OSError:
        raise AssertionError("tell() works again once the loop is done")
with open(p, "rb") as f:
    n = 0
    for line in f:
        assert line == rows[n].encode()
        n += 1
    assert n == N and f.tell() == os.path.getsize(p)
with open(p, "rb") as f:
    offsets = []
    for _ in range(2000):
        f.readline()
        offsets.append(f.tell())
    assert offsets == [sum(map(len, rows[: k + 1])) for k in range(2000)]

with open(p, "wb") as f:
    w = f.write
    for row in rows:
        assert w(row.encode()) == len(row)
with open(p, "rb") as f:
    assert f.read() == "".join(rows).encode()

# Lines that aren't plain: other line ends, non-ASCII, and bad bytes, each
# in the middle of a run of plain ones.
mixed = []
for i in range(3000):
    mixed.append(("plain %d\n", "crlf %d\r\n", "h\u00e9llo %d \u4e16\u754c\n", "cr %d\r")[i % 4] % i)
write("".join(mixed).encode())
with open(p, encoding="utf-8") as f:
    got = list(f)
assert got == [m.replace("\r\n", "\n").replace("\r", "\n") for m in mixed]
with open(p, encoding="utf-8", newline="") as f:
    got = []
    while line := f.readline():
        got.append(line)
assert got == mixed
with open(p, encoding="utf-8") as f:
    seen = 0
    for line in f:
        seen += 1
    assert seen == len(mixed) and f.newlines == ("\r", "\n", "\r\n")

write(b"ok 1\nok 2\nbad \xff\nok 4\n")
with open(p, encoding="utf-8") as f:
    got = []
    try:
        for line in f:
            got.append(line)
    except UnicodeDecodeError:
        pass
    else:
        raise AssertionError("expected UnicodeDecodeError")
    # (CPython decodes a block at a time, so it raises before the first
    # line; a line at a time gets as far as the bad one.)
    assert got == ["ok 1\n", "ok 2\n"][: len(got)]
with open(p, encoding="utf-8", errors="surrogateescape") as f:
    assert list(f) == ["ok 1\n", "ok 2\n", "bad \udcff\n", "ok 4\n"]
with open(p, encoding="latin-1") as f:
    assert list(f) == ["ok 1\n", "ok 2\n", "bad \xff\n", "ok 4\n"]

# A stream's configuration changing under a settled site.
with open(p, "w", newline="") as f:
    w = f.write
    for _ in range(200):
        w("a\n")
    f.reconfigure(newline="\r\n")
    for _ in range(200):
        w("b\n")
    f.reconfigure(encoding="utf-16-le", newline="")
    for _ in range(200):
        w("c\n")
with open(p, "rb") as f:
    assert f.read() == b"a\n" * 200 + b"b\r\n" * 200 + "c\n".encode("utf-16-le") * 200

# A closed stream refuses, at a site that has been appending.
f = open(p, "w")
w = f.write
rl = f.readline
for _ in range(200):
    w("x")
f.close()
for call in (lambda: w("y"), lambda: f.write("y"), rl):
    try:
        call()
    except ValueError:
        pass
    else:
        raise AssertionError("expected ValueError")
assert os.path.getsize(p) == 200
write("".join(rows[:1000]).encode())
f = open(p)
it = iter(f)
rl = f.readline
for _ in range(100):
    next(it)
    rl()
f.close()
for call in (lambda: next(it), rl, f.readline):
    try:
        call()
    except ValueError:
        pass
    else:
        raise AssertionError("expected ValueError")

s = io.StringIO()
w = s.write
for row in rows:
    assert w(row) == len(row)
assert s.getvalue() == "".join(rows)
s.seek(0)
n = 0
for line in s:
    assert line == rows[n]
    n += 1
assert n == N
s.seek(0)
rl = s.readline
n = 0
while line := rl():
    n += 1
assert n == N
s.close()
for call in (lambda: w("q"), rl):
    try:
        call()
    except ValueError:
        pass
    else:
        raise AssertionError("expected ValueError")
b = io.BytesIO("".join(rows).encode())
n = 0
for line in b:
    assert line == rows[n].encode()
    n += 1
assert n == N

# Text reads by character count, whole characters only.
text = "".join(mixed[2::4])
write(text.encode())
with open(p, encoding="utf-8") as f:
    got = []
    while chunk := f.read(7):
        assert len(chunk) <= 7
        got.append(chunk)
    assert "".join(got) == text
with open(p, encoding="utf-8") as f:
    assert f.read(3) == text[:3]
    assert f.readline() == text[3 : text.index("\n") + 1]
    assert f.read(1) == text[text.index("\n") + 1]

# A sized read of standard input, then the rest of it: nothing read ahead
# for the first is lost to the second.
payload = "a" * 47 + "b" * 200000
for script in (
    "import sys; sys.stdout.write(sys.stdin.read(47)); sys.stdout.write(sys.stdin.read())",
    "import sys; sys.stdout.write(sys.stdin.readline()); sys.stdout.write(sys.stdin.read())",
    "import sys; b = sys.stdin.buffer; sys.stdout.buffer.write(b.read(47) + b.read())",
    "import sys; b = sys.stdin.buffer; sys.stdout.buffer.write(b.readline() + b.read())",
):
    for text in (payload, "one\n" + payload, "\n".join([payload[:50]] * 2000)):
        r = subprocess.run([sys.executable, "-c", script], input=text, capture_output=True, text=True)
        assert r.stdout == text, (script, len(r.stdout), len(text))

# `input()` flushes standard output before it reads, with or without a
# prompt, so a parent on the other end of the pipes sees what was printed.
child = subprocess.Popen(
    [sys.executable, "-c", "print('ready'); print(input().upper()); print(input())"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    text=True,
    bufsize=0,
)
assert child.stdout.readline() == "ready\n"
child.stdin.write("first\n")
assert child.stdout.readline() == "FIRST\n"
child.stdin.write("second\n")
assert child.communicate()[0] == "second\n"

for name in os.listdir(d):
    os.unlink(os.path.join(d, name))
os.rmdir(d)
print("ok")
