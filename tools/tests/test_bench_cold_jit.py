"""Regression coverage for the portable cold-JIT diagnostic."""
import contextlib
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import bench_cold_jit


class ColdJitDiagnosticTests(unittest.TestCase):
    def test_binary_identity_includes_adjacent_runtime_dll(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "weavepy.exe"
            binary.write_bytes(b"shim")
            runtime = binary.with_name("python314.dll")
            runtime.write_bytes(b"runtime one")
            with mock.patch.object(bench_cold_jit.subprocess, "check_output", return_value="test version\n"):
                first = bench_cold_jit.binary_record(str(binary))
                runtime.write_bytes(b"runtime two")
                second = bench_cold_jit.binary_record(str(binary))
            self.assertEqual(first["version"], "test version")
            self.assertEqual(first["files"][str(binary.resolve())], second["files"][str(binary.resolve())])
            self.assertNotEqual(first["files"][str(runtime.resolve())], second["files"][str(runtime.resolve())])

    def test_driver_executes_both_calls_and_marks_trace_phases(self):
        with tempfile.TemporaryDirectory() as directory:
            fixture = Path(directory) / "fixture.py"
            fixture.write_text("calls = 0\ndef bench(n):\n    global calls\n    calls += 1\n    assert n == 7\n    assert calls <= 2\n")
            stdout, stderr = io.StringIO(), io.StringIO()
            scope = {}
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                exec(bench_cold_jit.driver(fixture, 7, True), scope)
            self.assertEqual(scope["scope"]["calls"], 2)
            self.assertRegex(stdout.getvalue(), r"WEAVEPY_BENCH_NS=\d+")
            self.assertEqual(stderr.getvalue().splitlines(), [
                "JIT_DIAGNOSTIC_FIRST_BEGIN", "JIT_DIAGNOSTIC_FIRST_END",
                "JIT_DIAGNOSTIC_SECOND_BEGIN", "JIT_DIAGNOSTIC_SECOND_END",
            ])

    def test_child_failure_preserves_its_error(self):
        result = subprocess.CompletedProcess([], 3, "partial output", "fixture failed")
        with mock.patch.object(bench_cold_jit.subprocess, "run", return_value=result):
            with self.assertRaisesRegex(RuntimeError, "fixture failed"):
                bench_cold_jit.run("weavepy", Path("fixture.py"), 7, Path("cache"), False)

    def test_timed_runs_remove_inherited_instrumentation(self):
        result = subprocess.CompletedProcess([], 0, "WEAVEPY_BENCH_NS=23\n", "")
        with mock.patch.dict("os.environ", {"WEAVEPY_JIT": "0", "WEAVEPY_JIT_TRACE": "1", "WEAVEPY_VM_STATS": "1"}):
            with mock.patch.object(bench_cold_jit.subprocess, "run", return_value=result) as run:
                sample, _, _ = bench_cold_jit.run("weavepy", Path("fixture.py"), 7, Path("cache/base"), False)
        env = run.call_args.kwargs["env"]
        self.assertEqual(env["WEAVEPY_JIT"], "1")
        self.assertEqual(env["WEAVEPY_BENCH_WORK"], "7")
        self.assertNotIn("WEAVEPY_JIT_TRACE", env)
        self.assertNotIn("WEAVEPY_VM_STATS", env)
        self.assertEqual(sample["ns"], 23)

    def test_empty_warmed_cache_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "sumvm.py").write_text("fixture")
            argv = ["bench_cold_jit", "--base", "base", "--new", "new", "--python", "cpython",
                    "--fixture-root", str(root), "--samples", "1", "--out", str(root / "out/report.json")]
            with mock.patch.object(sys, "argv", argv):
                with mock.patch.object(bench_cold_jit, "binary_record", side_effect=lambda name: {"path": name, "files": {}}):
                    with mock.patch.object(bench_cold_jit, "run", return_value=({"ns": 1, "wall_ns": 2}, "", "")):
                        with self.assertRaisesRegex(RuntimeError, "empty warmed frozen cache"):
                            bench_cold_jit.main()


if __name__ == "__main__":
    unittest.main()
