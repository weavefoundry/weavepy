"""Public ``sqlite3`` module (RFC 0019).

Wraps the Rust-backed ``_sqlite3`` core in a CPython-compatible
DB-API 2.0 surface. The Rust core exposes Connection-like and
Cursor-like dicts; this module decorates them with the convenience
behavior CPython users expect (`Connection.execute` shortcut, named
``description`` / ``rowcount`` / ``lastrowid`` properties, and a
context-manager that commits / rolls back).
"""

import _sqlite3

apilevel = "2.0"
threadsafety = 1
paramstyle = "qmark"

sqlite_version = _sqlite3.sqlite_version
sqlite_version_info = _sqlite3.sqlite_version_info
PARSE_DECLTYPES = 1
PARSE_COLNAMES = 2

Binary = bytes


class Error(Exception):
    """Base sqlite3 exception."""


class Warning(Exception):
    """DB-API warning."""


class InterfaceError(Error):
    pass


class DatabaseError(Error):
    pass


class DataError(DatabaseError):
    pass


class OperationalError(DatabaseError):
    pass


class IntegrityError(DatabaseError):
    pass


class InternalError(DatabaseError):
    pass


class ProgrammingError(DatabaseError):
    pass


class NotSupportedError(DatabaseError):
    pass


class Cursor:
    """DB-API 2.0 cursor."""

    def __init__(self, raw):
        self._raw = raw
        self.arraysize = 1
        self.row_factory = None

    @property
    def description(self):
        return self._raw["get_description"]()

    @property
    def rowcount(self):
        return self._raw["get_rowcount"]()

    @property
    def lastrowid(self):
        return self._raw["get_lastrowid"]()

    def execute(self, sql, params=None):
        try:
            self._raw["execute"](sql, params)
        except ValueError as e:
            raise OperationalError(str(e)) from None
        return self

    def executemany(self, sql, seq):
        try:
            self._raw["executemany"](sql, list(seq))
        except ValueError as e:
            raise OperationalError(str(e)) from None
        return self

    def fetchone(self):
        row = self._raw["fetchone"]()
        if row is None:
            return None
        if self.row_factory is not None:
            return self.row_factory(self, row)
        return row

    def fetchall(self):
        rows = self._raw["fetchall"]()
        if self.row_factory is not None:
            return [self.row_factory(self, r) for r in rows]
        return rows

    def fetchmany(self, size=None):
        if size is None:
            size = self.arraysize
        rows = self._raw["fetchmany"](size)
        if self.row_factory is not None:
            return [self.row_factory(self, r) for r in rows]
        return rows

    def close(self):
        self._raw["close"]()

    def __iter__(self):
        return self

    def __next__(self):
        row = self.fetchone()
        if row is None:
            raise StopIteration
        return row


class Connection:
    """DB-API 2.0 connection."""

    def __init__(self, raw):
        self._raw = raw
        self.row_factory = None
        self.text_factory = str
        self.isolation_level = ""
        self.in_transaction = False
        self._closed = False

    def cursor(self, factory=None):
        raw_cursor = self._raw["cursor"]()
        cur = (factory or Cursor)(raw_cursor)
        if self.row_factory is not None and isinstance(cur, Cursor):
            cur.row_factory = self.row_factory
        return cur

    def execute(self, sql, params=None):
        return self.cursor().execute(sql, params)

    def executemany(self, sql, seq):
        return self.cursor().executemany(sql, seq)

    def executescript(self, sql):
        try:
            self._raw["executescript"](sql)
        except ValueError as e:
            raise OperationalError(str(e)) from None

    def commit(self):
        try:
            self._raw["commit"]()
        except ValueError as e:
            raise OperationalError(str(e)) from None

    def rollback(self):
        try:
            self._raw["rollback"]()
        except ValueError as e:
            raise OperationalError(str(e)) from None

    def close(self):
        if not self._closed:
            self._raw["close"]()
            self._closed = True

    def __enter__(self):
        return self

    def __exit__(self, exc_type, exc, tb):
        if exc_type is None:
            self.commit()
        else:
            self.rollback()
        return False


def connect(database, timeout=5.0, detect_types=0, isolation_level="",
            check_same_thread=True, factory=None, cached_statements=128,
            uri=False, **kwargs):
    raw = _sqlite3.connect(database)
    cls = factory or Connection
    return cls(raw)


def register_converter(typename, converter):
    pass


def register_adapter(type, adapter):
    pass


def Row(cursor, row):
    """Default ``sqlite3.Row``-shaped factory.

    Provides indexed *and* keyed access. We keep this as a tiny
    helper class so users get ``row["column"]`` semantics without
    needing the full CPython ``sqlite3.Row``.
    """
    desc = cursor.description or []
    names = [d[0] for d in desc]

    class _Row(tuple):
        def __new__(cls):
            return tuple.__new__(cls, row)

        def keys(self):
            return list(names)

    inst = _Row()
    return inst


__all__ = ["connect", "Connection", "Cursor",
           "Error", "Warning", "InterfaceError", "DatabaseError",
           "DataError", "OperationalError", "IntegrityError",
           "InternalError", "ProgrammingError", "NotSupportedError",
           "Binary", "Row",
           "register_converter", "register_adapter",
           "PARSE_DECLTYPES", "PARSE_COLNAMES",
           "sqlite_version", "sqlite_version_info",
           "apilevel", "threadsafety", "paramstyle"]
