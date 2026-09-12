# Wide range iteration requires bounded subprocess probes

Source inspection while preparing the arithmetic sum path found that
PyIterator::RangeHuge advances with unchecked `current += step`. For a valid
range such as range(-2**127, 2**127-1, 2**124), its final advance crosses i128::MAX.
The release can wrap back to the starting point and keep summing indefinitely;
this hasn't been executed yet. Don't run the whole new range oracle in one
unbounded process. Isolate risky cases with strict timeouts and retain any
failures. The drafted formula itself uses checked arithmetic/full precision.

RangeHuge remaining()/length hints also perform unchecked span/rounding
arithmetic. Do not hide an iterator bug by only bypassing iteration for sum.
A complete follow-up should preserve wide iterator exhaustion and length hints,
including reverse/skip/reduce/state paths, before making a general range
correctness claim. A checked final advance can exhaust safely when it crosses
the representable endpoint, but review observable iterator reduction state.
Alternatively select RangeBig when wide intermediate arithmetic cannot be
represented, preserving the existing full-precision cursor implementation.
All ordinary inline-i64 range performance paths should stay unchanged.
No iterator fix or arithmetic-sum implementation has been applied yet.
