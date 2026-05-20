class A:
    def m(self):
        return "A"


class B(A):
    def m(self):
        return "B-" + super(B, self).m()


print(B().m())
