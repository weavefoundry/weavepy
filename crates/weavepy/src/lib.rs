//! WeavePy: a high-performance, CPython-compatible Python interpreter written in Rust.
//!
//! This crate is the public Rust entry point for embedding WeavePy. It
//! re-exports the stable surface of the underlying pipeline crates and
//! provides convenience wrappers for the common case of "run this Python
//! source string."
//!
//! # Example
//!
//! ```no_run
//! use weavepy::run_source;
//!
//! run_source("print('hello, weavepy')").unwrap();
//! ```

use std::fmt::Write;
use std::path::{Path, PathBuf};

use thiserror::Error;

pub use weavepy_compiler as compiler;
pub use weavepy_lexer as lexer;
pub use weavepy_parser as parser;
pub use weavepy_vm as vm;

/// Errors that can surface from the high-level [`run_source`] entry point.
#[derive(Debug, Error)]
pub enum Error {
    #[error("parse error: {0}")]
    Parse(#[from] parser::ParseError),
    #[error("compile error: {0}")]
    Compile(#[from] compiler::CompileError),
    #[error("runtime error: {0}")]
    Runtime(#[from] vm::RuntimeError),
}

impl Error {
    /// Render this error CPython-style, with file/line context.
    pub fn format(&self, source: &str, filename: &str) -> String {
        match self {
            Error::Parse(parser::ParseError::Lex(lex)) => format_lex_error(source, filename, lex),
            Error::Parse(parser::ParseError::Unexpected { span, message }) => {
                format_syntax_error(source, filename, span.start.0, message)
            }
            Error::Parse(parser::ParseError::NotImplemented { span, feature, rfc }) => {
                let message = format!("`{feature}` is not implemented in the slice ({rfc})");
                format_syntax_error(source, filename, span.start.0, &message)
            }
            Error::Compile(compile_err) => {
                // Compiler errors don't carry spans yet — surface as a
                // SyntaxError-shaped diagnostic without line info.
                format!("  File \"{filename}\", line ?\nSyntaxError: {compile_err}\n")
            }
            Error::Runtime(vm::RuntimeError::PyException(exc)) => {
                let mut s = String::new();
                let _ = writeln!(s, "Traceback (most recent call last):");
                // Tracebacks are accumulated from inner-most frame
                // outward; print outer-most first to match CPython.
                if exc.traceback.is_empty() {
                    let _ = writeln!(s, "  File \"{filename}\", line ?, in <module>");
                } else {
                    for entry in exc.traceback.iter().rev() {
                        let _ = writeln!(
                            s,
                            "  File \"{}\", line {}, in {}",
                            entry.filename, entry.lineno, entry.funcname
                        );
                    }
                }
                let _ = writeln!(s, "{}: {}", exc.type_name(), exc.message());
                s
            }
            Error::Runtime(vm::RuntimeError::Internal(msg)) => {
                format!(
                    "Traceback (most recent call last):\n  File \"{filename}\", line ?, in <module>\nInternalError: {msg}\n"
                )
            }
        }
    }
}

/// Convenience: parse, compile, and execute a Python source string under a
/// fresh interpreter, discarding the resulting module-level value.
///
/// Errors lose their file context here — use [`run_source_with_filename`]
/// (or [`Error::format`]) when you have one.
pub fn run_source(source: &str) -> Result<(), Error> {
    run_source_with_filename(source, "<string>")
}

/// As [`run_source`], but tags compile-time bookkeeping with `filename`
/// so traceback formatting can show it.
pub fn run_source_with_filename(source: &str, filename: &str) -> Result<(), Error> {
    let mut opts = RunOptions::new(filename);
    if filename != "<string>" && filename != "<stdin>" {
        opts = opts.with_script_dir(script_dir_of(filename));
    }
    run_source_with_options(source, &opts)
}

/// Knobs for running a Python source string.
///
/// Wraps the cross-cutting state — `sys.argv`, `sys.path` additions,
/// the displayed filename — so the CLI and tests don't have to grow
/// a function-argument soup as the feature surface expands.
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub filename: String,
    pub argv: Vec<String>,
    pub extra_path: Vec<PathBuf>,
    /// Directory to prepend to `sys.path` (typically the script's
    /// directory, mirroring CPython's `python script.py` behaviour).
    pub script_dir: Option<PathBuf>,
}

impl RunOptions {
    pub fn new(filename: impl Into<String>) -> Self {
        Self {
            filename: filename.into(),
            argv: Vec::new(),
            extra_path: Vec::new(),
            script_dir: None,
        }
    }

    #[must_use]
    pub fn with_argv<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.argv = args.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub fn with_script_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.script_dir = Some(dir.into());
        self
    }

    #[must_use]
    pub fn with_extra_path<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.extra_path = paths.into_iter().map(Into::into).collect();
        self
    }
}

/// Run `source` under a fresh interpreter, threading `argv`/`path`/
/// `filename` through `sys` and the import loader before execution.
///
/// This is the canonical embedding entry point: the CLI uses it to
/// emulate `python script.py arg1 arg2`, and tests use it for
/// multi-file fixtures.
pub fn run_source_with_options(source: &str, opts: &RunOptions) -> Result<(), Error> {
    let module = parser::parse_module(source)?;
    let code = compiler::compile_module_with_source(&module, source, &opts.filename)?;
    let mut interpreter = vm::Interpreter::default();
    if let Some(dir) = &opts.script_dir {
        interpreter.prepend_path(dir.clone());
    }
    for p in &opts.extra_path {
        interpreter.append_path(p.clone());
    }
    let argv: Vec<String> = if opts.argv.is_empty() {
        vec![opts.filename.clone()]
    } else {
        opts.argv.clone()
    };
    interpreter.set_argv(argv);
    let file_for_main = if opts.filename == "<string>" || opts.filename == "<stdin>" {
        None
    } else {
        Some(opts.filename.as_str())
    };
    let _ = interpreter.run_module_as(&code, "__main__", file_for_main)?;
    Ok(())
}

fn script_dir_of(filename: &str) -> PathBuf {
    Path::new(filename)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Render a CPython-style `SyntaxError`-shape diagnostic, with the
/// offending source line and a caret under the column.
fn format_syntax_error(source: &str, filename: &str, byte: u32, message: &str) -> String {
    let loc = SourceLocation::from_byte(source, byte);
    let mut out = String::new();
    let _ = writeln!(out, "  File \"{filename}\", line {}", loc.line);
    let _ = writeln!(out, "    {}", loc.line_text);
    let pad: String = " ".repeat(4 + loc.col.saturating_sub(1));
    let _ = writeln!(out, "{pad}^");
    let _ = writeln!(out, "SyntaxError: {message}");
    out
}

fn format_lex_error(source: &str, filename: &str, err: &lexer::LexError) -> String {
    let byte = match err {
        lexer::LexError::UnterminatedString { pos }
        | lexer::LexError::InvalidChar { pos, .. }
        | lexer::LexError::InconsistentIndent { pos }
        | lexer::LexError::UnknownDedent { pos }
        | lexer::LexError::InvalidNumber { pos, .. }
        | lexer::LexError::InvalidStringPrefix { pos, .. }
        | lexer::LexError::StrayBackslash { pos }
        | lexer::LexError::UnexpectedEof { pos, .. } => *pos,
    };
    format_syntax_error(source, filename, byte, &err.to_string())
}

/// `(line, column, line_text)` derived from a byte offset.
struct SourceLocation<'a> {
    line: usize,
    col: usize,
    line_text: &'a str,
}

impl<'a> SourceLocation<'a> {
    fn from_byte(source: &'a str, byte: u32) -> Self {
        let byte = (byte as usize).min(source.len());
        let mut line_start = 0usize;
        let mut line = 1usize;
        for (i, ch) in source.char_indices() {
            if i >= byte {
                break;
            }
            if ch == '\n' {
                line += 1;
                line_start = i + 1;
            }
        }
        let line_end = source[line_start..]
            .find('\n')
            .map_or(source.len(), |off| line_start + off);
        let col = source[line_start..byte].chars().count() + 1;
        Self {
            line,
            col,
            line_text: &source[line_start..line_end],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_empty_source_succeeds() {
        run_source("").expect("empty source should run");
    }

    #[test]
    fn syntax_error_carries_caret() {
        let err = run_source("def 3():\n    pass").unwrap_err();
        let msg = err.format("def 3():\n    pass", "/tmp/x.py");
        assert!(msg.contains("/tmp/x.py"), "{msg}");
        assert!(msg.contains("line 1"), "{msg}");
        assert!(msg.contains('^'), "{msg}");
        assert!(msg.contains("SyntaxError"), "{msg}");
    }

    #[test]
    fn runtime_error_includes_filename() {
        let err = run_source_with_filename("undefined_name", "/tmp/y.py").unwrap_err();
        let msg = err.format("undefined_name", "/tmp/y.py");
        assert!(msg.starts_with("Traceback"), "{msg}");
        assert!(msg.contains("/tmp/y.py"), "{msg}");
    }
}
