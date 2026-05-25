"""Basic statistics functions — WeavePy port of CPython's
``statistics``.

Implements the parts of the module most code reaches for: central
tendency (``mean``, ``median``, ``mode``, ``geometric_mean``,
``harmonic_mean``), variance / standard deviation, and the
``correlation`` / ``linear_regression`` helpers. We don't (yet)
ship :class:`statistics.NormalDist`.
"""

import math
from fractions import Fraction
from decimal import Decimal


class StatisticsError(ValueError):
    pass


def _sum(data):
    s = Fraction(0)
    count = 0
    for x in data:
        count += 1
        if isinstance(x, (int, Fraction)):
            s += Fraction(x)
        elif isinstance(x, float):
            s += Fraction(x)
        elif isinstance(x, Decimal):
            s += Fraction(int(x * 10 ** 30), 10 ** 30)
        else:
            raise TypeError("can't take statistics of %r" % type(x).__name__)
    return s, count


def mean(data):
    if iter(data) is data:
        data = list(data)
    n = len(data)
    if n < 1:
        raise StatisticsError("mean requires at least one data point")
    s, _ = _sum(data)
    return float(s) / n


def fmean(data, weights=None):
    if weights is None:
        if iter(data) is data:
            data = list(data)
        n = len(data)
        if n < 1:
            raise StatisticsError("fmean requires at least one data point")
        return sum(float(x) for x in data) / n
    num = 0.0
    den = 0.0
    for v, w in zip(data, weights):
        num += float(v) * float(w)
        den += float(w)
    if den == 0:
        raise StatisticsError("sum of weights must be non-zero")
    return num / den


def geometric_mean(data):
    if iter(data) is data:
        data = list(data)
    if not data:
        raise StatisticsError("geometric_mean requires at least one data point")
    prod = 1.0
    for x in data:
        if x < 0:
            raise StatisticsError("geometric_mean requires positive values")
        prod *= float(x)
    return prod ** (1.0 / len(data))


def harmonic_mean(data, weights=None):
    if iter(data) is data:
        data = list(data)
    n = len(data)
    if n < 1:
        raise StatisticsError("harmonic_mean requires at least one data point")
    if weights is None:
        s = 0.0
        for x in data:
            if x <= 0:
                if x == 0:
                    return 0.0
                raise StatisticsError("harmonic_mean requires non-negative values")
            s += 1.0 / x
        return n / s
    raise NotImplementedError("weighted harmonic_mean not implemented")


def median(data):
    data = sorted(data)
    n = len(data)
    if n == 0:
        raise StatisticsError("no median for empty data")
    if n % 2 == 1:
        return data[n // 2]
    return (data[n // 2 - 1] + data[n // 2]) / 2


def median_low(data):
    data = sorted(data)
    n = len(data)
    if n == 0:
        raise StatisticsError("no median for empty data")
    return data[(n - 1) // 2]


def median_high(data):
    data = sorted(data)
    n = len(data)
    if n == 0:
        raise StatisticsError("no median for empty data")
    return data[n // 2]


def median_grouped(data, interval=1):
    return median(data)


def mode(data):
    counts = {}
    first = None
    for x in data:
        counts[x] = counts.get(x, 0) + 1
        if first is None:
            first = x
    if not counts:
        raise StatisticsError("mode requires at least one data point")
    return max(counts, key=lambda k: counts[k])


def multimode(data):
    counts = {}
    for x in data:
        counts[x] = counts.get(x, 0) + 1
    if not counts:
        return []
    maxcount = max(counts.values())
    return [k for k, v in counts.items() if v == maxcount]


def variance(data, xbar=None):
    if iter(data) is data:
        data = list(data)
    n = len(data)
    if n < 2:
        raise StatisticsError("variance requires at least two data points")
    if xbar is None:
        xbar = mean(data)
    ss = sum((float(x) - xbar) ** 2 for x in data)
    return ss / (n - 1)


def pvariance(data, mu=None):
    if iter(data) is data:
        data = list(data)
    n = len(data)
    if n < 1:
        raise StatisticsError("pvariance requires at least one data point")
    if mu is None:
        mu = mean(data)
    ss = sum((float(x) - mu) ** 2 for x in data)
    return ss / n


def stdev(data, xbar=None):
    return math.sqrt(variance(data, xbar))


def pstdev(data, mu=None):
    return math.sqrt(pvariance(data, mu))


def correlation(x, y):
    x = list(x)
    y = list(y)
    n = len(x)
    if n != len(y):
        raise StatisticsError("correlation requires equal-length sequences")
    if n < 2:
        raise StatisticsError("correlation requires at least two data points")
    mx = mean(x)
    my = mean(y)
    sxy = sum((xi - mx) * (yi - my) for xi, yi in zip(x, y))
    sxx = sum((xi - mx) ** 2 for xi in x)
    syy = sum((yi - my) ** 2 for yi in y)
    den = (sxx * syy) ** 0.5
    if den == 0:
        raise StatisticsError("at least one of the inputs is constant")
    return sxy / den


def covariance(x, y):
    x = list(x)
    y = list(y)
    n = len(x)
    if n != len(y):
        raise StatisticsError("covariance requires equal-length sequences")
    if n < 2:
        raise StatisticsError("covariance requires at least two data points")
    mx = mean(x)
    my = mean(y)
    return sum((xi - mx) * (yi - my) for xi, yi in zip(x, y)) / (n - 1)


def linear_regression(x, y, *, proportional=False):
    x = list(x)
    y = list(y)
    n = len(x)
    if n != len(y):
        raise StatisticsError("linear_regression requires equal-length sequences")
    if n < 2:
        raise StatisticsError("linear_regression requires at least two data points")
    if proportional:
        sxy = sum(xi * yi for xi, yi in zip(x, y))
        sxx = sum(xi * xi for xi in x)
        if sxx == 0:
            raise StatisticsError("x is constant")
        slope = sxy / sxx
        return (slope, 0.0)
    mx = mean(x)
    my = mean(y)
    sxy = sum((xi - mx) * (yi - my) for xi, yi in zip(x, y))
    sxx = sum((xi - mx) ** 2 for xi in x)
    if sxx == 0:
        raise StatisticsError("x is constant")
    slope = sxy / sxx
    intercept = my - slope * mx
    return (slope, intercept)


def quantiles(data, *, n=4, method="exclusive"):
    data = sorted(data)
    ld = len(data)
    if ld < 2:
        raise StatisticsError("must have at least two data points")
    if n < 1:
        raise StatisticsError("n must be at least 1")
    if method == "exclusive":
        m = ld + 1
    elif method == "inclusive":
        m = ld - 1
    else:
        raise ValueError("Unknown method: %r" % method)
    result = []
    for i in range(1, n):
        j, delta = divmod(i * m, n)
        if 0 <= j and j + 1 < ld:
            interpolated = (data[j] * (n - delta) + data[j + 1] * delta) / n
        elif j + 1 >= ld:
            interpolated = data[-1]
        else:
            interpolated = data[0]
        result.append(interpolated)
    return result


__all__ = [
    "StatisticsError",
    "mean", "fmean", "geometric_mean", "harmonic_mean",
    "median", "median_low", "median_high", "median_grouped",
    "mode", "multimode",
    "variance", "pvariance", "stdev", "pstdev",
    "correlation", "covariance", "linear_regression",
    "quantiles",
]
