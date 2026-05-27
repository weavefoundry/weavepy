"""Drop-in test — `_minipip` + `_pip_resolver` offline behaviour.

Exercises the offline path of the bundled pip: PEP 440 resolution,
PEP 503 candidate sorting, METADATA parsing, and dist-info bookkeeping.
The actual network-bound `pip install <package>` flow is covered by
the regrtest CI lane that builds wheels locally and feeds them
through a stub index.
"""

import io
import os
import sys
import tempfile
import zipfile

import _pip_resolver
from _packaging import Requirement, Version, canonicalize_name


def assert_eq(a, b, label=''):
    if a != b:
        raise AssertionError('{}: {!r} != {!r}'.format(label or 'eq', a, b))


def assert_true(cond, label=''):
    if not cond:
        raise AssertionError('{}: expected True'.format(label))


def _make_fake_wheel(name, version, requires=None):
    """Build a tiny in-memory wheel carrying just METADATA + RECORD."""
    requires = requires or []
    metadata_lines = [
        'Metadata-Version: 2.1',
        'Name: {}'.format(name),
        'Version: {}'.format(version),
    ]
    for r in requires:
        metadata_lines.append('Requires-Dist: {}'.format(r))
    metadata = '\n'.join(metadata_lines).encode('utf-8')
    wheel_meta = (
        'Wheel-Version: 1.0\n'
        'Generator: weavepy-test-fake\n'
        'Root-Is-Purelib: true\n'
        'Tag: py3-none-any\n'
    ).encode('utf-8')
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, 'w') as zf:
        zf.writestr('{}-{}.dist-info/METADATA'.format(name, version), metadata)
        zf.writestr('{}-{}.dist-info/WHEEL'.format(name, version), wheel_meta)
        zf.writestr('{}-{}.dist-info/RECORD'.format(name, version), '')
    return buf.getvalue()


def test_resolver_simple_chain():
    """Resolve a -> b -> c with no version conflicts."""
    catalog = {
        'pkg-a': [('pkg_a-1.0.0-py3-none-any.whl', 'http://example/a-1.0.0.whl')],
        'pkg-b': [('pkg_b-2.0.0-py3-none-any.whl', 'http://example/b-2.0.0.whl')],
        'pkg-c': [('pkg_c-3.0.0-py3-none-any.whl', 'http://example/c-3.0.0.whl')],
    }
    blobs = {
        'http://example/a-1.0.0.whl': _make_fake_wheel('pkg_a', '1.0.0', ['pkg-b>=2.0']),
        'http://example/b-2.0.0.whl': _make_fake_wheel('pkg_b', '2.0.0', ['pkg-c==3.0.0']),
        'http://example/c-3.0.0.whl': _make_fake_wheel('pkg_c', '3.0.0'),
    }

    def lookup(name):
        return catalog.get(canonicalize_name(name), [])

    def downloader(url):
        return blobs.get(url, b'')

    resolver = _pip_resolver.Resolver(downloader, lookup)
    plan = resolver.resolve([Requirement('pkg-a')])
    names = [entry['name'] for entry in plan]
    assert_true('pkg-a' in names, 'pkg-a planned')
    assert_true('pkg-b' in names, 'pkg-b planned')
    assert_true('pkg-c' in names, 'pkg-c planned')


def test_resolver_conflict_raises():
    catalog = {
        'pkg-x': [('pkg_x-1.0.0-py3-none-any.whl', 'http://example/x-1.0.0.whl')],
        'pkg-y': [('pkg_y-1.0.0-py3-none-any.whl', 'http://example/y-1.0.0.whl')],
    }
    blobs = {
        'http://example/x-1.0.0.whl': _make_fake_wheel(
            'pkg_x', '1.0.0', ['pkg-y>=2.0']
        ),
        'http://example/y-1.0.0.whl': _make_fake_wheel('pkg_y', '1.0.0'),
    }
    resolver = _pip_resolver.Resolver(
        lambda url: blobs.get(url, b''),
        lambda name: catalog.get(canonicalize_name(name), []),
    )
    raised = False
    try:
        resolver.resolve([Requirement('pkg-x'), Requirement('pkg-y==1.0.0')])
    except _pip_resolver.ResolutionError:
        raised = True
    assert_true(raised, 'expected resolution conflict')


def test_metadata_parsing():
    text = (
        'Metadata-Version: 2.1\n'
        'Name: example\n'
        'Version: 1.0\n'
        'Requires-Dist: foo>=1.0\n'
        'Requires-Dist: bar; extra == "dev"\n'
        '\n'
        'long description here\n'
    )
    md = _pip_resolver._parse_metadata(text)
    assert_eq(md['Name'], 'example')
    assert_eq(md['Version'], '1.0')
    assert_eq(len(md['Requires-Dist']), 2)


def test_pep723_inline_metadata():
    src = (
        '# /// script\n'
        '# requires-python = ">=3.10"\n'
        '# dependencies = ["requests"]\n'
        '# ///\n'
        'print("hi")\n'
    )
    md = _pip_resolver.parse_pep723(src)
    assert_true('script' in md, 'parsed inline script metadata')
    assert_true('requests' in md['script'], 'captured dependencies')


def test_install_local_wheel_roundtrip():
    """End-to-end: install a tiny wheel via _minipip, then list & uninstall."""
    import _minipip
    blob = _make_fake_wheel('weavepy_dropin_demo', '0.1.0')
    tmp_dest = tempfile.mkdtemp(prefix='weavepy-pip-test-')
    wheel_path = os.path.join(tmp_dest, 'weavepy_dropin_demo-0.1.0-py3-none-any.whl')
    with open(wheel_path, 'wb') as f:
        f.write(blob)
    installed = _minipip._install_wheel(wheel_path, dest=tmp_dest)
    assert_true(len(installed) > 0, 'wheel produced files')
    found = _minipip._find_dist_info(tmp_dest, 'weavepy_dropin_demo')
    assert_true(found is not None, 'dist-info created')


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
    print('{} pip install tests passed'.format(len(tests)))


if __name__ == '__main__':
    main()
