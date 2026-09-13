"""Streaming sum callbacks and exact integer sequence accumulation."""
# The separate oracle retains three existing TypeError wording differences
# and two existing compensated-float differences. This regression checks
# every other oracle row exactly; invalid arities must still raise TypeError.

def append_during_sum():
    values = [1]
    events = []
    class Term:
        def __radd__(self, other):
            events.append(other)
            values.append(4)
            return other + 2
    values.extend([Term(), 3])
    return sum(values), events

def clear_during_sum():
    values = [1]
    events = []
    class Term:
        def __radd__(self, other):
            events.append(other)
            values.clear()
            return other + 2
    values.extend([Term(), 3])
    return sum(values), events

def no_length_hint():
    events = []
    class Source:
        def __iter__(self):
            events.append('iter')
            return iter((2, 3, 4))
        def __length_hint__(self):
            events.append('hint')
            raise ValueError('must not request a length hint')
    try:
        return sum(Source()), events
    except Exception as error:
        return type(error).__name__, str(error), events

def addition_stops_iteration():
    events = []
    class Source:
        def __init__(self):
            self.index = 0
        def __iter__(self):
            events.append('iter')
            return self
        def __next__(self):
            self.index += 1
            events.append(self.index)
            if self.index == 1:
                return 'bad'
            raise ValueError('consumed after failed addition')
    try:
        sum(Source())
    except Exception as error:
        return type(error).__name__, str(error), events

def overridden_list_iteration():
    class Custom(list):
        def __iter__(self):
            return iter((20, 30))
    return sum(Custom([1, 2, 3]))

def overridden_tuple_iteration():
    class Custom(tuple):
        def __iter__(self):
            return iter((20, 30))
    return sum(Custom((1, 2, 3)))

def overridden_integer():
    events = []
    class Number(int):
        def __radd__(self, other):
            events.append(other)
            return other + 100
    return sum([1, Number(2), 3]), events

def start_callback():
    events = []
    class Start:
        def __add__(self, value):
            events.append(value)
            return 40 + value
    return sum([1, 2], Start()), events

def case_0():
    return sum([])

def case_1():
    return sum(())

def case_2():
    return sum([], True)

def case_3():
    return sum([], 2**200)

def case_4():
    return sum([], -0.0)

def case_5():
    return sum([3, -4, 12, 7])

def case_6():
    return sum((3, -4, 12, 7))

def case_7():
    return sum([True, False, True])

def case_8():
    return sum([True, 2, False, -4])

def case_9():
    return sum([3, -4], 9)

def case_10():
    return sum((3, -4), start=-9)

def case_11():
    return sum([2, 3], True)

def case_12():
    return sum([2**63 - 1, 1])

def case_13():
    return sum([-2**63, -1])

def case_14():
    return sum([2**63 - 1, 1, -1])

def case_15():
    return sum([-2**63, -1, 1])

def case_16():
    return sum([2**63 - 1] * 40 + [-2**63] * 40)

def case_17():
    return sum([2], 2**63 - 1)

def case_18():
    return sum([2**200, 3, -2**200])

def case_19():
    return sum([1, 2, 2**200])

def case_20():
    return sum([1, 2], 2**200)

def case_21():
    return sum([0.5, 1.5, -1.0])

def case_22():
    return sum([1, 2, 0.5, -1])

def case_23():
    return sum([1, 2], 0.5)

def case_24():
    return sum([1, 2j, 3])

def case_25():
    return sum(range(-20, 21))

def case_26():
    return sum(iter([1, 2, 3]))

def case_27():
    return sum(x for x in (1, 2, 3))

def case_28():
    return sum(None)

def case_29():
    return sum([1, None, 2])

def case_30():
    return sum([], "")

def case_31():
    return sum([], b"")

def case_32():
    return sum([], bytearray())

def case_33():
    return sum()

def case_34():
    return sum([], 1, 2)

def case_35():
    return sum([], 1, start=2)

def case_36():
    return sum([], unknown=2)

def case_37():
    return sum([1e16, 1.0, -1e16])

def case_38():
    return sum([1e16, 1, -1e16])

def case_39():
    return append_during_sum()

def case_40():
    return clear_during_sum()

def case_41():
    return no_length_hint()

def case_42():
    return addition_stops_iteration()

def case_43():
    return overridden_list_iteration()

def case_44():
    return overridden_tuple_iteration()

def case_45():
    return overridden_integer()

def case_46():
    return start_callback()

CASES = [
    ('empty_list', case_0, ('ok', 'int', '0')),
    ('empty_tuple', case_1, ('ok', 'int', '0')),
    ('empty_bool_start', case_2, ('ok', 'bool', 'True')),
    ('empty_big_start', case_3, ('ok', 'int', '1606938044258990275541962092341162602522202993782792835301376')),
    ('empty_float_start', case_4, ('ok', 'float', '-0.0')),
    ('ints_list', case_5, ('ok', 'int', '18')),
    ('ints_tuple', case_6, ('ok', 'int', '18')),
    ('bools', case_7, ('ok', 'int', '2')),
    ('mixed_bools_ints', case_8, ('ok', 'int', '-1')),
    ('positive_start', case_9, ('ok', 'int', '8')),
    ('negative_start', case_10, ('ok', 'int', '-10')),
    ('bool_start', case_11, ('ok', 'int', '6')),
    ('positive_overflow', case_12, ('ok', 'int', '9223372036854775808')),
    ('negative_overflow', case_13, ('ok', 'int', '-9223372036854775809')),
    ('positive_recovery', case_14, ('ok', 'int', '9223372036854775807')),
    ('negative_recovery', case_15, ('ok', 'int', '-9223372036854775808')),
    ('wide_intermediate', case_16, ('ok', 'int', '-40')),
    ('start_overflow', case_17, ('ok', 'int', '9223372036854775809')),
    ('big_first', case_18, ('ok', 'int', '3')),
    ('big_last', case_19, ('ok', 'int', '1606938044258990275541962092341162602522202993782792835301379')),
    ('big_start', case_20, ('ok', 'int', '1606938044258990275541962092341162602522202993782792835301379')),
    ('floats', case_21, ('ok', 'float', '1.0')),
    ('mixed_float', case_22, ('ok', 'float', '2.5')),
    ('float_start', case_23, ('ok', 'float', '3.5')),
    ('complex', case_24, ('ok', 'complex', '(4+2j)')),
    ('range', case_25, ('ok', 'int', '0')),
    ('iterator', case_26, ('ok', 'int', '6')),
    ('generator', case_27, ('ok', 'int', '6')),
    ('invalid_none', case_28, ('err', 'TypeError', "'NoneType' object is not iterable")),
    ('invalid_element', case_29, ('err', 'TypeError', "unsupported operand type(s) for +: 'int' and 'NoneType'")),
    ('string_start', case_30, ('err', 'TypeError', "sum() can't sum strings [use ''.join(seq) instead]")),
    ('bytes_start', case_31, ('err', 'TypeError', "sum() can't sum bytes [use b''.join(seq) instead]")),
    ('bytearray_start', case_32, ('err', 'TypeError', "sum() can't sum bytearray [use b''.join(seq) instead]")),
    ('unknown_keyword', case_36, ('err', 'TypeError', "sum() got an unexpected keyword argument 'unknown'")),
    ('append_during_sum', case_39, ('ok', 'tuple', '(10, [1])')),
    ('clear_during_sum', case_40, ('ok', 'tuple', '(3, [1])')),
    ('no_length_hint', case_41, ('ok', 'tuple', "(9, ['iter'])")),
    ('addition_stops_iteration', case_42, ('ok', 'tuple', '(\'TypeError\', "unsupported operand type(s) for +: \'int\' and \'str\'", [\'iter\', 1])')),
    ('overridden_list_iteration', case_43, ('ok', 'int', '50')),
    ('overridden_tuple_iteration', case_44, ('ok', 'int', '50')),
    ('overridden_integer', case_45, ('ok', 'tuple', '(104, [1])')),
    ('start_callback', case_46, ('ok', 'tuple', '(43, [1])')),
]

for name, function, expected in CASES:
    for _ in range(12):
        try:
            value = function()
            result = ('ok', type(value).__name__, repr(value))
        except Exception as error:
            result = ('err', type(error).__name__, str(error))
        assert result == expected, (name, result, expected)

for function in (case_33, case_34, case_35):
    try:
        function()
    except TypeError:
        pass
    else:
        raise AssertionError('invalid sum arity was accepted')

# A consumed value removed from the live input can finalize before return.
events = []
values = []


class Term:
    def __radd__(self, other):
        values.clear()
        return other + 1

    def __del__(self):
        events.append('drop')


def perform():
    values.append(Term())
    result = sum(values)
    return result, len(values), events[:]


assert perform() == (1, 0, ['drop'])
print('ok')
