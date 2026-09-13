# Startup memory region and allocation diagnostics

These are **diagnostics**, not replacements for the uninstrumented benchmark
census. Candidate e5077bba remains experimental and the CPython-wide performance
goal remains unachieved. No runtime source was changed for these inspections.

The region probe imports os/time, announces its PID and a checksum, and sleeps
while the parent records ps RSS and vmmap -summary. One unmeasured seed process
precedes each inspection. CPython, retained 539c, and e507 all announce the right
value; all vmmap calls succeed and frozen caches remain unchanged after seeding.

| Startup snapshot | CPython | Retained 539c | Candidate e507 |
| --- | ---: | ---: | ---: |
| ps RSS, bytes | 15,073,280 | 29,310,976 | 29,605,888 |
| vmmap physical footprint, as reported | 6,672K | 10.7M | 11.0M |
| malloc-zone allocated bytes, as reported | 2,040K | 6,346K | 6,346K |
| malloc-zone fragmentation, as reported | 488K | 2,374K | 2,662K |

Do not equate vmmap's aggregate resident-region totals, which include shared
mappings, with ps RSS or private memory. CPython also uses arenas outside its
malloc-zone table, so malloc block counts are not Python object counts. The
WeavePy stack reservation is about 1 GiB of virtual address space, with only
352-368 KiB resident here. It is not a 1 GiB resident allocation. The snapshots
motivate investigating both executable/code mappings and heap metadata; they
do not give an exact additive attribution of the RSS difference.

Separate MallocStackLogging traces capture e507's live allocation sites. The
first uses a fresh frozen cache and includes cache-generation/compiler costs.
The second reuses that cache and verifies it remains unchanged. Instrumentation
changes memory conditions. These traces measure neither churn nor performance,
and their reported allocation sizes are not verified malloc request sizes.

The cold trace has 59,845 malloc blocks totaling 9,203,104 reported bytes. The
warm trace has 54,671 malloc blocks totaling 6,909,760 reported bytes. Reserved
stacks and other non-malloc records are classified separately and retained.
Each malloc block is attributed once, preventing overlapping-stack totals.
The first summary matches WeavePy markers in mangled symbols and can therefore
land on generic allocator instantiations. The second uses llvm-cxxfilt and
selects the first actual WeavePy implementation; both analyses are preserved.

The largest cold site is cpython_code::encode at about 1.66 million reported
bytes. Warm sites include decode_full (419,984 bytes at its largest site),
MarshalReader::read_code (355,328), decode_instructions (277,584), interned name
storage (212,992), and CacheTable::allocate (207,760). Other allocation sites,
including strings, frame/function construction, and GC registration, are fully
listed in the compressed JSON summaries. These are leads for source inspection,
not claims that the whole allocation can be removed or that removing it would
save the same amount of RSS.

The archive contains complete region outputs, capture reports, all aggregated
allocation-site records, methods, and source/executable identities. Full raw
stack histories remain local in the two paths listed in local-raw-traces.json;
their compressed and uncompressed hashes and sizes are recorded. They are not
backed up by this selected Git archive. No raw trace or negative observation
was deleted. The index verifies every selected artifact. No universal, energy,
controlled build-time, portability, or free-threaded advantage is established.
