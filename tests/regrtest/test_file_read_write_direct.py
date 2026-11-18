"""Binary file reads and writes served without extra copies.

A read of less than half a buffer is served from the stream's read-ahead
(one `read(2)` of a buffer's worth fills it, as CPython's
`BufferedReader` does), a read to the end of a regular file reads into a
`bytes` of the size `fstat` reports, and a write of at least a buffer's
worth with nothing staged goes straight to the descriptor. `tell`,
`seek`, `readline`, `peek`, `read1`, `readinto`, a file that grows
meanwhile, `r+b` streams, a text stream's `.buffer`, pipes and
unbuffered streams see the same bytes and positions as before. The
expected output is CPython's.
"""

import contextlib
import io
import os
import shutil
import tempfile
import threading


def reads():
    d = tempfile.mkdtemp()
    p = os.path.join(d, "f")
    data = bytes(range(256)) * 1000 + b"line1\nline2\n" * 5000
    with open(p, "wb") as f:
        f.write(data)
    out = []
    with open(p, "rb") as f:
        out.append(f.read(10)); out.append(f.tell()); out.append(f.readline()); out.append(f.read(3)); out.append(f.tell())
        f.seek(5); out.append(f.read(4)); out.append(f.tell())
        out.append(os.lseek(f.fileno(), 0, os.SEEK_CUR) >= f.tell())
        out.append(f.read1(5)); out.append(f.peek(2)[:2]); out.append(f.read(70000)[-3:]); out.append(f.tell())
        b = bytearray(7); out.append(f.readinto(b)); out.append(bytes(b)); out.append(f.tell())
        rest = f.read(); out.append(len(rest)); out.append(f.read(5)); out.append(f.tell())
    with open(p, "r+b") as f:
        out.append(f.read(4)); f.write(b"ZZ"); f.seek(0); out.append(f.read(8)); out.append(f.tell())
    with open(p, "rb") as f:
        n = 0
        while chunk := f.read(8192):
            n += len(chunk)
        out.append(n == len(data))
        f.seek(0); out.append(sum(len(c) for c in iter(lambda: f.read(1000), b"")))
    with open(p, "rb", buffering=0) as f:
        out.append(type(f).__name__); out.append(f.read(5)); out.append(f.tell())
    r, w = os.pipe()
    os.write(w, b"abcdefgh"); os.close(w)
    with open(r, "rb") as f:
        out.append(f.read(3)); out.append(f.read(100)); out.append(f.read(3))
    print(out)
    # read-all edge cases
    with open(p, "rb") as f:
        f.seek(len(data) - 5); print(f.read(), f.read(), f.tell())
        f.seek(len(data) + 10); print(f.read(), f.tell())
        f.seek(0); f.readline(); print(len(f.read()), f.tell())
    with open(p, "ab") as g:
        pass
    with open(p, "rb") as f, open(p, "ab") as g:
        f.read(7); g.write(b"TAIL"); g.flush(); print(f.read()[-6:], f.tell())
    open(p, "wb").close()
    with open(p, "rb") as f:
        print(f.read(), f.read(3))
    with open(p, "rb", buffering=0) as f:
        print(f.read(), f.readall())
    print(open("/dev/null", "rb").read())
    shutil.rmtree(d)


def writes():
    d = tempfile.mkdtemp(); p = os.path.join(d, "w")
    big = bytes(range(256)) * 600
    out = []
    with open(p, "wb") as f:
        f.write(b"head"); f.write(big); out.append(f.tell()); f.write(big); f.write(b"x" * 10)
    out.append(os.path.getsize(p))
    with open(p, "wb") as f:
        f.write(big); out.append(os.path.getsize(p)); f.write(b"tail"); out.append(os.path.getsize(p)); out.append(f.tell())
    out.append(os.path.getsize(p))
    with open(p, "r+b") as f:
        f.read(3); f.write(big); out.append(f.tell()); f.seek(0); out.append(len(f.read()))
    with open(p, "w", encoding="latin-1") as t:
        t.write("abc"); t.buffer.write(big); t.write("def")
    with open(p, "rb") as f:
        x = f.read(); out.append((len(x), x[:3], x[-3:]))
    with open(p, "ab") as f:
        f.write(big); out.append(f.tell())
    r, w = os.pipe()
    got = []
    def drain():
        n = 0
        while chunk := os.read(r, 1 << 20):
            n += len(chunk)
        got.append(n)
    th = threading.Thread(target=drain)
    with open(w, "wb") as f:
        th.start(); f.write(big[:150000])
    th.join()
    out.append(got[0])
    print(out)
    shutil.rmtree(d)


def output(case):
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        case()
    return buf.getvalue()


EXPECTED_READS = "[b'\\x00\\x01\\x02\\x03\\x04\\x05\\x06\\x07\\x08\\t', 10, b'\\n', b'\\x0b\\x0c\\r', 14, b'\\x05\\x06\\x07\\x08', 9, True, b'\\t\\n\\x0b\\x0c\\r', b'\\x0e\\x0f', b'{|}', 70014, 7, b'~\\x7f\\x80\\x81\\x82\\x83\\x84', 70021, 245979, b'', 316000, b'\\x00\\x01\\x02\\x03', b'\\x00\\x01\\x02\\x03ZZ\\x06\\x07', 8, True, 316000, 'FileIO', b'\\x00\\x01\\x02\\x03Z', 5, b'abc', b'defgh', b'']\nb'ine2\\n' b'' 316000\nb'' 316010\n315989 316000\nb'2\\nTAIL' 316004\nb'' b''\nb'' b''\nb''\n"
EXPECTED_WRITES = "[153604, 307214, 153600, 153600, 153604, 153604, 153603, 153604, (153606, b'\\x00\\x01\\x02', b'def'), 307206, 150000]\n"

got = output(reads)
assert got == EXPECTED_READS, got
got = output(writes)
assert got == EXPECTED_WRITES, got
print("ok")
