"""Regression coverage for the faithful ``re`` / ``_sre`` engine.

Exercises the CPython-ported backtracking matcher: quantifiers,
groups, backreferences, look-around, alternation, flags, Unicode and
bytes patterns, plus the zero-width scanning behaviour that previously
looped forever. All expectations were diffed against CPython 3.13.
"""

import re

# --- basic matching / search ------------------------------------------
assert re.match("abc", "abcdef").span() == (0, 3)
assert re.search("cd", "abcdef").span() == (2, 4)
assert re.match("abc", "xabc") is None
assert re.fullmatch("a.c", "abc") is not None
assert re.fullmatch("a.c", "abcd") is None

# --- quantifiers -------------------------------------------------------
assert re.findall(r"a{2,4}", "a aa aaa aaaa aaaaa") == ["aa", "aaa", "aaaa", "aaaa"]
assert re.findall(r"a{,3}", "aaaaa") == ["aaa", "aa", ""]
assert re.findall(r"<.+>", "<a><b>") == ["<a><b>"]      # greedy
assert re.findall(r"<.+?>", "<a><b>") == ["<a>", "<b>"]  # lazy
assert re.search(r"a.*c", "abcabc").span() == (0, 6)
assert re.search(r"a.*?c", "abcabc").span() == (0, 3)

# --- alternation / groups ---------------------------------------------
assert re.fullmatch("a|ab", "ab").group(0) == "ab"   # toplevel branch guard
assert re.match(r"(a)(b)(c)", "abc").groups() == ("a", "b", "c")
assert re.match(r"(a)(b)?(c)", "ac").groups() == ("a", None, "c")
assert re.match(r"(a)(b)?(c)", "ac").groups("X") == ("a", "X", "c")
m = re.match(r"(?P<y>\d{4})-(?P<m>\d{2})", "2026-05")
assert m.groupdict() == {"y": "2026", "m": "05"}
assert m["y"] == "2026" and m.group("m") == "05"
assert m.lastgroup == "m" and m.lastindex == 2

# --- backreferences ----------------------------------------------------
assert re.findall(r"(\w)\1", "aa bb cd ee") == ["a", "b", "e"]
assert re.search(r"(?P<q>['\"]).*?(?P=q)", "say 'hi' done").group(0) == "'hi'"
assert re.findall(r"<(\w+)>.*?</\1>", "<b>x</b><i>y</i>") == ["b", "i"]

# --- look-around -------------------------------------------------------
assert re.findall(r"\d+(?= dollars)", "100 dollars, 50 cents") == ["100"]
assert re.findall(r"\d+(?! dollars)", "100 dollars 50 cents") == ["10", "50"]
assert re.findall(r"(?<=\$)\d+", "$100 and 50") == ["100"]
assert re.findall(r"(?<!\$)\b\d+", "$100 and 50") == ["50"]

# --- zero-width scanning (previously looped forever) -------------------
assert re.findall(r"x*", "xxab") == ["xx", "", "", ""]
assert re.findall(r"(a)*", "aab") == ["a", "", ""]
assert re.findall(r"(a?)*", "aaa") == ["", ""]
assert re.findall(r"(a|b)*", "abc") == ["b", "", ""]
assert re.sub(r"x*", "-", "xxab") == "--a-b-"
assert re.sub(r"(a)*", "-", "aab") == "--b-"
assert re.split(r"x*", "axbxc") == ["", "a", "", "b", "", "c", ""]
assert re.split(r"(?<=,)", "a,b,c") == ["a,", "b,", "c"]

# --- substitution / templates -----------------------------------------
assert re.sub(r"(\w+)@(\w+)", r"\2.\1", "user@host") == "host.user"
assert re.sub(r"(?P<n>\w+)", r"[\g<n>]", "hi there") == "[hi] [there]"
assert re.subn(r"\d+", "#", "a1b22c333") == ("a#b#c#", 3)
assert re.sub(r"a", "b", "aaaa", count=2) == "bbaa"
assert re.sub(r"\d+", lambda mo: str(int(mo.group()) * 2), "1 2 3") == "2 4 6"
assert re.match(r"(\w+) (\w+)", "John Smith").expand(r"\2 \1") == "Smith John"

# --- flags -------------------------------------------------------------
assert re.findall(r"abc", "ABC abc", re.I) == ["ABC", "abc"]
assert re.findall(r"(?i)abc", "ABC abc") == ["ABC", "abc"]
assert re.findall(r"(?i:ab)c", "ABc abc ABC") == ["ABc", "abc"]
assert re.findall(r"^\w+", "foo\nbar\nbaz", re.M) == ["foo", "bar", "baz"]
assert re.findall(r"a.b", "a\nb", re.S) == ["a\nb"]
assert re.findall(r"""\d +  # int
                      \.    # dot
                      \d *  # frac""", "3.14 x", re.X) == ["3.14"]

# --- unicode vs ascii --------------------------------------------------
assert re.findall(r"\w+", "café déjà") == ["café", "déjà"]
assert re.findall(r"\w+", "café", re.A) == ["caf"]
assert re.findall(r"\d+", "\uff11\uff12 99") == ["\uff11\uff12", "99"]  # fullwidth
assert re.match(r"(?i)\u00e9", "\u00c9") is not None   # é ~ É
assert re.findall(r"\s", "a b\tc\u00a0d") == [" ", "\t", "\u00a0"]

# --- bytes patterns ----------------------------------------------------
assert re.findall(rb"\d+", b"a12b345") == [b"12", b"345"]
assert re.sub(rb"\s+", b"_", b"a  b\tc") == b"a_b_c"
assert re.match(rb"(\w+)@(\w+)", b"user@host").groups() == (b"user", b"host")
assert re.split(rb"[,;]", b"a,b;c") == [b"a", b"b", b"c"]
assert re.findall(rb"[\x00-\x02]", bytes(range(5))) == [b"\x00", b"\x01", b"\x02"]

# --- possessive / atomic ----------------------------------------------
assert re.search(r"(?>a+)b", "aaab") is not None
assert re.search(r"(?>a+)a", "aaa") is None     # atomic: no give-back
assert re.findall(r"a*+", "aaab") == ["aaa", "", ""]

# --- escape / error semantics -----------------------------------------
assert re.escape("a.b*c+d?") == r"a\.b\*c\+d\?"
for bad, msg in [
    (r"(?P<n>a)(?P<n>b)", "redefinition"),
    (r"a{2,1}", "min repeat greater than max repeat"),
    (r"(?P=undef)", "unknown group name"),
    (r"[", "unterminated character set"),
    (r"a\1", "invalid group reference"),
]:
    try:
        re.compile(bad)
    except re.error as e:
        assert msg in str(e), (bad, str(e))
    else:
        raise AssertionError("expected re.error for %r" % bad)

# --- compiled Pattern surface -----------------------------------------
p = re.compile(r"(\d+)")
assert p.pattern == r"(\d+)" and p.groups == 1
assert [mo.group(1) for mo in p.finditer("a1b22c")] == ["1", "22"]
assert isinstance(re.match(r"x", "x").re, re.Pattern)

print("ok")
