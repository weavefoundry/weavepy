"""Everyday-operations battery: short loops over standard-library and builtin
operations that ordinary programs lean on, for finding the ones an
interpreter runs far slower than another.

Run it under each interpreter and compare:

    python3.14 tools/pybench/everyday.py --json cpy.json
    weavepy tools/pybench/everyday.py --json wp.json
    python3 tools/pybench/everyday.py --compare cpy.json wp.json

Each case reports its best time of a few runs, so a loaded machine blurs
small differences; the battery is for finding factors, not percents.
"""

import json
import sys
import time

CASES = []


def case(fn):
    CASES.append(fn)
    return fn


# -- files and the file system ------------------------------------------------


def _tmp():
    import tempfile

    return tempfile.mkdtemp(prefix="everyday-")


@case
def file_write_lines():
    import os

    d = _tmp()
    p = os.path.join(d, "f.txt")
    with open(p, "w") as f:
        for i in range(20000):
            f.write("line %d\n" % i)
    os.unlink(p)
    os.rmdir(d)


@case
def file_read_lines():
    import os

    d = _tmp()
    p = os.path.join(d, "f.txt")
    with open(p, "w") as f:
        f.write("some line of text\n" * 20000)
    n = 0
    with open(p) as f:
        for line in f:
            n += len(line)
    os.unlink(p)
    os.rmdir(d)


@case
def file_read_binary_chunks():
    import os

    d = _tmp()
    p = os.path.join(d, "f.bin")
    with open(p, "wb") as f:
        f.write(b"x" * 4_000_000)
    n = 0
    with open(p, "rb") as f:
        while chunk := f.read(8192):
            n += len(chunk)
    os.unlink(p)
    os.rmdir(d)


@case
def file_small_open_close():
    import os

    d = _tmp()
    p = os.path.join(d, "f.txt")
    for i in range(300):
        with open(p, "w") as f:
            f.write("x")
        with open(p) as f:
            f.read()
    os.unlink(p)
    os.rmdir(d)


@case
def os_stat_exists():
    import os

    p = os.path.abspath(__file__)
    for i in range(3000):
        os.stat(p)
        os.path.exists(p)
        os.path.isfile(p)


@case
def os_listdir_walk():
    import os

    root = os.path.dirname(os.path.abspath(__file__))
    for i in range(30):
        os.listdir(root)
        for _ in os.walk(root):
            pass
        with os.scandir(root) as it:
            for e in it:
                e.is_file()


@case
def os_path_ops():
    import os.path as P

    for i in range(4000):
        p = P.join("/usr", "local", "lib", "python%d" % (i % 7), "site.py")
        P.basename(p), P.dirname(p), P.splitext(p), P.normpath(p), P.split(p)


@case
def pathlib_ops():
    from pathlib import Path

    for i in range(1500):
        p = Path("/usr/local") / "lib" / ("f%d.txt" % (i % 5))
        p.name, p.suffix, p.parent, p.stem, str(p), p.with_suffix(".py")


@case
def glob_fnmatch():
    import fnmatch, glob, os

    root = os.path.dirname(os.path.abspath(__file__))
    names = os.listdir(root) * 20
    for i in range(20):
        glob.glob(os.path.join(root, "*.py"))
        fnmatch.filter(names, "*.py")


@case
def shutil_copy_tree():
    import os, shutil

    d = _tmp()
    src = os.path.join(d, "src")
    os.mkdir(src)
    for i in range(30):
        with open(os.path.join(src, "f%d" % i), "w") as f:
            f.write("data" * 100)
    for i in range(5):
        dst = os.path.join(d, "dst%d" % i)
        shutil.copytree(src, dst)
    shutil.rmtree(d)


@case
def tempfile_use():
    import tempfile

    for i in range(150):
        with tempfile.TemporaryFile() as f:
            f.write(b"hello")
            f.seek(0)
            f.read()
        with tempfile.NamedTemporaryFile("w") as f:
            f.write("hello")


# -- text ---------------------------------------------------------------------


@case
def str_format_ops():
    for i in range(20000):
        "%s=%d (%.2f)" % ("k", i, i / 3)
        "{}={} ({:.2f})".format("k", i, i / 3)
        f"{'k'}={i} ({i / 3:.2f})"
        f"{i:>8} {i:08x} {i:,}"


@case
def str_methods_mix():
    s = "The quick brown fox jumps over the lazy dog, again and again."
    for i in range(8000):
        s.split(), s.lower().count("a"), s.replace("fox", "cat"), s.find("lazy")
        s.partition(","), s.title(), s.center(80), s.encode(), s.startswith("The")
        ",".join(["a", "b", "c", str(i)]), s[::-1], s.zfill(70), s.strip("."), s.isalnum()


@case
def str_build_and_slice():
    for i in range(300):
        parts = []
        for j in range(200):
            parts.append(str(j))
        s = "".join(parts)
        t = ""
        for j in range(100):
            t += s[j : j + 3]


@case
def bytes_ops():
    b = b"The quick brown fox jumps over the lazy dog"
    for i in range(8000):
        b.split(), b.replace(b"fox", b"cat"), b.find(b"lazy"), b.upper(), b.decode()
        b"%d:%s" % (i, b"x"), b + b"!", b[5:20], bytes(10), bytearray(b), b.hex()


@case
def unicode_ops():
    s = "héllo wörld 世界 \U0001f600 " * 10
    for i in range(3000):
        s.encode("utf-8").decode("utf-8"), s.upper(), s.casefold(), len(s), s[3:40]
        s.encode("utf-16"), s.isprintable(), s.split()


@case
def re_ops():
    import re

    text = "alpha=1, beta=22, gamma=333; delta = 4444 " * 20
    pat = re.compile(r"(\w+)\s*=\s*(\d+)")
    for i in range(300):
        pat.findall(text)
        pat.sub(r"\2=\1", text)
        for m in pat.finditer(text):
            m.group(1), m.start()
        re.split(r"[,;]\s*", text)
        re.match(r"\w+", text)
        re.search(r"delta\s*=\s*(\d+)", text)


@case
def textwrap_string_template():
    import string, textwrap

    text = "word " * 400
    t = string.Template("$a and ${b} and $c")
    for i in range(60):
        textwrap.fill(text, 60)
        textwrap.dedent("    a\n    b\n")
        textwrap.shorten(text, 50)
    for i in range(3000):
        t.substitute(a=1, b=2, c=3)


@case
def difflib_ops():
    import difflib

    a = ["line %d\n" % i for i in range(200)]
    b = ["line %d\n" % i for i in range(0, 200, 2)] + ["new\n"] * 20
    for i in range(6):
        list(difflib.unified_diff(a, b))
        difflib.SequenceMatcher(None, "abcdefgh" * 10, "abxdefgz" * 10).ratio()
        difflib.get_close_matches("appel", ["ape", "apple", "peach", "puppy"])


@case
def shlex_ops():
    import shlex

    for i in range(1500):
        shlex.split("cmd --flag 'quoted arg' \"another one\" plain")
        shlex.quote("it's a test")
        shlex.join(["a", "b c", "d"])


# -- data formats -------------------------------------------------------------


@case
def json_ops():
    import json

    obj = {"k%d" % i: [i, str(i), i / 7, None, True, {"n": i}] for i in range(60)}
    for i in range(200):
        s = json.dumps(obj)
        json.loads(s)
        json.dumps(obj, indent=2, sort_keys=True)


@case
def csv_ops():
    import csv, io

    rows = [[i, "name%d" % i, i * 1.5, "x,y"] for i in range(200)]
    for i in range(40):
        buf = io.StringIO()
        w = csv.writer(buf)
        w.writerows(rows)
        buf.seek(0)
        list(csv.reader(buf))
        buf.seek(0)
        list(csv.DictReader(buf, fieldnames=["a", "b", "c", "d"]))


@case
def pickle_copy_ops():
    import copy, pickle

    obj = {"k%d" % i: [i, str(i), (i, i + 1), {"n": i}] for i in range(80)}
    for i in range(150):
        pickle.loads(pickle.dumps(obj))
        copy.deepcopy(obj)
        copy.copy(obj)


@case
def struct_base64_binascii():
    import base64, binascii, struct

    data = bytes(range(256)) * 4
    for i in range(4000):
        struct.pack("<IHBd", i, 2, 3, 1.5)
        struct.unpack("<IHBd", b"\x01\x00\x00\x00\x02\x00\x03\x00\x00\x00\x00\x00\x00\x00\xf8?")
        base64.b64decode(base64.b64encode(data))
        binascii.hexlify(data)
        int.from_bytes(data[:8], "little"), (i).to_bytes(8, "big")


@case
def hashlib_zlib():
    import hashlib, zlib

    data = b"some data to hash and compress " * 300
    for i in range(300):
        hashlib.sha256(data).hexdigest()
        hashlib.md5(data).digest()
        zlib.decompress(zlib.compress(data))
        zlib.crc32(data)


@case
def configparser_ops():
    import configparser, io

    text = "".join("[s%d]\nkey = value %d\nnum = %d\nflag = yes\n\n" % (i, i, i) for i in range(40))
    for i in range(15):
        cp = configparser.ConfigParser()
        cp.read_string(text)
        for s in cp.sections():
            cp.get(s, "key"), cp.getint(s, "num"), cp.getboolean(s, "flag")
        out = io.StringIO()
        cp.write(out)


@case
def xml_etree_ops():
    import xml.etree.ElementTree as ET

    text = "<root>" + "".join('<item id="%d"><name>n%d</name><v>%d</v></item>' % (i, i, i) for i in range(100)) + "</root>"
    for i in range(30):
        root = ET.fromstring(text)
        for item in root.iter("item"):
            item.get("id"), item.find("name").text
        ET.tostring(root)


@case
def urllib_parse_ops():
    from urllib.parse import parse_qs, quote, unquote, urlencode, urljoin, urlparse

    for i in range(1500):
        u = urlparse("https://example.com:8080/a/b/c?x=1&y=two#frag")
        u.netloc, u.path, u.query
        parse_qs("x=1&y=two&z=3&x=4")
        urlencode({"a": 1, "b": "two words", "c": i})
        quote("a b/c?d=e&f"), unquote("a%20b%2Fc"), urljoin("http://a/b/c", "../d")


@case
def email_html_ops():
    import html
    from email.message import EmailMessage
    from email.parser import Parser

    for i in range(120):
        m = EmailMessage()
        m["From"] = "a@example.com"
        m["To"] = "b@example.com"
        m["Subject"] = "hello %d" % i
        m.set_content("body text\n" * 5)
        Parser().parsestr(m.as_string())
        html.escape("<a href='x'>&</a>" * 5), html.unescape("&lt;a&gt; &amp; &quot;" * 5)


# -- containers and algorithms ------------------------------------------------


@case
def list_dict_basics():
    for i in range(300):
        xs = list(range(300))
        ys = [x * 2 for x in xs if x % 3]
        d = {x: str(x) for x in xs}
        s = set(xs) & set(ys)
        sorted(ys, reverse=True), sum(xs), min(xs), max(ys), any(ys), all(xs[1:])
        for k, v in d.items():
            pass
        xs.sort(key=lambda x: -x)
        ys.extend(xs[:10]), ys.insert(0, 1), ys.pop(), ys.index(4), xs.reverse()


@case
def sort_ops():
    import random

    r = random.Random(1)
    data = [r.random() for _ in range(5000)]
    words = [str(r.randrange(10**6)) for _ in range(5000)]
    recs = [(r.randrange(100), w) for w in words]
    from operator import itemgetter

    for i in range(8):
        sorted(data), sorted(words), sorted(recs), sorted(recs, key=itemgetter(1))
        sorted(words, key=len), sorted(words, key=lambda w: w[::-1])


@case
def collections_ops():
    from collections import ChainMap, Counter, OrderedDict, defaultdict, deque, namedtuple

    P = namedtuple("P", "x y z")
    words = ("the quick brown fox jumps over the lazy dog " * 30).split()
    for i in range(60):
        c = Counter(words)
        c.most_common(3), c.update(words[:50])
        dd = defaultdict(list)
        for w in words:
            dd[len(w)].append(w)
        dq = deque(maxlen=50)
        for w in words:
            dq.append(w)
        dq.rotate(3), dq.popleft()
        od = OrderedDict.fromkeys(words)
        od.move_to_end("the"), od.popitem()
        p = P(1, 2, 3)
        p._replace(x=5), p._asdict(), p.x + p.y
        ChainMap({"a": 1}, {"b": 2})["b"]


@case
def itertools_functools():
    import functools, itertools, operator

    @functools.lru_cache(maxsize=256)
    def fib(n):
        return n if n < 2 else fib(n - 1) + fib(n - 2)

    add3 = functools.partial(operator.add, 3)
    for i in range(150):
        list(itertools.chain(range(50), range(50)))
        list(itertools.islice(itertools.count(), 100))
        list(itertools.product("abc", repeat=3))
        list(itertools.combinations(range(10), 3))
        list(itertools.groupby(sorted("mississippi")))
        list(itertools.accumulate(range(100)))
        list(itertools.zip_longest("abc", "de"))
        functools.reduce(operator.add, range(100))
        fib.cache_clear(), fib(60), add3(4)
        list(map(add3, range(100)))


@case
def heapq_bisect():
    import bisect, heapq, random

    r = random.Random(2)
    data = [r.random() for _ in range(2000)]
    for i in range(20):
        h = []
        for x in data:
            heapq.heappush(h, x)
        while h:
            heapq.heappop(h)
        heapq.nlargest(10, data), heapq.nsmallest(10, data)
        s = sorted(data)
        for x in data[:500]:
            bisect.bisect_left(s, x)
            bisect.insort(s, x)


@case
def enum_dataclass_ops():
    import dataclasses, enum

    class Color(enum.Enum):
        RED = 1
        GREEN = 2
        BLUE = 3

    class Flag(enum.IntFlag):
        A = 1
        B = 2
        C = 4

    @dataclasses.dataclass
    class Pt:
        x: int
        y: int = 0
        tags: list = dataclasses.field(default_factory=list)

    @dataclasses.dataclass(frozen=True, order=True)
    class Key:
        a: int
        b: str

    for i in range(3000):
        Color(1), Color["RED"], Color.RED.name, Color.RED.value, Color.RED is Color.RED
        Flag.A | Flag.B, Flag(3), Flag.A in Flag(3)
        p = Pt(i, 2)
        p == Pt(i, 2), repr(p), dataclasses.asdict(p), dataclasses.replace(p, x=1)
        k = Key(i, "x")
        hash(k), k < Key(i + 1, "x")


@case
def class_features():
    import abc

    class Base(abc.ABC):
        @abc.abstractmethod
        def f(self): ...

        @property
        def p(self):
            return self._p

        @p.setter
        def p(self, v):
            self._p = v

        @classmethod
        def make(cls):
            return cls()

        @staticmethod
        def s(x):
            return x

    class Impl(Base):
        __slots__ = ("_p", "a")

        def f(self):
            return 1

        def __eq__(self, o):
            return isinstance(o, Impl)

        def __hash__(self):
            return 7

        def __len__(self):
            return 3

        def __getitem__(self, i):
            return i

        def __contains__(self, x):
            return True

    for i in range(6000):
        o = Impl.make()
        o.p = i
        o.p, o.f(), Impl.s(1), o == Impl(), hash(o), len(o), o[2], 3 in o
        isinstance(o, Base), issubclass(Impl, Base), getattr(o, "p"), hasattr(o, "zz")
        super(Impl, o).__class__, type(o).__name__, callable(o.f)


@case
def exceptions_ops():
    class E(Exception):
        pass

    def thrower(n):
        if n:
            raise E(n)
        return 0

    def deep(n):
        return thrower(n) if n < 2 else deep(n - 1)

    d = {}
    for i in range(8000):
        try:
            thrower(1)
        except E as e:
            e.args
        try:
            d["missing"]
        except KeyError:
            pass
        try:
            int("x")
        except ValueError:
            pass
        try:
            deep(5)
        except E:
            pass
        try:
            [].pop()
        except IndexError:
            pass
        finally:
            pass
        try:
            getattr(d, "nope")
        except AttributeError:
            pass


@case
def context_managers_generators():
    import contextlib

    @contextlib.contextmanager
    def cm(x):
        yield x

    class C:
        def __enter__(self):
            return self

        def __exit__(self, *a):
            return False

    def gen(n):
        for i in range(n):
            yield i

    for i in range(3000):
        with cm(1) as a, C() as b:
            pass
        with contextlib.suppress(KeyError):
            {}["x"]
        sum(gen(20)), list(x for x in gen(10) if x % 2), dict(zip("abc", gen(3)))
        with contextlib.ExitStack() as st:
            st.enter_context(cm(2))
            st.callback(len, "x")


# -- numbers and time ---------------------------------------------------------


@case
def math_ops():
    import math

    for i in range(1, 20000):
        math.sqrt(i), math.sin(i), math.log(i), math.floor(i / 3), math.gcd(i, 36)
        math.isclose(i, i + 1e-9), math.hypot(i, 3), divmod(i, 7), pow(i, 3, 97), round(i / 7, 2)
        abs(-i), int(i / 2), float(i), i**2, i // 3, i % 5, i << 2


@case
def bigint_ops():
    for i in range(300):
        n = 3**400 + i
        n * n, n // 7, n % 1000007, str(n), int(str(n)), pow(n, 5, 10**9 + 7)
        n.bit_length(), hex(n), n >> 13, -n, n == n + 0, hash(n)


@case
def decimal_fraction_ops():
    from decimal import Decimal
    from fractions import Fraction

    for i in range(1, 1500):
        a, b = Decimal("1.1") * i, Decimal(i) / 7
        a + b, a * b, a.quantize(Decimal("0.01")), str(a), a > b
        f = Fraction(i, 7) + Fraction(3, i)
        f * f, float(f), f.numerator, str(f)


@case
def random_statistics_ops():
    import random, statistics

    r = random.Random(3)
    data = [r.random() for _ in range(500)]
    for i in range(60):
        r.randint(1, 100), r.choice(data), r.shuffle(data), r.sample(data, 10), r.uniform(1, 2)
        r.gauss(0, 1), r.randrange(1000), r.choices(data, k=5)
        statistics.mean(data), statistics.median(data), statistics.stdev(data)


@case
def datetime_time_ops():
    import datetime as dt, time

    for i in range(3000):
        now = dt.datetime(2024, 5, 17, 12, 30, 15) + dt.timedelta(seconds=i)
        now.isoformat(), now.strftime("%Y-%m-%d %H:%M:%S"), now.date(), now.weekday()
        dt.datetime.fromisoformat("2024-05-17T12:30:15"), now - dt.datetime(2020, 1, 1)
        dt.datetime.strptime("2024-05-17", "%Y-%m-%d"), dt.date(2024, 5, 17).toordinal()
        time.time(), time.perf_counter(), time.monotonic(), time.gmtime(i), time.strftime("%H:%M", time.gmtime(i))
        now.replace(tzinfo=dt.timezone.utc).timestamp(), now.astimezone(dt.timezone.utc)


@case
def uuid_secrets_ops():
    import secrets, uuid

    for i in range(1500):
        uuid.uuid4(), str(uuid.uuid4()), uuid.UUID("12345678123456781234567812345678").hex
        secrets.token_hex(8), secrets.randbelow(100), uuid.uuid5(uuid.NAMESPACE_DNS, "x%d" % i)


# -- program plumbing ---------------------------------------------------------


@case
def logging_ops():
    import io, logging

    log = logging.getLogger("everyday")
    log.setLevel(logging.INFO)
    log.propagate = False
    stream = io.StringIO()
    h = logging.StreamHandler(stream)
    h.setFormatter(logging.Formatter("%(asctime)s %(levelname)s %(name)s: %(message)s"))
    log.addHandler(h)
    for i in range(1500):
        log.info("value %d of %s", i, "x")
        log.debug("dropped %d", i)
        log.warning("warn %s", i)
    log.removeHandler(h)


@case
def argparse_ops():
    import argparse

    for i in range(60):
        ap = argparse.ArgumentParser(prog="x")
        ap.add_argument("--count", type=int, default=1)
        ap.add_argument("--name", default="n")
        ap.add_argument("-v", "--verbose", action="store_true")
        ap.add_argument("files", nargs="*")
        ap.parse_args(["--count", "3", "-v", "a", "b", "--name", "z"])
        ap.format_help()


@case
def subprocess_ops():
    import subprocess

    for i in range(12):
        subprocess.run(["/bin/echo", "hi"], capture_output=True, check=True)
        subprocess.check_output(["/bin/cat"], input=b"data")


@case
def threading_queue_ops():
    import queue, threading

    for i in range(15):
        q = queue.Queue()
        out = []

        def worker():
            while (item := q.get()) is not None:
                out.append(item * 2)

        ts = [threading.Thread(target=worker) for _ in range(3)]
        for t in ts:
            t.start()
        for j in range(300):
            q.put(j)
        for t in ts:
            q.put(None)
        for t in ts:
            t.join()
        lock = threading.Lock()
        for j in range(500):
            with lock:
                pass


@case
def asyncio_ops():
    import asyncio

    async def work(i):
        await asyncio.sleep(0)
        return i

    async def main():
        for _ in range(15):
            await asyncio.gather(*(work(i) for i in range(60)))
        q = asyncio.Queue()
        for i in range(200):
            await q.put(i)
            await q.get()

    asyncio.run(main())


@case
def sqlite_ops():
    import sqlite3

    for i in range(5):
        cx = sqlite3.connect(":memory:")
        cx.execute("create table t(a integer, b text)")
        cx.executemany("insert into t values (?, ?)", [(j, "v%d" % j) for j in range(400)])
        cx.execute("select count(*), sum(a) from t").fetchone()
        for row in cx.execute("select a, b from t where a % 3 = 0 order by b"):
            pass
        cx.close()


@case
def inspect_typing_ops():
    import inspect, typing

    def f(a, b: int = 2, *args, c: str = "x", **kw) -> typing.Optional[int]:
        return None

    class G(typing.Generic[typing.TypeVar("T")]):
        pass

    for i in range(400):
        inspect.signature(f), inspect.isfunction(f), inspect.getmembers(G)[:1]
        typing.get_type_hints(f), typing.List[int], typing.Optional[str], typing.Dict[str, typing.Any]
        isinstance(i, typing.Hashable), G[int]


@case
def weakref_gc_ops():
    import gc, weakref

    class N:
        pass

    for i in range(200):
        nodes = [N() for _ in range(100)]
        for a, b in zip(nodes, nodes[1:]):
            a.next = b
            b.prev = a
        refs = [weakref.ref(n) for n in nodes]
        wd = weakref.WeakValueDictionary((j, n) for j, n in enumerate(nodes))
        del nodes
        if i % 50 == 0:
            gc.collect()
        refs[0](), len(wd)


@case
def print_to_buffer():
    import io

    out = io.StringIO()
    for i in range(15000):
        print("row", i, "of", 15000, file=out)
        print(f"{i}: {'x' * 3}", end=";", file=out)


@case
def traceback_warnings_ops():
    import traceback, warnings

    def boom(n):
        if n == 0:
            raise ValueError("boom")
        boom(n - 1)

    for i in range(150):
        try:
            boom(6)
        except ValueError:
            traceback.format_exc()
        with warnings.catch_warnings(record=True) as w:
            warnings.simplefilter("always")
            warnings.warn("careful", DeprecationWarning)


def run_all(names):
    results = {}
    for fn in CASES:
        if names and fn.__name__ not in names:
            continue
        best = None
        err = None
        for _ in range(3):
            t0 = time.perf_counter()
            try:
                fn()
            except Exception as e:  # a case an interpreter can't run is a finding too
                err = "%s: %s" % (type(e).__name__, e)
                break
            dt = time.perf_counter() - t0
            best = dt if best is None else min(best, dt)
        results[fn.__name__] = err if err else best
    return results


def compare(a_path, b_path):
    with open(a_path) as f:
        a = json.load(f)
    with open(b_path) as f:
        b = json.load(f)
    rows = []
    for name, base in a.items():
        new = b.get(name)
        if isinstance(base, float) and isinstance(new, float):
            rows.append((new / base, name, base, new))
        else:
            print("%-28s %s | %s" % (name, base, new))
    prod, n = 1.0, 0
    for ratio, name, base, new in sorted(rows, reverse=True):
        print("%-28s %8.2f ms %8.2f ms  %5.2f" % (name, base * 1e3, new * 1e3, ratio))
        prod *= ratio
        n += 1
    if n:
        print("geomean over %d: %.3f" % (n, prod ** (1 / n)))


def main(argv):
    if argv[:1] == ["--compare"]:
        compare(argv[1], argv[2])
        return
    out = None
    if argv[:1] == ["--json"]:
        out, argv = argv[1], argv[2:]
    results = run_all(set(argv))
    if out:
        with open(out, "w") as f:
            json.dump(results, f, indent=1)
    else:
        for name, v in results.items():
            print("%-28s %s" % (name, "%8.2f ms" % (v * 1e3) if isinstance(v, float) else v))


if __name__ == "__main__":
    main(sys.argv[1:])
