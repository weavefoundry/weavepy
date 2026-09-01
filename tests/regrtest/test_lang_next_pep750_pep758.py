"""`-X lang=next` 3.14 language preview (RFC 0076 WS15).

Covers PEP 750 t-strings (literal syntax, Template/Interpolation
runtime semantics, `string.templatelib`) and PEP 758 unparenthesized
`except`/`except*` lists. The literal syntax is only legal under the
`-X lang=next` gate, so syntax-level cases run in a subprocess with the
flag; the `string.templatelib` module itself is importable without the
gate (as on CPython 3.14) and is exercised in-process.
"""

import subprocess
import sys
import textwrap
import unittest

from string.templatelib import Interpolation, Template, convert


def run_gated(code, *, gate=True):
    """Run *code* in a fresh interpreter, optionally with -X lang=next."""
    argv = [sys.executable]
    if gate:
        argv += ["-X", "lang=next"]
    argv += ["-c", textwrap.dedent(code)]
    return subprocess.run(argv, capture_output=True, text=True)


class TemplatelibModuleTests(unittest.TestCase):
    """string.templatelib works with or without the syntax gate."""

    def test_manual_template_construction(self):
        t = Template("Ah! ", Interpolation("Camembert", "cheese"), ".")
        self.assertEqual(t.strings, ("Ah! ", "."))
        self.assertEqual(t.values, ("Camembert",))
        self.assertEqual(len(t.interpolations), 1)

    def test_consecutive_strings_concatenate(self):
        t = Template("Ah! We do have ", "Camembert", ".")
        self.assertEqual(t.strings, ("Ah! We do have Camembert.",))
        self.assertEqual(t.interpolations, ())

    def test_consecutive_interpolations_insert_empties(self):
        t = Template(
            Interpolation("Camembert", "cheese"),
            Interpolation(".", "punctuation"),
        )
        self.assertEqual(t.strings, ("", "", ""))

    def test_empty_template(self):
        self.assertEqual(Template().strings, ("",))
        self.assertEqual(Template().values, ())

    def test_iteration_skips_empty_strings(self):
        i1 = Interpolation("a", "x")
        i2 = Interpolation("b", "y")
        t = Template(i1, i2, "tail")
        self.assertEqual(list(t), [i1, i2, "tail"])

    def test_add_concatenates_templates(self):
        t = Template("x") + Template("y", Interpolation(1, "a"))
        self.assertEqual(t.strings, ("xy", ""))
        self.assertEqual(t.values, (1,))

    def test_add_str_unsupported(self):
        with self.assertRaises(TypeError):
            Template("x") + "y"
        with self.assertRaises(TypeError):
            "y" + Template("x")

    def test_bad_arg_type(self):
        with self.assertRaises(TypeError):
            Template(42)

    def test_interpolation_defaults_and_validation(self):
        i = Interpolation(3)
        self.assertEqual(i.expression, "")
        self.assertIsNone(i.conversion)
        self.assertEqual(i.format_spec, "")
        with self.assertRaises(ValueError):
            Interpolation(3, "x", "z")
        with self.assertRaises(TypeError):
            Interpolation(3, 42)

    def test_immutability(self):
        i = Interpolation(3, "x")
        t = Template("a", i)
        with self.assertRaises(AttributeError):
            i.value = 4
        with self.assertRaises(AttributeError):
            t.strings = ()
        with self.assertRaises(AttributeError):
            del i.conversion

    def test_pattern_matching(self):
        match Interpolation(3.0, "1. + 2.", None, ".2f"):
            case Interpolation(value, expression, conversion, format_spec):
                self.assertEqual(
                    (value, expression, conversion, format_spec),
                    (3.0, "1. + 2.", None, ".2f"),
                )
            case _:
                self.fail("Interpolation did not match")

    def test_convert(self):
        self.assertEqual(convert(3, None), 3)
        self.assertEqual(convert("a\n", "s"), "a\n")
        self.assertEqual(convert("a\n", "r"), "'a\\n'")
        self.assertEqual(convert("\xe9", "a"), "'\\xe9'")
        with self.assertRaises(ValueError):
            convert(3, "z")

    def test_repr(self):
        i = Interpolation("v", "e", "r", ".2f")
        self.assertEqual(repr(i), "Interpolation('v', 'e', 'r', '.2f')")


class TStringSyntaxTests(unittest.TestCase):
    """t-string literals under -X lang=next (subprocess)."""

    def check(self, body):
        proc = run_gated(body)
        self.assertEqual(
            proc.returncode, 0,
            f"stdout={proc.stdout!r} stderr={proc.stderr!r}")

    def test_literal_shape(self):
        self.check("""
            from string.templatelib import Template
            name = 'World'
            t = t'Hello {name}!'
            assert type(t) is Template
            assert t.strings == ('Hello ', '!')
            assert t.values == ('World',)
            i = t.interpolations[0]
            assert i.expression == 'name'
            assert i.conversion is None and i.format_spec == ''
        """)

    def test_empty_and_adjacent_fields(self):
        self.check("""
            a, b = 1, 2
            assert t''.strings == ('',)
            assert t'{a}{b}'.strings == ('', '', '')
            assert t'{a}{b}'.values == (1, 2)
        """)

    def test_conversion_and_eager_spec(self):
        self.check("""
            from string.templatelib import convert
            name, w = 'World', 8
            i = t'{name!r:>{w}}'.interpolations[0]
            assert i.conversion == 'r'
            assert i.format_spec == '>8', i.format_spec
            assert convert(i.value, i.conversion) == "'World'"
        """)

    def test_debug_form(self):
        self.check("""
            val = 42
            t = t'{val=}'
            assert t.strings[0] == 'val='
            assert t.interpolations[0].conversion == 'r'
            # An explicit spec suppresses the implicit !r.
            t2 = t'{val=:>5}'
            assert t2.interpolations[0].conversion is None
        """)

    def test_implicit_concat_and_add(self):
        self.check("""
            a, b = 1, 2
            t = t'a{a}' t'b{b}'
            assert t.strings == ('a', 'b', '')
            assert t.values == (1, 2)
            t2 = t'x' + t'y{a}'
            assert t2.strings == ('xy', '')
        """)

    def test_raw_tstring(self):
        self.check(r"""
            x = 1
            t = rt'\n{x}'
            assert t.strings == ('\\n', '')
        """)

    def test_eval_compile_under_gate(self):
        self.check("""
            assert eval("t'{1 + 1}'").values == (2,)
            assert eval("t'{1 + 1}'").interpolations[0].expression == '1 + 1'
        """)

    def test_mixing_with_str_is_syntax_error(self):
        self.check("""
            for src in ("t'a' 'b'", "'b' t'a'", "t'a' f'{1}'", "t'a' b'x'"):
                try:
                    compile(src, '<t>', 'eval')
                except SyntaxError:
                    pass
                else:
                    raise AssertionError(f'{src!r} should not compile')
        """)

    def test_bad_prefix_combinations(self):
        self.check("""
            for src in ("bt''", "tf''", "ut''"):
                try:
                    compile(src, '<t>', 'eval')
                except SyntaxError:
                    pass
                else:
                    raise AssertionError(f'{src!r} should not compile')
        """)

    def test_rejected_without_gate(self):
        proc = run_gated("t'{1}'", gate=False)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("SyntaxError", proc.stderr)


class Pep758Tests(unittest.TestCase):
    """Unparenthesized except lists under -X lang=next (subprocess)."""

    def check(self, body):
        proc = run_gated(body)
        self.assertEqual(
            proc.returncode, 0,
            f"stdout={proc.stdout!r} stderr={proc.stderr!r}")

    def test_except_comma_list(self):
        self.check("""
            def boom(exc):
                try:
                    raise exc
                except ValueError, TypeError:
                    return 'caught'
            assert boom(ValueError()) == 'caught'
            assert boom(TypeError('x')) == 'caught'
            try:
                boom(KeyError('k'))
            except KeyError:
                pass
            else:
                raise AssertionError('KeyError should escape')
        """)

    def test_except_star_comma_list(self):
        self.check("""
            caught = 'no'
            try:
                raise ExceptionGroup('g', [TypeError('t')])
            except* ValueError, TypeError:
                caught = 'yes'
            assert caught == 'yes'
        """)

    def test_trailing_comma(self):
        self.check("""
            try:
                raise ValueError()
            except ValueError, TypeError,:
                pass
        """)

    def test_as_requires_parens(self):
        self.check("""
            try:
                compile('try:\\n pass\\nexcept A, B as e:\\n pass\\n',
                        '<t>', 'exec')
            except SyntaxError as e:
                assert "when using 'as'" in str(e), e
            else:
                raise AssertionError('expected SyntaxError')
            # The parenthesized form still works with `as`.
            src = ('try:\\n raise ValueError()\\n'
                   'except (ValueError, TypeError) as e:\\n pass\\n')
            exec(compile(src, '<t>', 'exec'))
        """)

    def test_rejected_without_gate(self):
        proc = run_gated(
            "compile('try:\\n pass\\nexcept A, B:\\n pass\\n', '<t>', 'exec')",
            gate=False)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("must be parenthesized", proc.stderr)


class XOptionSurfaceTests(unittest.TestCase):
    def test_sys_xoptions_carries_gate(self):
        proc = run_gated("import sys; print(sys._xoptions.get('lang'))")
        self.assertEqual(proc.stdout.strip(), "next")


if __name__ == "__main__":
    unittest.main()
