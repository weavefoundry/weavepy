"""Ecosystem probe: tqdm — iteration wrapper over a generator, manual
updates, and the bar_format/postfix output shape."""

import io

import tqdm
from tqdm import tqdm as bar


def gen():
    yield from range(50)


# wrapping a generator preserves the payload
buf = io.StringIO()
seen = list(bar(gen(), file=buf, total=50))
assert seen == list(range(50))
out = buf.getvalue()
assert "50/50" in out, out
assert "100%" in out, out

# manual update + postfix
buf = io.StringIO()
with bar(total=4, file=buf, postfix={"loss": 0.25}) as t:
    for _ in range(4):
        t.update(1)
out = buf.getvalue()
assert "4/4" in out, out
assert "loss" in out, out

# format_meter is deterministic text (no tty games)
s = tqdm.tqdm.format_meter(n=30, total=60, elapsed=1.0)
assert "50%" in s and "30/60" in s, s

print("tqdm ok", tqdm.__version__)
