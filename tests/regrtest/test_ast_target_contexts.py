"""Parser target contexts agree with CPython across source forms and modes."""

import ast


# Expected contexts and UTF-8 source offsets come from CPython 3.14.5.
# Complete AST comparisons are retained with the performance investigation.
CASES = [
    (
        'nested_assignment',
        '[a, (b, *c)] = value\n',
        'exec',
        False,
        (
            'List::Store@1:0 Name:a:Store@1:1 Name:b:Store@1:5 Name:c:Store@1:9 Name:value:Load@1:15 '
            'Starred::Store@1:8 Tuple::Store@1:4 '
        ),
    ),
    (
        'chained_assignment',
        'a = obj.field = data[index] = value\n',
        'exec',
        False,
        (
            'Attribute:field:Store@1:4 Name:a:Store@1:0 Name:data:Load@1:16 Name:index:Load@1:21 '
            'Name:obj:Load@1:4 Name:value:Load@1:30 Subscript::Store@1:16 '
        ),
    ),
    (
        'target_receivers',
        'obj[(key := index)].field = item\n',
        'exec',
        False,
        (
            'Attribute:field:Store@1:0 Name:index:Load@1:12 Name:item:Load@1:28 Name:key:Store@1:5 '
            'Name:obj:Load@1:0 Subscript::Load@1:0 '
        ),
    ),
    (
        'target_slice',
        'data[start:stop:step] = value\n',
        'exec',
        False,
        (
            'Name:data:Load@1:0 Name:start:Load@1:5 Name:step:Load@1:16 Name:stop:Load@1:11 '
            'Name:value:Load@1:24 Subscript::Store@1:0 '
        ),
    ),
    (
        'augmented',
        'a += value\nobj.field *= factor\ndata[index] -= offset\n',
        'exec',
        False,
        (
            'Attribute:field:Store@2:0 Name:a:Store@1:0 Name:data:Load@3:0 Name:factor:Load@2:13 '
            'Name:index:Load@3:5 Name:obj:Load@2:0 Name:offset:Load@3:15 Name:value:Load@1:5 '
            'Subscript::Store@3:0 '
        ),
    ),
    (
        'annotated',
        'name: Type = value\nobj.field: Type = value\ndata[index]: Type\n',
        'exec',
        False,
        (
            'Attribute:field:Store@2:0 Name:Type:Load@1:6 Name:Type:Load@2:11 Name:Type:Load@3:13 '
            'Name:data:Load@3:0 Name:index:Load@3:5 Name:name:Store@1:0 Name:obj:Load@2:0 '
            'Name:value:Load@1:13 Name:value:Load@2:18 Subscript::Store@3:0 '
        ),
    ),
    (
        'delete',
        'del a, obj.field, data[index], (b, [c, d])\n',
        'exec',
        False,
        (
            'Attribute:field:Del@1:7 List::Del@1:35 Name:a:Del@1:4 Name:b:Del@1:32 Name:c:Del@1:36 '
            'Name:d:Del@1:39 Name:data:Load@1:18 Name:index:Load@1:23 Name:obj:Load@1:7 '
            'Subscript::Del@1:18 Tuple::Del@1:31 '
        ),
    ),
    (
        'for',
        'for a, *b in pairs:\n    pass\nelse:\n    result = b\n',
        'exec',
        False,
        (
            'Name:a:Store@1:4 Name:b:Load@4:13 Name:b:Store@1:8 Name:pairs:Load@1:13 '
            'Name:result:Store@4:4 Starred::Store@1:7 Tuple::Store@1:4 '
        ),
    ),
    (
        'with',
        'with open() as (a, *b), cm as obj.field:\n    pass\n',
        'exec',
        False,
        (
            'Attribute:field:Store@1:30 Name:a:Store@1:16 Name:b:Store@1:20 Name:cm:Load@1:24 '
            'Name:obj:Load@1:30 Name:open:Load@1:5 Starred::Store@1:19 Tuple::Store@1:15 '
        ),
    ),
    (
        'async_targets',
        'async def consume(xs):\n    async for a, b in xs:\n        pass\n    async with open() as (x, y):\n        pass\n',
        'exec',
        False,
        (
            'Name:a:Store@2:14 Name:b:Store@2:17 Name:open:Load@4:15 Name:x:Store@4:26 '
            'Name:xs:Load@2:22 Name:y:Store@4:29 Tuple::Store@2:14 Tuple::Store@4:25 '
        ),
    ),
    (
        'list_comp',
        '[(x, y) for x in rows for y in x if y]',
        'eval',
        False,
        (
            'Name:rows:Load@1:17 Name:x:Load@1:2 Name:x:Load@1:31 Name:x:Store@1:12 Name:y:Load@1:5 '
            'Name:y:Load@1:36 Name:y:Store@1:26 Tuple::Load@1:1 '
        ),
    ),
    (
        'dict_comp',
        '{k: v for k, v in pairs}',
        'eval',
        False,
        (
            'Name:k:Load@1:1 Name:k:Store@1:10 Name:pairs:Load@1:18 Name:v:Load@1:4 Name:v:Store@1:13 '
            'Tuple::Store@1:10 '
        ),
    ),
    (
        'set_comp',
        '{tail for *head, tail in rows}',
        'eval',
        False,
        (
            'Name:head:Store@1:11 Name:rows:Load@1:25 Name:tail:Load@1:1 Name:tail:Store@1:17 '
            'Starred::Store@1:10 Tuple::Store@1:10 '
        ),
    ),
    (
        'generator',
        '(part for *head, part in rows)',
        'eval',
        False,
        (
            'Name:head:Store@1:11 Name:part:Load@1:1 Name:part:Store@1:17 Name:rows:Load@1:25 '
            'Starred::Store@1:10 Tuple::Store@1:10 '
        ),
    ),
    (
        'named',
        '(name := obj[index])',
        'eval',
        False,
        (
            'Name:index:Load@1:13 Name:name:Store@1:1 Name:obj:Load@1:9 Subscript::Load@1:9 '
        ),
    ),
    (
        'named_comp',
        '[(value := item) for item in rows if value]',
        'eval',
        False,
        (
            'Name:item:Load@1:11 Name:item:Store@1:21 Name:rows:Load@1:29 Name:value:Load@1:37 '
            'Name:value:Store@1:2 '
        ),
    ),
    (
        'async_comp',
        'async def f(rows):\n    return [x async for x, *y in rows if y]\n',
        'exec',
        False,
        (
            'Name:rows:Load@2:33 Name:x:Load@2:12 Name:x:Store@2:24 Name:y:Load@2:41 Name:y:Store@2:28 '
            'Starred::Store@2:27 Tuple::Store@2:24 '
        ),
    ),
    (
        'lambda',
        'lambda x, y=default: (x, y, (z := x))',
        'eval',
        False,
        (
            'Name:default:Load@1:12 Name:x:Load@1:22 Name:x:Load@1:34 Name:y:Load@1:25 '
            'Name:z:Store@1:29 Tuple::Load@1:21 '
        ),
    ),
    (
        'load_literals',
        '[a, (b, *c), obj.field, data[index]]',
        'eval',
        False,
        (
            'Attribute:field:Load@1:13 List::Load@1:0 Name:a:Load@1:1 Name:b:Load@1:5 Name:c:Load@1:9 '
            'Name:data:Load@1:24 Name:index:Load@1:29 Name:obj:Load@1:13 Starred::Load@1:8 '
            'Subscript::Load@1:24 Tuple::Load@1:4 '
        ),
    ),
    (
        'call_star',
        'function(*args, **kwargs)',
        'eval',
        False,
        (
            'Name:args:Load@1:10 Name:function:Load@1:0 Name:kwargs:Load@1:18 Starred::Load@1:9 '
        ),
    ),
    (
        'fstring',
        'f"{(x := value)} {obj.field}"',
        'eval',
        False,
        (
            'Attribute:field:Load@1:18 Name:obj:Load@1:18 Name:value:Load@1:9 Name:x:Store@1:4 '
        ),
    ),
    (
        'type_alias',
        'type Pair[T] = tuple[T, T]\n',
        'exec',
        False,
        (
            'Name:Pair:Store@1:5 Name:T:Load@1:21 Name:T:Load@1:24 Name:tuple:Load@1:15 '
            'Subscript::Load@1:15 Tuple::Load@1:21 '
        ),
    ),
    (
        'pattern',
        'match value:\n    case [head, *tail] if head:\n        result = tail\n    case Point(x=a, y=b):\n        result = a + b\n',
        'exec',
        False,
        (
            'Name:Point:Load@4:9 Name:a:Load@5:17 Name:b:Load@5:21 Name:head:Load@2:26 '
            'Name:result:Store@3:8 Name:result:Store@5:8 Name:tail:Load@3:17 Name:value:Load@1:6 '
        ),
    ),
    (
        'decorators',
        '@decorate(option)\ndef f(arg: Type=default) -> Result:\n    local = arg\n    return local\n',
        'exec',
        False,
        (
            'Name:Result:Load@2:28 Name:Type:Load@2:11 Name:arg:Load@3:12 Name:decorate:Load@1:1 '
            'Name:default:Load@2:16 Name:local:Load@4:11 Name:local:Store@3:4 Name:option:Load@1:10 '
        ),
    ),
    (
        'class',
        'class C(Base, option=value):\n    field = value\n',
        'exec',
        False,
        (
            'Name:Base:Load@1:8 Name:field:Store@2:4 Name:value:Load@1:21 Name:value:Load@2:12 '
        ),
    ),
    (
        'comprehension_target_subscript',
        '[x for data[index] in rows for x in data]',
        'eval',
        False,
        (
            'Name:data:Load@1:7 Name:data:Load@1:36 Name:index:Load@1:12 Name:rows:Load@1:22 '
            'Name:x:Load@1:1 Name:x:Store@1:31 Subscript::Store@1:7 '
        ),
    ),
    (
        'interactive',
        'a, *b = values\n',
        'single',
        False,
        (
            'Name:a:Store@1:0 Name:b:Store@1:4 Name:values:Load@1:8 Starred::Store@1:3 Tuple::Store@1:0 '
        ),
    ),
    (
        'type_comments',
        'a, b = value # type: tuple[int, int]\nfor a, b in pairs: # type: tuple[int, int]\n    pass\n',
        'exec',
        True,
        (
            'Name:a:Store@1:0 Name:a:Store@2:4 Name:b:Store@1:3 Name:b:Store@2:7 Name:pairs:Load@2:12 '
            'Name:value:Load@1:7 Tuple::Store@1:0 Tuple::Store@2:4 '
        ),
    ),
    (
        'unicode_crlf',
        'café, λ = values\r\nobj.é = café\r\n',
        'exec',
        False,
        (
            'Attribute:é:Store@2:0 Name:café:Load@2:9 Name:café:Store@1:0 Name:obj:Load@2:0 '
            'Name:values:Load@1:12 Name:λ:Store@1:7 Tuple::Store@1:0 '
        ),
    ),
    (
        'function_type',
        '(tuple[int, str], list[T]) -> Result',
        'func_type',
        False,
        (
            'Name:Result:Load@1:30 Name:T:Load@1:23 Name:int:Load@1:7 Name:list:Load@1:18 '
            'Name:str:Load@1:12 Name:tuple:Load@1:1 Subscript::Load@1:1 Subscript::Load@1:18 '
            'Tuple::Load@1:7 '
        ),
    ),
]


for name, source, mode, comments, expected in CASES:
    tree = ast.parse(source, mode=mode, type_comments=comments)
    actual = sorted(
        "%s:%s:%s@%d:%d" % (
            type(node).__name__,
            getattr(node, "id", getattr(node, "attr", "")),
            type(node.ctx).__name__,
            node.lineno,
            node.col_offset,
        )
        for node in ast.walk(tree)
        if hasattr(node, "ctx")
    )
    assert actual == sorted(expected.split()), (name, actual, expected)
    if mode != "func_type":
        compile(tree, "<target-contexts>", mode)

print("AST target contexts: ok")
