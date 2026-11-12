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

    def test_hot_group_is_what_one_process_ran_often(self):
        merged = ran(startup=5000, loop=900000, helper=4000, rare=3)
        runs = [ran(startup=5, loop=450000, helper=2000), ran(startup=5, loop=450000, rare=3)]
        lines, counts = order.layout(merged, runs, ["_startup", "_elsewhere"], 1000)
        self.assertEqual(lines, [
            order.HOT_START, "_loop", "_helper", order.HOT_END,
            "_startup", "_rare", "_elsewhere",
        ])
        self.assertEqual(counts, (2, 2, 1))

    def test_merged_counts_stand_in_without_runs(self):
        lines, _ = order.layout(ran(a=10, b=5000), [], [], 1000)
        self.assertEqual(lines, [order.HOT_START, "_b", order.HOT_END, "_a"])

    def test_base_markers_are_dropped(self):
        lines, _ = order.layout(ran(a=1), [], [order.HOT_START, "_a", order.HOT_END], 1000)
        self.assertEqual(lines, [order.HOT_START, order.HOT_END, "_a"])


if __name__ == "__main__":
    unittest.main()
