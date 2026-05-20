def make_adder(x):
    def adder(y):
        return x + y
    return adder

add5 = make_adder(5)
add10 = make_adder(10)
print(add5(3))
print(add10(3))
print(add5(add10(0)))

def counter():
    n = 0
    def incr():
        nonlocal n
        n = n + 1
        return n
    return incr

c = counter()
print(c())
print(c())
print(c())
