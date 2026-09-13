"""Keyword defaults preserve aliases and release borrows before user code."""
import gc
import weakref


class Token:
    pass


def exercise_sparse_and_supplied():
    def select(*, a, b, c, d):
        return a, b, c, d

    values = [Token() for _ in range(4)]
    defaults = dict(zip(('a', 'b', 'c', 'd'), values))
    defaults['unrelated'] = Token()
    defaults[17] = Token()
    select.__kwdefaults__ = defaults
    for _ in range(100):
        result = select(a=19, c=23)
        assert result[0] == 19 and result[2] == 23
        assert result[1] is values[1] and result[3] is values[3]
        assert select(**{}) == tuple(values)
        assert select(a=1, b=2, c=3, d=4) == (1, 2, 3, 4)
    defaults['d'] = values[0]
    assert select()[3] is values[0]
    select.__kwdefaults__ = None
    assert select(a=1, b=2, c=3, d=4) == (1, 2, 3, 4)
    try:
        select(a=1, c=3)
    except TypeError as error:
        assert "2 required keyword-only arguments: 'b' and 'd'" in str(error)
    else:
        raise AssertionError('cleared keyword defaults still bind')


def exercise_mutation_from_callee():
    def clear_from_body(*, value):
        clear_from_body.__kwdefaults__.clear()
        gc.collect()
        assert needed_ref() is value
        assert ignored_ref() is None
        return value

    needed, ignored = Token(), Token()
    needed_ref, ignored_ref = weakref.ref(needed), weakref.ref(ignored)
    clear_from_body.__kwdefaults__ = {'value': needed, 'unrelated': ignored}
    del needed, ignored
    result = clear_from_body()
    assert needed_ref() is result
    del result
    gc.collect()
    assert needed_ref() is None


def exercise_compiled_and_deleted():
    first, second = Token(), Token()

    def target(*, left=first, right=second):
        return left, right

    assert target() == (first, second)
    target.__kwdefaults__ = {'left': second, 'right': first}
    assert target() == (second, first)
    del target.__kwdefaults__
    try:
        target()
    except TypeError as error:
        assert "2 required keyword-only arguments" in str(error)
    else:
        raise AssertionError('deleting overrides revived compiled defaults')


def exercise_code_replacement_and_bound_receiver():
    def target(*, left, right):
        return left, right

    def replacement(*, right, flag):
        return right, flag

    one, two = Token(), Token()
    target.__kwdefaults__ = {'left': one, 'right': two}
    target.__code__ = replacement.__code__
    try:
        target()
    except TypeError as error:
        assert "keyword-only argument: 'flag'" in str(error)
    else:
        raise AssertionError('new keyword-only argument received stale default')
    target.__kwdefaults__['flag'] = one
    assert target() == (two, one)

    class Holder:
        def method(self, *, left, right):
            return self, left, right

    Holder.method.__kwdefaults__ = {'left': one, 'right': two}
    holder = Holder()
    for _ in range(100):
        result = holder.method(right=31)
        assert result[0] is holder and result[1] is one and result[2] == 31


exercise_sparse_and_supplied()
exercise_mutation_from_callee()
exercise_compiled_and_deleted()
exercise_code_replacement_and_bound_receiver()
print('overridden keyword defaults: ownership and binding ok')
