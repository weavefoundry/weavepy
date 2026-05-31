"""``test.libregrtest.cmdline`` — argument parsing for ``-m test``.

A faithful subset of CPython 3.13's
``Lib/test/libregrtest/cmdline.py``. Only the flags real CI invocation
lines use are modelled; anything we don't recognise is accepted and
ignored (via ``parse_known_args``) so a verbatim CPython command line
still runs.
"""

import argparse


RESOURCE_NAMES = (
    'audio', 'curses', 'largefile', 'network', 'decimal', 'cpu',
    'subprocess', 'urlfetch', 'gui', 'walltime',
)


class Namespace(argparse.Namespace):
    def __init__(self, **kwargs):
        self.verbose = 0
        self.quiet = False
        self.fail_fast = False
        self.fail_env_changed = False
        self.single = False
        self.fromfile = None
        self.exclude = False
        self.list_tests = False
        self.list_cases = False
        self.match_tests = None
        self.use_resources = None
        self.randomize = False
        self.random_seed = None
        self.jobs = 0
        self.rerun = False
        self.forever = False
        self.start = None
        self.tests = []
        self.testdir = None
        super().__init__(**kwargs)


def _create_parser():
    parser = argparse.ArgumentParser(
        prog="python -m test",
        description="Run the WeavePy / CPython regression test suite.")
    parser.add_argument('-v', '--verbose', action='count', default=0,
                        help="run tests in verbose mode")
    parser.add_argument('-q', '--quiet', action='store_true',
                        help="no output unless a test fails")
    parser.add_argument('-x', '--exclude', action='store_true',
                        help="arguments are tests to *exclude*")
    parser.add_argument('-s', '--single', action='store_true',
                        help="run a single test then write the next to a file")
    parser.add_argument('-G', '--failfast', action='store_true',
                        dest='fail_fast',
                        help="fail as soon as a test fails")
    parser.add_argument('-w', '--rerun', '--verbose2', action='store_true',
                        dest='rerun',
                        help="re-run failed tests in verbose mode")
    parser.add_argument('-f', '--fromfile', metavar='FILE',
                        help="read names of tests to run from FILE")
    parser.add_argument('--fail-env-changed', action='store_true',
                        help="mark tests that change the env as failures")
    parser.add_argument('-j', '--multiprocess', metavar='N', type=int,
                        default=0, dest='jobs',
                        help="run N tests in parallel worker processes")
    parser.add_argument('-u', '--use', metavar='RES1,RES2,...',
                        action='append', dest='use',
                        help="enable the given resources")
    parser.add_argument('-m', '--match', metavar='PAT', action='append',
                        dest='match_tests',
                        help="match test cases against PAT")
    parser.add_argument('-r', '--randomize', action='store_true',
                        help="randomize test order (honoured as no-op)")
    parser.add_argument('--randseed', metavar='SEED', type=int,
                        dest='random_seed')
    parser.add_argument('--list-tests', action='store_true',
                        help="only write the names of selected tests")
    parser.add_argument('--list-cases', action='store_true',
                        help="only write the test case identifiers")
    parser.add_argument('--testdir', metavar='DIR',
                        help="directory holding the test_*.py files")
    parser.add_argument('-F', '--forever', action='store_true',
                        help="run the selected tests in a loop (no-op once)")
    parser.add_argument('tests', nargs='*', help="names of tests to run")
    return parser


def _parse_resources(ns):
    resources = []
    for value in (getattr(ns, 'use', None) or []):
        for r in value.split(','):
            r = r.strip()
            if not r:
                continue
            if r == 'all':
                resources = list(RESOURCE_NAMES)
                continue
            if r == 'none':
                resources = []
                continue
            if r.startswith('-'):
                name = r[1:]
                if name in resources:
                    resources.remove(name)
            else:
                if r not in resources:
                    resources.append(r)
    return resources or None


def parse_args(args=None):
    """Parse ``-m test`` arguments into a :class:`Namespace`.

    Unknown flags are tolerated (CPython compatibility): a real
    invocation line is accepted verbatim.
    """
    parser = _create_parser()
    ns = Namespace()
    parsed, _unknown = parser.parse_known_args(args, namespace=ns)
    parsed.use_resources = _parse_resources(parsed)
    return parsed
