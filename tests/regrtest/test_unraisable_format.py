"""The default unraisable report renders exceptions as `str()` does.

Exceptions built natively don't store a separate message, so the report
must derive it the way each class's `__str__` does: an `OSError` from a
failed system call reads `[Errno N] strerror`, not its bare errno.
"""

import errno
import os
import unittest
from test.support import captured_stderr


class UnraisableFormat(unittest.TestCase):
    def report(self, exc_source):
        class Holder:
            def __del__(self):
                exc_source()

        with captured_stderr() as err:
            Holder()
        return err.getvalue()

    def test_os_error_from_a_system_call(self):
        def close_bad():
            os.close(-1)

        out = self.report(close_bad)
        self.assertIn("Exception ignored", out)
        self.assertIn("OSError: [Errno %d] " % errno.EBADF, out)

    def test_os_error_with_a_filename(self):
        def open_missing():
            open("/nonexistent/weavepy-unraisable")

        out = self.report(open_missing)
        self.assertIn("FileNotFoundError: [Errno %d] " % errno.ENOENT, out)
        self.assertIn("'/nonexistent/weavepy-unraisable'", out)

    def test_key_error_and_plain_message(self):
        def key():
            {}["missing"]

        def value():
            raise ValueError("bad value")

        self.assertIn("KeyError: 'missing'", self.report(key))
        self.assertIn("ValueError: bad value", self.report(value))


if __name__ == "__main__":
    unittest.main()
