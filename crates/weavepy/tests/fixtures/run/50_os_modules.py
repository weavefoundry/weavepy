import os
import tempfile
import shutil
import glob
import fnmatch
import zlib

print("--- tempfile ---")
_fd, path = tempfile.mkstemp(suffix=".weavepy")
print("ends with suffix:", path.endswith(".weavepy"))
os.remove(path)

dir_path = tempfile.mkdtemp(prefix="wp_")
print("dir exists:", os.path.isdir(dir_path))
print("dir starts with prefix:", "wp_" in dir_path)

# Drop a few files into it for glob/fnmatch.
for name in ["a.txt", "b.txt", "c.md"]:
    with open(os.path.join(dir_path, name), "w") as f:
        f.write("hello")

print("--- glob ---")
hits = sorted(glob.glob(os.path.join(dir_path, "*.txt")))
print("txt files:", [os.path.basename(h) for h in hits])

print("--- fnmatch ---")
print("a.txt vs *.txt:", fnmatch.fnmatch("a.txt", "*.txt"))
print("a.md vs *.txt:", fnmatch.fnmatch("a.md", "*.txt"))
print("translate:", fnmatch.translate("*.py")[:20], "...")

print("--- shutil ---")
copy_path = os.path.join(dir_path, "copy.txt")
shutil.copyfile(os.path.join(dir_path, "a.txt"), copy_path)
with open(copy_path) as f:
    print("copied content:", f.read())

shutil.rmtree(dir_path)
print("dir gone:", not os.path.exists(dir_path))

print("--- zlib ---")
data = b"hello world " * 10
compressed = zlib.compress(data)
print("compressed shorter:", len(compressed) < len(data))
print("roundtrip:", zlib.decompress(compressed) == data)
print("crc32:", zlib.crc32(b"hello"))
