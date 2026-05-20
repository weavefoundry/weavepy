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
    fn rejects_class_with_named_rfc() {
        let err = parse_module("class C: pass\n").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("RFC 0003"), "msg: {msg}");
    }

    #[test]
    fn rejects_try_with_named_rfc() {
        let err = parse_module("try:\n    pass\nexcept:\n    pass\n").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("RFC 0004"), "msg: {msg}");
    }
}
