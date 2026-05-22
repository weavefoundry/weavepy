import traceback
import warnings


def boom():
    raise ValueError("hello")


try:
    boom()
except ValueError:
    text = "".join(traceback.format_exc())
    print("has 'ValueError: hello':", "ValueError: hello" in text)
    print("has 'boom':", "boom" in text)


def chained():
    try:
        boom()
    except ValueError as e:
        raise TypeError("wrapped") from e


try:
    chained()
except TypeError:
    text = "".join(traceback.format_exc())
    print("has 'direct cause':", "direct cause" in text)
    print("has 'TypeError: wrapped':", "TypeError: wrapped" in text)


# TracebackException
try:
    boom()
except Exception as e:
    te = traceback.TracebackException.from_exception(e)
    out = "".join(te.format())
    print("TE has 'ValueError':", "ValueError: hello" in out)


# warnings
with warnings.catch_warnings(record=True) as log:
    warnings.simplefilter("always")
    warnings.warn("first warning", UserWarning)
    warnings.warn("second", DeprecationWarning)
    print("warnings captured:", len(log))
    print("first category:", log[0].category.__name__)
    print("first msg:", str(log[0].message))
