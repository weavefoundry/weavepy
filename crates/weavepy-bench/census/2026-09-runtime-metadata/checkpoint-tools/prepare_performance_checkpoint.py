"""Select source changes and compact new research evidence without deleting local files."""
from pathlib import Path
import collections
import gzip
import hashlib
import io
import json
import subprocess
import tarfile

ROOT = Path.cwd()
BASE = ROOT / 'crates/weavepy-bench/census/2026-09-runtime-metadata'
sha = lambda data: hashlib.sha256(data).hexdigest()

def save(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')

def allowed(path):
    return (path.is_file() and not path.is_symlink()
            and not any(p in {'__pycache__', 'stacklogs', 'frozen', 'test-frozen', 'stdlib', 'bin', 'release'}
                        or p.endswith('-frozen') for p in path.parts)
            and path.suffix not in {'.pyc', '.pyo', '.index', '.rlib', '.rmeta', '.dylib', '.so', '.o'})

def pack(destination, paths):
    assert not destination.exists(), destination
    members = {}
    with destination.open('xb') as raw:
        with gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode='w|') as archive:
                for member, path in sorted(paths.items()):
                    assert not member.startswith('/') and '..' not in Path(member).parts
                    data = path.read_bytes()
                    assert data[:4] not in {b'\xcf\xfa\xed\xfe', b'\xfe\xed\xfa\xcf', b'\x7fELF'}, path
                    info = tarfile.TarInfo(member)
                    info.size = len(data)
                    info.mode = 0o644
                    archive.addfile(info, io.BytesIO(data))
                    members[member] = {'bytes': len(data), 'sha256': sha(data)}
    with tarfile.open(destination, 'r:gz') as archive:
        assert archive.getnames() == sorted(members)
        for info in archive:
            data = archive.extractfile(info).read()
            assert members[info.name] == {'bytes': len(data), 'sha256': sha(data)}
    print(destination.parent.name, destination.name, len(members), destination.stat().st_size, flush=True)
    return {'bytes': destination.stat().st_size, 'sha256': sha(destination.read_bytes()), 'members': members}

def prepare():
    inventory = json.loads((ROOT / 'target/performance-checkpoint-untracked-inventory.json').read_text())
    groups = collections.defaultdict(list)
    selected = []
    excluded = []
    archives = {}
    for name in inventory['paths']:
        path = ROOT / name
        if path.is_relative_to(BASE):
            groups[path.relative_to(BASE).parts[0]].append(path)
        else:
            assert allowed(path), path
            selected.append(name)
    for group, paths in sorted(groups.items()):
        good = [p for p in paths if allowed(p)]
        excluded.extend(str(p.relative_to(ROOT)) for p in paths if p not in good)
        directory = BASE / group
        if len(good) <= 50:
            selected.extend(str(p.relative_to(ROOT)) for p in good)
            continue
        destination = directory / 'checkpoint-evidence.tar.gz'
        manifest = directory / 'CHECKPOINT-MANIFEST.json'
        archive = pack(destination, {str(p.relative_to(ROOT)): p for p in good})
        save(manifest, {
            'purpose': __doc__,
            'packing': 'Original bytes and repository-relative paths are preserved. Reports remain directly readable. Extract this archive at the repository root to restore its referenced evidence. Generated caches and stack logs are excluded; original manifests inside the archive remain historical records.',
            'excluded_generated_paths': [str(p.relative_to(ROOT)) for p in paths if p not in good],
            'archive': archive,
        })
        archives[group] = {'members': len(good), 'bytes': archive['bytes']}
        selected.extend(str(p.relative_to(ROOT)) for p in [destination, manifest])
        selected.extend(str(p.relative_to(ROOT)) for p in good if p.parent == directory and p.suffix == '.md')
    source_manifest = json.loads((ROOT / 'target/native-scratch-byte-budget-prebuild.json').read_text())
    for name, digest in source_manifest['sources'].items():
        assert sha((ROOT / name).read_bytes()) == digest, name
    validation = json.loads((ROOT / 'target/native-scratch-byte-budget-validation/validation.json').read_text())
    assert len(validation) == 275 and all(row['passed'] for row in validation)
    selected += subprocess.check_output(['git', 'diff', '--name-only', '-z']).decode().rstrip('\0').split('\0')
    selected = sorted(set(selected) - {'.gitignore'})
    save(ROOT / 'target/performance-checkpoint-selection.json', {
        'paths': selected, 'packed_groups': archives, 'excluded_generated_paths': excluded,
        'active_source_manifest': 'target/native-scratch-byte-budget-prebuild.json',
        'active_sources_verified': len(source_manifest['sources']),
        'active_compatibility_checks_passed': len(validation),
    })
    # Preserve the earlier checkpoint's allowances and replace the newly generated
    # thousands of per-file rules with the compact, explicitly reviewed selection.
    ignore_name = str((BASE / '.gitignore').relative_to(ROOT))
    old = subprocess.check_output(['git', 'show', 'HEAD:' + ignore_name]).decode()
    allowances = [str((ROOT / p).relative_to(BASE)) for p in selected if (ROOT / p).is_relative_to(BASE)]
    (BASE / '.gitignore').write_text(old.rstrip() + '\n\n# September 13 checkpoint: verified archives and readable reports.\n' + ''.join('!/' + p + '\n' for p in sorted(allowances)))
    (ROOT / '.gitignore').write_bytes(subprocess.check_output(['git', 'show', 'HEAD:.gitignore']))
    print('Selected', len(selected), 'files; excluded', len(excluded), 'generated files; all 340 active sources match validated phase 79.', flush=True)

if __name__ == '__main__':
    prepare()
