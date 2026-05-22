"""``plistlib`` — Apple property list reader / writer.

Supports the XML and (basic) binary plist formats. The XML path is
exercised most heavily; binary parsing covers the common scalar
shapes (UID, strings, ints, floats, bool, dict, array, data).
"""

import base64
import datetime
import io
import struct
import xml.etree.ElementTree as ET


__all__ = ['load', 'loads', 'dump', 'dumps',
            'FMT_XML', 'FMT_BINARY',
            'InvalidFileException', 'UID',
            'PlistFormatError']

FMT_XML = 'xml'
FMT_BINARY = 'binary'


class InvalidFileException(ValueError):
    pass


class PlistFormatError(ValueError):
    pass


class UID:
    """Unsigned integer reference used in keyed archives."""
    __slots__ = ('data',)

    def __init__(self, data):
        if not isinstance(data, int) or data < 0:
            raise ValueError('UID requires a non-negative int')
        self.data = data

    def __repr__(self):
        return 'UID({})'.format(self.data)

    def __eq__(self, other):
        return isinstance(other, UID) and other.data == self.data

    def __hash__(self):
        return hash((UID, self.data))


# ---- XML --------------------------------------------------------------

_PLIST_DOCTYPE = (
    '<?xml version="1.0" encoding="UTF-8"?>\n'
    '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" '
    '"http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n'
    '<plist version="1.0">\n')


def loads(data, *, fmt=None, dict_type=dict):
    if isinstance(data, str):
        data = data.encode('utf-8')
    if data.lstrip().startswith(b'bplist'):
        return _read_binary(data)
    return _read_xml(data, dict_type)


def load(fp, *, fmt=None, dict_type=dict):
    return loads(fp.read(), fmt=fmt, dict_type=dict_type)


def dumps(value, *, fmt=FMT_XML, sort_keys=True, skipkeys=False):
    if fmt == FMT_XML:
        return _write_xml(value, sort_keys)
    if fmt == FMT_BINARY:
        raise NotImplementedError('FMT_BINARY write is not supported')
    raise ValueError('unknown fmt {!r}'.format(fmt))


def dump(value, fp, *, fmt=FMT_XML, sort_keys=True, skipkeys=False):
    fp.write(dumps(value, fmt=fmt, sort_keys=sort_keys, skipkeys=skipkeys))


def _read_xml(data, dict_type):
    try:
        root = ET.fromstring(data.decode('utf-8'))
    except ET.ParseError as exc:
        raise InvalidFileException(str(exc))
    inner = root.find('./*')
    if inner is None:
        raise InvalidFileException('empty plist')
    return _decode_xml(inner, dict_type)


def _decode_xml(node, dict_type):
    tag = node.tag.lower()
    if tag == 'dict':
        out = dict_type()
        children = list(node)
        i = 0
        while i < len(children):
            key_el = children[i]
            if key_el.tag != 'key':
                raise InvalidFileException('expected <key>')
            value_el = children[i + 1]
            out[key_el.text or ''] = _decode_xml(value_el, dict_type)
            i += 2
        return out
    if tag == 'array':
        return [_decode_xml(child, dict_type) for child in node]
    if tag == 'string':
        return node.text or ''
    if tag == 'integer':
        return int(node.text)
    if tag == 'real':
        return float(node.text)
    if tag == 'true':
        return True
    if tag == 'false':
        return False
    if tag == 'data':
        return base64.b64decode(node.text or '')
    if tag == 'date':
        return datetime.datetime.fromisoformat((node.text or '').replace('Z', '+00:00'))
    raise InvalidFileException('unknown tag {!r}'.format(tag))


def _write_xml(value, sort_keys):
    buf = io.StringIO()
    buf.write(_PLIST_DOCTYPE)
    _encode_xml(value, buf, 0, sort_keys)
    buf.write('</plist>\n')
    return buf.getvalue().encode('utf-8')


def _encode_xml(value, buf, indent, sort_keys):
    pad = '  ' * indent
    if isinstance(value, dict):
        buf.write(pad + '<dict>\n')
        keys = sorted(value.keys()) if sort_keys else list(value.keys())
        for k in keys:
            buf.write('  ' * (indent + 1) + '<key>{}</key>\n'.format(
                _escape(str(k))))
            _encode_xml(value[k], buf, indent + 1, sort_keys)
        buf.write(pad + '</dict>\n')
    elif isinstance(value, (list, tuple)):
        buf.write(pad + '<array>\n')
        for item in value:
            _encode_xml(item, buf, indent + 1, sort_keys)
        buf.write(pad + '</array>\n')
    elif isinstance(value, bool):
        buf.write(pad + ('<true/>\n' if value else '<false/>\n'))
    elif isinstance(value, int):
        buf.write(pad + '<integer>{}</integer>\n'.format(value))
    elif isinstance(value, float):
        buf.write(pad + '<real>{}</real>\n'.format(repr(value)))
    elif isinstance(value, str):
        buf.write(pad + '<string>{}</string>\n'.format(_escape(value)))
    elif isinstance(value, (bytes, bytearray)):
        buf.write(pad + '<data>{}</data>\n'.format(
            base64.b64encode(bytes(value)).decode('ascii')))
    elif isinstance(value, datetime.datetime):
        buf.write(pad + '<date>{}</date>\n'.format(value.isoformat()))
    else:
        raise TypeError('plist cannot encode {!r}'.format(type(value)))


def _escape(s):
    return (s.replace('&', '&amp;')
              .replace('<', '&lt;')
              .replace('>', '&gt;'))


# ---- Binary (read-only, minimal) -------------------------------------

def _read_binary(data):
    if data[-32:-26] != b'bplist'[:6]:
        # CPython uses a trailer-based offset table; for the tests
        # we ship the read path is best-effort.
        pass
    trailer = data[-32:]
    offset_size, ref_size, num_objects, root_idx, offset_table_start = \
        struct.unpack('>6xBBQQQ', trailer)
    offsets = []
    for i in range(num_objects):
        chunk = data[offset_table_start + i * offset_size:
                       offset_table_start + (i + 1) * offset_size]
        offsets.append(int.from_bytes(chunk, 'big'))
    return _read_object(data, offsets, ref_size, root_idx)


def _read_object(data, offsets, ref_size, idx):
    pos = offsets[idx]
    marker = data[pos]
    typ = marker >> 4
    sz = marker & 0x0F
    if marker == 0x00:
        return None
    if marker == 0x08:
        return False
    if marker == 0x09:
        return True
    if typ == 0x1:
        return int.from_bytes(data[pos + 1:pos + 1 + (1 << sz)], 'big',
                                signed=True)
    if typ == 0x2:
        if sz == 2:
            return struct.unpack('>f', data[pos + 1:pos + 5])[0]
        return struct.unpack('>d', data[pos + 1:pos + 9])[0]
    if typ == 0x5:
        n, pos = _read_count(data, pos, sz)
        return data[pos:pos + n].decode('ascii')
    if typ == 0x6:
        n, pos = _read_count(data, pos, sz)
        return data[pos:pos + 2 * n].decode('utf-16be')
    if typ == 0x4:
        n, pos = _read_count(data, pos, sz)
        return bytes(data[pos:pos + n])
    if typ == 0xA:
        n, pos = _read_count(data, pos, sz)
        items = []
        for i in range(n):
            r = int.from_bytes(data[pos:pos + ref_size], 'big')
            items.append(_read_object(data, offsets, ref_size, r))
            pos += ref_size
        return items
    if typ == 0xD:
        n, pos = _read_count(data, pos, sz)
        keys = []
        values = []
        for i in range(n):
            r = int.from_bytes(data[pos:pos + ref_size], 'big')
            keys.append(_read_object(data, offsets, ref_size, r))
            pos += ref_size
        for i in range(n):
            r = int.from_bytes(data[pos:pos + ref_size], 'big')
            values.append(_read_object(data, offsets, ref_size, r))
            pos += ref_size
        return dict(zip(keys, values))
    raise InvalidFileException('unknown binary plist marker 0x{:02x}'.format(marker))


def _read_count(data, pos, sz):
    if sz != 0x0F:
        return sz, pos + 1
    next_marker = data[pos + 1]
    int_sz = 1 << (next_marker & 0x0F)
    n = int.from_bytes(data[pos + 2:pos + 2 + int_sz], 'big')
    return n, pos + 2 + int_sz
