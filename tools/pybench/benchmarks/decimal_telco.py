"""Decimal arithmetic with quantize and rounding, telco-billing style."""

from decimal import ROUND_DOWN, ROUND_HALF_EVEN, Decimal, getcontext

WORK = 60000


def bench(n):
    getcontext().rounding = ROUND_DOWN
    rates = [Decimal("0.0013"), Decimal("0.00894")]
    twodig = Decimal("0.01")
    disttax = Decimal("0.0341")
    basictax = Decimal("0.0675")
    sumt = sumb = sumd = Decimal(0)
    for i in range(n):
        calltype = i & 1
        r = rates[calltype]
        nsecs = Decimal((i * 7919) % 3600 + 1)
        p = (r * nsecs).quantize(twodig, rounding=ROUND_HALF_EVEN)
        b = (p * basictax).quantize(twodig)
        sumb += b
        t = p + b
        if calltype:
            d = (p * disttax).quantize(twodig)
            sumd += d
            t += d
        sumt += t
    return (str(sumt), str(sumb), str(sumd))
