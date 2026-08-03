"""Ecosystem probe: certifi — the CA bundle exists, parses as PEM, and
loads into an SSLContext."""

import os
import ssl

import certifi

path = certifi.where()
assert os.path.isfile(path), path

pem = certifi.contents()
assert "BEGIN CERTIFICATE" in pem

# the bundle actually loads as CA material
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
ctx.load_verify_locations(cafile=path)
stats = ctx.cert_store_stats()
assert stats["x509_ca"] > 100, stats

print("certifi ok", certifi.__version__)
