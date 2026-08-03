"""Ecosystem probe: markupsafe — escape correctness and, when the wheel
carries the C `_speedups` extension, its import and agreement with the
pure-Python implementation."""

import markupsafe
from markupsafe import Markup, escape

assert str(escape("<script>alert('x') & \"y\"</script>")) == (
    "&lt;script&gt;alert(&#39;x&#39;) &amp; &#34;y&#34;&lt;/script&gt;"
)

# Markup is html-safe and composes without double-escaping
m = Markup("<b>bold</b>")
assert str(escape(m)) == "<b>bold</b>"
assert str(Markup("<em>%s</em>") % "<unsafe>") == "<em>&lt;unsafe&gt;</em>"

# __html__ protocol
class Widget:
    def __html__(self):
        return "<div>widget</div>"


assert str(escape(Widget())) == "<div>widget</div>"

# the compiled leg: report which implementation is live, and if the
# speedups module is importable make sure it matches the pure-Python one.
# markupsafe >= 3.0 exports only `_escape_inner` (the `escape` wrapper is
# Python-level); it escapes the five HTML specials but does no Markup
# wrapping or __html__ dispatch.
try:
    from markupsafe import _speedups

    assert _speedups._escape_inner("<x> & 'y'") == "&lt;x&gt; &amp; &#39;y&#39;"
    impl = "c-speedups"
except ImportError:
    impl = "pure-python"

print("markupsafe ok", markupsafe.__version__, impl)
