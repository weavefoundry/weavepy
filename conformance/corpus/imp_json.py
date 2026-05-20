import json

print(json.dumps([1, 2, 3]))
print(json.dumps({"a": 1}))
print(json.loads('{"a": 1, "b": 2}'))
print(json.loads("[1, 2, 3]"))
