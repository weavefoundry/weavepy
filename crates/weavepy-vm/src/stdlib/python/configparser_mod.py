"""``configparser`` — INI-file parser.

Trimmed port of CPython's ``Lib/configparser.py``. Supports the
core ``[section]`` / ``key = value`` shape, including
``DEFAULTSECT``, value continuation, ``%``-style interpolation in
``ConfigParser`` (and "raw" mode in ``RawConfigParser``).

The deep corners (``ExtendedInterpolation``, the
``BasicInterpolation`` chain, converter registration, ``read_dict``
edge cases) follow the same spelling but with a smaller surface.
"""

import io
import os
import re
import sys


__all__ = [
    'DEFAULTSECT',
    'NoSectionError',
    'DuplicateOptionError',
    'DuplicateSectionError',
    'NoOptionError',
    'InterpolationError',
    'InterpolationDepthError',
    'InterpolationSyntaxError',
    'InterpolationMissingOptionError',
    'ParsingError',
    'MissingSectionHeaderError',
    'ConfigParser',
    'RawConfigParser',
    'SafeConfigParser',
    'BasicInterpolation',
    'Interpolation',
]


DEFAULTSECT = 'DEFAULT'

_BOOLEAN_STATES = {
    '1': True, 'yes': True, 'true': True, 'on': True,
    '0': False, 'no': False, 'false': False, 'off': False,
}


class Error(Exception):
    pass


class NoSectionError(Error):
    def __init__(self, section):
        Error.__init__(self, 'No section: %r' % section)
        self.section = section


class DuplicateSectionError(Error):
    def __init__(self, section, source=None, lineno=None):
        Error.__init__(
            self, 'Section %r already exists' % section)


class DuplicateOptionError(Error):
    def __init__(self, section, option, source=None, lineno=None):
        Error.__init__(
            self, 'Option %r already exists in section %r' % (option, section))


class NoOptionError(Error):
    def __init__(self, option, section):
        Error.__init__(self, 'No option %r in section %r' % (option, section))


class InterpolationError(Error):
    pass


class InterpolationDepthError(InterpolationError):
    pass


class InterpolationSyntaxError(InterpolationError):
    pass


class InterpolationMissingOptionError(InterpolationError):
    pass


class ParsingError(Error):
    pass


class MissingSectionHeaderError(ParsingError):
    pass


SECT_TMPL = re.compile(r'\[(?P<header>[^]]+)\]\s*$')
OPT_TMPL = re.compile(
    r'(?P<option>[^=:\s][^=:]*)\s*(?P<vi>[=:])\s*(?P<value>.*)$')


class Interpolation:
    def before_get(self, parser, section, option, value, defaults):
        return value

    def before_set(self, parser, section, option, value):
        return value

    def before_read(self, parser, section, option, value):
        return value

    def before_write(self, parser, section, option, value):
        return value


class BasicInterpolation(Interpolation):
    _KEYCRE = re.compile(r'%\(([^)]+)\)s')
    _MAX_DEPTH = 10

    def before_get(self, parser, section, option, value, defaults):
        return self._interpolate(parser, section, option, value, defaults, 1)

    def _interpolate(self, parser, section, option, value, defaults, depth):
        if depth > self._MAX_DEPTH:
            raise InterpolationDepthError
        def sub(m):
            key = parser.optionxform(m.group(1))
            if key in defaults:
                v = defaults[key]
            elif parser.has_option(section, key):
                v = parser.get(section, key, raw=True)
            else:
                raise InterpolationMissingOptionError(key)
            return self._interpolate(parser, section, option, v, defaults,
                                       depth + 1)
        return self._KEYCRE.sub(sub, value)


class RawConfigParser:
    """Raw (no interpolation) INI parser."""

    _DEFAULT_INTERPOLATION = Interpolation()

    def __init__(self, defaults=None, dict_type=dict, allow_no_value=False, *,
                  delimiters=('=', ':'), comment_prefixes=('#', ';'),
                  inline_comment_prefixes=None, strict=True,
                  empty_lines_in_values=True, default_section=DEFAULTSECT,
                  interpolation=None, converters=None):
        self._dict = dict_type
        self._sections = self._dict()
        self._defaults = self._dict()
        if defaults:
            for k, v in defaults.items():
                self._defaults[self.optionxform(k)] = v
        self._allow_no_value = allow_no_value
        self._delimiters = tuple(delimiters)
        self._comment_prefixes = tuple(comment_prefixes)
        self._inline_comment_prefixes = (
            tuple(inline_comment_prefixes) if inline_comment_prefixes else ())
        self._strict = strict
        self._empty_lines_in_values = empty_lines_in_values
        self._default_section = default_section
        self._interpolation = (
            interpolation if interpolation else self._DEFAULT_INTERPOLATION)

    def defaults(self):
        return self._defaults

    def sections(self):
        return list(self._sections.keys())

    def add_section(self, section):
        if section == self._default_section:
            raise ValueError(
                'invalid section name: %r' % section)
        if section in self._sections:
            raise DuplicateSectionError(section)
        self._sections[section] = self._dict()

    def has_section(self, section):
        return section in self._sections

    def options(self, section):
        try:
            opts = self._sections[section].copy()
        except KeyError:
            raise NoSectionError(section)
        opts.update(self._defaults)
        return list(opts.keys())

    def read(self, filenames, encoding=None):
        if isinstance(filenames, (str, bytes, os.PathLike)):
            filenames = [filenames]
        read_ok = []
        for filename in filenames:
            try:
                with open(filename, encoding=encoding or 'utf-8') as fp:
                    self._read(fp, str(filename))
            except OSError:
                continue
            read_ok.append(filename)
        return read_ok

    def read_file(self, f, source=None):
        if source is None:
            source = getattr(f, 'name', '<???>')
        self._read(f, source)

    def read_string(self, string, source='<string>'):
        sfile = io.StringIO(string)
        self.read_file(sfile, source)

    def read_dict(self, dictionary, source='<dict>'):
        for sec, opts in dictionary.items():
            sec = str(sec)
            if sec == self._default_section:
                target = self._defaults
            else:
                if sec not in self._sections:
                    self._sections[sec] = self._dict()
                target = self._sections[sec]
            for k, v in opts.items():
                target[self.optionxform(str(k))] = (
                    str(v) if v is not None else None)

    def get(self, section, option, *, raw=False, vars=None, fallback=None):
        try:
            opt = self.optionxform(option)
            if section == self._default_section:
                value = self._defaults[opt]
            else:
                if section not in self._sections:
                    raise NoSectionError(section)
                if opt in self._sections[section]:
                    value = self._sections[section][opt]
                elif opt in self._defaults:
                    value = self._defaults[opt]
                else:
                    raise NoOptionError(opt, section)
        except (NoSectionError, NoOptionError):
            if fallback is not None:
                return fallback
            raise
        if raw:
            return value
        merged = dict(self._defaults)
        if section in self._sections:
            merged.update(self._sections[section])
        if vars:
            for k, v in vars.items():
                merged[self.optionxform(k)] = v
        return self._interpolation.before_get(self, section, option, value,
                                                merged)

    def getint(self, section, option, **kw):
        return int(self.get(section, option, **kw))

    def getfloat(self, section, option, **kw):
        return float(self.get(section, option, **kw))

    def getboolean(self, section, option, **kw):
        v = self.get(section, option, **kw)
        if v is None:
            return False
        if str(v).lower() not in _BOOLEAN_STATES:
            raise ValueError('Not a boolean: %s' % v)
        return _BOOLEAN_STATES[str(v).lower()]

    def items(self, section=None, raw=False, vars=None):
        if section is None:
            return list(self._sections.items())
        opts = self.options(section)
        return [(opt, self.get(section, opt, raw=raw, vars=vars))
                  for opt in opts]

    def set(self, section, option, value=None):
        if section != self._default_section and section not in self._sections:
            raise NoSectionError(section)
        target = (self._defaults if section == self._default_section
                    else self._sections[section])
        target[self.optionxform(option)] = value

    def has_option(self, section, option):
        opt = self.optionxform(option)
        return (section in self._sections and opt in self._sections[section]) \
            or opt in self._defaults

    def remove_option(self, section, option):
        if section == self._default_section:
            opts = self._defaults
        elif section not in self._sections:
            raise NoSectionError(section)
        else:
            opts = self._sections[section]
        opt = self.optionxform(option)
        existed = opt in opts
        if existed:
            del opts[opt]
        return existed

    def remove_section(self, section):
        if section in self._sections:
            del self._sections[section]
            return True
        return False

    def optionxform(self, opt):
        return opt.lower()

    def write(self, fp, space_around_delimiters=True):
        d = ' = ' if space_around_delimiters else '='
        if self._defaults:
            self._write_section(fp, self._default_section,
                                  self._defaults.items(), d)
        for section, items in self._sections.items():
            self._write_section(fp, section, items.items(), d)

    def _write_section(self, fp, section, items, delimiter):
        fp.write('[%s]\n' % section)
        for key, value in items:
            if value is None and self._allow_no_value:
                fp.write('%s\n' % key)
            else:
                fp.write('%s%s%s\n' % (key, delimiter, value))
        fp.write('\n')

    def __getitem__(self, key):
        if key != self._default_section and key not in self._sections:
            raise KeyError(key)
        return _SectionProxy(self, key)

    def __setitem__(self, key, value):
        if key == self._default_section:
            self._defaults.clear()
            target = self._defaults
        else:
            self._sections[key] = self._dict()
            target = self._sections[key]
        for k, v in value.items():
            target[self.optionxform(k)] = str(v) if v is not None else None

    def __contains__(self, key):
        return key == self._default_section or key in self._sections

    def __iter__(self):
        return iter([self._default_section] + list(self._sections))

    # ---- parser ----------------------------------------------------------

    def _read(self, fp, source):
        cursect = None
        cur_option = None
        lineno = 0
        e = None
        for line in fp:
            lineno += 1
            stripped = line.strip()
            if not stripped:
                cur_option = None
                continue
            if stripped.startswith(self._comment_prefixes):
                continue
            if self._inline_comment_prefixes:
                for prefix in self._inline_comment_prefixes:
                    if prefix in line:
                        line = line.split(prefix, 1)[0]
                        stripped = line.strip()
                        if not stripped:
                            break
            if line.startswith((' ', '\t')) and cur_option:
                if cursect is None:
                    raise MissingSectionHeaderError(
                        'continuation without section')
                cursect[cur_option] += '\n' + stripped
                continue
            m = SECT_TMPL.match(stripped)
            if m:
                name = m.group('header').strip()
                if name == self._default_section:
                    cursect = self._defaults
                elif name in self._sections:
                    if self._strict:
                        raise DuplicateSectionError(name, source, lineno)
                    cursect = self._sections[name]
                else:
                    self._sections[name] = self._dict()
                    cursect = self._sections[name]
                cur_option = None
                continue
            if cursect is None:
                raise MissingSectionHeaderError(stripped)
            m = OPT_TMPL.match(stripped)
            if m:
                option = m.group('option').strip()
                value = m.group('value').strip()
                key = self.optionxform(option)
                cursect[key] = value
                cur_option = key
            elif self._allow_no_value:
                key = self.optionxform(stripped)
                cursect[key] = None
                cur_option = key
            else:
                raise ParsingError('line %d: %s' % (lineno, stripped))


class ConfigParser(RawConfigParser):
    _DEFAULT_INTERPOLATION = BasicInterpolation()


class SafeConfigParser(ConfigParser):
    """Deprecated alias kept for compatibility."""


class _SectionProxy:
    def __init__(self, parser, name):
        self._parser = parser
        self._name = name

    def __getitem__(self, key):
        return self._parser.get(self._name, key)

    def __setitem__(self, key, value):
        self._parser.set(self._name, key, value)

    def __contains__(self, key):
        return self._parser.has_option(self._name, key)

    def keys(self):
        return self._parser.options(self._name)

    def values(self):
        return [self._parser.get(self._name, k) for k in self.keys()]

    def items(self):
        return self._parser.items(self._name)

    def get(self, key, fallback=None):
        return self._parser.get(self._name, key, fallback=fallback)
