"""Preserve string-call results, argument errors, and index callbacks."""

def case_upper():
    return 'aBc'.upper()
def case_lower():
    return 'aBc'.lower()
def case_casefold():
    return 'Straße'.casefold()
def case_strip():
    return ' abXYZba '.strip(' ab')
def case_lstrip():
    return ' abXYZba '.lstrip(' ab')
def case_rstrip():
    return ' abXYZba '.rstrip(' ab')
def case_replace():
    return 'abab'.replace('ab', 'xyz', 1)
def case_startswith():
    return 'zabz'.startswith('ab', 1, 3)
def case_endswith():
    return 'zabz'.endswith('ab', 1, 3)
def case_find():
    return 'zababc'.find('ab', 1, 5)
def case_rfind():
    return 'zababc'.rfind('ab', 1, 5)
def case_index():
    return 'zababc'.index('ab', 1, 5)
def case_rindex():
    return 'zababc'.rindex('ab', 1, 5)
def case_count():
    return 'zababc'.count('ab', 1, 5)
def case_split():
    return 'ab cd ef'.split(' ', 1)
def case_rsplit():
    return 'ab cd ef'.rsplit(' ', 1)
def case_join():
    return '|'.join(['ab', 'cd'])
def case_title():
    return 'abc DEF'.title()
def case_capitalize():
    return 'abc DEF'.capitalize()
def case_swapcase():
    return 'aBc'.swapcase()
def case_zfill():
    return '-12'.zfill(7)
def case_removeprefix():
    return 'abcd'.removeprefix('ab')
def case_removesuffix():
    return 'abcd'.removesuffix('cd')
def case_isdigit():
    return '012'.isdigit()
def case_isnumeric():
    return '012'.isnumeric()
def case_isdecimal():
    return '012'.isdecimal()
def case_isalpha():
    return 'abc'.isalpha()
def case_isalnum():
    return 'abc12'.isalnum()
def case_isspace():
    return ' \t'.isspace()
def case_isupper():
    return 'ABC'.isupper()
def case_islower():
    return 'abc'.islower()
def case_istitle():
    return 'Abc Def'.istitle()
def case_isascii():
    return 'abc'.isascii()
def case_isidentifier():
    return 'abc_2'.isidentifier()
def case_isprintable():
    return 'abc !'.isprintable()
def case_extra_upper():
    return 'alpha beta'.upper(1, 2, 3, 4)
def case_extra_replace():
    return 'alpha beta'.replace('a', 'b', 1, 2)
def case_extra_find():
    return 'alpha beta'.find('a', 0, 3, 4)
def case_extra_split():
    return 'alpha beta'.split(' ', 1, 2, 3)
def case_replace_missing_both():
    return 'abc'.replace()
def case_replace_missing_new():
    return 'abc'.replace('a')
def case_replace_extra_positional():
    return 'abc'.replace('a', 'b', 1, 2)
def case_replace_none_count():
    return 'abc'.replace('a', 'b', None)
def case_replace_none_keyword_count():
    return 'abc'.replace('a', 'b', count=None)
def case_replace_duplicate_count():
    return 'abc'.replace('a', 'b', 1, count=2)
def case_replace_unexpected_keyword():
    return 'abc'.replace('a', 'b', limit=2)
def case_replace_keyword_old():
    return 'abc'.replace(old='a', new='b')
def case_replace_keyword_new():
    return 'abc'.replace('a', new='b')
def case_replace_zero():
    return 'aba'.replace('a', 'b', 0)
def case_replace_negative():
    return 'aba'.replace('a', 'b', -3)
def case_replace_true_count():
    return 'aba'.replace('a', 'b', True)
def case_replace_keyword_count():
    return 'aba'.replace('a', 'b', count=1)
def case_replace_huge_count():
    return 'aba'.replace('a', 'b', 2**100)
def case_replace_bad_old():
    return 'abc'.replace(1, 'b')
def case_replace_bad_new():
    return 'abc'.replace('a', 1)
def case_replace_bad_old_keyword():
    return 'abc'.replace(1, 'b', wrong=1)
def case_replace_bad_old_extra():
    return 'abc'.replace(1, 'b', 1, 2)
def case_replace_missing_with_extra_keywords():
    return 'abc'.replace(count=1, wrong=2, new='b', old='a')

cases = [
    ('upper', case_upper, {'result': 'ABC'}),
    ('lower', case_lower, {'result': 'abc'}),
    ('casefold', case_casefold, {'result': 'strasse'}),
    ('strip', case_strip, {'result': 'XYZ'}),
    ('lstrip', case_lstrip, {'result': 'XYZba '}),
    ('rstrip', case_rstrip, {'result': ' abXYZ'}),
    ('replace', case_replace, {'result': 'xyzab'}),
    ('startswith', case_startswith, {'result': True}),
    ('endswith', case_endswith, {'result': True}),
    ('find', case_find, {'result': 1}),
    ('rfind', case_rfind, {'result': 3}),
    ('index', case_index, {'result': 1}),
    ('rindex', case_rindex, {'result': 3}),
    ('count', case_count, {'result': 2}),
    ('split', case_split, {'result': ['ab', 'cd ef']}),
    ('rsplit', case_rsplit, {'result': ['ab cd', 'ef']}),
    ('join', case_join, {'result': 'ab|cd'}),
    ('title', case_title, {'result': 'Abc Def'}),
    ('capitalize', case_capitalize, {'result': 'Abc def'}),
    ('swapcase', case_swapcase, {'result': 'AbC'}),
    ('zfill', case_zfill, {'result': '-000012'}),
    ('removeprefix', case_removeprefix, {'result': 'cd'}),
    ('removesuffix', case_removesuffix, {'result': 'ab'}),
    ('isdigit', case_isdigit, {'result': True}),
    ('isnumeric', case_isnumeric, {'result': True}),
    ('isdecimal', case_isdecimal, {'result': True}),
    ('isalpha', case_isalpha, {'result': True}),
    ('isalnum', case_isalnum, {'result': True}),
    ('isspace', case_isspace, {'result': True}),
    ('isupper', case_isupper, {'result': True}),
    ('islower', case_islower, {'result': True}),
    ('istitle', case_istitle, {'result': True}),
    ('isascii', case_isascii, {'result': True}),
    ('isidentifier', case_isidentifier, {'result': True}),
    ('isprintable', case_isprintable, {'result': True}),
    ('extra_upper', case_extra_upper, {'message': 'str.upper() takes no arguments (4 given)', 'type': 'TypeError'}),
    ('extra_replace', case_extra_replace, {'message': 'replace() takes at most 3 arguments (4 given)', 'type': 'TypeError'}),
    ('extra_find', case_extra_find, {'message': 'find expected at most 3 arguments, got 4', 'type': 'TypeError'}),
    ('extra_split', case_extra_split, {'message': 'split() takes at most 2 arguments (4 given)', 'type': 'TypeError'}),
    ('replace_missing_both', case_replace_missing_both, {'message': 'replace() takes at least 2 positional arguments (0 given)', 'type': 'TypeError'}),
    ('replace_missing_new', case_replace_missing_new, {'message': 'replace() takes at least 2 positional arguments (1 given)', 'type': 'TypeError'}),
    ('replace_extra_positional', case_replace_extra_positional, {'message': 'replace() takes at most 3 arguments (4 given)', 'type': 'TypeError'}),
    ('replace_none_count', case_replace_none_count, {'message': "'NoneType' object cannot be interpreted as an integer", 'type': 'TypeError'}),
    ('replace_none_keyword_count', case_replace_none_keyword_count, {'message': "'NoneType' object cannot be interpreted as an integer", 'type': 'TypeError'}),
    ('replace_duplicate_count', case_replace_duplicate_count, {'message': 'replace() takes at most 3 arguments (4 given)', 'type': 'TypeError'}),
    ('replace_unexpected_keyword', case_replace_unexpected_keyword, {'message': "replace() got an unexpected keyword argument 'limit'", 'type': 'TypeError'}),
    ('replace_keyword_old', case_replace_keyword_old, {'message': 'replace() takes at least 2 positional arguments (0 given)', 'type': 'TypeError'}),
    ('replace_keyword_new', case_replace_keyword_new, {'message': 'replace() takes at least 2 positional arguments (1 given)', 'type': 'TypeError'}),
    ('replace_zero', case_replace_zero, {'result': 'aba'}),
    ('replace_negative', case_replace_negative, {'result': 'bbb'}),
    ('replace_true_count', case_replace_true_count, {'result': 'bba'}),
    ('replace_keyword_count', case_replace_keyword_count, {'result': 'bba'}),
    ('replace_huge_count', case_replace_huge_count, {'message': 'Python int too large to convert to C ssize_t', 'type': 'OverflowError'}),
    ('replace_bad_old', case_replace_bad_old, {'message': 'replace() argument 1 must be str, not int', 'type': 'TypeError'}),
    ('replace_bad_new', case_replace_bad_new, {'message': 'replace() argument 2 must be str, not int', 'type': 'TypeError'}),
    ('replace_bad_old_keyword', case_replace_bad_old_keyword, {'message': "replace() got an unexpected keyword argument 'wrong'", 'type': 'TypeError'}),
    ('replace_bad_old_extra', case_replace_bad_old_extra, {'message': 'replace() takes at most 3 arguments (4 given)', 'type': 'TypeError'}),
    ('replace_missing_with_extra_keywords', case_replace_missing_with_extra_keywords, {'message': 'replace() takes at most 3 keyword arguments (4 given)', 'type': 'TypeError'}),
]

for name, function, expected in cases:
    for _ in range(16):
        try:
            actual = {"result": function()}
        except Exception as exc:
            actual = {"type": type(exc).__name__, "message": str(exc)}
        if name == "extra_split":
            # Existing split wording differs; the storage fallback must still
            # propagate its TypeError. Replace messages are checked exactly.
            assert actual.get("type") == "TypeError", (name, actual)
        else:
            assert actual == expected, (name, actual, expected)

assert "\ud800a".replace("a", "b", 1) == "\ud800b"
index_calls = []
constructions = []


class Count:
    def __init__(self):
        constructions.append(1)

    def __index__(self):
        index_calls.append(1)
        return 1


def excess_count():
    return "aba".replace("a", "b", Count(), 9)


for _ in range(16):
    try:
        excess_count()
    except TypeError as exc:
        assert str(exc) == "replace() takes at most 3 arguments (4 given)"
    else:
        raise AssertionError("excess arguments were accepted")
assert constructions == [1] * 16
assert index_calls == []


class HugeCount:
    def __index__(self):
        return 2**100


class RaisingCount:
    def __index__(self):
        raise OverflowError("callback overflow")


for count, message in [
    (HugeCount(), "Python int too large to convert to C ssize_t"),
    (RaisingCount(), "callback overflow"),
]:
    try:
        "aba".replace("a", "b", count)
    except OverflowError as exc:
        assert str(exc) == message
    else:
        raise AssertionError("count conversion did not raise")
print("ok")
