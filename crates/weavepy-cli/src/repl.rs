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
    quiet: bool,
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
        let main_module = build_main_module(&interpreter);
        interpreter
            .module_cache()
            .insert("__main__", Object::Module(main_module.clone()));
        Ok(Self {
            interpreter,
            editor,
            main_module,
            history_path,
            quiet,
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
        let mut stdout = io::stdout().lock();
        let _ = writeln!(
            stdout,
            "WeavePy {VERSION} (Python 3.13 compatible) on {}",
            host_platform()
        );
        let _ = writeln!(
            stdout,
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
            let line = match self.editor.readline(&prompt) {
                Ok(l) => l,
                Err(ReadlineError::Interrupted) => {
                    let mut stderr = io::stderr().lock();
                    let _ = writeln!(stderr, "KeyboardInterrupt");
                    buffer.clear();
                    continue;
                }
                Err(ReadlineError::Eof) => {
                    let mut stdout = io::stdout().lock();
                    let _ = writeln!(stdout);
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

    /// Try to evaluate `source` as a single expression first (so the
    /// result can be printed and bound to `_`). On `SyntaxError` fall
    /// back to executing it as a statement / suite.
    fn eval_input(&mut self, source: &str) -> Result<(), String> {
        if let Ok(expr_repr) = self.try_eval_as_expression(source) {
            if let Some(text) = expr_repr {
                let mut stdout = io::stdout().lock();
                let _ = writeln!(stdout, "{text}");
            }
            return Ok(());
        }
        self.execute_once(source, "<stdin>".to_owned())
    }

    fn try_eval_as_expression(&mut self, source: &str) -> Result<Option<String>, ()> {
        let trimmed = source.trim_end_matches('\n');
        // A "single expression" candidate is one parse-able as
        // `Module(body=[Expr(value=…)])`. Anything else (statements,
        // multiple expressions, blocks) bails to the suite path.
        let module = parser::parse_module(trimmed).map_err(|_| ())?;
        if module.body.len() != 1 {
            return Err(());
        }
        let is_expr = matches!(module.body[0].kind, parser::ast::StmtKind::Expr(_));
        if !is_expr {
            return Err(());
        }
        let code =
            compiler::compile_module_with_source(&module, trimmed, "<stdin>").map_err(|_| ())?;
        let globals = self.main_module.dict.clone();
        let result = self
            .interpreter
            .exec_module_in(&code, globals)
            .map_err(|_| ())?;
        if matches!(result, Object::None) {
            return Ok(None);
        }
        // Bind `_` to the result (CPython behaviour).
        self.main_module
            .dict
            .borrow_mut()
            .insert(DictKey(Object::from_static("_")), result.clone());
        // We re-use the high-level `repr` over the result.
        let text = match &result {
            Object::Str(s) => format!("{s:?}"),
            other => format!("{other:?}"),
        };
        Ok(Some(text))
    }

    fn execute_once(&mut self, source: &str, filename: String) -> Result<(), String> {
        let module = parser::parse_module(source)
            .map_err(|e| weavepy::Error::Parse(e).format(source, &filename))?;
        let code = compiler::compile_module_with_source(&module, source, &filename)
            .map_err(|e| weavepy::Error::Compile(e).format(source, &filename))?;
        let globals = self.main_module.dict.clone();
        self.interpreter
            .exec_module_in(&code, globals)
            .map(|_| ())
            .map_err(|e| weavepy::Error::Runtime(e).format(source, &filename))
    }
}

fn build_main_module(interpreter: &Interpreter) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
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
            // Bracket-balance for triple-quote / parens.
            !is_balanced(source)
        }
        Err(parser::ParseError::Unexpected { span, .. }) => {
            span.end.0 as usize >= source.len().saturating_sub(1)
        }
        Err(parser::ParseError::Lex(lexer::LexError::UnterminatedString { .. })) => true,
        Err(parser::ParseError::Lex(lexer::LexError::UnexpectedEof { .. })) => true,
        Err(_) => false,
    }
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
