"""Ecosystem probe: six — moves re-exports and add_metaclass."""

import six

# six.moves resolves lazily against the 3.x stdlib.
from six.moves import range as smart_range

assert list(smart_range(3)) == [0, 1, 2]

from six.moves.urllib.parse import urlparse

assert urlparse("https://example.org/p?q=1").netloc == "example.org"

# add_metaclass round-trip.
class Meta(type):
    def __new__(mcls, name, bases, ns):
        ns["stamped"] = True
        return super().__new__(mcls, name, bases, ns)


@six.add_metaclass(Meta)
class Widget(object):
    pass


assert Widget.stamped is True
assert isinstance(Widget, Meta)

assert six.PY3
assert six.text_type is str
assert six.ensure_str(b"abc") == "abc"

print("six ok", six.__version__)
