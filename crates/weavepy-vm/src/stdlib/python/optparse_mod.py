"""``optparse`` — legacy command-line option parser.

Deprecated upstream (replaced by ``argparse``) but still in heavy
use. We ship the surface code most callers reach for: ``OptionParser``,
``Option``, ``Values``, ``make_option``, basic actions
(``store``/``store_true``/``store_false``/``append``/``count``), and
``OptionError``.
"""

import sys


__all__ = ['Option', 'OptionContainer', 'OptionGroup', 'OptionParser',
            'Values', 'OptionError', 'OptionConflictError',
            'OptionValueError', 'BadOptionError', 'make_option']


class OptParseError(Exception):
    def __init__(self, msg):
        Exception.__init__(self, msg)
        self.msg = msg

    def __str__(self):
        return self.msg


class OptionError(OptParseError):
    def __init__(self, msg, option=None):
        OptParseError.__init__(self, msg)
        self.option = option


class BadOptionError(OptParseError):
    pass


class OptionConflictError(OptionError):
    pass


class OptionValueError(OptParseError):
    pass


class Values:
    def __init__(self, defaults=None):
        if defaults:
            for k, v in defaults.items():
                setattr(self, k, v)

    def _update(self, dct, mode='loose'):
        for k, v in dct.items():
            setattr(self, k, v)

    def __repr__(self):
        return '<Values {!r}>'.format(self.__dict__)

    def ensure_value(self, attr, value):
        if not hasattr(self, attr) or getattr(self, attr) is None:
            setattr(self, attr, value)
        return getattr(self, attr)


class Option:
    ACTIONS = ('store', 'store_const', 'store_true', 'store_false',
                'append', 'append_const', 'count', 'callback', 'help',
                'version')
    STORE_ACTIONS = ('store', 'store_const', 'store_true', 'store_false',
                      'append', 'append_const', 'count')
    TYPED_ACTIONS = ('store', 'append', 'callback')
    ALWAYS_TYPED_ACTIONS = ('store', 'append')

    TYPES = ('string', 'int', 'long', 'float', 'choice')

    def __init__(self, *opts, **kw):
        self._short_opts = []
        self._long_opts = []
        for o in opts:
            if o.startswith('--'):
                self._long_opts.append(o)
            elif o.startswith('-'):
                self._short_opts.append(o)
            else:
                raise OptionError('option name must start with -', o)
        self.action = kw.pop('action', 'store')
        self.dest = kw.pop('dest', None)
        self.default = kw.pop('default', None)
        self.help = kw.pop('help', None)
        self.const = kw.pop('const', None)
        self.choices = kw.pop('choices', None)
        self.metavar = kw.pop('metavar', None)
        self.type = kw.pop('type', 'string')
        self.callback = kw.pop('callback', None)
        if self.dest is None and self.action in self.STORE_ACTIONS:
            self.dest = self._derive_dest()

    def _derive_dest(self):
        if self._long_opts:
            return self._long_opts[0][2:].replace('-', '_')
        if self._short_opts:
            return self._short_opts[0][1].replace('-', '_')
        return None

    def takes_value(self):
        return self.action in ('store', 'append', 'callback')

    def get_opt_string(self):
        if self._long_opts:
            return self._long_opts[0]
        return self._short_opts[0]


def make_option(*opts, **kw):
    return Option(*opts, **kw)


class OptionContainer:
    def __init__(self, option_class=Option, conflict_handler='error',
                  description=None):
        self.option_class = option_class
        self.conflict_handler = conflict_handler
        self.description = description
        self.option_list = []
        self._short_opt = {}
        self._long_opt = {}
        self._defaults = {}

    def add_option(self, *args, **kwargs):
        if args and isinstance(args[0], Option):
            option = args[0]
        else:
            option = self.option_class(*args, **kwargs)
        for o in option._short_opts:
            if o in self._short_opt and self.conflict_handler == 'error':
                raise OptionConflictError(o, option)
            self._short_opt[o] = option
        for o in option._long_opts:
            if o in self._long_opt and self.conflict_handler == 'error':
                raise OptionConflictError(o, option)
            self._long_opt[o] = option
        self.option_list.append(option)
        if option.dest is not None and option.default is not None:
            self._defaults[option.dest] = option.default
        return option

    def add_options(self, opts):
        for o in opts:
            self.add_option(o)


class OptionGroup(OptionContainer):
    def __init__(self, parser, title, description=None):
        OptionContainer.__init__(self, parser.option_class,
                                    parser.conflict_handler, description)
        self.parser = parser
        self.title = title


class OptionParser(OptionContainer):
    def __init__(self, usage=None, option_list=None, option_class=Option,
                  version=None, conflict_handler='error', description=None,
                  formatter=None, add_help_option=True, prog=None,
                  epilog=None):
        OptionContainer.__init__(self, option_class, conflict_handler,
                                    description)
        self.usage = usage
        self.prog = prog or 'optparse'
        self.version = version
        self.epilog = epilog
        self.allow_interspersed_args = True
        self.process_default_values = True
        if add_help_option:
            self.add_option('-h', '--help', action='help',
                              help='show this help message and exit')
        if version:
            self.add_option('--version', action='version',
                              help="show program's version number and exit")
        if option_list:
            self.add_options(option_list)

    def add_option_group(self, *args, **kwargs):
        if isinstance(args[0], OptionGroup):
            grp = args[0]
        else:
            grp = OptionGroup(self, *args, **kwargs)
        return grp

    def parse_args(self, args=None, values=None):
        if args is None:
            args = sys.argv[1:]
        values = values or Values(self._defaults)
        positional = []
        i = 0
        while i < len(args):
            a = args[i]
            if a == '--':
                positional.extend(args[i + 1:])
                break
            if a.startswith('--'):
                key, _, val = a.partition('=')
                opt = self._long_opt.get(key)
                if opt is None:
                    raise BadOptionError('unknown option {}'.format(key))
                if opt.takes_value():
                    if val == '':
                        i += 1
                        val = args[i] if i < len(args) else ''
                    self._apply(opt, val, values)
                else:
                    self._apply(opt, None, values)
            elif a.startswith('-') and a != '-':
                key = a[:2]
                opt = self._short_opt.get(key)
                if opt is None:
                    raise BadOptionError('unknown option {}'.format(key))
                if opt.takes_value():
                    val = a[2:]
                    if not val:
                        i += 1
                        val = args[i] if i < len(args) else ''
                    self._apply(opt, val, values)
                else:
                    self._apply(opt, None, values)
            else:
                if self.allow_interspersed_args:
                    positional.append(a)
                else:
                    positional.extend(args[i:])
                    break
            i += 1
        return values, positional

    def _apply(self, opt, raw_value, values):
        if opt.action == 'store':
            setattr(values, opt.dest, self._coerce(opt, raw_value))
        elif opt.action == 'store_const':
            setattr(values, opt.dest, opt.const)
        elif opt.action == 'store_true':
            setattr(values, opt.dest, True)
        elif opt.action == 'store_false':
            setattr(values, opt.dest, False)
        elif opt.action == 'append':
            seq = getattr(values, opt.dest, None)
            if seq is None:
                seq = []
                setattr(values, opt.dest, seq)
            seq.append(self._coerce(opt, raw_value))
        elif opt.action == 'append_const':
            seq = getattr(values, opt.dest, None)
            if seq is None:
                seq = []
                setattr(values, opt.dest, seq)
            seq.append(opt.const)
        elif opt.action == 'count':
            setattr(values, opt.dest, getattr(values, opt.dest, 0) + 1)
        elif opt.action == 'callback':
            if opt.callback is not None:
                opt.callback(opt, opt.get_opt_string(), raw_value, self)
        elif opt.action == 'help':
            self.print_help()
            sys.exit(0)
        elif opt.action == 'version':
            print(self.version)
            sys.exit(0)

    def _coerce(self, opt, val):
        if opt.type == 'int':
            return int(val)
        if opt.type in ('float', 'double'):
            return float(val)
        if opt.type == 'choice':
            if val not in opt.choices:
                raise OptionValueError(
                    'option {} requires one of {}'.format(
                        opt.get_opt_string(), opt.choices))
            return val
        return val

    def print_help(self, file=None):
        if file is None:
            file = sys.stdout
        if self.usage:
            file.write('Usage: ' + str(self.usage).replace('%prog', self.prog) + '\n\n')
        if self.description:
            file.write(self.description + '\n\n')
        file.write('Options:\n')
        for opt in self.option_list:
            names = ', '.join(opt._short_opts + opt._long_opts)
            file.write('  {:<24} {}\n'.format(names, opt.help or ''))
        if self.epilog:
            file.write('\n' + self.epilog + '\n')

    def error(self, msg):
        sys.stderr.write('{}: error: {}\n'.format(self.prog, msg))
        sys.exit(2)
