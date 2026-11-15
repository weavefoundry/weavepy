"""`os.DirEntry` answers from the directory read where it can: its methods
live on the type, and every kind of entry reports the same as `os.stat`
would, by path, by descriptor, and for `bytes` paths."""

import os
import shutil
import tempfile

d = tempfile.mkdtemp()
os.mkdir(os.path.join(d, "sub"))
with open(os.path.join(d, "file.txt"), "w") as f:
    f.write("x")
os.symlink(os.path.join(d, "sub"), os.path.join(d, "to_dir"))
os.symlink(os.path.join(d, "file.txt"), os.path.join(d, "to_file"))
os.symlink(os.path.join(d, "missing"), os.path.join(d, "dangling"))

want = {
    # name: (is_dir, is_file, is_symlink, is_dir(nofollow), is_file(nofollow))
    "sub": (True, False, False, True, False),
    "file.txt": (False, True, False, False, True),
    "to_dir": (True, False, True, False, False),
    "to_file": (False, True, True, False, False),
    "dangling": (False, False, True, False, False),
}


def kinds(e):
    return (
        e.is_dir(),
        e.is_file(),
        e.is_symlink(),
        e.is_dir(follow_symlinks=False),
        e.is_file(follow_symlinks=False),
    )


with os.scandir(d) as it:
    entries = {e.name: e for e in it}
assert set(entries) == set(want)
for name, e in entries.items():
    assert kinds(e) == want[name], (name, kinds(e))
    assert e.path == os.path.join(d, name) and os.fspath(e) == e.path
    assert e.inode() == os.lstat(e.path).st_ino
    assert e.stat(follow_symlinks=False).st_ino == e.inode()
    assert repr(e) == "<DirEntry %r>" % name
    assert not e.is_junction()
    assert type(e) is os.DirEntry
    if name != "dangling":
        assert e.stat().st_ino == os.stat(e.path).st_ino
    # The methods are the type's, bound per access.
    assert e.is_dir.__self__ is e and os.DirEntry.is_dir(e) == want[name][0]
try:
    entries["dangling"].stat()
except FileNotFoundError:
    pass
else:
    raise AssertionError("stat() of a dangling link follows it")

fd = os.open(d, os.O_RDONLY)
try:
    by_fd = {e.name: e for e in os.scandir(fd)}
    assert set(by_fd) == set(want)
    for name, e in by_fd.items():
        assert kinds(e) == want[name], (name, kinds(e))
        assert e.path == name
        assert e.inode() == entries[name].inode()
        assert e.stat(follow_symlinks=False).st_ino == e.inode()
finally:
    os.close(fd)

for e in os.scandir(os.fsencode(d)):
    name = os.fsdecode(e.name)
    assert isinstance(e.name, bytes) and isinstance(e.path, bytes)
    assert isinstance(os.fspath(e), bytes)
    assert kinds(e) == want[name], (name, kinds(e))
    assert e.stat(follow_symlinks=False).st_ino == e.inode()

# An entry outlives what it names: what the read said, and a `stat()` it
# has already made, stay; one it hasn't made says the file is gone.
gone = entries["file.txt"]
first = gone.stat()
os.unlink(gone.path)
assert gone.inode() > 0 and gone.name == "file.txt"
assert gone.is_symlink() is False and gone.is_file() is True
assert gone.stat() is first and gone.stat(follow_symlinks=False).st_ino == first.st_ino
with os.scandir(d) as it:
    fresh = {e.name: e for e in it}
os.unlink(fresh["to_file"].path)
try:
    fresh["to_file"].stat(follow_symlinks=False)
except FileNotFoundError:
    pass
else:
    raise AssertionError("stat() of a removed link")
os.symlink(os.path.join(d, "file.txt"), os.path.join(d, "to_file"))

walked = sorted((root[len(d) :], sorted(dirs), sorted(files)) for root, dirs, files in os.walk(d))
assert walked == [("", ["sub", "to_dir"], ["dangling", "to_file"]), ("/sub", [], [])], walked

try:
    os.DirEntry()
except TypeError:
    pass
else:
    raise AssertionError("DirEntry isn't constructible")
shutil.rmtree(d)
print("ok")
