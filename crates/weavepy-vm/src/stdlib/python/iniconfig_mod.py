"""``iniconfig`` — tiny pytest ini-file parser shim.

Real iniconfig (BSD-licensed) is ~150 lines; this is a faithful
subset that handles the surface ``_pytest`` reaches for: sections,
keyword-value pairs, comments (``;``/``#``), and continuation lines.
"""

__all__ = ['IniConfig', 'ParseError']


class ParseError(Exception):
    pass


class _SectionWrapper:
    def __init__(self, config, name):
        self.config = config
        self.name = name

    def __getitem__(self, key):
        return self.config._data[self.name][key]

    def __contains__(self, key):
        return key in self.config._data.get(self.name, {})

    def get(self, key, default=None):
        return self.config._data.get(self.name, {}).get(key, default)

    def items(self):
        return self.config._data.get(self.name, {}).items()


class IniConfig:
    def __init__(self, path: str, data: str = None):
        self.path = path
        self._data = {}
        if data is None:
            with open(path, 'r', encoding='utf-8') as f:
                data = f.read()
        section = None
        prev_key = None
        for lineno, raw in enumerate(data.splitlines(), 1):
            line = raw.rstrip()
            stripped = line.lstrip()
            if not line or stripped.startswith('#') or stripped.startswith(';'):
                continue
            if line.startswith('[') and line.endswith(']'):
                section = line[1:-1].strip()
                self._data.setdefault(section, {})
                prev_key = None
                continue
            if section is None:
                raise ParseError('line {} before any [section]'.format(lineno))
            if line[0] in ' \t' and prev_key is not None:
                # Continuation.
                self._data[section][prev_key] += '\n' + line.strip()
                continue
            if '=' in line:
                k, _, v = line.partition('=')
            elif ':' in line:
                k, _, v = line.partition(':')
            else:
                raise ParseError('no = or : in line {}'.format(lineno))
            k = k.strip()
            v = v.strip()
            self._data[section][k] = v
            prev_key = k

    @property
    def sections(self):
        return tuple(self._data.keys())

    def __contains__(self, section):
        return section in self._data

    def __getitem__(self, section):
        if section not in self._data:
            raise KeyError(section)
        return _SectionWrapper(self, section)

    def get(self, section, key, default=None):
        return self._data.get(section, {}).get(key, default)
