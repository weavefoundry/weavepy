"""Ecosystem probe: lxml (PyPI wheel) — etree round-trip, namespaced
XPath, XSLT, iterparse, lxml.html rewriting, XMLSchema validation,
deepcopy/pickle interop."""

import copy
import io
import pickle

from lxml import etree, html

# fromstring / tostring round-trip
doc = etree.fromstring(b"<root><child name='a'>text</child><child name='b'/></root>")
assert doc.tag == "root" and len(doc) == 2
assert doc[0].get("name") == "a" and doc[0].text == "text"
wire = etree.tostring(doc)
assert etree.tostring(etree.fromstring(wire)) == wire

# XPath with namespaces
ns_doc = etree.fromstring(
    b"<r xmlns:x='urn:probe'><x:item>1</x:item><x:item>2</x:item><item>skip</item></r>"
)
items = ns_doc.xpath("//p:item/text()", namespaces={"p": "urn:probe"})
assert items == ["1", "2"], items
assert ns_doc.xpath("count(//p:item)", namespaces={"p": "urn:probe"}) == 2.0

# XSLT transform
xslt_doc = etree.fromstring(
    b"""<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
  <xsl:template match="/root">
    <out><xsl:for-each select="child"><li><xsl:value-of select="@name"/></li></xsl:for-each></out>
  </xsl:template>
</xsl:stylesheet>"""
)
transform = etree.XSLT(xslt_doc)
result = transform(doc)
assert etree.tostring(result) == b"<out><li>a</li><li>b</li></out>", etree.tostring(result)

# iterparse over a bytes stream
stream = io.BytesIO(b"<log><e i='1'/><e i='2'/><e i='3'/></log>")
seen = [el.get("i") for _, el in etree.iterparse(stream, tag="e")]
assert seen == ["1", "2", "3"], seen

# lxml.html: fragment parse + link rewrite
frag = html.fromstring(
    "<div><a href='http://old.example/x'>one</a><p><a href='/rel'>two</a></p></div>"
)
frag.rewrite_links(lambda url: url.replace("old.example", "new.example"))
hrefs = [a.get("href") for a in frag.iter("a")]
assert hrefs == ["http://new.example/x", "/rel"], hrefs
assert frag.text_content() == "onetwo"

# XMLSchema validation
schema = etree.XMLSchema(
    etree.fromstring(
        b"""<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="point">
    <xs:complexType>
      <xs:sequence><xs:element name="x" type="xs:integer"/></xs:sequence>
    </xs:complexType>
  </xs:element>
</xs:schema>"""
    )
)
assert schema.validate(etree.fromstring(b"<point><x>3</x></point>"))
assert not schema.validate(etree.fromstring(b"<point><x>nope</x></point>"))

# interop: deepcopy an element tree; pickle its serialized form
clone = copy.deepcopy(doc)
clone[0].set("name", "mutated")
assert doc[0].get("name") == "a", "deepcopy must not share state"
assert etree.tostring(clone) != etree.tostring(doc)
pickled = pickle.dumps(etree.tostring(doc))
assert etree.tostring(etree.fromstring(pickle.loads(pickled))) == etree.tostring(doc)

print("lxml ok", etree.__version__)
