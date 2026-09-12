# Store interned strings once

Unimplemented lead from startup allocation profiles. The per-thread sys intern
pool is HashMap<String, Object>, but every entry is Object::Str. Its separate
owned key duplicates the string allocation and makes each bucket 48 bytes on
this host. A HashSet<Rc<str>> could retain the canonical allocation as the lookup
key itself, using borrowed &str lookups and returning cloned Object::Str handles.
The pool would retain the same canonical objects with the same lifetimes.

The current allocation-stack diagnostic identifies one 212,992-byte intern-table
allocation. This is a live allocation category, not a peak-RSS saving estimate.
Actual process RSS and throughput need measurement; allocator/page effects may
change the result. No savings have been measured for this proposal.

Apply only to sys.rs INTERN_POOL, intern_name, str_is_interned, and sys_intern.
str_is_interned currently creates a temporary String through to_str; direct
borrowed lookup plus Rc::ptr_eq would also avoid that allocation. sys_intern
must retain the first input's identity and all existing error/surrogate behavior.
WStr currently passes through without canonical pooling; do not claim that
existing CPython compatibility gap is fixed by changing plain-string storage.
The unrelated Object::interned_str process-wide pool is a separate future lead.

Validate canonical identity across runtime-created equal Unicode strings,
empty/ASCII/nul/long keys, marshal intern flags/round trips, attributes and
pickle, many interned names, threading/GIL0, and subclass/error behavior. Record
before/after bounded oracles rather than assuming current arity and WStr
behavior matches CPython. No runtime edits while ownership validation/timing runs.
