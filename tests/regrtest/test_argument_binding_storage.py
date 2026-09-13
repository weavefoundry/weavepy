"""Keep binding semantics at inline/heap boundaries and with many body locals."""

def make_function(count, kwonly=0, variadic=False, body_locals=0):
    names = ['a' + str(i) for i in range(count)]
    keywords = ['k' + str(i) for i in range(kwonly)]
    pieces = names[:]
    if variadic:
        pieces.append('*rest')
    elif keywords:
        pieces.append('*')
    pieces += [name + '=None' for name in keywords]
    if variadic:
        pieces.append('**extra')
    body = ['    body' + str(i) + ' = ' + str(i) for i in range(body_locals)]
    result = '(' + ', '.join(names + keywords) + ',)' if names or keywords else '()'
    if variadic:
        result = '(' + result + ', rest, extra)'
    body.append('    return ' + result)
    namespace = {}
    exec('def subject(' + ', '.join(pieces) + '):\n' + '\n'.join(body), namespace)
    return namespace['subject'], names, keywords


for count in [0, 1, 14, 15, 16, 17, 31, 32, 33, 64, 65, 130]:
    for kwonly in [0, 2, 17]:
        for variadic in [False, True]:
            subject, names, keywords = make_function(count, kwonly, variadic, 80)
            objects = [object() for unused in names]
            positional = tuple(objects)
            keyword_values = [object() for unused in keywords]
            bound = {name: value for name, value in zip(names, objects)}
            bound.update(zip(keywords, keyword_values))
            bound = dict(reversed(list(bound.items())))
            expected = positional + tuple(keyword_values)
            if variadic:
                bound['extra_value'] = None
                expected = (expected, (), {'extra_value': None})
            for unused in range(60):
                assert subject(**bound) == expected
            if names:
                duplicate = False
                try:
                    subject(objects[0], **bound)
                except TypeError as error:
                    duplicate = 'multiple values' in str(error) and names[0] in str(error)
                assert duplicate
                missing = {name: value for name, value in bound.items() if name != names[0]}
                try:
                    subject(**missing)
                except TypeError as error:
                    assert 'missing' in str(error) and names[0] in str(error)
                else:
                    raise AssertionError('missing required argument accepted')
                subject.__defaults__ = (None,) * (count + 2)
                expected_defaults = (None,) * count + tuple(keyword_values)
                default_kw = dict(zip(keywords, keyword_values))
                if variadic:
                    expected_defaults = (expected_defaults, (), {})
                assert subject(**default_kw) == expected_defaults
                subject.__defaults__ = None
            if keywords:
                subject.__kwdefaults__ = {}
                try:
                    subject(*positional)
                except TypeError as error:
                    assert 'keyword-only' in str(error) and keywords[0] in str(error)
                else:
                    raise AssertionError('missing keyword-only argument accepted')
            if variadic:
                subject.__kwdefaults__ = dict.fromkeys(keywords)
                surplus = (object(), object())
                assert subject(*(positional + surplus))[1] == surplus

# Body-local names must stay in the catchall instead of being bound as arguments.
subject, names, keywords = make_function(2, variadic=True, body_locals=130)
assert subject(1, 2, body90=3) == ((1, 2), (), {'body90': 3})
print('argument binding storage: ok')
