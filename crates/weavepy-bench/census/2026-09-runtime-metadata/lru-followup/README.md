# Native cache follow-up diagnostics

These untimed diagnostics identify two existing differences from CPython:

- A positional string matching the native wrapper's keyword marker can
  collide with a keyword call and return the wrong cached result.
- A comparison error after the wrapped function has run invokes equality
  twice and leaves an erroneous cache entry. The lookup argument order also
  differs from CPython. Before tuple caching, the error was swallowed; the
  tuple-cache release surfaces it but still leaves the entry.

The source, reference output, both WeavePy outputs, and binary checksums
are retained here. These cases are outside the completed 187-check census
and remain pending fixes; that count doesn't imply universal compatibility.
