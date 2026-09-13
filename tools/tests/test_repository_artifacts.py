"""Exercise artifact policy against real Git indexes, including forced additions."""

import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "check_repository_artifacts", ROOT / "tools/check_repository_artifacts.py"
)
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


class RepositoryArtifactTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="weavepy-artifact-test-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.git("init", "--quiet")
        ignores = [".gitignore", "crates/weavepy-bench/census/.gitignore"]
        ignores += [str(path.relative_to(ROOT)) for path in
                    (ROOT / "crates/weavepy-bench/census").glob("*/.gitignore")]
        for name in ignores:
            self.write(name, (ROOT / name).read_text())

    def git(self, *args):
        return subprocess.run(
            ["git", *args], cwd=self.root, check=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )

    def write(self, name, contents="sample\n"):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents)
        return path

    def track(self, *names):
        for name in names:
            self.write(name)
        self.git("add", "--force", "--", *names)

    def test_forced_generated_files_are_rejected(self):
        names = [
            "target/report.json", "nested/target/debug/build-output",
            "tools/__pycache__/cached file.pyc", ".venv/pyvenv.cfg",
            "node_modules/package/index.js",
            "crates/weavepy-bench/census/2026-09-execution/suite.json",
            "crates/weavepy-bench/census/2026-09-runtime-metadata/new.patch",
            "crates/weavepy-bench/census/future-trial/evidence.tar.gz",
            "crates/weavepy-bench/census/future-trial/generated_report.md",
        ]
        self.track(*names)
        self.assertEqual(CHECK.ignored_tracked_files(self.root), sorted(names))

    def test_build_inputs_fixtures_and_selected_tools_are_allowed(self):
        names = [
            "crates/weavepy-bench/baselines/bench-macos-aarch64.json",
            "crates/weavepy-vm/src/stdlib/ucd/names.bin",
            "crates/weavepy-vm/src/stdlib/python/_cjk_tables.py",
            "vendor/lzma-sys/.cargo_vcs_info.json",
            "vendor/expat-sys/src/lib.rs", "tests/fixtures/expected.json",
            "crates/weavepy-bench/census/2026-09-execution/probes.py",
            "crates/weavepy-bench/census/2026-09-runtime-metadata/tuple-capi-expectations.toml",
        ]
        self.track(*names)
        self.assertEqual(CHECK.ignored_tracked_files(self.root), [])

    def test_new_ignore_rule_applies_to_already_tracked_files(self):
        self.track("old-result.txt")
        with (self.root / ".gitignore").open("a") as output:
            output.write("\n/old-result.txt\n")
        self.assertEqual(CHECK.ignored_tracked_files(self.root), ["old-result.txt"])

    def test_required_fixture_can_have_a_narrow_exception(self):
        self.write("tests/fixtures/.gitignore", "!required.pyc\n")
        self.track("tests/fixtures/required.pyc", "tests/fixtures/accidental.pyc")
        self.assertEqual(CHECK.ignored_tracked_files(self.root),
                         ["tests/fixtures/accidental.pyc"])

    def test_personal_ignore_rules_do_not_affect_repository_policy(self):
        self.track("README.md", "local-preference.txt")
        personal = self.write("personal-ignore", "README.md\n")
        self.git("config", "core.excludesFile", str(personal))
        self.write(".git/info/exclude", "local-preference.txt\n")
        self.assertEqual(CHECK.ignored_tracked_files(self.root), [])


if __name__ == "__main__":
    unittest.main()
