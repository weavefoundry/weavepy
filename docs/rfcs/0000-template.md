# RFC 0000: <Title>

- **Status**: Draft
- **Authors**: <your name(s)>
- **Created**: <YYYY-MM-DD>
- **Tracking issue**: <link or "TBD">

## Summary

One paragraph explanation of the proposal.

## Motivation

Why are we doing this? What problem are we solving? Who benefits and how?
What is the cost of inaction?

## CPython reference

WeavePy's compatibility goal makes this section mandatory. Cite the
relevant CPython documentation, PEPs, source files, or test modules that
define the behavior we are trying to match (or deliberately diverge from).

## Detailed design

The bulk of the RFC. Describe the design in enough detail that an engineer
familiar with the codebase could implement it. Include:

- Affected crates and the crate-level API changes.
- Bytecode / opcode changes, if any.
- Object model / runtime impact.
- C-API or FFI implications.
- Performance assumptions and how we plan to validate them.

## Drawbacks

Why might we *not* do this? Code complexity, performance regressions,
compatibility risk, maintenance burden — be honest.

## Alternatives

What other approaches were considered? What did each give up? Why is the
chosen design preferred?

## Prior art

Has another Python implementation (CPython, PyPy, GraalPy, RustPython,
Pyston) tackled this problem? What did they learn? Cite specifics.

## Unresolved questions

What aspects of the design are still open? What needs to be answered
before this can be marked Accepted?

## Future work

Follow-on changes that build on this RFC but are out of scope here.
