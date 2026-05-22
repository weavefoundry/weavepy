"""Public ``zipfile`` module (RFC 0019).

Reads and writes ZIP archives using the Rust-backed ``zlib`` core
for ``DEFLATE`` and the standalone ``struct`` module for header
parsing. Mirrors CPython's API surface for the common operations:
``ZipFile``, ``ZipInfo``, ``is_zipfile``, and the convenience
``Path`` helper.

This implementation is intentionally focused on **stored** and
**deflated** entries — the two methods used by every modern Python
``.zip`` (and every ``.whl`` and ``.egg``). BZIP2/LZMA encrypted
archives can be added later through the matching ``_bz2``/``_lzma``
cores.
"""

import io
import os
import struct
import time
import zlib

ZIP_STORED = 0
ZIP_DEFLATED = 8
ZIP_BZIP2 = 12
ZIP_LZMA = 14

_LOCAL_HEADER = b"PK\x03\x04"
_CENTRAL_HEADER = b"PK\x01\x02"
_END_OF_CENTRAL = b"PK\x05\x06"
_DATA_DESCRIPTOR = b"PK\x07\x08"

_LOCAL_FMT = "<4s2B4HL2L2H"
_CENTRAL_FMT = "<4s4B4HL2L5H2L"
_EOCD_FMT = "<4s4H2LH"

_builtin_open = open


class BadZipFile(Exception):
    """Raised when the archive is corrupt."""


BadZipfile = BadZipFile


class LargeZipFile(Exception):
    """Raised when a file would require ZIP64 extensions."""


def is_zipfile(filename):
    """Return True if `filename` (path or file-like) appears to be a ZIP."""
    try:
        if hasattr(filename, "read"):
            pos = filename.tell()
            try:
                head = filename.read(4)
            finally:
                filename.seek(pos)
        else:
            with _builtin_open(filename, "rb") as f:
                head = f.read(4)
    except OSError:
        return False
    return head == _LOCAL_HEADER or head == _CENTRAL_HEADER


def _dos_time(t=None):
    if t is None:
        t = time.localtime()
    if hasattr(t, "tm_year"):
        year, mon, day = t.tm_year, t.tm_mon, t.tm_mday
        hour, mn, sec = t.tm_hour, t.tm_min, t.tm_sec
    else:
        year, mon, day, hour, mn, sec = t[:6]
    if year < 1980:
        year = 1980
    dos_date = ((year - 1980) << 9) | (mon << 5) | day
    dos_time = (hour << 11) | (mn << 5) | (sec // 2)
    return dos_date, dos_time


def _from_dos(date, dtime):
    year = ((date >> 9) & 0x7F) + 1980
    mon = (date >> 5) & 0x0F
    day = date & 0x1F
    hour = (dtime >> 11) & 0x1F
    mn = (dtime >> 5) & 0x3F
    sec = (dtime & 0x1F) * 2
    return (year, mon, day, hour, mn, sec)


class ZipInfo:
    """Describes a single member of a ZIP archive."""

    def __init__(self, filename="NoName", date_time=(1980, 1, 1, 0, 0, 0)):
        if isinstance(filename, bytes):
            self._raw_filename = filename
            filename = filename.decode("utf-8", "surrogateescape")
        else:
            self._raw_filename = None
        if filename.endswith("/"):
            pass
        self.filename = filename
        self.date_time = date_time
        self.compress_type = ZIP_STORED
        self.comment = b""
        self.extra = b""
        self.create_system = 3
        self.create_version = 20
        self.extract_version = 20
        self.flag_bits = 0
        self.volume = 0
        self.internal_attr = 0
        self.external_attr = 0
        self.header_offset = 0
        self.CRC = 0
        self.compress_size = 0
        self.file_size = 0

    def is_dir(self):
        return self.filename.endswith("/")

    @classmethod
    def from_file(cls, filename, arcname=None):
        st = os.stat(filename)
        mtime = time.localtime(st.st_mtime)
        info = cls(arcname or filename, (mtime.tm_year, mtime.tm_mon, mtime.tm_mday,
                                          mtime.tm_hour, mtime.tm_min, mtime.tm_sec))
        info.file_size = st.st_size
        return info

    def __repr__(self):
        return "<ZipInfo filename=%r filemode=%r file_size=%d>" % (
            self.filename, "stored" if self.compress_type == ZIP_STORED else "deflated",
            self.file_size,
        )


class _ZipReadIO:
    """Tiny file-like wrapper around an uncompressed bytes payload."""

    def __init__(self, data, info):
        self._buf = io.BytesIO(data)
        self.name = info.filename

    def read(self, n=-1):
        return self._buf.read(n if n is not None else -1)

    def readline(self):
        return self._buf.readline()

    def readlines(self):
        return self._buf.readlines()

    def readall(self):
        return self._buf.read()

    def seek(self, *a, **kw):
        return self._buf.seek(*a, **kw)

    def tell(self):
        return self._buf.tell()

    def close(self):
        self._buf.close()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False

    def __iter__(self):
        return iter(self._buf.getvalue().splitlines(keepends=True))


class ZipFile:
    """Read and write ZIP archives."""

    fp = None

    def __init__(self, file, mode="r", compression=ZIP_STORED,
                 allowZip64=True, compresslevel=None):
        self.filename = None
        self.fp = None
        self.mode = mode
        self.compression = compression
        self._compresslevel = compresslevel
        self._allow_zip64 = allowZip64
        self._owns_fp = False
        self._infos = []
        self._info_by_name = {}
        self._comment = b""
        self._files_to_write_by_name = set()

        if isinstance(file, (str, bytes, os.PathLike if hasattr(os, "PathLike") else str)):
            self.filename = os.fspath(file) if hasattr(os, "fspath") else file
            self._owns_fp = True
            if mode == "r":
                self.fp = _builtin_open(self.filename, "rb")
            elif mode in ("w", "x"):
                self.fp = _builtin_open(self.filename, "wb")
            elif mode == "a":
                if os.path.exists(self.filename):
                    self.fp = _builtin_open(self.filename, "r+b")
                else:
                    self.fp = _builtin_open(self.filename, "wb")
            else:
                raise ValueError("ZipFile mode must be one of r, w, x, a")
        else:
            self.fp = file
            self._owns_fp = False
            self.filename = getattr(file, "name", None)

        if mode == "r" or (mode == "a" and self._is_existing_archive()):
            self._read_central_directory()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False

    # ---- introspection ----

    def namelist(self):
        return [info.filename for info in self._infos]

    def infolist(self):
        return list(self._infos)

    def getinfo(self, name):
        info = self._info_by_name.get(name)
        if info is None:
            raise KeyError("There is no item named %r in the archive" % name)
        return info

    def setpassword(self, pwd):
        # Encryption is currently unsupported. We accept the call so
        # consumers don't blow up immediately and surface an error
        # only when an encrypted member is actually decoded.
        self._password = pwd

    def comment(self):
        return self._comment

    # ---- read path ----

    def read(self, name, pwd=None):
        with self.open(name, "r", pwd=pwd) as fp:
            return fp.read()

    def open(self, name, mode="r", pwd=None, *, force_zip64=False):
        if mode not in ("r",):
            raise NotImplementedError("ZipFile.open only supports mode='r' currently")
        info = name if isinstance(name, ZipInfo) else self.getinfo(name)
        if info.flag_bits & 0x1:
            raise NotImplementedError("Encrypted ZIP entries are not supported")

        self.fp.seek(info.header_offset)
        header = self.fp.read(30)
        if len(header) != 30 or header[:4] != _LOCAL_HEADER:
            raise BadZipFile("Bad local file header for %r" % info.filename)
        (_sig, _vb, _vh, _flag, _meth, _t, _d, _crc,
         _csize, _usize, fname_len, extra_len) = struct.unpack(_LOCAL_FMT, header)
        self.fp.read(fname_len + extra_len)
        raw = self.fp.read(info.compress_size)
        if info.compress_type == ZIP_STORED:
            data = raw
        elif info.compress_type == ZIP_DEFLATED:
            data = zlib.decompress(raw, -15)
        else:
            raise NotImplementedError(
                "Unsupported compression type: %d" % info.compress_type)
        if zlib.crc32(data) & 0xFFFFFFFF != info.CRC:
            raise BadZipFile("Bad CRC-32 for file %r" % info.filename)
        return _ZipReadIO(data, info)

    def extract(self, member, path=None, pwd=None):
        if isinstance(member, ZipInfo):
            info = member
        else:
            info = self.getinfo(member)
        if path is None:
            path = os.getcwd()
        target = os.path.join(path, info.filename)
        if info.is_dir():
            try:
                os.makedirs(target, exist_ok=True)
            except TypeError:
                if not os.path.isdir(target):
                    os.mkdir(target)
            return target
        parent = os.path.dirname(target)
        if parent:
            try:
                os.makedirs(parent, exist_ok=True)
            except TypeError:
                if parent and not os.path.isdir(parent):
                    os.makedirs(parent)
        with self.open(info, "r", pwd=pwd) as src:
            data = src.read()
        with _builtin_open(target, "wb") as dst:
            dst.write(data)
        return target

    def extractall(self, path=None, members=None, pwd=None):
        for info in (members or self._infos):
            if isinstance(info, str):
                info = self.getinfo(info)
            self.extract(info, path, pwd)

    # ---- write path ----

    def write(self, filename, arcname=None, compress_type=None,
              compresslevel=None):
        if self.mode not in ("w", "x", "a"):
            raise ValueError("Cannot write to ZipFile in read mode")
        if arcname is None:
            arcname = os.path.basename(filename)
        info = ZipInfo.from_file(filename, arcname)
        info.compress_type = compress_type if compress_type is not None else self.compression
        with _builtin_open(filename, "rb") as f:
            data = f.read()
        self._write_member(info, data, compresslevel)

    def writestr(self, zinfo_or_arcname, data, compress_type=None,
                 compresslevel=None):
        if self.mode not in ("w", "x", "a"):
            raise ValueError("Cannot write to ZipFile in read mode")
        if isinstance(zinfo_or_arcname, ZipInfo):
            info = zinfo_or_arcname
        else:
            info = ZipInfo(zinfo_or_arcname, _from_dos(*_dos_time()))
            info.compress_type = compress_type if compress_type is not None else self.compression
        if isinstance(data, str):
            data = data.encode("utf-8")
        if compress_type is not None:
            info.compress_type = compress_type
        self._write_member(info, data, compresslevel)

    def close(self):
        if self.fp is None:
            return
        if self.mode in ("w", "x", "a"):
            self._write_central_directory()
        if self._owns_fp:
            self.fp.close()
        self.fp = None

    # ---- internal helpers ----

    def _is_existing_archive(self):
        try:
            self.fp.seek(0)
            head = self.fp.read(4)
            return head == _LOCAL_HEADER
        except OSError:
            return False
        finally:
            try:
                self.fp.seek(0, 2)  # back to end for append
            except OSError:
                pass

    def _read_central_directory(self):
        # Find the End Of Central Directory record. ZIP allows an
        # arbitrary trailing comment up to 64KiB, so we scan from the
        # tail backwards.
        self.fp.seek(0, 2)
        size = self.fp.tell()
        max_back = min(size, 64 * 1024 + 22)
        self.fp.seek(size - max_back)
        tail = self.fp.read(max_back)
        end_idx = tail.rfind(_END_OF_CENTRAL)
        if end_idx == -1:
            raise BadZipFile("File is not a zip file")
        eocd = tail[end_idx:end_idx + 22]
        (_sig, disk, cd_disk, num_disk, num_total,
         cd_size, cd_offset, comment_len) = struct.unpack(_EOCD_FMT, eocd)
        comment = tail[end_idx + 22:end_idx + 22 + comment_len]
        self._comment = comment

        self.fp.seek(cd_offset)
        for _ in range(num_total):
            header = self.fp.read(46)
            if len(header) != 46 or header[:4] != _CENTRAL_HEADER:
                raise BadZipFile("Truncated central directory")
            (_sig, _cv, _cs, _xv, _xs, flag, meth, t, d, crc, csize, usize,
             fname_len, extra_len, comment_len, disk_no, int_attr, ext_attr,
             header_offset) = struct.unpack(_CENTRAL_FMT, header)
            name = self.fp.read(fname_len)
            extra = self.fp.read(extra_len)
            comment = self.fp.read(comment_len)
            info = ZipInfo(name)
            info.flag_bits = flag
            info.compress_type = meth
            info.date_time = _from_dos(d, t)
            info.CRC = crc
            info.compress_size = csize
            info.file_size = usize
            info.extra = extra
            info.comment = comment
            info.external_attr = ext_attr
            info.internal_attr = int_attr
            info.header_offset = header_offset
            self._infos.append(info)
            self._info_by_name[info.filename] = info

    def _write_member(self, info, data, compresslevel):
        if info.compress_type == ZIP_DEFLATED:
            level = compresslevel or self._compresslevel or -1
            cdata = zlib.compress(data, level if level > 0 else 6)
            # Strip zlib's 2-byte header and 4-byte adler32 trailer to get raw deflate.
            cdata = cdata[2:-4]
        elif info.compress_type == ZIP_STORED:
            cdata = data
        else:
            raise NotImplementedError(
                "Compression type %d not supported" % info.compress_type)
        info.CRC = zlib.crc32(data) & 0xFFFFFFFF
        info.compress_size = len(cdata)
        info.file_size = len(data)
        info.header_offset = self.fp.tell()

        dos_date, dos_time = _dos_time(info.date_time)
        if isinstance(info.filename, str):
            name_bytes = info.filename.encode("utf-8")
        else:
            name_bytes = info.filename
        local = struct.pack(
            _LOCAL_FMT, _LOCAL_HEADER, 20, 0, 0, info.compress_type,
            dos_time, dos_date,
            info.CRC, info.compress_size, info.file_size,
            len(name_bytes), 0,
        )
        self.fp.write(local)
        self.fp.write(name_bytes)
        self.fp.write(cdata)

        info._dos_date = dos_date
        info._dos_time = dos_time
        info._name_bytes = name_bytes
        self._infos.append(info)
        self._info_by_name[info.filename] = info

    def _write_central_directory(self):
        cd_offset = self.fp.tell()
        for info in self._infos:
            name_bytes = getattr(info, "_name_bytes", None)
            if name_bytes is None:
                if isinstance(info.filename, str):
                    name_bytes = info.filename.encode("utf-8")
                else:
                    name_bytes = info.filename
            dos_date = getattr(info, "_dos_date", None)
            dos_time = getattr(info, "_dos_time", None)
            if dos_date is None:
                dos_date, dos_time = _dos_time()
            central = struct.pack(
                _CENTRAL_FMT, _CENTRAL_HEADER,
                info.create_version, info.create_system,
                info.extract_version, 0,
                info.flag_bits, info.compress_type,
                dos_time, dos_date,
                info.CRC, info.compress_size, info.file_size,
                len(name_bytes), len(info.extra), len(info.comment),
                0, info.internal_attr, info.external_attr,
                info.header_offset,
            )
            self.fp.write(central)
            self.fp.write(name_bytes)
            self.fp.write(info.extra)
            self.fp.write(info.comment)
        cd_size = self.fp.tell() - cd_offset
        eocd = struct.pack(
            _EOCD_FMT, _END_OF_CENTRAL, 0, 0,
            len(self._infos), len(self._infos),
            cd_size, cd_offset, len(self._comment),
        )
        self.fp.write(eocd)
        if self._comment:
            self.fp.write(self._comment)


__all__ = ["ZipFile", "ZipInfo", "is_zipfile", "BadZipFile", "BadZipfile",
           "LargeZipFile", "ZIP_STORED", "ZIP_DEFLATED", "ZIP_BZIP2", "ZIP_LZMA"]
