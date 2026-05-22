"""``pstats`` — printable statistics over a ``Profile`` object."""

import sys


__all__ = ['Stats']


class Stats:
    def __init__(self, *args):
        self.stats = {}
        self.sort_key = None
        for arg in args:
            self._load(arg)

    def _load(self, source):
        if hasattr(source, 'create_stats'):
            self._merge(source.create_stats())
        elif isinstance(source, str):
            import marshal
            with open(source, 'rb') as f:
                self._merge(marshal.load(f))
        else:
            self._merge(source)

    def _merge(self, stats):
        for key, vals in stats.items():
            if key in self.stats:
                cur = list(self.stats[key])
                cur[0] += vals[0]
                cur[1] += vals[1]
                cur[2] += vals[2]
                cur[3] += vals[3]
                self.stats[key] = tuple(cur)
            else:
                self.stats[key] = vals

    def add(self, *args):
        for arg in args:
            self._load(arg)
        return self

    def sort_stats(self, *fields):
        if not fields:
            fields = ('cumulative',)
        key_map = {
            'calls': 0, 'ncalls': 0,
            'pcalls': 1,
            'tottime': 2, 'time': 2,
            'cumulative': 3, 'cumtime': 3,
            -1: 3, 1: 2, 2: 0, 3: 3,
            'nfl': 'nfl', 'stdname': 'stdname',
        }
        keys = [key_map.get(f, f) for f in fields]
        def sort_key(item):
            key, vals = item
            out = []
            for k in keys:
                if k == 'nfl':
                    out.append((key[2], key[0], key[1]))
                elif k == 'stdname':
                    out.append(key)
                else:
                    out.append(-vals[k] if isinstance(k, int) else 0)
            return tuple(out)
        self.sorted = sorted(self.stats.items(), key=sort_key)
        return self

    def print_stats(self, *amount):
        items = getattr(self, 'sorted', None) or list(self.stats.items())
        if amount:
            n = amount[0]
            if isinstance(n, int) and n > 0:
                items = items[:n]
        total_calls = sum(v[0] for v in self.stats.values())
        total_time = sum(v[2] for v in self.stats.values())
        print('{} function calls in {:.3f} seconds\n'.format(
            total_calls, total_time))
        print('   ncalls  tottime  percall  cumtime  percall  filename:lineno(function)')
        for key, vals in items:
            ncalls, _, tottime, cumtime, _ = vals
            filename, lineno, name = key
            pct_tt = tottime / ncalls if ncalls else 0
            pct_ct = cumtime / ncalls if ncalls else 0
            print('   {:>6}  {:>7.3f}  {:>7.3f}  {:>7.3f}  {:>7.3f}  {}:{}({})'.format(
                ncalls, tottime, pct_tt, cumtime, pct_ct, filename, lineno, name))
        return self

    def print_callers(self, *amount):
        return self

    def print_callees(self, *amount):
        return self

    def strip_dirs(self):
        new = {}
        for (path, line, func), vals in self.stats.items():
            base = path.rsplit('/', 1)[-1]
            new[(base, line, func)] = vals
        self.stats = new
        return self

    def dump_stats(self, filename):
        import marshal
        with open(filename, 'wb') as f:
            marshal.dump(self.stats, f)
