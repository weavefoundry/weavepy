"""Field arguments keep evaluation order, caller state, and ownership."""
import gc
import sys
import threading
import weakref


class Value:
    def __init__(self, rank):
        self.rank = rank


class Holder:
    def __init__(self, left, right):
        self.left, self.right = left, right


class Slots:
    __slots__ = ('left', 'right')
    def __init__(self, left, right):
        self.left, self.right = left, right


def stronger(left, right):
    return left.rank < right.rank


class Rules:
    @staticmethod
    def stronger(left, right):
        return left.rank < right.rank
    def compare(self, left, right):
        return left.rank < right.rank


def global_call(root):
    return stronger(root.left, root.right.left)


def static_call(root):
    return Rules.stronger(root.left, root.right.left)


def method_call(root, rules):
    return rules.compare(root.left, root.right.left)


def alias_call(root):
    return stronger(root.left, root.left)


rules = Rules()
for cls in (Holder, Slots):
    root = cls(Value(1), cls(Value(2), None))
    for _ in range(4000):
        assert global_call(root) is True
        assert static_call(root) is True
        assert method_call(root, rules) is True
        assert alias_call(root) is False
    # Reordered fields and slot storage still resolve the requested name.
    del root.left
    root.left = Value(3)
    assert global_call(root) is False
    assert static_call(root) is False
    assert method_call(root, rules) is False

# A missing first argument stops evaluation before the second argument.
events = []
class Missing:
    @property
    def left(self):
        events.append('missing-left')
        raise AttributeError('missing argument')
    @property
    def right(self):
        events.append('unexpected-right')
        return Holder(Value(4), None)
for call in [global_call, static_call]:
    events.clear()
    try:
        call(Missing())
    except AttributeError as error:
        assert str(error) == 'missing argument'
    else:
        raise AssertionError('missing field must raise')
    assert events == ['missing-left'], events

# Descriptor mutation invalidates the caller's argument cache. The first
# argument callback can change the local used to evaluate the second one.
class MutatingHolder(Holder):
    pass
root = MutatingHolder(Value(1), Holder(Value(2), None))
for _ in range(4000):
    assert global_call(root) is True
replacement = Holder(Value(20), Holder(Value(0), None))
class Left:
    def __get__(self, instance, owner):
        frame = sys._getframe(1)
        assert frame.f_code.co_name == 'global_call'
        assert frame.f_locals['root'] is root
        frame.f_locals['root'] = replacement
        events.append('left')
        return Value(1)
    def __set__(self, instance, value):
        raise AssertionError('unexpected store')
MutatingHolder.left = Left()
events.clear()
assert global_call(root) is False
assert events == ['left'], events
del MutatingHolder.left
assert global_call(root) is True

# Rich comparison keeps the predicate activation and receives arguments
# evaluated in the caller before the predicate executes.
token = object()
class Rich:
    def __lt__(self, other):
        frame = sys._getframe(1)
        assert frame.f_code.co_name == 'stronger'
        assert frame.f_locals['left'] is root.left
        assert frame.f_locals['right'] is root.right.left
        events.append(('compare', other))
        return token
root.left.rank = Rich()
events.clear()
assert global_call(root) is token
assert events == [('compare', 2)], events
root.left.rank = 1

# Code and global mutation must revoke the old pure callee.
def opposite(left, right):
    return left.rank > right.rank
saved_code = stronger.__code__
stronger.__code__ = opposite.__code__
assert global_call(root) is False
stronger.__code__ = saved_code
assert global_call(root) is True
original = stronger
stronger = opposite
assert global_call(root) is False
stronger = original
assert global_call(root) is True

# Default values and method bindings remain live after warmup.
def default_field(left, right=Value(2)):
    return left.rank < right.rank

def default_call(root):
    return default_field(root.left)
for _ in range(4000):
    assert default_call(root) is True
default_field.__defaults__ = (Value(0),)
assert default_call(root) is False
saved_static = Rules.__dict__['stronger']
Rules.stronger = staticmethod(opposite)
assert static_call(root) is False
Rules.stronger = saved_static
assert static_call(root) is True
rules.compare = lambda left, right: 'shadowed'
assert method_call(root, rules) == 'shadowed'
del rules.compare
assert method_call(root, rules) is True

# The argument capacity and field-chain cutoff keep their fallback.
def take8(a, b, c, d, e, f, g, h):
    return h

def take9(a, b, c, d, e, f, g, h, i):
    return i

def eight(root):
    return take8(root.left, root.left, root.left, root.left,
                 root.left, root.left, root.left, root.left)

def nine(root):
    return take9(root.left, root.left, root.left, root.left,
                 root.left, root.left, root.left, root.left, root.left)

def identity(value):
    return value

def long_chain(root):
    return identity(root.right.right.right.right.right.right.right.right.left)
for _ in range(4000):
    assert eight(root) is root.left
    assert nine(root) is root.left
chain = root
for _ in range(8):
    chain = Holder(None, chain)
for _ in range(4000):
    assert long_chain(chain) is root.left

# The returned argument must be owned even after its root is released.
def retain(root):
    return identity(root.left)
root = Holder(Value(17), None)
for _ in range(4000):
    assert retain(root) is root.left
result = retain(root)
root_ref, value_ref = weakref.ref(root), weakref.ref(result)
del root
gc.collect()
assert root_ref() is None
assert value_ref() is result
result.rank = 19
assert value_ref().rank == 19
del result
gc.collect()
assert value_ref() is None

# Active observers still see the called function.
root = Holder(Value(1), Holder(Value(2), None))
events.clear()
def trace(frame, event, arg):
    if frame.f_code is stronger.__code__:
        assert frame.f_locals['left'] is root.left
        events.append(event)
    return trace
sys.settrace(trace)
try:
    assert global_call(root) is True
finally:
    sys.settrace(None)
assert 'call' in events and 'return' in events, events
# Publishing the receiver to another thread revokes private-storage reads.
def change_shared():
    root.left = Value(4)
worker = threading.Thread(target=change_shared)
worker.start()
worker.join()
assert global_call(root) is False
assert static_call(root) is False
assert method_call(root, rules) is False
print('Borrowed field arguments: ok')
