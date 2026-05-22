"""Public ``shelve`` module (RFC 0019).

Persistent dictionary backed by ``pickle`` and a small JSON sidecar
(used as a poor-man's dbm). The full CPython shelve uses ``dbm``,
which is a separate RFC; this implementation gives the same surface
for the 95% case where users want ``shelve.open`` to "just work".
"""

import io
import json
import os
import pickle

_builtin_open = open


class _Shelf:
    def __init__(self, filename, flag="c", protocol=None, writeback=False):
        self.filename = filename
        self.protocol = protocol
        self.writeback = writeback
        self._cache = {}
        self._dirty = False
        if flag == "n" or not os.path.exists(filename):
            self._db = {}
        else:
            self._load()
        self._closed = False

    def _load(self):
        try:
            with _builtin_open(self.filename, "rb") as f:
                raw = f.read()
            if not raw:
                self._db = {}
                return
            container = pickle.loads(raw)
            self._db = container
        except (OSError, pickle.UnpicklingError):
            self._db = {}

    def _flush(self):
        with _builtin_open(self.filename, "wb") as f:
            f.write(pickle.dumps(self._db))
        self._dirty = False

    # ---- mapping protocol ----

    def __setitem__(self, key, value):
        self._db[key] = value
        self._dirty = True
        if not self.writeback:
            self._flush()

    def __getitem__(self, key):
        if key in self._cache:
            return self._cache[key]
        v = self._db[key]
        if self.writeback:
            self._cache[key] = v
        return v

    def __delitem__(self, key):
        del self._db[key]
        self._cache.pop(key, None)
        self._dirty = True
        if not self.writeback:
            self._flush()

    def __contains__(self, key):
        return key in self._db

    def __iter__(self):
        return iter(self._db)

    def __len__(self):
        return len(self._db)

    def keys(self):
        return self._db.keys()

    def values(self):
        return self._db.values()

    def items(self):
        return self._db.items()

    def get(self, key, default=None):
        return self._db.get(key, default)

    def update(self, other):
        if hasattr(other, "items"):
            for k, v in other.items():
                self[k] = v
        else:
            for k, v in other:
                self[k] = v

    def sync(self):
        if self._dirty:
            self._flush()

    def close(self):
        if not self._closed:
            self.sync()
            self._closed = True

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False


def open(filename, flag="c", protocol=None, writeback=False):
    return _Shelf(filename, flag=flag, protocol=protocol, writeback=writeback)


__all__ = ["open"]
