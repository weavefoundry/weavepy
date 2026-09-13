# Single-allocation thin string and byte owners, unapplied design

The current Object's three wide shared-slice variants hold two-word owners.
A separate design could move the slice length into the shared allocation so
each strong owner takes one word. All three variants must migrate before an
Object-size improvement is possible. This isn't implemented or measured.

Prefer retaining std::sync::Arc's reference counts and weak handles over writing
a new reference-counting algorithm. A potential representation is a repr(C)
Payload<T> with a usize length followed by a [T] tail. Restrict T to sealed u8
and u32 element types, which are Copy, have no destructor, and have alignment
no greater than usize on supported targets. A strong thin owner stores only a
NonNull pointer to the payload header and PhantomData for ownership. While it
owns a strong reference, its initialized length permits reconstruction of the
original DST pointer. Clone/drop/downgrade can then use Arc's existing operations
through carefully scoped ManuallyDrop wrappers. A weak handle can retain the
ordinary wide Weak<Payload<T>>, so it never reads a destroyed payload to recover
metadata. A successful upgrade already owns a live Arc and can make it thin.

Construction needs a documented same-size, same-alignment Arc conversion, not
assumptions about private ArcInner fields. One candidate allocates enough
MaybeUninit<usize> words for the length header and element tail, rounded up to
usize alignment. Initialize the length and all elements through the allocation's
unique mutable pointer, retain padding as uninitialized, then convert the raw
Arc slice pointer to Payload<T> with tail metadata equal to the element count.
Its total padded payload layout and alignment must exactly match the original
word slice for every length, including empty values and odd u8/u32 lengths.
Check overflow and allocation limits before allocating. A Rust language/library
contract audit must establish that this conversion is supported; numerical
layout equality alone isn't sufficient evidence of soundness.

No strong owner can read the header after relinquishing its last Arc reference.
Raw Arc reconstruction must create exactly one accounted owner, and temporary
borrowed Arc views must never decrement its count. Weak downgrade/upgrade/drop
remain with std, preserving atomic synchronization, overflow behavior, and
cross-thread lifetime. Send/Sync implementations, if needed for NonNull, require
the same immutable payload bounds as Arc. Mutable payload access is not exposed.
String views require a separate UTF-8 invariant; wide Unicode keeps u32 data.
No public from_raw constructor is needed for the VM migration.

Before applying, use a standalone prototype and memory-safety checks covering
all short lengths, alignment, large sizes, clone/weak/drop interleavings, failed
and successful upgrades, concurrent upgrade/drop, UTF-8 and arbitrary byte/u32
payloads, and exact last-owner lifetime. Preserve existing intern identities,
C API weak owners and stable NUL-terminated mirrors, memoryview ownership,
marshal/pickle bytes, frozen cache values, and GIL-disabled thread transfers.
Measure full object populations and string/bytes throughput, construction,
clone, length, peak RSS, startup, and executable costs. Header metadata adds a
word per allocation; slot savings depend on sharing and object populations.
No safety, size, memory, or speed claim is established by this design note.

## Correction from the current tuple-storage audit

TupleStorage defaults its final generic parameter to unsized [Object], so
Object::Tuple(Rc<TupleStorage>) is ALSO a wide shared pointer. The earlier
statement that all non-string/byte payloads are sized was incorrect. Migrating
only Str, WStr, and Bytes cannot shrink Object. Any full layout experiment must
also address the tuple owner and preserve its hash cache, element destructors,
Arc weak ownership, fixed-array and dynamic constructors, and unique mutable
access used by tuple recycling. The standalone u8/u32 prototype tests none of
those tuple behaviors. No Object-size claim has been established.
