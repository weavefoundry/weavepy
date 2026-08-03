"""Ecosystem probe: cryptography (abi3 Rust wheel) — Fernet round-trip,
hashing, and X.509 self-signed certificate generation + parse."""

import datetime

import cryptography
from cryptography import x509
from cryptography.fernet import Fernet, InvalidToken
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.x509.oid import NameOID

# --- Fernet symmetric round-trip ------------------------------------------
key = Fernet.generate_key()
f = Fernet(key)
token = f.encrypt(b"weavepy probe payload")
assert f.decrypt(token) == b"weavepy probe payload"
try:
    Fernet(Fernet.generate_key()).decrypt(token)
except InvalidToken:
    pass
else:
    raise AssertionError("InvalidToken not raised for wrong key")

# --- hashes ----------------------------------------------------------------
digest = hashes.Hash(hashes.SHA256())
digest.update(b"abc")
assert digest.finalize().hex() == (
    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
)

# --- X.509: generate a self-signed cert, serialize, parse back -------------
private_key = ec.generate_private_key(ec.SECP256R1())
name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "weavepy.test")])
now = datetime.datetime.now(datetime.timezone.utc)
cert = (
    x509.CertificateBuilder()
    .subject_name(name)
    .issuer_name(name)
    .public_key(private_key.public_key())
    .serial_number(x509.random_serial_number())
    .not_valid_before(now)
    .not_valid_after(now + datetime.timedelta(days=1))
    .add_extension(
        x509.SubjectAlternativeName([x509.DNSName("weavepy.test")]), critical=False
    )
    .sign(private_key, hashes.SHA256())
)
pem = cert.public_bytes(serialization.Encoding.PEM)
parsed = x509.load_pem_x509_certificate(pem)
cn = parsed.subject.get_attributes_for_oid(NameOID.COMMON_NAME)[0].value
assert cn == "weavepy.test", cn
sans = parsed.extensions.get_extension_for_class(
    x509.SubjectAlternativeName
).value.get_values_for_type(x509.DNSName)
assert sans == ["weavepy.test"], sans

print("cryptography ok", cryptography.__version__)
