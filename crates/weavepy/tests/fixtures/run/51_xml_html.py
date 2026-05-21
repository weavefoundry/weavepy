import html
from xml.etree import ElementTree as ET

print("--- html ---")
print("escape:", html.escape("<b>'one' & \"two\"</b>"))
print("unescape:", html.unescape("&amp;lt;&gt;&quot;&#65;&#x42;"))

print("--- xml.etree ---")
xml = """<root>
  <person id="1">
    <name>Alice</name>
    <age>30</age>
  </person>
  <person id="2">
    <name>Bob</name>
    <age>25</age>
  </person>
</root>"""

root = ET.fromstring(xml)
print("root tag:", root.tag)
print("children count:", len(root))
for p in root.findall("person"):
    print("person", p.get("id"), p.findtext("name"), p.findtext("age"))

elem = ET.Element("greeting")
elem.text = "hello & welcome"
ET.SubElement(elem, "to").text = "world"
print("serialised:", ET.tostring(elem, encoding="unicode"))
