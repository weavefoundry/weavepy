"""WeavePy `mimetypes` — extension to MIME-type mapping.

This is a baked-in static table covering the common types listed in
CPython's `mimetypes.types_map`. The module exposes the standard
public surface (`guess_type`, `guess_extension`, `add_type`,
`init`, `MimeTypes`, ...).
"""

import os


__all__ = [
    "knownfiles", "inited", "MimeTypes", "guess_type", "guess_all_extensions",
    "guess_extension", "add_type", "init", "read_mime_types", "suffix_map",
    "encodings_map", "types_map", "common_types",
]


knownfiles = []
inited = False


suffix_map = {
    ".svgz": ".svg.gz",
    ".tgz": ".tar.gz",
    ".taz": ".tar.gz",
    ".tz": ".tar.gz",
    ".tbz2": ".tar.bz2",
    ".txz": ".tar.xz",
}


encodings_map = {
    ".gz": "gzip",
    ".Z": "compress",
    ".bz2": "bzip2",
    ".xz": "xz",
    ".br": "br",
}


_TYPES = {
    ".a": "application/octet-stream",
    ".ai": "application/postscript",
    ".aif": "audio/x-aiff",
    ".aifc": "audio/x-aiff",
    ".aiff": "audio/x-aiff",
    ".au": "audio/basic",
    ".avi": "video/x-msvideo",
    ".bat": "text/plain",
    ".bcpio": "application/x-bcpio",
    ".bin": "application/octet-stream",
    ".bmp": "image/bmp",
    ".c": "text/plain",
    ".cdf": "application/x-netcdf",
    ".cpio": "application/x-cpio",
    ".csh": "application/x-csh",
    ".css": "text/css",
    ".csv": "text/csv",
    ".dll": "application/octet-stream",
    ".doc": "application/msword",
    ".dot": "application/msword",
    ".dvi": "application/x-dvi",
    ".eml": "message/rfc822",
    ".eps": "application/postscript",
    ".etx": "text/x-setext",
    ".exe": "application/octet-stream",
    ".gif": "image/gif",
    ".gtar": "application/x-gtar",
    ".h": "text/plain",
    ".hdf": "application/x-hdf",
    ".htm": "text/html",
    ".html": "text/html",
    ".ico": "image/vnd.microsoft.icon",
    ".ief": "image/ief",
    ".jpe": "image/jpeg",
    ".jpeg": "image/jpeg",
    ".jpg": "image/jpeg",
    ".js": "application/javascript",
    ".json": "application/json",
    ".latex": "application/x-latex",
    ".m1v": "video/mpeg",
    ".m3u": "application/vnd.apple.mpegurl",
    ".m3u8": "application/vnd.apple.mpegurl",
    ".man": "application/x-troff-man",
    ".md": "text/markdown",
    ".me": "application/x-troff-me",
    ".mht": "message/rfc822",
    ".mhtml": "message/rfc822",
    ".mif": "application/x-mif",
    ".mov": "video/quicktime",
    ".movie": "video/x-sgi-movie",
    ".mp2": "audio/mpeg",
    ".mp3": "audio/mpeg",
    ".mp4": "video/mp4",
    ".mpa": "video/mpeg",
    ".mpe": "video/mpeg",
    ".mpeg": "video/mpeg",
    ".mpg": "video/mpeg",
    ".ms": "application/x-troff-ms",
    ".nc": "application/x-netcdf",
    ".nws": "message/rfc822",
    ".o": "application/octet-stream",
    ".obj": "application/octet-stream",
    ".oda": "application/oda",
    ".p12": "application/x-pkcs12",
    ".p7c": "application/pkcs7-mime",
    ".pbm": "image/x-portable-bitmap",
    ".pdf": "application/pdf",
    ".pfx": "application/x-pkcs12",
    ".pgm": "image/x-portable-graymap",
    ".png": "image/png",
    ".pnm": "image/x-portable-anymap",
    ".pot": "application/vnd.ms-powerpoint",
    ".ppa": "application/vnd.ms-powerpoint",
    ".ppm": "image/x-portable-pixmap",
    ".pps": "application/vnd.ms-powerpoint",
    ".ppt": "application/vnd.ms-powerpoint",
    ".ps": "application/postscript",
    ".pwz": "application/vnd.ms-powerpoint",
    ".py": "text/x-python",
    ".pyc": "application/x-python-code",
    ".pyo": "application/x-python-code",
    ".qt": "video/quicktime",
    ".ra": "audio/x-pn-realaudio",
    ".ram": "application/x-pn-realaudio",
    ".rdf": "application/xml",
    ".rgb": "image/x-rgb",
    ".roff": "application/x-troff",
    ".rtx": "text/richtext",
    ".sgm": "text/x-sgml",
    ".sgml": "text/x-sgml",
    ".sh": "application/x-sh",
    ".shar": "application/x-shar",
    ".snd": "audio/basic",
    ".so": "application/octet-stream",
    ".src": "application/x-wais-source",
    ".sv4cpio": "application/x-sv4cpio",
    ".sv4crc": "application/x-sv4crc",
    ".svg": "image/svg+xml",
    ".swf": "application/x-shockwave-flash",
    ".t": "application/x-troff",
    ".tar": "application/x-tar",
    ".tcl": "application/x-tcl",
    ".tex": "application/x-tex",
    ".texi": "application/x-texinfo",
    ".texinfo": "application/x-texinfo",
    ".tif": "image/tiff",
    ".tiff": "image/tiff",
    ".tr": "application/x-troff",
    ".tsv": "text/tab-separated-values",
    ".txt": "text/plain",
    ".ustar": "application/x-ustar",
    ".vcf": "text/x-vcard",
    ".wasm": "application/wasm",
    ".wav": "audio/x-wav",
    ".webm": "video/webm",
    ".webmanifest": "application/manifest+json",
    ".wiz": "application/msword",
    ".wsdl": "application/xml",
    ".xbm": "image/x-xbitmap",
    ".xlb": "application/vnd.ms-excel",
    ".xls": "application/vnd.ms-excel",
    ".xml": "text/xml",
    ".xpdl": "application/xml",
    ".xpm": "image/x-xpixmap",
    ".xsl": "application/xml",
    ".xwd": "image/x-xwindowdump",
    ".yaml": "application/yaml",
    ".yml": "application/yaml",
    ".zip": "application/zip",
}


types_map = dict(_TYPES)
common_types = {}


class MimeTypes:
    """Class wrapper around `guess_type`/`guess_extension`."""

    def __init__(self, filenames=(), strict=True):
        self.types_map = (dict(_TYPES), {})
        self.types_map_inv = ({}, {})
        self.encodings_map = dict(encodings_map)
        self.suffix_map = dict(suffix_map)
        for ext, ty in _TYPES.items():
            self.types_map_inv[0].setdefault(ty, []).append(ext)

    def guess_type(self, url, strict=True):
        return guess_type(url, strict)

    def guess_extension(self, type, strict=True):
        return guess_extension(type, strict)

    def guess_all_extensions(self, type, strict=True):
        return guess_all_extensions(type, strict)

    def add_type(self, type, ext, strict=True):
        add_type(type, ext, strict)


def _split_filename(filename):
    base, ext = os.path.splitext(filename.lower())
    while ext in suffix_map:
        base, ext = os.path.splitext(base + suffix_map[ext])
    encoding = None
    if ext in encodings_map:
        encoding = encodings_map[ext]
        base, ext = os.path.splitext(base)
    return ext, encoding


def guess_type(url, strict=True):
    """Return `(type, encoding)` for `url`."""
    ext, encoding = _split_filename(url)
    if ext in types_map:
        return types_map[ext], encoding
    return None, encoding


def guess_extension(type, strict=True):
    for ext, ty in types_map.items():
        if ty == type:
            return ext
    return None


def guess_all_extensions(type, strict=True):
    return [ext for ext, ty in types_map.items() if ty == type]


def add_type(type, ext, strict=True):
    types_map[ext] = type


def init(files=None):
    global inited
    inited = True


def read_mime_types(file):
    return {}
