#!/usr/bin/env python3
"""Populate an offline wheel cache for the ecosystem conformance lane.

Reads every requirement string out of tests/ecosystem/manifest.toml and
downloads the wheels (plus transitive deps and pip/setuptools for venv
seeding) into --dest using the host CPython's pip. The harness then runs
fully offline:

    python3 tools/ecosystem_fetch.py --dest target/ecosystem-wheels
    cargo run -p weavepy-conformance -- ecosystem --wheels target/ecosystem-wheels
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

try:
    import tomllib  # 3.11+
except ModuleNotFoundError:
    tomllib = None

WORKSPACE = Path(__file__).resolve().parent.parent
MANIFEST = WORKSPACE / "tests" / "ecosystem" / "manifest.toml"

# venv seeding needs these even though no manifest row lists them
BOOTSTRAP = ["pip", "setuptools", "wheel"]


def load_requirement_strings(manifest_path: Path):
    """Every `requirements = "..."` value in the manifest."""
    if tomllib is not None:
        with open(manifest_path, "rb") as f:
            manifest = tomllib.load(f)
        return [row["requirements"] for row in manifest.get("packages", {}).values()]
    # Pre-3.11 fallback: the manifest sticks to a flat
    # `requirements = "…"` dialect, so a line regex is exact.
    text = manifest_path.read_text()
    return re.findall(r'^\s*requirements\s*=\s*"([^"]*)"', text, re.MULTILINE)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--dest", type=Path, required=True, help="wheel cache directory")
    ap.add_argument("--manifest", type=Path, default=MANIFEST)
    args = ap.parse_args()

    requirements = list(BOOTSTRAP)
    for reqs in load_requirement_strings(args.manifest):
        requirements.extend(reqs.split())

    args.dest.mkdir(parents=True, exist_ok=True)
    # Target WeavePy's compatibility surface (cp313), not the host
    # interpreter — otherwise a cp39 host fetches cp39 binary wheels
    # (charset_normalizer, markupsafe) that WeavePy's pip then rejects.
    # Cross-version download requires --only-binary, which is what the
    # offline lane wants anyway.
    import platform

    machine = platform.machine()
    if sys.platform == "darwin":
        plats = [f"macosx_11_0_{machine}", "macosx_10_9_universal2"]
    elif sys.platform.startswith("linux"):
        plats = [f"manylinux2014_{machine}", f"manylinux_2_17_{machine}"]
    else:
        plats = []
    cmd = [
        sys.executable,
        "-m",
        "pip",
        "download",
        "--dest",
        str(args.dest),
        "--only-binary",
        ":all:",
        "--implementation",
        "cp",
        "--python-version",
        "3.13",
        *[a for p in plats for a in ("--platform", p)],
        "--platform",
        "any",
        *sorted(set(requirements)),
    ]
    print("+", " ".join(cmd))
    return subprocess.run(cmd).returncode


if __name__ == "__main__":
    raise SystemExit(main())
