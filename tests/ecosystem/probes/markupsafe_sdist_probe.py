"""RFC 0062 WS2 — prove the sdist-built markupsafe is the *compiled* one.

The row installs `markupsafe==3.0.3` with `no_binary`, so pip built the
wheel from source through `_pep517` + setuptools + cc. markupsafe 3.0.3
exports `escape` from `__init__.py` and takes the hot path from
`_escape_inner`, imported from the C `markupsafe._speedups` module with a
silent pure-Python `._native` fallback — this probe fails unless the C
module both built *and* is the implementation actually wired in.
"""

import markupsafe
from markupsafe import _speedups  # ImportError = the extension didn't build

assert markupsafe._escape_inner is _speedups._escape_inner, (
    "markupsafe fell back to the pure-Python _native implementation: "
    f"{markupsafe._escape_inner!r}"
)

m = markupsafe.escape("<script>\"&'</script>")
assert str(m) == "&lt;script&gt;&#34;&amp;&#39;&lt;/script&gt;", str(m)
assert isinstance(m, markupsafe.Markup)
# Already-escaped Markup must survive a second trip through the C path.
assert markupsafe.escape(m) == m

# __html__ objects route through escape() untouched.
class Widget:
    def __html__(self):
        return "<b>safe</b>"

assert str(markupsafe.escape(Widget())) == "<b>safe</b>"

print("markupsafe sdist probe ok:", _speedups.__file__)
