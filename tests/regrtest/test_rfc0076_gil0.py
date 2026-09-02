"""RFC 0076 WS11 — the `-X gil=0` free-threaded runtime mode.

Covers PEP 703's user-visible switches: `sys._is_gil_enabled()`
truthfulness, `-X gil` / `PYTHON_GIL` precedence and validation, real
multi-thread execution under the mode, and (when the C fixture is
available) the `Py_mod_gil` extension-import contract — a
non-declaring extension re-enables the GIL with a RuntimeWarning
unless `PYTHON_GIL=0` forced the mode.

The mode is process-wide and decided at startup, so every case runs
in a subprocess.
"""

import os
import subprocess
import sys
import textwrap
import unittest


def run_weavepy(code, *, xgil=None, env_gil=None):
    """Run *code* in a fresh interpreter with the given gil settings."""
    argv = [sys.executable]
    if xgil is not None:
        argv += ["-X", f"gil={xgil}"]
    argv += ["-c", textwrap.dedent(code)]
    env = dict(os.environ)
    env.pop("PYTHON_GIL", None)
    if env_gil is not None:
        env["PYTHON_GIL"] = env_gil
    return subprocess.run(argv, capture_output=True, text=True, env=env)


class GilFlagSurfaceTests(unittest.TestCase):
    def assert_gil_enabled(self, proc, expected):
        self.assertEqual(
            proc.returncode, 0,
            f"stdout={proc.stdout!r} stderr={proc.stderr!r}")
        self.assertEqual(proc.stdout.strip(), str(expected))

    INTROSPECT = "import sys; print(sys._is_gil_enabled())"

    def test_default_gil_on(self):
        self.assert_gil_enabled(run_weavepy(self.INTROSPECT), True)

    def test_xoption_disables(self):
        self.assert_gil_enabled(run_weavepy(self.INTROSPECT, xgil="0"), False)

    def test_env_disables(self):
        self.assert_gil_enabled(
            run_weavepy(self.INTROSPECT, env_gil="0"), False)

    def test_xoption_one_keeps_gil(self):
        self.assert_gil_enabled(run_weavepy(self.INTROSPECT, xgil="1"), True)

    def test_xoption_beats_env(self):
        # -X gil=1 wins over PYTHON_GIL=0 (CPython config_read_gil).
        self.assert_gil_enabled(
            run_weavepy(self.INTROSPECT, xgil="1", env_gil="0"), True)

    def test_invalid_value_is_fatal(self):
        proc = run_weavepy("pass", xgil="2")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('must be "0" or "1"', proc.stderr)
        proc = run_weavepy("pass", env_gil="yes")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn('must be "0" or "1"', proc.stderr)

    def test_xoptions_carries_gate(self):
        proc = run_weavepy(
            "import sys; print(sys._xoptions.get('gil'))", xgil="0")
        self.assertEqual(proc.stdout.strip(), "0")


class FreeThreadedExecutionTests(unittest.TestCase):
    """Real threaded workloads under the mode (subprocess with -X gil=0)."""

    def check(self, body):
        proc = run_weavepy(body, xgil="0")
        self.assertEqual(
            proc.returncode, 0,
            f"stdout={proc.stdout!r} stderr={proc.stderr!r}")
        return proc

    def test_threads_run_and_join(self):
        self.check("""
            import sys, threading
            assert not sys._is_gil_enabled()
            results = []
            lock = threading.Lock()
            def work(n):
                acc = sum(range(n))
                with lock:
                    results.append(acc)
            ts = [threading.Thread(target=work, args=(10000,))
                  for _ in range(4)]
            for t in ts: t.start()
            for t in ts: t.join()
            assert results == [49995000] * 4, results
        """)

    def test_locked_shared_mutation(self):
        self.check("""
            import threading
            lock = threading.Lock()
            counter = 0
            def bump():
                global counter
                for _ in range(5000):
                    with lock:
                        counter += 1
            ts = [threading.Thread(target=bump) for _ in range(4)]
            for t in ts: t.start()
            for t in ts: t.join()
            assert counter == 20000, counter
        """)

    def test_queue_producer_consumer(self):
        self.check("""
            import queue, threading
            q = queue.Queue()
            got = []
            def cons():
                while True:
                    v = q.get()
                    if v is None:
                        return
                    got.append(v)
            c = threading.Thread(target=cons)
            c.start()
            for i in range(2000):
                q.put(i)
            q.put(None)
            c.join()
            assert len(got) == 2000
        """)

    def test_thread_pool_executor(self):
        self.check("""
            from concurrent.futures import ThreadPoolExecutor
            with ThreadPoolExecutor(max_workers=4) as ex:
                rs = list(ex.map(lambda n: sum(range(n)), [100] * 20))
            assert all(r == 4950 for r in rs), rs
        """)


def extension_fixture_dir():
    """Directory containing the `_smalltest.so` single-phase C fixture
    built by weavepy-capi's build.rs, or None when unavailable."""
    exe_dir = os.path.dirname(os.path.abspath(sys.executable))
    build_dir = os.path.join(exe_dir, "build")
    if not os.path.isdir(build_dir):
        return None
    candidates = []
    for entry in os.listdir(build_dir):
        so = os.path.join(build_dir, entry, "out", "capi_ext",
                          "_smalltest.so")
        if os.path.isfile(so):
            candidates.append(os.path.dirname(so))
    if not candidates:
        return None
    # Newest artifact wins (parallel fingerprint dirs).
    return max(candidates, key=lambda d: os.path.getmtime(
        os.path.join(d, "_smalltest.so")))


class ExtensionContractTests(unittest.TestCase):
    """Py_mod_gil at import: a single-phase extension never declares
    free-threading support, so it re-enables the GIL under -X gil=0."""

    BODY = """
        import sys, warnings
        sys.path.insert(0, {fixture_dir!r})
        assert not sys._is_gil_enabled()
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            import _smalltest
        assert any("has not declared that it can run safely without"
                   in str(w.message) for w in caught), caught
        print(sys._is_gil_enabled())
    """

    def setUp(self):
        self.fixture_dir = extension_fixture_dir()
        if self.fixture_dir is None:
            self.skipTest("_smalltest.so fixture not built")

    def test_xoption_mode_reenables(self):
        proc = run_weavepy(
            self.BODY.format(fixture_dir=self.fixture_dir), xgil="0")
        self.assertEqual(
            proc.returncode, 0,
            f"stdout={proc.stdout!r} stderr={proc.stderr!r}")
        self.assertEqual(proc.stdout.strip(), "True")

    def test_env_forced_mode_stays_off(self):
        proc = run_weavepy(
            self.BODY.format(fixture_dir=self.fixture_dir), env_gil="0")
        self.assertEqual(
            proc.returncode, 0,
            f"stdout={proc.stdout!r} stderr={proc.stderr!r}")
        self.assertEqual(proc.stdout.strip(), "False")


if __name__ == "__main__":
    unittest.main()
