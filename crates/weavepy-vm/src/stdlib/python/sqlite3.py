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


class PrepareProtocol:
    """Adapter-protocol marker (CPython's ``sqlite3.PrepareProtocol``)."""


# type -> callable registries backing ``register_adapter`` /
# ``register_converter`` (CPython keeps these process-global too).
adapters = {}
converters = {}


def _adapt(value):
    """Apply the adapter protocol to one bind parameter.

    CPython looks up the exact type first, then falls back to
    ``__conform__``; we additionally walk the MRO so a subclass of a
    registered type (e.g. ``pd.Timestamp`` vs ``datetime``) adapts too.
    """
    adapter = adapters.get((type(value), PrepareProtocol))
    if adapter is None:
        for base in type(value).__mro__[1:]:
            adapter = adapters.get((base, PrepareProtocol))
            if adapter is not None:
                break
    if adapter is not None:
        return adapter(value)
    conform = getattr(value, '__conform__', None)
    if conform is not None:
        try:
            adapted = conform(PrepareProtocol)
        except TypeError:
            adapted = None
        if adapted is not None:
            return adapted
    return value


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


def _raise_db_error(e):
    """Map a raw-core ``ValueError`` onto the CPython exception type."""
    msg = str(e)
    if 'connection closed' in msg:
        raise ProgrammingError('Cannot operate on a closed database.') from None
    raise OperationalError(msg) from None


class Cursor:
    """DB-API 2.0 cursor."""

    def __init__(self, raw, detect_types=0):
        self._raw = raw
        self._detect_types = detect_types
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
        if params is not None:
            params = [_adapt(p) for p in params]
        try:
            self._raw["execute"](sql, params)
        except ValueError as e:
            _raise_db_error(e)
        return self

    def executemany(self, sql, seq):
        rows = [[_adapt(p) for p in row] for row in seq]
        try:
            self._raw["executemany"](sql, rows)
        except ValueError as e:
            _raise_db_error(e)
        return self

    def _convert_row(self, row):
        """Apply ``detect_types``/``register_converter`` to one row.

        CPython hands the converter the raw value as *bytes* (pandas'
        converters call ``val.decode()``), keyed by the column's declared
        type's first word, upper-cased."""
        if not (self._detect_types & PARSE_DECLTYPES) or not converters:
            return row
        get_decl = self._raw.get("get_decltypes")
        if get_decl is None:
            return row
        decls = get_decl()
        out = list(row)
        for i, decl in enumerate(decls):
            if i >= len(out) or not decl or out[i] is None:
                continue
            key = decl.split('(')[0].split()[0].upper() if decl.strip() else ''
            conv = converters.get(key)
            if conv is not None:
                val = out[i]
                if isinstance(val, str):
                    val = val.encode('utf-8')
                elif not isinstance(val, bytes):
                    val = str(val).encode('utf-8')
                out[i] = conv(val)
        return tuple(out)

    def fetchone(self):
        row = self._raw["fetchone"]()
        if row is None:
            return None
        row = self._convert_row(row)
        if self.row_factory is not None:
            return self.row_factory(self, row)
        return row

    def fetchall(self):
        rows = [self._convert_row(r) for r in self._raw["fetchall"]()]
        if self.row_factory is not None:
            return [self.row_factory(self, r) for r in rows]
        return rows

    def fetchmany(self, size=None):
        if size is None:
            size = self.arraysize
        rows = [self._convert_row(r) for r in self._raw["fetchmany"](size)]
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

    def __init__(self, raw, detect_types=0, **kwargs):
        # CPython's ``Connection`` is directly constructible from a
        # database path (``sqlite3.Connection(":memory:")``); the shim
        # also accepts the raw core dict handed out by ``connect``.
        if isinstance(raw, (str, bytes)) or hasattr(raw, '__fspath__'):
            import os
            raw = _sqlite3.connect(os.fspath(raw) if not isinstance(raw, (str, bytes)) else raw)
        self._raw = raw
        self._detect_types = detect_types
        self.row_factory = None
        self.text_factory = str
        self.isolation_level = ""
        self.in_transaction = False
        self._closed = False

    def cursor(self, factory=None):
        if self._closed:
            raise ProgrammingError('Cannot operate on a closed database.')
        raw_cursor = self._raw["cursor"]()
        if factory is not None:
            cur = factory(raw_cursor)
        else:
            cur = Cursor(raw_cursor, self._detect_types)
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
            _raise_db_error(e)

    def commit(self):
        if self._closed:
            raise ProgrammingError('Cannot operate on a closed database.')
        try:
            self._raw["commit"]()
        except ValueError as e:
            _raise_db_error(e)

    def rollback(self):
        if self._closed:
            raise ProgrammingError('Cannot operate on a closed database.')
        try:
            self._raw["rollback"]()
        except ValueError as e:
            _raise_db_error(e)

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
    return cls(raw, detect_types=detect_types)


def register_converter(typename, converter):
    """Register a converter applied on fetch to columns whose declared
    type matches ``typename`` (requires ``detect_types=PARSE_DECLTYPES``)."""
    converters[typename.upper()] = converter


def register_adapter(type, adapter):
    """Register an adapter used to convert bind parameters of ``type``."""
    adapters[(type, PrepareProtocol)] = adapter


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


__all__ = ["connect", "Connection", "Cursor", "PrepareProtocol",
           "Error", "Warning", "InterfaceError", "DatabaseError",
           "DataError", "OperationalError", "IntegrityError",
           "InternalError", "ProgrammingError", "NotSupportedError",
           "Binary", "Row",
           "register_converter", "register_adapter",
           "adapters", "converters",
           "PARSE_DECLTYPES", "PARSE_COLNAMES",
           "sqlite_version", "sqlite_version_info",
           "apilevel", "threadsafety", "paramstyle"]
