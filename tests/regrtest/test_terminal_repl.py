"""The prompt on a terminal, driven through a pseudo-terminal.

A terminal gets `_pyrepl` unless `PYTHON_BASIC_REPL` or the terminal says
otherwise, and in every mode the lines typed ahead of the prompt must wait for
it: the whole script arrives before the interpreter has even started.
"""

import os
import select
import subprocess
import sys
import tempfile
import unittest

try:
    import pty
except ImportError:
    pty = None

TIMEOUT = 60.0
PROBE = (
    "import sys\n"
    "print('pyrepl' if hasattr(sys.modules['__main__'], '__file__') else 'basic')\n"
    "print(6 * 7)\n"
    "exit()\n"
)


@unittest.skipIf(pty is None or sys.platform == "win32", "needs a pseudo-terminal")
class TerminalReplTests(unittest.TestCase):
    def run_repl(self, script, extra_env=None, args=("-i", "-u", "-I")):
        env = {k: v for k, v in os.environ.items() if k != "PYTHON_BASIC_REPL"}
        env["TERM"] = "xterm"
        env["NO_COLOR"] = "1"
        env.update(extra_env or {})
        master, slave = pty.openpty()
        with tempfile.TemporaryDirectory() as cwd:
            env["PYTHON_HISTORY"] = os.path.join(cwd, ".history")
            process = subprocess.Popen(
                [sys.executable, *args],
                stdin=slave, stdout=slave, stderr=slave,
                cwd=cwd, env=env, close_fds=True,
            )
            os.close(slave)
            os.write(master, script.encode())
            output = []
            while select.select([master], [], [], TIMEOUT)[0]:
                try:
                    data = os.read(master, 4096)
                except OSError:
                    break
                if not data:
                    break
                output.append(data)
            else:
                process.kill()
                process.wait()
                os.close(master)
                self.fail(f"no prompt answered: {b''.join(output)!r}")
            os.close(master)
            return b"".join(output).decode(errors="replace"), process.wait(TIMEOUT)

    def test_pyrepl_is_the_default(self):
        output, code = self.run_repl(PROBE)
        self.assertEqual(code, 0, output)
        self.assertIn("pyrepl\r\n", output)
        self.assertIn("42\r\n", output)

    def test_basic_repl_keeps_typed_ahead_lines(self):
        output, code = self.run_repl(PROBE, {"PYTHON_BASIC_REPL": "1"}, ("-i", "-u"))
        self.assertEqual(code, 0, output)
        self.assertIn("basic\r\n", output)
        self.assertIn("42\r\n", output)

    def test_ignored_environment_ignores_the_basic_request(self):
        output, code = self.run_repl(PROBE, {"PYTHON_BASIC_REPL": "1"}, ("-i", "-u", "-E"))
        self.assertEqual(code, 0, output)
        self.assertIn("pyrepl\r\n", output)

    def test_dumb_terminal_falls_back_with_a_warning(self):
        output, code = self.run_repl(PROBE, {"TERM": "dumb"})
        self.assertEqual(code, 0, output)
        self.assertIn("warning: can't use pyrepl", output)
        self.assertIn("basic\r\n", output)
        self.assertNotIn("Traceback", output)

    def test_exit_code_and_clean_globals(self):
        output, code = self.run_repl(
            "print(sorted(k for k in globals() if not k.startswith('__')))\n"
            "raise SystemExit(3)\n"
        )
        self.assertEqual(code, 3, output)
        self.assertIn("[]\r\n", output)


if __name__ == "__main__":
    unittest.main()
