"""Tokenize, parse (recursive descent into node classes), and evaluate a
small expression language: the shape of template engines and DSLs."""

WORK = 10000

SOURCE = """
let a = 3 * (4 + x) - y / 2;
let b = a * a + (x - 1) * (y + 2);
if b > 100 then b - a else a + b * 2;
let c = max(a, b, 10) + min(x, y);
c * 2 + (a - b) / (x + 1)
"""


class Tok:
    __slots__ = ("kind", "text")

    def __init__(self, kind, text):
        self.kind = kind
        self.text = text


def tokenize(src):
    toks = []
    i, n = 0, len(src)
    while i < n:
        ch = src[i]
        if ch.isspace():
            i += 1
        elif ch.isdigit():
            j = i
            while j < n and src[j].isdigit():
                j += 1
            toks.append(Tok("num", src[i:j]))
            i = j
        elif ch.isalpha() or ch == "_":
            j = i
            while j < n and (src[j].isalnum() or src[j] == "_"):
                j += 1
            word = src[i:j]
            toks.append(Tok("kw" if word in ("let", "if", "then", "else") else "name", word))
            i = j
        else:
            toks.append(Tok("op", ch))
            i += 1
    toks.append(Tok("eof", ""))
    return toks


class Num:
    def __init__(self, v):
        self.v = v

    def eval(self, env):
        return self.v


class Var:
    def __init__(self, name):
        self.name = name

    def eval(self, env):
        return env[self.name]


class BinOp:
    OPS = {"+": lambda a, b: a + b, "-": lambda a, b: a - b, "*": lambda a, b: a * b,
           "/": lambda a, b: a / b, ">": lambda a, b: a > b, "<": lambda a, b: a < b}

    def __init__(self, op, left, right):
        self.fn = self.OPS[op]
        self.left = left
        self.right = right

    def eval(self, env):
        return self.fn(self.left.eval(env), self.right.eval(env))


class Call:
    FUNCS = {"max": max, "min": min}

    def __init__(self, name, args):
        self.name = name
        self.args = args

    def eval(self, env):
        return self.FUNCS[self.name](*[a.eval(env) for a in self.args])


class If:
    def __init__(self, cond, then, other):
        self.cond, self.then, self.other = cond, then, other

    def eval(self, env):
        return self.then.eval(env) if self.cond.eval(env) else self.other.eval(env)


class Let:
    def __init__(self, name, value):
        self.name, self.value = name, value

    def eval(self, env):
        env[self.name] = v = self.value.eval(env)
        return v


class Parser:
    PREC = {">": 1, "<": 1, "+": 2, "-": 2, "*": 3, "/": 3}

    def __init__(self, toks):
        self.toks = toks
        self.pos = 0

    def peek(self):
        return self.toks[self.pos]

    def next(self):
        t = self.toks[self.pos]
        self.pos += 1
        return t

    def expect(self, text):
        t = self.next()
        if t.text != text:
            raise SyntaxError("expected %r got %r" % (text, t.text))

    def program(self):
        stmts = [self.statement()]
        while self.peek().text == ";":
            self.next()
            stmts.append(self.statement())
        return stmts

    def statement(self):
        t = self.peek()
        if t.text == "let":
            self.next()
            name = self.next().text
            self.expect("=")
            return Let(name, self.expr(0))
        if t.text == "if":
            self.next()
            cond = self.expr(0)
            self.expect("then")
            then = self.expr(0)
            self.expect("else")
            return If(cond, then, self.expr(0))
        return self.expr(0)

    def expr(self, min_prec):
        left = self.atom()
        while True:
            t = self.peek()
            prec = self.PREC.get(t.text) if t.kind == "op" else None
            if prec is None or prec <= min_prec:
                return left
            self.next()
            left = BinOp(t.text, left, self.expr(prec))

    def atom(self):
        t = self.next()
        if t.kind == "num":
            return Num(int(t.text))
        if t.kind == "name":
            if self.peek().text == "(":
                self.next()
                args = [self.expr(0)]
                while self.peek().text == ",":
                    self.next()
                    args.append(self.expr(0))
                self.expect(")")
                return Call(t.text, args)
            return Var(t.text)
        if t.text == "(":
            e = self.expr(0)
            self.expect(")")
            return e
        raise SyntaxError(t.text)


def bench(n):
    total = 0.0
    prog = Parser(tokenize(SOURCE)).program()
    for i in range(n):
        if i % 10 == 0:
            prog = Parser(tokenize(SOURCE)).program()
        env = {"x": i % 13, "y": i % 7 + 1}
        for stmt in prog:
            r = stmt.eval(env)
        total += r
    return round(total, 6)
