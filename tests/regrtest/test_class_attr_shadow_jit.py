"""A compiled attribute read of a class attribute that some instances
shadow returns each instance's own value, and the class's for the
others, in whatever order the instances come (`tarfile.TarFile.encoding`
read by `TarInfo._fromtarfile`: a TarFile opened with an explicit
encoding left its value cached for the default ones that followed)."""


class Archive:
    encoding = "utf-8"

    def __init__(self, encoding=None, errors="surrogateescape"):
        self.fileobj = 1
        if encoding is not None:
            self.encoding = encoding
        self.errors = errors


class Info:
    @classmethod
    def frombuf(cls, buf, encoding, errors, *, dircheck=True):
        return encoding

    @classmethod
    def fromarchive(cls, archive, *, dircheck=True):
        buf = archive.fileobj
        return cls.frombuf(buf, archive.encoding, archive.errors, dircheck=dircheck)


# (At module level: a loop in a function compiles along with the read,
# which changes when the read's site warms.)
for k in range(20000):
    phase = (k // 500) % 3
    a = Archive("iso8859-1") if phase == 1 else Archive()
    want = "iso8859-1" if phase == 1 else "utf-8"
    for _ in range(3):
        got = Info.fromarchive(a)
        assert got == want, (k, phase, got, want)

# The same through a plain function, the shadowing instance first.
def read(a):
    return a.encoding


for k in range(6000):
    a = Archive("latin-1") if k % 2 else Archive()
    assert read(a) == ("latin-1" if k % 2 else "utf-8"), k

# A class attribute rebound on the class is read anew.
for k in range(3000):
    if k == 1500:
        Archive.encoding = "ascii"
    assert read(Archive()) == ("ascii" if k >= 1500 else "utf-8"), k

print("ok")
