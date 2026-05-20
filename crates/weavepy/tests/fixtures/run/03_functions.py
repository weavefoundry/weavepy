def add(a, b):
    return a + b

def fact(n):
    if n <= 1:
        return 1
    return n * fact(n - 1)

def fib(n):
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)

print(add(2, 3))
print(fact(5))
print(fact(10))
print(fib(10))
