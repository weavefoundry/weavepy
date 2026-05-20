//! Python parser for WeavePy.
//!
//! Consumes tokens produced by [`weavepy_lexer`] and produces an AST
//! that aims to be a faithful representation of CPython's `ast`
//! module. The AST is exposed through the [`ast`] submodule, and a
//! convenience `parse_module` entry point handles tokenization
//! internally for callers that just want "source → AST."
//!
//! # Compatibility level
//!
//! - **Tracks CPython** for grammar productions, operator precedence,
//!   and AST node shape.
//! - **Experimental** for the size of the grammar: see
//!   `docs/rfcs/0001-executable-slice.md` for what's in and out.

pub mod ast;
pub mod error;
mod parser;

pub use ast::{dump_module, Module};
pub use error::ParseError;

/// Parse a Python source buffer into a [`Module`].
pub fn parse_module(source: &str) -> Result<Module, ParseError> {
    let tokens = weavepy_lexer::tokenize(source)?;
    parser::parse(source, tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_empty_module() {
        let m = parse_module("").expect("empty parses");
        assert!(m.body.is_empty());
    }

    #[test]
    fn parses_simple_expression_statement() {
        let m = parse_module("1 + 2\n").expect("ok");
        assert_eq!(m.body.len(), 1);
    }

    #[test]
    fn parses_function_def_and_call() {
        let src = "def add(a, b):\n    return a + b\nadd(1, 2)\n";
        let m = parse_module(src).expect("ok");
        assert_eq!(m.body.len(), 2);
    }

    #[test]
    fn parses_if_elif_else() {
        let src = "if x:\n    y\nelif z:\n    w\nelse:\n    v\n";
        let _ = parse_module(src).expect("ok");
    }

    #[test]
    fn parses_for_with_range() {
        let src = "for i in range(10):\n    print(i)\n";
        let _ = parse_module(src).expect("ok");
    }

    #[test]
    fn parses_list_comp() {
        let _ = parse_module("[x * x for x in range(10) if x % 2 == 0]\n").expect("ok");
    }

    #[test]
    fn parses_chained_comparison() {
        let _ = parse_module("1 < x < 10\n").expect("ok");
    }

    #[test]
    fn parses_class_with_body() {
        let module = parse_module("class C:\n    pass\n").expect("parse class");
        assert_eq!(module.body.len(), 1);
    }

    #[test]
    fn parses_try_except() {
        let module =
            parse_module("try:\n    pass\nexcept ValueError:\n    pass\n").expect("parse try");
        assert_eq!(module.body.len(), 1);
    }

    #[test]
    fn parses_simple_fstring() {
        let m = parse_module("x = f'hello {name}'\n").expect("parse fstring");
        assert_eq!(m.body.len(), 1);
    }

    #[test]
    fn parses_fstring_with_format_spec() {
        let _ = parse_module("y = f'{val:.2f}'\n").expect("format spec");
    }

    #[test]
    fn parses_fstring_with_conversion_and_spec() {
        let _ = parse_module("z = f'{obj!r:>10}'\n").expect("conv + spec");
    }

    #[test]
    fn parses_fstring_debug_form() {
        let _ = parse_module("print(f'{x = }')\n").expect("debug f-string");
    }

    #[test]
    fn parses_yield_expression() {
        let _ = parse_module("def g():\n    yield 1\n    yield\n").expect("yield");
    }

    #[test]
    fn parses_yield_from() {
        let _ = parse_module("def g():\n    yield from range(10)\n").expect("yield from");
    }

    #[test]
    fn parses_match_with_literal_and_capture() {
        let src = "match x:\n    case 0:\n        pass\n    case y:\n        pass\n";
        let _ = parse_module(src).expect("match basic");
    }

    #[test]
    fn parses_match_with_class_pattern() {
        let src = "match p:\n    case Point(x=0, y=0):\n        pass\n    case _:\n        pass\n";
        let _ = parse_module(src).expect("match class");
    }

    #[test]
    fn parses_match_with_sequence_and_star() {
        let src = "match xs:\n    case [a, b, *rest]:\n        pass\n";
        let _ = parse_module(src).expect("match seq");
    }

    #[test]
    fn parses_match_with_or_and_guard() {
        let src = "match v:\n    case 1 | 2 | 3 if v > 0:\n        pass\n";
        let _ = parse_module(src).expect("match or+guard");
    }

    #[test]
    fn match_is_soft_keyword_when_not_at_statement_start() {
        // `re.match` should still parse as an identifier.
        let _ = parse_module("x = re.match(p, s)\n").expect("match as ident");
    }
}
