"""RFC 0042 WS5 — `email.mime.*` in-process fixture.

The `smtplib` driver builds messages with the `email.mime` package; this
exercises constructing a multipart MIME message, attaching text/binary parts,
serialising it, and re-parsing it. Pure, no network.
"""

from email.mime.multipart import MIMEMultipart
from email.mime.text import MIMEText
from email.mime.base import MIMEBase
from email.mime.application import MIMEApplication
from email import encoders
from email.parser import Parser

msg = MIMEMultipart()
msg["From"] = "alice@example.com"
msg["To"] = "bob@example.com"
msg["Subject"] = "WS5 fixture"

msg.attach(MIMEText("Hello, body text.\n", "plain"))

blob = MIMEBase("application", "octet-stream")
blob.set_payload(b"\x00\x01\x02\x03binary")
encoders.encode_base64(blob)
blob.add_header("Content-Disposition", "attachment", filename="data.bin")
msg.attach(blob)

app = MIMEApplication(b"PDFDATA", "pdf")
msg.attach(app)

assert msg.is_multipart()
parts = msg.get_payload()
assert len(parts) == 3

serialized = msg.as_string()
assert "Subject: WS5 fixture" in serialized
assert "Content-Type: multipart/mixed" in serialized
assert "Content-Transfer-Encoding: base64" in serialized

# Re-parse and confirm structure survives the round-trip.
parsed = Parser().parsestr(serialized)
assert parsed.is_multipart()
assert parsed["From"] == "alice@example.com"
reparts = parsed.get_payload()
assert len(reparts) == 3
assert reparts[0].get_content_type() == "text/plain"
assert reparts[0].get_payload(decode=True) == b"Hello, body text.\n"
assert reparts[1].get_payload(decode=True) == b"\x00\x01\x02\x03binary"

print("WS5 email.mime fixture ok")
