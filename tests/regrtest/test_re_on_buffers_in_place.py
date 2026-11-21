"""Regular expressions over a `bytearray` or a `memoryview`.

A bytes pattern matches a `bytearray` or a plain `memoryview` window
where its bytes are, as it matches a `bytes` object: searching a large
buffer costs what the search reads, not a conversion of the whole
buffer. The pieces it returns are `bytes`, and every result equals the
result for the same bytes as a `bytes` object.
"""

import re
import time
import unittest

PATTERNS = [rb"a", rb"(a)(b)?", rb"\d+", rb"(?P<x>\w+)@(\w+)", rb"^ab", rb"b$", rb"", rb"(a|b)*", rb"[^a]+", rb"\xfe\xff", rb"(?i)AB", rb"\bab\b"]
SUBJECTS = [b"", b"a", b"ab", b"abab 12 cd 345", b"me@host and you@there", b"xxabxx", b"\xfe\xff\x00\x01" * 3, b"AbaB ab"]


def results(pattern, subject):
    m = pattern.search(subject)
    return (
        None if m is None else (m.group(), m.groups(), m.span(), m.groupdict(), m.lastindex),
        None if (m := pattern.match(subject)) is None else (m.group(0), m.span()),
        None if (m := pattern.fullmatch(subject)) is None else m.span(),
        pattern.findall(subject),
        pattern.split(subject),
        [m.span() for m in pattern.finditer(subject)],
        pattern.sub(b"-", subject),
        pattern.subn(b"<\\g<0>>", subject),
        pattern.sub(lambda m: b"#", subject),
        pattern.findall(subject, 1, 5),
        pattern.search(subject, 2) is not None,
    )


def views(data):
    return {
        "bytearray": bytearray(data),
        "memoryview": memoryview(data),
        "window": memoryview(b"zz" + data + b"zz")[2:-2],
        "bytearray window": memoryview(bytearray(b"z" + data))[1:],
    }


class SameResultsTests(unittest.TestCase):
    def test_every_buffer_kind_matches_like_bytes(self):
        for raw in PATTERNS:
            pattern = re.compile(raw)
            for data in SUBJECTS:
                want = results(pattern, data)
                for name, subject in views(data).items():
                    with self.subTest(pattern=raw, data=data, kind=name):
                        self.assertEqual(results(pattern, subject), want)

    def test_pieces_are_bytes(self):
        for name, subject in views(b"key=value; other=thing").items():
            with self.subTest(kind=name):
                m = re.search(rb"(\w+)=(\w+)", subject)
                self.assertIs(type(m.group()), bytes)
                self.assertIs(type(m.group(1)), bytes)
                self.assertIs(m.string, subject)
                for piece in re.findall(rb"\w+", subject) + re.split(rb"[=;]", subject):
                    self.assertIs(type(piece), bytes)
                self.assertIs(type(re.sub(rb"=", b":", subject)), bytes)
                self.assertEqual(re.findall(rb"(\w+)=(\w+)", subject), [(b"key", b"value"), (b"other", b"thing")])
        everything = re.search(rb".*", bytearray(b"abc"))
        self.assertEqual(everything.group(), b"abc")
        self.assertIs(type(everything.group()), bytes)

    def test_a_buffer_changed_after_the_match(self):
        buf = bytearray(b"abc abc")
        m = re.search(rb"b(c)", buf)
        buf[1] = ord("X")
        self.assertEqual(m.span(), (1, 3))
        buf.extend(b" more")
        self.assertEqual(re.findall(rb"\w+", buf), [b"aXc", b"abc", b"more"])

    def test_wrong_kinds(self):
        with self.assertRaises(TypeError):
            re.search("a", bytearray(b"a"))
        with self.assertRaises(TypeError):
            re.search("a", memoryview(b"a"))
        with self.assertRaises(TypeError):
            re.search(b"a", "a")


class CostTests(unittest.TestCase):
    def test_a_search_costs_what_it_reads(self):
        pattern = re.compile(rb"\xfe\xff\x00\x01")

        def best(buf):
            t = float("inf")
            for _ in range(3):
                start = time.perf_counter()
                for _ in range(200):
                    pattern.search(buf)
                t = min(t, time.perf_counter() - start)
            return t

        small = best(bytearray(range(256)) * 4)
        large = best(bytearray(range(256)) * 4000)
        # (The match is near the front either way; converting the whole
        # buffer per search made the large one a thousand times slower.)
        self.assertLess(large, small * 20 + 0.005)
        self.assertLess(best(memoryview(bytes(range(256)) * 4000)), small * 20 + 0.005)


if __name__ == "__main__":
    unittest.main()
