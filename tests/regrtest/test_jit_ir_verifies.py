"""Compiled plans and frames pass Cranelift's IR verifier.

Release builds skip the verifier, and some invalid IR (an extension to
the type a value already has) still compiles on x86-64 but encodes an
invalid instruction on AArch64. This runs the plan-cache tests, which
exercise the in-line method, class-attribute and global caches, with
both verifiers on, and fails if any function didn't compile.
"""

import os
import subprocess
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))


class IrVerifies(unittest.TestCase):
    def test_plan_caches_compile_verified(self):
        script = os.path.join(HERE, "test_jit_plan_caches.py")
        if not os.path.exists(script):
            self.skipTest("test_jit_plan_caches.py isn't beside this test")
        env = dict(os.environ)
        env["WEAVEPY_PLAN_VERIFY"] = "1"
        env["WEAVEPY_FRAME_JIT_VERIFY"] = "1"
        proc = subprocess.run(
            [sys.executable, script],
            capture_output=True,
            text=True,
            env=env,
            timeout=120,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr[-3000:])
        self.assertNotIn("failed to compile", proc.stderr, proc.stderr[-3000:])
        self.assertNotIn("verifier error", proc.stderr, proc.stderr[-3000:])


if __name__ == "__main__":
    unittest.main()
