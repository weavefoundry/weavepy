import io
import logging

stream = io.StringIO()
handler = logging.StreamHandler(stream)
handler.setFormatter(logging.Formatter("%(levelname)s:%(name)s:%(message)s"))

log = logging.getLogger("demo")
log.setLevel(logging.DEBUG)
log.addHandler(handler)
log.propagate = False

log.debug("a debug msg")
log.info("hello %s", "world")
log.warning("watch out")
log.error("bad thing %d", 42)
try:
    raise ValueError("nope")
except ValueError:
    log.exception("caught it")

text = stream.getvalue()
print("DEBUG present:", "DEBUG:demo:a debug msg" in text)
print("INFO present:", "INFO:demo:hello world" in text)
print("WARNING present:", "WARNING:demo:watch out" in text)
print("ERROR present:", "ERROR:demo:bad thing 42" in text)
print("EXC present:", "ValueError: nope" in text)
print("level >=", log.getEffectiveLevel() <= logging.DEBUG)
