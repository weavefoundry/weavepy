"""Regenerate report tables from complete paired measurement files."""

import json
import math
from pathlib import Path
import statistics
import runpy

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]


def main():
    suite = json.loads((HERE / "suite.json").read_text())
    probes = json.loads((HERE / "probes.json").read_text())
    assert set(probes["rows"]) == set(runpy.run_path(str(HERE / "probes.py"))["KERNELS"])
    rows = suite["rows"]
    assert len(rows) == 24, "The standard suite must finish before summarizing"
    env = json.loads((HERE / "environment.json").read_text())
    validation = json.loads((HERE / "validation.json").read_text())
    assert len(validation) == 196 and all(row["passed"] for row in validation)
    candidate = env["binaries"]["new"]["sha256"]
    previous = "78f309f65565be93dc7a5cb725df2243640c566199374110515207191998edd5"
    tuple_cache = "a8faed6241e637f921ad1bb22f7ea16d48bae1b93a5ad1bf78a590e87670610b"
    assert env["binaries"]["previous"]["sha256"] == previous
    assert suite["binaries"]["previous"]["sha256"] == previous
    assert all("previous_comparisons" in row for row in rows.values())
    parallel = json.loads((HERE / "parallel.json").read_text())
    assert parallel["binaries"]["new"]["sha256"] == candidate
    assert set(parallel["modes"]) == {"1", "0"}
    startup = json.loads((HERE / "startup.json").read_text())
    assert startup["binaries"]["new"]["sha256"] == candidate
    assert suite["binaries"]["new"]["sha256"] == candidate
    assert probes["binaries"]["new"]["sha256"] == candidate
    oracle = json.loads((HERE / "float-oracle.json").read_text())
    assert oracle["binaries"]["new"]["sha256"] == candidate
    assert oracle["differences"]["new_vs_cpython"]["count"] == 0
    out = ["# Runtime metadata and numeric text performance\n\n",
           "These changes reduce object metadata storage, dictionary bookkeeping, "
           "float-formatting allocations, and JSON key allocations. They also "
           "extend compiled enumeration, list builders, constant loads, and arithmetic "
           "on generic call results, and cache successful tuple hashes. WeavePy still does not "
           "outperform CPython across all workloads or all measured metrics.\n\n",
           "The baseline is commit `9a69c4161ade6527a5f4457d7e7272075cb1f0d9`. "
           "The candidate uses the same release profile and default JIT feature "
           "on macOS ARM64, with CPython 3.14.7 as the reference. "
           "The standard suite also pairs the immediately preceding small-slot "
           "release with the candidate in the same measurement cycles. "
           "[Raw samples, checksums, and reproduction instructions]"
           "(../crates/weavepy-bench/census/2026-09-runtime-metadata/) "
           "include intermediate stages as well as these measured results.\n\n",
           f"The measured executable is `{env['binaries']['new']['path']}` "
           f"with SHA-256 `{candidate}`.\n\n",
           "## Changes\n\n",
           "- Replace lock-backed cells for small copyable metadata with native "
           "atomic cells. Borrowed payloads and fields without native atomic "
           "support retain their existing locks, including fork recovery. "
           "A boolean cell occupies one byte and a 64-bit cell occupies eight.\n",
           "- Keep dictionary comparison bookkeeping and built-in type registry "
           "handles in ordinary thread-local cells. Shared Python objects retain "
           "their synchronization, and nested key comparisons preserve outer errors.\n",
           "- Format float digits with the already locked Zmij dependency, adapting "
           "its notation to Python's rules. Write exact JSON integers and finite "
           "floats directly into the output buffer. This also fixes shortest-decimal "
           "rounding ties that previously differed from CPython.\n",
           "- Construct Python strings for native scalars directly from stack "
           "buffers. Scalar subclasses retain their protocol dispatch, and "
           "large integers retain their conversion limits.\n",
           "- Look up unescaped UTF-8 JSON keys before allocating string storage. "
           "Escaped spellings still share identity with equal literal keys; "
           "surrogate-bearing strings keep their existing representation.\n",
           "- Build decoded JSON dictionaries as fields arrive. Only the pairs "
           "hook stages a pair vector. Overwritten values are released before "
           "the next field, matching CPython's callback timing.\n",
           "- Infer integer index and byte lanes for certified `enumerate(bytes)` "
           "calls. Generic enumeration retains an integer index and an object "
           "value. Existing runtime guards validate consumed values and preserve "
           "them when execution returns to the interpreter. Generic pair loops "
           "can keep immutable primitive values boxed without an immediate exit.\n",
           "- Read native byte enumeration into the compiled pair lanes without "
           "allocating temporary Python tuples. Shared iterator positions remain "
           "current, and counter overflow falls back before consuming a byte. "
           "Known local objects also retain their inferred enumeration lanes.\n",
           "- Enumerate exact tuples of immutable values directly into integer "
           "and object lanes. Shared cursors advance together, and unsupported "
           "values, counter overflow, and pin pressure retain the generic path.\n",
           "- Align three interpreter entry points independently on macOS ARM64. "
           "Paired experiments recovered code-placement regressions without the "
           "executable growth of aligning every function. Other targets retain "
           "their existing layout.\n",
           "- Stage scalar type tags for compiled list appends, allowing fresh "
           "generic lists to accept integers, floats, and booleans without "
           "leaving native code. Typed lists keep their existing element "
           "constraints, and failures resume before mutating the list.\n",
           "- Reap temporary list pins on native frame exit. Previously, the "
           "collector retained those lists until a later collection; cleanup "
           "now releases them and their dead children promptly, including "
           "when cyclic collection is disabled. Escaped lists stay alive.\n",
           "- Cache Python hashes in eight bytes with a native atomic word. "
           "Hash protocol results normalize the reserved -1 value, preserve "
           "machine-sized integers, and hash larger integers without invoking "
           "conversion hooks on integer subclasses. Temporary results are "
           "released before hash() returns. Instances and frozen sets previously "
           "paid for 40-byte locked optional hashes.\n",
           "- Propagate key hash and equality exceptions from dictionary and "
           "set construction, including comprehensions and mapping inputs. "
           "Exact dictionary copies retain the stored hashes instead of "
           "calling key hash methods again.\n",
           "- Store the first instance slot inline on 64-bit targets, avoiding "
           "a table allocation. Two to eight populated slots use a small "
           "ordered vector; the ninth promotes to an ordered table. Updates "
           "reuse the existing key, and new slot names go directly into string "
           "storage without a temporary String. Smaller targets retain "
           "ordinary table storage.\n\n",
           "- Construct fixed-size tuples directly from arrays, removing the "
           "intermediate vector allocation. Bytecode tuples with up to three "
           "elements retain the existing free list, and immutable scalar "
           "enumeration values use the inline operand copier.\n",
           "- Store successful tuple hashes beside the elements in the same "
           "allocation. Failed hashes remain uncached, unique free-list reuse "
           "clears the cache, and callback exceptions propagate from dictionary "
           "and set lookup. The hash word adds eight bytes per tuple.\n",
           "- Use cached tuple keys directly in functools caches. Hash arguments "
           "once per call, surface hash and equality failures before invoking "
           "the wrapped function, and remove the Python fallback key wrapper.\n",
           "- Load canonical string and tuple constants into compiled frames, "
           "preserving interpreter identity across activations. Constant pins "
           "are reused within each activation. Guarded tuple length accepts "
           "exact tuples and resumes the interpreter for other objects.\n",
           "- Guard boxed integer operands before native arithmetic, allowing "
           "generic call results to stay in compiled loops. Other types, "
           "overflow, and errors retain their original operands and callback "
           "results when execution resumes in the interpreter. Calls that skip "
           "a defaulted parameter use the existing generic keyword binder.\n",
           "- Guard native integer true division before converting either "
           "operand to binary64. Values outside the exact conversion range "
           "resume the interpreter for correctly rounded division.\n\n",
           "## Validation\n\n",
           f"All {len(validation)} compatibility checks pass, including threading, "
           "forking, tracing, GC, pickle, JSON, floats, complex numbers, and "
           "dictionary comparisons, tuple storage, functools, and tuple C-API "
           "operations. VM unit tests, JIT tests, Clippy, compilation "
           "without the JIT, and workspace checks also pass. The tuple allocation "
           "module also passes three Miri checks on ARM64 and 32-bit x86 under "
           "both tested aliasing models; the [standalone harness and its limits]"
           "(../crates/weavepy-bench/census/2026-09-runtime-metadata/tuple-cache-miri/) "
           "are retained.\n\n",
           f"The float oracle checks {oracle['values']:,} binary64 values in "
           "`repr`, `str`, complex text, and JSON output. It finds no differences "
           "from CPython; the checkpoint differed on "
           f"{oracle['differences']['base_vs_cpython']['count']} values. "
           "Seeded datetime comparisons retain the checkpoint's five known "
           "timedelta microsecond differences.\n\n",
           "## Standard suite\n\n",
           "Values are candidate/reference ratios; smaller values mean less time "
           "or memory. Geometric means give each fixture equal weight. The "
           "workload aggregate includes all 23 timed workloads; the process "
           "aggregates include all 24 fixtures, including startup.\n\n",
           "| Metric | New/base JIT | New/base interpreter | New/previous JIT | New/previous interpreter | New/CPython JIT | New/CPython interpreter |\n",
           "| --- | ---: | ---: | ---: | ---: | ---: | ---: |\n"]
    for metric, label in [("ns", "Timed workload"), ("wall_ns", "Process elapsed time"),
                          ("cpu_ns", "Process CPU time"), ("rss_bytes", "Peak RSS")]:
        selected = [r for name, r in rows.items() if metric != "ns" or name != "startup"]
        values = [statistics.geometric_mean(r[group][mode][metric] for r in selected)
                  for group in ("comparisons", "previous_comparisons", "cpython_comparisons")
                  for mode in ("jit", "interp")]
        out.append("| " + label + " | " + " | ".join(f"{v:.3f}" for v in values) + " |\n")
    out.extend(["\n| Fixture | New/base JIT time | New/base interpreter time | New/CPython JIT time | New/CPython JIT RSS |\n",
                "| --- | ---: | ---: | ---: | ---: |\n"])
    for name, row in rows.items():
        values = [row["comparisons"]["jit"]["ns"], row["comparisons"]["interp"]["ns"],
                  row["cpython_comparisons"]["jit"]["ns"], row["cpython_comparisons"]["jit"]["rss_bytes"]]
        out.append(f"| `{name}` | " + " | ".join(f"{v:.3f}" for v in values) + " |\n")
    out.extend(["\nThe incremental comparison below uses the preceding small-slot "
                "release from the same alternating process cycles. It includes every "
                "standard fixture, whether or not it uses instance slots.\n\n",
                "| Fixture | New/previous JIT time | New/previous interpreter time | New/previous JIT CPU | New/previous interpreter CPU | New/previous JIT RSS |\n",
                "| --- | ---: | ---: | ---: | ---: | ---: |\n"])
    for name, row in rows.items():
        c = row["previous_comparisons"]
        values = [c["jit"]["ns"], c["interp"]["ns"], c["jit"]["cpu_ns"],
                  c["interp"]["cpu_ns"], c["jit"]["rss_bytes"]]
        out.append(f"| `{name}` | " + " | ".join(f"{v:.3f}" for v in values) + " |\n")
    out.append("\nThe CPython datetime reference varies substantially between "
               "measurement runs, including process CPU time. An earlier "
               "post-census diagnostic confirms native datetime constructors "
               "and arithmetic in its processes, but doesn't identify what "
               "earlier processes loaded or explain the timing difference. "
               "[Diagnostic samples]"
               "(../crates/weavepy-bench/census/2026-09-runtime-metadata/before-tuple-hash-cache/cpython-datetime-diagnostic.json) "
               "and the [original-file repeat]"
               "(../crates/weavepy-bench/census/2026-09-runtime-metadata/before-tuple-hash-cache/cpython-datetime-original-repeat.json) "
               "are retained alongside the original census. Treat CPython "
               "ratios as measurements of the recorded runs, rather than "
               "stable ratios across censuses.\n")
    repeat_path = HERE / "repeat.json"
    if repeat_path.exists():
        repeat = json.loads(repeat_path.read_text())
        assert repeat["binaries"]["new"]["sha256"] == candidate
        out.extend(["\nA separate nine-cycle repeat checks the apparent regressions "
                    "in the byte scrambler, list operations, and string methods. "
                    "It retains the standard work values and includes first-run "
                    "execution, as the main suite does.\n\n",
                    "| Fixture | Repeated new/base JIT time | Repeated new/base interpreter time |\n",
                    "| --- | ---: | ---: |\n"])
        for name, row in repeat["rows"].items():
            c = row["comparisons"]
            out.append(f"| `{name}` | {c['jit']['ns']:.3f} | {c['interp']['ns']:.3f} |\n")
    out.extend(["\n## Supplemental probes\n\n",
                "Elapsed times are workload medians in milliseconds. CPU and RSS "
                "ratios are medians of paired candidate/reference samples. Workload "
                "CPU excludes setup; peak RSS covers the entire process.\n\n",
                "| Probe | Baseline ms | New ms | CPython ms | New/base workload CPU | New/base RSS | New/CPython RSS |\n",
                "| --- | ---: | ---: | ---: | ---: | ---: | ---: |\n"])
    for name, row in probes["rows"].items():
        elapsed = [statistics.median(v["ns"] for v in row["samples"][label]) / 1e6
                   for label in ("base", "new", "cpython")]
        ratios = [row["comparisons"]["base"]["work_cpu_ns"],
                  row["comparisons"]["base"]["rss_bytes"],
                  row["comparisons"]["cpython"]["rss_bytes"]]
        out.append(f"| `{name}` | " + " | ".join(f"{v:.4f}" for v in elapsed) + " | " +
                   " | ".join(f"{v:.6f}" if v < 0.01 else f"{v:.3f}" for v in ratios) + " |\n")
    instances = json.loads((HERE / "instance-probes.json").read_text())
    assert instances["binaries"]["new"]["sha256"] == candidate
    assert set(instances["rows"]) == set(runpy.run_path(str(HERE / "instance_probes.py"))["KERNELS"])
    out.extend(["\n## Instance and hash storage\n\n",
                "These interpreter-mode probes compare compact caches and inline "
                "slots with the preceding native-list-cleanup build. Nine paired "
                "cycles retain allocation, destruction, repeated access, and hash "
                "workloads. RSS covers the whole process.\n\n",
                "| Probe | New/previous time | New/previous workload CPU | New/previous RSS | New/CPython time | New/CPython RSS |\n",
                "| --- | ---: | ---: | ---: | ---: | ---: |\n"])
    for name, row in instances["rows"].items():
        c = row["comparisons"]
        values = [c["base"]["ns"], c["base"]["work_cpu_ns"], c["base"]["rss_bytes"],
                  c["cpython"]["ns"], c["cpython"]["rss_bytes"]]
        out.append(f"| `{name}` | " + " | ".join(f"{v:.3f}" for v in values) + " |\n")
    cache_delta = json.loads((HERE / "before-tuple-hash-cache/hash-cache-delta.json").read_text())
    assert cache_delta["binaries"]["new"]["sha256"] == "cd9d7362f00fb642f99a3c980811b0f794cf420cf24de6be7c332ce633d5eb01"
    assert cache_delta["binaries"]["base"]["sha256"] == "43c70898ea0a57a7aa9ce2db7e6e2aeced2b5e3076c642f28dde525329920181"
    assert cache_delta["samples"] == 9 and len(cache_delta["rows"]) == 6
    out.extend(["\nThe retained preceding comparison isolates the eight-byte cache and hash "
                "protocol fixes against the immediately preceding small-tuple "
                "release, before tuple hash caching. Each row retains nine paired cycles. This historical comparison "
                "includes the larger-slot workload even where elapsed time "
                "doesn't improve.\n\n",
                "| Probe | New/previous time | New/previous workload CPU | New/previous RSS | New/CPython time | New/CPython RSS |\n",
                "| --- | ---: | ---: | ---: | ---: | ---: |\n"])
    for name, row in cache_delta["rows"].items():
        c = row["comparisons"]
        values = [c["base"]["ns"], c["base"]["work_cpu_ns"], c["base"]["rss_bytes"],
                  c["cpython"]["ns"], c["cpython"]["rss_bytes"]]
        out.append(f"| `{name}` | " + " | ".join(f"{v:.3f}" for v in values) + " |\n")
    tuples = json.loads((HERE / "tuple-probes.json").read_text())
    assert tuples["binaries"]["new"]["sha256"] == candidate
    assert set(tuples["rows"]) == set(runpy.run_path(str(HERE / "tuple_probes.py"))["KERNELS"])
    out.extend(["\n## Tuple construction\n\n",
                "These interpreter-mode probes compare direct tuple allocation "
                "with the preceding tuple-enumeration build. Nine paired cycles "
                "cover retained tuples, allocation and reuse, dictionary items, "
                "enumeration, and string partitioning. The eight-element literal "
                "retains the general bytecode construction path.\n\n",
                "| Probe | New/previous time | New/previous workload CPU | New/previous RSS | New/CPython time | New/CPython RSS |\n",
                "| --- | ---: | ---: | ---: | ---: | ---: |\n"])
    for name, row in tuples["rows"].items():
        c = row["comparisons"]
        values = [c["base"]["ns"], c["base"]["work_cpu_ns"], c["base"]["rss_bytes"],
                  c["cpython"]["ns"], c["cpython"]["rss_bytes"]]
        out.append(f"| `{name}` | " + " | ".join(f"{v:.3f}" for v in values) + " |\n")
    for filename, kernel_file, heading, description in [
        ("tuple-hash-probes.json", "tuple_hash_probes.py", "Tuple hash caching",
         "Repeated hashes, dictionary lookups, fresh keys, and functools calls compare the tuple-cache build with the immediately preceding hash-normalization release."),
        ("tuple-cache-allocation.json", "tuple_probes.py", "Tuple cache allocation costs",
         "Unhashed tuple workloads use the same immediately preceding release to expose the cost of adding the hash word, including retained one-, two-, three-, and eight-element tuples.")]:
        measured = json.loads((HERE / "before-small-slots" / filename).read_text())
        assert measured["binaries"]["new"]["sha256"] == tuple_cache
        assert measured["binaries"]["base"]["sha256"] == "cd9d7362f00fb642f99a3c980811b0f794cf420cf24de6be7c332ce633d5eb01"
        assert measured["samples"] == 9
        assert set(measured["rows"]) == set(runpy.run_path(str(HERE / kernel_file))["KERNELS"])
        out.extend([f"\n## Preceding {heading.lower()}\n\n{description} These historical measurements precede the small-slot change. Nine paired interpreter-mode cycles retain every workload, including regressions.\n\n",
                    "| Probe | New/previous time | New/previous workload CPU | New/previous RSS | New/CPython time | New/CPython RSS |\n",
                    "| --- | ---: | ---: | ---: | ---: | ---: |\n"])
        for name, row in measured["rows"].items():
            c = row["comparisons"]
            values = [c["base"]["ns"], c["base"]["work_cpu_ns"], c["base"]["rss_bytes"],
                      c["cpython"]["ns"], c["cpython"]["rss_bytes"]]
            out.append(f"| `{name}` | " + " | ".join(f"{v:.3f}" for v in values) + " |\n")
    small_slots = json.loads((HERE / "small-slot-probes.json").read_text())
    assert small_slots["binaries"]["new"]["sha256"] == candidate
    assert small_slots["binaries"]["base"]["sha256"] == tuple_cache
    assert small_slots["samples"] == 9
    assert set(small_slots["rows"]) == set(runpy.run_path(str(HERE / "small_slot_probes.py"))["KERNELS"])
    out.extend(["\n## Small-slot allocation and access\n\n",
                "These eleven interpreter-mode workloads compare the current "
                "candidate with the earlier tuple-cache "
                "release. Nine paired cycles cover one, two, eight, nine, and sixteen "
                "slots. Repeated access targets the last populated field, where the "
                "vector search costs the most; deletion and reinsertion retain order.\n\n",
                "| Probe | New/previous time | New/previous workload CPU | New/previous RSS | New/CPython time | New/CPython RSS |\n",
                "| --- | ---: | ---: | ---: | ---: | ---: |\n"])
    for name, row in small_slots["rows"].items():
        c = row["comparisons"]
        values = [c["base"]["ns"], c["base"]["work_cpu_ns"], c["base"]["rss_bytes"],
                  c["cpython"]["ns"], c["cpython"]["rss_bytes"]]
        out.append(f"| `{name}` | " + " | ".join(f"{v:.3f}" for v in values) + " |\n")
    tuple_repeat = json.loads((HERE / "before-hash-normalization/tuple-repeat.json").read_text())
    assert tuple_repeat["binaries"]["new"]["sha256"] == "43c70898ea0a57a7aa9ce2db7e6e2aeced2b5e3076c642f28dde525329920181"
    assert tuple_repeat["samples"] == 19
    repeat_metrics = tuple_repeat["rows"]["string_partition"]["comparisons"]["base"]
    out.append("\nThe preceding small-tuple candidate had a focused partition run "
               "with higher elapsed time despite "
               "slightly lower CPU use. Its separate 19-cycle repeat used "
               f"{repeat_metrics['ns']:.3f} times the preceding elapsed time, "
               f"{repeat_metrics['work_cpu_ns']:.3f} times its workload CPU, and "
               f"{repeat_metrics['rss_bytes']:.3f} times its peak RSS. The "
               "[initial and repeated samples]"
               "(../crates/weavepy-bench/census/2026-09-runtime-metadata/small-tuples-initial/) "
               "remain available.\n")
    enumeration = json.loads((HERE / "enumerate-probes.json").read_text())
    assert set(enumeration["rows"]) == set(runpy.run_path(str(HERE / "jit_probes.py"))["KERNELS"])
    assert enumeration["binaries"]["new"]["sha256"] == candidate
    out.extend(["\n## Enumeration\n\n",
                "These warm probes isolate the JIT change against the preserved "
                "metadata and JSON build. The interpreter comparison is a control. "
                "Each workload processes 2,048 elements per call and calls the "
                "checksum function 200 times.\n\n",
                "| Probe | New/previous JIT time | New/previous interpreter time | New/CPython JIT time | New/CPython JIT RSS |\n",
                "| --- | ---: | ---: | ---: | ---: |\n"])
    for name, row in enumeration["rows"].items():
        c = row["comparisons"]
        values = [c["jit"]["ns"], c["interp"]["ns"], c["cpython"]["ns"],
                  c["cpython"]["rss_bytes"]]
        out.append(f"| `{name}` | " + " | ".join(f"{v:.3f}" for v in values) + " |\n")
    builders = json.loads((HERE / "builder-probes.json").read_text())
    assert set(builders["rows"]) == set(runpy.run_path(str(HERE / "builder_probes.py"))["KERNELS"])
    assert builders["binaries"]["new"]["sha256"] == candidate
    builder_validation = json.loads((HERE / "builder-validation.json").read_text())
    assert builder_validation["binaries"]["new"]["sha256"] == candidate
    assert all(row["returncode"] == 0 and row["compiled"] and row["deopts"] == 0
               for row in builder_validation["steady_paths"].values())
    out.extend(["\n## List builders\n\n",
                "These checked warm workloads build 1,024-element results 200 times. "
                "The previous build includes the targeted interpreter alignment, "
                "so this comparison covers tagged appends and native list cleanup. Nine paired cycles "
                "measure time, CPU use, and peak RSS. Each builder compiles without "
                "repeated deoptimizations in the separate trace check.\n\n",
                "| Probe | New/previous JIT time | New/previous interpreter time | New/CPython JIT time | New/CPython workload CPU | New/CPython JIT RSS |\n",
                "| --- | ---: | ---: | ---: | ---: | ---: |\n"])
    for name, row in builders["rows"].items():
        c = row["comparisons"]
        values = [c["jit"]["ns"], c["interp"]["ns"], c["cpython"]["ns"],
                  c["cpython"]["work_cpu_ns"], c["cpython"]["rss_bytes"]]
        out.append(f"| `{name}` | " + " | ".join(f"{v:.3f}" for v in values) + " |\n")
    for stem, kernel_file, heading, description in [
        ("constant", "constant_probes.py", "Compiled constants and tuple length",
         "Five warm workloads cover tuple constants, guarded tuple lengths, repeated string loads, and short activations."),
        ("scalar-result", "scalar_result_probes.py", "Generic call results",
         "Four warm workloads consume integer results from callback parameters, keyword calls, and callable instances.")]:
        measured = json.loads((HERE / f"{stem}-probes.json").read_text())
        checked = json.loads((HERE / f"{stem}-validation.json").read_text())
        assert measured["binaries"]["new"]["sha256"] == candidate
        assert measured["binaries"]["base"]["sha256"] == previous
        assert checked["binaries"]["new"]["sha256"] == candidate
        assert measured["samples"] == 9
        assert set(measured["rows"]) == set(runpy.run_path(str(HERE / kernel_file))["KERNELS"])
        assert all(row["returncode"] == 0 and row["compiled"] and row["deopts"] == 0
                   for row in checked["steady_paths"].values())
        if stem == "constant":
            assert checked["call_fixture"]["returncode"] == 0
            assert checked["call_fixture"]["compiled"]
        out.extend([f"\n## {heading}\n\n{description} Nine paired cycles compare the candidate with the preceding small-slot release and CPython. Separate traces confirm each measured kernel compiles without repeated exits.\n\n",
                    "| Probe | New/previous JIT time | New/previous interpreter time | New/previous workload CPU | New/previous RSS | New/CPython JIT time | New/CPython JIT RSS |\n",
                    "| --- | ---: | ---: | ---: | ---: | ---: | ---: |\n"])
        for name, row in measured["rows"].items():
            c = row["comparisons"]
            values = [c["jit"]["ns"], c["interp"]["ns"], c["jit"]["work_cpu_ns"],
                      c["jit"]["rss_bytes"], c["cpython"]["ns"], c["cpython"]["rss_bytes"]]
            out.append(f"| `{name}` | " + " | ".join(f"{v:.3f}" for v in values) + " |\n")
    out.append("\nThe [initial constant-load candidate]"
               "(../crates/weavepy-bench/census/2026-09-runtime-metadata/constant-loads-initial/) "
               "regressed repeated string loads by 12 percent. Its samples remain "
               "available. The current candidate forces the shared constant helper "
               "to inline. An intermediate scalar-result candidate also exposed "
               "incorrect rounding in existing native integer division; its "
               "[failing regression and source]"
               "(../crates/weavepy-bench/census/2026-09-runtime-metadata/scalar-guard-precision-failure/) "
               "are retained without performance claims.\n")
    out.extend(["\n## Startup and imports\n\n",
                "These interpreter-mode probes retain 31 paired samples after a "
                "discarded warmup cycle, using the staged standard library. "
                "Times include the whole process. The 95th percentile uses the "
                "nearest-rank sample; it is a local observation, not a latency "
                "guarantee.\n\n",
                "| Probe | Baseline median ms | New median ms | CPython median ms | Baseline p95 ms | New p95 ms | CPython p95 ms | New/base RSS | New/CPython RSS |\n",
                "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n"])
    assert set(startup["rows"]) == {"startup", "startup_no_site", "imports"}
    for name, row in startup["rows"].items():
        samples = row["samples"]
        assert all(len(values) == 31 for values in samples.values())
        elapsed = [[v["wall_ns"] / 1e6 for v in samples[label]]
                   for label in ("base", "new", "cpython")]
        values = [statistics.median(v) for v in elapsed]
        values.extend(sorted(v)[math.ceil(len(v) * 0.95) - 1] for v in elapsed)
        values.extend(statistics.median(n["rss_bytes"] / r["rss_bytes"]
                                        for n, r in zip(samples["new"], samples[label], strict=True))
                      for label in ("base", "cpython"))
        out.append(f"| `{name}` | " + " | ".join(f"{v:.3f}" for v in values) + " |\n")
    out.extend(["\n## Eight-thread execution\n\n",
                "The checked concurrency probe uses the existing arithmetic kernel "
                f"with {parallel['work_per_thread']:,} iterations per worker. "
                "It starts timing before releasing an event gate and verifies all "
                "eight results. Thread creation is outside the timer; event release, "
                "worker execution, and joins are inside. New worker threads retain "
                "their normal JIT initialization costs. Each GIL mode runs separately "
                "with five measured cycles and one discarded cycle.\n\n",
                "Both WeavePy modes request the JIT; runtime gating still applies. "
                "The installed CPython is a GIL build, so this is not a comparison "
                "with free-threaded CPython. Larger serial/parallel speedups are "
                "better; smaller time, CPU, and memory ratios are better.\n\n",
                "| WeavePy mode | New/base parallel time | New/CPython parallel time | New/base parallel CPU | New/CPython parallel CPU | New/base RSS | New/CPython RSS | New serial/parallel speedup | CPython speedup |\n",
                "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n"])
    for gil, row in parallel["modes"].items():
        assert all(len(values) == 5 for values in row["samples"].values())
        values = [row["comparisons"][reference][metric]
                  for metric in ("parallel_ns", "parallel_cpu_ns", "rss_bytes")
                  for reference in ("base", "cpython")]
        values.extend(statistics.median(v["serial_ns"] / v["parallel_ns"]
                                        for v in row["samples"][label])
                      for label in ("new", "cpython"))
        name = "GIL enabled" if gil == "1" else "GIL disabled"
        out.append(f"| {name} | " + " | ".join(f"{v:.3f}" for v in values) + " |\n")
    timed = {name: row for name, row in rows.items() if name != "startup"}
    faster = sum(row["cpython_comparisons"]["jit"]["ns"] < 1 for row in timed.values())
    lower_memory = sum(row["cpython_comparisons"]["jit"]["rss_bytes"] < 1
                       for row in timed.values())
    out.extend(["\n## Remaining gaps\n\n",
                f"With the JIT enabled, the candidate uses less workload time than "
                f"CPython in {faster} of {len(timed)} standard timed fixtures and "
                f"less peak process memory in {lower_memory}. The largest remaining "
                "time ratios are below. These gaps remain part of the measurement "
                "set, including workloads that use pure-Python standard-library "
                "implementations where CPython has native implementations.\n\n",
                "| Fixture | New/CPython JIT time | New/CPython JIT RSS |\n",
                "| --- | ---: | ---: |\n"])
    slowest = sorted(timed.items(),
                     key=lambda item: item[1]["cpython_comparisons"]["jit"]["ns"], reverse=True)
    for name, row in slowest[:5]:
        metrics = row["cpython_comparisons"]["jit"]
        out.append(f"| `{name}` | {metrics['ns']:.3f} | {metrics['rss_bytes']:.3f} |\n")
    base_bytes = env["binaries"]["base"]["bytes"]
    new_bytes = env["binaries"]["new"]["bytes"]
    out.append(f"\nThe executable contains {new_bytes:,} bytes, compared with "
               f"{base_bytes:,} at the checkpoint. Build latency was not measured "
               "under controlled conditions. The memory figures are process peak "
               "RSS, not isolated object sizes or allocation counts. These "
               "measurements cover this host and these workloads; they cannot "
               "establish superiority for every Python program.\n")
    (ROOT / "docs/PERFORMANCE-RUNTIME-METADATA.md").write_text("".join(out))


if __name__ == "__main__":
    main()
