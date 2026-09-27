"""Dynamic attribute results own their values after the receiver borrow ends."""

import math as module


class Box:
    def __init__(self, value):
        self.payload = value

    @property
    def value(self):
        return self.payload


class SelfBox:
    @property
    def value(self):
        return self


class Constants:
    value = Box({"kept": [7]})


# Distinct code objects preserve native coverage for every result shape.
for name in ("read_self", "read_list", "read_dict", "read_tuple", "read_text",
             "read_none", "read_class", "read_module"):
    exec("def " + name + "(root, n):\n"
         "    result = root\n"
         "    for _ in range(n):\n"
         "        result = root.value\n"
         "    return result\n")


def capture_list(root, n):
    result = root.copy
    for _ in range(n):
        result = root.copy
    return result


self_box = SelfBox()
list_value = [1, 2, 3]
dict_value = {"kept": list_value}
tuple_value = (list_value, dict_value)
text_value = "owned result " * 20
module.value = tuple_value
for _ in range(2):
    assert read_self(self_box, 9000) is self_box
    assert read_list(Box(list_value), 9000) is list_value
    assert read_dict(Box(dict_value), 9000) is dict_value
    assert read_tuple(Box(tuple_value), 9000) is tuple_value
    assert read_text(Box(text_value), 9000) is text_value
    assert read_none(Box(None), 9000) is None
    assert read_class(Constants, 9000) is Constants.value
    assert read_module(module, 9000) is tuple_value
    method = capture_list([1, 2, 3], 9000)
    assert method() == [1, 2, 3]

# OWNERSHIP MUTATIONS: Rust verifies native reads before these changes.

# The result, rather than the source dictionary, must keep the value alive.
held = read_class(Constants, 3)
Constants.value = None
held.payload["kept"].append(11)
assert held.payload == {"kept": [7, 11]}
held = read_module(module, 3)
del module.value
assert held is tuple_value
list_value.append(5)
assert held[0] == [1, 2, 3, 5]
assert held[1]["kept"] is list_value

# A method captured from the specialized list lane owns its receiver.
root = [17, 19]
method = capture_list(root, 3)
root.append(23)
del root
assert method() == [17, 19, 23]
assert read_none(Box(None), 3) is None
assert read_self(self_box, 3) is self_box
print("dynamic attribute ownership: ok")
