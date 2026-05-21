import email
from email.message import EmailMessage
from http.cookies import SimpleCookie

print("--- email parse ---")
raw = """From: alice@example.com
To: bob@example.com
Subject: Hello

Hi Bob,

Just a quick test.
"""
m = email.message_from_string(raw)
print("from:", m["From"])
print("to:", m["To"])
print("subject:", m["Subject"])
print("body starts:", m.get_payload().splitlines()[0])

print("--- email build ---")
em = EmailMessage()
em["From"] = "weave@example.com"
em["To"] = "you@example.com"
em["Subject"] = "Greetings"
em.set_content("Body here")
s = str(em)
print("From in str:", "From: weave@example.com" in s)
print("Body in str:", "Body here" in s)

print("--- cookies ---")
c = SimpleCookie()
c["session"] = "abc123"
c["session"]["path"] = "/"
c["theme"] = "dark"
out = c.output(sep="; ")
print("contains session:", "session=abc123" in out)
print("contains theme:", "theme=dark" in out)

c2 = SimpleCookie()
c2.load("a=1; b=two")
print("parsed a:", c2["a"].value)
print("parsed b:", c2["b"].value)
