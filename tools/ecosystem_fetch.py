#!/usr/bin/env python3
"""Populate an offline wheel cache for the ecosystem conformance lane.

Reads every requirement string out of tests/ecosystem/manifest.toml —
including selftest test-dep requirements (RFC 0062 WS4) — and downloads
the wheels (plus transitive deps and pip/setuptools for venv seeding)
into --dest using the host CPython's pip. Rows with `no_binary` and
`[packages.X.selftest]` tables additionally need *sdists* (the source
tarballs pip compiles / the suites run from), which are fetched with
`pip download --no-binary=:all: --no-deps`. The harness then runs fully
offline:

    python3 tools/ecosystem_fetch.py --dest target/ecosystem-wheels
    cargo run -p weavepy-conformance -- ecosystem --wheels target/ecosystem-wheels --selftests
"""

import argparse
import json
import re
import subprocess
import sys
import urllib.request
from pathlib import Path

try:
    import tomllib  # 3.11+
except ModuleNotFoundError:
    tomllib = None

WORKSPACE = Path(__file__).resolve().parent.parent
MANIFEST = WORKSPACE / "tests" / "ecosystem" / "manifest.toml"

# venv seeding needs these even though no manifest row lists them
BOOTSTRAP = ["pip", "setuptools", "wheel"]


def _requirement_name(spec):
    """Base project name of a pip requirement string."""
    return re.split(r"[<>=!~\[;( ]", spec, maxsplit=1)[0].strip()


def _normalize(name):
    """PEP 503 normalization (same rule as the in-tree pip)."""
    return re.sub(r"[-_.]+", "-", name).lower()


def load_manifest(manifest_path: Path):
    """Return (wheel_req_groups, sdist_requirements) from the manifest.

    wheel_req_groups — one requirement list per manifest row (the row's
    `requirements` plus its selftest `requirements`, which install into
    the same venv). Each group is resolved by a *separate* `pip
    download` invocation: rows are mutually independent (each gets its
    own venv), and different rows deliberately pin conflicting versions
    of shared test deps (pytest==8.3.5 vs ==8.4.2), which a single
    global resolve rejects as impossible.
    sdist_requirements — the pinned requirement of each `no_binary`
    package and each selftest `source` (the harness needs the tarballs).
    """
    wheel_groups = []
    sdist_reqs = []
    if tomllib is not None:
        with open(manifest_path, "rb") as f:
            manifest = tomllib.load(f)
        for row in manifest.get("packages", {}).values():
            requirements = row.get("requirements", "").split()
            group = list(requirements)
            for pkg in re.split(r"[,\s]+", row.get("no_binary", "")):
                if not pkg:
                    continue
                pinned = next(
                    r
                    for r in requirements
                    if _normalize(_requirement_name(r)) == _normalize(pkg)
                )
                sdist_reqs.append(pinned)
            selftest = row.get("selftest")
            if selftest:
                group.extend(selftest.get("requirements", "").split())
                sdist_reqs.append(selftest["source"])
            if group:
                wheel_groups.append(group)
        return wheel_groups, sdist_reqs
    # Pre-3.11 fallback: the manifest sticks to a flat quoted-string
    # dialect, so line regexes are exact. Each `requirements` line
    # becomes its own download group (row and selftest deps land in
    # separate groups, which is fine — the cache is a union); sdists
    # come from `source` lines plus any `==`-pinned requirement
    # matching a `no_binary` name.
    text = manifest_path.read_text()
    for reqs in re.findall(r'^\s*requirements\s*=\s*"([^"]*)"', text, re.MULTILINE):
        if reqs.split():
            wheel_groups.append(reqs.split())
    sdist_reqs.extend(
        re.findall(r'^\s*source\s*=\s*"([^"]*)"', text, re.MULTILINE)
    )
    no_binary_names = set()
    for names in re.findall(r'^\s*no_binary\s*=\s*"([^"]*)"', text, re.MULTILINE):
        no_binary_names.update(
            _normalize(n) for n in re.split(r"[,\s]+", names) if n
        )
    sdist_reqs.extend(
        r
        for group in wheel_groups
        for r in group
        if "==" in r and _normalize(_requirement_name(r)) in no_binary_names
    )
    return wheel_groups, sdist_reqs


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--dest", type=Path, required=True, help="wheel cache directory")
    ap.add_argument("--manifest", type=Path, default=MANIFEST)
    args = ap.parse_args()

    wheel_groups, sdist_reqs = load_manifest(args.manifest)
    wheel_groups = [BOOTSTRAP] + wheel_groups

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
    seen = set()
    for group in wheel_groups:
        key = tuple(sorted(set(group)))
        if not key or key in seen:
            continue
        seen.add(key)
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
            *key,
        ]
        print("+", " ".join(cmd))
        rc = subprocess.run(cmd).returncode
        if rc != 0:
            return rc

    # Source tarballs for the no_binary / selftest lanes (RFC 0062),
    # fetched straight off the PyPI JSON API (the same route the harness
    # takes online). NOT `pip download --no-binary :all:`: that runs a
    # PEP 517 metadata build for the sdist, and --no-binary makes the
    # build backend's own deps (hatchling, …) resolve from source too —
    # a recursive source-build cascade that hangs for minutes-to-forever
    # on older host pips. The sdists here are `==`-pinned by
    # construction, so the exact file is a single JSON lookup away.
    for req in sorted(set(sdist_reqs)):
        name, _, version = req.partition("==")
        if not version:
            print(f"error: sdist requirement {req!r} is not ==-pinned", file=sys.stderr)
            return 1
        url = f"https://pypi.org/pypi/{name.strip()}/{version.strip()}/json"
        with urllib.request.urlopen(url) as resp:
            release = json.load(resp)
        sdist = next(
            (u for u in release["urls"] if u["packagetype"] == "sdist"), None
        )
        if sdist is None:
            print(f"error: no sdist on PyPI for {req}", file=sys.stderr)
            return 1
        target = args.dest / sdist["filename"]
        if target.exists():
            print(f". {sdist['filename']} (cached)")
            continue
        print(f"+ {sdist['url']}")
        with urllib.request.urlopen(sdist["url"]) as resp:
            target.write_bytes(resp.read())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
