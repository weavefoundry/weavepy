"""Public ``tarfile`` module (RFC 0019).

A pragmatic implementation of the POSIX ustar tar format, with
gzip/bzip2/lzma compression layered on top via the matching modules.
This covers what most real-world Python code uses (reading and
writing ``.tar``, ``.tar.gz``, ``.tar.bz2``, ``.tar.xz``) without
the complexity of CPython's GNU/PAX-extension support — those are
flagged in the RFC as a future increment.
"""

import bz2
import gzip
import io
import lzma
import os
import struct
import time

NUL = b"\x00"
BLOCKSIZE = 512

REGTYPE = b"0"
AREGTYPE = b"\0"
LNKTYPE = b"1"
SYMTYPE = b"2"
CHRTYPE = b"3"
BLKTYPE = b"4"
DIRTYPE = b"5"
FIFOTYPE = b"6"
CONTTYPE = b"7"

USTAR_MAGIC = b"ustar"
GNU_MAGIC = b"ustar  "

USTAR_FORMAT = 0
GNU_FORMAT = 1
PAX_FORMAT = 2
DEFAULT_FORMAT = USTAR_FORMAT

ENCODING = "utf-8"

_builtin_open = open


class TarError(Exception):
    """Base of tarfile errors."""


class ReadError(TarError):
    """Raised when an archive cannot be parsed."""


class CompressionError(TarError):
    """Raised when a stream cannot be (de)compressed."""


class StreamError(TarError):
    """Raised when seeking is required on a non-seekable stream."""


class ExtractError(TarError):
    """Raised by ``extract*`` methods when a member cannot be extracted."""


def _pad(name, length):
    if isinstance(name, str):
        name = name.encode(ENCODING)
    name = name[:length]
    return name + NUL * (length - len(name))


def _octal(value, length):
    """Produce a NUL-terminated octal string of width `length`."""
    s = ("%o" % int(value)).encode("ascii")
    s = s[-(length - 1):]
    s = b"0" * (length - 1 - len(s)) + s
    return s + NUL


def _parse_octal(buf):
    s = buf.split(NUL, 1)[0].strip().decode("ascii", "strict")
    if not s:
        return 0
    return int(s, 8)


def _checksum(header):
    return sum(header[:148]) + 32 * 8 + sum(header[156:512])


class TarInfo:
    """Describes a single member of a tar archive."""

    def __init__(self, name=""):
        self.name = name
        self.size = 0
        self.mode = 0o644
        self.uid = 0
        self.gid = 0
        self.mtime = int(time.time())
        self.type = REGTYPE
        self.linkname = ""
        self.uname = ""
        self.gname = ""
        self.devmajor = 0
        self.devminor = 0
        self.offset = 0
        self.offset_data = 0
        self.pax_headers = {}

    def isfile(self):
        return self.type == REGTYPE or self.type == AREGTYPE

    def isreg(self):
        return self.isfile()

    def isdir(self):
        return self.type == DIRTYPE

    def issym(self):
        return self.type == SYMTYPE

    def islnk(self):
        return self.type == LNKTYPE

    @classmethod
    def from_buf(cls, buf):
        if len(buf) != BLOCKSIZE:
            raise ReadError("truncated header block")
        if buf == NUL * BLOCKSIZE:
            return None
        info = cls()
        info.name = buf[0:100].split(NUL, 1)[0].decode(ENCODING, "surrogateescape")
        info.mode = _parse_octal(buf[100:108])
        info.uid = _parse_octal(buf[108:116])
        info.gid = _parse_octal(buf[116:124])
        info.size = _parse_octal(buf[124:136])
        info.mtime = _parse_octal(buf[136:148])
        chksum = _parse_octal(buf[148:156])
        info.type = buf[156:157] or REGTYPE
        info.linkname = buf[157:257].split(NUL, 1)[0].decode(ENCODING, "surrogateescape")
        magic = buf[257:263]
        info.uname = buf[265:297].split(NUL, 1)[0].decode(ENCODING, "surrogateescape")
        info.gname = buf[297:329].split(NUL, 1)[0].decode(ENCODING, "surrogateescape")
        info.devmajor = _parse_octal(buf[329:337])
        info.devminor = _parse_octal(buf[337:345])
        prefix = buf[345:500].split(NUL, 1)[0].decode(ENCODING, "surrogateescape")
        if prefix:
            info.name = prefix + "/" + info.name
        if _checksum(buf) != chksum:
            raise ReadError("invalid checksum on tar header")
        return info

    def tobuf(self):
        # Split long names across `prefix` and `name` fields when possible.
        name = self.name
        prefix = ""
        if isinstance(name, bytes):
            name = name.decode(ENCODING, "surrogateescape")
        if len(name.encode(ENCODING)) > 100:
            # POSIX ustar allows up to 155 bytes of prefix + 100 bytes of name.
            n = name
            slash = n.rfind("/", 0, 156)
            if slash > 0:
                prefix, name = n[:slash], n[slash + 1:]
            else:
                # Truncate and surface a warning by raising — this is
                # the GNU long-name case which we don't yet emit.
                raise ValueError("name too long for ustar header: %r" % name)
        type_byte = self.type if isinstance(self.type, bytes) else self.type.encode("ascii")
        if not type_byte:
            type_byte = REGTYPE
        elif len(type_byte) > 1:
            type_byte = type_byte[:1]
        header = b"".join([
            _pad(name, 100),
            _octal(self.mode, 8),
            _octal(self.uid, 8),
            _octal(self.gid, 8),
            _octal(self.size, 12),
            _octal(self.mtime, 12),
            b" " * 8,                          # checksum placeholder
            type_byte,
            _pad(self.linkname or "", 100),
            b"ustar\x00",
            b"00",
            _pad(self.uname or "", 32),
            _pad(self.gname or "", 32),
            _octal(self.devmajor, 8),
            _octal(self.devminor, 8),
            _pad(prefix, 155),
            b"\x00" * 12,
        ])
        chksum = _checksum(header)
        header = header[:148] + _octal(chksum, 8) + header[156:]
        return header

    @classmethod
    def from_file(cls, name, arcname=None):
        st = os.stat(name)
        info = cls(arcname or name)
        info.size = st.st_size
        info.mtime = int(st.st_mtime)
        info.mode = 0o644
        return info


class _MemberFile:
    """Read-only view onto the bytes for a single tar member."""

    def __init__(self, data, name):
        self._buf = io.BytesIO(data)
        self.name = name

    def read(self, n=-1):
        return self._buf.read(n if n is not None else -1)

    def close(self):
        self._buf.close()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False


class TarFile:
    """Top-level tar archive."""

    def __init__(self, name=None, mode="r", fileobj=None, format=DEFAULT_FORMAT,
                 dereference=False, encoding=ENCODING, errors="surrogateescape"):
        self.name = name
        self.mode = mode
        self.format = format
        self.encoding = encoding
        self.errors = errors
        self._members = []
        self._closed = False
        self._owns_fp = False
        if fileobj is None:
            self.fileobj = _builtin_open(name, mode + "b" if "b" not in mode else mode)
            self._owns_fp = True
        else:
            self.fileobj = fileobj
        if mode.startswith("r"):
            self._read_archive()

    # ---- context manager ----

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False

    # ---- factory: gzip/bz2/xz wrappers ----

    @classmethod
    def open(cls, name=None, mode="r", fileobj=None, **kwargs):
        if mode in ("r", "r:*", "r:"):
            # Attempt magic-based detection.
            return _open_for_read(name, fileobj, **kwargs)
        if mode == "r:gz":
            tar = cls(name, "r", fileobj=_GzipBinaryReader(name, fileobj), **kwargs)
            tar._owns_fp = True
            return tar
        if mode == "r:bz2":
            tar = cls(name, "r", fileobj=_Bz2BinaryReader(name, fileobj), **kwargs)
            tar._owns_fp = True
            return tar
        if mode in ("r:xz", "r:lzma"):
            tar = cls(name, "r", fileobj=_LzmaBinaryReader(name, fileobj), **kwargs)
            tar._owns_fp = True
            return tar
        if mode == "w":
            return cls(name, "w", fileobj=fileobj, **kwargs)
        if mode == "w:gz":
            tar = cls(name, "w", fileobj=_GzipBinaryWriter(name, fileobj), **kwargs)
            tar._owns_fp = True
            return tar
        if mode == "w:bz2":
            tar = cls(name, "w", fileobj=_Bz2BinaryWriter(name, fileobj), **kwargs)
            tar._owns_fp = True
            return tar
        if mode in ("w:xz", "w:lzma"):
            tar = cls(name, "w", fileobj=_LzmaBinaryWriter(name, fileobj), **kwargs)
            tar._owns_fp = True
            return tar
        if mode == "a":
            return cls(name, "a", fileobj=fileobj, **kwargs)
        raise ValueError("unknown mode %r" % mode)

    # ---- reading ----

    def getmembers(self):
        return list(self._members)

    def getnames(self):
        return [m.name for m in self._members]

    def getmember(self, name):
        for m in self._members:
            if m.name == name:
                return m
        raise KeyError("filename %r not found" % name)

    def extractfile(self, member):
        if isinstance(member, str):
            member = self.getmember(member)
        if not member.isfile():
            return None
        self.fileobj.seek(member.offset_data)
        data = self.fileobj.read(member.size)
        return _MemberFile(data, member.name)

    def extract(self, member, path="", set_attrs=True):
        if isinstance(member, str):
            member = self.getmember(member)
        target = os.path.join(path or os.getcwd(), member.name)
        if member.isdir():
            try:
                os.makedirs(target, exist_ok=True)
            except TypeError:
                if not os.path.isdir(target):
                    os.mkdir(target)
            return
        parent = os.path.dirname(target)
        if parent:
            try:
                os.makedirs(parent, exist_ok=True)
            except TypeError:
                if parent and not os.path.isdir(parent):
                    os.makedirs(parent)
        with self.extractfile(member) as src:
            data = src.read()
        with _builtin_open(target, "wb") as dst:
            dst.write(data)

    def extractall(self, path="", members=None, *, numeric_owner=False):
        for m in (members or self._members):
            self.extract(m, path)

    # ---- writing ----

    def add(self, name, arcname=None, recursive=True):
        info = TarInfo.from_file(name, arcname)
        if os.path.isdir(name):
            info.type = DIRTYPE
            info.size = 0
            self.addfile(info)
            if recursive:
                for entry in sorted(os.listdir(name)):
                    self.add(os.path.join(name, entry),
                             arcname=os.path.join(info.name, entry),
                             recursive=True)
            return
        with _builtin_open(name, "rb") as src:
            data = src.read()
        info.size = len(data)
        self.addfile(info, io.BytesIO(data))

    def addfile(self, tarinfo, fileobj=None):
        if not self.mode.startswith("w") and not self.mode.startswith("a"):
            raise StreamError("cannot add member: archive opened for reading")
        header = tarinfo.tobuf()
        self.fileobj.write(header)
        if fileobj is not None and tarinfo.size > 0:
            data = fileobj.read()
            self.fileobj.write(data)
            pad = (-tarinfo.size) % BLOCKSIZE
            if pad:
                self.fileobj.write(NUL * pad)
        self._members.append(tarinfo)

    def close(self):
        if self._closed:
            return
        if self.mode.startswith("w") or self.mode.startswith("a"):
            self.fileobj.write(NUL * BLOCKSIZE * 2)  # two-block tail
            if hasattr(self.fileobj, "flush"):
                self.fileobj.flush()
        if self._owns_fp:
            self.fileobj.close()
        self._closed = True

    # ---- internals ----

    def _read_archive(self):
        offset = 0
        while True:
            self.fileobj.seek(offset)
            block = self.fileobj.read(BLOCKSIZE)
            if not block or len(block) < BLOCKSIZE:
                break
            try:
                info = TarInfo.from_buf(block)
            except ReadError as e:
                raise ReadError("%s @offset %d" % (e, offset)) from None
            if info is None:
                # End-of-archive (NUL block).
                break
            info.offset = offset
            info.offset_data = offset + BLOCKSIZE
            data_size = info.size if info.isfile() else 0
            blocks = (data_size + BLOCKSIZE - 1) // BLOCKSIZE
            offset = info.offset_data + blocks * BLOCKSIZE
            self._members.append(info)


def _open_for_read(name, fileobj, **kwargs):
    """Detect compression and dispatch to the right reader."""
    if fileobj is None:
        fp = _builtin_open(name, "rb")
    else:
        fp = fileobj
    head = fp.read(6)
    if hasattr(fp, "seek"):
        fp.seek(0)
    if head.startswith(b"\x1f\x8b"):
        tar = TarFile(name, "r", fileobj=_GzipBinaryReader(None, fp))
        tar._owns_fp = True
        return tar
    if head.startswith(b"BZh"):
        tar = TarFile(name, "r", fileobj=_Bz2BinaryReader(None, fp))
        tar._owns_fp = True
        return tar
    if head.startswith(b"\xfd7zXZ\x00"):
        tar = TarFile(name, "r", fileobj=_LzmaBinaryReader(None, fp))
        tar._owns_fp = True
        return tar
    tar = TarFile(name, "r", fileobj=fp)
    if fileobj is None:
        tar._owns_fp = True
    return tar


# --- Compression reader/writer wrappers ----------------------------------

class _GzipBinaryReader:
    def __init__(self, name, fileobj):
        if fileobj is None:
            fileobj = _builtin_open(name, "rb")
        data = fileobj.read()
        try:
            self._buf = io.BytesIO(gzip.decompress(data))
        except Exception as e:
            raise CompressionError(str(e)) from None

    def read(self, n=-1):
        return self._buf.read(n if n is not None else -1)

    def seek(self, off, whence=0):
        return self._buf.seek(off, whence)

    def tell(self):
        return self._buf.tell()

    def close(self):
        self._buf.close()


class _Bz2BinaryReader(_GzipBinaryReader):
    def __init__(self, name, fileobj):
        if fileobj is None:
            fileobj = _builtin_open(name, "rb")
        data = fileobj.read()
        try:
            self._buf = io.BytesIO(bz2.decompress(data))
        except Exception as e:
            raise CompressionError(str(e)) from None


class _LzmaBinaryReader(_GzipBinaryReader):
    def __init__(self, name, fileobj):
        if fileobj is None:
            fileobj = _builtin_open(name, "rb")
        data = fileobj.read()
        try:
            self._buf = io.BytesIO(lzma.decompress(data))
        except Exception as e:
            raise CompressionError(str(e)) from None


class _GzipBinaryWriter:
    """Buffer everything in memory, compress on close."""

    def __init__(self, name, fileobj):
        if fileobj is None:
            fileobj = _builtin_open(name, "wb")
        self._dst = fileobj
        self._buf = io.BytesIO()

    def write(self, data):
        return self._buf.write(data)

    def flush(self):
        pass

    def close(self):
        self._dst.write(gzip.compress(self._buf.getvalue()))
        self._dst.close()


class _Bz2BinaryWriter(_GzipBinaryWriter):
    def close(self):
        self._dst.write(bz2.compress(self._buf.getvalue()))
        self._dst.close()


class _LzmaBinaryWriter(_GzipBinaryWriter):
    def close(self):
        self._dst.write(lzma.compress(self._buf.getvalue()))
        self._dst.close()


def open(name=None, mode="r", fileobj=None, **kwargs):
    return TarFile.open(name, mode, fileobj, **kwargs)


def is_tarfile(name):
    try:
        with _builtin_open(name, "rb") as f:
            head = f.read(265)
    except OSError:
        return False
    if head.startswith(b"\x1f\x8b") or head.startswith(b"BZh") \
            or head.startswith(b"\xfd7zXZ\x00"):
        return True
    if len(head) < 265:
        return False
    return head[257:262] == b"ustar"


__all__ = ["TarFile", "TarInfo", "open", "is_tarfile",
           "TarError", "ReadError", "CompressionError",
           "StreamError", "ExtractError",
           "REGTYPE", "AREGTYPE", "LNKTYPE", "SYMTYPE", "CHRTYPE",
           "BLKTYPE", "DIRTYPE", "FIFOTYPE", "CONTTYPE",
           "USTAR_FORMAT", "GNU_FORMAT", "PAX_FORMAT", "DEFAULT_FORMAT",
           "ENCODING", "BLOCKSIZE"]
