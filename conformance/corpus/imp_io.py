import io

buf = io.StringIO()
buf.write("hello ")
buf.write("world")
print(buf.getvalue())

byts = io.BytesIO(b"abcdef")
print(byts.read(3))
print(byts.read())
