import contextvars

cv = contextvars.ContextVar("cv", default="default")
print("default:", cv.get())

cv.set("outer")
print("outer:", cv.get())


def child():
    print("in child sees:", cv.get())
    cv.set("child-only")
    print("in child set:", cv.get())


ctx = contextvars.copy_context()
ctx.run(child)
print("after run still:", cv.get())

# token reset
tok = cv.set("temp")
print("temp:", cv.get())
cv.reset(tok)
print("reset:", cv.get())
