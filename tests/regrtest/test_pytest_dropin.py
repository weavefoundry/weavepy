"""Drop-in test — bundled `pytest`.

Exercises the slice of the pytest API our in-tree ``_pytest`` shim
guarantees: ``pytest.raises`` / ``pytest.warns`` / ``pytest.approx``,
the ``@pytest.fixture`` / ``@pytest.mark.*`` decorators, and the CLI
runner discovering ``test_*`` items in a temp directory.
"""

import os
import sys
import tempfile

import pytest


def assert_eq(a, b, label=''):
    if a != b:
        raise AssertionError('{}: {!r} != {!r}'.format(label or 'eq', a, b))


def assert_true(cond, label=''):
    if not cond:
        raise AssertionError('{}: expected True'.format(label))


def test_raises_context_manager():
    with pytest.raises(ValueError):
        raise ValueError('boom')
    # No exception → DID NOT RAISE.
    raised = False
    try:
        with pytest.raises(ValueError):
            pass
    except AssertionError:
        raised = True
    assert_true(raised, 'pytest.raises must complain when no exception is raised')


def test_raises_match():
    with pytest.raises(ValueError, match='boom'):
        raise ValueError('big boom')


def test_warns():
    import warnings
    with pytest.warns(UserWarning):
        warnings.warn('hi', UserWarning)


def test_approx():
    assert_eq(0.1 + 0.2, pytest.approx(0.3))
    assert_true([0.1 + 0.2, 0.4] == pytest.approx([0.3, 0.4]))


def test_skip_xfail_markers():
    @pytest.mark.skip(reason='not yet')
    def _skipped():
        raise RuntimeError('should not run')

    @pytest.mark.xfail(reason='known broken')
    def _xfailed():
        raise AssertionError('expected fail')

    # Markers are stored in `_pytest_marks` for the runner to consume;
    # asserting they're attached is enough.
    assert_true(any(m.name == 'skip' for m in _skipped._pytest_marks))
    assert_true(any(m.name == 'xfail' for m in _xfailed._pytest_marks))


def test_fixture_decorator():
    @pytest.fixture
    def something():
        return 'x'

    assert_true(hasattr(something, '_pytest_fixture'))
    assert_eq(something._pytest_fixture['scope'], 'function')


def test_runner_discovers_tests():
    """Spawn a tiny test file in a tempdir, run it through pytest.main."""
    src = (
        'def test_passes():\n'
        '    assert 1 + 1 == 2\n'
        '\n'
        'def test_assert_fails():\n'
        '    assert 1 == 2\n'
        '\n'
        'def test_with_fixture(tmp_path):\n'
        '    assert tmp_path is not None\n'
    )
    tmpdir = tempfile.mkdtemp(prefix='weavepy-pytest-')
    test_path = os.path.join(tmpdir, 'test_subject.py')
    with open(test_path, 'w') as f:
        f.write(src)
    rc = pytest.main([tmpdir, '-q'])
    # We expect TESTS_FAILED because the bundled test asserts 1==2.
    assert_eq(rc, pytest.ExitCode.TESTS_FAILED,
              'pytest.main should report failure for asserted-false test')


def test_runner_only_passing_tests():
    """All-passing test file → ExitCode.OK."""
    src = (
        'def test_one():\n'
        '    assert True\n'
        '\n'
        'def test_two():\n'
        '    assert 1 + 1 == 2\n'
    )
    tmpdir = tempfile.mkdtemp(prefix='weavepy-pytest-')
    test_path = os.path.join(tmpdir, 'test_ok.py')
    with open(test_path, 'w') as f:
        f.write(src)
    rc = pytest.main([tmpdir, '-q'])
    assert_eq(rc, pytest.ExitCode.OK,
              'pytest.main should report OK when all tests pass')


def test_runner_no_tests():
    """Empty directory → ExitCode.NO_TESTS_COLLECTED."""
    tmpdir = tempfile.mkdtemp(prefix='weavepy-pytest-empty-')
    rc = pytest.main([tmpdir, '-q'])
    assert_eq(rc, pytest.ExitCode.NO_TESTS_COLLECTED,
              'empty dir should report NO_TESTS_COLLECTED')


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
    print('{} pytest drop-in tests passed'.format(len(tests)))


if __name__ == '__main__':
    main()
