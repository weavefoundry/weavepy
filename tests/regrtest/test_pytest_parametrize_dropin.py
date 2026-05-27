"""Drop-in test — pytest parametrize matrices + fixture scopes.

RFC 0031 extends the bundled ``pytest`` shim with the four features
real-world test suites depend on:

* ``@pytest.mark.parametrize`` matrix expansion (stacking multiple
  parametrize markers makes a Cartesian product).
* Fixture scopes (``function`` / ``class`` / ``module`` /
  ``session``) with caching so a ``scope='session'`` fixture only
  builds once.
* ``yield``-style fixtures with deterministic teardown.
* ``request.addfinalizer`` + autouse fixtures.
* ``pytest.param(value, id=..., marks=...)`` rows.
"""

import pytest


def assert_eq(a, b, label=''):
    if a != b:
        raise AssertionError('{}: {!r} != {!r}'.format(label or 'eq', a, b))


def assert_true(cond, label=''):
    if not cond:
        raise AssertionError('{}: expected True'.format(label or 'true'))


def test_parametrize_single_dim():
    pieces = []

    @pytest.mark.parametrize('value', [1, 2, 3])
    def t(value):
        pieces.append(value)

    class FakeParent:
        nodeid = '<test>'
        parent = None

    items = pytest._expand_parametrize('t', FakeParent(), t, t._pytest_marks)
    assert_eq(len(items), 3, '3 items from 3 values')
    for it in items:
        it.runtest()
    assert_eq(sorted(pieces), [1, 2, 3])


def test_parametrize_cartesian_matrix():
    seen = []

    @pytest.mark.parametrize('a', [1, 2])
    @pytest.mark.parametrize('b', ['x', 'y'])
    def t(a, b):
        seen.append((a, b))

    class FakeParent:
        nodeid = '<test>'
        parent = None

    items = pytest._expand_parametrize('t', FakeParent(), t, t._pytest_marks)
    assert_eq(len(items), 4, 'cartesian matrix is 2*2 = 4')
    for it in items:
        it.runtest()
    expected = {(1, 'x'), (1, 'y'), (2, 'x'), (2, 'y')}
    assert_eq(set(seen), expected)


def test_parametrize_tuple_unpacking():
    seen = []

    @pytest.mark.parametrize('a,b,expected', [
        (1, 2, 3),
        (5, 5, 10),
        (0, 0, 0),
    ])
    def t(a, b, expected):
        seen.append((a, b, expected))

    class FakeParent:
        nodeid = '<test>'
        parent = None

    items = pytest._expand_parametrize('t', FakeParent(), t, t._pytest_marks)
    assert_eq(len(items), 3)
    for it in items:
        it.runtest()
    assert_eq(sorted(seen), [(0, 0, 0), (1, 2, 3), (5, 5, 10)])


def test_parametrize_param_helper_with_id():
    @pytest.mark.parametrize('value', [
        pytest.param(1, id='one'),
        pytest.param(2, id='two'),
        pytest.param(3, id='three'),
    ])
    def t(value):
        pass

    class FakeParent:
        nodeid = '<test>'
        parent = None

    items = pytest._expand_parametrize('t', FakeParent(), t, t._pytest_marks)
    ids = [it._param_id for it in items]
    assert_eq(ids, ['one', 'two', 'three'])


def test_fixture_scope_session_caches():
    pytest._FIXTURE_REGISTRY.clear()
    pytest._FIXTURE_MANAGER.reset_scope('session')
    pytest._FIXTURE_MANAGER.reset_scope('module')
    pytest._FIXTURE_MANAGER.reset_scope('function')
    counter = {'n': 0}

    @pytest.fixture(scope='session')
    def heavy():
        counter['n'] += 1
        return counter['n']

    class FakeNode:
        nodeid = '<test>'
        parent = None

    class FakeItem:
        _fixture_params = {}
        parent = FakeNode()

    m = pytest._FIXTURE_MANAGER
    first = pytest._resolve_fixture('heavy', m, FakeItem(), FakeNode())
    second = pytest._resolve_fixture('heavy', m, FakeItem(), FakeNode())
    assert_eq(first, 1, 'first build runs the body once')
    assert_eq(second, 1, 'session-scoped fixture cached across requests')
    assert_eq(counter['n'], 1, 'body invoked exactly once')
    m.reset_scope('session')
    third = pytest._resolve_fixture('heavy', m, FakeItem(), FakeNode())
    assert_eq(third, 2, 'reset_scope re-runs the body')


def test_yield_fixture_teardown():
    pytest._FIXTURE_REGISTRY.clear()
    pytest._FIXTURE_MANAGER.reset_scope('session')
    pytest._FIXTURE_MANAGER.reset_scope('function')
    log = []

    @pytest.fixture
    def resource():
        log.append('open')
        yield 'handle'
        log.append('close')

    class FakeNode:
        nodeid = '<test>'
        parent = None

    class FakeItem:
        _fixture_params = {}
        parent = FakeNode()

    m = pytest._FIXTURE_MANAGER
    v = pytest._resolve_fixture('resource', m, FakeItem(), FakeNode())
    assert_eq(v, 'handle')
    m.reset_scope('function')
    assert_eq(log, ['open', 'close'], 'yield fixture teardown fires on scope reset')


def test_addfinalizer_runs_in_lifo():
    pytest._FIXTURE_REGISTRY.clear()
    pytest._FIXTURE_MANAGER.reset_scope('function')
    log = []

    @pytest.fixture
    def widgets(request):
        request.addfinalizer(lambda: log.append('one'))
        request.addfinalizer(lambda: log.append('two'))
        return 'ok'

    class FakeNode:
        nodeid = '<test>'
        parent = None

    class FakeItem:
        _fixture_params = {}
        parent = FakeNode()

    m = pytest._FIXTURE_MANAGER
    pytest._resolve_fixture('widgets', m, FakeItem(), FakeNode())
    m.reset_scope('function')
    assert_eq(log, ['two', 'one'], 'finalizers run in LIFO order')


def test_indirect_fixture_via_resolver():
    """`getfixturevalue` lets one fixture pull another by name."""
    pytest._FIXTURE_REGISTRY.clear()
    pytest._FIXTURE_MANAGER.reset_scope('function')

    @pytest.fixture
    def inner():
        return 'inner-value'

    @pytest.fixture
    def outer(request):
        return request.getfixturevalue('inner') + ':outer'

    class FakeNode:
        nodeid = '<test>'
        parent = None

    class FakeItem:
        _fixture_params = {}
        parent = FakeNode()

    m = pytest._FIXTURE_MANAGER
    v = pytest._resolve_fixture('outer', m, FakeItem(), FakeNode())
    assert_eq(v, 'inner-value:outer')


def main():
    tests = [v for k, v in globals().items()
             if k.startswith('test_') and callable(v)]
    failures = 0
    for fn in tests:
        try:
            fn()
            print('OK   {}'.format(fn.__name__))
        except Exception as exc:
            failures += 1
            print('FAIL {}: {}'.format(fn.__name__, exc))
    if failures:
        raise SystemExit(1)
    print('{} pytest parametrize/fixture tests passed'.format(len(tests)))


if __name__ == '__main__':
    main()
