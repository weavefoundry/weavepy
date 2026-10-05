"""xml.etree.ElementTree: build, serialize, parse, and query a document."""

import xml.etree.ElementTree as ET

WORK = 40


def build():
    root = ET.Element("catalog", version="2")
    for i in range(150):
        book = ET.SubElement(root, "book", id="b%d" % i, lang="en" if i % 2 else "fr")
        ET.SubElement(book, "title").text = "Title & number %d" % i
        ET.SubElement(book, "price").text = "%.2f" % (i * 1.1)
        tags = ET.SubElement(book, "tags")
        for t in range(i % 4):
            ET.SubElement(tags, "tag").text = "t%d" % t
    return root


def bench(n):
    total = 0
    for _ in range(n):
        root = build()
        data = ET.tostring(root, encoding="unicode")
        total += len(data)
        back = ET.fromstring(data)
        total += sum(1 for _ in back.iter("tag"))
        total += len(back.findall("book[@lang='fr']"))
        total += int(sum(float(p.text) for p in back.iter("price")))
    return total
