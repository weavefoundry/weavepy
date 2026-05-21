import csv
import mimetypes
import io

print("--- csv reader ---")
text = "name,age\nAlice,30\nBob,25\n"
buf = io.StringIO(text)
for row in csv.reader(buf):
    print(row)

print("--- csv DictReader ---")
buf = io.StringIO(text)
for row in csv.DictReader(buf):
    print(row)

print("--- csv writer ---")
out = io.StringIO()
w = csv.writer(out)
w.writerow(["x", "y"])
w.writerow([1, 2])
w.writerow([3, 4])
print(repr(out.getvalue()))

print("--- csv DictWriter ---")
out = io.StringIO()
dw = csv.DictWriter(out, fieldnames=["k", "v"])
dw.writeheader()
dw.writerow({"k": "a", "v": 1})
dw.writerow({"k": "b", "v": 2})
print(repr(out.getvalue()))

print("--- mimetypes ---")
print("json:", mimetypes.guess_type("data.json"))
print("png:", mimetypes.guess_type("img.png"))
print("tar.gz:", mimetypes.guess_type("file.tar.gz"))
print("unknown:", mimetypes.guess_type("file.weird"))
