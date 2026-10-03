"""An interrupt reaches code that only calls and catches exceptions.

Recursion retried from its own `RecursionError` handler never returns
normally and has no loop, so the interpreter must still give up the GIL
and look for signals when it catches an exception (gh-102056's shape,
from `test_threading`).
"""

import subprocess
import sys
import unittest

SCRIPT = r"""
import time
import threading
import _thread

def f():
    try:
        f()
    except RecursionError:
        f()

def h():
    time.sleep(0.5)
    _thread.interrupt_main()

t = threading.Thread(target=h)
t.start()
try:
    f()
except KeyboardInterrupt:
    print("interrupted")
t.join()
"""


class InterruptWhileCatching(unittest.TestCase):
    def test_interrupt_main_lands(self):
        proc = subprocess.run(
            [sys.executable, "-c", SCRIPT],
            capture_output=True,
            text=True,
            timeout=45,
        )
        self.assertIn("interrupted", proc.stdout, proc.stderr[-2000:])


if __name__ == "__main__":
    unittest.main()
