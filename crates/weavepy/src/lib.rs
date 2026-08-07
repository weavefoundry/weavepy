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
use std::sync::Once;

use thiserror::Error;

pub use weavepy_capi as capi;
pub use weavepy_compiler as compiler;
pub use weavepy_lexer as lexer;
pub use weavepy_parser as parser;
pub use weavepy_vm as vm;

/// Wire the C-extension loader (RFC 0022) into the VM. Called once
/// at process startup before any user code runs. Idempotent — safe
/// to call multiple times.
pub fn install_capi_loader() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        capi::force_link();
        vm::ext_loader::install_extension_loader(load_extension);
    });
}

fn load_extension(
    interp: &mut vm::Interpreter,
    full_name: &str,
) -> Result<Option<vm::object::Object>, vm::RuntimeError> {
    let path = match capi::loader::find_extension_on_path(interp, full_name) {
        Some(p) => p,
        None => return Ok(None),
    };
    let interp_ptr: *mut vm::Interpreter = interp;
    match capi::load_extension_module(interp_ptr, &path, full_name) {
        Ok(module) => Ok(Some(module)),
        Err(err) => Err(vm::RuntimeError::PyException(
            vm::PyException::from_builtin(
                "ImportError",
                format!("could not load extension '{full_name}': {err}"),
            ),
        )),
    }
}

/// Errors that can surface from the high-level [`run_source`] entry point.
#[derive(Debug, Error)]
pub enum Error {
    #[error("parse error: {0}")]
    Parse(#[from] parser::ParseError),
    #[error("compile error: {0}")]
    Compile(#[from] compiler::CompileError),
    #[error("runtime error: {0}")]
    Runtime(#[from] vm::RuntimeError),
    /// As [`Error::Runtime`], but the traceback was already written
    /// to stderr by the interpreter (CPython-style, via the
    /// `traceback` module). Callers must not print it again.
    #[error("runtime error: {0}")]
    RuntimePrinted(vm::RuntimeError),
}

impl Error {
    /// Render this error CPython-style, with file/line context.
    pub fn format(&self, source: &str, filename: &str) -> String {
        match self {
            Error::Parse(parser::ParseError::Lex(lex)) => format_lex_error(source, filename, lex),
            Error::Parse(parser::ParseError::Unexpected { span, message })
            | Error::Parse(parser::ParseError::Indentation { span, message }) => {
                format_syntax_error_span(source, filename, span.start.0, span.end.0, message)
            }
            Error::Parse(parser::ParseError::NotImplemented { span, feature, rfc }) => {
                let message = format!("`{feature}` is not implemented in the slice ({rfc})");
                format_syntax_error_span(source, filename, span.start.0, span.end.0, &message)
            }
            Error::Parse(err @ parser::ParseError::IdentifierConstant { .. }) => {
                // A plain ValueError in CPython — no caret/source context.
                format!("ValueError: {}\n", err.syntax_message())
            }
            Error::Compile(compile_err) => format_compile_error(source, filename, compile_err),
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
            // Already on stderr — render nothing.
            Error::RuntimePrinted(_) => String::new(),
        }
    }

    /// `true` when the traceback was already written to stderr by the
    /// interpreter and the caller must not print [`Error::format`]'s
    /// output again.
    pub fn already_printed(&self) -> bool {
        matches!(self, Error::RuntimePrinted(_))
    }

    /// When this error is a `SystemExit` (or subclass) propagating out
    /// of the program, return its exit `code` object. The CLI honours
    /// it like CPython: terminate with the code and print no traceback.
    /// Returns `None` for every other error.
    pub fn system_exit_code(&self) -> Option<vm::object::Object> {
        match self {
            Error::Runtime(vm::RuntimeError::PyException(exc))
            | Error::RuntimePrinted(vm::RuntimeError::PyException(exc)) => exc.system_exit_code(),
            _ => None,
        }
    }

    /// `true` when an unhandled `KeyboardInterrupt` reached the top
    /// level. The CLI then mimics CPython's `exit_sigint()` — reset
    /// `SIGINT` to `SIG_DFL` and `kill(getpid(), SIGINT)` — so the
    /// process terminates *by the signal*.
    pub fn is_keyboard_interrupt(&self) -> bool {
        match self {
            Error::Runtime(vm::RuntimeError::PyException(exc))
            | Error::RuntimePrinted(vm::RuntimeError::PyException(exc)) => {
                exc.is_keyboard_interrupt()
            }
            _ => false,
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
/// the displayed filename, every `-X foo=bar` / `-W ignore` / `-O`
/// flag CPython honours — so the CLI and tests don't have to grow a
/// function-argument soup as the surface area expands.
///
/// Construct with [`RunOptions::new`] and chain `with_*` setters.
/// Defaults match `python script.py` invoked with no flags:
/// `optimize = 0`, site enabled, env vars honoured, user site
/// enabled.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub filename: String,
    pub argv: Vec<String>,
    pub extra_path: Vec<PathBuf>,
    /// Directory to prepend to `sys.path` (typically the script's
    /// directory, mirroring CPython's `python script.py` behaviour).
    pub script_dir: Option<PathBuf>,
    /// Prepend [`Self::script_dir`] even under `-P`/`-I`. CPython's
    /// `pymain_run_python` adds a directory/zipfile argument
    /// (`main_importer_path`) to `sys.path[0]` unconditionally —
    /// `safe_path` only suppresses the *script's parent dir* — so
    /// `python -I app/` still finds `app/__main__.py`.
    pub script_dir_always: bool,
    /// Set of flag toggles the CLI hands the VM. The VM reflects
    /// these on `sys.flags`, `sys.dont_write_bytecode`,
    /// `sys._xoptions`, and `sys.warnoptions`.
    pub flags: InterpreterFlags,
    /// CLI behaviour: print an uncaught exception to stderr through
    /// the interpreter's own machinery (`sys.excepthook` → `traceback`
    /// module, with carets and chained exceptions) before returning
    /// it. The returned [`Error`] then reports
    /// [`Error::already_printed`] so the caller doesn't double-print.
    /// Library embedders keep the default (`false`) and render via
    /// [`Error::format`].
    pub print_uncaught: bool,
}

pub use vm::InterpreterFlags;

impl RunOptions {
    pub fn new(filename: impl Into<String>) -> Self {
        Self {
            filename: filename.into(),
            argv: Vec::new(),
            extra_path: Vec::new(),
            script_dir: None,
            script_dir_always: false,
            flags: InterpreterFlags::default(),
            print_uncaught: false,
        }
    }

    /// See [`RunOptions::print_uncaught`].
    #[must_use]
    pub fn with_print_uncaught(mut self, value: bool) -> Self {
        self.print_uncaught = value;
        self
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

    /// See [`RunOptions::script_dir_always`].
    #[must_use]
    pub fn with_script_dir_always(mut self, dir: impl Into<PathBuf>) -> Self {
        self.script_dir = Some(dir.into());
        self.script_dir_always = true;
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

    #[must_use]
    pub fn with_flags(mut self, flags: InterpreterFlags) -> Self {
        self.flags = flags;
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
    run_source_with_options_impl(source, opts, false).1
}

/// As [`run_source_with_options`], but keeps the interpreter (and its
/// `sys.modules['__main__']` globals) alive for the caller — the `-i`
/// / `PYTHONINSPECT` path, where the REPL that follows must share the
/// just-run program's namespace (`python -i -m timeit` then typing
/// `Timer` shows `__main__.Timer`). No shutdown finalizers run; the
/// caller owns the interpreter's remaining lifecycle.
pub fn run_source_keep_interpreter(
    source: &str,
    opts: &RunOptions,
) -> (vm::Interpreter, Result<(), Error>) {
    let (interp, result) = run_source_with_options_impl(source, opts, true);
    let interp = interp.unwrap_or_else(|| {
        // Parse/compile failed before an interpreter existed — hand
        // back a configured one so the REPL can still start (CPython's
        // `-i` drops to the prompt even after a startup SyntaxError).
        let mut i = vm::Interpreter::default();
        i.apply_run_options(&opts.flags);
        i
    });
    (interp, result)
}

fn run_source_with_options_impl(
    source: &str,
    opts: &RunOptions,
    keep_interpreter: bool,
) -> (Option<vm::Interpreter>, Result<(), Error>) {
    // `-x`: drop the first physical line (typical use is to allow a
    // shell-style shebang to live above a self-extracting payload).
    let effective_source: String;
    let source_ref: &str = if opts.flags.skip_first_line {
        effective_source = match source.find('\n') {
            Some(idx) => source[idx + 1..].to_owned(),
            None => String::new(),
        };
        &effective_source
    } else {
        source
    };
    install_capi_loader();
    // `\N{NAME}` escapes resolve through the VM's UCD tables even for this
    // pre-interpreter parse of the main module.
    vm::install_parser_unicode_hook();
    // Tokenizer-collected invalid-escape diagnostics (CPython's
    // `SyntaxWarning`s) are replayed through the `warnings` machinery
    // once the interpreter is up, just before the module body runs.
    let (module_res, escape_warnings) = parser::parse_module_with_warnings(source_ref);
    let module = match module_res {
        Ok(m) => m,
        Err(e) => return (None, Err(e.into())),
    };
    // `-O`/`-OO` applies to the main module too (assert/docstring
    // stripping, `__debug__` folding) — RFC 0052.
    let code = match compiler::compile_module_with_options(
        &module,
        source_ref,
        &opts.filename,
        compiler::CompileOptions {
            flags: 0,
            optimize: opts.flags.optimize,
        },
    ) {
        Ok(c) => c,
        Err(e) => return (None, Err(e.into())),
    };
    let mut interpreter = vm::Interpreter::default();
    interpreter.apply_run_options(&opts.flags);
    for p in &opts.extra_path {
        interpreter.append_path(p.clone());
    }
    let argv: Vec<String> = if opts.argv.is_empty() {
        vec![opts.filename.clone()]
    } else {
        opts.argv.clone()
    };
    interpreter.set_argv(argv);
    if !opts.flags.no_site {
        // Best-effort run of the `site` module — failures are
        // suppressed (matching CPython's behaviour) so a botched
        // `.pth` file can't break the interpreter outright.
        let _ = interpreter.run_site();
    }
    // `sys.path[0]` (the script's directory, `''` for `-c`/stdin, the
    // dir/zip argument itself) goes in *after* `site` runs — CPython's
    // `pymain_run_python` inserts path0 post-init, which is what keeps
    // site's `removeduppaths()` from absolutizing the `''` entry
    // (`test_cmd_line_script.test_issue8202_dash_c_file_ignored`).
    if !opts.flags.safe_path || opts.script_dir_always {
        if let Some(dir) = &opts.script_dir {
            interpreter.prepend_path(dir.clone());
        }
    }
    // Synthetic source names are wrapped in angle brackets (`<string>`,
    // `<stdin>`, `<multiprocessing-fork>`, …) and, like CPython, must *not*
    // install `__main__.__file__` — only a real on-disk script does. This is
    // load-bearing for `multiprocessing` spawn: a spawned child runs the
    // `<multiprocessing-fork>` bridge as `__main__`; if that carried a
    // `__file__`, `multiprocessing.spawn.get_preparation_data` would record
    // `init_main_from_path='<multiprocessing-fork>'` and every *nested* spawn
    // (resource_tracker, manager server, a Pool created inside a worker) would
    // `runpy.run_path('<multiprocessing-fork>')` → `FileNotFoundError`.
    let file_for_main = if opts.filename.starts_with('<') && opts.filename.ends_with('>') {
        None
    } else {
        Some(opts.filename.as_str())
    };
    // `python -c CMD` keeps the command text reachable for tracebacks
    // even though `<string>` has no file: CPython registers it with
    // `linecache._register_code` (gh-103987). Mirror that for our
    // CLI-driven `<string>` runs.
    if opts.print_uncaught && opts.filename == "<string>" {
        let code_rc = vm::sync::Rc::new(code.clone());
        interpreter.register_source_with_linecache(&code_rc, source_ref, "<string>");
    }
    let result = interpreter
        .emit_escape_warnings(source_ref, &opts.filename, &escape_warnings)
        .and_then(|()| interpreter.run_module_as(&code, "__main__", file_for_main));
    // CPython prints the uncaught exception (via `sys.excepthook` /
    // the traceback module) *before* `Py_FinalizeEx` runs shutdown
    // finalizers — `__del__` output interleaves after the traceback.
    let result = match result {
        Err(vm::RuntimeError::PyException(exc))
            if opts.print_uncaught && exc.system_exit_code().is_none() =>
        {
            if interpreter.print_uncaught_exception(&exc) {
                Err(Error::RuntimePrinted(vm::RuntimeError::PyException(exc)))
            } else {
                // Rendering machinery unavailable — let the caller's
                // plain formatter handle it.
                Err(Error::Runtime(vm::RuntimeError::PyException(exc)))
            }
        }
        other => other.map_err(Error::from),
    };
    if keep_interpreter {
        // `-i` / `PYTHONINSPECT`: the program's namespace lives on into
        // the REPL — no shutdown, no finalizers; just make the output
        // visible before the prompt appears.
        let _ = interpreter.flush_streams();
        return (Some(interpreter), result.map(|_| ()));
    }
    // CPython's `Py_FinalizeEx` first joins non-daemon threads
    // (`threading._shutdown()`) and runs `atexit` callbacks, *then*
    // finalizes everything still alive. Do the same so a program that
    // returns from `__main__` while a non-daemon worker is still running
    // waits for it (and its output) before exit.
    interpreter.run_interpreter_shutdown();
    // CPython runs finalizers for everything still alive during
    // interpreter shutdown — including a module-global object whose
    // `__del__` raises (which is reported via `sys.unraisablehook`).
    // Do this whether the module returned normally or via `SystemExit`,
    // before the caller turns a `SystemExit` into a process exit.
    interpreter.run_shutdown_finalizers();
    // Flush buffered stdout while the interpreter (and its sinks) are
    // still alive. The CLI turns a `SystemExit` into `std::process::exit`,
    // which skips Rust's end-of-main stdout flush, so without this an
    // output-buffering `sys.exit()` run (e.g. a redirected unittest
    // session) would lose its tail — or all of it, for sub-buffer output.
    if !interpreter.flush_streams() {
        // CPython: a failed `Py_FinalizeEx` (here: `sys.stdout` would
        // not flush) turns the whole run into exit status 120,
        // overriding even a `SystemExit` code.
        std::process::exit(120);
    }
    (Some(interpreter), result.map(|_| ()))
}

fn script_dir_of(filename: &str) -> PathBuf {
    Path::new(filename)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Render a CPython-style `SyntaxError`-shape diagnostic, with the
/// offending source line and a caret under the column.
/// Render the `File … / source line / caret run / SyntaxError: …` block
/// for a syntax error, replicating CPython's
/// `traceback.TracebackException.format_exception_only` clamp logic.
/// `offset`/`end_offset` are the 1-based columns stored on the
/// exception (character-based for parser errors, byte-based for
/// compile/symtable errors — CPython renders both as if they indexed
/// characters, so we do too).
fn render_caret_block(
    filename: &str,
    lineno: usize,
    line_text: &str,
    offset: usize,
    end_offset: usize,
    message: &str,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "  File \"{filename}\", line {lineno}");
    let rtext = line_text.trim_end_matches('\n');
    let ltext = rtext.trim_start_matches([' ', '\n', '\x0c']);
    let rtext_chars = rtext.chars().count();
    let ltext_chars = ltext.chars().count();
    let spaces = rtext_chars - ltext_chars;
    // `.text` is the newline-terminated line; CPython clamps offsets
    // that fall past it back to just-after-end-of-line.
    let text_chars = rtext_chars + 1;
    let mut offset = offset;
    let mut end_offset = end_offset;
    if offset > text_chars {
        offset = rtext_chars + 1;
    }
    if end_offset > text_chars {
        end_offset = rtext_chars + 1;
    }
    if offset >= end_offset {
        end_offset = offset + 1;
    }
    let colno = offset as i64 - 1 - spaces as i64;
    let end_colno = end_offset as i64 - 1 - spaces as i64;
    let _ = writeln!(out, "    {ltext}");
    if colno >= 0 {
        // Non-space whitespace (tabs) is kept so the carets align.
        let caretspace: String = ltext
            .chars()
            .take(colno as usize)
            .map(|c| if c.is_whitespace() { c } else { ' ' })
            .collect();
        let _ = writeln!(
            out,
            "    {caretspace}{}",
            "^".repeat((end_colno - colno) as usize)
        );
    }
    let _ = writeln!(out, "SyntaxError: {message}");
    out
}

/// Render a compile/symtable-stage error CPython-style. Byte columns
/// for compile-stage spans, character columns for parser-stage ones —
/// matching what the VM stores on the `SyntaxError` object.
fn format_compile_error(source: &str, filename: &str, err: &compiler::CompileError) -> String {
    let Some(span) = err.span else {
        return format!(
            "  File \"{filename}\", line ?\nSyntaxError: {}\n",
            err.message
        );
    };
    let start = SourceLocation::from_byte(source, span.start.0);
    let end = SourceLocation::from_byte(source, span.end.0);
    let line_start_byte = {
        let byte = (span.start.0 as usize).min(source.len());
        source[..byte].rfind('\n').map_or(0, |i| i + 1)
    };
    let byte_col = |byte: u32| (byte as usize).min(source.len()) - line_start_byte + 1;
    let (offset, end_offset) = if err.parser_stage {
        let end_col = if end.line == start.line {
            end.col
        } else {
            start.line_text.chars().count() + 1
        };
        (start.col, end_col)
    } else {
        let end_col = if end.line == start.line {
            byte_col(span.end.0)
        } else {
            start.line_text.len() + 1
        };
        (byte_col(span.start.0), end_col)
    };
    render_caret_block(
        filename,
        start.line,
        start.line_text,
        offset,
        end_offset,
        &err.message,
    )
}

fn format_lex_error(source: &str, filename: &str, err: &lexer::LexError) -> String {
    // A line continuation at EOF with nothing before it on the line:
    // CPython's *file* tokenizer reports column 0 for this, and the
    // C-level error printer then omits the caret line entirely
    // (test_eof's bpo-2180 from-file cases assert on that shape).
    if let lexer::LexError::UnexpectedEofParsing {
        pos,
        line_had_tokens: false,
    } = err
    {
        let loc = SourceLocation::from_byte(source, *pos);
        let rtext = loc.line_text.trim_end_matches('\n');
        let ltext = rtext.trim_start_matches([' ', '\n', '\x0c']);
        return format!(
            "  File \"{filename}\", line {}\n    {ltext}\nSyntaxError: {err}\n",
            loc.line
        );
    }
    let byte = err.byte_offset();
    format_syntax_error_span(source, filename, byte, byte, &err.to_string())
}

/// Render a parser-stage error with character-based columns derived
/// from the byte span.
fn format_syntax_error_span(
    source: &str,
    filename: &str,
    start_byte: u32,
    end_byte: u32,
    message: &str,
) -> String {
    let start = SourceLocation::from_byte(source, start_byte);
    let end = SourceLocation::from_byte(source, end_byte);
    let end_col = if end.line == start.line {
        end.col
    } else {
        start.line_text.chars().count() + 1
    };
    render_caret_block(
        filename,
        start.line,
        start.line_text,
        start.col,
        end_col,
        message,
    )
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
