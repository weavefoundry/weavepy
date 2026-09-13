"""Preserve isolated ownership prototypes without claiming a runtime migration."""
from pathlib import Path
import hashlib,json
parent=Path('crates/weavepy-bench/census/2026-09-runtime-metadata')
out=parent/'thin-owner-prototypes';assert not out.exists();out.mkdir()
records=[]
paths=['thin_shared_slice_prototype.rs','thin_shared_slice_prototype_build.txt','thin_shared_slice_prototype_tests.txt','thin_shared_slice_miri_install.txt','thin_shared_slice_miri_rust_src.txt','thin_shared_slice_miri_tests.txt','thin_shared_slice_prototype_report.json','thin_owner_tuple_prototype.rs','thin_owner_tuple_prototype_build.txt','thin_owner_tuple_prototype_tests.txt','thin_owner_tuple_miri_tests.txt','thin_owner_tuple_miri_i686_tests.txt','thin_owner_tuple_prototype_report.json','thin_shared_payload_design.md','thin_object_payload_followup.md']
for group in ('thin-shared-slice-prototype','thin-owner-tuple-prototype'):
 for name in ('Cargo.toml','Cargo.lock','source-index.json'):
  paths.append(group+'/'+name)
for name in paths:
 src=Path('target',name);dst=out/name;dst.parent.mkdir(parents=True,exist_ok=True);dst.write_bytes(src.read_bytes())
 records.append({'path':name,'source':str(src),'bytes':src.stat().st_size,'sha256':hashlib.sha256(src.read_bytes()).hexdigest()})
(out/'REPORT.md').write_text('''# Isolated thin-owner prototypes

These are unapplied, standalone experiments. No WeavePy value layout has been
changed, and no speed, RSS, or Object-size improvement has been measured.
The overall performance goal remains unachieved.

The first prototype stores immutable u8/u32 slice lengths in their allocation
and retains Rust's Arc reference counting and wide Weak handles. Its two
ordinary tests and two strict-provenance Miri tests pass on aarch64-apple-darwin,
covering every length from 0 through 1024 and concurrent weak upgrades/drop.
The observed Miri test duration was 269.76 seconds, not a performance metric.

A current-source audit corrected the earlier design's scope: TupleStorage
has an unsized [Object] tail, so Object::Tuple is ALSO a wide pointer. Shrinking
Str, WStr, and Bytes alone cannot shrink Object. The retained design notes
explicitly record that correction. No layout claim should assume otherwise.

The second prototype generalizes the owner to a header and destructible tail.
Its three ordinary tests and all three Miri tests pass on both aarch64-apple-
darwin and i686-unknown-linux-gnu. They cover exact element/header destruction
at the last strong reference before final weak release, fixed-array unsizing,
unique mutation refusing existing strong or weak aliases, dynamic allocation
layout including empty values, and atomic-header thread handoffs. Observed Miri
test durations were 3.75 and 3.60 seconds, not runtime performance evidence.

Both prototypes retain std::sync::Arc's counts, synchronization, upgrades, and
destruction. A thin strong owner reconstructs metadata only while holding a
live strong reference. Wide weak handles retain their own metadata after the
payload dies. Dynamic construction uses equal-size/equal-alignment payload
conversion; it does not depend on private ArcInner fields. All safety comments,
exact sources, manifests, toolchain installation output, warnings, test logs,
and file hashes are retained. Zero-test doc-test output is not extra coverage.

Nightly 1.100.0 (0fc141305, 2026-09-11) was installed with Miri and matching
Rust sources without changing the default stable toolchain. Miri used
-Zmiri-strict-provenance. These finite checks aren't a soundness proof and do
not test a full VM, CachedHash, UTF-8 string API, C API mirrors, buffer exports,
frozen caches, or Python object populations. Production integration and broad
correctness/performance evaluation remain future work. Build caches, executables,
and installed toolchains remain local and aren't part of this archive.
''')
p=out/'REPORT.md';records.append({'path':p.name,'bytes':p.stat().st_size,'sha256':hashlib.sha256(p.read_bytes()).hexdigest()})
p=out/Path(__file__).name;p.write_bytes(Path(__file__).read_bytes());records.append({'path':p.name,'bytes':p.stat().st_size,'sha256':hashlib.sha256(p.read_bytes()).hexdigest()})
(out/'index.json').write_text(json.dumps(records,indent=2)+'\n')
for row in records:assert hashlib.sha256((out/row['path']).read_bytes()).hexdigest()==row['sha256']
with (parent/'.gitignore').open('a') as f:
 f.write('\n# Standalone ownership prototypes and exact Miri evidence, no runtime claim.\n')
 for p in sorted(out.rglob('*')):
  if p.is_file():f.write('!'+str(p.relative_to(parent))+'\n')
with (parent/'README.md').open('a') as f:
 f.write('\nThe [thin-owner prototypes](thin-owner-prototypes/REPORT.md) preserve standalone\nownership and Miri checks, including a correction that tuple owners also carry\nwide slice metadata. They have not been integrated into WeavePy or benchmarked.\n')
print('Exported',len(records)+1,'files;',sum(p.stat().st_size for p in out.rglob('*') if p.is_file()),'bytes; all hashes verified.')
