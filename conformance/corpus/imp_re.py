import re

m = re.match(r"(\w+)\s+(\w+)", "hello world")
print(m.group(0), m.group(1), m.group(2))

print(re.findall(r"\d+", "a1 b22 c333"))
print(re.sub(r"\s+", "_", "a b   c"))
