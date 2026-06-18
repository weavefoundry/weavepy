"""WeavePy `xml.etree.ElementTree` — a lite implementation.

Supports the most-used surface:

* `Element(tag, attrib={}, **extra)` — element nodes with `text`,
  `tail`, `tag`, `attrib`, `children`.
* `SubElement(parent, tag, attrib={}, **extra)` — append child.
* `Comment`, `ProcessingInstruction`, `CDATA` — sentinel factories.
* `ElementTree(element=None, file=None)` — tree wrapper with
  `getroot`, `parse`, `write`.
* `parse(file)`, `fromstring(text)`, `tostring(element)`,
  `iterparse(file)` — convenience APIs.

The parser is a hand-rolled scanner — it handles attributes, nested
elements, text/tail content, and CDATA. Namespaces, DTDs, XInclude,
and entity expansion beyond the five core entities are out of scope.
"""

import re as _re
from io import StringIO


class ParseError(SyntaxError):
    def __init__(self, msg, position=None):
        SyntaxError.__init__(self, msg)
        self.position = position


_TAG_RE = _re.compile(r"<(/?)([A-Za-z_][A-Za-z0-9_:\.-]*)((?:\s+[^>]*)?)(/?)>")
_ATTR_RE = _re.compile(r'([A-Za-z_][A-Za-z0-9_:\.-]*)\s*=\s*(?:"([^"]*)"|\'([^\']*)\')')

_ENTITIES = {
    "&amp;": "&",
    "&lt;": "<",
    "&gt;": ">",
    "&quot;": '"',
    "&apos;": "'",
}


def _unescape(s):
    for k, v in _ENTITIES.items():
        s = s.replace(k, v)
    return s


def _escape(s):
    return (
        s.replace("&", "&amp;")
         .replace("<", "&lt;")
         .replace(">", "&gt;")
    )


def _iterfind(elem, path):
    """A small subset of ElementPath sufficient for the bundled stdlib.

    Supports ``.`` (self), a leading ``./``, the ``*`` child wildcard, exact
    tag steps, ``/``-separated multi-step paths, and the ``//`` descendant
    axis (e.g. ``.//tag``). Predicates (``[...]``) are not implemented.
    """
    if not path:
        return
    if path == ".":
        yield elem
        return
    if path.startswith("./"):
        path = path[2:]
    # Split into steps; an empty step (from "//") means "descendant axis".
    steps = path.split("/")

    def walk(node, steps):
        if not steps:
            yield node
            return
        step, rest = steps[0], steps[1:]
        if step == "":
            # Descendant-or-self axis for the following step.
            nxt = rest[0] if rest else "*"
            tail = rest[1:]
            for d in node.iter():
                if d is node:
                    continue
                if nxt == "*" or d.tag == nxt:
                    yield from walk(d, tail)
            return
        if step == ".":
            yield from walk(node, rest)
            return
        for c in node._children:
            if step == "*" or c.tag == step:
                yield from walk(c, rest)

    yield from walk(elem, steps)


class Element:
    def __init__(self, tag, attrib=None, **extra):
        self.tag = tag
        self.attrib = dict(attrib) if attrib else {}
        if extra:
            self.attrib.update(extra)
        self.text = None
        self.tail = None
        self._children = []

    def __iter__(self):
        return iter(self._children)

    def __len__(self):
        return len(self._children)

    def __getitem__(self, idx):
        return self._children[idx]

    def append(self, subelement):
        self._children.append(subelement)

    def extend(self, items):
        self._children.extend(items)

    def insert(self, index, subelement):
        self._children.insert(index, subelement)

    def remove(self, subelement):
        self._children.remove(subelement)

    def get(self, key, default=None):
        return self.attrib.get(key, default)

    def set(self, key, value):
        self.attrib[key] = value

    def keys(self):
        return list(self.attrib.keys())

    def items(self):
        return list(self.attrib.items())

    def find(self, path):
        for c in _iterfind(self, path):
            return c
        return None

    def findall(self, path):
        return list(_iterfind(self, path))

    def iterfind(self, path):
        return _iterfind(self, path)

    def findtext(self, path, default=None):
        c = self.find(path)
        if c is None:
            return default
        return c.text or ""

    def iter(self, tag=None):
        if tag is None or self.tag == tag:
            yield self
        for c in self._children:
            for sub in c.iter(tag):
                yield sub

    def itertext(self):
        if self.text:
            yield self.text
        for c in self._children:
            for s in c.itertext():
                yield s
            if c.tail:
                yield c.tail

    def __repr__(self):
        return "<Element {!r}>".format(self.tag)


def SubElement(parent, tag, attrib=None, **extra):
    elem = Element(tag, attrib, **extra)
    parent.append(elem)
    return elem


def Comment(text=None):
    elem = Element("<comment>")
    elem.text = text
    return elem


def ProcessingInstruction(target, text=None):
    elem = Element("<processing>")
    elem.text = "{} {}".format(target, text or "")
    return elem


PI = ProcessingInstruction


def CDATA(text=None):
    elem = Element("<cdata>")
    elem.text = text
    return elem


def _serialize(elem, out):
    if elem.tag == "<comment>":
        out.write("<!--")
        out.write(elem.text or "")
        out.write("-->")
    else:
        out.write("<")
        out.write(elem.tag)
        for k, v in elem.attrib.items():
            out.write(' {}="{}"'.format(k, _escape(str(v))))
        if not elem._children and not elem.text:
            out.write(" />")
        else:
            out.write(">")
            if elem.text:
                out.write(_escape(elem.text))
            for c in elem._children:
                _serialize(c, out)
                if c.tail:
                    out.write(_escape(c.tail))
            out.write("</{}>".format(elem.tag))


def tostring(element, encoding=None, method=None, *, xml_declaration=None, default_namespace=None, short_empty_elements=True):
    buf = StringIO()
    if xml_declaration:
        buf.write('<?xml version="1.0" encoding="utf-8"?>\n')
    _serialize(element, buf)
    s = buf.getvalue()
    if encoding == "unicode":
        return s
    if encoding is None:
        return s.encode("utf-8")
    return s.encode(encoding)


def fromstring(text):
    if isinstance(text, (bytes, bytearray)):
        text = text.decode("utf-8")
    return _parse_text(text)


def _parse_text(text):
    stack = []
    root = None
    pos = 0
    n = len(text)
    while pos < n:
        if text[pos] != "<":
            # Run of text.
            nxt = text.find("<", pos)
            chunk = text[pos:nxt] if nxt != -1 else text[pos:]
            if stack:
                top = stack[-1]
                if top._children:
                    top._children[-1].tail = (top._children[-1].tail or "") + _unescape(chunk)
                else:
                    top.text = (top.text or "") + _unescape(chunk)
            pos = nxt if nxt != -1 else n
            continue
        if text.startswith("<!--", pos):
            end = text.find("-->", pos)
            if end == -1:
                raise ParseError("unterminated comment")
            pos = end + 3
            continue
        if text.startswith("<![CDATA[", pos):
            end = text.find("]]>", pos)
            if end == -1:
                raise ParseError("unterminated CDATA")
            data = text[pos + 9:end]
            if stack:
                top = stack[-1]
                if top._children:
                    top._children[-1].tail = (top._children[-1].tail or "") + data
                else:
                    top.text = (top.text or "") + data
            pos = end + 3
            continue
        if text.startswith("<?", pos):
            end = text.find("?>", pos)
            if end == -1:
                raise ParseError("unterminated PI")
            pos = end + 2
            continue
        if text.startswith("<!", pos):
            end = text.find(">", pos)
            if end == -1:
                raise ParseError("unterminated declaration")
            pos = end + 1
            continue
        m = _TAG_RE.match(text, pos)
        if not m:
            raise ParseError("malformed tag at {}".format(pos))
        closing, tag, attrs, self_close = m.groups()
        pos = m.end()
        if closing:
            if not stack or stack[-1].tag != tag:
                raise ParseError("unbalanced close tag: {}".format(tag))
            stack.pop()
            continue
        attrib = {}
        for am in _ATTR_RE.finditer(attrs):
            k, v1, v2 = am.groups()
            attrib[k] = _unescape(v1 if v1 is not None else v2)
        elem = Element(tag, attrib)
        if stack:
            stack[-1].append(elem)
        else:
            root = elem
        if not self_close:
            stack.append(elem)
    if stack:
        raise ParseError("unclosed element: {}".format(stack[-1].tag))
    if root is None:
        raise ParseError("no root element")
    return root


class ElementTree:
    def __init__(self, element=None, file=None):
        self._root = element
        if file is not None:
            self.parse(file)

    def getroot(self):
        return self._root

    def parse(self, source, parser=None):
        if hasattr(source, "read"):
            text = source.read()
        else:
            with open(source, "rb") as f:
                text = f.read()
        if isinstance(text, (bytes, bytearray)):
            text = text.decode("utf-8")
        self._root = _parse_text(text)
        return self._root

    def write(self, file, encoding=None, xml_declaration=None, default_namespace=None,
              method=None, *, short_empty_elements=True):
        data = tostring(self._root, encoding=encoding or "utf-8",
                         xml_declaration=xml_declaration)
        if hasattr(file, "write"):
            if isinstance(data, bytes) and hasattr(file, "mode") and "b" not in file.mode:
                file.write(data.decode("utf-8"))
            else:
                file.write(data)
        else:
            mode = "wb" if isinstance(data, bytes) else "w"
            with open(file, mode) as f:
                f.write(data)

    def find(self, path):
        return self._root.find(path) if self._root is not None else None

    def findall(self, path):
        return self._root.findall(path) if self._root is not None else []

    def findtext(self, path, default=None):
        return self._root.findtext(path, default) if self._root is not None else default

    def iter(self, tag=None):
        if self._root is None:
            return iter([])
        return self._root.iter(tag)


def parse(source, parser=None):
    return ElementTree(file=source)


def iterparse(source, events=None):
    tree = ElementTree(file=source)
    target_events = events or ("end",)
    out = []
    for elem in tree.getroot().iter():
        for ev in target_events:
            out.append((ev, elem))
    return iter(out)


def register_namespace(prefix, uri):
    pass


def dump(elem):
    import sys
    s = tostring(elem, encoding="unicode")
    sys.stdout.write(s + "\n")


__all__ = [
    "Element", "SubElement", "Comment", "ProcessingInstruction", "PI", "CDATA",
    "ElementTree", "ParseError", "tostring", "fromstring", "parse", "iterparse",
    "register_namespace", "dump",
]
