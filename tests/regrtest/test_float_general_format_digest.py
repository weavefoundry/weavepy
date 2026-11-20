"""`g`-style float formatting matches CPython over many values.

`format(x, 'g')`, `'%g'`, the type-omitted form with a precision, and
their flags lay out the digits of one scientific formatting; the output
for 6,000 values and 15 specs hashes to CPython 3.14's digest.
"""

import hashlib
import random

random.seed(7)
vals = [0.0, -0.0, 1.0, 100.0, 1e16, 1e-5, 0.0001, 123456789.0, 1234.5678, 9.9999,
        0.5, 1e300, 5e-324, 2.5, 99999.5, 1e5, 999999.5]
vals += [random.uniform(-1e6, 1e6) for _ in range(3000)]
# (Scaled by parsed powers of ten, not `10 ** x`: the C library's `pow`
# differs in the last bit between platforms.)
vals += [random.uniform(1, 10) * float("1e%d" % random.randint(-20, 20)) for _ in range(3000)]
specs = ["g", "G", ".3g", ".1g", ".0g", ".10g", ".17g", "#g", "#.3g", "", ".4", ".1",
         "12.4g", "+.5g", "%"]
h = hashlib.sha256()
for v in vals:
    for s in specs:
        h.update(format(v, s).encode())
    h.update(("%g|%.3g|%G" % (v, v, v)).encode())
assert h.hexdigest() == "ad57386050c841d9fcbf14076a4bab13a40da57e7b3b48f86d3a9d7d4772ecf0", h.hexdigest()
print("float general format digest: ok")
