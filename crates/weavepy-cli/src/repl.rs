//! Interactive REPL for `weavepy`.
//!
//! This is the user-visible "type `weavepy` at a shell, get a Python
//! prompt" experience. Built on `rustyline` for line editing, history,
//! and Ctrl-C / Ctrl-D handling. Each top-level input is parsed; if it
//! looks incomplete (unclosed bracket, dangling `:`-suite, unterminated
//! string) we re-prompt with `ps2` ("... ") until the user finishes the
//! statement. Successful evaluations of bare expressions print their
//! `repr()` and rebind `_` to the result, mirroring CPython.
//!
//! Persistent history lives at `$WEAVEPY_HISTORY`, falling back to
//! `$XDG_DATA_HOME/weavepy/history` (Linux), `~/Library/Application
//! Support/weavepy/history` (macOS), `%APPDATA%/weavepy/history`
//! (Windows), or `~/.weavepy_history` if none of those resolve. Read
//! on startup, appended on every accepted input.
//!
//! `PYTHONSTARTUP` runs once before the first prompt. The REPL also
//! injects a fresh `__main__` module whose globals persist across
//! prompts so user-typed bindings stick.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use weavepy_vm::sync::Rc;
use weavepy_vm::sync::RefCell;

use anyhow::Result;
use rustyline::error::ReadlineError;
use rustyline::history::FileHistory;
use rustyline::{Config, EditMode, Editor};
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
    editor: Editor<(), FileHistory>,
    main_module: Rc<PyModule>,
    history_path: Option<PathBuf>,
    /// Whether stdin is a terminal. When it isn't (piped input), lines
    /// are read directly rather than through rustyline, so EOF
    /// handling can match CPython's `PyOS_StdioReadline`.
    stdin_tty: bool,
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
    pub(crate) fn new(interpreter: Interpreter, quiet: bool) -> Result<Self> {
        let config = Config::builder()
            .history_ignore_dups(true)
            .map_err(io_err)?
            .edit_mode(EditMode::Emacs)
            .auto_add_history(true)
            .build();
        let mut editor: Editor<(), FileHistory> = Editor::with_config(config).map_err(io_err)?;
        let history_path = history_file_path();
        if let Some(p) = history_path.as_ref() {
            let _ = editor.load_history(p);
        }
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
            editor,
            main_module,
            history_path,
            stdin_tty: std::io::IsTerminal::is_terminal(&io::stdin()),
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
        self.run_loop()
    }

    fn print_banner(&self) {
        // CPython's interactive banner goes to *stderr* (as do the
        // prompts): `test_cmd_line_script.interactive_python` drains
        // stderr until the first `>>> ` while stdout must carry only
        // program output.
        let mut stderr = io::stderr().lock();
        let _ = writeln!(
            stderr,
            "WeavePy {VERSION} (Python 3.13 compatible) on {}",
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
                Err(ReadlineError::Interrupted) => {
                    let mut stderr = io::stderr().lock();
                    let _ = writeln!(stderr, "KeyboardInterrupt");
                    buffer.clear();
                    continue;
                }
                Err(ReadlineError::Eof) => {
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
                    if let Some(p) = self.history_path.as_ref() {
                        let _ = self.editor.save_history(p);
                    }
                    return Ok(());
                }
                Err(e) => {
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
            if let Some(p) = self.history_path.as_ref() {
                let _ = self.editor.save_history(p);
            }
        }
    }

    /// One line of input. A terminal goes through rustyline (editing,
    /// history, Ctrl-C/Ctrl-D); piped stdin reads plainly, writing the
    /// prompt to stdout the way CPython's `PyOS_StdioReadline` does.
    fn read_input(&mut self, prompt: &str) -> rustyline::Result<String> {
        if self.stdin_tty {
            return self.editor.readline(prompt);
        }
        // Piped-stdin prompts go to *stderr*, like CPython's
        // `PyOS_StdioReadline` (stdout must carry only program output —
        // `test_cmd_line_script.test_repl_stdout_flush_separate_stderr`).
        {
            let mut out = io::stderr().lock();
            let _ = write!(out, "{prompt}");
            let _ = out.flush();
        }
        let mut line = String::new();
        match io::stdin().read_line(&mut line) {
            Ok(0) => Err(ReadlineError::Eof),
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
            Err(e) => Err(ReadlineError::Io(e)),
        }
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
            compiler::compile_interactive_with_options
        } else {
            compiler::compile_module_with_options
        };
        let code = compile(&module, source, &filename, self.compile_options())
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
                if let Some(p) = self.history_path.clone() {
                    let _ = self.editor.save_history(&p);
                }
                let _ = self.interpreter.flush_streams();
                crate::exit_with_system_exit(code);
            }
        }
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

fn history_file_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("WEAVEPY_HISTORY") {
        return Some(PathBuf::from(p));
    }
    if let Some(dir) = dirs::data_dir() {
        let p = dir.join("weavepy").join("history");
        let _ = fs::create_dir_all(p.parent().unwrap_or(&p));
        return Some(p);
    }
    if let Some(home) = dirs::home_dir() {
        return Some(home.join(".weavepy_history"));
    }
    None
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

fn io_err<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::other(e.to_string())
}
