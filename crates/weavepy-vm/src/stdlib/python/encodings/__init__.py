"""WeavePy frozen subset of the CPython encodings package.

Only the modules WeavePy resolves through codecs.lookup lookup live
here (e.g. idna, punycode). The bulk of the encodings are served
natively, so this package is intentionally not the codec search bootstrap.
"""
