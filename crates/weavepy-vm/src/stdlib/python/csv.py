"""WeavePy `csv` — Python wrapper over the `_csv` Rust core.

Adds `DictReader`, `DictWriter`, `Sniffer`, dialect registry, and
re-exports of the Rust-defined `reader`, `writer`, and quoting
constants.
"""

import _csv as _native


QUOTE_MINIMAL = _native.QUOTE_MINIMAL
QUOTE_ALL = _native.QUOTE_ALL
QUOTE_NONNUMERIC = _native.QUOTE_NONNUMERIC
QUOTE_NONE = _native.QUOTE_NONE

Error = _native.Error

reader = _native.reader
list_dialects = _native.list_dialects


class _Writer:
    """Thin Python wrapper around the Rust-side writer dict.

    The Rust core returns a dict carrying `writerow` / `writerows`
    closures; we re-expose it as an object with method attributes so
    `w.writerow(...)` works the way CPython users expect.
    """

    def __init__(self, csvfile, dialect="excel"):
        self._native = _native.writer(csvfile, dialect)

    def writerow(self, row):
        return self._native["writerow"](row)

    def writerows(self, rows):
        return self._native["writerows"](rows)


def writer(csvfile, dialect="excel", *args, **kwds):
    return _Writer(csvfile, dialect)


_dialects = {}


class Dialect:
    delimiter = ","
    doublequote = True
    escapechar = None
    lineterminator = "\r\n"
    quotechar = '"'
    quoting = QUOTE_MINIMAL
    skipinitialspace = False
    strict = False


class excel(Dialect):
    delimiter = ","
    quotechar = '"'
    doublequote = True
    skipinitialspace = False
    lineterminator = "\r\n"
    quoting = QUOTE_MINIMAL


class excel_tab(excel):
    delimiter = "\t"


class unix_dialect(Dialect):
    delimiter = ","
    quotechar = '"'
    doublequote = True
    skipinitialspace = False
    lineterminator = "\n"
    quoting = QUOTE_ALL


def register_dialect(name, dialect=None, **kwargs):
    if dialect is None:
        dialect = Dialect
    _dialects[name] = (dialect, kwargs)


def unregister_dialect(name):
    if name in _dialects:
        del _dialects[name]


def get_dialect(name):
    if name not in _dialects:
        raise Error("unknown dialect {!r}".format(name))
    return _dialects[name][0]


register_dialect("excel", excel)
register_dialect("excel-tab", excel_tab)
register_dialect("unix", unix_dialect)


class DictReader:
    """Iterate CSV rows yielded as `dict` keyed by `fieldnames`."""

    def __init__(self, f, fieldnames=None, restkey=None, restval=None,
                 dialect="excel", *args, **kwds):
        self.fieldnames = fieldnames
        self.restkey = restkey
        self.restval = restval
        self._rows = iter(reader(f, dialect))
        self.dialect = dialect
        self.line_num = 0

    def __iter__(self):
        return self

    def __next__(self):
        if self.fieldnames is None:
            self.fieldnames = next(self._rows)
        row = next(self._rows)
        self.line_num += 1
        while row == []:
            row = next(self._rows)
        d = dict(zip(self.fieldnames, row))
        lf = len(self.fieldnames)
        lr = len(row)
        if lf < lr:
            d[self.restkey] = row[lf:]
        elif lf > lr:
            for key in self.fieldnames[lr:]:
                d[key] = self.restval
        return d


class DictWriter:
    """Write rows from `dict` objects."""

    def __init__(self, f, fieldnames, restval="", extrasaction="raise",
                 dialect="excel", *args, **kwds):
        self.fieldnames = fieldnames
        self.restval = restval
        if extrasaction.lower() not in ("raise", "ignore"):
            raise ValueError("extrasaction must be 'raise' or 'ignore', got {!r}".format(extrasaction))
        self.extrasaction = extrasaction
        self.writer = writer(f, dialect)

    def writeheader(self):
        return self.writer.writerow(self.fieldnames)

    def _dict_to_list(self, rowdict):
        if self.extrasaction == "raise":
            extras = set(rowdict.keys()) - set(self.fieldnames)
            if extras:
                raise ValueError("fields not in fieldnames: {}".format(", ".join(extras)))
        return [rowdict.get(key, self.restval) for key in self.fieldnames]

    def writerow(self, rowdict):
        return self.writer.writerow(self._dict_to_list(rowdict))

    def writerows(self, rowdicts):
        for r in rowdicts:
            self.writerow(r)


class Sniffer:
    """Heuristic dialect detection. We implement a very simple version
    that picks a delimiter from a small alphabet."""

    preferred = [",", "\t", ";", "|", ":"]

    def sniff(self, sample, delimiters=None):
        cands = delimiters or self.preferred
        best = ","
        best_score = -1
        for c in cands:
            score = sample.count(c)
            if score > best_score:
                best = c
                best_score = score
        class _D(Dialect):
            delimiter = best
        return _D

    def has_header(self, sample):
        return False


field_size_limit_value = 131072


def field_size_limit(new_limit=None):
    global field_size_limit_value
    old = field_size_limit_value
    if new_limit is not None:
        field_size_limit_value = new_limit
    return old


__all__ = [
    "QUOTE_MINIMAL", "QUOTE_ALL", "QUOTE_NONNUMERIC", "QUOTE_NONE",
    "Error", "reader", "writer", "DictReader", "DictWriter",
    "Sniffer", "Dialect", "excel", "excel_tab", "unix_dialect",
    "register_dialect", "unregister_dialect", "get_dialect",
    "list_dialects", "field_size_limit",
]
