"""The profile-guided order file's three parts, and its name matching."""

import importlib.util
from pathlib import Path
import unittest

TOOL = Path(__file__).resolve().parents[2] / "tools" / "pgo" / "order.py"
spec = importlib.util.spec_from_file_location("pgo_order", TOOL)
order = importlib.util.module_from_spec(spec)
spec.loader.exec_module(order)


def ran(**counts):
    return {order.key(name): (name, count) for name, count in counts.items()}


class LayoutTests(unittest.TestCase):
    def test_key_masks_crate_hashes_and_promotions(self):
        a = "__RNvNtCs9p2CpBq5ts3_10weavepy_vm9proc_init16snapshot_std_fds"
        b = "_RNvNtCsaKrnU3FAIWq_10weavepy_vm9proc_init16snapshot_std_fds.llvm.123"
        self.assertEqual(order.key(a), order.key(b))

    def test_steady_counts_are_the_long_run_less_the_short(self):
        short = ran(startup=5000, loop=1000, compile=70000)
        long = ran(startup=5000, loop=9000, compile=70000, late=4)
        self.assertEqual(order.steady(short, long), {"loop": 8000, "late": 4})

    def test_hot_group_is_what_programs_keep_running(self):
        merged = ran(startup=900000, shared=50000, a_own=7000, b_own=6000, c_own=5000, rare=3)
        programs = [
            {"shared": 9000, "a_own": 7000, "startup": 0, "rare": 3},
            {"shared": 9000, "b_own": 6000},
            {"shared": 8000, "c_own": 5000, "b_own": 2000},
        ]
        lines, counts = order.layout(merged, programs, ["_startup", "_elsewhere"], 1000)
        self.assertEqual(lines, [
            "_shared", "_b_own", "_a_own", "_c_own",
            "_startup", "_rare", "_elsewhere",
        ])
        self.assertEqual(counts, (4, 2, 1))

    def test_the_rest_goes_by_how_many_programs_ran_it(self):
        merged = ran(imports=40, a_setup=900, b_setup=30, tests_only=5000)
        used = [
            {"imports", "a_setup"},
            {"imports", "b_setup"},
            {"imports"},
        ]
        lines, _ = order.layout(merged, [{}, {}, {}], [], 1000, used)
        self.assertEqual(lines, ["_imports", "_a_setup", "_b_setup", "_tests_only"])

    def test_merged_counts_stand_in_without_runs(self):
        lines, _ = order.layout(ran(a=10, b=5000), [], [], 1000)
        self.assertEqual(lines, ["_b", "_a"])

    def test_markers_go_around_the_hot_group(self):
        lines = ["_shared", "_own", "_startup", "_rare"]
        self.assertEqual(order.marked(lines, 2), [
            order.STEADY_START, "_shared", "_own", order.STEADY_END,
            "_startup", "_rare",
        ])
        self.assertEqual(order.marked(lines, 0), lines)

    def test_base_entries_keep_their_order_and_spelling(self):
        merged = ran(_RNvCs1abc_3foo3bar=2, late=9)
        lines, _ = order.layout(merged, [], ["__RNvCs9xyz_3foo3bar", "_gone"], 1000)
        self.assertEqual(lines, ["__RNvCs9xyz_3foo3bar", "_late", "_gone"])


if __name__ == "__main__":
    unittest.main()
