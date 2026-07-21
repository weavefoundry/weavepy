"""Ecosystem probe: pytest — run a small fixture project through the
installed pytest and check the pass/fail accounting."""

import subprocess
import sys
import tempfile
import textwrap
from pathlib import Path

with tempfile.TemporaryDirectory() as tmp:
    proj = Path(tmp)
    (proj / "test_sample.py").write_text(
        textwrap.dedent(
            """
            import pytest

            @pytest.fixture
            def numbers():
                return [1, 2, 3]

            def test_sum(numbers):
                assert sum(numbers) == 6

            @pytest.mark.parametrize("n,sq", [(2, 4), (3, 9)])
            def test_square(n, sq):
                assert n * n == sq

            @pytest.mark.skip(reason="baseline skip")
            def test_skipped():
                assert False

            def test_raises():
                with pytest.raises(ZeroDivisionError):
                    1 / 0
            """
        )
    )
    out = subprocess.run(
        [sys.executable, "-m", "pytest", "-q", str(proj)],
        capture_output=True,
        text=True,
        timeout=300,
    )
    sys.stdout.write(out.stdout)
    sys.stderr.write(out.stderr)
    assert out.returncode == 0, f"pytest exited {out.returncode}"
    assert "4 passed" in out.stdout, out.stdout
    assert "1 skipped" in out.stdout, out.stdout

print("pytest ok")
