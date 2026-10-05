"""xml.etree.ElementTree over its native bodies.

WeavePy gives the Python `Element`, `TreeBuilder` and `XMLParser` classes,
`SubElement`, the serializer and `ElementPath`'s cached selectors native
bodies that fall back to the Python code for anything unusual. These checks
compare the accelerated module with the pure-Python one (an import that
blocks `_elementtree`) and pin the behaviors the natives must keep: element
construction and mutation, subclasses and shadowed methods, lazy iteration
that sees mutation, `find`/`findall`/`findtext` over cached paths,
serialization (namespaces, comments, escaping, errors), parsing through
`XMLParser`, `XMLPullParser` and `iterparse`, and pickling.
"""

import copy
import importlib
import io
import pickle
import sys

import xml.etree.ElementTree as ET


def import_pure():
    """`xml.etree.ElementTree` imported with `_elementtree` blocked."""
    names = ("xml.etree.ElementTree", "_elementtree")
    saved = {n: sys.modules.get(n) for n in names}
    sys.modules.pop("xml.etree.ElementTree")
    sys.modules["_elementtree"] = None
    try:
        return importlib.import_module("xml.etree.ElementTree")
    finally:
        for n, m in saved.items():
            if m is None:
                sys.modules.pop(n, None)
            else:
                sys.modules[n] = m


pyET = import_pure()
MODULES = (ET, pyET)


def dump(e, M=ET):
    return M.tostring(e, encoding="unicode")


def build(M):
    root = M.Element("catalog", version="2")
    for i in range(30):
        book = M.SubElement(root, "book", {"n": str(i)}, id="b%d" % i,
                            lang="en" if i % 2 else "fr")
        M.SubElement(book, "title").text = "Title & <%d>" % i
        M.SubElement(book, "price").text = "%.2f" % (i * 1.1)
        tags = M.SubElement(book, "tags")
        for t in range(i % 4):
            M.SubElement(tags, "tag").text = "t%d" % t
        book.tail = "\n" if i % 3 else None
    return root


# Construction, serialization and parsing agree with the Python code.
for M in MODULES:
    root = build(M)
    s = M.tostring(root, encoding="unicode")
    assert s == pyET.tostring(build(pyET), encoding="unicode"), M
    back = M.fromstring(s)
    assert M.tostring(back, encoding="unicode") == s
    assert M.tostring(root) == s.encode("ascii")
    assert len(back) == 30 and back[0].get("id") == "b0" and back[-1].tag == "book"
    assert back.get("missing", 7) == 7 and back.get("version") == "2"
    assert [e.tag for e in back[5]] == ["title", "price", "tags"]
    assert sum(1 for _ in back.iter("tag")) == sum(i % 4 for i in range(30))
    assert len(list(back.iter("*"))) == len(list(back.iter()))
    assert len(back.findall("book[@lang='fr']")) == 15
    assert len(back.findall("book[@lang='fr']")) == 15  # cached selector
    assert len(back.findall("book[@lang!='fr']")) == 15
    assert len(back.findall(".//tag")) == len(back.findall(".//tag"))
    assert len(back.findall("*/tags/tag")) == sum(i % 4 for i in range(30))
    assert len(back.findall("./book[@id]")) == 30
    assert back.find("book/title").text == "Title & <0>"
    assert back.findtext("book/price") == "0.00"
    assert back.findtext("nothing", "dflt") == "dflt"
    assert back.find("nothing") is None
    assert "".join(back[7].itertext()) == "Title & <7>7.70t0t1t2"
    assert M.tostring(root, method="text", encoding="unicode") == \
        pyET.tostring(build(pyET), method="text", encoding="unicode")


# Element methods: mutation, indexing, attributes.
for M in MODULES:
    e = M.Element("e", {"a": "1"}, b="2")
    assert e.attrib == {"a": "1", "b": "2"} and e.text is None and e.tail is None
    e.insert(0, M.Element("x"))
    e.insert(-1, M.Element("y"))
    e.insert(10, M.Element("z"))
    e.extend([M.Element("w")])
    e.extend(M.Element(t) for t in "uv")
    assert [c.tag for c in e] == ["y", "x", "z", "w", "u", "v"]
    assert e[-1].tag == "v" and len(e) == 6
    try:
        e[10]
    except IndexError:
        pass
    else:
        raise AssertionError("no IndexError")
    for bad in (lambda: e.append(5), lambda: e.extend([e, 5]), lambda: e.insert(0, "x")):
        try:
            bad()
        except TypeError:
            pass
        else:
            raise AssertionError("no TypeError")
    assert len(e) == 7  # extend appended `e` before the bad item
    del e[-1]
    e.set("c", "3")
    assert e.get("c") == "3" and list(e.keys()) == list(e.attrib)
    assert e.makeelement("m", {"k": "v"}).attrib == {"k": "v"}
    try:
        M.Element("x", [1])
    except TypeError:
        pass
    else:
        raise AssertionError("no TypeError")
    sub = M.SubElement(e, "s", attrib={"q": "1"}, r="2")
    assert sub.attrib == {"q": "1", "r": "2"} and e[-1] is sub
    attrib = {"k": "v"}
    made = M.Element("t", attrib)
    attrib["k"] = "changed"
    assert made.get("k") == "v"  # the element holds a copy


# Subclasses and instance attributes that shadow methods take the Python
# code, which calls the overrides. (CPython's C accelerator walks
# subclasses' children itself.)
for M in MODULES if sys.implementation.name != "cpython" else (pyET,):
    calls = []

    class Sub(M.Element):
        def iter(self, tag=None):
            calls.append("iter")
            return super().iter(tag)

        def itertext(self):
            calls.append("itertext")
            yield "sub"

    root = M.Element("r")
    root.append(Sub("s"))
    root[0].append(M.Element("c"))
    assert [x.tag for x in root.iter()] == ["r", "s", "c"]
    assert list(root.itertext()) == ["sub"]
    assert calls == ["iter", "itertext"]
    assert type(root[0].makeelement("m", {})) is Sub
    tb = M.TreeBuilder(element_factory=Sub)
    p = M.XMLParser(target=tb)
    p.feed("<a x='1'><b>t</b>tail<c/></a>")
    r = p.close()
    assert type(r) is Sub and dump(r, M) == '<a x="1"><b>t</b>tail<c /></a>'


# Iteration is lazy and sees mutation, like the Python generators.
for M in MODULES:
    r = M.fromstring("<a><b/><c/><d/></a>")
    seen = []
    for el in r.iter():
        seen.append(el.tag)
        if el.tag == "b":
            el.append(M.Element("b2"))
        if el.tag == "c":
            r.append(M.Element("e"))
    assert seen == ["a", "b", "b2", "c", "d", "e"], seen
    seen = []
    for el in r:
        seen.append(el.tag)
        if el.tag == "c":
            r.remove(el)
    assert seen == ["b", "c", "e"] and [c.tag for c in r] == ["b", "d", "e"]
    it = r.iter()
    next(it)
    r.clear()
    assert list(it) == []
    x = M.fromstring("<q>a<w>b</w>c<v/>d</q>")
    assert list(x.itertext()) == ["a", "b", "c", "d"]
    x.append(M.Comment("comment"))
    x[-1].tail = "after"
    assert list(x.itertext()) == ["a", "b", "c", "d", "after"]
    assert [e.tag for e in x.iter(M.Comment)] == [M.Comment]
    for it in (x.iter(), x.itertext()):
        for dumper in (copy.copy, pickle.dumps):
            try:
                dumper(it)
            except (TypeError, pickle.PicklingError):
                pass
            else:
                raise AssertionError("an element iterator copied")


# Text truthiness goes through `__bool__`, and the original object is
# yielded (gh-25902).
class Text:
    def __bool__(self):
        e.text = "changed"
        return True


for M in MODULES:
    e = M.Element("tag")
    e.text = Text()
    t = next(e.itertext())
    assert isinstance(t, Text) and e.text == "changed"


# Serialization: namespaces, comments, PIs, escaping, empty elements.
for M in MODULES:
    M.register_namespace("foo", "http://foo/")
for M in MODULES:

    def ns_tree(M):
        r = M.Element("{http://foo/}root", {"{http://bar/}a": "1", "plain": 'x\n"<&>\t\r'})
        M.SubElement(r, "{http://foo/}child").text = "t&<>"
        M.SubElement(r, "{http://baz/}c2", attr="v").tail = "tail&"
        r.append(M.Comment("a comment"))
        r.append(M.PI("target", "data"))
        return r

    r = ns_tree(M)
    for kwargs in ({}, {"short_empty_elements": False}, {"method": "html"}):
        got = M.tostring(r, encoding="unicode", **kwargs)
        assert got == pyET.tostring(ns_tree(pyET), encoding="unicode", **kwargs), (M, kwargs)
    assert M.tostring(r, encoding="unicode").startswith(
        '<foo:root xmlns:foo="http://foo/" xmlns:ns1="http://bar/" '
        'xmlns:ns2="http://baz/" ns1:a="1" plain="x&#10;&quot;&lt;&amp;&gt;&#09;&#13;">')
    try:
        M.tostring(r, encoding="unicode", default_namespace="http://foo/")
    except ValueError:
        pass
    else:
        raise AssertionError("no ValueError")
    d = M.Element("{http://foo/}a")
    M.SubElement(d, "{http://foo/}b")
    assert M.tostring(d, encoding="unicode", default_namespace="http://foo/") == \
        '<a xmlns="http://foo/"><b /></a>'
    bad = M.Element("x")
    bad.text = 5
    try:
        M.tostring(bad)
    except TypeError as exc:
        assert "cannot serialize" in str(exc)
    else:
        raise AssertionError("no TypeError")
    q = M.Element(M.QName("http://foo/", "q"), {M.QName("http://foo/", "k"): "v"})
    assert M.tostring(q, encoding="unicode") == \
        '<foo:q xmlns:foo="http://foo/" foo:k="v" />'
    out = io.StringIO()
    M.ElementTree(build(M)).write(out, encoding="unicode")
    assert out.getvalue() == pyET.tostring(build(pyET), encoding="unicode")


# Parsing: entities, namespaces, tails, errors, and parser targets.
for M in MODULES:
    doc = ("<?xml version='1.0'?><!DOCTYPE a><a xmlns='http://d/' x='&lt;'>"
           "&lt;&amp;<b>1</b>t<!--c--><?p d?></a>")
    r = M.fromstring(doc)
    assert dump(r, M) == ('<ns0:a xmlns:ns0="http://d/" x="&lt;">&lt;&amp;'
                       '<ns0:b>1</ns0:b>t</ns0:a>'), dump(r, M)
    try:
        M.fromstring("<a><b></a>")
    except M.ParseError as exc:
        assert exc.code == 7 and exc.position == (1, 8), (exc.code, exc.position)
    else:
        raise AssertionError("no ParseError")
    tb = M.TreeBuilder(insert_comments=True, insert_pis=True)
    p = M.XMLParser(target=tb)
    p.feed("<a><!--c--><?pi d?>x<b/>y</a>")
    r3 = p.close()
    assert dump(r3, M) == "<a><!--c--><?pi d?>x<b />y</a>"
    events = []

    class Target:
        def start(self, tag, attrib):
            events.append(("start", tag, attrib))

        def end(self, tag):
            events.append(("end", tag))

        def data(self, data):
            events.append(("data", data))

        def close(self):
            return "closed"

    p = M.XMLParser(target=Target())
    p.feed("<a k='v'>x<b/></a>")
    assert p.close() == "closed"
    assert events == [("start", "a", {"k": "v"}), ("data", "x"), ("start", "b", {}),
                      ("end", "b"), ("end", "a")]
    tb = M.TreeBuilder()
    tb.data("ignored")
    tb.start("t", {"a": "b"})
    tb.data("x")
    tb.data("y")
    tb.end("t")
    assert dump(tb.close(), M) == '<t a="b">xy</t>'
    ev = [(e, el.tag) for e, el in M.iterparse(
        io.BytesIO(b"<a><b x='1'>t</b><c/></a>"), events=("start", "end"))]
    assert ev == [("start", "a"), ("start", "b"), ("end", "b"), ("start", "c"),
                  ("end", "c"), ("end", "a")]
    pp = M.XMLPullParser(events=("start", "end"))
    pp.feed("<root><x>1</x>")
    first = [(e, el.tag) for e, el in pp.read_events()]
    pp.feed("</root>")
    assert first == [("start", "root"), ("start", "x"), ("end", "x")]
    assert [(e, el.tag) for e, el in pp.read_events()] == [("end", "root")]
    big = M.fromstring("<r>" + "".join("<i n='%d'>%s</i>" % (i, "v" * (i % 50))
                                       for i in range(3000)) + "</r>")
    assert len(big) == 3000 and big[2999].get("n") == "2999"
    assert big[49].text == "v" * 49 and big[50].text is None


# Copying and pickling keep the Python layout.
for M in MODULES:
    e = build(M)[3]
    for c in (copy.copy(e), copy.deepcopy(e)):
        assert dump(c, M) == dump(e, M)
    # (Only the imported module's classes pickle by reference.)
    for proto in range(2, pickle.HIGHEST_PROTOCOL + 1) if M is ET else ():
        assert dump(pickle.loads(pickle.dumps(e, proto)), M) == dump(e, M)


# The pure-Python import keeps Python functions.
assert type(pyET.SubElement).__name__ == "function"
assert type(pyET.Element.__init__).__name__ == "function"
