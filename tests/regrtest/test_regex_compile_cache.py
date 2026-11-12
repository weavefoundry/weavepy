"""Compiled regular expressions that a later process takes from the disk
cache (`_sre._cache_get`) instead of compiling again keep CPython's
results: matches, groups by name and number, flags (inline, verbose,
bytes), and a pattern's FutureWarning, which shows in every process."""
import os
import subprocess
import sys
import tempfile

CHILD = r'''
import re, warnings
out = []
p = re.compile(r"(?P<year>\d{4})-(?P<mon>\d\d)(?:-(\d\d))?", re.ASCII)
m = p.match("2026-11-05")
out.append((m.groups(), m.groupdict(), p.groupindex["mon"], p.groups, p.flags))
v = re.compile(r"""
    (?i) ab+   # letters
    \s* (?P<n> [0-9]+ )
""", re.VERBOSE)
out.append((v.flags, v.match("ABBB 42").group("n"), v.pattern.count("#")))
b = re.compile(rb"(\w+)=(\w*)")
out.append((b.findall(b"a=1 bb= c=x"), b.flags))
with warnings.catch_warnings(record=True) as w:
    warnings.simplefilter("always")
    re.compile("[[a]")
out.append([str(x.category.__name__) for x in w])
out.append(re.sub(r"(?m)^(\w)", lambda m: m.group(1).upper(), "ab\ncd"))
try:
    re.compile("(unclosed")
except re.error as e:
    out.append(e.msg)
print(repr(out))
'''

with tempfile.TemporaryDirectory() as d:
    env = dict(os.environ, WEAVEPY_FROZEN_CACHE=d)
    runs = [
        subprocess.run([sys.executable, "-c", CHILD], env=env, capture_output=True, text=True)
        for _ in range(2)
    ]
    for r in runs:
        assert r.returncode == 0, r.stderr
    assert runs[0].stdout == runs[1].stdout, (runs[0].stdout, runs[1].stdout)
    out = eval(runs[1].stdout)
    assert out[0] == (("2026", "11", "05"), {"year": "2026", "mon": "11"}, 2, 3, 256), out[0]
    assert out[1] == (re_flags := out[1][0], "42", 1) and re_flags & 2 and re_flags & 64
    assert out[2][0] == [(b"a", b"1"), (b"bb", b""), (b"c", b"x")], out[2]
    assert out[3] == ["FutureWarning"], out[3]
    assert out[4] == "Ab\nCd" and out[5] == "missing ), unterminated subpattern"
print("ok")
