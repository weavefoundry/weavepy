code = compile("x = 1 + 1", "<test>", "exec")
print("code is set:", code is not None)
ns = {}
exec(code, ns)
print("x:", ns["x"])

exec("y = 42", ns)
print("y:", ns["y"])

result = eval("3 * 5", ns)
print("eval result:", result)

# eval with locals via globals
result = eval("x + y + 10", ns)
print("eval expr:", result)


# closures inside exec
ns2 = {}
exec("def f(a): return a * 2\nv = f(7)", ns2)
print("ns2 v:", ns2["v"])
