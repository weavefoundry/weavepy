"""Regression checks for paired benchmark reporting."""

import importlib.util
import statistics
import unittest
from pathlib import Path

TOOL = Path(__file__).resolve().parents[1] / "bench_compare.py"
spec = importlib.util.spec_from_file_location("bench_compare", TOOL)
bench_compare = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench_compare)


def samples(values):
    return [{"ns": value} for value in values]


class PairedRatioTests(unittest.TestCase):
    def test_constant_regression_survives_host_drift(self):
        before = [10, 20, 100, 80, 50]
        after = [value * 1.2 for value in before]
        self.assertAlmostEqual(
            bench_compare.paired_ratio(samples(after), samples(before), "ns"), 1.2
        )

    def test_marginal_medians_do_not_replace_matched_cycles(self):
        before = [15, 15, 11, 11, 10]
        after = [15, 15, 14, 10, 10]
        self.assertGreater(statistics.median(after) / statistics.median(before), 1.25)
        self.assertEqual(
            bench_compare.paired_ratio(samples(after), samples(before), "ns"), 1.0
        )

    def test_unpaired_samples_are_rejected(self):
        with self.assertRaises(ValueError):
            bench_compare.paired_ratio(samples([10]), samples([10, 20]), "ns")

    def test_workload_cpu_is_separate_from_wall_time(self):
        before = [{"ns": 100, "work_cpu_ns": 100}]
        after = [{"ns": 150, "work_cpu_ns": 90}]
        self.assertEqual(bench_compare.paired_ratio(after, before, "ns"), 1.5)
        self.assertEqual(bench_compare.paired_ratio(after, before, "work_cpu_ns"), 0.9)

    def test_time_win_does_not_hide_memory_or_cpu_regressions(self):
        before = [{"ns": 100, "wall_ns": 200, "cpu_ns": 150, "rss_bytes": 1000}]
        after = [{"ns": 50, "wall_ns": 220, "cpu_ns": 300, "rss_bytes": 2000}]
        self.assertEqual(
            bench_compare.relative_metrics(after, before),
            {"ns": 0.5, "wall_ns": 1.1, "cpu_ns": 2.0, "rss_bytes": 2.0},
        )

    def test_incomplete_measurements_are_rejected(self):
        with self.assertRaises(ValueError):
            bench_compare.relative_metrics([], [])
        with self.assertRaises(ValueError):
            bench_compare.relative_metrics([{"ns": 1}], [{"ns": 2, "cpu_ns": 2}])


if __name__ == "__main__":
    unittest.main()
