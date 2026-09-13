"""Checkpoint validated datetime initializer guards before performance measurement."""
from pathlib import Path
import fcntl
import json
import shutil
from prepare_performance_checkpoint import ROOT, BASE, allowed, pack, save, sha

lease = (ROOT / 'target/weavepy-performance-stage.lock').open('a+')
fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
runtime = ROOT / 'target/datetime-initializer-guards-runtime'
proto = ROOT / 'target/datetime-initializer-guards'
out = BASE / 'datetime-initializer-guards'
assert not out.exists()
build = json.loads((runtime / 'build.json').read_text())
release = json.loads((ROOT / 'target/datetime-initializer-guards-release/report.json').read_text())
assert build['status'] == release['status'] == 'complete'
validation = json.loads((ROOT / 'target/datetime-initializer-guards-runtime-validation/validation.json').read_text())
assert len(validation) == 275 and all(row['passed'] for row in validation)
coverage = json.loads((ROOT / 'target/datetime-initializer-guards-census-coverage/report.json').read_text())
assert coverage['status'] == 'complete' and len(coverage['rows']) == 24 and not coverage['result_mismatches']
sources = {}
for name, digest in build['source_hashes'].items():
    path = runtime / 'source' / name
    assert sha(path.read_bytes()) == digest, name
    sources[name] = path
evidence = {}
def add(path):
    if allowed(path) and not path.is_relative_to(runtime / 'source'):
        evidence[str(path.relative_to(ROOT))] = path
for path in (ROOT / 'target').iterdir():
    if path.name.startswith('datetime-initializer-guards') or 'datetime_initializer_guards' in path.name or 'datetime_initializer_subclass' in path.name:
        if path.is_dir():
            if path.name.endswith('-tests'):
                continue
            for item in path.rglob('*'):
                add(item)
        else:
            add(path)
for manifest_name in ['datetime-initializer-guards-release-methods.json', 'datetime-initializer-guards-timing-methods.json', 'datetime-initializer-guards-subclass-methods.json']:
    for name, digest in json.loads((ROOT / 'target' / manifest_name).read_text()).items():
        path = ROOT / name
        assert sha(path.read_bytes()) == digest, name
        add(path)
for name in ['performance_phase96_status.md', 'prepare_performance_checkpoint.py', 'prune_completed_initializer_guard_predecessor_artifacts.py']:
    add(ROOT / 'target' / name)
out.mkdir()
manifest = {'purpose': __doc__, 'status': 'validated; unmeasured; not integrated into the active runtime',
            'candidate': build['candidate'], 'reference': build['reference'],
            'pending': ['Run the separate ordinary-subclass input preflight.', 'Use the v2 pipeline and frozen supplement for performance measurement before acceptance.'],
            'archives': {}}
manifest['archives']['sources.tar.gz'] = pack(out / 'sources.tar.gz', sources)
manifest['archives']['evidence.tar.gz'] = pack(out / 'evidence.tar.gz', evidence)
shutil.copyfile(proto / 'changes-01.patch', out / 'changes.patch')
(out / 'REPORT.md').write_text('''# Datetime initializer guards: unmeasured checkpoint

This isolated phase 96 candidate caches successful slot-layout validation by
class version and sends ordinary subclasses directly to their original Python
initialization path. Native payload, C-body, setter, metaclass, version, and
empty-slot guards still apply. Mutation invalidates the successful-layout cache.

Counter-based regression tests reduced full layout checks for 2,000 constructor
calls from about 2,000 to 3 through 5, depending on the datetime type. Subclass
helper attempts fell to import-time constants. These are path counts, not speed
measurements. Original failures, source versions, and corrected results remain
in the evidence archive.

Validation passed 380 VM tests, 156 C API tests with JIT enabled, 79 JIT tests,
strict lint and format checks, no-JIT checks, 99 targeted checks, 275 compatibility
checks, and all 24 census result comparisons. The 12 original constructor inputs
also passed six-engine correctness checks. The additional four ordinary-subclass
input checks and all performance measurements remain pending at the user's
requested checkpoint. No timing gate or sample has run for this revision.

The CLI is 44,119,120 bytes, SHA-256
`7f2b0647c1cf9d1454e70e36f97877296398ce7098bd8fb18e5431f5d54e4a61`.
The 340 sources, incremental patch against phase 95, methods, inputs, raw
validation, counter evidence, and timing protocol are archived. Binaries and
generated caches are excluded. Extract sources into a separate checkout and
build with `cargo build --release -p weavepy-cli`. The prepared v2 timing pipeline
keeps the four new subclass controls separate from the original twelve cases.

This candidate is not integrated into the active phase 79 runtime. Known
fold-index coercion and native-frame identity differences remain unresolved.
There is no runtime speed, peak RSS, 32-bit execution, energy, scaling, or
controlled build-time claim for this candidate.
''')
for name in ['REPORT.md', 'changes.patch']:
    data = (out / name).read_bytes()
    manifest[name] = {'bytes': len(data), 'sha256': sha(data)}
save(out / 'MANIFEST.json', manifest)
print('Phase 96 validated sources and evidence saved; timing remains pending.', flush=True)
