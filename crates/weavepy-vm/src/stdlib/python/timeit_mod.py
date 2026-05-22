"""``timeit`` — measure execution time of small code snippets.

The user surface (``timeit.timeit``, ``timeit.repeat``,
``timeit.Timer``) mirrors CPython's ``Lib/timeit.py``. The
``python -m timeit`` CLI is included.
"""

import gc
import sys
import time as _time


__all__ = ['Timer', 'timeit', 'repeat', 'default_timer', 'default_number',
            'default_repeat', 'reindent', 'main']

default_number = 1000000
default_repeat = 5
default_timer = _time.perf_counter

TEMPLATE = """
def inner(_it, _timer{init}):
    {setup}
    _t0 = _timer()
    for _i in _it:
        {stmt}
    _t1 = _timer()
    return _t1 - _t0
"""


def reindent(src, indent):
    """Indent every line of ``src`` by ``indent`` columns."""
    return src.replace('\n', '\n' + ' ' * indent)


class Timer:
    def __init__(self, stmt='pass', setup='pass', timer=default_timer,
                  globals=None):
        self.timer = timer
        local_ns = {}
        global_ns = globals if globals is not None else {}
        if isinstance(stmt, str):
            stmt = reindent(stmt, 8)
        else:
            local_ns['_stmt'] = stmt
            stmt = '_stmt()'
        if isinstance(setup, str):
            setup = reindent(setup, 4)
            init = ''
        else:
            local_ns['_setup'] = setup
            setup = '_setup()'
            init = ''
        src = TEMPLATE.format(stmt=stmt, setup=setup, init=init)
        code = compile(src, '<timeit-src>', 'exec')
        exec(code, global_ns, local_ns)
        self.src = src
        self.inner = local_ns['inner']

    def timeit(self, number=default_number):
        gc_was = gc.isenabled() if hasattr(gc, 'isenabled') else False
        if hasattr(gc, 'disable'):
            gc.disable()
        try:
            return self.inner(_iter_times(number), self.timer)
        finally:
            if gc_was and hasattr(gc, 'enable'):
                gc.enable()

    def repeat(self, repeat=default_repeat, number=default_number):
        return [self.timeit(number) for _ in range(repeat)]

    def autorange(self, callback=None):
        i = 1
        while True:
            for j in (1, 2, 5):
                number = i * j
                t = self.timeit(number)
                if callback:
                    callback(number, t)
                if t >= 0.2:
                    return number, t
            i *= 10


def _iter_times(n):
    return [None] * n


def timeit(stmt='pass', setup='pass', timer=default_timer, number=default_number,
            globals=None):
    return Timer(stmt, setup, timer, globals).timeit(number)


def repeat(stmt='pass', setup='pass', timer=default_timer,
            repeat=default_repeat, number=default_number, globals=None):
    return Timer(stmt, setup, timer, globals).repeat(repeat, number)


def main(args=None):
    """``python -m timeit``."""
    import getopt
    if args is None:
        args = sys.argv[1:]
    opts, prog = getopt.getopt(
        args, 'n:s:r:u:t:vh',
        ['number=', 'setup=', 'repeat=', 'unit=', 'verbose', 'help'])
    number = 0
    setup = []
    repeat_n = default_repeat
    unit = None
    verbose = False
    for opt, val in opts:
        if opt in ('-n', '--number'):
            number = int(val)
        elif opt in ('-s', '--setup'):
            setup.append(val)
        elif opt in ('-r', '--repeat'):
            repeat_n = int(val)
        elif opt in ('-u', '--unit'):
            unit = val
        elif opt in ('-v', '--verbose'):
            verbose = True
        elif opt in ('-h', '--help'):
            print(__doc__)
            return 0
    stmt = '\n'.join(prog) if prog else 'pass'
    setup_src = '\n'.join(setup) if setup else 'pass'
    timer = Timer(stmt, setup_src)
    if number == 0:
        number, t = timer.autorange()
    times = timer.repeat(repeat_n, number)
    best = min(times)
    per_loop = best / number
    units = {'ns': 1e9, 'us': 1e6, 'ms': 1e3, 's': 1.0}
    if unit and unit in units:
        scale = units[unit]
    elif per_loop < 1e-6:
        unit, scale = 'ns', 1e9
    elif per_loop < 1e-3:
        unit, scale = 'us', 1e6
    elif per_loop < 1:
        unit, scale = 'ms', 1e3
    else:
        unit, scale = 's', 1.0
    print('{} loops, best of {}: {:.3f} {} per loop'.format(
        number, repeat_n, per_loop * scale, unit))
    return 0


if __name__ == '__main__':
    sys.exit(main())
