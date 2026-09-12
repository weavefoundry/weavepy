# Integer-division guard regression

The scalar-result guard passed its original callback, overflow, and fallback
checks. Extending those checks to exact floating-point results exposed the
existing native integer-division path rounding operands before division.
The new case fails in the debug JIT and this release, while CPython passes.
The test trace confirms that exact_division compiled. The release binary,
its lowering source, regression, and checksums are retained locally.
This candidate is not validated and has no performance claim.
