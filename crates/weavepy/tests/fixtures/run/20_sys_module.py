import sys

print(sys.version_info[0])
print(sys.version_info[1])
print(isinstance(sys.maxsize, int))
print("sys" in sys.modules)
print(type(sys.path).__name__)
