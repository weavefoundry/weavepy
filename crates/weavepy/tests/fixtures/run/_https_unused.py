"""Network-dependent HTTPS fixture — not run by default because the
test runner can't rely on internet access. Manually exercise via:

    cargo run -p weavepy-cli -- crates/weavepy/tests/fixtures/run/_https_unused.py
"""

import urllib.request

resp = urllib.request.urlopen("https://example.com/")
print("status:", resp.status)
data = resp.read()
print("len-class:", "small" if len(data) < 2000 else "large")
print("starts-html:", data.lstrip().startswith(b"<!doctype html"))
