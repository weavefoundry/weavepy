"""WeavePy ``doctest`` — a faithful subset of CPython 3.13's ``doctest``.

Covers what stdlib self-tests and ``test.support.run_doctest`` need:

* ``Example`` / ``DocTest`` data classes.
* ``DocTestParser`` — a line-based parser (robust on a subset regex
  engine) that recognises ``>>>``/``...`` prompts, ``want`` output,
  ``<BLANKLINE>``, expected exceptions and ``# doctest:`` option
  directives.
* ``DocTestFinder`` — walks a module's functions / classes / methods and
  its ``__test__`` mapping.
* ``DocTestRunner`` / ``DebugRunner`` — execute examples (in interactive
  / "single" mode so expression results echo through ``sys.displayhook``)
  and compare with ``OutputChecker``.
* ``OutputChecker`` — ``ELLIPSIS`` / ``NORMALIZE_WHITESPACE`` /
  ``IGNORE_EXCEPTION_DETAIL`` / ``DONT_ACCEPT_TRUE_FOR_1`` /
  ``DONT_ACCEPT_BLANKLINE`` / ``SKIP``.
* ``testmod`` / ``testfile`` / ``run_docstring_examples`` front ends.
* The ``DocTestSuite`` / ``DocFileSuite`` ``unittest`` bridge.
"""

import inspect
import os
import sys
import traceback
import unittest
from io import StringIO

__all__ = [
    "register_optionflag",
    "DONT_ACCEPT_TRUE_FOR_1",
    "DONT_ACCEPT_BLANKLINE",
    "NORMALIZE_WHITESPACE",
    "ELLIPSIS",
    "SKIP",
    "IGNORE_EXCEPTION_DETAIL",
    "COMPARISON_FLAGS",
    "REPORT_UDIFF",
    "REPORT_CDIFF",
    "REPORT_NDIFF",
    "REPORT_ONLY_FIRST_FAILURE",
    "REPORTING_FLAGS",
    "FAIL_FAST",
    "Example",
    "DocTest",
    "DocTestParser",
    "DocTestFinder",
    "DocTestRunner",
    "OutputChecker",
    "DocTestFailure",
    "UnexpectedException",
    "DebugRunner",
    "testmod",
    "testfile",
    "run_docstring_examples",
    "DocTestSuite",
    "DocFileSuite",
    "DocFileTest",
    "set_unittest_reportflags",
    "DocTestCase",
    "ELLIPSIS_MARKER",
    "BLANKLINE_MARKER",
]


# ---------------------------------------------------------------------------
# Option flags
# ---------------------------------------------------------------------------

OPTIONFLAGS_BY_NAME = {}


def register_optionflag(name):
    return OPTIONFLAGS_BY_NAME.setdefault(name, 1 << len(OPTIONFLAGS_BY_NAME))


DONT_ACCEPT_TRUE_FOR_1 = register_optionflag('DONT_ACCEPT_TRUE_FOR_1')
DONT_ACCEPT_BLANKLINE = register_optionflag('DONT_ACCEPT_BLANKLINE')
NORMALIZE_WHITESPACE = register_optionflag('NORMALIZE_WHITESPACE')
ELLIPSIS = register_optionflag('ELLIPSIS')
SKIP = register_optionflag('SKIP')
IGNORE_EXCEPTION_DETAIL = register_optionflag('IGNORE_EXCEPTION_DETAIL')

COMPARISON_FLAGS = (DONT_ACCEPT_TRUE_FOR_1 |
                    DONT_ACCEPT_BLANKLINE |
                    NORMALIZE_WHITESPACE |
                    ELLIPSIS |
                    SKIP |
                    IGNORE_EXCEPTION_DETAIL)

REPORT_UDIFF = register_optionflag('REPORT_UDIFF')
REPORT_CDIFF = register_optionflag('REPORT_CDIFF')
REPORT_NDIFF = register_optionflag('REPORT_NDIFF')
REPORT_ONLY_FIRST_FAILURE = register_optionflag('REPORT_ONLY_FIRST_FAILURE')
FAIL_FAST = register_optionflag('FAIL_FAST')

REPORTING_FLAGS = (REPORT_UDIFF |
                   REPORT_CDIFF |
                   REPORT_NDIFF |
                   REPORT_ONLY_FIRST_FAILURE |
                   FAIL_FAST)

ELLIPSIS_MARKER = '...'
BLANKLINE_MARKER = '<BLANKLINE>'

_TRACEBACK_HEADERS = (
    'Traceback (most recent call last):',
    'Traceback (innermost last):',
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _extract_future_flags(globs):
    return 0


def _normalize_module(module, depth=2):
    if inspect.ismodule(module):
        return module
    elif isinstance(module, str):
        return __import__(module, globals(), locals(), ["*"])
    elif module is None:
        try:
            frame = sys._getframe(depth)
        except (AttributeError, ValueError):
            return sys.modules.get('__main__')
        return sys.modules[frame.f_globals['__name__']]
    else:
        raise TypeError("Expected a module, string, or None")


def _load_testfile(filename, package, module_relative, encoding):
    if module_relative:
        package = _normalize_module(package, 3)
        filename = _module_relative_path(package, filename)
    with open(filename, encoding=encoding or 'utf-8') as f:
        return f.read(), filename


def _module_relative_path(module, test_path):
    if not inspect.ismodule(module):
        raise TypeError('Expected a module: %r' % module)
    if os.path.isabs(test_path):
        raise ValueError('Module-relative files may not have absolute paths')
    test_path = os.path.join(*test_path.split('/'))
    basedir = os.path.split(getattr(module, '__file__', '.'))[0]
    return os.path.join(basedir, test_path)


def _indent(s, indent=4):
    return '\n'.join(((indent * ' ') + line if line else line)
                     for line in s.split('\n'))


def _exception_traceback(exc_info):
    excout = StringIO()
    exc_type, exc_val, exc_tb = exc_info
    traceback.print_exception(exc_type, exc_val, exc_tb, file=excout)
    return excout.getvalue()


def _strip_exception_details(msg):
    # Mirror CPython: drop everything from the first ':' of the final
    # exception line and the module qualifier from the class name.
    start, end = 0, len(msg)
    i = msg.find("\n")
    if i >= 0:
        end = i
    i = msg.find(':', 0, end)
    if i >= 0:
        end = i
    i = msg.rfind('.', 0, end)
    if i >= 0:
        start = i + 1
    return msg[start:end]


class _SpoofOut:
    """A capture buffer standing in for ``sys.stdout`` while examples run.

    CPython subclasses ``io.StringIO``; WeavePy exposes ``StringIO`` as a
    factory (not a subclassable class), so we wrap an instance and
    delegate. ``getvalue`` appends a trailing newline (matching CPython)
    and ``truncate`` resets the buffer.
    """

    def __init__(self):
        self._buf = StringIO()

    def write(self, s):
        return self._buf.write(s)

    def flush(self):
        pass

    def getvalue(self):
        result = self._buf.getvalue()
        if result and not result.endswith("\n"):
            result += "\n"
        return result

    def truncate(self, size=None):
        self._buf = StringIO()

    def __getattr__(self, name):
        return getattr(self._buf, name)


def _ellipsis_match(want, got):
    """``True`` if ``got`` matches ``want`` with ``...`` wildcards."""
    if ELLIPSIS_MARKER not in want:
        return want == got

    ws = want.split(ELLIPSIS_MARKER)
    assert len(ws) >= 2

    startpos, endpos = 0, len(got)
    w = ws[0]
    if w:
        if got.startswith(w):
            startpos = len(w)
            del ws[0]
        else:
            return False
    w = ws[-1]
    if w:
        if got.endswith(w):
            endpos -= len(w)
            del ws[-1]
        else:
            return False

    if startpos > endpos:
        return False

    for w in ws:
        startpos = got.find(w, startpos, endpos)
        if startpos < 0:
            return False
        startpos += len(w)

    return True


def _comment_options(source_line):
    """Parse a trailing ``# doctest: +FLAG -FLAG`` directive."""
    idx = source_line.find('#')
    if idx < 0:
        return None
    comment = source_line[idx + 1:].strip()
    if not comment.lower().startswith('doctest:'):
        return None
    directive = comment[len('doctest:'):].strip()
    options = {}
    for token in directive.replace(',', ' ').split():
        if not token or token[0] not in '+-':
            raise ValueError('Invalid doctest option directive: %r' % token)
        on = token[0] == '+'
        name = token[1:]
        if name not in OPTIONFLAGS_BY_NAME:
            raise ValueError('Unknown doctest option: %r' % name)
        options[OPTIONFLAGS_BY_NAME[name]] = on
    return options or None


# ---------------------------------------------------------------------------
# Example / DocTest
# ---------------------------------------------------------------------------

class Example:
    def __init__(self, source, want, exc_msg=None, lineno=0, indent=0,
                 options=None):
        if not source.endswith('\n'):
            source += '\n'
        self.source = source
        self.want = want
        self.lineno = lineno
        self.indent = indent
        if options is None:
            options = {}
        self.options = options
        self.exc_msg = exc_msg

    def __eq__(self, other):
        if type(self) is not type(other):
            return NotImplemented
        return (self.source == other.source and
                self.want == other.want and
                self.lineno == other.lineno and
                self.indent == other.indent and
                self.options == other.options and
                self.exc_msg == other.exc_msg)

    def __hash__(self):
        return hash((self.source, self.want, self.lineno, self.indent,
                     self.exc_msg))


class DocTest:
    def __init__(self, examples, globs, name, filename, lineno, docstring):
        self.examples = examples
        self.docstring = docstring
        self.globs = globs.copy()
        self.name = name
        self.filename = filename
        self.lineno = lineno

    def __repr__(self):
        if len(self.examples) == 0:
            examples = 'no examples'
        elif len(self.examples) == 1:
            examples = '1 example'
        else:
            examples = '%d examples' % len(self.examples)
        return '<%s %s from %s:%s (%s)>' % (type(self).__name__, self.name,
                                            self.filename, self.lineno,
                                            examples)

    def __eq__(self, other):
        if type(self) is not type(other):
            return NotImplemented
        return (self.examples == other.examples and
                self.docstring == other.docstring and
                self.globs == other.globs and
                self.name == other.name and
                self.filename == other.filename and
                self.lineno == other.lineno)

    def __hash__(self):
        return hash((self.docstring, self.name, self.filename, self.lineno))

    def __lt__(self, other):
        if not isinstance(other, DocTest):
            return NotImplemented
        return ((self.name, self.filename, self.lineno, id(self)) <
                (other.name, other.filename, other.lineno, id(other)))


# ---------------------------------------------------------------------------
# Parser
# ---------------------------------------------------------------------------

class DocTestParser:
    """Line-based parser extracting interactive examples from a string."""

    def parse(self, string, name='<string>'):
        string = string.expandtabs()
        min_indent = self._min_indent(string)
        if min_indent > 0:
            string = '\n'.join(l[min_indent:] for l in string.split('\n'))

        output = []
        lines = string.split('\n')
        i = 0
        charno = 0
        # Track char offsets per line for lineno bookkeeping.
        line_offsets = []
        off = 0
        for ln in lines:
            line_offsets.append(off)
            off += len(ln) + 1

        prose_start = 0
        while i < len(lines):
            stripped = lines[i].lstrip(' ')
            if stripped.startswith('>>>'):
                # Flush preceding prose.
                if i > prose_start:
                    output.append('\n'.join(lines[prose_start:i]))
                example, i = self._parse_example(lines, i, name)
                output.append(example)
                prose_start = i
            else:
                i += 1
        if prose_start < len(lines):
            output.append('\n'.join(lines[prose_start:]))
        return output

    def get_doctest(self, string, globs, name, filename, lineno):
        return DocTest(self.get_examples(string, name), globs, name,
                       filename, lineno, string)

    def get_examples(self, string, name='<string>'):
        return [x for x in self.parse(string, name) if isinstance(x, Example)]

    # -- internals --

    def _min_indent(self, s):
        indents = []
        for line in s.split('\n'):
            stripped = line.lstrip(' ')
            if stripped:
                indents.append(len(line) - len(stripped))
        return min(indents) if indents else 0

    def _parse_example(self, lines, i, name):
        line = lines[i]
        indent = len(line) - len(line.lstrip(' '))
        lineno = i

        # Collect the source: the PS1 line plus PS2 (...) continuation.
        source_lines = []
        first = line[indent:]
        self._check_prompt(first, '>>>', name, i)
        source_lines.append(first[3:].lstrip(' ') if len(first) > 3 else '')
        # Reconstruct preserving inner indentation after the prompt+space.
        source_lines[-1] = self._after_prompt(first, '>>>')
        i += 1
        while i < len(lines):
            cur = lines[i]
            cur_stripped = cur[indent:] if len(cur) >= indent else cur.lstrip(' ')
            if cur_stripped.startswith('...'):
                source_lines.append(self._after_prompt(cur_stripped, '...'))
                i += 1
            else:
                break
        source = '\n'.join(source_lines)

        # Collect want: following non-blank, non-PS1 lines (indent-stripped).
        want_lines = []
        while i < len(lines):
            cur = lines[i]
            if cur.strip() == '':
                break
            cur_stripped = cur.lstrip(' ')
            if cur_stripped.startswith('>>>'):
                break
            # Strip the example indentation.
            if len(cur) >= indent:
                want_lines.append(cur[indent:])
            else:
                want_lines.append(cur_stripped)
            i += 1

        want = '\n'.join(want_lines)
        if want:
            want += '\n'

        # Option directives from the source lines.
        options = {}
        for sl in source_lines:
            opts = _comment_options(sl)
            if opts:
                options.update(opts)

        # Expected exception?
        exc_msg = None
        want_stripped = want_lines
        if want_stripped:
            head = want_stripped[0].strip()
            if head in _TRACEBACK_HEADERS:
                exc_msg = self._extract_exc_msg(want_stripped)
                want = ''

        example = Example(source, want, exc_msg=exc_msg, lineno=lineno,
                          indent=indent, options=options)
        return example, i

    def _after_prompt(self, line, prompt):
        rest = line[len(prompt):]
        if rest.startswith(' '):
            rest = rest[1:]
        return rest

    def _check_prompt(self, line, prompt, name, lineno):
        rest = line[len(prompt):]
        if rest and not rest.startswith(' '):
            raise ValueError(
                'line %d of the docstring for %s lacks blank after %s: %r' %
                (lineno + 1, name, prompt, line))

    def _extract_exc_msg(self, want_lines):
        # Skip the traceback header and stack frames (indented lines);
        # the message is the first column-0 line that starts with a word
        # character, plus any following lines.
        idx = 1
        while idx < len(want_lines):
            ln = want_lines[idx]
            if ln[:1].isalnum() or ln[:1] == '_':
                break
            idx += 1
        if idx >= len(want_lines):
            return '\n'
        msg = '\n'.join(want_lines[idx:])
        if not msg.endswith('\n'):
            msg += '\n'
        return msg


_default_parser = DocTestParser()


# ---------------------------------------------------------------------------
# Finder
# ---------------------------------------------------------------------------

class DocTestFinder:
    def __init__(self, verbose=False, parser=None, recurse=True,
                 exclude_empty=True):
        self._parser = parser or DocTestParser()
        self._verbose = verbose
        self._recurse = recurse
        self._exclude_empty = exclude_empty

    def find(self, obj, name=None, module=None, globs=None, extraglobs=None):
        if name is None:
            name = getattr(obj, '__name__', None)
            if name is None:
                raise ValueError("DocTestFinder.find: name must be given "
                                 "when obj.__name__ doesn't exist: %r" %
                                 (type(obj),))

        if module is False:
            module = None
        elif module is None:
            module = inspect.getmodule(obj)

        if globs is None:
            if module is None:
                globs = {}
            else:
                globs = dict(getattr(module, '__dict__', {}))
        else:
            globs = dict(globs)
        if extraglobs is not None:
            globs = dict(globs)
            globs.update(extraglobs)
        if '__name__' not in globs:
            globs['__name__'] = '__main__'

        tests = []
        self._find(tests, obj, name, module, set(), globs)
        tests.sort()
        return tests

    def _find(self, tests, obj, name, module, seen, globs):
        if id(obj) in seen:
            return
        seen.add(id(obj))

        test = self._get_test(obj, name, module, globs)
        if test is not None:
            tests.append(test)

        if inspect.ismodule(obj) and self._recurse:
            for valname, val in getattr(obj, '__dict__', {}).items():
                if valname.startswith('_'):
                    continue
                if ((inspect.isroutine(val) or inspect.isclass(val)) and
                        self._from_module(module, val)):
                    self._find(tests, val, '%s.%s' % (name, valname),
                               module, seen, globs)
            # __test__ mapping.
            test_dict = getattr(obj, '__test__', {})
            if test_dict:
                for valname, val in test_dict.items():
                    if isinstance(val, str):
                        val_test = self._parser.get_doctest(
                            val, globs, '%s.__test__.%s' % (name, valname),
                            getattr(module, '__file__', name), 0)
                        if val_test.examples or not self._exclude_empty:
                            tests.append(val_test)
                    else:
                        self._find(tests, val,
                                   '%s.__test__.%s' % (name, valname),
                                   module, seen, globs)

        if inspect.isclass(obj) and self._recurse:
            for valname, val in getattr(obj, '__dict__', {}).items():
                if isinstance(val, (staticmethod, classmethod)):
                    val = val.__func__
                if ((inspect.isroutine(val) or inspect.isclass(val)) and
                        self._from_module(module, val)):
                    self._find(tests, val, '%s.%s' % (name, valname),
                               module, seen, globs)

    def _from_module(self, module, obj):
        if module is None:
            return True
        try:
            obj_mod = inspect.getmodule(obj)
        except Exception:
            return True
        if obj_mod is None:
            return True
        return obj_mod is module

    def _get_test(self, obj, name, module, globs):
        try:
            docstring = inspect.getdoc(obj) or ''
        except Exception:
            docstring = ''
        if not isinstance(docstring, str):
            docstring = str(docstring)
        lineno = self._find_lineno(obj)
        if self._exclude_empty and '>>>' not in docstring:
            return None
        filename = getattr(module, '__file__', None) if module else None
        if filename is None:
            filename = name
        return self._parser.get_doctest(docstring, globs, name, filename,
                                        lineno)

    def _find_lineno(self, obj):
        try:
            if inspect.isfunction(obj) or inspect.ismethod(obj):
                obj = getattr(obj, '__func__', obj)
                code = getattr(obj, '__code__', None)
                if code is not None:
                    return getattr(code, 'co_firstlineno', 0)
            if inspect.isclass(obj):
                _src, lineno = inspect.getsourcelines(obj)
                return lineno
        except Exception:
            pass
        return 0


# ---------------------------------------------------------------------------
# OutputChecker
# ---------------------------------------------------------------------------

class OutputChecker:
    def check_output(self, want, got, optionflags):
        if got == want:
            return True

        if not (optionflags & DONT_ACCEPT_TRUE_FOR_1):
            if (got, want) == ("True\n", "1\n"):
                return True
            if (got, want) == ("False\n", "0\n"):
                return True

        if not (optionflags & DONT_ACCEPT_BLANKLINE):
            want_b = self._strip_blanklines(want)
            got_b = self._strip_trailing_ws(got)
            if got_b == want_b:
                return True
            want = want_b
            got = got_b

        if optionflags & NORMALIZE_WHITESPACE:
            got_n = ' '.join(got.split())
            want_n = ' '.join(want.split())
            if got_n == want_n:
                return True

        if optionflags & ELLIPSIS:
            if _ellipsis_match(want, got):
                return True

        return False

    def _strip_blanklines(self, want):
        out = []
        for line in want.split('\n'):
            if line.strip() == BLANKLINE_MARKER:
                out.append('')
            else:
                out.append(line)
        return '\n'.join(out)

    def _strip_trailing_ws(self, got):
        return '\n'.join(line.rstrip() for line in got.split('\n'))

    def output_difference(self, example, got, optionflags):
        want = example.want
        if not (optionflags & DONT_ACCEPT_BLANKLINE):
            want = self._strip_blanklines(want)
        if want and got:
            return 'Expected:\n%sGot:\n%s' % (_indent(want), _indent(got))
        elif want:
            return 'Expected:\n%sGot nothing\n' % _indent(want)
        elif got:
            return 'Expected nothing\nGot:\n%s' % _indent(got)
        else:
            return 'Expected nothing\nGot nothing\n'


# ---------------------------------------------------------------------------
# Failure exceptions
# ---------------------------------------------------------------------------

class DocTestFailure(Exception):
    def __init__(self, test, example, got):
        self.test = test
        self.example = example
        self.got = got

    def __str__(self):
        return str(self.test)


class UnexpectedException(Exception):
    def __init__(self, test, example, exc_info):
        self.test = test
        self.example = example
        self.exc_info = exc_info

    def __str__(self):
        return str(self.test)


# ---------------------------------------------------------------------------
# Runner
# ---------------------------------------------------------------------------

class TestResults:
    def __init__(self, failed, attempted):
        self.failed = failed
        self.attempted = attempted

    def __iter__(self):
        return iter((self.failed, self.attempted))

    def __getitem__(self, idx):
        return (self.failed, self.attempted)[idx]

    def __len__(self):
        return 2

    def __eq__(self, other):
        return tuple(self) == tuple(other)


class DocTestRunner:
    DIVIDER = "*" * 70

    def __init__(self, checker=None, verbose=None, optionflags=0):
        self._checker = checker or OutputChecker()
        if verbose is None:
            verbose = '-v' in sys.argv
        self._verbose = verbose
        self.optionflags = optionflags
        self.original_optionflags = optionflags
        self.tries = 0
        self.failures = 0
        self.skips = 0
        self._stats = {}
        self._fakeout = _SpoofOut()

    def report_start(self, out, test, example):
        if self._verbose:
            if example.want:
                out('Trying:\n' + _indent(example.source) +
                    'Expecting:\n' + _indent(example.want))
            else:
                out('Trying:\n' + _indent(example.source) +
                    'Expecting nothing\n')

    def report_success(self, out, test, example, got):
        if self._verbose:
            out("ok\n")

    def report_failure(self, out, test, example, got):
        out(self._failure_header(test, example) +
            self._checker.output_difference(example, got, self.optionflags))

    def report_unexpected_exception(self, out, test, example, exc_info):
        out(self._failure_header(test, example) +
            'Exception raised:\n' + _indent(_exception_traceback(exc_info)))

    def _failure_header(self, test, example):
        out = [self.DIVIDER]
        if test.filename:
            if test.lineno is not None and example.lineno is not None:
                lineno = test.lineno + example.lineno + 1
            else:
                lineno = '?'
            out.append('File "%s", line %s, in %s' %
                       (test.filename, lineno, test.name))
        else:
            out.append('Line %s, in %s' % (example.lineno + 1, test.name))
        out.append('Failed example:')
        out.append(_indent(example.source))
        return '\n'.join(out)

    def run(self, test, compileflags=None, out=None, clear_globs=True):
        self.test = test
        if compileflags is None:
            compileflags = _extract_future_flags(test.globs)

        save_stdout = sys.stdout
        if out is None:
            out = save_stdout.write
        sys.stdout = self._fakeout

        save_displayhook = getattr(sys, 'displayhook', None)
        if hasattr(sys, '__displayhook__'):
            sys.displayhook = sys.__displayhook__

        try:
            return self.__run(test, compileflags, out)
        finally:
            sys.stdout = save_stdout
            if save_displayhook is not None:
                sys.displayhook = save_displayhook
            if clear_globs:
                test.globs.clear()

    def __run(self, test, compileflags, out):
        failures = 0
        tries = 0
        skips = 0
        original_optionflags = self.optionflags
        SUCCESS, FAILURE, BOOM = range(3)
        check = self._checker.check_output

        for examplenum, example in enumerate(test.examples):
            quiet = (self.optionflags & REPORT_ONLY_FIRST_FAILURE and
                     failures > 0)

            self.optionflags = original_optionflags
            if example.options:
                for (optionflag, val) in example.options.items():
                    if val:
                        self.optionflags |= optionflag
                    else:
                        self.optionflags &= ~optionflag

            if self.optionflags & SKIP:
                skips += 1
                continue

            tries += 1
            if not quiet:
                self.report_start(out, test, example)

            filename = '<doctest %s[%d]>' % (test.name, examplenum)

            try:
                exec(compile(example.source, filename, "single"), test.globs)
                exception = None
            except KeyboardInterrupt:
                raise
            except BaseException:
                exception = sys.exc_info()

            got = self._fakeout.getvalue()
            self._fakeout.truncate(0)
            outcome = FAILURE

            if exception is None:
                if check(example.want, got, self.optionflags):
                    outcome = SUCCESS
            else:
                formatted_ex = traceback.format_exception_only(
                    exception[0], exception[1])
                exc_msg = formatted_ex[-1] if formatted_ex else ''
                if not quiet:
                    got += _exception_traceback(exception)

                if example.exc_msg is None:
                    outcome = BOOM
                elif check(example.exc_msg, exc_msg, self.optionflags):
                    outcome = SUCCESS
                elif self.optionflags & IGNORE_EXCEPTION_DETAIL:
                    if check(_strip_exception_details(example.exc_msg),
                             _strip_exception_details(exc_msg),
                             self.optionflags):
                        outcome = SUCCESS

            if outcome is SUCCESS:
                if not quiet:
                    self.report_success(out, test, example, got)
            elif outcome is FAILURE:
                if not quiet:
                    self.report_failure(out, test, example, got)
                failures += 1
            elif outcome is BOOM:
                if not quiet:
                    self.report_unexpected_exception(out, test, example,
                                                     exception)
                failures += 1

        self.optionflags = original_optionflags
        self.tries += tries
        self.failures += failures
        self.skips += skips
        self._stats[test.name] = (failures, tries, skips)
        return TestResults(failures, tries)

    def summarize(self, verbose=None):
        if verbose is None:
            verbose = self._verbose
        notests, passed, failed = [], [], []
        total_tries = total_failures = 0
        for name in sorted(self._stats):
            failures, tries, _skips = self._stats[name]
            if tries == 0:
                notests.append(name)
            elif failures == 0:
                passed.append((name, tries))
            else:
                failed.append((name, (failures, tries)))
            total_tries += tries
            total_failures += failures
        if verbose:
            if notests:
                print(len(notests), "items had no tests:")
                for name in sorted(notests):
                    print("   ", name)
            if passed:
                print(len(passed), "items passed all tests:")
                for name, count in sorted(passed):
                    print(" %3d tests in %s" % (count, name))
        if failed:
            print(self.DIVIDER)
            print(len(failed), "items had failures:")
            for name, (failures, tries) in sorted(failed):
                print(" %3d of %3d in %s" % (failures, tries, name))
        if verbose:
            print(total_tries, "tests in", len(self._stats), "items.")
            print(total_tries - total_failures, "passed and",
                  total_failures, "failed.")
        if total_failures:
            print("***Test Failed***", total_failures, "failures.")
        elif verbose:
            print("Test passed.")
        return TestResults(total_failures, total_tries)

    def merge(self, other):
        for name, stats in other._stats.items():
            self._stats[name] = stats
        self.tries += other.tries
        self.failures += other.failures
        self.skips += other.skips


class DebugRunner(DocTestRunner):
    def run(self, test, compileflags=None, out=None, clear_globs=True):
        r = DocTestRunner.run(self, test, compileflags, out, False)
        if clear_globs:
            test.globs.clear()
        return r

    def report_unexpected_exception(self, out, test, example, exc_info):
        raise UnexpectedException(test, example, exc_info)

    def report_failure(self, out, test, example, got):
        raise DocTestFailure(test, example, got)


# ---------------------------------------------------------------------------
# Front ends
# ---------------------------------------------------------------------------

master = None


def testmod(m=None, name=None, globs=None, verbose=None, report=True,
            optionflags=0, extraglobs=None, raise_on_error=False,
            exclude_empty=False):
    global master

    if m is None:
        m = sys.modules.get('__main__')
    if not inspect.ismodule(m):
        raise TypeError("testmod: module required; %r" % (m,))

    if name is None:
        name = getattr(m, '__name__', None) or 'NoName'

    finder = DocTestFinder(exclude_empty=exclude_empty)
    if raise_on_error:
        runner = DebugRunner(verbose=verbose, optionflags=optionflags)
    else:
        runner = DocTestRunner(verbose=verbose, optionflags=optionflags)

    for test in finder.find(m, name, globs=globs, extraglobs=extraglobs):
        runner.run(test)

    if report:
        runner.summarize()

    if master is None:
        master = runner
    else:
        master.merge(runner)

    return TestResults(runner.failures, runner.tries)


def testfile(filename, module_relative=True, name=None, package=None,
             globs=None, verbose=None, report=True, optionflags=0,
             extraglobs=None, raise_on_error=False, parser=None,
             encoding=None):
    global master

    if package and not module_relative:
        raise ValueError("Package may only be specified for module-relative "
                         "paths.")

    text, filename = _load_testfile(filename, package, module_relative,
                                    encoding)

    if name is None:
        name = os.path.basename(filename)

    if globs is None:
        globs = {}
    else:
        globs = globs.copy()
    if extraglobs is not None:
        globs.update(extraglobs)
    if '__name__' not in globs:
        globs['__name__'] = '__main__'

    if parser is None:
        parser = DocTestParser()

    if raise_on_error:
        runner = DebugRunner(verbose=verbose, optionflags=optionflags)
    else:
        runner = DocTestRunner(verbose=verbose, optionflags=optionflags)

    test = parser.get_doctest(text, globs, name, filename, 0)
    runner.run(test)

    if report:
        runner.summarize()

    if master is None:
        master = runner
    else:
        master.merge(runner)

    return TestResults(runner.failures, runner.tries)


def run_docstring_examples(f, globs, verbose=False, name="NoName",
                           compileflags=None, optionflags=0):
    finder = DocTestFinder(verbose=verbose, recurse=False)
    runner = DocTestRunner(verbose=verbose, optionflags=optionflags)
    for test in finder.find(f, name, globs=globs):
        runner.run(test, compileflags=compileflags)


# ---------------------------------------------------------------------------
# unittest bridge
# ---------------------------------------------------------------------------

_unittest_reportflags = 0


def set_unittest_reportflags(flags):
    global _unittest_reportflags
    if (flags & REPORTING_FLAGS) != flags:
        raise ValueError("Only reporting flags allowed", flags)
    old = _unittest_reportflags
    _unittest_reportflags = flags
    return old


class DocTestCase(unittest.TestCase):
    def __init__(self, test, optionflags=0, setUp=None, tearDown=None,
                 checker=None):
        super().__init__()
        self._dt_optionflags = optionflags
        self._dt_checker = checker
        self._dt_test = test
        self._dt_setUp = setUp
        self._dt_tearDown = tearDown

    def setUp(self):
        test = self._dt_test
        self._dt_globs = test.globs.copy()
        if self._dt_setUp is not None:
            self._dt_setUp(test)

    def tearDown(self):
        test = self._dt_test
        if self._dt_tearDown is not None:
            self._dt_tearDown(test)
        test.globs.clear()
        test.globs.update(self._dt_globs)

    def runTest(self):
        test = self._dt_test
        optionflags = self._dt_optionflags
        if not (optionflags & REPORTING_FLAGS):
            optionflags |= _unittest_reportflags
        runner = DocTestRunner(optionflags=optionflags,
                               checker=self._dt_checker, verbose=False)
        out = StringIO()
        results = runner.run(test, out=out.write, clear_globs=False)
        if results.failed:
            raise self.failureException(self.format_failure(out.getvalue()))

    def format_failure(self, err):
        test = self._dt_test
        if test.lineno is None:
            lineno = 'unknown line number'
        else:
            lineno = '%s' % test.lineno
        lname = '.'.join(test.name.split('.')[-1:])
        return ('Failed doctest test for %s\n'
                '  File "%s", line %s, in %s\n\n%s'
                % (test.name, test.filename, lineno, lname, err))

    def id(self):
        return self._dt_test.name

    def __eq__(self, other):
        if type(self) is not type(other):
            return NotImplemented
        return self._dt_test == other._dt_test

    def __hash__(self):
        return hash(self._dt_test)

    def __repr__(self):
        name = self._dt_test.name.split('.')
        return "%s (%s)" % (name[-1], '.'.join(name[:-1]))

    __str__ = __repr__

    def shortDescription(self):
        return "Doctest: " + self._dt_test.name


class SkipDocTestCase(DocTestCase):
    def __init__(self, module):
        self.module = module
        super().__init__(None)

    def setUp(self):
        self.skipTest("DocTestSuite will not work with -O2 and above")

    def test_skip(self):
        pass

    def shortDescription(self):
        return "Skipping tests from %s" % self.module.__name__

    __str__ = shortDescription


def DocTestSuite(module=None, globs=None, extraglobs=None, test_finder=None,
                 **options):
    if test_finder is None:
        test_finder = DocTestFinder()

    module = _normalize_module(module)
    tests = test_finder.find(module, globs=globs, extraglobs=extraglobs)

    if not tests and sys.flags.optimize >= 2:
        suite = _DocTestSuite()
        suite.addTest(SkipDocTestCase(module))
        return suite

    tests.sort()
    suite = _DocTestSuite()
    for test in tests:
        if len(test.examples) == 0:
            continue
        suite.addTest(DocTestCase(test, **options))
    return suite


class _DocTestSuite(unittest.TestSuite):
    pass


class DocFileCase(DocTestCase):
    def id(self):
        return '_'.join(self._dt_test.name.split('.'))

    def __repr__(self):
        return self._dt_test.filename

    __str__ = __repr__

    def format_failure(self, err):
        return ('Failed doctest test for %s\n  File "%s", line 0\n\n%s'
                % (self._dt_test.name, self._dt_test.filename, err))


def DocFileTest(path, module_relative=True, package=None, globs=None,
                parser=None, encoding=None, **options):
    if globs is None:
        globs = {}
    else:
        globs = globs.copy()
    if parser is None:
        parser = DocTestParser()

    if package and not module_relative:
        raise ValueError("Package may only be specified for module-relative "
                         "paths.")

    doc, path = _load_testfile(path, package, module_relative, encoding)

    if "__file__" not in globs:
        globs["__file__"] = path

    name = os.path.basename(path)
    test = parser.get_doctest(doc, globs, name, path, 0)
    return DocFileCase(test, **options)


def DocFileSuite(*paths, **kw):
    suite = _DocTestSuite()
    if kw.get('module_relative', True):
        kw['package'] = _normalize_module(kw.get('package'), 3)
    for path in paths:
        suite.addTest(DocFileTest(path, **kw))
    return suite
