"""Verify original archive hashes, member hashes, and safe paths before the Git checkpoint."""
from pathlib import Path
import json
import tarfile
from prepare_performance_checkpoint import ROOT, BASE, sha, save

selection = json.loads((ROOT / 'target/performance-checkpoint-selection.json').read_text())
manifests = [ROOT / name for name in selection['paths'] if name.endswith('/MANIFEST.json') or name.endswith('/CHECKPOINT-MANIFEST.json')]
manifests += sorted((BASE / 'native-scratch-byte-budget').glob('MANIFEST.json'))
results = []
generated = []
for path in manifests:
    manifest = json.loads(path.read_text())
    archives = manifest.get('archives', {})
    if 'archive' in manifest:
        archives = {'checkpoint-evidence.tar.gz': manifest['archive']}
    for name, expected in archives.items():
        archive_path = path.parent / name
        assert archive_path.stat().st_size == expected['bytes'], archive_path
        assert sha(archive_path.read_bytes()) == expected['sha256'], archive_path
        seen = set()
        with tarfile.open(archive_path, 'r:gz') as archive:
            for info in archive:
                assert info.isfile() and not info.name.startswith('/') and '..' not in Path(info.name).parts, info.name
                assert info.name not in seen, info.name
                seen.add(info.name)
                if '__pycache__' in Path(info.name).parts or Path(info.name).suffix in {'.pyc', '.pyo', '.rlib', '.rmeta', '.dylib', '.so', '.index'}:
                    generated.append({'archive': str(archive_path.relative_to(ROOT)), 'member': info.name})
                data = archive.extractfile(info).read()
                assert data[:4] not in {b'\xcf\xfa\xed\xfe', b'\xfe\xed\xfa\xcf', b'\x7fELF'}, info.name
                assert len(data) == expected['members'][info.name]['bytes'], info.name
                assert sha(data) == expected['members'][info.name]['sha256'], info.name
        assert seen == set(expected['members']), archive_path
        results.append({'path': str(archive_path.relative_to(ROOT)), 'sha256': expected['sha256'], 'members': len(seen)})
    for name in ['REPORT.md', 'changes.patch', 'EXPORT.txt']:
        expected = manifest.get(name)
        if isinstance(expected, dict) and 'sha256' in expected:
            assert sha((path.parent / name).read_bytes()) == expected['sha256'], (path.parent / name)
    print(path.parent.name, 'verified', flush=True)
save(ROOT / 'target/performance-checkpoint-archive-verification.json', {'status': 'identities_verified', 'archives': results, 'members_verified': sum(x['members'] for x in results), 'generated_members_to_exclude': generated})
print('All selected archive and member identities verified;', len(generated), 'generated members to exclude.', flush=True)
