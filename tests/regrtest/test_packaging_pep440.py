"""Drop-in test — PEP 440 versions + PEP 503 names + PEP 508 markers.

Exercises the in-tree ``_packaging`` (and the ``packaging.*`` aliases)
that back ``pip``. The point is to assert that the same Python source
behaves the same on WeavePy as it would on CPython once ``packaging``
is installed.
"""

from _packaging import (
    InvalidMarker,
    InvalidRequirement,
    InvalidSpecifier,
    InvalidVersion,
    Marker,
    Requirement,
    SpecifierSet,
    Version,
    canonicalize_name,
    compatible_tags,
    default_environment,
    parse_wheel_filename,
    wheel_is_compatible,
    wheel_score,
)
from _packaging import _platform_tags


def assert_eq(a, b, label=''):
    if a != b:
        raise AssertionError('{}: {!r} != {!r}'.format(label or 'eq', a, b))


def assert_true(cond, label=''):
    if not cond:
        raise AssertionError('{}: expected True, got False'.format(label or 'true'))


def assert_false(cond, label=''):
    if cond:
        raise AssertionError('{}: expected False, got True'.format(label or 'false'))


def assert_raises(exc, fn, *args, **kwargs):
    try:
        fn(*args, **kwargs)
    except exc:
        return
    raise AssertionError('{} not raised'.format(exc.__name__))


def test_version_basic():
    assert_eq(str(Version('1.4.0')), '1.4.0', 'roundtrip')
    assert_true(Version('1.4.0') < Version('1.4.1'), '1.4.0 < 1.4.1')
    assert_true(Version('1.4.0a1') < Version('1.4.0'), 'pre < release')
    assert_eq(Version('1.0'), Version('1.0.0'), 'trailing zeros')
    assert_true(Version('2!1.0') > Version('1.99'), 'epoch wins')
    assert_eq(Version('1.0').public, '1.0')
    assert_eq(Version('1.0+local.1').public, '1.0')
    assert_true(Version('1.4.0.post1').is_postrelease)
    assert_true(Version('1.4.0.dev1').is_prerelease)
    assert_raises(InvalidVersion, Version, 'not-a-version!')


def test_specifier_set():
    s = SpecifierSet('>=1.0,<2.0')
    assert_true(s.contains('1.5'))
    assert_false(s.contains('2.0'))
    assert_false(s.contains('0.9'))
    assert_true(SpecifierSet('==1.4.*').contains('1.4.99'))
    assert_false(SpecifierSet('==1.4.*').contains('1.5.0'))
    assert_true(SpecifierSet('~=2.2').contains('2.5.0'))
    assert_false(SpecifierSet('~=2.2').contains('3.0.0'))
    assert_true(SpecifierSet('!=1.0').contains('1.0.1'))
    assert_raises(InvalidSpecifier, SpecifierSet, 'wat')


def test_requirement():
    r = Requirement('numpy[fast]>=1.20')
    assert_eq(r.name, 'numpy')
    assert_eq(r.extras, {'fast'})
    assert_true(r.specifier.contains('1.21'))
    r2 = Requirement('foo>=1.0; python_version >= "3.10"')
    assert_true(r2.marker is not None)
    env = default_environment()
    env['python_version'] = '3.13'
    assert_true(r2.applies_to(env))
    env['python_version'] = '3.5'
    assert_false(r2.applies_to(env))
    assert_raises(InvalidRequirement, Requirement, '!!!')


def test_marker():
    m = Marker('python_version >= "3.10" and sys_platform == "linux"')
    env = default_environment()
    env['python_version'] = '3.13'
    env['sys_platform'] = 'linux'
    assert_true(m.evaluate(env))
    env['sys_platform'] = 'darwin'
    assert_false(m.evaluate(env))
    m_or = Marker('python_version < "3.0" or python_version >= "3.10"')
    env['python_version'] = '3.13'
    assert_true(m_or.evaluate(env))
    assert_raises(InvalidMarker, Marker, 'totally not a marker')


def test_canonicalize_name():
    assert_eq(canonicalize_name('Foo.Bar_Baz'), 'foo-bar-baz')
    assert_eq(canonicalize_name('NUMPY'), 'numpy')


def test_wheel_filename():
    name, version, build, tags = parse_wheel_filename(
        'numpy-2.0.0-cp313-cp313-manylinux_2_17_x86_64.whl'
    )
    assert_eq(name, 'numpy')
    assert_eq(version, '2.0.0')
    assert_eq(build, None)
    assert_true(any(t.python == 'cp313' for t in tags))
    # Tag-based compatibility & scoring.
    assert_true(wheel_is_compatible('foo-1.0-py3-none-any.whl'))
    assert_true(wheel_score('numpy-2.0.0-cp313-cp313-macosx_11_0_arm64.whl') > 0)


def test_wheel_matrix_wave5():
    # RFC 0047 (wave 5): the full manylinux / macOS / musllinux platform
    # matrix a real numpy/pandas wheel ships. Parameterised so the test is
    # host-independent.
    linux = _platform_tags('linux', 'x86_64')
    assert_true('manylinux_2_17_x86_64' in linux, 'manylinux present')
    assert_true('manylinux2014_x86_64' in linux, 'legacy manylinux present')
    assert_true('musllinux_1_1_x86_64' in linux, 'musllinux_1_1 present')
    assert_true('musllinux_1_2_x86_64' in linux, 'musllinux_1_2 present')
    mac = _platform_tags('darwin', 'arm64')
    assert_true('macosx_11_0_arm64' in mac, 'macOS arm64 present')
    assert_true('macosx_11_0_universal2' in mac, 'macOS universal2 present')


def test_wheel_provenance_wave5():
    # RFC 0047 (wave 5): the WeavePy provenance interpreter tag. Only
    # meaningful when running under WeavePy (it is gated on the
    # interpreter); on stock CPython `compatible_tags` never emits it.
    import sys
    if sys.implementation.name != 'weavepy':
        return
    accept = {(t.python, t.abi, t.platform) for t in compatible_tags()}
    assert_true(('weavepy', 'none', 'any') in accept, 'weavepy-none-any accepted')
    assert_true(wheel_is_compatible('foo-1.0-weavepy-none-any.whl'),
                'weavepy provenance wheel compatible')
    # A provenance wheel outranks the stock cp313 wheel it shadows.
    prov = wheel_score('numpy-2.0.0-weavepy-cp313-manylinux_2_17_x86_64.whl')
    stock = wheel_score('numpy-2.0.0-cp313-cp313-manylinux_2_17_x86_64.whl')
    assert_true(prov > stock, 'provenance outranks stock ({} vs {})'.format(prov, stock))


def main():
    tests = [v for k, v in globals().items() if k.startswith('test_')]
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
    print('{} tests passed'.format(len(tests)))


if __name__ == '__main__':
    main()
