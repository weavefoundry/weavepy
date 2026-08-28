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

# The expectations spelling of this host (the harness's own mapping).
HOST_OS = {"win32": "windows", "darwin": "macos"}.get(sys.platform, "linux")


def skipped_rows(manifest_path: Path):
    """Row names whose baseline says `status_<host> = \"skip\"`.

    The harness never installs those rows on this platform (RFC 0063),
    and some of them are *unfetchable* here by construction — uvloop
    publishes no Windows wheels at all, so `pip download` for its group
    would fail the whole cache build on the windows runner.
    """
    expectations = manifest_path.parent / "expectations.toml"
    if not expectations.is_file():
        return set()
    text = expectations.read_text()
    if tomllib is not None:
        rows = tomllib.loads(text).get("packages", {})
        return {
            name
            for name, row in rows.items()
            if row.get(f"status_{HOST_OS}") == "skip"
        }
    return {
        m.group(1)
        for m in re.finditer(
            r'^\[packages\.([^\]]+)\][^\[]*?^status_'
            + HOST_OS
            + r'\s*=\s*"skip"',
            text,
            re.MULTILINE | re.DOTALL,
        )
    }


def _requirement_name(spec):
    """Base project name of a pip requirement string."""
    return re.split(r"[<>=!~\[;( ]", spec, maxsplit=1)[0].strip()


def _normalize(name):
    """PEP 503 normalization (same rule as the in-tree pip)."""
    return re.sub(r"[-_.]+", "-", name).lower()


def load_manifest(manifest_path: Path, skip=frozenset()):
    """Return (wheel_req_groups, sdist_requirements) from the manifest.

    Rows named in `skip` (platform-skipped per the baseline — see
    `skipped_rows`) contribute nothing to either list.

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
        for name, row in manifest.get("packages", {}).items():
            if name in skip:
                continue
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
                # `mode = "installed"` selftests (RFC 0066 WS5) run out of
                # the wheel already fetched via `requirements`; sdist
                # selftests — and installed-mode rows with an `overlay`
                # (RFC 0075 WS9: the sdist donates its test subtree) —
                # carry a `source` tarball to cache.
                if "source" in selftest:
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
    # Drop the [packages.<skipped>] sections (and their .selftest
    # sub-tables) before the line scans below.
    if skip:
        text = "".join(
            chunk
            for chunk in re.split(r"(?=^\[packages\.)", text, flags=re.MULTILINE)
            if not any(
                re.match(rf"\[packages\.{re.escape(name)}[.\]]", chunk)
                for name in skip
            )
        )
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

    skip = skipped_rows(args.manifest)
    if skip:
        print(f"skipping {HOST_OS}-skipped rows: {', '.join(sorted(skip))}")
    wheel_groups, sdist_reqs = load_manifest(args.manifest, skip)
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
        # Heavy-native wheels (scipy, matplotlib) tag against newer macOS
        # deployment targets; offer every major tag from 11.0 up so pip can
        # pick whatever the project ships.
        plats = [
            f"macosx_{v}_{machine}" for v in ("11_0", "12_0", "13_0", "14_0", "15_0")
        ] + ["macosx_10_9_universal2", "macosx_10_13_universal2"]
    elif sys.platform.startswith("linux"):
        # Heavy-native wheels moved past manylinux2014: numpy 2.3+ and
        # scipy 1.16+ ship manylinux_2_28 (and newer) only, so a
        # 2014/2_17-only ladder can't see them (`numpy==2.5.2` resolved
        # nothing on the ubuntu CI lane). Offer the glibc ladder up
        # through the runner's own floor; pip picks the newest tag the
        # project ships.
        plats = [f"manylinux2014_{machine}"] + [
            f"manylinux_{v}_{machine}"
            for v in ("2_17", "2_24", "2_27", "2_28", "2_31", "2_34", "2_35")
        ]
    elif sys.platform == "win32":
        # RFC 0063: the Windows CI lane. Without a binary platform tag,
        # `--platform any` alone can't fetch compiled wheels (markupsafe,
        # numpy, pydantic-core, ...). `platform.machine()` reports the
        # WMI spelling (AMD64/ARM64), not the wheel-tag one.
        plats = [
            {"AMD64": "win_amd64", "ARM64": "win_arm64", "x86": "win32"}.get(
                machine, f"win_{machine.lower()}"
            )
        ]
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
