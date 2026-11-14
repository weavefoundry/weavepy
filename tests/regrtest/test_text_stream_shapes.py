"""Text streams at hot call sites, in every shape the in-place line and
write steps serve or decline: line ends of each kind under each newline
mode, the single-byte codecs, non-ASCII text, error handlers, and
`readline` limits. What comes back is what CPython's `TextIOWrapper` and
`StringIO` give."""

import io
import os
import tempfile

d = tempfile.mkdtemp()
p = os.path.join(d, "t.txt")
N = 3000


def put(data):
    with open(p, "wb") as f:
        f.write(data)


def lines_by_readline(**kw):
    out = []
    with open(p, **kw) as f:
        rl = f.readline
        while line := rl():
            out.append(line)
        seen = f.newlines
    return out, seen


def lines_by_method(**kw):
    out = []
    with open(p, **kw) as f:
        while line := f.readline():
            out.append(line)
    return out


def lines_by_loop(**kw):
    with open(p, **kw) as f:
        return list(f)


# Each kind of line end, under each newline mode.
unix = b"".join(b"row %d\n" % i for i in range(N))
dos = unix.replace(b"\n", b"\r\n")
mac = unix.replace(b"\n", b"\r")
mixed = b"".join((b"row %d\n", b"row %d\r\n", b"row %d\r")[i % 3] % i for i in range(N))
rows = ["row %d\n" % i for i in range(N)]
for data, kinds in ((unix, "\n"), (dos, "\r\n"), (mac, "\r"), (mixed, ("\r", "\n", "\r\n"))):
    put(data)
    got, seen = lines_by_readline()
    assert got == rows and seen == kinds, (kinds, seen)
    assert lines_by_method() == rows and lines_by_loop() == rows
    raw = data.decode()
    got, seen = lines_by_readline(newline="")
    assert "".join(got) == raw and len(got) == N and seen == kinds
    assert lines_by_loop(newline="") == got == lines_by_method(newline="")
    for nl in ("\n", "\r", "\r\n"):
        want = raw.split(nl)
        want = [w + nl for w in want[:-1]] + ([want[-1]] if want[-1] else [])
        got, _ = lines_by_readline(newline=nl)
        assert got == want, (kinds, nl)
        assert lines_by_loop(newline=nl) == want == lines_by_method(newline=nl)

# Single-byte codecs and non-ASCII text.
text = "".join("café %d üñ\n" % i for i in range(N))
for enc in ("latin-1", "iso-8859-1", "cp1252", "utf-8", "utf-16"):
    with open(p, "w", encoding=enc) as f:
        w = f.write
        for i in range(N):
            assert w("café %d üñ\n" % i) == len("café %d üñ\n" % i)
    with open(p, "rb") as f:
        assert f.read() == text.encode(enc), enc
    got, _ = lines_by_readline(encoding=enc)
    assert "".join(got) == text and len(got) == N, enc
    assert "".join(lines_by_loop(encoding=enc)) == text
put(bytes(range(256)) * 20)
with open(p, encoding="latin-1", newline="\n") as f:
    assert f.read() == "".join(map(chr, range(256))) * 20
got, _ = lines_by_readline(encoding="latin-1", newline="\n")
assert "".join(got) == "".join(map(chr, range(256))) * 20

# What a codec can't take is its error, or its handler's answer.
for enc, bad in (("ascii", "café\n"), ("latin-1", "€1\n")):
    with open(p, "w", encoding=enc) as f:
        for _ in range(200):
            f.write("plain\n")
        try:
            f.write(bad)
        except UnicodeEncodeError:
            pass
        else:
            raise AssertionError("%s can't encode %r" % (enc, bad))
        f.write("after\n")
    with open(p, encoding=enc) as f:
        assert f.read() == "plain\n" * 200 + "after\n"
    with open(p, "w", encoding=enc, errors="replace") as f:
        for _ in range(200):
            f.write(bad)
    with open(p, "rb") as f:
        assert f.read() == bad.encode(enc, "replace") * 200
put(b"ok\n" * 200 + b"caf\xe9\n" + b"ok\n")
with open(p, encoding="ascii") as f:
    got = []
    try:
        while line := f.readline():
            got.append(line)
    except UnicodeDecodeError:
        pass
    else:
        raise AssertionError("ascii can't decode 0xe9")
    assert got == ["ok\n"] * len(got) and len(got) <= 200
with open(p, encoding="ascii", errors="replace") as f:
    assert list(f) == ["ok\n"] * 200 + ["caf�\n", "ok\n"]
with open(p, encoding="utf-8", errors="surrogateescape") as f:
    assert f.readlines()[200] == "caf\udce9\n"

# Writes whose `\n` becomes something else.
for nl, ending in (("\r\n", b"\r\n"), ("\r", b"\r"), ("\n", b"\n"), ("", b"\n"), (None, os.linesep.encode())):
    with open(p, "w", newline=nl) as f:
        w = f.write
        for i in range(N):
            w("a\nb%d\n\n" % i)
            f.write("c")
    with open(p, "rb") as f:
        want = b"".join(b"a\nb%d\n\nc" % i for i in range(N)).replace(b"\n", ending)
        assert f.read() == want, nl

# Limits, through a bound method and through the attribute.
put("".join("café %d\n" % i for i in range(N)).encode())
for kw in ({"mode": "rb"}, {"encoding": "latin-1"}):
    with open(p, **kw) as f, open(p, **kw) as g:
        rl = f.readline
        for i in range(N):
            a = rl(3)
            b = rl(2)
            c = rl()
            whole = g.readline(-1)
            assert a + b + c == whole and len(a) == 3 and len(b) == 2, i
        assert rl(5) == whole[:0] and g.readline() == whole[:0]
with open(p, encoding="utf-8") as f:
    got = []
    while chunk := f.readline(4):
        got.append(chunk)
    assert "".join(got) == "".join("café %d\n" % i for i in range(N))
s = io.StringIO("".join("héllo %d\n" % i for i in range(N)))
rl = s.readline
for i in range(N):
    assert rl(2) == "hé" and rl(100) == "llo %d\n" % i
assert rl(3) == "" and rl() == ""
b = io.BytesIO(b"abc\ndef\n" * N)
rl = b.readline
for i in range(N):
    assert rl(2) == b"ab" and rl() == b"c\n" and rl(10) == b"def\n"

# `StringIO` newline modes on the way in and out.
for nl, stored in ((None, "a\nb\nc\n"), ("", "a\r\nb\rc\n"), ("\n", "a\r\nb\rc\n"), ("\r\n", "a\r\r\nb\rc\r\n")):
    s = io.StringIO(newline=nl)
    w = s.write
    for _ in range(N):
        assert w("a\r\nb\rc\n") == 7
    assert s.getvalue() == stored * N, nl
    s.seek(0)
    assert "".join(iter(s.readline, "")) == stored * N

for name in os.listdir(d):
    os.unlink(os.path.join(d, name))
os.rmdir(d)
print("ok")
