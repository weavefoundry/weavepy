"""JSON buffers preserve Unicode, callbacks, number conversion, and errors."""

import _json
import json
import math
import sys
from json.encoder import py_encode_basestring, py_encode_basestring_ascii


def pure_decode(text, **options):
    decoder = json.JSONDecoder(**options)
    decoder.parse_string = json.decoder.py_scanstring
    decoder.scan_once = json.scanner.py_make_scanner(decoder)
    return decoder.decode(text)


strings = [
    "", "plain ASCII", "\\\"\b\f\n\r\t\x00\x1f\x7f", "caf\u00e9 \u03a3 \U0001f600",
    "café\\\nΣ\"😀",
    "\ud800", "a\udfff\U0001f600z", "\ud800\udc00", "a" * 10000,
    "".join(chr(c) for c in range(256)),
]
for text in strings:
    assert _json.encode_basestring(text) == py_encode_basestring(text)
    assert _json.encode_basestring_ascii(text) == py_encode_basestring_ascii(text)
    # Unescaped surrogate pairs retain two code points; escaped valid pairs
    # decode into one scalar, as in the reference decoder.
    for ascii in (False, True):
        for indent in (None, "", "  ", "\ud800"):
            value = {"key": [text, None, True, False, -123, 0.5]}
            encoder = json.JSONEncoder(ensure_ascii=ascii, indent=indent)
            actual = encoder.encode(value)
            expected = "".join(encoder.iterencode(value, _one_shot=False))
            assert actual == expected, (repr(actual), repr(expected))
            if indent != "\ud800":
                assert json.loads(actual) == pure_decode(actual)

for sep in [("\ud800", "\udfff"), (" | ", " -> "), ("", "")]:
    encoder = json.JSONEncoder(separators=sep)
    value = {"a": [1, 2], "b": 3}
    assert encoder.encode(value) == "".join(encoder.iterencode(value))

for token in [
    "0", "-0", "9223372036854775807", "-9223372036854775808",
    "9223372036854775808", "-9223372036854775809", "1" * 200,
    "-0.0", "1e400", "-1e400", "1e-400", "-1e-400",
    "2.2250738585072014e-308", "4.9406564584124654e-324",
    "0.1000000000000000055511151231257827021181583404541015625",
]:
    for text in ("[" + token + "]", '["\u03a3",' + token + "]"):
        actual, expected = json.loads(text)[-1], pure_decode(text)[-1]
        assert actual == expected
        assert type(actual) is type(expected)
        if isinstance(actual, float):
            assert math.copysign(1, actual) == math.copysign(1, expected)

old_limit = sys.get_int_max_str_digits()
try:
    sys.set_int_max_str_digits(640)
    try:
        json.loads("1" * 641)
    except ValueError:
        pass
    else:
        raise AssertionError("integer conversion limit was bypassed")
finally:
    sys.set_int_max_str_digits(old_limit)

calls = []


def number(text):
    calls.append(text)
    return "number:" + text


assert json.loads("[-0,1.5,1e3]", parse_int=number, parse_float=number) == [
    "number:-0", "number:1.5", "number:1e3",
]
assert calls == ["-0", "1.5", "1e3"]


class BadBool:
    def __bool__(self):
        raise ValueError("strict first")


class Context:
    strict = BadBool()

    @property
    def parse_float(self):
        raise AssertionError("parser inspected before strict")


try:
    _json.make_scanner(Context())
except ValueError as error:
    assert str(error) == "strict first"
else:
    raise AssertionError("strict callback was bypassed")

assert json.loads('{"a":1,"a":2}', object_pairs_hook=lambda x: x) == [("a", 1), ("a", 2)]
decoded = json.loads('[{"same":1},{"same":2}]')
assert next(iter(decoded[0])) is next(iter(decoded[1]))


def check_duplicate_callbacks(parser_name, tokens, pairs_hook):
    events = []

    class Box:
        def __init__(self, value):
            self.value = value

        def __del__(self):
            events.append(("drop", self.value))

    def parse(text):
        events.append(("parse", text))
        return Box(text)

    options = {parser_name: parse}
    if pairs_hook:
        options["object_pairs_hook"] = lambda pairs: pairs
    document = '{"same":' + tokens[0] + ',"same":' + tokens[1] + ',"tail":' + tokens[2] + '}'
    result = json.loads(document, **options)
    expected = [("parse", tokens[0]), ("parse", tokens[1])]
    if not pairs_hook:
        expected.append(("drop", tokens[0]))
    expected.append(("parse", tokens[2]))
    assert events == expected, (parser_name, pairs_hook, events)
    if pairs_hook:
        assert [value.value for key, value in result] == tokens
    else:
        assert result["same"].value == tokens[1]


for parser_name, tokens in (
    ("parse_int", ["1", "2", "3"]),
    ("parse_float", ["1.0", "2.0", "3.0"]),
    ("parse_constant", ["NaN", "Infinity", "-Infinity"]),
):
    for pairs_hook in (False, True):
        check_duplicate_callbacks(parser_name, tokens, pairs_hook)

# Memoization must use decoded text, regardless of the spelling or input
# storage. A surrogate value makes the entire input use wide storage.
for first, second, expected_key in [
    ('"repeated_key"', '"repeated_key"', "repeated_key"),
    ('"repeated_key"', '"repeated_\\u006bey"', "repeated_key"),
    ('"repeated_\\u006bey"', '"repeated_key"', "repeated_key"),
    ('"caf\\u00e9"', '"café"', "café"),
    ('"café"', '"caf\\u00e9"', "café"),
    ('"\\ud83d\\ude00"', '"😀"', "😀"),
    ('"\\ud800_key"', '"\ud800_key"', "\ud800_key"),
    ('"line\\nkey"', '"line\\u000akey"', "line\nkey"),
]:
    for prefix in ('"text"', '"\udfff"'):
        document = '[' + prefix + ',{' + first + ':1},{' + second + ':2}]'
        decoded = json.loads(document)
        a, b = next(iter(decoded[1])), next(iter(decoded[2]))
        assert a == b == expected_key
        assert a is b, (first, second, prefix)
        pairs = json.loads('{' + first + ':1,' + second + ':2}',
                           object_pairs_hook=lambda value: value)
        assert pairs[0][0] is pairs[1][0]


def reentrant_object_hook(value):
    inner = json.loads('{"repeated_key":3}')
    assert inner == {"repeated_key": 3}
    return value


decoded = json.loads('[{"repeated_key":1},{"repeated_key":2}]',
                     object_hook=reentrant_object_hook)
assert next(iter(decoded[0])) is next(iter(decoded[1]))

# Scanner arguments and results are character offsets, even when UTF-8
# byte lengths differ and raw_decode starts partway through the document.
decoder = json.JSONDecoder()
for prefix in ["abc", "é Σ 😀", "\ud800", "\n é"]:
    payload = '["é\\tΣ", "😀", 1.25]'
    document = prefix + payload + " trailing"
    obj, end = decoder.raw_decode(document, len(prefix))
    assert obj == ["é\tΣ", "😀", 1.25]
    assert end == len(prefix) + len(payload)
    for position in [len(document), len(document) + 9]:
        try:
            decoder.raw_decode(document, position)
        except json.JSONDecodeError as error:
            assert error.pos == position
        else:
            raise AssertionError("out-of-range decoder offset accepted")


def custom_encoder(value):
    calls.append(value)
    return '"custom\ud800"'


# Replacing the module export must never mark a custom callable as native.
original = _json.encode_basestring_ascii
try:
    _json.encode_basestring_ascii = custom_encoder
    encoder = _json.make_encoder({}, None, custom_encoder, None, ": ", ", ", False, False, True)
    calls.clear()
    assert "".join(encoder(["a", "b"], 0)) == '["custom\ud800", "custom\ud800"]'
    assert calls == ["a", "b"]
finally:
    _json.encode_basestring_ascii = original


class String(str):
    pass


encoder = _json.make_encoder({}, None, lambda _: String('"subclass"'), None, ":", ",", False, False, True)
assert "".join(encoder(["a"], 0)) == '["subclass"]'

values = [object()]


def mutate(_):
    values.append(42)
    return "first"


assert json.dumps(values, default=mutate) == '["first", 42]'
cycle = []
cycle.append(cycle)
try:
    json.dumps(cycle)
except ValueError as error:
    assert "Circular reference" in str(error)
else:
    raise AssertionError("circular reference accepted")

for bad in ['[1,]', '{"a":1,}', '[1e]', '["\\u123x"]', '["\x01"]', '["\u03a3",]']:
    try:
        pure_decode(bad)
    except json.JSONDecodeError as error:
        position = error.pos
    else:
        raise AssertionError("invalid reference input")
    try:
        json.loads(bad)
    except json.JSONDecodeError as error:
        assert error.pos == position, (bad, error.pos, position)
    else:
        raise AssertionError("invalid input accepted")

# CPython's C scanner points at the backslash for an invalid escape;
# its pure-Python scanner points one character later.
for bad, position in [('["\\x"]', 2), ('["\u03a3\\x"]', 3)]:
    try:
        json.loads(bad)
    except json.JSONDecodeError as error:
        assert error.pos == position
    else:
        raise AssertionError("invalid escape accepted")
