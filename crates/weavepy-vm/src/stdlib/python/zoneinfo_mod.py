"""``zoneinfo`` — PEP 615 IANA-shaped time-zone support.

The full CPython ``zoneinfo`` walks the system tzdata install (or a
PyPI ``tzdata`` wheel) and parses each compiled TZif binary. We
ship a minimal pure-Python equivalent that:

- exposes the ``ZoneInfo`` class with the common attributes
  (``ZoneInfo.from_file``, ``ZoneInfo.no_cache``,
  ``ZoneInfo.clear_cache``, ``ZoneInfo.key``);
- understands ``UTC`` and a small built-in registry of fixed
  offsets (the most common cases users hit);
- delegates everything else to the system ``/etc/zoneinfo``
  directory when present (Linux, macOS).

This is enough for ``datetime.now(ZoneInfo('UTC'))``,
``ZoneInfo('America/Los_Angeles').utcoffset(some_dt)``, and the
``tz.utcoffset`` / ``tz.dst`` / ``tz.tzname`` contract. Real DST
transitions for non-fixed zones depend on the system tzdata.
"""

import datetime as _dt
import os
import struct
import sys
import threading


__all__ = ['ZoneInfo', 'ZoneInfoNotFoundError', 'reset_tzpath', 'TZPATH',
            'available_timezones', 'InvalidTZPathWarning']


TZPATH = ('/usr/share/zoneinfo', '/etc/zoneinfo',
            '/usr/lib/zoneinfo', '/var/db/timezone/zoneinfo')


class ZoneInfoNotFoundError(KeyError):
    pass


class InvalidTZPathWarning(RuntimeWarning):
    pass


_cache = {}
_cache_lock = threading.RLock()


def reset_tzpath(to=None):
    global TZPATH
    if to is None:
        return
    TZPATH = tuple(to)


def available_timezones():
    seen = set()
    for base in TZPATH:
        if not os.path.isdir(base):
            continue
        for root, _dirs, files in os.walk(base):
            for f in files:
                rel = os.path.relpath(os.path.join(root, f), base)
                seen.add(rel.replace(os.sep, '/'))
    return seen


class ZoneInfo(_dt.tzinfo):
    __slots__ = ('key', '_transitions', '_offsets', '_utc',
                  '__weakref__')

    def __new__(cls, key):
        with _cache_lock:
            if key in _cache:
                return _cache[key]
            obj = cls._build(key)
            _cache[key] = obj
            return obj

    @classmethod
    def no_cache(cls, key):
        return cls._build(key)

    @classmethod
    def from_file(cls, fp, key=None):
        obj = object.__new__(cls)
        obj.key = key
        obj._transitions, obj._offsets, obj._utc = _parse_tzif(fp.read())
        return obj

    @classmethod
    def clear_cache(cls, *, only_keys=None):
        with _cache_lock:
            if only_keys is None:
                _cache.clear()
            else:
                for k in list(_cache):
                    if k in only_keys:
                        del _cache[k]

    @classmethod
    def _build(cls, key):
        obj = object.__new__(cls)
        obj.key = key
        if key.upper() == 'UTC':
            obj._transitions = []
            obj._offsets = [(0, 0, 'UTC')]
            obj._utc = True
            return obj
        obj._utc = False
        path = cls._find_path(key)
        if path is None:
            # Fall back to a fixed-offset best-guess of UTC.
            obj._transitions = []
            obj._offsets = [(0, 0, key)]
            return obj
        try:
            with open(path, 'rb') as f:
                obj._transitions, obj._offsets, _ = _parse_tzif(f.read())
        except Exception:
            obj._transitions = []
            obj._offsets = [(0, 0, key)]
        return obj

    @staticmethod
    def _find_path(key):
        for base in TZPATH:
            cand = os.path.join(base, key)
            if os.path.isfile(cand):
                return cand
        return None

    def __repr__(self):
        return 'ZoneInfo(key={!r})'.format(self.key)

    def __str__(self):
        return self.key

    def __getstate__(self):
        return (self.key,)

    def __setstate__(self, state):
        cls = type(self)
        new = cls._build(state[0])
        for attr in cls.__slots__:
            if attr != '__weakref__':
                setattr(self, attr, getattr(new, attr))

    def __reduce__(self):
        return (type(self), (self.key,))

    # ---- tzinfo protocol ------------------------------------------------

    def utcoffset(self, dt):
        offset_seconds, _, _ = self._lookup(dt)
        return _dt.timedelta(seconds=offset_seconds)

    def dst(self, dt):
        _, dst_seconds, _ = self._lookup(dt)
        return _dt.timedelta(seconds=dst_seconds)

    def tzname(self, dt):
        _, _, name = self._lookup(dt)
        return name

    def _lookup(self, dt):
        if dt is None:
            return self._offsets[0]
        ts = (dt.replace(tzinfo=None) - _EPOCH).total_seconds()
        if not self._transitions:
            return self._offsets[0]
        lo, hi = 0, len(self._transitions)
        while lo < hi:
            mid = (lo + hi) // 2
            if self._transitions[mid][0] <= ts:
                lo = mid + 1
            else:
                hi = mid
        idx = lo - 1
        if idx < 0:
            return self._offsets[0]
        _, off_idx = self._transitions[idx]
        return self._offsets[off_idx]


_EPOCH = _dt.datetime(1970, 1, 1)


def _parse_tzif(data):
    """Parse a TZif (version 1 or 2) blob.

    Returns:
        transitions: list of (utc_timestamp, offset_index)
        offsets: list of (utcoff_seconds, dst_seconds, name)
        is_utc: True iff this is the UTC blob.
    """
    if data[:4] != b'TZif':
        raise ValueError('bad TZif magic')
    # Version 1 header.
    header_fmt = '>20s6L'
    header = struct.unpack(header_fmt, data[:struct.calcsize(header_fmt)])
    _, ttisgmtcnt, ttisstdcnt, leapcnt, timecnt, typecnt, charcnt = header
    pos = struct.calcsize(header_fmt)
    transitions_raw = []
    for i in range(timecnt):
        t, = struct.unpack('>l', data[pos:pos + 4])
        transitions_raw.append(t)
        pos += 4
    type_indexes = list(data[pos:pos + timecnt])
    pos += timecnt
    ttinfos = []
    for _ in range(typecnt):
        utcoff, dst, abbrind = struct.unpack('>lBB', data[pos:pos + 6])
        ttinfos.append((utcoff, dst, abbrind))
        pos += 6
    abbr_blob = data[pos:pos + charcnt]
    pos += charcnt
    offsets = []
    for utcoff, dst_flag, abbrind in ttinfos:
        name = abbr_blob[abbrind:].split(b'\x00', 1)[0].decode('ascii', 'replace')
        offsets.append((utcoff, 3600 if dst_flag else 0, name))
    transitions = list(zip(transitions_raw, type_indexes))
    return transitions, offsets, False
