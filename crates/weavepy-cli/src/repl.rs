//! Interactive REPL for `weavepy`.
//!
//! This is the user-visible "type `weavepy` at a shell, get a Python
//! prompt" experience. On a terminal it's CPython's `_pyrepl`, as in
//! `pymain_run_python`. This module owns what happens around it, and the
//! basic REPL for everything else: `PYTHON_BASIC_REPL`, a terminal
//! `_pyrepl` can't drive (`TERM=dumb`), and piped stdin under `-i`.
//!
//! The basic REPL reads plain lines. Each top-level input is parsed; if it
//! looks incomplete (unclosed bracket, dangling `:`-suite, unterminated
//! string) we re-prompt with `ps2` ("... ") until the user finishes the
//! statement. Successful evaluations of bare expressions print their
//! `repr()` and rebind `_` to the result, mirroring CPython.
//!
//! `PYTHONSTARTUP` runs once before the first prompt. The REPL also
//! injects a fresh `__main__` module whose globals persist across
//! prompts so user-typed bindings stick.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use weavepy_vm::shared_value::SharedStr;
use weavepy_vm::sync::Rc;
use weavepy_vm::sync::RefCell;

use anyhow::Result;
use weavepy::vm::{
    object::{DictData, DictKey, Object, PyModule},
    Interpreter,
};
use weavepy::{compiler, lexer, parser};

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Configurable REPL state. Build with [`Repl::new`], call
/// [`Repl::run`].
pub(crate) struct Repl {
    interpreter: Interpreter,
    main_module: Rc<PyModule>,
    /// Whether stdin is a terminal.
    stdin_tty: bool,
    /// `-E`: `PYTHON_BASIC_REPL` doesn't count.
    ignore_env: bool,
    quiet: bool,
    /// `CO_FUTURE_*` bits accumulated from executed inputs, so a
    /// `from __future__ import …` typed at one prompt affects every
    /// later prompt (CPython's `codeop.Compile` behaviour).
    future_flags: u32,
}

impl Repl {
    /// Build a REPL around an already-configured interpreter. The
    /// interpreter's `__main__` module is created and registered into
    /// `sys.modules` so subsequent inputs can reach each other via
    /// `globals()`.
    pub(crate) fn new(interpreter: Interpreter, quiet: bool, ignore_env: bool) -> Result<Self> {
        // `-i` / `PYTHONINSPECT`: a program already ran and left its
        // `__main__` behind — the REPL continues in that namespace
        // (CPython's `pymain_repl`), so `python -i -m timeit` sees
        // `Timer` at the prompt. Only a fresh session builds one.
        let main_module = match interpreter.module_cache().get("__main__") {
            Some(Object::Module(m)) => m,
            _ => {
                let m = build_main_module(&interpreter);
                interpreter
                    .module_cache()
                    .insert("__main__", Object::Module(m.clone()));
                m
            }
        };
        Ok(Self {
            interpreter,
            main_module,
            stdin_tty: std::io::IsTerminal::is_terminal(&io::stdin()),
            ignore_env,
            quiet,
            future_flags: 0,
        })
    }

    /// Execute an optional `PYTHONSTARTUP` file before entering the
    /// read-eval-print loop. Errors in the startup file are printed
    /// in CPython-style and the REPL continues regardless.
    pub(crate) fn run(mut self, startup: Option<&Path>) -> Result<()> {
        if !self.quiet {
            self.print_banner();
        }
        if let Some(p) = startup {
            self.run_startup(p);
        }
        self.enter(false)
    }

    /// `pymain_repl`: the prompt after a `-i` program, which is where
    /// `_pyrepl` itself runs `PYTHONSTARTUP`.
    pub(crate) fn run_inspect(mut self) -> Result<()> {
        self.enter(true)
    }

    /// `_pymain_run_repl`: `_pyrepl` on a terminal, unless `PYTHON_BASIC_REPL`
    /// asks for the basic REPL or `_pyrepl` can't run here.
    fn enter(&mut self, pythonstartup: bool) -> Result<()> {
        if self.stdin_tty {
            self.run_interactive_hook();
            if (self.ignore_env || !basic_repl_requested()) && self.run_pyrepl(pythonstartup) {
                self.finalize_interpreter();
                return Ok(());
            }
        }
        self.install_prompts();
        self.run_loop()
    }

    /// `pymain_run_interactive_hook`: `site`'s readline and history setup.
    fn run_interactive_hook(&mut self) {
        const SOURCE: &str = "\
import sys
hook = getattr(sys, '__interactivehook__', None)
if hook is not None:
    sys.audit('cpython.run_interactivehook', hook)
    try:
        hook()
    except SystemExit:
        raise
    except BaseException:
        print('Failed calling sys.__interactivehook__', file=sys.stderr)
        sys.excepthook(*sys.exc_info())
";
        self.exec_private(SOURCE, "<interactivehook>");
    }

    /// `pymain_run_pyrepl`: `false` when `_pyrepl` can't run here, having
    /// said why, for the basic REPL to take over. (`interactive_console`
    /// would fall back through `sys._baserepl`, which the basic REPL doesn't
    /// need, being this module.)
    fn run_pyrepl(&mut self, pythonstartup: bool) -> bool {
        let pyrepl = match self.interpreter.import_path("_pyrepl.main") {
            Ok(module) => module,
            Err(e) => {
                self.report_error(e);
                return false;
            }
        };
        let can_use = self
            .interpreter
            .load_attr_public(&pyrepl, "CAN_USE_PYREPL")
            .is_ok_and(|v| v.is_truthy());
        if !can_use {
            if let Ok(reason) = self.interpreter.load_attr_public(&pyrepl, "FAIL_REASON") {
                if let Object::Str(reason) = &reason {
                    let _ = writeln!(io::stderr(), "{}", reason.as_str());
                }
            }
            return false;
        }
        let startup = if pythonstartup { "True" } else { "False" };
        self.exec_private(
            &format!(
                "import __main__\n\
                 from _pyrepl.main import interactive_console\n\
                 interactive_console(mainmodule=__main__, pythonstartup={startup})\n"
            ),
            "<pyrepl>",
        );
        true
    }

    /// Run `source` in a namespace of its own, so that `__main__` stays as
    /// the prompt should find it. A `SystemExit` ends the session; anything
    /// else is reported.
    fn exec_private(&mut self, source: &str, filename: &str) {
        let module = match parser::parse_module_with_warnings_flags(source, false).0 {
            Ok(module) => module,
            Err(e) => {
                let e = weavepy::Error::Parse(e).format(source, filename);
                let _ = io::stderr().write_all(e.as_bytes());
                return;
            }
        };
        let code = match compiler::compile_owned_module_with_options(
            module,
            source,
            filename,
            compiler::CompileOptions::default(),
        ) {
            Ok(code) => code,
            Err(e) => {
                let e = weavepy::Error::Compile(e).format(source, filename);
                let _ = io::stderr().write_all(e.as_bytes());
                return;
            }
        };
        let globals = Rc::new(RefCell::new(DictData::default()));
        globals.borrow_mut().insert(
            DictKey(Object::from_static("__builtins__")),
            Object::Dict(self.interpreter.builtins_dict()),
        );
        if let Err(e) = self.interpreter.exec_module_in(&code, globals) {
            self.report_error(e);
        }
    }

    /// Print an exception that ended a step outside any prompt's input.
    fn report_error(&mut self, e: weavepy::vm::RuntimeError) {
        self.exit_if_system_exit(&e);
        if let weavepy::vm::RuntimeError::PyException(exc) = &e {
            if self.interpreter.print_uncaught_exception(exc) {
                return;
            }
        }
        let _ = writeln!(io::stderr(), "{e}");
    }

    /// CPython's `PyRun_InteractiveLoop` defines `sys.ps1` / `sys.ps2`
    /// (when the startup file hasn't) on entering the loop; their
    /// presence is how pdb / `code.interact` detect an interactive
    /// session. Scripts run outside the REPL never see them.
    fn install_prompts(&mut self) {
        let Ok(Object::Module(sys)) = self.interpreter.import_path("sys") else {
            return;
        };
        let mut d = sys.dict.borrow_mut();
        for (name, value) in [("ps1", ps1()), ("ps2", ps2())] {
            let key = DictKey(Object::from_static(name));
            if !d.contains_key(&key) {
                d.insert(key, Object::Str(SharedStr::from(value.as_str())));
            }
        }
    }

    fn print_banner(&self) {
        // CPython's interactive banner goes to *stderr* (as do the
        // prompts): `test_cmd_line_script.interactive_python` drains
        // stderr until the first `>>> ` while stdout must carry only
        // program output.
        let mut stderr = io::stderr().lock();
        // CPython's `Python {sys.version} on {sys.platform}` shape (the
        // version string already carries the "(WeavePy)" implementation
        // tag), so tooling that sniffs the banner sees the interpreter
        // version WeavePy identifies as (RFC 0077).
        let (maj, min, mic) = weavepy_vm::stdlib::sys::PY_VERSION;
        let _ = writeln!(
            stderr,
            "Python {maj}.{min}.{mic} (WeavePy {VERSION}) on {}",
            host_platform()
        );
        let _ = writeln!(
            stderr,
            "Type \"help\", \"copyright\", \"credits\" or \"license\" for more information."
        );
    }

    fn run_startup(&mut self, path: &Path) {
        match fs::read_to_string(path) {
            Ok(source) => {
                if let Err(e) = self.execute_once(&source, path.display().to_string()) {
                    let mut stderr = io::stderr().lock();
                    let _ = writeln!(stderr, "{e}");
                }
            }
            Err(e) => {
                let mut stderr = io::stderr().lock();
                let _ = writeln!(stderr, "PYTHONSTARTUP: {e}");
            }
        }
    }

    fn run_loop(&mut self) -> Result<()> {
        let mut buffer = String::new();
        loop {
            let prompt = if buffer.is_empty() { ps1() } else { ps2() };
            let line = match self.read_input(&prompt) {
                Ok(l) => l,
                Err(ReadError::Interrupted) => {
                    let mut stderr = io::stderr().lock();
                    let _ = writeln!(stderr, "KeyboardInterrupt");
                    buffer.clear();
                    continue;
                }
                Err(ReadError::Eof) => {
                    {
                        // Terminates the prompt, which lives on stderr.
                        let mut stderr = io::stderr().lock();
                        let _ = writeln!(stderr);
                    }
                    // A block still being continued when EOF arrives is
                    // compiled and run (CPython's tokenizer treats EOF
                    // as the terminating blank line: `def f(x, x): ...`
                    // + EOF still reports its SyntaxError —
                    // `test_repl.test_runsource_show_syntax_error_location`).
                    if !buffer.trim().is_empty() {
                        let to_run = std::mem::take(&mut buffer);
                        if let Err(e) = self.eval_input(&to_run) {
                            let mut stderr = io::stderr().lock();
                            let _ = stderr.write_all(e.as_bytes());
                        }
                        continue;
                    }
                    self.finalize_interpreter();
                    return Ok(());
                }
                Err(ReadError::Io(e)) => {
                    let mut stderr = io::stderr().lock();
                    let _ = writeln!(stderr, "weavepy: input error: {e}");
                    return Ok(());
                }
            };
            buffer.push_str(&line);
            buffer.push('\n');
            if needs_continuation(&buffer) {
                continue;
            }
            let trimmed = buffer.trim_end_matches(['\n', ' ', '\t']);
            if trimmed.is_empty() {
                buffer.clear();
                continue;
            }
            let to_run = buffer.clone();
            buffer.clear();
            if let Err(e) = self.eval_input(&to_run) {
                let mut stderr = io::stderr().lock();
                let _ = stderr.write_all(e.as_bytes());
            }
        }
    }

    /// One line of input, writing the prompt the way CPython's
    /// `PyOS_StdioReadline` does. A terminal's lines come straight from its
    /// line discipline, so input typed ahead stays there for the next prompt.
    fn read_input(&mut self, prompt: &str) -> Result<String, ReadError> {
        // Piped-stdin prompts go to *stderr*, like CPython's
        // `PyOS_StdioReadline` (stdout must carry only program output —
        // `test_cmd_line_script.test_repl_stdout_flush_separate_stderr`).
        {
            let mut out = io::stderr().lock();
            let _ = write!(out, "{prompt}");
            let _ = out.flush();
        }
        let mut line = String::new();
        let read = if self.stdin_tty {
            self.read_tty_line(&mut line)
        } else {
            io::stdin().read_line(&mut line)
        };
        match read {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Err(ReadError::Interrupted),
            Ok(0) => Err(ReadError::Eof),
            Ok(_) => {
                if let Some(stripped) = line.strip_suffix('\n') {
                    Ok(stripped.to_owned())
                } else {
                    // EOF terminated the line. CPython's tokenizer asks
                    // for one more line — printing the continuation
                    // prompt — and the EOF answer ends the prompt line,
                    // so any execution output starts on a fresh line
                    // (`test_repl.test_pythonstartup_error_reporting`
                    // writes `1/0` with no trailing newline).
                    let mut out = io::stderr().lock();
                    let _ = writeln!(out, "{}", ps2());
                    let _ = out.flush();
                    Ok(line)
                }
            }
            Err(e) => Err(ReadError::Io(e)),
        }
    }

    /// A line from a terminal, unbuffered and running signal handlers when a
    /// signal interrupts the read: `ErrorKind::Interrupted` when one raises
    /// (`my_fgets`'s `KeyboardInterrupt` at the prompt).
    #[cfg(unix)]
    fn read_tty_line(&mut self, line: &mut String) -> io::Result<usize> {
        let mut bytes = Vec::new();
        let mut byte = 0_u8;
        loop {
            // SAFETY: reads at most one byte into `byte`, a live local.
            let n = unsafe { libc::read(0, (&raw mut byte).cast(), 1) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::Interrupted {
                    return Err(e);
                }
                if let Err(raised) = self.interpreter.run_pending_signals_public() {
                    self.exit_if_system_exit(&raised);
                    return Err(e);
                }
                continue;
            }
            if n == 0 {
                break;
            }
            bytes.push(byte);
            if byte == b'\n' {
                break;
            }
        }
        line.push_str(&String::from_utf8_lossy(&bytes));
        Ok(bytes.len())
    }

    #[cfg(not(unix))]
    fn read_tty_line(&mut self, line: &mut String) -> io::Result<usize> {
        io::stdin().read_line(line)
    }

    /// Run one accepted prompt input the way CPython's REPL does:
    /// compiled in interactive ("single") mode, so a top-level
    /// expression statement echoes its value through `sys.displayhook`
    /// (which also binds `builtins._`) — see `OpCode::PrintExpr`.
    fn eval_input(&mut self, source: &str) -> Result<(), String> {
        self.execute_source(source, "<stdin>".to_owned(), true)
    }

    fn execute_once(&mut self, source: &str, filename: String) -> Result<(), String> {
        self.execute_source(source, filename, false)
    }

    fn execute_source(
        &mut self,
        source: &str,
        filename: String,
        interactive: bool,
    ) -> Result<(), String> {
        let module = parser::parse_module_with_warnings_flags(source, self.flufl_active())
            .0
            .map_err(|e| weavepy::Error::Parse(e).format(source, &filename))?;
        let compile = if interactive {
            compiler::compile_owned_interactive_with_options
        } else {
            compiler::compile_owned_module_with_options
        };
        let code = compile(module, source, &filename, self.compile_options())
            .map_err(|e| weavepy::Error::Compile(e).format(source, &filename))?;
        self.future_flags |= code.future_flags;
        if interactive {
            // gh-103987: every interactive input is registered with
            // `linecache._register_code` (keyed per contained code
            // object), so tracebacks — possibly from a *later* prompt —
            // render this input's source lines and `~~^~~` anchors.
            let code_rc = Rc::new(code.clone());
            self.interpreter
                .register_source_with_linecache(&code_rc, source, &filename);
        }
        let globals = self.main_module.dict.clone();
        match self.interpreter.exec_module_in(&code, globals) {
            Ok(_) => Ok(()),
            Err(e) => {
                self.exit_if_system_exit(&e);
                // CPython's REPL reports through `sys.excepthook` /
                // the `traceback` module (source lines, anchors,
                // chained exceptions); the plain formatter is only the
                // fallback when that machinery is unavailable.
                if let weavepy::vm::RuntimeError::PyException(exc) = &e {
                    if self.interpreter.print_uncaught_exception(exc) {
                        return Ok(());
                    }
                }
                Err(weavepy::Error::Runtime(e).format(source, &filename))
            }
        }
    }

    /// A `SystemExit` raised at the prompt (`exit()`, `sys.exit(2)`,
    /// `raise SystemExit`) terminates the session with CPython's
    /// exit-code semantics instead of printing a traceback.
    fn exit_if_system_exit(&mut self, e: &weavepy::vm::RuntimeError) {
        if let weavepy::vm::RuntimeError::PyException(exc) = e {
            if let Some(code) = exc.system_exit_code() {
                self.finalize_interpreter();
                crate::exit_with_system_exit(code);
            }
        }
    }

    /// `Py_FinalizeEx` for the interactive session: join non-daemon
    /// threads and run `atexit` callbacks, sweep leftover
    /// sub-interpreters (GH-135729's "close them with
    /// Interpreter.close()" warning), run finalizers, flush. Both exits
    /// from the loop (EOF and `SystemExit`) come through here, as in
    /// CPython's `pymain_run_python` → `Py_RunMain`.
    fn finalize_interpreter(&mut self) {
        self.interpreter.run_interpreter_shutdown();
        self.interpreter.run_shutdown_finalizers();
        let _ = self.interpreter.flush_streams();
    }

    fn flufl_active(&self) -> bool {
        self.future_flags & compiler::flags::CO_FUTURE_BARRY_AS_BDFL != 0
    }

    fn compile_options(&self) -> compiler::CompileOptions {
        compiler::CompileOptions {
            flags: self.future_flags,
            ..Default::default()
        }
    }
}

fn build_main_module(interpreter: &Interpreter) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::default()));
    let module = Rc::new(PyModule {
        name: "__main__".to_owned(),
        filename: None,
        dict: dict.clone(),
    });
    let mut d = dict.borrow_mut();
    d.insert(
        DictKey(Object::from_static("__name__")),
        Object::from_static("__main__"),
    );
    d.insert(DictKey(Object::from_static("__doc__")), Object::None);
    d.insert(
        DictKey(Object::from_static("__package__")),
        Object::from_static(""),
    );
    d.insert(DictKey(Object::from_static("__file__")), Object::None);
    d.insert(
        DictKey(Object::from_static("__builtins__")),
        Object::Dict(interpreter.builtins_dict()),
    );
    drop(d);
    module
}

fn needs_continuation(source: &str) -> bool {
    // Lightweight "is the buffer still incomplete" test driven by the
    // parser. A `ParseError::Unexpected` whose span is right at the
    // end of input is treated as "you need more text"; everything
    // else (including a successful parse or a mid-buffer error) is
    // "done, hand it to the evaluator."
    // CPython's interactive tokenizer only closes a *compound*
    // statement on a blank line (`>>> def f(): ...` shows the `... `
    // prompt until one arrives), even when the suite is already
    // syntactically complete — and even when it is about to be a
    // SyntaxError (`test_repl.test_runsource_show_syntax_error_location`
    // types `def f(x, x): ...`). Lexical, like the tokenizer: it must
    // not depend on a successful parse.
    if starts_with_compound_keyword(source) && !ends_with_blank_line(source) {
        return true;
    }
    match parser::parse_module(source.trim_end_matches('\n')) {
        Ok(module) => {
            // Empty parse on a non-empty trimmed buffer means the user
            // typed something like `if x:` and we're waiting for a body.
            // Heuristic: trailing line ends with `:` and last
            // non-blank line is indented less than expected.
            let last = source.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("");
            if last.trim_end().ends_with(':') {
                return module.body.is_empty();
            }
            // `match` is a soft keyword, so the lexical check above
            // skips it; a parsed `match` *statement* is compound too.
            if let [stmt] = module.body.as_slice() {
                if matches!(stmt.kind, parser::ast::StmtKind::Match { .. })
                    && !ends_with_blank_line(source)
                {
                    return true;
                }
            }
            // Bracket-balance for triple-quote / parens.
            !is_balanced(source)
        }
        Err(parser::ParseError::Unexpected { span, .. }) => {
            span.end.0 as usize >= source.len().saturating_sub(1)
        }
        Err(parser::ParseError::Lex(lexer::LexError::UnterminatedString { .. })) => true,
        Err(parser::ParseError::Lex(lexer::LexError::UnterminatedTripleString { .. })) => true,
        // An unterminated (possibly triple-quoted) f-string literal is the
        // multi-line-continuation case too — the user is still typing it.
        // (`FstringExpectingBrace`/`...OrSpec` are real errors, not these.)
        Err(parser::ParseError::Lex(lexer::LexError::UnterminatedFstring { .. })) => true,
        Err(parser::ParseError::Lex(lexer::LexError::UnterminatedTripleFstring { .. })) => true,
        Err(parser::ParseError::Lex(lexer::LexError::UnexpectedEof { .. })) => true,
        // An open bracket at EOF is the canonical "keep typing" state
        // (`(1,` ⏎). The batch compiler reports it as a hard
        // SyntaxError; the REPL reads it as continuation.
        Err(parser::ParseError::Lex(lexer::LexError::BracketNeverClosed { .. })) => true,
        Err(_) => false,
    }
}

/// Whether the buffer's first statement opens with a (hard) compound
/// keyword or a decorator — CPython's interactive grammar
/// (`statement_newline: compound_stmt NEWLINE | simple_stmts | …`)
/// demands an extra NEWLINE, i.e. a blank line, after these.
fn starts_with_compound_keyword(source: &str) -> bool {
    let first = source
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim_start();
    if first.starts_with('@') {
        return true;
    }
    let word: String = first
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    matches!(
        word.as_str(),
        "if" | "while" | "for" | "try" | "with" | "def" | "class" | "async"
    )
}

/// Whether the buffer's final physical line (before the trailing
/// newline the loop appends) is blank.
fn ends_with_blank_line(source: &str) -> bool {
    let trimmed = source.strip_suffix('\n').unwrap_or(source);
    trimmed
        .rsplit('\n')
        .next()
        .is_some_and(|line| line.trim().is_empty())
}

/// Rough delimiter balance. Used by [`needs_continuation`] only.
fn is_balanced(source: &str) -> bool {
    let mut depth: i32 = 0;
    let mut in_str: Option<char> = None;
    let mut triple = false;
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if let Some(q) = in_str {
            if c == '\\' {
                i += 2;
                continue;
            }
            if triple {
                if bytes.len() >= i + 3
                    && bytes[i] as char == q
                    && bytes[i + 1] as char == q
                    && bytes[i + 2] as char == q
                {
                    in_str = None;
                    triple = false;
                    i += 3;
                    continue;
                }
            } else if c == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        if c == '#' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if (c == '"' || c == '\'')
            && bytes.len() >= i + 3
            && bytes[i + 1] as char == c
            && bytes[i + 2] as char == c
        {
            in_str = Some(c);
            triple = true;
            i += 3;
            continue;
        }
        if c == '"' || c == '\'' {
            in_str = Some(c);
            i += 1;
            continue;
        }
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    in_str.is_none() && depth <= 0
}

fn ps1() -> String {
    std::env::var("WEAVEPY_PS1").unwrap_or_else(|_| ">>> ".to_owned())
}

fn ps2() -> String {
    std::env::var("WEAVEPY_PS2").unwrap_or_else(|_| "... ".to_owned())
}

/// `PYTHON_BASIC_REPL` (`_Py_GetEnv`: set and not empty).
fn basic_repl_requested() -> bool {
    std::env::var_os("PYTHON_BASIC_REPL").is_some_and(|v| !v.is_empty())
}

fn host_platform() -> &'static str {
    if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "win32"
    } else {
        "unknown"
    }
}

/// Why [`Repl::read_input`] has no line.
enum ReadError {
    Interrupted,
    Eof,
    Io(io::Error),
}
