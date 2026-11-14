"""The csv reader takes runs of ordinary characters whole; what it yields is
what a character at a time yields, for every dialect feature that gives a
character meaning, and at the field size limit."""

import csv
import io


def rows(text, **kw):
    return list(csv.reader(io.StringIO(text, newline=""), **kw))


assert rows("a,b,c\r\n1,2,3\r\n") == [["a", "b", "c"], ["1", "2", "3"]]
assert rows("a,b,c\n\n1,,3\n") == [["a", "b", "c"], [], ["1", "", "3"]]
assert rows('x,"a,b",y\n') == [["x", "a,b", "y"]]
assert rows('"multi\nline","q""uote"\n') == [["multi\nline", 'q"uote']]
assert rows('"a\r\nb",c\r\n') == [["a\r\nb", "c"]]
assert rows('ab"cd,e"f\n') == [['ab"cd', 'e"f']]
assert rows("a\\,b,c\n", escapechar="\\") == [["a,b", "c"]]
assert rows('"a\\"b",c\n', escapechar="\\", doublequote=False) == [['a"b', "c"]]
assert rows("a\\\nb,c\n", escapechar="\\") == [["a\nb", "c"]]
assert rows("a;b;c\n", delimiter=";") == [["a", "b", "c"]]
assert rows("a|'b|c'|d\n", delimiter="|", quotechar="'") == [["a", "b|c", "d"]]
assert rows('a,"b,c\n', quoting=csv.QUOTE_NONE) == [["a", '"b', "c"]]
assert rows("a,  b, c\n", skipinitialspace=True) == [["a", "b", "c"]]
assert rows('1,2.5,"x"\n', quoting=csv.QUOTE_NONNUMERIC) == [[1.0, 2.5, "x"]]
assert rows("héllo,wörld 世界,\U0001f600\n") == [["héllo", "wörld 世界", "\U0001f600"]]
assert rows("a§b§c\n", delimiter="§") == [["a", "b", "c"]]
assert rows("a,\u00abb,c\u00ab,d\n", quotechar="\u00ab") == [["a", "b,c", "d"]]
assert rows("last,no newline") == [["last", "no newline"]]
assert rows("") == [] and rows("\n") == [[]]
assert list(csv.reader(["a,b", "c,d\n", "e,f\r\n"])) == [["a", "b"], ["c", "d"], ["e", "f"]]

try:
    rows('a,"b\n', strict=True)
except csv.Error:
    pass
else:
    raise AssertionError("unterminated quote in strict mode")
try:
    rows('"a"b,c\n', strict=True)
except csv.Error:
    pass
else:
    raise AssertionError("text after a closing quote in strict mode")
try:
    list(csv.reader(["a\nb,c"]))
except csv.Error:
    pass
else:
    raise AssertionError("a newline inside an unquoted field")

# The field size limit, exactly at and one past it, in and out of quotes.
old = csv.field_size_limit(10)
try:
    assert rows("0123456789,ab\n") == [["0123456789", "ab"]]
    assert rows('"0123456789",ab\n') == [["0123456789", "ab"]]
    for text in ("0123456789a,ab\n", '"0123456789a",ab\n', "x,0123456789a\n"):
        try:
            rows(text)
        except csv.Error as e:
            assert "field larger than field limit" in str(e)
        else:
            raise AssertionError("field over the limit: %r" % text)
finally:
    csv.field_size_limit(old)

# The reader's own state.
r = csv.reader(io.StringIO("a,b\r\nc,d\r\n", newline=""))
assert r.line_num == 0 and next(r) == ["a", "b"] and r.line_num == 1
assert r.dialect.delimiter == "," and next(r) == ["c", "d"] and r.line_num == 2
big = "".join("%d,name%d,%s,\"x,y\"\r\n" % (i, i, i * 1.5) for i in range(5000))
out = rows(big)
assert len(out) == 5000 and out[4999] == ["4999", "name4999", "7498.5", "x,y"]
d = list(csv.DictReader(io.StringIO("a,b\n1,2\n3,4\n")))
assert d == [{"a": "1", "b": "2"}, {"a": "3", "b": "4"}]
print("ok")
