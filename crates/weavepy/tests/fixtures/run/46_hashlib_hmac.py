import hashlib
import hmac
import base64
import binascii
import secrets

print("--- hashlib ---")
data = b"hello world"
print("md5:", hashlib.md5(data).hexdigest())
print("sha1:", hashlib.sha1(data).hexdigest())
print("sha256:", hashlib.sha256(data).hexdigest()[:20], "...")
h = hashlib.sha256()
h.update(b"hello ")
h.update(b"world")
print("incremental sha256 matches:", h.hexdigest() == hashlib.sha256(data).hexdigest())

print("--- hmac ---")
mac = hmac.new(b"key", b"hello", "sha256")
print("hmac sha256:", mac.hexdigest()[:20], "...")
print("hmac eq:", hmac.compare_digest("abc", "abc"), hmac.compare_digest("abc", "abd"))

print("--- base64 ---")
encoded = base64.b64encode(b"WeavePy rocks")
print("b64encode:", encoded)
print("b64decode:", base64.b64decode(encoded))
print("b32encode:", base64.b32encode(b"hi"))
print("b16encode:", base64.b16encode(b"hi"))

print("--- binascii ---")
print("hex:", binascii.b2a_hex(b"AB"))
print("unhex:", binascii.a2b_hex(b"4142"))
print("crc32:", binascii.crc32(b"hello"))

print("--- secrets ---")
print("token_hex len:", len(secrets.token_hex(8)))
print("randbelow ok:", 0 <= secrets.randbelow(100) < 100)
