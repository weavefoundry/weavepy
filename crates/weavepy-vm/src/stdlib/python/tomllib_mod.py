"""``tomllib`` — TOML parser (read-only).

A trimmed port of CPython's ``Lib/tomllib`` (which itself is a port
of `tomli`). Surface:

    loads(s)        - parse a TOML string into a dict
    load(file)      - parse a TOML file
    TOMLDecodeError - parser failures

For write support, use a third-party ``tomli_w`` (not bundled).
"""

import re
from datetime import date, datetime, time, timedelta, timezone


__all__ = ['loads', 'load', 'TOMLDecodeError']


class TOMLDecodeError(ValueError):
    """Raised on invalid TOML input."""


_RE_INT = re.compile(r'[+-]?(?:0|[1-9](?:_?\d)*)$')
_RE_BIN = re.compile(r'0b[01](?:_?[01])*$')
_RE_OCT = re.compile(r'0o[0-7](?:_?[0-7])*$')
_RE_HEX = re.compile(r'0x[0-9A-Fa-f](?:_?[0-9A-Fa-f])*$')
_RE_FLOAT = re.compile(
    r'[+-]?(?:'
    r'(?:0|[1-9](?:_?\d)*)(?:\.\d(?:_?\d)*)?(?:[eE][+-]?\d(?:_?\d)*)?'
    r'|nan|inf)$')
_RE_DATETIME = re.compile(
    r'(\d{4}-\d{2}-\d{2})'                # date
    r'(?:[T ](\d{2}:\d{2}:\d{2}(?:\.\d+)?))?'   # time
    r'(Z|[+-]\d{2}:\d{2})?$')
_RE_TIME_ONLY = re.compile(r'\d{2}:\d{2}:\d{2}(?:\.\d+)?$')
_RE_BARE_KEY = re.compile(r'[A-Za-z0-9_-]+')


def load(fp):
    data = fp.read()
    if isinstance(data, bytes):
        data = data.decode('utf-8')
    return loads(data)


def loads(s):
    if isinstance(s, bytes):
        s = s.decode('utf-8')
    parser = _Parser(s)
    return parser.parse()


# --------------------------------------------------------------------- parser

class _Parser:
    def __init__(self, src):
        self.src = src
        self.pos = 0
        self.line = 1
        self.root = {}
        self.current = self.root
        self.defined_tables = set()
        self.explicit_tables = set()
        self.array_tables = set()

    def parse(self):
        self._skip_ws_and_comments()
        while self.pos < len(self.src):
            c = self.src[self.pos]
            if c == '[':
                if self._peek_n(1) == '[':
                    self._parse_array_table()
                else:
                    self._parse_table_header()
            elif c == '#':
                self._skip_comment()
            elif c == '\n':
                self.pos += 1
                self.line += 1
            elif c.isspace():
                self.pos += 1
            else:
                key, value = self._parse_keyvalue()
                self._assign(self.current, key, value)
            self._skip_ws_and_comments()
        return self.root

    # ---- helpers ----

    def _peek_n(self, n):
        if self.pos + n < len(self.src):
            return self.src[self.pos + n]
        return ''

    def _error(self, msg):
        raise TOMLDecodeError('{} at line {}'.format(msg, self.line))

    def _skip_comment(self):
        while self.pos < len(self.src) and self.src[self.pos] != '\n':
            self.pos += 1

    def _skip_ws_and_comments(self):
        while self.pos < len(self.src):
            c = self.src[self.pos]
            if c == '#':
                self._skip_comment()
            elif c == '\n':
                self.pos += 1
                self.line += 1
            elif c in ' \t\r':
                self.pos += 1
            else:
                break

    # ---- keys ----

    def _parse_key_parts(self):
        parts = []
        while True:
            c = self.src[self.pos]
            if c == '"':
                parts.append(self._parse_basic_string(multiline=False))
            elif c == "'":
                parts.append(self._parse_literal_string(multiline=False))
            else:
                m = _RE_BARE_KEY.match(self.src, self.pos)
                if not m:
                    self._error('invalid bare key')
                parts.append(m.group(0))
                self.pos = m.end()
            self._skip_inline_ws()
            if self.pos < len(self.src) and self.src[self.pos] == '.':
                self.pos += 1
                self._skip_inline_ws()
            else:
                break
        return parts

    def _skip_inline_ws(self):
        while self.pos < len(self.src) and self.src[self.pos] in ' \t':
            self.pos += 1

    # ---- key / value pair ----

    def _parse_keyvalue(self):
        parts = self._parse_key_parts()
        if self.pos >= len(self.src) or self.src[self.pos] != '=':
            self._error('expected =')
        self.pos += 1
        self._skip_inline_ws()
        value = self._parse_value()
        self._skip_inline_ws()
        if self.pos < len(self.src) and self.src[self.pos] not in '\n\r#':
            if self.src[self.pos] != '\n':
                self._error('extra content after value')
        return parts, value

    def _assign(self, root, key_parts, value):
        target = root
        for part in key_parts[:-1]:
            existing = target.get(part)
            if existing is None:
                new = {}
                target[part] = new
                target = new
            elif isinstance(existing, dict):
                target = existing
            elif isinstance(existing, list) and existing \
                    and isinstance(existing[-1], dict):
                target = existing[-1]
            else:
                self._error('cannot extend non-table {!r}'.format(part))
        leaf = key_parts[-1]
        if leaf in target and not isinstance(target[leaf], dict):
            self._error('duplicate key {!r}'.format(leaf))
        target[leaf] = value

    # ---- table headers ----

    def _parse_table_header(self):
        self.pos += 1  # skip [
        self._skip_inline_ws()
        parts = self._parse_key_parts()
        self._skip_inline_ws()
        if self.pos >= len(self.src) or self.src[self.pos] != ']':
            self._error('expected closing ]')
        self.pos += 1
        target = self.root
        for part in parts[:-1]:
            target = target.setdefault(part, {})
            if not isinstance(target, dict):
                self._error('cannot redefine non-table {!r}'.format(part))
        leaf = parts[-1]
        target.setdefault(leaf, {})
        self.current = target[leaf]

    def _parse_array_table(self):
        self.pos += 2  # skip [[
        self._skip_inline_ws()
        parts = self._parse_key_parts()
        self._skip_inline_ws()
        if self.src[self.pos:self.pos + 2] != ']]':
            self._error('expected closing ]]')
        self.pos += 2
        target = self.root
        for part in parts[:-1]:
            target = target.setdefault(part, {})
            if not isinstance(target, dict):
                self._error('cannot extend non-table {!r}'.format(part))
        leaf = parts[-1]
        arr = target.setdefault(leaf, [])
        if not isinstance(arr, list):
            self._error('{} is not an array-of-tables'.format(leaf))
        new = {}
        arr.append(new)
        self.current = new

    # ---- values ----

    def _parse_value(self):
        c = self.src[self.pos]
        if c == '"':
            if self.src[self.pos:self.pos + 3] == '"""':
                return self._parse_basic_string(multiline=True)
            return self._parse_basic_string(multiline=False)
        if c == "'":
            if self.src[self.pos:self.pos + 3] == "'''":
                return self._parse_literal_string(multiline=True)
            return self._parse_literal_string(multiline=False)
        if c == '[':
            return self._parse_array()
        if c == '{':
            return self._parse_inline_table()
        return self._parse_scalar()

    def _parse_basic_string(self, multiline):
        self.pos += 3 if multiline else 1
        out = []
        if multiline and self.pos < len(self.src) and self.src[self.pos] == '\n':
            self.pos += 1
            self.line += 1
        while self.pos < len(self.src):
            c = self.src[self.pos]
            if c == '\\':
                self.pos += 1
                if self.pos >= len(self.src):
                    self._error('unterminated string escape')
                esc = self.src[self.pos]
                if esc in '"\\':
                    out.append(esc)
                elif esc == 'n':
                    out.append('\n')
                elif esc == 't':
                    out.append('\t')
                elif esc == 'r':
                    out.append('\r')
                elif esc == 'b':
                    out.append('\b')
                elif esc == 'f':
                    out.append('\f')
                elif esc == '/':
                    out.append('/')
                elif esc == 'u':
                    out.append(chr(int(self.src[self.pos + 1:self.pos + 5], 16)))
                    self.pos += 4
                elif esc == 'U':
                    out.append(chr(int(self.src[self.pos + 1:self.pos + 9], 16)))
                    self.pos += 8
                elif esc == '\n' and multiline:
                    self.line += 1
                    self.pos += 1
                    while self.pos < len(self.src) and self.src[self.pos] in ' \t\n':
                        if self.src[self.pos] == '\n':
                            self.line += 1
                        self.pos += 1
                    continue
                else:
                    self._error('invalid escape \\{}'.format(esc))
                self.pos += 1
                continue
            if multiline and self.src[self.pos:self.pos + 3] == '"""':
                self.pos += 3
                return ''.join(out)
            if not multiline and c == '"':
                self.pos += 1
                return ''.join(out)
            if c == '\n':
                if not multiline:
                    self._error('newline in single-line string')
                self.line += 1
            out.append(c)
            self.pos += 1
        self._error('unterminated string')

    def _parse_literal_string(self, multiline):
        self.pos += 3 if multiline else 1
        start = self.pos
        if multiline and self.pos < len(self.src) and self.src[self.pos] == '\n':
            self.pos += 1
            start += 1
            self.line += 1
        while self.pos < len(self.src):
            if multiline and self.src[self.pos:self.pos + 3] == "'''":
                out = self.src[start:self.pos]
                self.pos += 3
                return out
            if not multiline and self.src[self.pos] == "'":
                out = self.src[start:self.pos]
                self.pos += 1
                return out
            if self.src[self.pos] == '\n':
                if not multiline:
                    self._error('newline in single-line string')
                self.line += 1
            self.pos += 1
        self._error('unterminated literal string')

    def _parse_array(self):
        self.pos += 1
        out = []
        while True:
            self._skip_ws_and_comments()
            if self.pos >= len(self.src):
                self._error('unterminated array')
            if self.src[self.pos] == ']':
                self.pos += 1
                return out
            value = self._parse_value()
            out.append(value)
            self._skip_ws_and_comments()
            if self.pos < len(self.src) and self.src[self.pos] == ',':
                self.pos += 1
            elif self.pos < len(self.src) and self.src[self.pos] == ']':
                self.pos += 1
                return out
            else:
                self._error('expected , or ]')

    def _parse_inline_table(self):
        self.pos += 1
        out = {}
        self._skip_inline_ws()
        if self.pos < len(self.src) and self.src[self.pos] == '}':
            self.pos += 1
            return out
        while True:
            parts = self._parse_key_parts()
            if self.pos >= len(self.src) or self.src[self.pos] != '=':
                self._error('expected = in inline table')
            self.pos += 1
            self._skip_inline_ws()
            value = self._parse_value()
            self._assign(out, parts, value)
            self._skip_inline_ws()
            if self.pos < len(self.src) and self.src[self.pos] == ',':
                self.pos += 1
                self._skip_inline_ws()
            elif self.pos < len(self.src) and self.src[self.pos] == '}':
                self.pos += 1
                return out
            else:
                self._error('expected , or }')

    def _parse_scalar(self):
        start = self.pos
        while self.pos < len(self.src) and \
                self.src[self.pos] not in '\n,#]}':
            self.pos += 1
        text = self.src[start:self.pos].strip()
        if text == 'true':
            return True
        if text == 'false':
            return False
        if _RE_INT.match(text):
            return int(text.replace('_', ''))
        if _RE_HEX.match(text):
            return int(text[2:].replace('_', ''), 16)
        if _RE_OCT.match(text):
            return int(text[2:].replace('_', ''), 8)
        if _RE_BIN.match(text):
            return int(text[2:].replace('_', ''), 2)
        if _RE_FLOAT.match(text):
            return float(text.replace('_', ''))
        m = _RE_DATETIME.match(text)
        if m:
            return _parse_datetime(m)
        if _RE_TIME_ONLY.match(text):
            return time.fromisoformat(text)
        self._error('unrecognised value {!r}'.format(text))


def _parse_datetime(m):
    date_part, time_part, tz = m.group(1), m.group(2), m.group(3)
    if not time_part:
        return date.fromisoformat(date_part)
    if tz:
        if tz == 'Z':
            tzinfo = timezone.utc
        else:
            sign = 1 if tz[0] == '+' else -1
            h, mins = tz[1:].split(':')
            tzinfo = timezone(timedelta(hours=int(h), minutes=int(mins)) * sign)
    else:
        tzinfo = None
    if '.' in time_part:
        hms, frac = time_part.split('.')
    else:
        hms, frac = time_part, ''
    h, mi, s = hms.split(':')
    micro = int((frac + '000000')[:6]) if frac else 0
    return datetime(
        *[int(x) for x in date_part.split('-')],
        hour=int(h), minute=int(mi), second=int(s),
        microsecond=micro, tzinfo=tzinfo)
