# RFC 0027 Group 7: IO + OS + argparse + inspect + typing tail.
#
# Exercises ``io`` text/binary buffering (``BytesIO``, ``StringIO``),
# ``os`` filesystem primitives, ``pathlib.Path`` operations,
# ``argparse`` flag parsing (subparsers, choices, action=append),
# ``inspect`` signatures / parameter ordering, ``functools``
# decorators, ``typing`` generics, and the ``dataclasses`` features
# that landed in Python 3.10–3.13. Each block is self-contained.

import os
import tempfile


# ---------- io.BytesIO / io.StringIO ----------
import io

buf = io.BytesIO()
buf.write(b"hello")
buf.write(b" world")
buf.seek(0)
assert buf.read() == b"hello world"
assert buf.tell() == 11

buf.seek(0)
chunks = list(iter(lambda: buf.read(4), b""))
assert chunks == [b"hell", b"o wo", b"rld"]

sbuf = io.StringIO()
sbuf.write("hello")
sbuf.write(" world")
sbuf.seek(0)
assert sbuf.read() == "hello world"
assert sbuf.getvalue() == "hello world"

# Line iteration
sbuf = io.StringIO("line1\nline2\nline3\n")
assert list(sbuf) == ["line1\n", "line2\n", "line3\n"]


# ---------- temp files + os.scandir ----------
with tempfile.TemporaryDirectory() as d:
    p_a = os.path.join(d, "a.txt")
    p_b = os.path.join(d, "b.txt")
    with open(p_a, "w") as f:
        f.write("apple")
    with open(p_b, "w") as f:
        f.write("banana")

    entries = sorted(e.name for e in os.scandir(d))
    assert entries == ["a.txt", "b.txt"]

    for e in os.scandir(d):
        assert e.is_file()
        assert not e.is_dir()
        assert e.path.startswith(d)
        assert e.stat().st_size > 0


# ---------- pathlib ----------
from pathlib import Path

with tempfile.TemporaryDirectory() as d:
    base = Path(d)
    sub = base / "sub"
    sub.mkdir()
    f = sub / "x.txt"
    f.write_text("contents")
    assert f.exists()
    assert f.is_file()
    assert f.read_text() == "contents"
    assert f.suffix == ".txt"
    assert f.stem == "x"
    assert f.name == "x.txt"
    assert f.parent == sub
    assert f.parent.parent == base

    parts = sorted(str(p.relative_to(base)) for p in base.rglob("*.txt"))
    assert parts == ["sub/x.txt"], parts


# ---------- functools ----------
import functools


@functools.lru_cache(maxsize=128)
def fib(n):
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)


assert fib(10) == 55
info = fib.cache_info()
assert info.hits >= 1
assert info.misses >= 1

fib.cache_clear()
info = fib.cache_info()
assert info.hits == 0
assert info.misses == 0

# functools.partial
add = lambda a, b: a + b
add5 = functools.partial(add, 5)
assert add5(3) == 8

# functools.reduce
assert functools.reduce(lambda a, b: a * b, [1, 2, 3, 4]) == 24
assert functools.reduce(lambda a, b: a + b, [1, 2, 3], 10) == 16

# functools.cached_property
class Counter:
    _calls = 0

    @functools.cached_property
    def expensive(self):
        type(self)._calls += 1
        return 42


c = Counter()
assert c.expensive == 42
assert c.expensive == 42
assert Counter._calls == 1


# ---------- argparse ----------
import argparse

parser = argparse.ArgumentParser()
parser.add_argument("-v", "--verbose", action="store_true")
parser.add_argument("--count", type=int, default=1)
parser.add_argument("--mode", choices=["debug", "release"], default="release")
parser.add_argument("--tag", action="append", default=[])
parser.add_argument("positional", nargs="?")
args = parser.parse_args(
    ["-v", "--count", "3", "--mode", "debug", "--tag", "a", "--tag", "b", "hi"]
)
assert args.verbose is True
assert args.count == 3
assert args.mode == "debug"
assert args.tag == ["a", "b"]
assert args.positional == "hi"


# ---------- inspect ----------
import inspect


def f(a, b=1, *args, c, d=2, **kwargs):
    pass


sig = inspect.signature(f)
params = list(sig.parameters.values())
names = [p.name for p in params]
assert names == ["a", "b", "args", "c", "d", "kwargs"]

kinds = [p.kind for p in params]
assert kinds[0] == inspect.Parameter.POSITIONAL_OR_KEYWORD
assert kinds[2] == inspect.Parameter.VAR_POSITIONAL
assert kinds[3] == inspect.Parameter.KEYWORD_ONLY
assert kinds[5] == inspect.Parameter.VAR_KEYWORD

# bind
bound = sig.bind(10, c=30)
assert bound.arguments["a"] == 10
assert bound.arguments["c"] == 30


# ---------- typing ----------
import typing

# Generic type aliases via NewType / TypeAlias
UserId = typing.NewType("UserId", int)
assert UserId(5) == 5


def get_first(items: typing.List[int]) -> int:
    return items[0]


assert get_first([1, 2, 3]) == 1


# Optional / Union / Literal accept basic checks
def opt(x: typing.Optional[int] = None) -> typing.Optional[int]:
    return x


assert opt(5) == 5
assert opt() is None


# get_type_hints
def annotated(a: int, b: "str") -> typing.List[int]:
    return [a]


hints = typing.get_type_hints(annotated)
assert hints["a"] is int
assert hints["b"] is str


# ---------- dataclasses (PEP 557 + 681) ----------
import dataclasses


@dataclasses.dataclass(frozen=True)
class Point:
    x: int
    y: int


p = Point(1, 2)
assert p.x == 1
assert p.y == 2
try:
    p.x = 10  # frozen
except dataclasses.FrozenInstanceError:
    pass
else:
    raise AssertionError("frozen dataclass must reject assignment")

assert hash(p) == hash(Point(1, 2))

# default_factory
@dataclasses.dataclass
class Bag:
    items: list = dataclasses.field(default_factory=list)


a = Bag()
b = Bag()
a.items.append(1)
assert a.items == [1]
assert b.items == []

# asdict / astuple
@dataclasses.dataclass
class Box:
    a: int
    b: int


box = Box(1, 2)
assert dataclasses.asdict(box) == {"a": 1, "b": 2}
assert dataclasses.astuple(box) == (1, 2)


# ---------- enum mixin types ----------
import enum


class Color(enum.Enum):
    RED = 1
    GREEN = 2
    BLUE = 3


assert Color.RED.value == 1
assert Color(2) == Color.GREEN
assert Color["BLUE"] == Color.BLUE
assert list(Color) == [Color.RED, Color.GREEN, Color.BLUE]


class Mode(enum.IntFlag):
    READ = 1
    WRITE = 2
    EXEC = 4


m = Mode.READ | Mode.WRITE
assert m & Mode.READ == Mode.READ
assert int(m) == 3


# ---------- itertools tail ----------
import itertools

assert list(itertools.accumulate([1, 2, 3, 4])) == [1, 3, 6, 10]
assert list(itertools.accumulate([1, 2, 3], initial=10)) == [10, 11, 13, 16]
assert list(itertools.takewhile(lambda x: x < 3, [1, 2, 3, 4])) == [1, 2]
assert list(itertools.dropwhile(lambda x: x < 3, [1, 2, 3, 4, 1])) == [3, 4, 1]
assert list(itertools.compress("abcd", [1, 0, 1, 0])) == ["a", "c"]
assert list(itertools.starmap(lambda a, b: a * b, [(1, 2), (3, 4)])) == [2, 12]
assert list(itertools.zip_longest("ab", "xyz", fillvalue="_")) == [
    ("a", "x"),
    ("b", "y"),
    ("_", "z"),
]
assert list(itertools.batched("abcdef", 2)) == [
    ("a", "b"),
    ("c", "d"),
    ("e", "f"),
]


print("test_io_os_argparse_inspect: OK")
