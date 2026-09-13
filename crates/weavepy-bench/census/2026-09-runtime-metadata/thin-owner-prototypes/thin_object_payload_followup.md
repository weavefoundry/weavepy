# Possible broader value-layout experiment, not implemented

Source audit only. Object in crates/weavepy-vm/src/object.rs carries three
wide shared-slice payloads: Str(Arc<str>), WStr(Arc<[u32]>), and Bytes(Arc<[u8]>).
The other enum payloads are scalars or sized shared pointers; Complex is already
behind a shared pointer. sync::Rc is std::sync::Arc, preserving cross-thread
ownership. Verify actual release layouts before reporting a size improvement.

Thin ownership for all three slice payloads might shrink every Object slot,
including locals, stacks, lists, tuples, dictionaries, and GC metadata. Changing
only one of the three cannot establish the desired enum-size reduction. A
simple Arc<Vec<T>> adds storage, an indirection, and potentially an allocation
for each string/byte value; it may lose more than it saves. Any such comparison
needs object populations and string/bytes throughput, not only enum size.

A single-allocation thin representation would need an explicit stored length
and a carefully audited ownership implementation. Do not casually cast between
Arc allocation types: DST layout, alignment, trailing padding, deallocation
layout, pointer provenance, last-reference drop, and weak references must all
match. Prefer an established suitable implementation if available and verified.
Existing Arc/Weak identities, interned names, C-API mirrors, buffer exports,
Unicode/surrogate behavior, pickle bytes, frozen caches, and GIL-free ownership
would need a full migration audit. Construction/cloning/length access overhead
and executable/build costs can offset smaller slots.

No new dependency, representation source, unsafe code, measurement, or layout
claim has been introduced for this lead. Finish the active post-binding call
experiment first. The smaller pin-memo draft is still a separate unapplied
allocation experiment with a much narrower validation surface.

Further source audit: twenty current source/test files (excluding archived
census snapshots) explicitly mention these shared-slice payloads. The C API
strings.rs cache is a concrete liveness dependency: CStrOwner holds Weak<str>
and Weak<[u8]> so stable NUL-terminated pointers survive thread exit while the
original object lives, and dead cache entries can be swept. Replacing owners
with strong references would reintroduce a retention problem. A weak-less thin
owner cannot be substituted without preserving this behavior. The tuple
ownership test also exercises weak upgrades through the string lifetime.

Any custom thin strong owner would still need a compatible weak handle. That
weak handle can remain wide, carrying its own length after the value dies;
reading a destroyed payload to recover weak-pointer metadata is not acceptable.
A proposed one-allocation representation would store a length beside immutable
payload data and must prove identical allocation/deallocation layout, padding,
alignment, provenance, initialization, refcount overflow, and panic behavior.
These are design requirements, not a validated implementation. No dependency
or unsafe source change has been made for this lead.

## Correction from the current tuple-storage audit

TupleStorage defaults its final generic parameter to unsized [Object], so
Object::Tuple(Rc<TupleStorage>) is ALSO a wide shared pointer. The earlier
statement that all non-string/byte payloads are sized was incorrect. Migrating
only Str, WStr, and Bytes cannot shrink Object. Any full layout experiment must
also address the tuple owner and preserve its hash cache, element destructors,
Arc weak ownership, fixed-array and dynamic constructors, and unique mutable
access used by tuple recycling. The standalone u8/u32 prototype tests none of
those tuple behaviors. No Object-size claim has been established.
