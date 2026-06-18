"""RFC 0040 WS7/WS8 — archive round-trips and the I/O fidelity tail.

Pins the faithful `tarfile`/`zipfile` packages, `shutil.make_archive` over a
directory tree, `tempfile.SpooledTemporaryFile` rollover, and the WS7 native
text-stream fidelity (`.newlines` seen-set tracking, real `.encoding`/
`.errors`, the `BufferedReader`/`BufferedWriter` wrappers) the archive tail
depends on.
"""

import io
import os
import shutil
import tarfile
import tempfile
import zipfile


# ---------------------------------------------------------------------------
# tarfile: write two members (incl. a >100-char name needing a GNU/PAX long
# header) into an in-memory tar and read them back.
# ---------------------------------------------------------------------------

long_name = "deeply/nested/" + "x" * 120 + ".txt"
buf = io.BytesIO()
with tarfile.open(fileobj=buf, mode="w") as tf:
    for name, data in [("short.txt", b"alpha"), (long_name, b"beta")]:
        info = tarfile.TarInfo(name)
        info.size = len(data)
        tf.addfile(info, io.BytesIO(data))

buf.seek(0)
with tarfile.open(fileobj=buf, mode="r") as tf:
    members = {m.name: tf.extractfile(m).read() for m in tf.getmembers()}
assert members["short.txt"] == b"alpha", members
assert members[long_name] == b"beta", list(members)


# ---------------------------------------------------------------------------
# tarfile streaming mode (w|gz / r|gz) — no seek on the underlying stream.
# ---------------------------------------------------------------------------

sbuf = io.BytesIO()
with tarfile.open(fileobj=sbuf, mode="w|gz") as tf:
    info = tarfile.TarInfo("stream.txt")
    payload = b"streamed-content\n" * 100
    info.size = len(payload)
    tf.addfile(info, io.BytesIO(payload))
sbuf.seek(0)
with tarfile.open(fileobj=sbuf, mode="r|gz") as tf:
    m = tf.next()
    assert tf.extractfile(m).read() == payload


# ---------------------------------------------------------------------------
# zipfile: write + read back, with a directory entry and a compressed member.
# ---------------------------------------------------------------------------

zbuf = io.BytesIO()
with zipfile.ZipFile(zbuf, "w", zipfile.ZIP_DEFLATED) as zf:
    zf.writestr("a.txt", "hello")
    zf.writestr("sub/b.txt", "world" * 1000)
zbuf.seek(0)
with zipfile.ZipFile(zbuf, "r") as zf:
    assert zf.read("a.txt") == b"hello"
    assert zf.read("sub/b.txt") == b"world" * 1000
    assert "sub/b.txt" in zf.namelist()


# ---------------------------------------------------------------------------
# shutil.make_archive("zip") over a real directory tree round-trips.
# ---------------------------------------------------------------------------

work = tempfile.mkdtemp()
try:
    src = os.path.join(work, "tree")
    os.makedirs(os.path.join(src, "pkg"))
    with open(os.path.join(src, "top.txt"), "w") as f:
        f.write("top")
    with open(os.path.join(src, "pkg", "inner.txt"), "w") as f:
        f.write("inner")

    base = os.path.join(work, "out")
    archive = shutil.make_archive(base, "zip", root_dir=src)
    assert archive.endswith(".zip") and os.path.exists(archive)
    with zipfile.ZipFile(archive) as zf:
        names = set(zf.namelist())
        assert "top.txt" in names, names
        assert "pkg/inner.txt" in names, names
        assert zf.read("pkg/inner.txt") == b"inner"
finally:
    shutil.rmtree(work)


# ---------------------------------------------------------------------------
# tempfile.SpooledTemporaryFile: rolls over from memory to disk past max_size.
# ---------------------------------------------------------------------------

with tempfile.SpooledTemporaryFile(max_size=8, mode="w+b") as spool:
    spool.write(b"123")
    spool.write(b"4567890")  # crosses max_size -> rolls over to a real file
    spool.seek(0)
    assert spool.read() == b"1234567890"


# ---------------------------------------------------------------------------
# WS7: a native text stream tracks the newline kinds it has seen (.newlines)
# and reports its real .encoding / .errors (not a hard-coded utf-8/strict).
# ---------------------------------------------------------------------------

path = tempfile.mktemp()
try:
    with open(path, "wb") as f:
        f.write(b"a\r\nb\nc")
    with open(path, "r", encoding="latin-1", errors="replace", newline=None) as f:
        assert f.encoding == "latin-1", f.encoding
        assert f.errors == "replace", f.errors
        assert f.read() == "a\nb\nc"
        nl = f.newlines
        # Universal-newline read saw both CRLF and LF.
        assert nl == ("\r\n", "\n") or set(nl) == {"\r\n", "\n"}, nl
finally:
    os.remove(path)


# ---------------------------------------------------------------------------
# WS7: BufferedReader / BufferedWriter wrappers over an in-memory raw stream.
# ---------------------------------------------------------------------------

raw = io.BytesIO()
bw = io.BufferedWriter(raw)
bw.write(b"buffered")
bw.flush()
assert raw.getvalue() == b"buffered"

br = io.BufferedReader(io.BytesIO(b"readme"))
assert br.read() == b"readme"


print("WS7/WS8 archive + io fidelity ok")
