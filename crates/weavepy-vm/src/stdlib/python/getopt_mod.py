"""``getopt`` — Unix-style command-line option parsing.

Mirrors CPython's ``Lib/getopt.py``: ``getopt(args, shortopts,
longopts)`` for the strict POSIX behaviour and ``gnu_getopt`` for
the GNU permutation extension.
"""

__all__ = ['getopt', 'gnu_getopt', 'GetoptError', 'error']


class GetoptError(Exception):
    """Raised when an unknown option is encountered or an option
    requires an argument that isn't given.
    """

    def __init__(self, msg, opt=''):
        self.msg = msg
        self.opt = opt
        Exception.__init__(self, msg, opt)

    def __str__(self):
        return self.msg


error = GetoptError


def getopt(args, shortopts, longopts=None):
    """Strict POSIX getopt: option processing stops at the first
    non-option argument.
    """
    opts = []
    if longopts is None:
        longopts = []
    elif isinstance(longopts, str):
        longopts = [longopts]
    longopts = list(longopts)
    while args and args[0].startswith('-') and args[0] != '-':
        if args[0] == '--':
            args = args[1:]
            break
        if args[0].startswith('--'):
            opts, args = _do_longs(opts, args[0][2:], longopts, args[1:])
        else:
            opts, args = _do_shorts(opts, args[0][1:], shortopts, args[1:])
    return opts, args


def gnu_getopt(args, shortopts, longopts=None):
    """Permutation getopt: option args may appear anywhere on the
    command line. A leading ``-`` in ``shortopts`` reverts to POSIX
    strict mode; ``+`` does the same upstream.
    """
    opts = []
    prog_args = []
    if longopts is None:
        longopts = []
    elif isinstance(longopts, str):
        longopts = [longopts]
    longopts = list(longopts)
    if shortopts.startswith('+'):
        shortopts = shortopts[1:]
        all_options_first = True
    elif 'POSIXLY_CORRECT' in __import__('os').environ:
        all_options_first = True
    else:
        all_options_first = False
    while args:
        if args[0] == '--':
            prog_args.extend(args[1:])
            break
        if args[0].startswith('--'):
            opts, args = _do_longs(opts, args[0][2:], longopts, args[1:])
        elif args[0].startswith('-') and args[0] != '-':
            opts, args = _do_shorts(opts, args[0][1:], shortopts, args[1:])
        else:
            if all_options_first:
                prog_args.extend(args)
                break
            prog_args.append(args[0])
            args = args[1:]
    return opts, prog_args


def _do_longs(opts, opt, longopts, args):
    try:
        i = opt.index('=')
    except ValueError:
        optarg = None
    else:
        opt, optarg = opt[:i], opt[i + 1:]
    has_arg, opt = _long_has_args(opt, longopts)
    if has_arg:
        if optarg is None:
            if not args:
                raise GetoptError(
                    'option --{} requires argument'.format(opt), opt)
            optarg, args = args[0], args[1:]
    elif optarg is not None:
        raise GetoptError(
            'option --{} must not have argument'.format(opt), opt)
    opts.append(('--' + opt, optarg or ''))
    return opts, args


def _long_has_args(opt, longopts):
    possibilities = [o for o in longopts if o.startswith(opt)]
    if not possibilities:
        raise GetoptError('option --{} not recognised'.format(opt), opt)
    if opt in possibilities:
        return False, opt
    elif (opt + '=') in possibilities:
        return True, opt
    if len(possibilities) > 1:
        raise GetoptError(
            'option --{} not a unique prefix'.format(opt), opt)
    chosen = possibilities[0]
    has_arg = chosen.endswith('=')
    if has_arg:
        chosen = chosen[:-1]
    return has_arg, chosen


def _do_shorts(opts, optstring, shortopts, args):
    while optstring:
        opt, optstring = optstring[0], optstring[1:]
        if _short_has_arg(opt, shortopts):
            if optstring == '':
                if not args:
                    raise GetoptError(
                        'option -{} requires argument'.format(opt), opt)
                optstring, args = args[0], args[1:]
            opts.append(('-' + opt, optstring))
            optstring = ''
        else:
            opts.append(('-' + opt, ''))
    return opts, args


def _short_has_arg(opt, shortopts):
    for i, c in enumerate(shortopts):
        if c == opt and c != ':':
            return shortopts[i + 1:i + 2] == ':'
    raise GetoptError('option -{} not recognised'.format(opt), opt)
