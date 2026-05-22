"""``profile`` / ``cProfile`` — call-by-call profiler.

WeavePy ships a single profiler module that satisfies both
``import profile`` and ``import cProfile`` (CPython has separate
pure-Python and C implementations; we don't).

The profiler hooks ``sys.setprofile``, collecting per-function call
counts and cumulative durations. Output is consumable by ``pstats``.
"""

import marshal
import sys
import time as _time


__all__ = ['Profile', 'run', 'runctx']


class Profile:
    """Per-function call counter + timer."""

    def __init__(self, timer=None, bias=0):
        self.timer = timer or _time.perf_counter
        self.bias = bias
        self.timings = {}   # frame.code -> [calls, total_time, cum_time]
        self._call_stack = []  # [(code, t_entry)]

    # ---- sys.setprofile callback -----------------------------------------

    def _dispatch(self, frame, event, arg):
        if event == 'call':
            self._call(frame)
        elif event == 'return':
            self._return(frame)

    def _call(self, frame):
        code = frame.f_code
        self._call_stack.append((code, self.timer()))

    def _return(self, frame):
        if not self._call_stack:
            return
        code, t_entry = self._call_stack.pop()
        if code is not frame.f_code:
            # Mismatch (likely an exception unwind). Push the popped
            # entry back so subsequent returns line up.
            self._call_stack.append((code, t_entry))
            return
        dt = self.timer() - t_entry - self.bias
        stats = self.timings.setdefault(code, [0, 0.0, 0.0])
        stats[0] += 1
        stats[1] += dt
        stats[2] += dt

    # ---- driver ---------------------------------------------------------

    def enable(self):
        sys.setprofile(self._dispatch)

    def disable(self):
        sys.setprofile(None)

    def __enter__(self):
        self.enable()
        return self

    def __exit__(self, *exc):
        self.disable()

    def run(self, cmd):
        if isinstance(cmd, str):
            cmd = compile(cmd, '<profile>', 'exec')
        globals_ = {'__name__': '__main__'}
        self.enable()
        try:
            exec(cmd, globals_)
        finally:
            self.disable()
        return self

    def runctx(self, cmd, globals_, locals_):
        if isinstance(cmd, str):
            cmd = compile(cmd, '<profile>', 'exec')
        self.enable()
        try:
            exec(cmd, globals_, locals_)
        finally:
            self.disable()
        return self

    def runcall(self, func, *args, **kwargs):
        self.enable()
        try:
            return func(*args, **kwargs)
        finally:
            self.disable()

    def create_stats(self):
        return self.stats()

    def stats(self):
        out = {}
        for code, vals in self.timings.items():
            key = (
                getattr(code, 'co_filename', '<?>'),
                getattr(code, 'co_firstlineno', 0),
                getattr(code, 'co_name', '<?>'),
            )
            out[key] = (vals[0], vals[0], vals[1], vals[2], {})
        return out

    def print_stats(self, sort=-1):
        import pstats
        pstats.Stats(self).sort_stats(sort).print_stats()

    def dump_stats(self, file):
        with open(file, 'wb') as f:
            marshal.dump(self.stats(), f)


def run(statement, filename=None, sort=-1):
    """Convenience: profile ``statement`` and print stats."""
    p = Profile()
    p.run(statement)
    if filename:
        p.dump_stats(filename)
    else:
        p.print_stats(sort)


def runctx(statement, globals_, locals_, filename=None, sort=-1):
    p = Profile()
    p.runctx(statement, globals_, locals_)
    if filename:
        p.dump_stats(filename)
    else:
        p.print_stats(sort)
