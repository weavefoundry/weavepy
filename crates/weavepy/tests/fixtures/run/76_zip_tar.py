import os, io, tempfile
import zipfile, tarfile

_tmp = tempfile.gettempdir()

zpath = os.path.join(_tmp, "_weavepy_fixture.zip")
if os.path.exists(zpath):
    os.remove(zpath)

with zipfile.ZipFile(zpath, "w", zipfile.ZIP_DEFLATED) as z:
    z.writestr("hello.txt", "Hello, ZIP!")
    z.writestr("nested/data.txt", b"a" * 100)

with zipfile.ZipFile(zpath, "r") as z:
    print("zip names:", sorted(z.namelist()))
    print("hello:", z.read("hello.txt"))
    print("nested len:", len(z.read("nested/data.txt")))

print("is_zipfile:", zipfile.is_zipfile(zpath))
os.remove(zpath)

tpath = os.path.join(_tmp, "_weavepy_fixture.tar")
if os.path.exists(tpath):
    os.remove(tpath)

with tarfile.open(tpath, "w") as t:
    info = tarfile.TarInfo("hello.txt")
    payload = b"Hello, TAR!"
    info.size = len(payload)
    t.addfile(info, io.BytesIO(payload))

with tarfile.open(tpath, "r") as t:
    print("tar names:", t.getnames())
    f = t.extractfile("hello.txt")
    print("hello:", f.read())

print("is_tarfile:", tarfile.is_tarfile(tpath))
os.remove(tpath)
