"""RFC 0077 — wide method calls never take the CALL_FUNCTION_EX route.

CPython 3.14's `maybe_optimize_method_call` emits CALL/CALL_KW itself
whenever `argsl + kwdsl + (kwdsl != 0) < STACK_USE_GUIDELINE`, so a
method-flagged LOAD_ATTR is always paired with CALL or CALL_KW. Only a
plain call (PUSH_NULL) can go through `codegen_call_helper`'s ex_call
branch (`nelts + nkwelts*2 > STACK_USE_GUIDELINE`). Pairing the method
load with CALL_FUNCTION_EX would leave the receiver in the self slot,
which CALL_FUNCTION_EX asserts NULL (fastapi's `APIRouter.api_route`
decorator, 2 positionals plus 22 keywords, tripped this).
"""

import dis


def _call_ops(src):
    ns = {}
    exec(src, ns)
    return [
        i.opname
        for i in dis.get_instructions(ns["f"])
        if i.opname.startswith(("CALL", "LOAD_ATTR", "PUSH_NULL"))
    ]


def _src(callee, nargs, nkw):
    args = ", ".join(f"a{i}" for i in range(nargs))
    kws = ", ".join(f"k{i}={i}" for i in range(nkw))
    inner = ", ".join(x for x in (args, kws) if x)
    params = ", ".join(x for x in ("self", "g", args) if x)
    return f"def f({params}):\n    return {callee}({inner})\n"


# Method form: 2 + 22 + 1 = 25 < 30 -> CALL_KW even though 2 + 44 > 30.
assert _call_ops(_src("self.m", 2, 22)) == ["LOAD_ATTR", "CALL_KW"], _call_ops(_src("self.m", 2, 22))
assert _call_ops(_src("self.m", 0, 16)) == ["LOAD_ATTR", "CALL_KW"]
assert _call_ops(_src("self.m", 29, 0)) == ["LOAD_ATTR", "CALL"]
# Over the method guard: plain load, PUSH_NULL, then the helper's shape.
assert _call_ops(_src("self.m", 30, 0)) == ["LOAD_ATTR", "PUSH_NULL", "CALL"]
assert _call_ops(_src("self.m", 0, 29)) == ["LOAD_ATTR", "PUSH_NULL", "CALL_FUNCTION_EX"]
assert _call_ops(_src("self.m", 31, 0)) == [
    "LOAD_ATTR",
    "PUSH_NULL",
    "CALL_INTRINSIC_1",
    "PUSH_NULL",  # the empty kwargs slot
    "CALL_FUNCTION_EX",
]
# Plain callable keeps the ex_call threshold.
assert _call_ops(_src("g", 2, 14)) == ["PUSH_NULL", "CALL_KW"]
assert _call_ops(_src("g", 2, 15)) == ["PUSH_NULL", "CALL_FUNCTION_EX"]
assert _call_ops(_src("g", 0, 16)) == ["PUSH_NULL", "CALL_FUNCTION_EX"]


# And the call actually runs with the receiver bound.
class C:
    def m(self, *a, **k):
        return (self, a, len(k))


ns = {}
exec(_src("self.m", 2, 22), ns)
c = C()
got = ns["f"](c, None, 1, 2)
assert got == (c, (1, 2), 22), got
ns = {}
exec(_src("self.m", 0, 29), ns)
assert ns["f"](c, None) == (c, (), 29)

print("rfc0077-wide-method-call: ok")
