#!/usr/bin/env python3
"""Reject tracked files that violate the repository's .gitignore rules."""

from pathlib import Path
import subprocess
import sys


def ignored_tracked_files(root: Path) -> list[str]:
    # Use only repository rules, not personal global ignores or .git/info/exclude.
    # Checking the index also catches existing files and files added with -f.
    result = subprocess.run(
        [
            "git", "ls-files", "--cached", "--ignored",
            "--exclude-per-directory=.gitignore", "-z",
        ],
        cwd=root,
        check=True,
        stdout=subprocess.PIPE,
    )
    return sorted({name.decode("utf-8", errors="surrogateescape")
                   for name in result.stdout.split(b"\0") if name})


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    paths = ignored_tracked_files(root)
    if not paths:
        print("Repository artifact check passed.")
        return 0
    print("Tracked files violate repository ignore rules:", file=sys.stderr)
    for path in paths:
        print(f"  {path!r}", file=sys.stderr)
    print(
        "Store generated output under target/ or outside the repository. "
        "Use git rm --cached -- <path> to untrack a file while keeping it locally. "
        "Required source or fixture data needs a narrow, reviewed ignore exception.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
