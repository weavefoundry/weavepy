"""`for x in reversed(xs)` in a loop that runs long enough to be compiled.

A reversed list's iterator steps from the last item to the first. It
reads the list as it is at each step, so items appended meanwhile are
never visited, a list that shrinks ends the loop once the cursor is past
its end, and an exhausted iterator stays exhausted. The loop gives the
same results however many times it runs.
"""

import unittest

ROUNDS = 400


def total(xs):
    t = 0
    for x in reversed(xs):
        t += x
    return t


def totals(xs, rounds):
    acc = 0
    for _ in range(rounds):
        t = 0
        for x in reversed(xs):
            t += x
        acc += t
    return acc


def float_total(xs, rounds):
    acc = 0.0
    for _ in range(rounds):
        for x in reversed(xs):
            acc += x * 0.5
    return acc


def joined(xs, rounds):
    out = None
    for _ in range(rounds):
        parts = []
        for x in reversed(xs):
            parts.append(x)
        out = parts
    return out


def mixed(xs, rounds):
    ints = 0
    others = 0
    for _ in range(rounds):
        for x in reversed(xs):
            if type(x) is int:
                ints += x
            else:
                others += 1
    return ints, others


def first_match(xs, want, rounds):
    found = -1
    for _ in range(rounds):
        i = len(xs)
        for x in reversed(xs):
            i -= 1
            if x == want:
                found = i
                break
    return found


def shrinking(n, rounds):
    seen = []
    for _ in range(rounds):
        xs = list(range(n))
        got = 0
        for x in reversed(xs):
            got += 1
            if x == n - 2:
                del xs[2:]
        seen.append(got)
    return seen


def growing(n, rounds):
    last = None
    for _ in range(rounds):
        xs = list(range(n))
        visited = []
        for x in reversed(xs):
            visited.append(x)
            if len(xs) < n + 5:
                xs.append(100 + x)
        last = (visited, len(xs))
    return last


class ReversedLoopTests(unittest.TestCase):
    def test_sums(self):
        xs = list(range(1000))
        self.assertEqual(total(xs), 499500)
        self.assertEqual(totals(xs, ROUNDS), 499500 * ROUNDS)
        self.assertEqual(totals([], ROUNDS), 0)
        self.assertEqual(totals([5], ROUNDS), 5 * ROUNDS)
        self.assertEqual(float_total([1.5, 2.5, 4.0], ROUNDS * 10), 4.0 * ROUNDS * 10)

    def test_order(self):
        xs = ["a", "b", "c", "d"]
        self.assertEqual(joined(xs, ROUNDS * 10), ["d", "c", "b", "a"])
        objs = [object() for _ in range(50)]
        out = joined(objs, ROUNDS)
        self.assertTrue(all(a is b for a, b in zip(out, objs[::-1])))

    def test_mixed_items(self):
        xs = [1, "a", 2, None, 3.5, 4, (5,), 10**30, True, 7]
        rounds = ROUNDS * 5
        self.assertEqual(mixed(xs, rounds), ((1 + 2 + 4 + 7 + 10**30) * rounds, 5 * rounds))

    def test_break(self):
        xs = list(range(200))
        self.assertEqual(first_match(xs, 150, ROUNDS * 5), 150)
        self.assertEqual(first_match(xs, -1, ROUNDS), -1)

    def test_a_list_that_shrinks_ends_the_loop(self):
        # The cursor is at index n - 3 when the list drops to two items:
        # past the end, so the loop is over.
        self.assertEqual(shrinking(50, ROUNDS * 5), [2] * (ROUNDS * 5))

    def test_appended_items_are_not_visited(self):
        visited, size = growing(20, ROUNDS * 5)
        self.assertEqual(visited, list(range(19, -1, -1)))
        self.assertEqual(size, 25)

    def test_an_exhausted_iterator_stays_exhausted(self):
        for _ in range(ROUNDS):
            xs = [1, 2, 3]
            it = reversed(xs)
            got = []
            for x in it:
                got.append(x)
            xs.extend([4, 5, 6])
            for x in it:
                got.append(x)
            self.assertEqual(got, [3, 2, 1])
            self.assertEqual(list(it), [])

    def test_a_partly_used_iterator(self):
        acc = 0
        for _ in range(ROUNDS * 5):
            it = reversed([1, 2, 3, 4, 5])
            next(it)
            next(it)
            for x in it:
                acc += x
        self.assertEqual(acc, 6 * ROUNDS * 5)

    def test_reversed_of_other_sequences_in_the_same_loop(self):
        def run(seqs, rounds):
            acc = 0
            for _ in range(rounds):
                for seq in seqs:
                    for x in reversed(seq):
                        acc += x
            return acc

        seqs = [[1, 2, 3], (4, 5, 6), range(7, 10), [10], bytearray(b"\x01\x02"), {1: 0, 2: 0}]
        self.assertEqual(run(seqs, ROUNDS * 3), (55 + 3 + 3) * ROUNDS * 3)

    def test_copies_of_a_reversed_list(self):
        xs = list(range(300))
        for _ in range(ROUNDS):
            self.assertEqual(list(reversed(xs)), xs[::-1])
            self.assertEqual(tuple(reversed(xs)), tuple(xs[::-1]))
            self.assertEqual(sum(reversed(xs)), 44850)
            self.assertEqual(max(reversed(xs)), 299)


if __name__ == "__main__":
    unittest.main()
