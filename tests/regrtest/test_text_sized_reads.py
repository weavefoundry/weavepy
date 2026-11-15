"""A text stream's `read(n)` returns `n` characters, whatever the codec
makes of a byte: single-byte codecs, UTF-8 with and without a byte-order
mark, the double-byte codecs, and universal newlines, where a `\\r\\n` is
one character and may straddle two reads."""

import os
import random
import tempfile

ALPHABETS = {
    "utf-8": "ab éö中\U0001f600\r\n\n\r",
    "utf-8-sig": "ab éö中\U0001f600\r\n\n\r",
    "latin-1": "ab éöàÿ\r\n\n\r",
    "ascii": "abc d\r\n\n\r",
    "cp1252": "ab éö€’\r\n\n\r",
    "koi8-r": "ab при\r\n\n\r",
    "shift_jis": "ab 日本語テｶ\r\n\n\r",
    "euc-jp": "ab 日本語テ\r\n\n\r",
    "gbk": "ab 中文测\r\n\n\r",
    "gb18030": "ab 中文\U0001f600é\r\n\n\r",
    "utf-16": "ab é中\U0001f600\r\n\n\r",
}
SIZES = [0, 1, 1, 2, 3, 4, 7, 16, 100, 8191, 8192, 8193]


def translated(text, newline):
    if newline is None:
        return text.replace("\r\n", "\n").replace("\r", "\n")
    return text


def check(path, enc, text, newline, rng):
    want = translated(text, newline)
    sizes = [rng.choice(SIZES) for _ in range(rng.randint(1, 10))]
    with open(path, encoding=enc, newline=newline) as f:
        at = 0
        for k in sizes:
            part = f.read(k)
            assert part == want[at:at + k], (enc, newline, k, at, part[:20])
            at += len(part)
        assert f.read() == want[at:], (enc, newline, sizes)


def known_cases(d):
    path = os.path.join(d, "known")
    with open(path, "wb") as f:
        f.write("héllo wörld".encode("latin-1"))
    with open(path, encoding="latin-1") as f:
        assert [f.read(3), f.read(1), f.read(5), f.read()] == ["hél", "l", "o wör", "ld"]
    with open(path, "wb") as f:
        f.write(b"a\r\nb\r\nc\rd\ne\r\n")
    with open(path) as f:
        assert [f.read(3), f.read(1), f.read(2), f.read()] == ["a\nb", "\n", "c\n", "d\ne\n"]
    # A `\r\n` across two reads is still one line end.
    with open(path) as f:
        assert [f.read(2), f.read(2)] == ["a\n", "b\n"]
    with open(path, newline="") as f:
        assert [f.read(2), f.read(2)] == ["a\r", "\nb"]
    with open(path, "wb") as f:
        f.write("日本語abc".encode("shift_jis"))
    with open(path, encoding="shift_jis") as f:
        assert [f.read(1), f.read(2), f.read(2), f.read()] == ["日", "本語", "ab", "c"]
    with open(path, "wb") as f:
        f.write("héllo".encode("utf-8-sig"))
    with open(path, encoding="utf-8-sig") as f:
        assert [f.read(3), f.read()] == ["hél", "lo"]
    # Bytes an error handler drops or replaces don't end the read early.
    with open(path, "wb") as f:
        f.write(b"ab\xff\xfecd\xffef")
    with open(path, encoding="ascii", errors="ignore") as f:
        assert [f.read(3), f.read(2), f.read()] == ["abc", "de", "f"]
    with open(path, encoding="utf-8", errors="replace") as f:
        assert [f.read(3), f.read()] == ["ab�", "�cd�ef"]


def main():
    rng = random.Random(20261114)
    with tempfile.TemporaryDirectory() as d:
        known_cases(d)
        path = os.path.join(d, "text")
        for enc, alphabet in ALPHABETS.items():
            for length in (0, 1, 2, 17, 300, 9000):
                text = "".join(rng.choice(alphabet) for _ in range(length))
                with open(path, "w", encoding=enc, newline="") as f:
                    f.write(text)
                for newline in (None, "", "\r\n"):
                    check(path, enc, text, newline, rng)


main()
print("ok")
