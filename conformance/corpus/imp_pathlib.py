from pathlib import PurePath

p = PurePath("a", "b", "c.txt")
print(p.name)
print(p.stem)
print(p.suffix)
print(p.parts)
