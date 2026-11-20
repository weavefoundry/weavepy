"""Method calls on builtin values in hot loops.

A loop that runs long enough is compiled twice over, and the second
compiler loads `d.pop` and calls it as two steps. For a builtin value (a
dict, list, str, set, ...) whose method needs no Python to run, the load
leaves the receiver where the bound method would go and the call runs the
method on it directly. Nothing may tell the difference: the result, the
order of evaluation, the exception and where it is raised, and what an
argument sees when it runs Python between the load and the call.

Each case runs a loop long enough to be compiled, checks every result,
and compares with the same loop run cold (too short to compile).
"""

import collections
import sys
import unittest

HOT = 60_000
COLD = 40


def both(test, fn, *args):
    """`fn(n, *args)` hot agrees with a cold run's prefix and returns."""
    cold = fn(COLD, *args)
    hot = fn(HOT, *args)
    test.assertEqual(hot[: len(cold)] if isinstance(hot, list) else None, cold if isinstance(cold, list) else None)
    return hot


class DictMethodTests(unittest.TestCase):
    def test_pop_oldest(self):
        def run(n):
            d = {i: str(i) for i in range(100)}
            out = []
            for i in range(100, 100 + n):
                d[i] = str(i)
                v = d.pop(i - 100)
                if i % 997 == 0 or i < 140:
                    out.append(v)
            out.append(len(d))
            out.append(list(d)[:3])
            return out

        hot = run(HOT)
        self.assertEqual(hot[-2], 100)
        self.assertEqual(hot[-1], [HOT, HOT + 1, HOT + 2])
        self.assertEqual(run(COLD)[:40], hot[:40])

    def test_pop_default_and_missing(self):
        def run(n):
            d = {i: i for i in range(0, 2 * n, 2)}
            hits = misses = 0
            for i in range(n):
                if d.pop(i, None) is None:
                    misses += 1
                else:
                    hits += 1
            return [hits, misses, len(d)]

        self.assertEqual(run(HOT), [HOT // 2, HOT // 2, HOT // 2])
        self.assertEqual(run(COLD), [COLD // 2, COLD // 2, COLD // 2])

    def test_pop_raises_where_it_should(self):
        def run(n):
            d = {i: i for i in range(n)}
            total = 0
            try:
                for i in range(n + 5):
                    total += d.pop(i)
            except KeyError as e:
                return [total, e.args[0], len(d)]
            return [total, None, len(d)]

        self.assertEqual(run(HOT), [HOT * (HOT - 1) // 2, HOT, 0])
        self.assertEqual(run(COLD), [COLD * (COLD - 1) // 2, COLD, 0])

    def test_get_setdefault_keys_items(self):
        def run(n):
            d = {}
            total = 0
            for i in range(n):
                k = i % 50
                total += d.setdefault(k, i)
                total += d.get(k, -1)
                total += d.get(-1 - k, 3)
                if i % 5000 == 0:
                    total += len(d.keys()) + len(d.values()) + len(d.items())
            return [total, len(d)]

        hot, cold = run(HOT), run(COLD)
        want = 0
        d = {}
        for i in range(HOT):
            k = i % 50
            want += d.setdefault(k, i) + d.get(k, -1) + d.get(-1 - k, 3)
            if i % 5000 == 0:
                want += 3 * len(d)
        self.assertEqual(hot, [want, 50])
        self.assertEqual(cold[1], 40)

    def test_argument_runs_python_between_load_and_call(self):
        # The receiver's method is looked up before the argument runs; the
        # argument then changes the dict the method works on.
        def run(n):
            d = {}
            seen = []

            def key(i):
                d[i] = i * 2
                return i

            total = 0
            for i in range(n):
                total += d.pop(key(i))
                if i % 9973 == 0:
                    seen.append(len(d))
            return [total, seen, len(d)]

        hot = run(HOT)
        self.assertEqual(hot[0], HOT * (HOT - 1))
        self.assertEqual(hot[2], 0)
        self.assertEqual(run(COLD)[0], COLD * (COLD - 1))

    def test_argument_rebinds_the_receiver_name(self):
        # `d.pop(f())` pops from the dict `d` named at the load, whatever
        # `f` binds the name to meanwhile.
        def run(n):
            first = {i: "first" for i in range(n)}
            second = {i: "second" for i in range(n)}
            d = first
            out = []

            def swap(i):
                nonlocal d
                d = second if d is first else first
                return i

            for i in range(n):
                out.append(d.pop(swap(i)))
            return [out[:6], len(first), len(second), out.count("first")]

        hot = run(HOT)
        self.assertEqual(hot[0], ["first", "second"] * 3)
        self.assertEqual((hot[1], hot[2], hot[3]), (HOT // 2, HOT // 2, HOT // 2))

    def test_argument_raises_between_load_and_call(self):
        def run(n):
            d = {i: i for i in range(n)}
            caught = 0
            total = 0

            def key(i):
                if i % 1000 == 999:
                    raise ValueError(i)
                return i

            for i in range(n):
                try:
                    total += d.pop(key(i))
                except ValueError:
                    caught += 1
            return [total, caught, len(d)]

        hot = run(HOT)
        self.assertEqual(hot[1], HOT // 1000)
        self.assertEqual(hot[2], HOT // 1000)
        self.assertEqual(hot[0], sum(i for i in range(HOT) if i % 1000 != 999))

    def test_receiver_kind_changes_at_one_site(self):
        class Popper:
            def __init__(self):
                self.calls = 0

            def pop(self, k, default=None):
                self.calls += 1
                return ("popper", k)

        def run(n):
            d = {i: i for i in range(n)}
            p = Popper()
            od = collections.OrderedDict((i, -i) for i in range(n))
            xs = list(range(n))
            out = []
            for i in range(n):
                phase = (i // 7) % 4
                recv = (d, p, od, xs)[phase]
                v = recv.pop(i) if phase != 3 else recv.pop()
                if i % 3001 == 0 or i < 30:
                    out.append(v)
            return [out, p.calls, len(d), len(od), len(xs)]

        hot = run(HOT)
        cold = run(COLD)
        self.assertEqual(hot[0][:8], [0, 1, 2, 3, 4, 5, 6, ("popper", 7)])
        self.assertEqual(cold[0][:8], hot[0][:8])
        self.assertEqual(hot[1] + (HOT - hot[2]) + (HOT - hot[3]) + (HOT - hot[4]), HOT)

    def test_missing_method_raises_at_the_load(self):
        def run(n):
            d = {}
            order = []

            def arg():
                order.append("arg")
                return 1

            caught = 0
            for i in range(n):
                recv = d if i % 500 else 5
                try:
                    recv.setdefault(arg(), i)
                except AttributeError:
                    caught += 1
            return [caught, len(order)]

        # (An int has no setdefault: the load raises before the argument runs.)
        self.assertEqual(run(HOT), [HOT // 500, HOT - HOT // 500])


# Loops with enough integer work around each call for the second compiler
# to take them (it leaves a loop that is all method calls to the first),
# written at module level with plain locals. Each has a reference that
# reaches the same values without a method call.


def pop_loop(n):
    d = {i: i for i in range(100)}
    acc = 0
    for i in range(100, 100 + n):
        d[i] = i
        a = i * 3 + 1
        b = a - i
        c = (b ^ i) & 1023
        acc += d.pop(i - 100) + (a - b - c) % 7
    return acc, len(d)


def pop_loop_reference(n):
    acc = 0
    for i in range(100, 100 + n):
        a = i * 3 + 1
        b = a - i
        c = (b ^ i) & 1023
        acc += (i - 100) + (a - b - c) % 7
    return acc, 100


def overflow_in_argument(n, scale):
    # `scale * (i + 1)` outgrows a machine word between the load of
    # `d.get` and its call.
    d = {i: i for i in range(64)}
    acc = 0
    for i in range(n):
        scale = scale * 2 + (i & 1)
        k = (i * 7 + 3) & 63
        t = (k + 1) * 3 - k
        acc += d.get((scale * (i + 1)) & 63, -1) + t - (t & 1)
    return acc


def overflow_in_argument_reference(n, scale):
    acc = 0
    for i in range(n):
        scale = scale * 2 + (i & 1)
        k = (i * 7 + 3) & 63
        t = (k + 1) * 3 - k
        acc += ((scale * (i + 1)) & 63) + t - (t & 1)
    return acc


def division_in_argument(n, bad):
    # The argument divides by zero at `i == bad`, after `d.get` is loaded.
    d = {i: i for i in range(64)}
    acc = 0
    for i in range(n):
        a = i * 5 + 2
        b = (a ^ i) & 63
        acc += d.get(b // (bad - i), -7) + a - (a & 3) + b
    return acc


def division_in_argument_reference(n, bad):
    acc = 0
    for i in range(min(n, bad)):
        a = i * 5 + 2
        b = (a ^ i) & 63
        q = b // (bad - i)
        acc += (q if q < 64 else -7) + a - (a & 3) + b
    return acc


class Doubler:
    def get(self, k, default):
        return k * 2


def changing_receivers(n, four):
    # One call site, whose receiver is a dict three times in four.
    acc = 0
    for i in range(n):
        a = i * 3 + 1
        b = (a - i) & 15
        acc += four[i & 3].get(b, -1) + a - b + (i & 7)
    return acc


def changing_receivers_reference(n):
    acc = 0
    for i in range(n):
        a = i * 3 + 1
        b = (a - i) & 15
        which = i & 3
        got = b if which == 0 else b * 2 if which == 1 else (-b if b < 8 else -1) if which == 2 else -1
        acc += got + a - b + (i & 7)
    return acc


def changing_kinds(n, four):
    # One call site over lists and tuples: two builtins of one name.
    acc = 0
    for i in range(n):
        a = i * 3 + 1
        b = (a - i) & 15
        o = four[i & 3]
        acc += o.count(b) + a - b + (i & 7)
    return acc


def changing_kinds_reference(n, four):
    acc = 0
    for i in range(n):
        a = i * 3 + 1
        b = (a - i) & 15
        acc += sum(1 for x in four[i & 3] if x == b) + a - b + (i & 7)
    return acc


def nested_calls(n):
    d = {i: i + 1 for i in range(64)}
    acc = 0
    for i in range(n):
        a = (i * 5 + 2) & 31
        b = (a ^ i) & 15
        acc += d.get(d.get(b, 0), -1) + a - b + (a & 3)
    return acc


def nested_calls_reference(n):
    acc = 0
    for i in range(n):
        a = (i * 5 + 2) & 31
        b = (a ^ i) & 15
        acc += b + 2 + a - b + (a & 3)
    return acc


class CompiledLoopTests(unittest.TestCase):
    def test_pop_in_a_loop_with_integer_work(self):
        self.assertEqual(pop_loop(HOT), pop_loop_reference(HOT))
        self.assertEqual(pop_loop(COLD), pop_loop_reference(COLD))

    def test_argument_overflows_between_load_and_call(self):
        for scale in (1, 3):
            self.assertEqual(
                overflow_in_argument(HOT, scale),
                overflow_in_argument_reference(HOT, scale),
            )

    def test_argument_raises_between_load_and_call(self):
        bad = HOT - 5000
        with self.assertRaises(ZeroDivisionError):
            division_in_argument(HOT, bad)
        self.assertEqual(
            division_in_argument(bad, bad),
            division_in_argument_reference(bad, bad),
        )
        self.assertEqual(
            division_in_argument(HOT, 10 * HOT),
            division_in_argument_reference(HOT, 10 * HOT),
        )

    def test_one_site_many_receivers(self):
        four = ({i: i for i in range(16)}, Doubler(), {i: -i for i in range(8)}, {})
        self.assertEqual(changing_receivers(HOT, four), changing_receivers_reference(HOT))
        four = ([1, 2, 3, 2], (2, 2, 5), [7], (1,))
        self.assertEqual(changing_kinds(HOT, four), changing_kinds_reference(HOT, four))

    def test_a_call_inside_a_call(self):
        self.assertEqual(nested_calls(HOT), nested_calls_reference(HOT))


class OtherReceiverTests(unittest.TestCase):
    def test_str_methods(self):
        def run(n):
            words = ["Alpha", " beta ", "GAMMA", "delta\n", "x=1"]
            total = 0
            for i in range(n):
                w = words[i % 5]
                if w.startswith("A") or w.endswith("\n"):
                    total += 1
                total += len(w.strip()) + len(w.lower()) + w.find("a") + len(w.partition("=")[0])
                total += w.upper().count("A")
            return [total]

        hot = run(HOT)
        cold = run(COLD)
        self.assertEqual(hot[0], cold[0] * (HOT // COLD))

    def test_list_methods(self):
        def run(n):
            xs = []
            total = 0
            for i in range(n):
                xs.append(i)
                xs.insert(0, -i)
                total += xs.pop() + xs.pop(0)
                if i % 100 == 0:
                    xs.extend((i, i + 1))
                    total += xs.index(i) + xs.count(i + 1)
                    xs.reverse()
                    ys = xs.copy()
                    ys.clear()
                    xs.remove(i)
                    total += xs.pop()
            return [total, len(xs)]

        hot, cold = run(HOT), run(COLD)
        self.assertEqual(hot[1], 0)
        self.assertEqual(cold[1], 0)
        self.assertEqual(hot[0], sum(i + 2 for i in range(0, HOT, 100)))

    def test_set_methods(self):
        def run(n):
            s = set()
            total = 0
            for i in range(n):
                s.add(i % 97)
                s.discard((i * 7) % 97)
                if i % 97 in s:
                    total += 1
                if i % 1000 == 0:
                    total += len(s.copy())
                    s.update((1, 2, 3))
                    s.remove(1)
                    total += s.pop() >= 0
            return [total, len(s) <= 97]

        hot, cold = run(HOT), run(COLD)
        self.assertEqual(hot, run(HOT))
        self.assertTrue(hot[1] and cold[1])

    def test_bytes_tuple_int_float(self):
        def run(n):
            b = b"key=value"
            t = (1, 2, 3, 2)
            total = 0
            for i in range(n):
                total += b.find(b"=") + t.count(2) + t.index(3)
                total += (i).bit_length() + (i + 0.5).is_integer() + i.to_bytes(4, "big")[3]
            return [total]

        hot = run(HOT)
        want = sum(3 + 2 + 2 + i.bit_length() + (i & 255) for i in range(HOT))
        self.assertEqual(hot[0], want)

    def test_bound_method_kept_in_a_local(self):
        def run(n):
            d = {i: i for i in range(n)}
            pop = d.pop
            get = d.get
            total = 0
            for i in range(n):
                total += get(i, 0) + pop(i)
                m = d.get
                total += m(i, 1)
            return [total, len(d), pop.__self__ is d, pop.__name__]

        hot = run(HOT)
        self.assertEqual(hot, [HOT * (HOT - 1) + HOT, 0, True, "pop"])

    def test_keyword_and_wide_calls(self):
        def run(n):
            total = 0
            xs = list(range(10))
            for i in range(n):
                total += int.from_bytes(i.to_bytes(4, byteorder="little"), byteorder="little")
                total += len("a,b,c,d".split(",", maxsplit=i % 3))
                total += len(sorted(xs, key=None, reverse=bool(i & 1)))
                total += "x".join(("a", "b")).count("x")
            return [total]

        hot = run(HOT)
        want = sum(i + i % 3 + 1 + 10 + 1 for i in range(HOT))
        self.assertEqual(hot[0], want)

    def test_tracing_sees_the_same_calls(self):
        def run(n):
            d = {i: i for i in range(n)}
            total = 0
            for i in range(n):
                total += d.pop(i)
            return total

        run(HOT)
        events = []

        def tracer(frame, event, arg):
            if event == "c_call":
                events.append(arg.__name__)

        sys.setprofile(tracer)
        try:
            got = run(300)
        finally:
            sys.setprofile(None)
        self.assertEqual(got, 300 * 299 // 2)
        self.assertEqual(events.count("pop"), 300)


if __name__ == "__main__":
    unittest.main()
