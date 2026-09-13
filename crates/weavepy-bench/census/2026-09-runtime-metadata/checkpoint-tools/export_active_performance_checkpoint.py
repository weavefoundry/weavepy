"""Preserve the validated, unmeasured phase 79 runtime checkpoint and its methods."""
from pathlib import Path
import json
import shutil
from prepare_performance_checkpoint import ROOT, BASE, allowed, pack, save, sha

out = BASE / 'native-scratch-byte-budget'
assert not out.exists()
out.mkdir()
pre = json.loads((ROOT / 'target/native-scratch-byte-budget-prebuild.json').read_text())
for name, digest in pre['sources'].items():
    assert sha((ROOT / name).read_bytes()) == digest, name
sources = {name: ROOT / name for name in pre['sources']}
evidence = {}
def add(path):
    if allowed(path):
        evidence[str(path.relative_to(ROOT))] = path
for path in (ROOT / 'target').iterdir():
    if path.name.startswith('native-scratch-byte-budget') or 'native_scratch_byte_budget' in path.name:
        if path.is_dir():
            for item in path.rglob('*'):
                add(item)
        else:
            add(path)
for name, digest in pre['methods'].items():
    path = ROOT / name
    assert sha(path.read_bytes()) == digest, name
    add(path)
for name in ['performance_phase79_status.md', 'export_active_performance_checkpoint.py', 'prepare_performance_checkpoint.py']:
    add(ROOT / 'target' / name)
manifest = {'purpose': __doc__, 'status': 'validated, unmeasured, not accepted as an overall CPython win',
            'active_source_count': len(sources), 'frozen_method_count': len(pre['methods']),
            'archives': {}}
manifest['archives']['sources.tar.gz'] = pack(out / 'sources.tar.gz', sources)
manifest['archives']['evidence.tar.gz'] = pack(out / 'evidence.tar.gz', evidence)
shutil.copyfile(ROOT / 'target/native-scratch-byte-budget-review.diff', out / 'changes.patch')
(out / 'REPORT.md').write_text('''# Active runtime checkpoint: bounded native scratch retention

The September 13 PR checkpoint builds the phase 79 runtime. All 340 source
hashes match its completed validation: 353 VM tests, 156 C API tests, strict
lint and format checks, no-JIT checks, 99 targeted checks, 44 independent
CPython result checks, and 275 compatibility checks passed.

Phase 78 consolidated nested native-call scratch storage. This revision adds
a 16 KiB retained element-capacity budget per scratch element type, alongside
the existing 64-entry cap. Oversized active calls remain supported. The two
pools retain at most 32 KiB of element capacity per thread; this does not bound
active storage, allocator overhead, or process RSS.

Both timing gates stopped before starting a benchmark because host load was
too high. This revision is validated but unmeasured. The phase 78 predecessor
measured about 1.5% less full-suite time against the retained phase 69 reference;
that result is not a measurement of this additional capacity limit.

The CLI built with `cargo build --release -p weavepy-cli` was 44,097,264 bytes,
SHA-256 `4c627e81ee77bd25ec6140fdfd13f13c0886c82d0dddca0be1d476661c2f0c76`.
The archive contains sources, methods, inputs, validation, both unsuccessful
gate attempts, and their original limitations. Build products and caches are
excluded. The separate phases 80 through 96 have not been overlaid onto this
runtime; their source archives preserve that experimental work.
''')
for name in ['REPORT.md', 'changes.patch']:
    data = (out / name).read_bytes()
    manifest[name] = {'bytes': len(data), 'sha256': sha(data)}
save(out / 'MANIFEST.json', manifest)
print('Active runtime source and evidence checkpoint complete.', flush=True)
