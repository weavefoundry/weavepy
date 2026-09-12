"""Draft generator for weakref storage, lookup, and construction controls."""
import hashlib
import json
from pathlib import Path

root = Path('target/weakref-shared-keys-inputs')
assert not root.exists()
root.mkdir()
common = '''import gc
import weakref
gc.disable()

class Node:
    __slots__ = ('value', '__weakref__')
    def __init__(self, value):
        self.value = value

class CallableNode(Node):
    __slots__ = ()
    def __call__(self, extra):
        return self.value + extra

callbacks = 0
def on_collect(ref):
    global callbacks
    assert ref() is None
    callbacks += 1
'''
reports = {}

def add(group, name, work, source, verification):
    directory = root / group
    directory.mkdir(exist_ok=True)
    report = reports.setdefault(group, {'purpose': __doc__, 'work': work, 'rows': {}})
    assert report['work'] == work
    path = directory / (name + '.py')
    driver = directory / (name + '-verify.py')
    path.write_text(common + source)
    driver.write_text(common + source + '\nimport json\nfor stage in range(2):\n    if stage:\n        bench(' + str(work) + ')\n' + ''.join('    ' + line + '\n' for line in verification.splitlines()))
    report['rows'][name] = {'path': str(path), 'source_sha256': hashlib.sha256(path.read_bytes()).hexdigest(), 'driver': str(driver), 'driver_sha256': hashlib.sha256(driver.read_bytes()).hexdigest()}

makers = {'ordinary': 'None', 'ref': 'weakref.ref(node)', 'callback_ref': 'weakref.ref(node, on_collect)', 'proxy': 'weakref.proxy(node)', 'callable_proxy': 'weakref.proxy(node)'}
for kind, maker in makers.items():
    cls = 'CallableNode' if kind == 'callable_proxy' else 'Node'
    population = f'retained = [{cls}(i) for i in range(30000)]\n'
    population += 'watchers = []\n' if kind == 'ordinary' else f'watchers = [{maker} for node in retained]\n'
    check = 'not watchers' if kind == 'ordinary' else ('all(watchers[i]() is retained[i] for i in (0, 15000, 29999))' if kind in ('ref', 'callback_ref') else 'all(watchers[i].value == retained[i].value for i in (0, 15000, 29999))')
    source = population + '''
def bench(n):
    for _ in range(n):
        gc.collect(2)
    return n
'''
    verify = f'''assert bench(2) == 2
assert len(retained) == 30000
assert {check}
assert callbacks == 0
print(json.dumps([len(retained), len(watchers), retained[0].value, retained[15000].value, retained[-1].value, callbacks]))'''
    add('heaps', kind + '_30000', 5, source, verify)

accesses = {
    'ordinary_attr': ('target = Node(7)\n', 'target.value', 7),
    'ref_call': ('target = Node(7)\nref = weakref.ref(target)\n', 'ref().value', 7),
    'ref_explicit_call': ('target = Node(7)\nref = weakref.ref(target)\n', 'ref.__call__().value', 7),
    'callback_read': ('target = Node(7)\nref = weakref.ref(target, on_collect)\n', 'int(ref.__callback__ is on_collect)', 1),
    'proxy_attr': ('target = Node(7)\nproxy = weakref.proxy(target)\n', 'proxy.value', 7),
    'proxy_call': ('target = CallableNode(7)\nproxy = weakref.proxy(target)\n', 'proxy(3)', 10),
    'weak_dict_get': ('target = Node(7)\nvalues = weakref.WeakValueDictionary()\nvalues[1] = target\n', 'values[1].value', 7),
    'dead_ref_call': ('target = Node(7)\nref = weakref.ref(target)\ndel target\ngc.collect(2)\nassert ref() is None\n', 'int(ref() is None)', 1),
}
for name, (setup, expression, factor) in accesses.items():
    source = setup + f'''
def bench(n):
    total = 0
    for _ in range(n):
        total += {expression}
    return total
'''
    add('access', name, 100000, source, f'assert bench(17) == {17 * factor}\nassert callbacks == 0\nprint(json.dumps([{17 * factor}, callbacks]))')

for kind, maker in makers.items():
    cls = 'CallableNode' if kind == 'callable_proxy' else 'Node'
    setup = f'retained = [{cls}(i) for i in range(3000)]\nwatchers = []\n'
    construction = 'list(retained)' if kind == 'ordinary' else f'[{maker} for node in retained]'
    source = setup + f'''
def bench(n):
    global watchers
    for _ in range(n):
        watchers.clear()
        gc.collect(2)
        watchers = {construction}
    return len(watchers)
'''
    check = 'watchers[1500] is retained[1500]' if kind == 'ordinary' else ('watchers[1500]() is retained[1500]' if kind in ('ref', 'callback_ref') else 'watchers[1500].value == retained[1500].value')
    add('construction', kind + '_3000', 5, source, f'assert bench(2) == 3000\nassert {check}\nassert callbacks == 0\nprint(json.dumps([len(retained), len(watchers), callbacks]))')

source = '''
def bench(n):
    before = callbacks
    for _ in range(n):
        nodes = [Node(i) for i in range(3000)]
        refs = [weakref.ref(node, on_collect) for node in nodes]
        nodes.clear()
        gc.collect(2)
        assert refs[0]() is None and refs[-1]() is None
    return callbacks - before
'''
add('construction', 'callback_churn_3000', 5, source, 'result = bench(2)\nassert result == 6000\nprint(json.dumps([result]))')
for group, report in reports.items():
    (root / group / 'preparation.json').write_text(json.dumps(report, indent=2) + '\n')
print({group: len(report['rows']) for group, report in reports.items()})
