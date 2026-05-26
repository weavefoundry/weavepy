//! WeavePy virtual machine.
//!
//! Drives a stack of [`Frame`]s through a [`weavepy_compiler::CodeObject`]
//! and produces the same observable effects as CPython for the
//! subset of Python defined by RFC 0001.
//!
//! [`Interpreter`] is the embedding API. A typical caller wires
//! source through `weavepy_lexer` / `weavepy_parser` / `weavepy_compiler`
//! and then hands the resulting code object to [`Interpreter::run_module`].
//!
//! # Output capture
//!
//! Programs print via `print(...)`, which writes to a sink supplied
//! through [`Interpreter::set_stdout`]. Hosts that want to capture
//! output (REPL, test runners, the conformance harness) plug in a
//! `Vec<u8>` writer; the CLI uses the process stdout.

use crate::sync::Rc;
use crate::sync::{Cell, RefCell};
use std::io::Write;
use std::path::{Path, PathBuf};

use num_traits::{Signed, ToPrimitive, Zero};
use weavepy_compiler::{
    BinOpKind, CodeObject, CompareKind, Constant, ExcHandler, OpCode, UnaryKind, COOLDOWN,
};

pub mod builtin_types;
pub mod builtins;
pub mod error;
pub mod ext_loader;
pub mod frozen_code_cache;
pub mod gc_trace;
pub mod gil;
pub mod import;
pub mod object;
pub mod pycache;
pub mod specialize;
pub mod stdlib;
pub mod sync;
pub mod thread_registry;
pub mod types;
pub mod vm_singletons;
pub mod weakref_registry;

use crate::builtin_types::{builtin_types, instance_is_subclass, make_exception_with_class};
use crate::error::{
    attribute_error, import_error, index_error, key_error, module_not_found_error, name_error,
    runtime_error, stop_async_iteration, stop_iteration, stop_iteration_with, type_error,
    value_error, zero_division_error, TracebackEntry,
};
pub use crate::error::{PyException, RuntimeError};
pub use crate::import::ModuleCache;
use crate::import::{package_search_path, resolve_relative};
use crate::object::{
    BoundMethod, BuiltinFn, DictData, DictKey, GeneratorState, Object, PyFrame, PyFunction,
    PyGenerator, PyModule, PySlice, PyTraceback,
};
use crate::types::{PyInstance, TypeObject};

// ---------- frame ----------

struct Frame {
    code: Rc<CodeObject>,
    /// Local variables, indexed by `LOAD_FAST` / `STORE_FAST`.
    locals: Vec<Object>,
    /// Cell storage. Layout: `code.cellvars` first, then `code.freevars`.
    cells: Vec<Rc<RefCell<Object>>>,
    /// Evaluation stack.
    stack: Vec<Object>,
    /// Globals shared across frames within the same module.
    globals: Rc<RefCell<DictData>>,
    /// For class-body frames, names are stored here instead of globals.
    /// `None` for ordinary function and module frames.
    class_namespace: Option<Rc<RefCell<DictData>>>,
    /// Stack of currently-handled exceptions. `PUSH_EXC_INFO` pushes
    /// onto this; `POP_EXCEPT` pops; `RERAISE 1` re-raises the top.
    exc_handlers: Vec<PyException>,
    /// pc *before* the current instruction — used to look up the
    /// exception handler when an opcode raises.
    pc: u32,
}

impl Frame {
    fn push(&mut self, v: Object) {
        self.stack.push(v);
    }

    fn pop(&mut self) -> Result<Object, RuntimeError> {
        self.stack
            .pop()
            .ok_or_else(|| RuntimeError::Internal("stack underflow".to_owned()))
    }

    fn top(&self) -> Result<&Object, RuntimeError> {
        self.stack
            .last()
            .ok_or_else(|| RuntimeError::Internal("stack empty".to_owned()))
    }

    /// Peek `n` elements down from the top (`n == 0` is TOS,
    /// `n == 1` is TOS-1, etc.). Used by RFC 0021's specialized
    /// fast paths to inspect operands without popping them.
    #[inline]
    fn peek_back(&self, n: usize) -> Option<&Object> {
        let len = self.stack.len();
        if n >= len {
            return None;
        }
        self.stack.get(len - 1 - n)
    }
}

// ---------- interpreter ----------

/// Output sink. Either the process's stdout or a `Vec<u8>` for
/// embedding callers. The `+ Send + Sync` bound is what lets
/// `Object::File(PyFile { … stdout sink … })` cross thread
/// boundaries (RFC 0025).
pub type Stdout = Rc<RefCell<dyn Write + Send + Sync>>;

/// Cross-cutting CLI-driven flags the VM honours.
///
/// Defined here (rather than on the `weavepy` umbrella crate) so the
/// VM can apply them without a circular dependency. The `weavepy`
/// crate re-exports this as `weavepy::InterpreterFlags`.
#[derive(Debug, Clone, Default)]
pub struct InterpreterFlags {
    pub optimize: u8,
    pub dont_write_bytecode: bool,
    pub inspect: bool,
    pub verbose: bool,
    pub no_site: bool,
    pub no_user_site: bool,
    pub ignore_environment: bool,
    pub isolated: bool,
    pub quiet: bool,
    pub unbuffered: bool,
    pub skip_first_line: bool,
    pub bytes_warning: u8,
    pub safe_path: bool,
    pub debug: bool,
    pub xoptions: Vec<String>,
    pub warning_filters: Vec<String>,
    pub hash_seed: Option<u32>,
}

/// The top-level entry point for executing WeavePy bytecode.
#[allow(missing_debug_implementations)]
pub struct Interpreter {
    stdout: Stdout,
    builtins: Rc<RefCell<DictData>>,
    cache: ModuleCache,
    /// Live call stack of Python-visible frame snapshots, in
    /// outer-to-inner order. The topmost entry corresponds to the
    /// currently-executing `Frame`. RFC 0018: used by
    /// `sys._getframe`, `traceback`, and the unwind machinery.
    pub(crate) frame_stack: Rc<RefCell<Vec<Rc<PyFrame>>>>,
    /// Stack of currently-handled exceptions across all frames. The
    /// top is what `sys.exc_info()` returns. Pushed by
    /// `PUSH_EXC_INFO`; popped by `POP_EXCEPT`.
    pub(crate) exc_info_stack: Rc<RefCell<Vec<PyException>>>,
    /// User-installable hook called when an exception escapes the
    /// top-level frame. Defaults to a Rust builtin that prints the
    /// canonical CPython-style traceback to `sys.stderr`.
    /// Reachable today through `sys.excepthook`; reserved for the
    /// VM's top-level exit handler.
    #[allow(dead_code)]
    pub(crate) excepthook: Rc<RefCell<Object>>,
    /// Companion to `excepthook` for unraisable exceptions (e.g.
    /// errors during `__del__`). Reserved for future use.
    #[allow(dead_code)]
    pub(crate) unraisable_hook: Rc<RefCell<Object>>,
}

impl Default for Interpreter {
    fn default() -> Self {
        let stdout: Stdout = Rc::new(RefCell::new(std::io::stdout()));
        let mut builtins_dict = builtins::default_builtins();
        // Wire `print` directly into the shared builtins dict so that
        // user-driven `exec` / `eval` (which builds an arbitrary
        // globals dict) can still find it via the normal fallback in
        // `lookup_global_or_builtin`. The VM still intercepts the
        // call to drive `__str__` dispatch — only the dict entry
        // moves; the dispatch path is unchanged.
        builtins_dict.insert(
            DictKey(Object::from_static("print")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "print",
                call: Box::new(|_args| Err(runtime_error("internal: print called outside VM"))),
                call_kw: None,
            })),
        );
        let builtins = Rc::new(RefCell::new(builtins_dict));
        let cache = ModuleCache::default();
        stdlib::register_all(&cache);
        let excepthook = Rc::new(RefCell::new(Object::None));
        let unraisable_hook = Rc::new(RefCell::new(Object::None));
        let frame_stack: Rc<RefCell<Vec<Rc<PyFrame>>>> = Rc::new(RefCell::new(Vec::new()));
        let exc_info_stack = Rc::new(RefCell::new(Vec::new()));
        // Eagerly build the `sys` module so the per-interpreter
        // frame_stack / exc_info_stack are visible to user code via
        // `sys._getframe` and `sys.exc_info()`. The factory in the
        // module cache is left in place as a fallback for embedders
        // that explicitly clear `sys.modules`.
        let sys_module = crate::stdlib::sys::build_with_state(
            &cache,
            frame_stack.clone(),
            exc_info_stack.clone(),
            excepthook.clone(),
            unraisable_hook.clone(),
        );
        cache.insert("sys", Object::Module(sys_module));
        let interp = Self {
            stdout,
            builtins,
            cache,
            frame_stack,
            exc_info_stack,
            excepthook,
            unraisable_hook,
        };
        // RFC 0025: publish the shared parts of this interpreter
        // (builtins / module cache / stdout / hooks) so workers
        // spawned by `_thread.start_new_thread` can fork from us
        // instead of paying for a fresh `Interpreter::new()`. The
        // worker gets its own frame_stack / exc_info_stack so the
        // dispatch loops don't trample each other.
        crate::vm_singletons::publish_interpreter_seed(&interp);
        interp
    }
}

impl Interpreter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Plug in a custom stdout sink (e.g. a `Vec<u8>` for tests).
    pub fn set_stdout(&mut self, stdout: Stdout) {
        self.stdout = stdout;
    }

    /// RFC 0025: build a worker [`Interpreter`] that shares all
    /// shared state with `self` (builtins, module cache, stdout,
    /// excepthook, unraisable hook) but owns a **fresh** frame
    /// stack and exception stack. Spawned threads call this to
    /// get their own dispatch context without paying for a full
    /// `Interpreter::new()` (which would re-build the entire
    /// `sys.modules` table from scratch).
    ///
    /// All shared state crosses the thread boundary because every
    /// underlying handle is `Arc<…>` and every `Object` variant is
    /// `Send + Sync` (see the compile-time assertion in `object.rs`).
    pub fn fork_for_thread(&self) -> Self {
        Self {
            stdout: self.stdout.clone(),
            builtins: self.builtins.clone(),
            cache: self.cache.clone(),
            frame_stack: Rc::new(RefCell::new(Vec::new())),
            exc_info_stack: Rc::new(RefCell::new(Vec::new())),
            excepthook: self.excepthook.clone(),
            unraisable_hook: self.unraisable_hook.clone(),
        }
    }

    /// Expose the module cache (so the embedding host can poke
    /// `sys.modules`, register custom built-in modules, etc.).
    pub fn module_cache(&self) -> &ModuleCache {
        &self.cache
    }

    /// Replace `sys.argv` with the given values. The first entry is
    /// the script name; subsequent entries are passed-through args.
    pub fn set_argv<I, S>(&self, values: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut argv = self.cache.argv.borrow_mut();
        argv.clear();
        for v in values {
            argv.push(Object::from_str(v.into()));
        }
    }

    /// Prepend a directory to `sys.path`. Idempotent.
    pub fn prepend_path(&self, dir: impl Into<PathBuf>) {
        let s = dir.into().to_string_lossy().into_owned();
        let mut path = self.cache.path.borrow_mut();
        if !path_contains(&path, &s) {
            path.insert(0, Object::from_str(s));
        }
    }

    /// Append a directory to `sys.path`. Idempotent.
    pub fn append_path(&self, dir: impl Into<PathBuf>) {
        let s = dir.into().to_string_lossy().into_owned();
        let mut path = self.cache.path.borrow_mut();
        if !path_contains(&path, &s) {
            path.push(Object::from_str(s));
        }
    }

    /// Apply the cross-cutting `InterpreterFlags` set the embedding
    /// host wants the VM to honour. Reflected on `sys.flags`,
    /// `sys._xoptions`, `sys.warnoptions`, and
    /// `sys.dont_write_bytecode`. Called once at startup before any
    /// user code runs.
    pub fn apply_run_options(&mut self, opts: &InterpreterFlags) {
        let flags = &opts;
        if let Some(Object::Module(m)) = self
            .cache
            .modules
            .borrow()
            .get(&crate::object::DictKey(Object::from_static("sys")))
        {
            let mut d = m.dict.borrow_mut();
            if let Some(Object::Dict(fl)) = d
                .get(&crate::object::DictKey(Object::from_static("flags")))
                .cloned()
            {
                let mut fld = fl.borrow_mut();
                let set = |fld: &mut crate::object::DictData, k: &'static str, v: i64| {
                    fld.insert(
                        crate::object::DictKey(Object::from_static(k)),
                        Object::Int(v),
                    );
                };
                set(&mut fld, "optimize", flags.optimize.into());
                set(
                    &mut fld,
                    "dont_write_bytecode",
                    i64::from(flags.dont_write_bytecode),
                );
                set(&mut fld, "inspect", i64::from(flags.inspect));
                set(&mut fld, "interactive", i64::from(flags.inspect));
                set(&mut fld, "verbose", i64::from(flags.verbose));
                set(&mut fld, "quiet", i64::from(flags.quiet));
                set(&mut fld, "no_site", i64::from(flags.no_site));
                set(&mut fld, "no_user_site", i64::from(flags.no_user_site));
                set(
                    &mut fld,
                    "ignore_environment",
                    i64::from(flags.ignore_environment),
                );
                set(&mut fld, "isolated", i64::from(flags.isolated));
                set(&mut fld, "bytes_warning", flags.bytes_warning.into());
                set(&mut fld, "safe_path", i64::from(flags.safe_path));
                set(&mut fld, "debug", i64::from(flags.debug));
                set(
                    &mut fld,
                    "hash_randomization",
                    flags.hash_seed.map_or(1, |_| 0),
                );
                set(&mut fld, "utf8_mode", 1);
                set(&mut fld, "dev_mode", 0);
                set(&mut fld, "int_max_str_digits", 4300);
                set(&mut fld, "warn_default_encoding", 0);
            }
            d.insert(
                crate::object::DictKey(Object::from_static("dont_write_bytecode")),
                Object::Bool(flags.dont_write_bytecode),
            );
            d.insert(
                crate::object::DictKey(Object::from_static("warnoptions")),
                Object::new_list(
                    flags
                        .warning_filters
                        .iter()
                        .map(|s| Object::from_str(s.clone()))
                        .collect(),
                ),
            );
            // `sys._xoptions` is a dict whose values are either
            // `True` (for bare keys) or the `value` part of
            // `-X key=value`.
            let mut xopts = crate::object::DictData::new();
            for raw in &flags.xoptions {
                let (k, v) = match raw.split_once('=') {
                    Some((k, v)) => (k.to_owned(), Object::from_str(v.to_owned())),
                    None => (raw.clone(), Object::Bool(true)),
                };
                xopts.insert(crate::object::DictKey(Object::from_str(k)), v);
            }
            d.insert(
                crate::object::DictKey(Object::from_static("_xoptions")),
                Object::Dict(crate::sync::Rc::new(crate::sync::RefCell::new(xopts))),
            );
        }
    }

    /// Run the `site` module on first interpreter start, mirroring
    /// CPython's bootstrap. We `import site` if available, then call
    /// `site.main()`. Errors are intentionally swallowed — a broken
    /// `.pth` file shouldn't kill the whole interpreter.
    pub fn run_site(&mut self) -> Result<(), RuntimeError> {
        let site = match self.import_path("site") {
            Ok(m) => m,
            Err(_) => return Ok(()),
        };
        if let Object::Module(m) = site {
            let main_fn = m
                .dict
                .borrow()
                .get(&crate::object::DictKey(Object::from_static("main")))
                .cloned();
            if let Some(main_fn) = main_fn {
                let globals = m.dict.clone();
                let _ = self.call(&main_fn, &[], &[], &globals);
            }
        }
        Ok(())
    }

    /// Wire `print` (and friends) to this interpreter's stdout.
    /// `print` is installed as a special builtin — the VM intercepts
    /// the call so it can dispatch `__str__` on user types.
    fn install_print_into(&self, dict: &mut DictData) {
        let f = BuiltinFn {
            name: "print",
            call: Box::new(move |_args: &[Object]| {
                Err(runtime_error("internal: print called outside VM"))
            }),
            call_kw: None,
        };
        dict.insert(
            DictKey(Object::from_static("print")),
            Object::Builtin(Rc::new(f)),
        );
    }

    /// Run a module-level [`CodeObject`] as `__main__`. The most
    /// common entry point for the CLI and embedding hosts.
    pub fn run_module(&mut self, code: &CodeObject) -> Result<Object, RuntimeError> {
        self.run_module_as(code, "__main__", None)
    }

    /// Hand back the shared `__builtins__` dict so the REPL can drop
    /// it into a synthetic `__main__` module's globals.
    pub fn builtins_dict(&self) -> Rc<RefCell<DictData>> {
        self.builtins.clone()
    }

    /// Public dispatch entry point used by the C-API
    /// (RFC 0022). Equivalent to invoking `callable(*args, **kwargs)`
    /// in source, but reachable from outside the VM dispatch loop.
    /// Mostly used by `PyObject_Call` / `PyObject_CallObject` /
    /// `PyObject_CallMethod` in the C-API bridge.
    pub fn call_object(
        &mut self,
        callable: Object,
        args: &[Object],
        kwargs: &[(String, Object)],
    ) -> Result<Object, RuntimeError> {
        let _interp_guard =
            crate::vm_singletons::publish_interpreter_ptr(std::ptr::from_mut::<Self>(self));
        let _handles = self.activate_thread_handles();
        let globals = self.builtins.clone();
        self.call(&callable, args, kwargs, &globals)
    }

    /// Public iterator-construction entry point. Mirrors `iter(o)`.
    /// Used by `PyObject_GetIter` in the C-API.
    pub fn iter_object(&mut self, value: Object) -> Result<Object, RuntimeError> {
        let _interp_guard =
            crate::vm_singletons::publish_interpreter_ptr(std::ptr::from_mut::<Self>(self));
        let _handles = self.activate_thread_handles();
        let globals = self.builtins.clone();
        self.make_iter(&value, &globals)
    }

    /// Pull the next value out of an iterator (`next(it)`), returning
    /// `Ok(None)` for `StopIteration`. Used by `PyIter_Next` in the
    /// C-API.
    pub fn iter_next_object(&mut self, iter: Object) -> Result<Option<Object>, RuntimeError> {
        let _interp_guard =
            crate::vm_singletons::publish_interpreter_ptr(std::ptr::from_mut::<Self>(self));
        let _handles = self.activate_thread_handles();
        let globals = self.builtins.clone();
        self.iter_next(&iter, &globals)
    }

    /// RFC 0025: install this interpreter's per-thread handles as
    /// the active set for the calling OS thread. The returned guard
    /// pops the handles on drop, restoring the previous registration
    /// (so re-entrant calls from a C-extension don't trample each
    /// other).
    ///
    /// Every public entry point that hands control to user Python
    /// (`call_object`, `iter_object`, `iter_next_object`,
    /// `run_module_as`, `exec_module_in`) calls this on the way in.
    /// The `sys` module builtins read through
    /// `vm_singletons::current_thread_handles()` so `sys.exc_info()`,
    /// `sys._getframe()` etc. always see the *current* thread's
    /// state — critical now that worker threads have their own
    /// forked interpreter with independent frame / exception stacks.
    pub(crate) fn activate_thread_handles(&self) -> crate::vm_singletons::ThreadHandlesGuard {
        crate::vm_singletons::activate_thread_handles(crate::vm_singletons::ThreadHandles {
            frame_stack: self.frame_stack.clone(),
            exc_info_stack: self.exc_info_stack.clone(),
            excepthook: self.excepthook.clone(),
            unraisable_hook: self.unraisable_hook.clone(),
        })
    }

    /// `True` when `__pycache__` writes are forbidden — either by
    /// `-B` / `PYTHONDONTWRITEBYTECODE` at startup, or because user
    /// code mutated `sys.dont_write_bytecode = True` mid-run. Reads
    /// the live `sys` module dict.
    fn bytecode_writes_disabled(&self) -> bool {
        let sys = self.cache.modules.borrow();
        let key = crate::object::DictKey(Object::from_static("sys"));
        let Some(sys_mod) = sys.get(&key) else {
            return false;
        };
        let dict = match sys_mod {
            Object::Module(m) => m.dict.clone(),
            _ => return false,
        };
        drop(sys);
        crate::pycache::dont_write_bytecode(&dict)
    }

    /// Execute a compiled module-level code object against an
    /// externally-supplied globals dict (rather than a fresh one
    /// created by [`Self::build_module_globals`]). The REPL uses
    /// this so user-typed names persist across prompts.
    pub fn exec_module_in(
        &mut self,
        code: &CodeObject,
        globals: Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let _handles = self.activate_thread_handles();
        let code_rc = Rc::new(code.clone());
        let mut frame = self.make_frame(code_rc, Vec::new(), Vec::new(), globals, true);
        self.run_frame(&mut frame)
    }

    /// As [`run_module`], but lets the caller pick the `__name__` and
    /// optional `__file__` to install in the module's globals.
    pub fn run_module_as(
        &mut self,
        code: &CodeObject,
        name: &str,
        file: Option<&str>,
    ) -> Result<Object, RuntimeError> {
        let _handles = self.activate_thread_handles();
        let globals = self.build_module_globals(name, file, None);
        // Insert the module into `sys.modules` so callers can introspect
        // `sys.modules["__main__"]` (pickle by qualified name and the
        // multiprocessing spawn helper both rely on this).
        let module = Rc::new(PyModule {
            name: name.to_owned(),
            filename: file.map(|f| f.to_owned()),
            dict: globals.clone(),
        });
        self.cache.insert(name, Object::Module(module));
        let code_rc = Rc::new(code.clone());
        let mut frame = self.make_frame(code_rc, Vec::new(), Vec::new(), globals, true);
        let result = self.run_frame(&mut frame);
        // After the module finishes, run any deferred `__del__`
        // finalizers queued by the cycle GC. Errors propagate via
        // `sys.unraisablehook` (logged + suppressed) — a finalizer
        // failure must not change the program's overall exit
        // status.
        self.run_pending_finalizers();
        result
    }

    /// Invoke any `__del__` finalizers queued by the cycle GC.
    /// Each finalizer runs at most once. Exceptions from a
    /// finalizer are routed through `sys.unraisablehook` (today
    /// just logged to stderr) so they don't propagate.
    pub fn run_pending_finalizers(&mut self) {
        loop {
            let pending = crate::vm_singletons::drain_pending_finalizers();
            if pending.is_empty() {
                return;
            }
            for obj in pending {
                if let Object::Instance(inst) = &obj {
                    if let Some(del) = inst.class.lookup("__del__") {
                        let bound = Object::BoundMethod(Rc::new(BoundMethod {
                            receiver: obj.clone(),
                            function: del,
                        }));
                        let kwargs: Vec<(String, Object)> = Vec::new();
                        let outer = Rc::new(RefCell::new(DictData::new()));
                        let _ = self.call(&bound, &[], &kwargs, &outer);
                    }
                }
            }
        }
    }

    /// Populate a fresh module-globals dict with builtins, builtin
    /// types, and the standard module dunders. Used by both
    /// `run_module_as` and the import loader.
    fn build_module_globals(
        &self,
        name: &str,
        file: Option<&str>,
        package: Option<&str>,
    ) -> Rc<RefCell<DictData>> {
        let globals = Rc::new(RefCell::new(DictData::new()));
        let mut g = globals.borrow_mut();
        self.install_print_into(&mut g);
        for (n, value) in builtin_types().as_globals() {
            g.insert(DictKey(Object::from_str(n)), value);
        }
        g.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_str(name),
        );
        g.insert(DictKey(Object::from_static("__doc__")), Object::None);
        g.insert(
            DictKey(Object::from_static("__package__")),
            match package {
                Some(p) => Object::from_str(p),
                None => Object::from_static(""),
            },
        );
        if let Some(f) = file {
            g.insert(
                DictKey(Object::from_static("__file__")),
                Object::from_str(f),
            );
        }
        g.insert(
            DictKey(Object::from_static("__builtins__")),
            Object::Dict(self.builtins.clone()),
        );
        drop(g);
        globals
    }

    fn make_frame(
        &self,
        code: Rc<CodeObject>,
        positional: Vec<Object>,
        closure: Vec<Object>,
        globals: Rc<RefCell<DictData>>,
        _is_module: bool,
    ) -> Frame {
        let mut locals = vec![Object::None; code.varnames.len()];
        for (i, v) in positional.into_iter().enumerate() {
            if i < locals.len() {
                locals[i] = v;
            }
        }
        // Build cells: cellvars come first (fresh), then freevars
        // (provided by the caller via `closure`).
        let mut cells: Vec<Rc<RefCell<Object>>> =
            Vec::with_capacity(code.cellvars.len() + code.freevars.len());
        for cell_name in &code.cellvars {
            // If a cellvar matches a parameter name, the initial
            // value comes from `locals` — promote it.
            let initial = code
                .varnames
                .iter()
                .position(|n| n == cell_name)
                .map_or(Object::None, |idx| locals[idx].clone());
            cells.push(Rc::new(RefCell::new(initial)));
        }
        for cell in closure {
            match cell {
                Object::Cell(c) => cells.push(c),
                other => cells.push(Rc::new(RefCell::new(other))),
            }
        }
        Frame {
            code,
            locals,
            cells,
            stack: Vec::with_capacity(16),
            globals,
            class_namespace: None,
            exc_handlers: Vec::new(),
            pc: 0,
        }
    }

    // ---------- dispatch ----------

    fn run_frame(&mut self, frame: &mut Frame) -> Result<Object, RuntimeError> {
        match self.run_until_yield_or_return(frame, None)? {
            FrameOutcome::Returned(v) => Ok(v),
            FrameOutcome::Yielded(_) => Err(RuntimeError::Internal(
                "generator frame yielded to a non-generator caller".to_owned(),
            )),
            FrameOutcome::StartGenerator => {
                // Caller of run_frame for a generator function uses
                // call_python which handles this case separately.
                Err(RuntimeError::Internal(
                    "generator start outside call_python".to_owned(),
                ))
            }
        }
    }

    /// Run the frame until it yields, returns, or starts a generator.
    /// When `sent` is `Some`, push it onto the stack first — this is
    /// how `gen.send(v)` resumes from `YIELD_VALUE`.
    fn run_until_yield_or_return(
        &mut self,
        frame: &mut Frame,
        sent: Option<Object>,
    ) -> Result<FrameOutcome, RuntimeError> {
        if let Some(v) = sent {
            frame.push(v);
        }
        // Push a Python-visible frame snapshot for the duration of
        // this run. The same frame may be entered multiple times
        // (generators on resume) — each entry gets a fresh PyFrame
        // because the `back` chain reflects who is calling *now*.
        let py_frame = self.push_py_frame(frame);
        let result = loop {
            // Mirror the live `pc` into the snapshot so `f_lineno`
            // reads correctly when user code introspects via
            // `sys._getframe`.
            py_frame.lasti.set(frame.pc);
            // Re-sync the locals mirror so a child frame's
            // `f_back.f_locals` reflects this frame's mutations.
            self.sync_py_locals(frame);
            match self.step(frame) {
                Ok(StepOutcome::Continue) => {}
                Ok(StepOutcome::Return(v)) => break Ok(FrameOutcome::Returned(v)),
                Ok(StepOutcome::Yield(v)) => break Ok(FrameOutcome::Yielded(v)),
                Ok(StepOutcome::StartGenerator) => break Ok(FrameOutcome::StartGenerator),
                Err(err) => {
                    if let RuntimeError::PyException(exc) = err {
                        match self.handle_exception(frame, exc) {
                            Ok(Some(())) => continue,
                            Ok(None) => unreachable!(),
                            Err(e) => break Err(e),
                        }
                    } else {
                        break Err(err);
                    }
                }
            }
        };
        self.pop_py_frame();
        result
    }

    /// Build a [`PyFrame`] snapshot for `frame` and push it onto the
    /// interpreter's call stack. The snapshot's `back` chain points
    /// at whatever was on top of the stack before the push, so the
    /// call hierarchy is recoverable from any frame.
    fn push_py_frame(&self, frame: &Frame) -> Rc<PyFrame> {
        let varnames = frame.code.varnames.clone();
        let locals_snapshot = Rc::new(RefCell::new(frame.locals.clone()));
        let cell_names: Vec<String> = frame
            .code
            .cellvars
            .iter()
            .chain(frame.code.freevars.iter())
            .cloned()
            .collect();
        let cells_snapshot: Vec<Rc<RefCell<Object>>> = frame.cells.clone();
        let globals = frame.globals.clone();
        let class_ns = frame.class_namespace.clone();
        let snapshot_for_provider = locals_snapshot.clone();
        let provider: Rc<dyn Fn() -> Object + Send + Sync> = Rc::new(move || {
            let snapshot = snapshot_for_provider.borrow();
            // For module / class bodies the user-visible locals are
            // the corresponding namespace dict (class_ns when set,
            // otherwise globals).
            if let Some(ns) = class_ns.as_ref() {
                return Object::Dict(ns.clone());
            }
            // Function frames: copy the locals array into a dict so
            // user code can read by name. We honour cell variables
            // (their value lives in the cell, not the local slot).
            let mut d = DictData::new();
            for (name, value) in varnames.iter().zip(snapshot.iter()) {
                if matches!(value, Object::None) && cell_names.iter().any(|c| c == name) {
                    let idx = cell_names.iter().position(|c| c == name).unwrap();
                    if let Some(cell) = cells_snapshot.get(idx) {
                        d.insert(
                            DictKey(Object::from_str(name.clone())),
                            cell.borrow().clone(),
                        );
                        continue;
                    }
                }
                if !matches!(value, Object::None) {
                    d.insert(DictKey(Object::from_str(name.clone())), value.clone());
                }
            }
            // Cellvars not present in varnames (e.g. `__class__`).
            for (i, name) in cell_names.iter().enumerate() {
                if varnames.iter().any(|v| v == name) {
                    continue;
                }
                if let Some(cell) = cells_snapshot.get(i) {
                    d.insert(
                        DictKey(Object::from_str(name.clone())),
                        cell.borrow().clone(),
                    );
                }
            }
            Object::Dict(Rc::new(RefCell::new(d)))
        });
        let back = self.frame_stack.borrow().last().cloned();
        let py = Rc::new(PyFrame {
            code: frame.code.clone(),
            globals,
            builtins: self.builtins.clone(),
            lasti: Cell::new(frame.pc),
            back: RefCell::new(back),
            locals_cache: RefCell::new(None),
            locals_provider: RefCell::new(Some(provider)),
            locals_mirror: RefCell::new(Some(locals_snapshot)),
            trace: RefCell::new(Object::None),
            override_lineno: Cell::new(None),
        });
        self.frame_stack.borrow_mut().push(py.clone());
        py
    }

    /// Refresh the live-locals mirror on the current Python frame.
    /// Called between bytecode steps so `sys._getframe(...).f_locals`
    /// reflects the most recent `STORE_FAST` / `DELETE_FAST`.
    fn sync_py_locals(&self, frame: &Frame) {
        if let Some(py) = self.frame_stack.borrow().last() {
            if let Some(mirror) = py.locals_mirror.borrow().as_ref() {
                let mut slot = mirror.borrow_mut();
                if slot.len() == frame.locals.len() {
                    slot.clone_from(&frame.locals);
                } else {
                    *slot = frame.locals.clone();
                }
            }
            py.invalidate_locals();
        }
    }

    fn pop_py_frame(&self) {
        self.frame_stack.borrow_mut().pop();
    }

    /// Run a single instruction. The `pc` is advanced past it; if the
    /// instruction returns from the frame we surface that via
    /// `StepOutcome::Return`.
    fn step(&mut self, frame: &mut Frame) -> Result<StepOutcome, RuntimeError> {
        let raised_at = frame.pc;
        let ins = frame
            .code
            .instructions
            .get(frame.pc as usize)
            .copied()
            .ok_or_else(|| {
                RuntimeError::Internal(format!(
                    "pc {} out of bounds in {}",
                    frame.pc, frame.code.name
                ))
            })?;
        // RFC 0021 — adaptive specialization. Each hot-opcode arm
        // consults `frame.code.caches.get(cache_pc)` and either
        // takes a fast path or runs the generic handler and
        // installs a specialization on the way out.
        let cache_pc = raised_at;
        specialize::record_dispatch();
        frame.pc += 1;
        match ins.op {
            OpCode::Nop | OpCode::Resume => {}
            OpCode::LoadConst => {
                let c = frame
                    .code
                    .constants
                    .get(ins.arg as usize)
                    .ok_or_else(|| RuntimeError::Internal("bad const index".to_owned()))?
                    .clone();
                frame.push(constant_to_object(c));
            }
            OpCode::LoadName => {
                let name = self.name_at(&frame.code, ins.arg)?;
                let from_ns = frame
                    .class_namespace
                    .as_ref()
                    .and_then(|ns| ns.borrow().get(&DictKey(Object::from_str(&name))).cloned());
                let v = match from_ns {
                    Some(v) => v,
                    None => self.lookup_global_or_builtin(&frame.globals, &name)?,
                };
                frame.push(v);
            }
            OpCode::LoadGlobal => {
                let v = self.specialized_load_global(frame, cache_pc, ins.arg)?;
                frame.push(v);
            }
            OpCode::LoadFast => {
                let v = frame
                    .locals
                    .get(ins.arg as usize)
                    .cloned()
                    .ok_or_else(|| RuntimeError::Internal("bad local index".to_owned()))?;
                if matches!(v, Object::None)
                    && frame
                        .code
                        .varnames
                        .get(ins.arg as usize)
                        .map(|n| !is_param(&frame.code, n))
                        .unwrap_or(false)
                {
                    // It's possible the variable hasn't been
                    // assigned yet. We push None as a placeholder.
                }
                frame.push(v);
            }
            OpCode::StoreFast => {
                let v = frame.pop()?;
                let slot = ins.arg as usize;
                if slot < frame.locals.len() {
                    frame.locals[slot] = v;
                }
            }
            OpCode::DeleteFast => {
                let slot = ins.arg as usize;
                if slot < frame.locals.len() {
                    frame.locals[slot] = Object::None;
                }
            }
            OpCode::StoreName => {
                let v = frame.pop()?;
                let name = self.name_at(&frame.code, ins.arg)?;
                if let Some(ns) = &frame.class_namespace {
                    ns.borrow_mut().insert(DictKey(Object::from_str(name)), v);
                } else {
                    frame
                        .globals
                        .borrow_mut()
                        .insert(DictKey(Object::from_str(name)), v);
                }
            }
            OpCode::StoreGlobal => {
                let v = frame.pop()?;
                let name = self.name_at(&frame.code, ins.arg)?;
                frame
                    .globals
                    .borrow_mut()
                    .insert(DictKey(Object::from_str(name)), v);
            }
            OpCode::DeleteName => {
                let name = self.name_at(&frame.code, ins.arg)?;
                if let Some(ns) = &frame.class_namespace {
                    if ns
                        .borrow_mut()
                        .shift_remove(&DictKey(Object::from_str(&name)))
                        .is_some()
                    {
                        return Ok(StepOutcome::Continue);
                    }
                }
                frame
                    .globals
                    .borrow_mut()
                    .shift_remove(&DictKey(Object::from_str(name)));
            }
            OpCode::DeleteGlobal => {
                let name = self.name_at(&frame.code, ins.arg)?;
                frame
                    .globals
                    .borrow_mut()
                    .shift_remove(&DictKey(Object::from_str(name)));
            }
            OpCode::LoadDeref => {
                let cell = frame
                    .cells
                    .get(ins.arg as usize)
                    .cloned()
                    .ok_or_else(|| RuntimeError::Internal("bad cell index".to_owned()))?;
                let v = cell.borrow().clone();
                frame.push(v);
            }
            OpCode::StoreDeref => {
                let v = frame.pop()?;
                let cell = frame
                    .cells
                    .get(ins.arg as usize)
                    .cloned()
                    .ok_or_else(|| RuntimeError::Internal("bad cell index".to_owned()))?;
                *cell.borrow_mut() = v;
            }
            OpCode::MakeCell => {
                let slot = ins.arg as usize;
                if slot >= frame.cells.len() {
                    return Err(RuntimeError::Internal(
                        "MakeCell index out of bounds".to_owned(),
                    ));
                }
            }
            OpCode::LoadClosure => {
                let cell = frame
                    .cells
                    .get(ins.arg as usize)
                    .cloned()
                    .ok_or_else(|| RuntimeError::Internal("bad cell index".to_owned()))?;
                frame.push(Object::Cell(cell));
            }
            OpCode::LoadAttr => {
                let v = self.specialized_load_attr(frame, cache_pc, ins.arg)?;
                frame.push(v);
            }
            OpCode::StoreAttr => {
                self.specialized_store_attr(frame, cache_pc, ins.arg)?;
            }
            OpCode::DeleteAttr => {
                let obj = frame.pop()?;
                let name = self.name_at(&frame.code, ins.arg)?;
                self.delete_attr(&obj, &name)?;
            }
            OpCode::BinarySubscr => {
                let i = frame.pop()?;
                let v = frame.pop()?;
                let r = if let Object::Instance(_) = &v {
                    if let Some(method) = instance_method(&v, "__getitem__") {
                        self.call(
                            &method,
                            std::slice::from_ref(&i),
                            &[],
                            &frame.globals.clone(),
                        )?
                    } else {
                        self.binary_subscr(&v, &i)?
                    }
                } else if let Object::Type(ty) = &v {
                    // `Foo[args]` — CPython looks up `__getitem__`
                    // on the *metaclass* first (so `EnumMeta` can
                    // intercept `Color['RED']`), then falls back to
                    // `__class_getitem__` on the class itself
                    // (PEP 560).
                    let meta = ty.metaclass_or_type();
                    let bt = builtin_types();
                    let meta_getitem = if Rc::ptr_eq(&meta, &bt.type_) {
                        None
                    } else {
                        meta.lookup("__getitem__")
                    };
                    if let Some(method) = meta_getitem {
                        let bound = Object::BoundMethod(Rc::new(BoundMethod {
                            receiver: Object::Type(ty.clone()),
                            function: method,
                        }));
                        self.call(
                            &bound,
                            std::slice::from_ref(&i),
                            &[],
                            &frame.globals.clone(),
                        )?
                    } else if let Some(method) = ty.lookup("__class_getitem__") {
                        let callable = match method {
                            Object::ClassMethod(inner) => (*inner).clone(),
                            Object::StaticMethod(inner) => (*inner).clone(),
                            other => other,
                        };
                        self.call(
                            &callable,
                            &[Object::Type(ty.clone()), i.clone()],
                            &[],
                            &frame.globals.clone(),
                        )?
                    } else if ty.flags.is_builtin && !ty.flags.is_exception {
                        // PEP 585 fallback — `list[int]`, `dict[str, int]`,
                        // etc. We build a SimpleNamespace-shaped
                        // GenericAlias with `__origin__` and `__args__`
                        // attributes, matching the duck-typed surface of
                        // `types.GenericAlias`. `isinstance(x, list[int])`
                        // and other reflective uses go through `__origin__`.
                        make_generic_alias(Object::Type(ty.clone()), i.clone())
                    } else {
                        self.binary_subscr(&v, &i)?
                    }
                } else {
                    self.binary_subscr(&v, &i)?
                };
                frame.push(r);
            }
            OpCode::StoreSubscr => {
                let i = frame.pop()?;
                let target = frame.pop()?;
                let value = frame.pop()?;
                if let Object::Instance(_) = &target {
                    if let Some(method) = instance_method(&target, "__setitem__") {
                        self.call(&method, &[i.clone(), value], &[], &frame.globals.clone())?;
                    } else {
                        self.store_subscr(&target, &i, value)?;
                    }
                } else {
                    self.store_subscr(&target, &i, value)?;
                }
            }
            OpCode::DeleteSubscr => {
                let i = frame.pop()?;
                let target = frame.pop()?;
                if let Object::Instance(_) = &target {
                    if let Some(method) = instance_method(&target, "__delitem__") {
                        self.call(
                            &method,
                            std::slice::from_ref(&i),
                            &[],
                            &frame.globals.clone(),
                        )?;
                    } else {
                        self.delete_subscr(&target, &i)?;
                    }
                } else {
                    self.delete_subscr(&target, &i)?;
                }
            }
            OpCode::BinaryOp => {
                let kind: BinOpKind = unsafe { std::mem::transmute(ins.arg as u8) };
                if !self.specialized_binary_op(frame, cache_pc, kind)? {
                    let b = frame.pop()?;
                    let a = frame.pop()?;
                    let r = self.dispatch_binary_op(&a, &b, kind, &frame.globals)?;
                    frame.push(r);
                }
            }
            OpCode::UnaryOp => {
                let v = frame.pop()?;
                let kind: UnaryKind = unsafe { std::mem::transmute(ins.arg as u8) };
                let r = unary_op(&v, kind)?;
                frame.push(r);
            }
            OpCode::CompareOp => {
                let kind: CompareKind = unsafe { std::mem::transmute(ins.arg as u8) };
                if !self.specialized_compare_op(frame, cache_pc, kind)? {
                    let b = frame.pop()?;
                    let a = frame.pop()?;
                    let r = self.dispatch_compare_op(&a, &b, kind, &frame.globals)?;
                    frame.push(Object::Bool(r));
                }
            }
            OpCode::IsOp => {
                let b = frame.pop()?;
                let a = frame.pop()?;
                let same = a.is_same(&b);
                let result = if ins.arg == 1 { !same } else { same };
                frame.push(Object::Bool(result));
            }
            OpCode::ContainsOp => {
                let container = frame.pop()?;
                let item = frame.pop()?;
                let found = if let Some(method) = instance_method(&container, "__contains__") {
                    let r = self.call(
                        &method,
                        std::slice::from_ref(&item),
                        &[],
                        &frame.globals.clone(),
                    )?;
                    r.is_truthy()
                } else {
                    container.contains(&item)?
                };
                let result = if ins.arg == 1 { !found } else { found };
                frame.push(Object::Bool(result));
            }
            OpCode::PopTop => {
                frame.pop()?;
            }
            OpCode::CopyTop => {
                let v = frame.top()?.clone();
                frame.push(v);
            }
            OpCode::Swap => {
                let depth = ins.arg as usize;
                let n = frame.stack.len();
                if depth >= 2 && depth <= n {
                    frame.stack.swap(n - 1, n - depth);
                }
            }
            OpCode::Call => {
                let argc = ins.arg as usize;
                let split_at = frame.stack.len().saturating_sub(argc);
                let mut args: Vec<Object> = frame.stack.split_off(split_at);
                let callable = frame.pop()?;
                // Zero-arg super(): inject __class__ from the free
                // cell named "__class__" and `self` from local 0.
                if args.is_empty() && is_super_callable(&callable) {
                    if let Some(class_cell) = find_cell(frame, "__class__") {
                        let class_obj = class_cell.borrow().clone();
                        if !matches!(class_obj, Object::None) {
                            let self_obj = frame.locals.first().cloned().unwrap_or(Object::None);
                            args.push(class_obj);
                            args.push(self_obj);
                        }
                    }
                }
                let r = self.call(&callable, &args, &[], &frame.globals)?;
                frame.push(r);
            }
            OpCode::CallKw => {
                let argc = ins.arg as usize;
                // Stack (top-down): kw_names_tuple, kw_values..., positional_values..., callable
                let names_obj = frame.pop()?;
                let names: Vec<String> = match names_obj {
                    Object::Tuple(items) => items.iter().map(|x| x.to_str()).collect(),
                    _ => {
                        return Err(RuntimeError::Internal(
                            "CallKw expects a tuple of names".to_owned(),
                        ))
                    }
                };
                let kwc = names.len();
                let split_kw_at = frame.stack.len().saturating_sub(kwc);
                let kw_values: Vec<Object> = frame.stack.split_off(split_kw_at);
                let split_pos_at = frame.stack.len().saturating_sub(argc);
                let pos_args: Vec<Object> = frame.stack.split_off(split_pos_at);
                let callable = frame.pop()?;
                let kw_pairs: Vec<(String, Object)> = names.into_iter().zip(kw_values).collect();
                let r = self.call(&callable, &pos_args, &kw_pairs, &frame.globals)?;
                frame.push(r);
            }
            OpCode::CallEx => {
                // CALL_FUNCTION_EX: `arg = 0` → stack has (callable,
                // args_tuple); `arg = 1` → (callable, args_tuple,
                // kwargs_dict).
                let has_kwargs = ins.arg == 1;
                let kwargs_obj = if has_kwargs { Some(frame.pop()?) } else { None };
                let args_obj = frame.pop()?;
                let callable = frame.pop()?;
                let pos_args: Vec<Object> = match args_obj {
                    Object::Tuple(items) => items.iter().cloned().collect(),
                    Object::List(items) => items.borrow().clone(),
                    other => {
                        return Err(crate::error::type_error(format!(
                            "argument after * must be an iterable, not {}",
                            other.type_name()
                        )))
                    }
                };
                let kw_pairs: Vec<(String, Object)> = match kwargs_obj {
                    None => Vec::new(),
                    Some(Object::Dict(d)) => d
                        .borrow()
                        .iter()
                        .map(|(k, v)| (k.0.to_str(), v.clone()))
                        .collect(),
                    Some(other) => {
                        return Err(crate::error::type_error(format!(
                            "argument after ** must be a mapping, not {}",
                            other.type_name()
                        )))
                    }
                };
                let r = self.call(&callable, &pos_args, &kw_pairs, &frame.globals)?;
                frame.push(r);
            }
            OpCode::ReturnValue => {
                return Ok(StepOutcome::Return(frame.pop()?));
            }
            OpCode::PopJumpIfFalse => {
                let v = frame.pop()?;
                if !v.is_truthy() {
                    frame.pc += ins.arg;
                }
            }
            OpCode::PopJumpIfTrue => {
                let v = frame.pop()?;
                if v.is_truthy() {
                    frame.pc += ins.arg;
                }
            }
            OpCode::JumpForward => {
                frame.pc += ins.arg;
            }
            OpCode::JumpBackward => {
                frame.pc = frame.pc.saturating_sub(ins.arg);
            }
            OpCode::GetIter => {
                let v = frame.pop()?;
                let it = self.make_iter(&v, &frame.globals)?;
                frame.push(it);
            }
            OpCode::ForIter => {
                if self.specialized_for_iter(frame, cache_pc, ins.arg)? {
                    // Fast path consumed (or didn't); pc is already
                    // adjusted for exhaustion. Continue dispatch.
                    return Ok(StepOutcome::Continue);
                }
                let it_obj = frame
                    .stack
                    .last()
                    .cloned()
                    .ok_or_else(|| RuntimeError::Internal("FOR_ITER no iter".to_owned()))?;
                let next = match &it_obj {
                    Object::Iter(it) => it.borrow_mut().next_value(),
                    Object::Generator(g) => match self.generator_send(g, Object::None) {
                        Ok(v) => Some(v),
                        Err(RuntimeError::PyException(exc))
                            if exc.type_name() == "StopIteration" =>
                        {
                            None
                        }
                        Err(e) => return Err(e),
                    },
                    Object::Instance(_) => {
                        // Call __next__; treat StopIteration as exhaustion.
                        match instance_method(&it_obj, "__next__") {
                            Some(m) => match self.call(&m, &[], &[], &frame.globals) {
                                Ok(v) => Some(v),
                                Err(RuntimeError::PyException(exc))
                                    if exc.type_name() == "StopIteration" =>
                                {
                                    None
                                }
                                Err(e) => return Err(e),
                            },
                            None => {
                                return Err(type_error(
                                    "iter() returned non-iterator without __next__",
                                ));
                            }
                        }
                    }
                    _ => {
                        return Err(RuntimeError::Internal(
                            "FOR_ITER expects iterator on stack".to_owned(),
                        ))
                    }
                };
                match next {
                    Some(v) => frame.push(v),
                    None => {
                        frame.pop()?;
                        frame.pc += ins.arg;
                    }
                }
            }
            OpCode::EndFor => {
                // Iterator was already popped by the exhausted
                // branch of FOR_ITER. Nothing to do.
            }
            OpCode::BuildList => {
                let n = ins.arg as usize;
                let split = frame.stack.len().saturating_sub(n);
                let items = frame.stack.split_off(split);
                frame.push(Object::new_list(items));
            }
            OpCode::BuildTuple => {
                let n = ins.arg as usize;
                let split = frame.stack.len().saturating_sub(n);
                let items = frame.stack.split_off(split);
                frame.push(Object::new_tuple(items));
            }
            OpCode::BuildSet => {
                let n = ins.arg as usize;
                let split = frame.stack.len().saturating_sub(n);
                let items = frame.stack.split_off(split);
                frame.push(Object::new_set_from(items));
            }
            OpCode::BuildMap => {
                let n = ins.arg as usize;
                let split = frame.stack.len().saturating_sub(2 * n);
                let pairs = frame.stack.split_off(split);
                let mut d = DictData::new();
                let mut it = pairs.into_iter();
                for _ in 0..n {
                    let k = it.next().ok_or_else(|| {
                        RuntimeError::Internal("BUILD_MAP missing key".to_owned())
                    })?;
                    let v = it.next().ok_or_else(|| {
                        RuntimeError::Internal("BUILD_MAP missing value".to_owned())
                    })?;
                    d.insert(DictKey(k), v);
                }
                frame.push(Object::Dict(Rc::new(RefCell::new(d))));
            }
            OpCode::BuildString => {
                let n = ins.arg as usize;
                let split = frame.stack.len().saturating_sub(n);
                let parts = frame.stack.split_off(split);
                let mut s = String::new();
                for p in parts {
                    s.push_str(&p.to_str());
                }
                frame.push(Object::from_str(s));
            }
            OpCode::ListAppend => {
                let v = frame.pop()?;
                let depth = ins.arg as usize;
                let lst = frame
                    .stack
                    .get(frame.stack.len().wrapping_sub(depth))
                    .cloned()
                    .ok_or_else(|| {
                        RuntimeError::Internal("LIST_APPEND depth out of range".to_owned())
                    })?;
                if let Object::List(lst) = lst {
                    lst.borrow_mut().push(v);
                } else {
                    return Err(RuntimeError::Internal(
                        "LIST_APPEND target is not a list".to_owned(),
                    ));
                }
            }
            OpCode::SetAdd => {
                let v = frame.pop()?;
                let depth = ins.arg as usize;
                let s = frame
                    .stack
                    .get(frame.stack.len().wrapping_sub(depth))
                    .cloned()
                    .ok_or_else(|| {
                        RuntimeError::Internal("SET_ADD depth out of range".to_owned())
                    })?;
                if let Object::Set(s) = s {
                    s.borrow_mut().insert(DictKey(v));
                }
            }
            OpCode::MapAdd => {
                let v = frame.pop()?;
                let k = frame.pop()?;
                let depth = ins.arg as usize;
                let d = frame
                    .stack
                    .get(frame.stack.len().wrapping_sub(depth))
                    .cloned()
                    .ok_or_else(|| {
                        RuntimeError::Internal("MAP_ADD depth out of range".to_owned())
                    })?;
                if let Object::Dict(d) = d {
                    d.borrow_mut().insert(DictKey(k), v);
                }
            }
            OpCode::UnpackSequence => {
                let n = ins.arg as usize;
                if self.specialized_unpack_sequence(frame, cache_pc, n)? {
                    return Ok(StepOutcome::Continue);
                }
                let v = frame.pop()?;
                let items: Vec<Object> = match v {
                    Object::Tuple(items) => items.iter().cloned().collect(),
                    Object::List(items) => items.borrow().clone(),
                    Object::Str(s) => s.chars().map(|c| Object::from_str(c.to_string())).collect(),
                    Object::Range(r) => {
                        let mut out = Vec::new();
                        let mut cur = r.start;
                        while (r.step > 0 && cur < r.stop) || (r.step < 0 && cur > r.stop) {
                            out.push(Object::Int(cur));
                            cur += r.step;
                        }
                        out
                    }
                    Object::Bytes(b) => b.iter().map(|x| Object::Int(i64::from(*x))).collect(),
                    Object::ByteArray(b) => b
                        .borrow()
                        .iter()
                        .map(|x| Object::Int(i64::from(*x)))
                        .collect(),
                    Object::Set(s) => s.borrow().iter().map(|k| k.0.clone()).collect(),
                    Object::FrozenSet(s) => s.iter().map(|k| k.0.clone()).collect(),
                    Object::Generator(g) => {
                        let gen_obj = Object::Generator(g);
                        let globals = frame.globals.clone();
                        self.collect_iterable(&gen_obj, &globals)?
                    }
                    Object::Instance(_) | Object::Dict(_) | Object::Iter(_) => {
                        let globals = frame.globals.clone();
                        self.collect_iterable(&v, &globals)?
                    }
                    _ => {
                        return Err(type_error(format!(
                            "cannot unpack non-iterable {} object",
                            v.type_name()
                        )))
                    }
                };
                if items.len() != n {
                    return Err(value_error(format!(
                        "expected {} values to unpack, got {}",
                        n,
                        items.len()
                    )));
                }
                // Push in reverse so the first element ends up
                // at the lowest stack position — matches the
                // grouped STORE_FAST sequence emitted by the
                // compiler.
                for x in items.into_iter().rev() {
                    frame.push(x);
                }
            }
            OpCode::UnpackEx => {
                // RFC 0020: `a, *b, c = xs` style starred unpack.
                let before = ((ins.arg >> 8) & 0xFF) as usize;
                let after = (ins.arg & 0xFF) as usize;
                let v = frame.pop()?;
                let items: Vec<Object> = match v {
                    Object::Tuple(items) => items.iter().cloned().collect(),
                    Object::List(items) => items.borrow().clone(),
                    Object::Str(s) => s.chars().map(|c| Object::from_str(c.to_string())).collect(),
                    Object::Bytes(b) => b.iter().map(|x| Object::Int(i64::from(*x))).collect(),
                    Object::ByteArray(b) => b
                        .borrow()
                        .iter()
                        .map(|x| Object::Int(i64::from(*x)))
                        .collect(),
                    Object::Set(s) => s.borrow().iter().map(|k| k.0.clone()).collect(),
                    Object::FrozenSet(s) => s.iter().map(|k| k.0.clone()).collect(),
                    Object::Range(r) => {
                        let mut out = Vec::new();
                        let mut cur = r.start;
                        while (r.step > 0 && cur < r.stop) || (r.step < 0 && cur > r.stop) {
                            out.push(Object::Int(cur));
                            cur += r.step;
                        }
                        out
                    }
                    Object::Generator(g) => {
                        let gen_obj = Object::Generator(g);
                        let globals = frame.globals.clone();
                        self.collect_iterable(&gen_obj, &globals)?
                    }
                    Object::Instance(_) | Object::Dict(_) | Object::Iter(_) => {
                        let globals = frame.globals.clone();
                        self.collect_iterable(&v, &globals)?
                    }
                    _ => {
                        return Err(type_error(format!(
                            "cannot unpack non-iterable {} object",
                            v.type_name()
                        )))
                    }
                };
                if items.len() < before + after {
                    return Err(value_error(format!(
                        "not enough values to unpack (expected at least {}, got {})",
                        before + after,
                        items.len()
                    )));
                }
                // Pushed top-down so the next STOREs pop in source order.
                // Stack layout after: [..., tail_last, ..., tail_first, middle_list, head_last, ..., head_first]
                let mid_end = items.len() - after;
                let middle: Vec<Object> = items[before..mid_end].to_vec();
                // Tail: push reversed so STORE_FAST pops in source order.
                for x in items[mid_end..].iter().rev() {
                    frame.push(x.clone());
                }
                frame.push(Object::new_list(middle));
                // Head: reversed.
                for x in items[..before].iter().rev() {
                    frame.push(x.clone());
                }
            }
            OpCode::DictUpdate => {
                // Stack: [..., dict, other] -> [..., dict (updated)].
                let other = frame.pop()?;
                let dict = frame.top()?.clone();
                let target = match &dict {
                    Object::Dict(d) => d.clone(),
                    _ => {
                        return Err(type_error(
                            "DICT_UPDATE expects a dict on the stack".to_owned(),
                        ));
                    }
                };
                match other {
                    Object::Dict(src) => {
                        let mut t = target.borrow_mut();
                        for (k, v) in src.borrow().iter() {
                            t.insert(k.clone(), v.clone());
                        }
                    }
                    _ => {
                        // Iterate the mapping protocol via .keys() + subscript.
                        let globals = frame.globals.clone();
                        let key_method = self.load_attr(&other, "keys").map_err(|_| {
                            type_error("argument to ** must be a mapping".to_owned())
                        })?;
                        let keys = self.call(&key_method, &[], &[], &globals)?;
                        let keys = self.collect_iterable(&keys, &globals)?;
                        let mut t = target.borrow_mut();
                        for k in keys {
                            let value = self.binary_subscr(&other, &k).map_err(|_| {
                                type_error(format!(
                                    "cannot access key in {} for ** spread",
                                    other.type_name()
                                ))
                            })?;
                            t.insert(crate::object::DictKey(k), value);
                        }
                    }
                }
            }
            OpCode::MakeFunction => {
                let code_obj = frame.pop()?;
                let code = match code_obj {
                    Object::Code(c) => c,
                    _ => {
                        return Err(RuntimeError::Internal(
                            "MAKE_FUNCTION expects code on top".to_owned(),
                        ))
                    }
                };
                let flags = ins.arg;
                let mut closure: Vec<Object> = Vec::new();
                if flags & 0x08 != 0 {
                    let tup = frame.pop()?;
                    if let Object::Tuple(items) = tup {
                        closure = items.iter().cloned().collect();
                    }
                }
                // Flag 0x04 — annotations dict produced by the
                // compiler from ``def f(x: T) -> R`` annotations.
                let mut annotations_obj: Option<Object> = None;
                if flags & 0x04 != 0 {
                    annotations_obj = Some(frame.pop()?);
                }
                let mut kw_defaults: Vec<(String, Object)> = Vec::new();
                if flags & 0x02 != 0 {
                    let dict = frame.pop()?;
                    if let Object::Dict(d) = dict {
                        for (k, v) in d.borrow().iter() {
                            if let Object::Str(name) = &k.0 {
                                kw_defaults.push((name.to_string(), v.clone()));
                            }
                        }
                    }
                }
                let mut defaults: Vec<Object> = Vec::new();
                if flags & 0x01 != 0 {
                    let tup = frame.pop()?;
                    if let Object::Tuple(items) = tup {
                        defaults = items.iter().cloned().collect();
                    }
                }
                let name = code.name.clone();
                let attrs = Rc::new(RefCell::new(DictData::new()));
                // Stamp __module__ from globals['__name__'] (mirrors CPython's
                // function dispatch). Pickle relies on this to serialise the
                // function by qualified name.
                if let Some(name_obj) = frame
                    .globals
                    .borrow()
                    .get(&DictKey(Object::from_static("__name__")))
                    .cloned()
                {
                    attrs
                        .borrow_mut()
                        .insert(DictKey(Object::from_static("__module__")), name_obj);
                }
                attrs.borrow_mut().insert(
                    DictKey(Object::from_static("__qualname__")),
                    Object::from_str(name.clone()),
                );
                if let Some(ann) = annotations_obj {
                    attrs
                        .borrow_mut()
                        .insert(DictKey(Object::from_static("__annotations__")), ann);
                }
                let f = PyFunction {
                    name,
                    code,
                    globals: frame.globals.clone(),
                    defaults,
                    kw_defaults,
                    closure,
                    attrs,
                };
                frame.push(Object::Function(Rc::new(f)));
            }
            OpCode::BuildSlice => {
                let step = frame.pop()?;
                let stop = frame.pop()?;
                let start = frame.pop()?;
                frame.push(Object::Slice(Rc::new(PySlice { start, stop, step })));
            }
            OpCode::PrintExpr => {
                let v = frame.pop()?;
                if !matches!(v, Object::None) {
                    let mut sink = self.stdout.borrow_mut();
                    let _ = writeln!(sink, "{}", v.repr());
                }
            }
            OpCode::LoadBuildClass => {
                let f = builtins::build_class_builtin();
                frame.push(Object::Builtin(Rc::new(f)));
            }
            OpCode::LoadClassderef => {
                let idx = ins.arg as usize;
                let free_offset = frame.code.cellvars.len();
                let free_index = idx.saturating_sub(free_offset);
                let name = frame
                    .code
                    .freevars
                    .get(free_index)
                    .cloned()
                    .unwrap_or_default();
                let from_ns = frame
                    .class_namespace
                    .as_ref()
                    .and_then(|ns| ns.borrow().get(&DictKey(Object::from_str(&name))).cloned());
                let v = match from_ns {
                    Some(v) => v,
                    None => {
                        let cell =
                            frame.cells.get(idx).cloned().ok_or_else(|| {
                                RuntimeError::Internal("bad cell index".to_owned())
                            })?;
                        let v = cell.borrow().clone();
                        v
                    }
                };
                frame.push(v);
            }
            OpCode::RaiseVarargs => {
                let mut exc = match ins.arg {
                    0 => {
                        // Re-raise the currently-handled exception.
                        let top = frame
                            .exc_handlers
                            .last()
                            .cloned()
                            .ok_or_else(|| runtime_error("No active exception to re-raise"))?;
                        top
                    }
                    1 => {
                        let arg = frame.pop()?;
                        Self::normalize_exception(arg, None)?
                    }
                    2 => {
                        let cause = frame.pop()?;
                        let arg = frame.pop()?;
                        Self::normalize_exception(arg, Some(cause))?
                    }
                    other => {
                        return Err(RuntimeError::Internal(format!(
                            "RAISE_VARARGS arg out of range: {other}"
                        )))
                    }
                };
                self.attach_implicit_context(&mut exc);
                Self::sync_exc_attrs(&exc);
                return Err(RuntimeError::PyException(exc));
            }
            OpCode::CheckExcMatch => {
                // Compiler emits `CopyTop; <type>; CheckExcMatch`,
                // so on entry the stack ends in `[exc, exc, type]`.
                // We consume both the copied `exc` and the `type`,
                // leaving `[exc, bool]` for the no-match branch to
                // peek/POP appropriately.
                let ty = frame.pop()?;
                let exc = frame.pop()?;
                let matched = self.exception_matches(&exc, &ty)?;
                frame.push(Object::Bool(matched));
            }
            OpCode::CheckEGMatch => {
                // PEP 654: stack on entry `[..., exc, type]`. Splits
                // `exc` (an ExceptionGroup or a singleton) into the
                // matched and remaining pieces.
                let ty = frame.pop()?;
                let exc = frame.pop()?;
                let is_group = matches!(
                    &exc,
                    Object::Instance(i) if i.class.is_subclass_of(
                        &builtin_types().base_exception_group
                    )
                );
                let (matched, rest) = if is_group {
                    crate::builtin_types::split_exception_group(&exc, &ty)?
                } else {
                    // Singleton: matches the type or doesn't, no
                    // wrapping required (the spec says a bare
                    // exception is treated as a one-element group for
                    // matching purposes).
                    if self.exception_matches(&exc, &ty)? {
                        (exc.clone(), Object::None)
                    } else {
                        (Object::None, exc.clone())
                    }
                };
                frame.push(rest);
                frame.push(matched);
            }
            OpCode::PushExcInfo => {
                // The handler entry leaves the exception on top —
                // we record it for the duration of the handler. The
                // same exception goes onto the interpreter-wide
                // `exc_info_stack` so `sys.exc_info()` sees it.
                let exc = frame.top()?.clone();
                if let Object::Instance(_) = &exc {
                    let pe = PyException::new(exc);
                    frame.exc_handlers.push(pe.clone());
                    self.exc_info_stack.borrow_mut().push(pe);
                }
            }
            OpCode::PopExcept => {
                frame.exc_handlers.pop();
                self.exc_info_stack.borrow_mut().pop();
            }
            OpCode::Reraise => {
                let exc = if ins.arg == 0 {
                    frame.pop()?
                } else {
                    // `RERAISE 1` re-raises the currently-active exc.
                    let exc = frame
                        .exc_handlers
                        .last()
                        .map(|pe| pe.instance.clone())
                        .ok_or_else(|| runtime_error("No active exception to re-raise"))?;
                    exc
                };
                let pe = Self::normalize_exception(exc, None)?;
                Self::sync_exc_attrs(&pe);
                return Err(RuntimeError::PyException(pe));
            }
            OpCode::BeforeWith => {
                let cm = frame.pop()?;
                let exit_method = self.load_attr(&cm, "__exit__")?;
                let enter_method = self.load_attr(&cm, "__enter__")?;
                let entered = self.call(&enter_method, &[], &[], &frame.globals)?;
                // Stack on exit: [exit_method, entered_value]
                frame.push(exit_method);
                frame.push(entered);
            }
            OpCode::WithExceptStart => {
                // Stack on entry (top → bottom): [exc, exit]
                // We call exit(type(exc), exc, None) and push the
                // result, leaving exc and exit beneath.
                let exc = frame
                    .stack
                    .last()
                    .cloned()
                    .ok_or_else(|| RuntimeError::Internal("WITH_EXCEPT_START".to_owned()))?;
                let exit_method = frame
                    .stack
                    .get(frame.stack.len().wrapping_sub(2))
                    .cloned()
                    .ok_or_else(|| RuntimeError::Internal("WITH_EXCEPT_START".to_owned()))?;
                let ty = match &exc {
                    Object::Instance(inst) => Object::Type(inst.class.clone()),
                    _ => Object::None,
                };
                let result =
                    self.call(&exit_method, &[ty, exc, Object::None], &[], &frame.globals)?;
                frame.push(result);
            }
            OpCode::ImportName => {
                let fromlist = frame.pop()?;
                let level_obj = frame.pop()?;
                let level = match level_obj {
                    Object::Int(i) if i >= 0 => i as u32,
                    Object::Int(_) => {
                        return Err(value_error("relative import level must be non-negative"))
                    }
                    other => {
                        return Err(type_error(format!(
                            "level must be int, not '{}'",
                            other.type_name()
                        )))
                    }
                };
                let name = self.name_at(&frame.code, ins.arg)?;
                let module = self.do_import(&name, &fromlist, level, &frame.globals)?;
                frame.push(module);
            }
            OpCode::ImportFrom => {
                let module =
                    frame.stack.last().cloned().ok_or_else(|| {
                        RuntimeError::Internal("IMPORT_FROM empty stack".to_owned())
                    })?;
                let name = self.name_at(&frame.code, ins.arg)?;
                let attr = self.import_from(&module, &name)?;
                frame.push(attr);
            }
            OpCode::ImportStar => {
                let module = frame.pop()?;
                self.import_star(&module, &frame.globals)?;
            }
            OpCode::FormatValue => {
                let arg = ins.arg;
                let conversion = arg & 0x03;
                let has_spec = (arg & 0x04) != 0;
                let spec = if has_spec { Some(frame.pop()?) } else { None };
                let value = frame.pop()?;
                let globals = frame.globals.clone();
                let formatted = self.format_value(&value, conversion, spec.as_ref(), &globals)?;
                frame.push(Object::from_str(formatted));
            }
            OpCode::YieldValue => {
                let v = frame.pop()?;
                return Ok(StepOutcome::Yield(v));
            }
            OpCode::ReturnGenerator => {
                return Ok(StepOutcome::StartGenerator);
            }
            OpCode::GetYieldFromIter => {
                let v = frame.pop()?;
                let it = match v {
                    Object::Generator(_) => v,
                    other => self.make_iter(&other, &frame.globals)?,
                };
                frame.push(it);
            }
            OpCode::Send => {
                // CPython 3.13 SEND semantics: stack on entry is
                // `[..., iter, value]`. We pop `value`, peek `iter`,
                // and either push the yielded value (success) or
                // replace `value` with `e.value` and jump (StopIter).
                // The iterator stays at sub-top in BOTH cases — the
                // surrounding `END_SEND` pops it once the loop ends.
                let value = frame.pop()?;
                let iter = frame
                    .stack
                    .last()
                    .cloned()
                    .ok_or_else(|| RuntimeError::Internal("SEND empty stack".to_owned()))?;
                let result = match &iter {
                    Object::Generator(g) | Object::Coroutine(g) => self.generator_send(g, value),
                    Object::AsyncGenerator(g) => {
                        // Async-generator semantics under SEND
                        // (simple cooperative model — no support for
                        // `await` *inside* the agen body, which would
                        // require CPython's intermediate-value
                        // passthrough machinery):
                        //   * `agen` yields `v` -> asend completes
                        //     with value `v` (i.e. emulate
                        //     `StopIteration(v)` so SEND short-
                        //     circuits to `END_SEND`).
                        //   * `agen` returns -> raise
                        //     `StopAsyncIteration`.
                        //   * `agen` raises -> propagate.
                        match self.generator_send(g, value) {
                            Ok(v) => Err(stop_iteration_with(v)),
                            Err(RuntimeError::PyException(exc))
                                if exc.type_name() == "StopIteration" =>
                            {
                                Err(stop_async_iteration())
                            }
                            other => other,
                        }
                    }
                    Object::Iter(_) => {
                        if !matches!(value, Object::None) {
                            return Err(type_error(
                                "can't send non-None value to a just-started iterator",
                            ));
                        }
                        match self.iter_next(&iter, &frame.globals)? {
                            Some(v) => Ok(v),
                            None => Err(stop_iteration()),
                        }
                    }
                    _ => Err(type_error("SEND expects an iterator or generator")),
                };
                match result {
                    Ok(v) => frame.push(v),
                    Err(RuntimeError::PyException(exc)) if exc.type_name() == "StopIteration" => {
                        // CPython 3.13: SEND only short-circuits on
                        // `StopIteration`. `StopAsyncIteration` must
                        // propagate so the surrounding async-for's
                        // exception handler (END_ASYNC_FOR) can clean
                        // up.
                        let payload = exception_value(&exc.instance);
                        frame.push(payload);
                        frame.pc += ins.arg;
                    }
                    Err(e) => return Err(e),
                }
            }
            OpCode::EndSend => {
                // Stack: [iter, value]. Pop iter, keep value.
                let value = frame.pop()?;
                let _iter = frame.pop()?;
                frame.push(value);
            }
            OpCode::GetAwaitable => {
                let v = frame.pop()?;
                let it = self.get_awaitable(v)?;
                frame.push(it);
            }
            OpCode::GetAiter => {
                let v = frame.pop()?;
                let it = self.get_aiter(v, &frame.globals.clone())?;
                frame.push(it);
            }
            OpCode::GetAnext => {
                let aiter =
                    frame.stack.last().cloned().ok_or_else(|| {
                        RuntimeError::Internal("GET_ANEXT empty stack".to_owned())
                    })?;
                let globals = frame.globals.clone();
                let aw = self.get_anext(&aiter, &globals)?;
                frame.push(aw);
            }
            OpCode::EndAsyncFor => {
                // Stack: [aiter, exc]. We need to drop both. Re-raise
                // anything that isn't StopAsyncIteration.
                let exc = frame.pop()?;
                let _aiter = frame.pop()?;
                if !is_stop_async_iteration_obj(&exc) {
                    let py_exc = PyException::new(exc);
                    return Err(RuntimeError::PyException(py_exc));
                }
            }
            OpCode::BeforeAsyncWith => {
                let cm = frame.pop()?;
                let globals = frame.globals.clone();
                let aexit = self.load_attr(&cm, "__aexit__")?;
                let aenter = self.load_attr(&cm, "__aenter__")?;
                let aw = self.call(&aenter, &[], &[], &globals)?;
                frame.push(aexit);
                frame.push(aw);
            }
            OpCode::MatchSequence => {
                let v = frame.top()?;
                let is_seq = matches!(
                    v,
                    Object::Tuple(_) | Object::List(_) | Object::Range(_) | Object::Str(_)
                );
                frame.push(Object::Bool(is_seq));
            }
            OpCode::MatchMapping => {
                let v = frame.top()?;
                let is_map = matches!(v, Object::Dict(_));
                frame.push(Object::Bool(is_map));
            }
            OpCode::GetLen => {
                let len = frame.top()?.len()?;
                frame.push(Object::Int(len as i64));
            }
            OpCode::MatchKeys => {
                let keys_obj = frame.pop()?;
                let subject = frame.top()?.clone();
                let keys: Vec<Object> = match keys_obj {
                    Object::Tuple(items) => items.iter().cloned().collect(),
                    _ => {
                        return Err(RuntimeError::Internal(
                            "MATCH_KEYS expects tuple".to_owned(),
                        ))
                    }
                };
                let result = match &subject {
                    Object::Dict(d) => {
                        let d = d.borrow();
                        let mut values = Vec::with_capacity(keys.len());
                        let mut found = true;
                        for k in &keys {
                            if let Some(v) = d.get(&DictKey(k.clone())) {
                                values.push(v.clone());
                            } else {
                                found = false;
                                break;
                            }
                        }
                        if found {
                            Object::new_tuple(values)
                        } else {
                            Object::None
                        }
                    }
                    _ => Object::None,
                };
                frame.push(result);
            }
            OpCode::MatchClass => {
                let nargs = ins.arg as usize;
                let names_obj = frame.pop()?;
                let cls = frame.pop()?;
                let subject = frame.pop()?;
                let kw_names: Vec<String> = match names_obj {
                    Object::Tuple(items) => items.iter().map(|x| x.to_str()).collect(),
                    _ => {
                        return Err(RuntimeError::Internal(
                            "MATCH_CLASS expects tuple of names".to_owned(),
                        ))
                    }
                };
                let result = self.match_class(&subject, &cls, nargs, &kw_names, &frame.globals)?;
                frame.push(result);
            }
        }
        Ok(StepOutcome::Continue)
    }

    // ---------- exception handling ----------

    /// Look up a handler for `exc` at the current pc. If found,
    /// truncate the stack and jump to the handler. Otherwise propagate.
    fn handle_exception(
        &self,
        frame: &mut Frame,
        mut exc: PyException,
    ) -> Result<Option<()>, RuntimeError> {
        // Note `pc` was already advanced past the raising instruction,
        // so the protected range matches against `pc - 1`.
        let raise_pc = frame.pc.saturating_sub(1);
        let line = frame
            .code
            .linetable
            .get(raise_pc as usize)
            .copied()
            .unwrap_or(0);
        // Push a traceback entry for *this* frame regardless of
        // whether we end up handling here — CPython's `__traceback__`
        // includes the catching frame too. The chain grows
        // outward-from-raise as the exception propagates.
        self.append_traceback(&mut exc, frame, raise_pc, line);
        if let Some(handler) = find_handler(&frame.code.exception_table, raise_pc) {
            // Drop entries above the recorded stack depth.
            while frame.stack.len() > handler.depth as usize {
                frame.stack.pop();
            }
            // Push the exception instance for the handler to bind /
            // CHECK_EXC_MATCH to inspect.
            frame.stack.push(exc.instance.clone());
            frame.pc = handler.handler;
            return Ok(Some(()));
        }
        Err(RuntimeError::PyException(exc))
    }

    /// Push one frame's worth of `tb_*` info onto the exception's
    /// traceback chain — both the legacy `Vec<TracebackEntry>` (used
    /// for the cheap `RuntimeError` Display impl) and the new
    /// `PyTraceback` chain stored on the instance dict so Python code
    /// can walk `exc.__traceback__`.
    fn append_traceback(&self, exc: &mut PyException, frame: &Frame, lasti: u32, lineno: u32) {
        exc.push_traceback(TracebackEntry {
            filename: frame.code.filename.clone(),
            funcname: frame.code.name.clone(),
            lineno,
        });
        let py_frame = self
            .frame_stack
            .borrow()
            .last()
            .cloned()
            .unwrap_or_else(|| {
                // Fall back to a synthetic snapshot if for some
                // reason the stack is empty (shouldn't happen in
                // normal flow but keeps the chain non-empty).
                Rc::new(PyFrame {
                    code: frame.code.clone(),
                    globals: frame.globals.clone(),
                    builtins: self.builtins.clone(),
                    lasti: Cell::new(lasti),
                    back: RefCell::new(None),
                    locals_cache: RefCell::new(None),
                    locals_provider: RefCell::new(None),
                    locals_mirror: RefCell::new(None),
                    trace: RefCell::new(Object::None),
                    override_lineno: Cell::new(None),
                })
            });
        let new_tb = Rc::new(PyTraceback {
            frame: py_frame,
            lineno,
            lasti,
            next: RefCell::new(None),
        });
        // CPython chains outward: the catching frame ends up at the
        // *head*; `tb_next` walks toward the raise site. Each
        // propagation prepends the current frame's `tb` to the
        // existing chain.
        if let Object::Instance(inst) = &exc.instance {
            let key = DictKey(Object::from_static("__traceback__"));
            let prev = inst.dict.borrow().get(&key).cloned();
            if let Some(Object::Traceback(prev_tb)) = prev {
                *new_tb.next.borrow_mut() = Some(prev_tb);
            }
            inst.dict
                .borrow_mut()
                .insert(key, Object::Traceback(new_tb));
        }
    }

    /// If the most-recent handled exception is still active when
    /// `raise X` runs without a `from` clause, attach it as the new
    /// exception's `__context__` so chained tracebacks render
    /// `During handling of the above exception, another exception
    /// occurred:`. Mirrors PEP 3134 / CPython.
    fn attach_implicit_context(&self, exc: &mut PyException) {
        if exc.cause.is_some() {
            return;
        }
        let stack = self.exc_info_stack.borrow();
        let Some(ctx) = stack.last() else {
            return;
        };
        // Don't self-reference if user code re-raises through `raise`
        // (the existing context-handler is the same exception).
        if Rc::as_ptr(&match &ctx.instance {
            Object::Instance(i) => i.clone(),
            _ => return,
        }) == Rc::as_ptr(&match &exc.instance {
            Object::Instance(i) => i.clone(),
            _ => return,
        }) {
            return;
        }
        exc.context = Some(Box::new(ctx.clone()));
    }

    /// Mirror the `cause` / `context` chain onto the instance dict so
    /// Python code accessing `e.__cause__` / `e.__context__` sees
    /// the canonical values. Called right before raising.
    fn sync_exc_attrs(exc: &PyException) {
        if let Object::Instance(inst) = &exc.instance {
            let mut dict = inst.dict.borrow_mut();
            if let Some(cause) = exc.cause.as_ref() {
                dict.insert(
                    DictKey(Object::from_static("__cause__")),
                    cause.instance.clone(),
                );
                // Explicit cause suppresses __context__ rendering by
                // default; user code can still set __suppress_context__.
                dict.insert(
                    DictKey(Object::from_static("__suppress_context__")),
                    Object::Bool(true),
                );
            } else if !dict.contains_key(&DictKey(Object::from_static("__cause__"))) {
                dict.insert(DictKey(Object::from_static("__cause__")), Object::None);
            }
            if let Some(context) = exc.context.as_ref() {
                dict.insert(
                    DictKey(Object::from_static("__context__")),
                    context.instance.clone(),
                );
            } else if !dict.contains_key(&DictKey(Object::from_static("__context__"))) {
                dict.insert(DictKey(Object::from_static("__context__")), Object::None);
            }
            if !dict.contains_key(&DictKey(Object::from_static("__suppress_context__"))) {
                dict.insert(
                    DictKey(Object::from_static("__suppress_context__")),
                    Object::Bool(false),
                );
            }
            if !dict.contains_key(&DictKey(Object::from_static("__traceback__"))) {
                dict.insert(DictKey(Object::from_static("__traceback__")), Object::None);
            }
        }
    }

    /// Materialise a raised value into a [`PyException`]. Accepts an
    /// exception class (instantiates it) or an instance.
    fn normalize_exception(
        value: Object,
        cause: Option<Object>,
    ) -> Result<PyException, RuntimeError> {
        let bt = builtin_types();
        let inst = match value {
            Object::Type(t) => {
                if !t.flags.is_exception {
                    return Err(type_error(format!(
                        "exceptions must derive from BaseException, not '{}'",
                        t.name
                    )));
                }
                make_exception_with_class(t, "")
            }
            Object::Instance(inst) => {
                if !inst.class.flags.is_exception && !inst.class.is_subclass_of(&bt.base_exception)
                {
                    return Err(type_error("exceptions must derive from BaseException"));
                }
                Object::Instance(inst)
            }
            other => {
                return Err(type_error(format!(
                    "exceptions must derive from BaseException, not '{}'",
                    other.type_name()
                )));
            }
        };
        let mut pe = PyException::new(inst);
        if let Some(c) = cause {
            // ``raise X from None`` explicitly clears the cause and
            // signals ``__suppress_context__ = True`` so the printer
            // hides any implicit ``__context__`` chain. The exception
            // payload itself stays the same.
            if matches!(c, Object::None) {
                pe.cause = None;
                if let Object::Instance(ref inst_rc) = pe.instance {
                    inst_rc.dict.borrow_mut().insert(
                        crate::object::DictKey(Object::from_static("__suppress_context__")),
                        Object::Bool(true),
                    );
                }
            } else {
                let cpe = Self::normalize_exception(c, None)?;
                pe.cause = Some(Box::new(cpe));
            }
        }
        Ok(pe)
    }

    /// `True` if `exc`'s class is a subclass of the given type or any
    /// element of a tuple of types.
    fn exception_matches(&self, exc: &Object, ty: &Object) -> Result<bool, RuntimeError> {
        match ty {
            Object::Type(t) => Ok(instance_is_subclass(exc, t)),
            Object::Tuple(items) => {
                for t in items.iter() {
                    if let Object::Type(t) = t {
                        if instance_is_subclass(exc, t) {
                            return Ok(true);
                        }
                    }
                }
                Ok(false)
            }
            _ => Err(type_error(
                "catching classes that do not inherit from BaseException is not allowed",
            )),
        }
    }

    // ---------- helpers ----------

    fn name_at(&self, code: &CodeObject, arg: u32) -> Result<String, RuntimeError> {
        code.names
            .get(arg as usize)
            .cloned()
            .ok_or_else(|| RuntimeError::Internal("bad name index".to_owned()))
    }

    fn lookup_global_or_builtin(
        &self,
        globals: &Rc<RefCell<DictData>>,
        name: &str,
    ) -> Result<Object, RuntimeError> {
        let key = DictKey(Object::from_str(name));
        if let Some(v) = globals.borrow().get(&key) {
            return Ok(v.clone());
        }
        if let Some(v) = self.builtins.borrow().get(&key) {
            return Ok(v.clone());
        }
        Err(name_error(format!("name '{name}' is not defined")))
    }

    fn load_attr(&mut self, obj: &Object, name: &str) -> Result<Object, RuntimeError> {
        match obj {
            Object::Generator(g) | Object::Coroutine(g) | Object::AsyncGenerator(g) => {
                let allowed: &[&str] = match obj {
                    Object::Generator(_) => &["send", "throw", "close", "__next__", "__iter__"],
                    Object::Coroutine(_) => &["send", "throw", "close", "__await__"],
                    Object::AsyncGenerator(_) => {
                        &["__aiter__", "__anext__", "asend", "athrow", "aclose"]
                    }
                    _ => &[],
                };
                if allowed.contains(&name) {
                    let method = make_gen_method(name, obj);
                    return Ok(method);
                }
                match name {
                    "__name__" | "__qualname__" => Ok(Object::from_str(&g.name)),
                    _ => Err(attribute_error(format!(
                        "'{}' object has no attribute '{}'",
                        obj.type_name(),
                        name
                    ))),
                }
            }
            Object::Instance(inst) => self.load_attr_instance(inst, obj, name),
            Object::Type(ty) => self.load_attr_type(ty, name),
            Object::Property(p) => match name {
                "fget" => Ok(p.fget.clone()),
                "fset" => Ok(p.fset.clone()),
                "fdel" => Ok(p.fdel.clone()),
                "__doc__" => Ok(p.doc.clone()),
                _ => {
                    if let Some(method) = self.lookup_method(obj, name) {
                        return Ok(Object::BoundMethod(Rc::new(BoundMethod {
                            receiver: obj.clone(),
                            function: method,
                        })));
                    }
                    Err(attribute_error(format!(
                        "'property' object has no attribute '{}'",
                        name
                    )))
                }
            },
            Object::StaticMethod(inner) => match name {
                "__func__" => Ok((**inner).clone()),
                "__isabstractmethod__" => {
                    // Honour an `@abstractmethod` decorator applied
                    // *under* `@staticmethod` (`@staticmethod
                    // @abstractmethod def f(): ...`).
                    Ok(self
                        .load_attr(inner.as_ref(), "__isabstractmethod__")
                        .unwrap_or(Object::Bool(false)))
                }
                _ => Err(attribute_error(format!(
                    "'staticmethod' object has no attribute '{}'",
                    name
                ))),
            },
            Object::ClassMethod(inner) => match name {
                "__func__" => Ok((**inner).clone()),
                "__isabstractmethod__" => Ok(self
                    .load_attr(inner.as_ref(), "__isabstractmethod__")
                    .unwrap_or(Object::Bool(false))),
                _ => Err(attribute_error(format!(
                    "'classmethod' object has no attribute '{}'",
                    name
                ))),
            },
            Object::Module(m) => {
                if let Some(v) = m.dict.borrow().get(&DictKey(Object::from_str(name))) {
                    return Ok(v.clone());
                }
                match name {
                    "__name__" => return Ok(Object::from_str(&m.name)),
                    "__file__" => {
                        return Ok(m.filename.as_deref().map_or(Object::None, Object::from_str));
                    }
                    "__dict__" => return Ok(Object::Dict(m.dict.clone())),
                    _ => {}
                }
                Err(attribute_error(format!(
                    "module '{}' has no attribute '{}'",
                    m.name, name
                )))
            }
            Object::SimpleNamespace(d) => {
                if let Some(v) = d.borrow().get(&DictKey(Object::from_str(name))) {
                    return Ok(v.clone());
                }
                if name == "__dict__" {
                    return Ok(Object::Dict(d.clone()));
                }
                Err(attribute_error(format!(
                    "'SimpleNamespace' object has no attribute '{name}'"
                )))
            }
            Object::MappingProxy(d) => {
                if name == "__dict__" {
                    return Ok(Object::Dict(d.clone()));
                }
                Err(attribute_error(format!(
                    "'mappingproxy' object has no attribute '{name}'"
                )))
            }
            Object::MemoryView(mv) => match name {
                "nbytes" => Ok(Object::Int(mv.len.get() as i64)),
                "itemsize" => Ok(Object::Int(mv.itemsize.get() as i64)),
                "ndim" => Ok(Object::Int(1)),
                "readonly" => Ok(Object::Bool(mv.readonly.get())),
                "format" => Ok(Object::from_str(mv.format.borrow().as_str())),
                "shape" => Ok(Object::new_tuple(vec![Object::Int(mv.len.get() as i64)])),
                "strides" => Ok(Object::new_tuple(vec![Object::Int(
                    mv.itemsize.get() as i64
                )])),
                "suboffsets" => Ok(Object::new_tuple(vec![])),
                "c_contiguous" | "f_contiguous" | "contiguous" => Ok(Object::Bool(true)),
                _ => {
                    if let Some(m) = self.lookup_method(obj, name) {
                        return Ok(Object::BoundMethod(Rc::new(BoundMethod {
                            receiver: obj.clone(),
                            function: m,
                        })));
                    }
                    Err(attribute_error(format!(
                        "'memoryview' object has no attribute '{name}'"
                    )))
                }
            },
            Object::Function(f) => {
                if let Some(v) = f.attrs.borrow().get(&DictKey(Object::from_str(name))) {
                    return Ok(v.clone());
                }
                match name {
                    "__name__" => return Ok(Object::from_str(&f.name)),
                    "__qualname__" => return Ok(Object::from_str(&f.name)),
                    "__doc__" => {
                        // CPython convention: the first statement of
                        // the function body, if it is a string
                        // literal, is stored as ``co_consts[0]`` and
                        // surfaces as ``__doc__``.
                        return Ok(crate::builtins::code_docstring(&f.code).unwrap_or(Object::None));
                    }
                    "__module__" => {
                        // Fall back to globals['__name__'] if the function's
                        // attrs dict didn't already pin a value (e.g. for
                        // synthesised functions in tests / REPL).
                        if let Some(name_obj) = f
                            .globals
                            .borrow()
                            .get(&DictKey(Object::from_static("__name__")))
                            .cloned()
                        {
                            return Ok(name_obj);
                        }
                        return Ok(Object::None);
                    }
                    "__dict__" => return Ok(Object::Dict(f.attrs.clone())),
                    "__code__" => return Ok(Object::Code(f.code.clone())),
                    "__globals__" => return Ok(Object::Dict(f.globals.clone())),
                    "__defaults__" => {
                        if f.defaults.is_empty() {
                            return Ok(Object::None);
                        }
                        return Ok(Object::new_tuple(f.defaults.clone()));
                    }
                    "__kwdefaults__" => {
                        if f.kw_defaults.is_empty() {
                            return Ok(Object::None);
                        }
                        let mut d = DictData::new();
                        for (k, v) in &f.kw_defaults {
                            d.insert(DictKey(Object::from_str(k)), v.clone());
                        }
                        return Ok(Object::Dict(Rc::new(RefCell::new(d))));
                    }
                    "__closure__" => {
                        if f.closure.is_empty() {
                            return Ok(Object::None);
                        }
                        return Ok(Object::new_tuple(f.closure.clone()));
                    }
                    "__annotations__" => {
                        // CPython auto-creates an empty dict if the
                        // function was defined without annotations,
                        // so reads of ``__annotations__`` never raise
                        // ``AttributeError``. Stash it on the
                        // function's attrs so subsequent writes mutate
                        // the same dict.
                        let key = DictKey(Object::from_static("__annotations__"));
                        if let Some(v) = f.attrs.borrow().get(&key) {
                            return Ok(v.clone());
                        }
                        let d = Object::Dict(Rc::new(RefCell::new(DictData::new())));
                        f.attrs.borrow_mut().insert(key, d.clone());
                        return Ok(d);
                    }
                    _ => {}
                }
                Err(attribute_error(format!(
                    "'function' object has no attribute '{}'",
                    name
                )))
            }
            Object::Code(c) => {
                if let Some(v) = crate::builtins::code_synthetic_attr(c, name) {
                    return Ok(v);
                }
                Err(attribute_error(format!(
                    "'code' object has no attribute '{}'",
                    name
                )))
            }
            Object::Frame(fr) => match name {
                "f_code" => Ok(Object::Code(fr.code.clone())),
                "f_globals" => Ok(Object::Dict(fr.globals.clone())),
                "f_builtins" => Ok(Object::Dict(fr.builtins.clone())),
                "f_locals" => Ok(fr.locals()),
                "f_lineno" => Ok(Object::Int(i64::from(fr.current_lineno()))),
                "f_lasti" => Ok(Object::Int(i64::from(fr.lasti.get()))),
                "f_back" => match fr.back.borrow().as_ref() {
                    Some(parent) => Ok(Object::Frame(parent.clone())),
                    None => Ok(Object::None),
                },
                "f_trace" => Ok(fr.trace.borrow().clone()),
                "f_trace_lines" => Ok(Object::Bool(true)),
                "f_trace_opcodes" => Ok(Object::Bool(false)),
                _ => Err(attribute_error(format!(
                    "'frame' object has no attribute '{}'",
                    name
                ))),
            },
            Object::Traceback(tb) => match name {
                "tb_frame" => Ok(Object::Frame(tb.frame.clone())),
                "tb_lineno" => Ok(Object::Int(i64::from(tb.lineno))),
                "tb_lasti" => Ok(Object::Int(i64::from(tb.lasti))),
                "tb_next" => match tb.next.borrow().as_ref() {
                    Some(n) => Ok(Object::Traceback(n.clone())),
                    None => Ok(Object::None),
                },
                _ => Err(attribute_error(format!(
                    "'traceback' object has no attribute '{}'",
                    name
                ))),
            },
            Object::Builtin(b) => match name {
                "__name__" | "__qualname__" => Ok(Object::from_static(b.name)),
                "__module__" => Ok(Object::from_static("builtins")),
                "__doc__" => Ok(Object::None),
                "__self__" => Ok(Object::None),
                _ => Err(attribute_error(format!(
                    "'builtin_function_or_method' object has no attribute '{}'",
                    name
                ))),
            },
            Object::BoundMethod(bm) => match name {
                "__func__" => Ok(bm.function.clone()),
                "__self__" => Ok(bm.receiver.clone()),
                "__name__" => match &bm.function {
                    Object::Function(f) => Ok(Object::from_str(f.name.clone())),
                    Object::Builtin(b) => Ok(Object::from_static(b.name)),
                    _ => Ok(Object::from_static("?")),
                },
                "__doc__" => Ok(Object::None),
                "__code__" => match &bm.function {
                    Object::Function(f) => Ok(Object::Code(f.code.clone())),
                    _ => Err(attribute_error(format!(
                        "'method' object has no attribute '{}'",
                        name
                    ))),
                },
                _ => Err(attribute_error(format!(
                    "'method' object has no attribute '{}'",
                    name
                ))),
            },
            _ => {
                // Numeric data attributes — exposed by the
                // ``numbers`` protocol (``real``, ``imag``,
                // ``numerator``, ``denominator``). Returned as
                // plain values, not bound methods.
                if let Some(v) = numeric_data_attr(obj, name) {
                    return Ok(v);
                }
                if let Some(method) = self.lookup_method(obj, name) {
                    return Ok(Object::BoundMethod(Rc::new(BoundMethod {
                        receiver: obj.clone(),
                        function: method,
                    })));
                }
                Err(attribute_error(format!(
                    "'{}' object has no attribute '{}'",
                    obj.type_name(),
                    name
                )))
            }
        }
    }

    /// Attribute access on a user-defined class instance. Implements
    /// the CPython data/non-data descriptor protocol:
    ///
    ///   1. Look up `name` in the type's MRO → `meta_attr`.
    ///   2. If `meta_attr` is a *data* descriptor (`property`,
    ///      slot descriptor, or an instance whose class defines
    ///      `__set__`/`__delete__`), call its `__get__` and return.
    ///   3. Otherwise, return the instance dict entry if present.
    ///   4. Otherwise, if `meta_attr` exists, run the (possibly
    ///      non-data) descriptor `__get__` — that's also where bare
    ///      functions become bound methods.
    ///   5. Otherwise, dispatch the class's `__getattr__` if any.
    ///   6. Otherwise, raise `AttributeError`.
    fn load_attr_instance(
        &mut self,
        inst: &Rc<PyInstance>,
        instance_obj: &Object,
        name: &str,
    ) -> Result<Object, RuntimeError> {
        // Super proxies stash the real receiver under `__self__`.
        // Re-bind methods looked up via the proxy so they run
        // against the right `self` AND against the original class
        // (not the proxy) for classmethod binding. CPython's
        // `super.__getattribute__` passes `su.__obj_type__` — the
        // class that originally triggered super — as the `owner`
        // argument to the descriptor protocol; we mirror that here.
        let super_receiver = inst
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("__self__")))
            .cloned();
        if name != "__self__" {
            if let Some(receiver) = super_receiver {
                if let Some(v) = inst.class.lookup(name) {
                    let owner = match &receiver {
                        Object::Type(t) => Object::Type(t.clone()),
                        Object::Instance(i) => Object::Type(i.class.clone()),
                        _ => Object::Type(inst.class.clone()),
                    };
                    return self.descriptor_get(&v, &receiver, &owner);
                }
                return Err(attribute_error(format!(
                    "'super' object has no attribute '{}'",
                    name
                )));
            }
        }

        let meta_attr = inst.class.lookup(name);
        let owner = Object::Type(inst.class.clone());

        // (1) Data descriptor on class wins over instance dict.
        if let Some(ref attr) = meta_attr {
            if self.is_data_descriptor(attr) {
                return self.descriptor_get(attr, instance_obj, &owner);
            }
        }

        // (2) Instance dict.
        if let Some(v) = inst.dict.borrow().get(&DictKey(Object::from_str(name))) {
            return Ok(v.clone());
        }

        // (3) Non-data descriptor / function on class.
        if let Some(attr) = meta_attr {
            return self.descriptor_get(&attr, instance_obj, &owner);
        }

        // (3b) Synthetic dunders served from the instance itself —
        // `__dict__` and `__class__` aren't normally stored on the
        // user's instance dict but Python code (e.g.
        // `functools.cached_property`) reaches for them anyway.
        match name {
            "__dict__" => return Ok(Object::Dict(inst.dict.clone())),
            "__class__" => return Ok(Object::Type(inst.class.clone())),
            _ => {}
        }

        // (4) __getattr__ fall-back.
        if let Some(getattr) = inst.class.lookup("__getattr__") {
            let bound = Object::BoundMethod(Rc::new(BoundMethod {
                receiver: instance_obj.clone(),
                function: getattr,
            }));
            return self.call(
                &bound,
                &[Object::from_str(name)],
                &[],
                &self.builtins.clone(),
            );
        }

        Err(attribute_error(format!(
            "'{}' object has no attribute '{}'",
            inst.class.name, name
        )))
    }

    /// Attribute access on a class. Mirrors CPython's
    /// `type.__getattribute__`: the metaclass MRO contributes data
    /// descriptors that beat class-level attrs, the class itself is
    /// then consulted with the *unbound* descriptor protocol (so
    /// classmethods bind to the class, plain functions stay
    /// unbound).
    fn load_attr_type(&mut self, ty: &Rc<TypeObject>, name: &str) -> Result<Object, RuntimeError> {
        let meta = ty.metaclass_or_type();
        let owner = Object::Type(ty.clone());
        let self_as_obj = Object::Type(ty.clone());

        // (1) Metaclass-level data descriptor wins.
        let meta_attr = meta.lookup(name);
        if let Some(ref attr) = meta_attr {
            if self.is_data_descriptor(attr) {
                return self.descriptor_get(attr, &self_as_obj, &Object::Type(meta.clone()));
            }
        }

        // (2) Look up the name in `ty` itself (and its MRO).
        if let Some(attr) = ty.lookup(name) {
            // Apply the descriptor protocol with no instance: classmethods
            // bind to the class, plain functions stay as functions,
            // staticmethods unwrap, properties remain themselves.
            return self.descriptor_get(&attr, &Object::None, &owner);
        }

        // (3) Fall-through to (possibly non-data) metaclass attribute.
        if let Some(attr) = meta_attr {
            return self.descriptor_get(&attr, &self_as_obj, &Object::Type(meta.clone()));
        }

        // (4) Synthetic attributes.
        match name {
            "__name__" => return Ok(Object::from_str(&ty.name)),
            "__qualname__" => return Ok(Object::from_str(&ty.name)),
            "__bases__" => {
                let bases = ty.bases.iter().map(|b| Object::Type(b.clone())).collect();
                return Ok(Object::new_tuple(bases));
            }
            "__mro__" => {
                let mro = ty
                    .mro
                    .borrow()
                    .iter()
                    .map(|b| Object::Type(b.clone()))
                    .collect();
                return Ok(Object::new_tuple(mro));
            }
            "__class__" => return Ok(Object::Type(meta)),
            "__dict__" => return Ok(Object::Dict(ty.dict.clone())),
            _ => {}
        }

        // (5) Built-in class methods not stored in ``ty.dict``: most
        // CPython classmethods/staticmethods (``str.maketrans``,
        // ``bytes.fromhex``, ``int.from_bytes``, ``dict.fromkeys``,
        // ``float.fromhex``, ``frozenset()``-like constructors) are
        // synthesized on demand. We expose them as plain builtins
        // bound to no instance.
        if let Some(b) = crate::builtins::builtin_classmethod(&ty.name, name) {
            return Ok(b);
        }

        Err(attribute_error(format!(
            "type object '{}' has no attribute '{}'",
            ty.name, name
        )))
    }

    /// Does `attr`, looked up on a class, behave as a *data*
    /// descriptor? Data descriptors win against instance `__dict__`
    /// during attribute access.
    fn is_data_descriptor(&self, attr: &Object) -> bool {
        match attr {
            Object::Property(_) | Object::SlotDescriptor(_) => true,
            Object::Instance(inst) => {
                inst.class.lookup("__set__").is_some() || inst.class.lookup("__delete__").is_some()
            }
            _ => false,
        }
    }

    /// Run the descriptor protocol against `attr` (already resolved
    /// from a class MRO). `instance` is `Object::None` when accessed
    /// directly on the class (e.g. `Foo.bar`).
    fn descriptor_get(
        &mut self,
        attr: &Object,
        instance: &Object,
        owner: &Object,
    ) -> Result<Object, RuntimeError> {
        match attr {
            Object::Property(prop) => {
                if matches!(instance, Object::None) {
                    return Ok(attr.clone());
                }
                if matches!(prop.fget, Object::None) {
                    return Err(attribute_error("unreadable attribute"));
                }
                self.call(
                    &prop.fget,
                    std::slice::from_ref(instance),
                    &[],
                    &self.builtins.clone(),
                )
            }
            Object::StaticMethod(inner) => Ok((**inner).clone()),
            Object::ClassMethod(inner) => Ok(Object::BoundMethod(Rc::new(BoundMethod {
                receiver: owner.clone(),
                function: (**inner).clone(),
            }))),
            Object::SlotDescriptor(slot) => match instance {
                Object::None => Ok(attr.clone()),
                Object::Instance(inst) => {
                    let key = DictKey(Object::from_str(&slot.name));
                    match inst.dict.borrow().get(&key) {
                        Some(v) => Ok(v.clone()),
                        None => Err(attribute_error(format!(
                            "'{}' object has no attribute '{}'",
                            inst.class.name, slot.name
                        ))),
                    }
                }
                _ => Err(type_error("slot descriptor requires an instance")),
            },
            Object::Function(_) | Object::Builtin(_) => {
                if matches!(instance, Object::None) {
                    Ok(attr.clone())
                } else {
                    Ok(Object::BoundMethod(Rc::new(BoundMethod {
                        receiver: instance.clone(),
                        function: attr.clone(),
                    })))
                }
            }
            Object::Instance(inner_inst) => {
                // User-defined descriptor: invoke its `__get__` if
                // present, otherwise pass the descriptor through.
                if let Some(get_method) = inner_inst.class.lookup("__get__") {
                    let bound = Object::BoundMethod(Rc::new(BoundMethod {
                        receiver: attr.clone(),
                        function: get_method,
                    }));
                    return self.call(
                        &bound,
                        &[instance.clone(), owner.clone()],
                        &[],
                        &self.builtins.clone(),
                    );
                }
                Ok(attr.clone())
            }
            _ => Ok(attr.clone()),
        }
    }

    fn maybe_bind(&self, receiver: &Object, attr: Object) -> Object {
        match &attr {
            Object::Function(_) | Object::Builtin(_) => Object::BoundMethod(Rc::new(BoundMethod {
                receiver: receiver.clone(),
                function: attr,
            })),
            Object::ClassMethod(inner) => Object::BoundMethod(Rc::new(BoundMethod {
                receiver: match receiver {
                    Object::Instance(inst) => Object::Type(inst.class.clone()),
                    other => other.clone(),
                },
                function: (**inner).clone(),
            })),
            Object::StaticMethod(inner) => (**inner).clone(),
            _ => attr,
        }
    }

    fn lookup_method(&self, obj: &Object, name: &str) -> Option<Object> {
        builtins::lookup_method(obj, name)
    }

    /// `print(*args, sep=' ', end='\n', file=...)`. We honour `sep`
    /// and `end` from kwargs; `file` is ignored (always our stdout).
    fn do_print(
        &mut self,
        args: &[Object],
        kwargs: &[(String, Object)],
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let mut sep = String::from(" ");
        let mut end = String::from("\n");
        for (k, v) in kwargs {
            match k.as_str() {
                "sep" => sep = v.to_str(),
                "end" => end = v.to_str(),
                "file" | "flush" => {}
                other => {
                    return Err(type_error(format!(
                        "'{other}' is an invalid keyword argument for print()"
                    )))
                }
            }
        }
        let mut sink = self.stdout.borrow_mut();
        for (i, a) in args.iter().enumerate() {
            if i > 0 {
                let _ = write!(sink, "{sep}");
            }
            drop(sink);
            let s = self.stringify(a, globals)?;
            sink = self.stdout.borrow_mut();
            let _ = write!(sink, "{s}");
        }
        let _ = write!(sink, "{end}");
        Ok(Object::None)
    }

    fn do_str_call(
        &mut self,
        v: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        Ok(Object::from_str(self.stringify(v, globals)?))
    }

    /// VM-aware variant of [`str_format_impl`] that dispatches
    /// `__str__` / `__repr__` for conversions on instances, so
    /// `"{!r}".format(obj)` mirrors `repr(obj)` for user types.
    fn do_str_format(
        &mut self,
        template: &str,
        positional: &[Object],
        keyword: &[(String, Object)],
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<String, RuntimeError> {
        // Pre-stringify any Instance arg by converting through the
        // VM so user dunders run. We don't know yet which conversion
        // each field will pick, so we materialise both `!s` and `!r`
        // upfront when the arg is an Instance.
        let mut positional_resolved: Vec<Object> = Vec::with_capacity(positional.len());
        let mut keyword_resolved: Vec<(String, Object)> = Vec::with_capacity(keyword.len());
        // We let `str_format_impl` do the normal field resolution
        // and conversion; the only thing we need to fix is that when
        // the value is an Instance and there is an `!s` / `!r`
        // conversion, the plain `to_str()` / `repr()` in
        // `render_format_field` won't dispatch dunders. We do a
        // pre-pass and replace bare Instances with proxy strings
        // when no conversion is requested.
        //
        // Conversion dispatch: we recognise `{x!s}` / `{x!r}` by
        // post-processing — we leave Instances alone for the
        // straight `{x}` path (the default conversion falls back to
        // `value.to_str()` which CPython's `format` actually also
        // does — it calls `__format__`, but lacking that here we
        // emit a `<X object>` placeholder).
        for arg in positional {
            positional_resolved.push(arg.clone());
        }
        for (k, v) in keyword {
            keyword_resolved.push((k.clone(), v.clone()));
        }
        // The straightforward fix is to override conversion: parse
        // each field's `!s` / `!r` and substitute the user-method
        // result back into the field as a Str literal before
        // delegating to `str_format_impl`. We do that with a
        // pre-pass below.
        let preprocessed =
            self.preprocess_str_format(template, &positional_resolved, &keyword_resolved, globals)?;
        str_format_impl(&preprocessed, &positional_resolved, &keyword_resolved)
    }

    /// Walk every `{...}` field; when the conversion is `!s` or `!r`
    /// and the referenced value is an Instance, replace the field
    /// with a pre-rendered literal so the downstream formatter sees
    /// a string instead of the unconverted object.
    fn preprocess_str_format(
        &mut self,
        template: &str,
        positional: &[Object],
        keyword: &[(String, Object)],
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<String, RuntimeError> {
        let bytes = template.as_bytes();
        let mut out = String::with_capacity(template.len());
        let mut i = 0;
        let mut auto_idx = 0usize;
        while i < bytes.len() {
            let b = bytes[i];
            if b == b'{' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                    out.push_str("{{");
                    i += 2;
                    continue;
                }
                let (field, end) = scan_format_field(bytes, i + 1)?;
                i = end;
                let (name_part, conv, spec_part) = split_format_field(&field);
                let conv_char = conv;
                let mut tmp_idx = auto_idx;
                let resolved =
                    resolve_field_name(name_part, positional, keyword, &mut tmp_idx, None);
                let consumed_auto = name_part.is_empty();
                if matches!(conv_char, Some('s') | Some('r')) {
                    if let Ok(value) = resolved.as_ref() {
                        if matches!(value, Object::Instance(_)) {
                            let rendered = match conv_char {
                                Some('s') => self.stringify(value, globals)?,
                                Some('r') => self.repr_of(value, globals)?,
                                _ => unreachable!(),
                            };
                            let final_text = match spec_part {
                                Some(spec) => format_via_spec(&Object::from_str(rendered), spec)?,
                                None => rendered,
                            };
                            for ch in final_text.chars() {
                                if ch == '{' || ch == '}' {
                                    out.push(ch);
                                    out.push(ch);
                                } else {
                                    out.push(ch);
                                }
                            }
                            auto_idx = tmp_idx;
                            continue;
                        }
                    }
                } else if conv_char.is_none() {
                    // `{x}` with no explicit conversion: CPython calls
                    // `__format__(x, spec)`, which on instances falls
                    // back to `__str__`. We don't yet hop through
                    // `__format__`, but invoking `__str__` is the
                    // common-case match users expect.
                    if let Ok(value) = resolved.as_ref() {
                        if matches!(value, Object::Instance(_)) {
                            let s = self.stringify(value, globals)?;
                            let final_text = match spec_part {
                                Some(spec) => format_via_spec(&Object::from_str(s), spec)?,
                                None => s,
                            };
                            for ch in final_text.chars() {
                                if ch == '{' || ch == '}' {
                                    out.push(ch);
                                    out.push(ch);
                                } else {
                                    out.push(ch);
                                }
                            }
                            auto_idx = tmp_idx;
                            continue;
                        }
                    }
                }
                // Field unchanged. If we consumed an auto-index slot
                // and the field doesn't carry a name, rewrite it as a
                // positional `{N}` so the downstream formatter's
                // separate auto-index counter doesn't desync with ours.
                if consumed_auto {
                    let idx = auto_idx;
                    auto_idx = tmp_idx;
                    out.push('{');
                    out.push_str(&idx.to_string());
                    // Preserve trailers (everything after the empty name_part).
                    let after_base = &field[name_part.len()..];
                    out.push_str(after_base);
                    out.push('}');
                } else {
                    out.push('{');
                    out.push_str(&field);
                    out.push('}');
                }
            } else if b == b'}' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'}' {
                    out.push_str("}}");
                    i += 2;
                    continue;
                }
                return Err(value_error("Single '}' encountered in format string"));
            } else {
                let ch_len = utf8_seq_len(b);
                let end = (i + ch_len).min(bytes.len());
                out.push_str(&template[i..end]);
                i = end;
            }
        }
        Ok(out)
    }

    fn do_repr_call(
        &mut self,
        v: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        Ok(Object::from_str(self.repr_of(v, globals)?))
    }

    fn do_len_call(
        &mut self,
        v: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        if let Some(method) = instance_method(v, "__len__") {
            let r = self.call(&method, &[], &[], globals)?;
            return match r {
                Object::Int(i) => Ok(Object::Int(i)),
                other => Err(type_error(format!(
                    "'__len__' should return int, not '{}'",
                    other.type_name()
                ))),
            };
        }
        Ok(Object::Int(v.len()? as i64))
    }

    /// `int(x)` with a fallback to the user-defined `__int__`. Matches
    /// CPython's coercion rules well enough for the common cases —
    /// user classes that store an integer payload (enums, ipaddress,
    /// etc.) just work.
    fn do_int_call(
        &mut self,
        args: &[Object],
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        if args.is_empty() {
            return Ok(Object::Int(0));
        }
        match &args[0] {
            Object::Int(_)
            | Object::Long(_)
            | Object::Bool(_)
            | Object::Float(_)
            | Object::Str(_)
            | Object::Bytes(_)
            | Object::ByteArray(_) => builtins::b_int_compat(args),
            other => {
                if let Some(method) = instance_method(other, "__int__") {
                    let r = self.call(&method, &[], &[], globals)?;
                    return match r {
                        Object::Int(i) => Ok(Object::Int(i)),
                        Object::Bool(b) => Ok(Object::Int(i64::from(b))),
                        other => Err(type_error(format!(
                            "'__int__' should return int, not '{}'",
                            other.type_name()
                        ))),
                    };
                }
                Err(type_error(format!(
                    "int() argument must be a string or a real number, not '{}'",
                    other.type_name()
                )))
            }
        }
    }

    fn do_float_call(
        &mut self,
        args: &[Object],
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        if args.is_empty() {
            return Ok(Object::Float(0.0));
        }
        match &args[0] {
            Object::Int(_)
            | Object::Long(_)
            | Object::Bool(_)
            | Object::Float(_)
            | Object::Str(_)
            | Object::Bytes(_)
            | Object::ByteArray(_) => builtins::b_float_compat(args),
            other => {
                if let Some(method) = instance_method(other, "__float__") {
                    let r = self.call(&method, &[], &[], globals)?;
                    return match r {
                        Object::Float(f) => Ok(Object::Float(f)),
                        Object::Int(i) => Ok(Object::Float(i as f64)),
                        other => Err(type_error(format!(
                            "'__float__' should return float, not '{}'",
                            other.type_name()
                        ))),
                    };
                }
                Err(type_error(format!(
                    "float() argument must be a string or a real number, not '{}'",
                    other.type_name()
                )))
            }
        }
    }

    /// `next(it[, default])` — drives an iterator. Generators need
    /// the interpreter on the call path, which is why this lives here
    /// rather than in `builtins.rs`.
    fn do_next_call(
        &mut self,
        args: &[Object],
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let it = &args[0];
        let default = args.get(1).cloned();
        // Drive the iterator directly so we can surface a
        // `StopIteration(value)` raised by a generator's `return`
        // statement instead of losing the value to `iter_next`'s
        // exhausted-or-not boolean.
        match it {
            Object::Generator(g) => match self.generator_send(g, Object::None) {
                Ok(v) => Ok(v),
                Err(RuntimeError::PyException(exc)) if exc.type_name() == "StopIteration" => {
                    if let Some(d) = default {
                        Ok(d)
                    } else {
                        Err(RuntimeError::PyException(exc))
                    }
                }
                Err(e) => Err(e),
            },
            _ => match self.iter_next(it, globals) {
                Ok(Some(v)) => Ok(v),
                Ok(None) => default.ok_or_else(stop_iteration),
                Err(e) => Err(e),
            },
        }
    }

    fn do_iter_call(
        &mut self,
        v: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        self.make_iter(v, globals)
    }

    /// `iter(callable, sentinel)` — eagerly drains the callable in
    /// a tight loop, building a list. Simpler than synthesising a
    /// generator and matches the documented CPython semantics for
    /// the common usage pattern (read-until-sentinel). The
    /// resulting list iterator behaves identically for all
    /// finite-sentinel cases; infinite sequences with this form
    /// would also hang in CPython.
    fn do_iter_callable_sentinel(
        &mut self,
        args: &[Object],
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let callable = args[0].clone();
        let sentinel = args[1].clone();
        let mut out: Vec<Object> = Vec::new();
        // CPython caps the number of iterations at a very large
        // value to keep accidental infinite loops bounded; we use
        // i64::MAX iterations as the safety limit but in practice
        // expect the sentinel to fire much sooner.
        for _ in 0_i64..i64::MAX {
            let v = self.call(&callable, &[], &[], globals)?;
            if self.dispatch_compare_op(&v, &sentinel, CompareKind::Eq, globals)? {
                break;
            }
            out.push(v);
        }
        let list = Object::new_list(out);
        self.make_iter(&list, globals)
    }

    fn do_list_or_tuple_call(
        &mut self,
        name: &str,
        v: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let collected = self.collect_iterable(v, globals)?;
        if name == "list" {
            Ok(Object::new_list(collected))
        } else {
            Ok(Object::new_tuple(collected))
        }
    }

    /// CPython's `dict(obj)` checks for the mapping protocol (`keys()` +
    /// `__getitem__`) before falling back to iter-of-pairs. We do the
    /// same for user-defined instances: if the instance exposes
    /// `keys()`, call it and pull each value via subscript.
    fn try_dict_from_mapping(
        &mut self,
        v: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Option<Object>, RuntimeError> {
        let Object::Instance(inst) = v else {
            return Ok(None);
        };
        // Prefer the instance's own `keys` (rare), then walk the MRO.
        // `inst.class.lookup` already handles inheritance, which is
        // how `_MappingMixin` subclasses (defaultdict, Counter, …)
        // get their mapping API.
        let keys_attr = inst
            .dict
            .borrow()
            .get(&DictKey(Object::from_str("keys")))
            .cloned()
            .or_else(|| inst.class.lookup("keys"));
        let Some(keys_fn) = keys_attr else {
            return Ok(None);
        };
        let bound = self.maybe_bind(v, keys_fn);
        let keys = self.call(&bound, &[], &[], globals)?;
        let mut d = DictData::new();
        let it = self.make_iter(&keys, globals)?;
        while let Some(k) = self.iter_next(&it, globals)? {
            // Use `__getitem__` if it's defined (the typical case for
            // user mappings); fall back to native subscript for the
            // few built-in iterables that might land here.
            let val = if let Some(getitem) = instance_method(v, "__getitem__") {
                self.call(&getitem, std::slice::from_ref(&k), &[], globals)?
            } else {
                self.binary_subscr(v, &k)?
            };
            d.insert(DictKey(k), val);
        }
        Ok(Some(Object::Dict(Rc::new(RefCell::new(d)))))
    }

    fn collect_iterable(
        &mut self,
        v: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Vec<Object>, RuntimeError> {
        match v {
            Object::List(items) => Ok(items.borrow().clone()),
            Object::Tuple(items) => Ok(items.to_vec()),
            Object::Set(s) => Ok(s.borrow().iter().map(|k| k.0.clone()).collect()),
            Object::FrozenSet(s) => Ok(s.iter().map(|k| k.0.clone()).collect()),
            Object::Generator(_) | Object::Instance(_) => {
                let it = self.make_iter(v, globals)?;
                let mut out = Vec::new();
                while let Some(x) = self.iter_next(&it, globals)? {
                    out.push(x);
                }
                Ok(out)
            }
            other => {
                // Fall through to the existing builtin so we don't
                // re-implement range/dict/str iteration here.
                let it = self.make_iter(other, globals)?;
                let mut out = Vec::new();
                while let Some(x) = self.iter_next(&it, globals)? {
                    out.push(x);
                }
                Ok(out)
            }
        }
    }

    fn do_sum_call(
        &mut self,
        args: &[Object],
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        if args.is_empty() {
            return Err(type_error("sum() expects at least one argument"));
        }
        let mut acc = args.get(1).cloned().unwrap_or(Object::Int(0));
        let items = self.collect_iterable(&args[0], globals)?;
        for x in items {
            acc = binary_op(&acc, &x, BinOpKind::Add)?;
        }
        Ok(acc)
    }

    fn do_min_max_call(
        &mut self,
        name: &str,
        args: &[Object],
        kwargs: &[(String, Object)],
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let want_max = name == "max";
        let default = kwargs
            .iter()
            .find_map(|(k, v)| (k == "default").then(|| v.clone()));
        let key_fn = kwargs
            .iter()
            .find_map(|(k, v)| (k == "key").then(|| v.clone()));
        let items: Vec<Object> = if args.len() == 1 {
            self.collect_iterable(&args[0], globals)?
        } else {
            args.to_vec()
        };
        if items.is_empty() {
            return default
                .ok_or_else(|| value_error(format!("{name}() arg is an empty sequence")));
        }
        let key_of = |slf: &mut Self, item: &Object| -> Result<Object, RuntimeError> {
            if let Some(f) = &key_fn {
                slf.call(f, std::slice::from_ref(item), &[], globals)
            } else {
                Ok(item.clone())
            }
        };
        let mut best_value = items[0].clone();
        let mut best_key = key_of(self, &items[0])?;
        for item in &items[1..] {
            let candidate_key = key_of(self, item)?;
            let order = candidate_key.cmp(&best_key)?;
            let take = if want_max {
                matches!(order, std::cmp::Ordering::Greater)
            } else {
                matches!(order, std::cmp::Ordering::Less)
            };
            if take {
                best_value = item.clone();
                best_key = candidate_key;
            }
        }
        Ok(best_value)
    }

    fn do_any_all_call(
        &mut self,
        name: &str,
        args: &[Object],
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let items = self.collect_iterable(&args[0], globals)?;
        let want_any = name == "any";
        for x in items {
            if x.is_truthy() {
                if want_any {
                    return Ok(Object::Bool(true));
                }
            } else if !want_any {
                return Ok(Object::Bool(false));
            }
        }
        Ok(Object::Bool(!want_any))
    }

    /// `isinstance(obj, classinfo)` — honours `__instancecheck__` on
    /// the *metaclass* of any class in `classinfo`, falling back to
    /// the plain MRO walk otherwise. ABCMeta uses this to register
    /// virtual subclasses.
    fn do_isinstance_call(
        &mut self,
        obj: &Object,
        classinfo: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        // Tuple of types: short-circuit on first match.
        if let Object::Tuple(items) = classinfo {
            for it in items.iter() {
                if self.do_isinstance_call(obj, it, globals)?.is_truthy() {
                    return Ok(Object::Bool(true));
                }
            }
            return Ok(Object::Bool(false));
        }
        if let Object::Type(cls) = classinfo {
            let meta = cls.metaclass_or_type();
            // Only dispatch to a *user-defined* __instancecheck__.
            // The built-in `type.__instancecheck__` is already what
            // we'd compute by default, so skip it to avoid recursion.
            if !Rc::ptr_eq(&meta, &builtin_types().type_) {
                if let Some(hook) = meta.lookup("__instancecheck__") {
                    let bound = Object::BoundMethod(Rc::new(BoundMethod {
                        receiver: Object::Type(cls.clone()),
                        function: hook,
                    }));
                    let res = self.call(&bound, std::slice::from_ref(obj), &[], globals)?;
                    return Ok(Object::Bool(res.is_truthy()));
                }
            }
        }
        // Default path: delegate to the builtin.
        Ok(Object::Bool(builtins::matches_classinfo(obj, classinfo)?))
    }

    /// `issubclass(cls, classinfo)` — same protocol as
    /// [`do_isinstance_call`] but for class membership.
    fn do_issubclass_call(
        &mut self,
        cls: &Object,
        classinfo: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        if let Object::Tuple(items) = classinfo {
            for it in items.iter() {
                if self.do_issubclass_call(cls, it, globals)?.is_truthy() {
                    return Ok(Object::Bool(true));
                }
            }
            return Ok(Object::Bool(false));
        }
        if let Object::Type(info_cls) = classinfo {
            let meta = info_cls.metaclass_or_type();
            if !Rc::ptr_eq(&meta, &builtin_types().type_) {
                if let Some(hook) = meta.lookup("__subclasscheck__") {
                    let bound = Object::BoundMethod(Rc::new(BoundMethod {
                        receiver: Object::Type(info_cls.clone()),
                        function: hook,
                    }));
                    let res = self.call(&bound, std::slice::from_ref(cls), &[], globals)?;
                    return Ok(Object::Bool(res.is_truthy()));
                }
            }
        }
        let cls_inner = match cls {
            Object::Type(t) => t.clone(),
            _ => return Err(type_error("issubclass() arg 1 must be a class")),
        };
        Ok(Object::Bool(builtins::class_matches_classinfo(
            &cls_inner, classinfo,
        )?))
    }

    /// `hash(obj)` — dispatch through the instance's `__hash__` if
    /// defined, otherwise fall back to the structural hash. We also
    /// reject objects whose class has `__hash__ = None` (CPython's
    /// "unhashable" marker, used e.g. by `dataclass(eq=True)` when
    /// frozen is False).
    fn do_hash_call(
        &mut self,
        obj: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        if let Object::Instance(inst) = obj {
            match inst.class.lookup("__hash__") {
                Some(Object::None) => {
                    return Err(type_error(format!(
                        "unhashable type: '{}'",
                        inst.class.name
                    )));
                }
                Some(method @ (Object::Function(_) | Object::BoundMethod(_))) => {
                    let bound = Object::BoundMethod(Rc::new(BoundMethod {
                        receiver: obj.clone(),
                        function: method,
                    }));
                    return self.call(&bound, &[], &[], globals);
                }
                _ => {}
            }
        }
        builtins::hash_object(obj)
    }

    fn do_sorted_call(
        &mut self,
        args: &[Object],
        kwargs: &[(String, Object)],
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let mut items = self.collect_iterable(&args[0], globals)?;
        let reverse = kwargs
            .iter()
            .find_map(|(k, v)| (k == "reverse").then(|| v.is_truthy()))
            .unwrap_or(false);
        let key_fn = kwargs
            .iter()
            .find_map(|(k, v)| (k == "key").then(|| v.clone()));
        self.sort_with_key(&mut items, key_fn.as_ref(), reverse, globals)?;
        Ok(Object::new_list(items))
    }

    /// VM-routed dispatch for ``re.sub(pattern, repl_callable, text,
    /// count=0, flags=0)`` where ``repl`` is a callable. We
    /// collect the spans up-front (no VM reentrancy mid-iteration)
    /// and then call ``repl(match)`` once per match.
    fn do_re_sub_callable(
        &mut self,
        args: &[Object],
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        use crate::stdlib::re as remod;
        let pat_obj = args
            .first()
            .ok_or_else(|| type_error("re.sub: missing pattern"))?;
        let repl = args
            .get(1)
            .ok_or_else(|| type_error("re.sub: missing repl"))?
            .clone();
        let text = match args.get(2) {
            Some(Object::Str(s)) => s.to_string(),
            _ => return Err(type_error("re.sub: expected str text")),
        };
        let count = match args.get(3) {
            Some(Object::Int(i)) => *i,
            _ => 0,
        };
        let (pat, default_flags) = remod::extract_pattern_pub(pat_obj)?;
        let flags = match args.get(4) {
            Some(Object::Int(i)) => *i,
            _ => default_flags,
        };
        let matches = remod::collect_all_matches(&pat, flags, &text)?;
        let mut out = String::new();
        let mut last_end = 0usize;
        for (idx, (s, e, groups)) in matches.iter().enumerate() {
            if count > 0 && (idx as i64) >= count {
                break;
            }
            out.push_str(&text[last_end..*s]);
            let m_obj = remod::build_match_object(&pat, &text, groups, *s, *e);
            let ret = self.call_object(repl.clone(), &[m_obj], &[])?;
            match ret {
                Object::Str(rs) => out.push_str(&rs),
                _ => return Err(type_error("re.sub callable must return str")),
            }
            last_end = *e;
        }
        out.push_str(&text[last_end..]);
        let _ = globals;
        Ok(Object::from_str(out))
    }

    fn do_list_sort_call(
        &mut self,
        args: &[Object],
        kwargs: &[(String, Object)],
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let list = match args.first() {
            Some(Object::List(l)) => l.clone(),
            _ => return Err(type_error("list.sort expects a list receiver")),
        };
        let reverse = kwargs
            .iter()
            .find_map(|(k, v)| (k == "reverse").then(|| v.is_truthy()))
            .unwrap_or(false);
        let key_fn = kwargs
            .iter()
            .find_map(|(k, v)| (k == "key").then(|| v.clone()));
        let mut items = list.borrow().clone();
        self.sort_with_key(&mut items, key_fn.as_ref(), reverse, globals)?;
        *list.borrow_mut() = items;
        Ok(Object::None)
    }

    /// Stable sort over `items`. With `key`, every element is mapped
    /// through it once and the results are sorted alongside the
    /// originals (decorate-sort-undecorate). Errors from the key
    /// function propagate.
    fn sort_with_key(
        &mut self,
        items: &mut Vec<Object>,
        key_fn: Option<&Object>,
        reverse: bool,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<(), RuntimeError> {
        if let Some(f) = key_fn {
            let mut decorated: Vec<(Object, Object)> = Vec::with_capacity(items.len());
            for item in items.iter() {
                let k = self.call(f, std::slice::from_ref(item), &[], globals)?;
                decorated.push((k, item.clone()));
            }
            decorated.sort_by(|a, b| a.0.cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            if reverse {
                decorated.reverse();
            }
            *items = decorated.into_iter().map(|(_, v)| v).collect();
        } else {
            items.sort_by(|a, b| a.cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            if reverse {
                items.reverse();
            }
        }
        Ok(())
    }

    /// Run `__str__` on instances, falling back to `__repr__` then
    /// the default. Built-in types use their existing `to_str`.
    fn stringify(
        &mut self,
        v: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<String, RuntimeError> {
        if let Object::Instance(_) = v {
            if let Some(method) = instance_method(v, "__str__") {
                let r = self.call(&method, &[], &[], globals)?;
                return Ok(r.to_str());
            }
            return self.repr_of(v, globals);
        }
        Ok(v.to_str())
    }

    fn repr_of(
        &mut self,
        v: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<String, RuntimeError> {
        if let Object::Instance(_) = v {
            if let Some(method) = instance_method(v, "__repr__") {
                let r = self.call(&method, &[], &[], globals)?;
                return Ok(r.to_str());
            }
        }
        Ok(v.repr())
    }

    /// Either build a native iterator (for built-ins) or call
    /// `__iter__` and return whatever the user method produced.
    fn make_iter(
        &mut self,
        v: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        match v {
            Object::Generator(_) | Object::Iter(_) => Ok(v.clone()),
            Object::Instance(_) => {
                if let Some(method) = instance_method(v, "__iter__") {
                    return self.call(&method, &[], &[], globals);
                }
                Err(type_error(format!(
                    "'{}' object is not iterable",
                    v.type_name_owned()
                )))
            }
            Object::Type(ty) => {
                // Iterating a class consults the metaclass for
                // `__iter__` — that's how `list(MyEnum)` works.
                let meta = ty.metaclass_or_type();
                if let Some(method) = meta.lookup("__iter__") {
                    let bound = Object::BoundMethod(Rc::new(BoundMethod {
                        receiver: Object::Type(ty.clone()),
                        function: method,
                    }));
                    return self.call(&bound, &[], &[], globals);
                }
                Err(type_error("'type' object is not iterable"))
            }
            _ => {
                let it = v.make_iter()?;
                Ok(Object::Iter(Rc::new(RefCell::new(it))))
            }
        }
    }

    /// Drive an awaitable into its underlying iterator (PEP 492 /
    /// RFC 0016). A coroutine is itself awaitable; an async generator
    /// is not (it must be consumed via `async for`). Any other object
    /// is consulted via `__await__()`.
    fn get_awaitable(&mut self, value: Object) -> Result<Object, RuntimeError> {
        match &value {
            // An async generator that surfaced through `__anext__` is
            // already drivable via SEND; treat it as its own
            // awaitable so the surrounding await-dance can run.
            Object::Coroutine(_) | Object::Generator(_) | Object::AsyncGenerator(_) => Ok(value),
            Object::Instance(_) => {
                if let Some(method) = instance_method(&value, "__await__") {
                    let it = self.call(&method, &[], &[], &fallback_globals())?;
                    return Ok(it);
                }
                Err(type_error(format!(
                    "object {} can't be used in 'await' expression",
                    value.type_name_owned()
                )))
            }
            _ => Err(type_error(format!(
                "object {} can't be used in 'await' expression",
                value.type_name_owned()
            ))),
        }
    }

    /// `__aiter__` dispatch — `aiter()`. Async generators are
    /// directly iterable; other objects must implement `__aiter__`.
    fn get_aiter(
        &mut self,
        value: Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        match &value {
            Object::AsyncGenerator(_) => Ok(value),
            Object::Instance(_) => {
                if let Some(method) = instance_method(&value, "__aiter__") {
                    return self.call(&method, &[], &[], globals);
                }
                Err(type_error(format!(
                    "'{}' object is not async-iterable",
                    value.type_name_owned()
                )))
            }
            _ => Err(type_error(format!(
                "'{}' object is not async-iterable",
                value.type_name_owned()
            ))),
        }
    }

    /// `__anext__` dispatch — returns the awaitable that yields the
    /// next value of the async iterator.
    fn get_anext(
        &mut self,
        aiter: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        match aiter {
            Object::AsyncGenerator(_) => {
                // The async generator is itself the awaitable for the
                // next yield (cooperative model — we don't allocate a
                // fresh `async_generator_asend` like CPython does).
                // `SEND` knows how to translate `StopIteration` into
                // `StopAsyncIteration` for async generators.
                Ok(aiter.clone())
            }
            Object::Instance(_) => {
                if let Some(method) = instance_method(aiter, "__anext__") {
                    return self.call(&method, &[], &[], globals);
                }
                Err(type_error(format!(
                    "'{}' object is not an async iterator",
                    aiter.type_name_owned()
                )))
            }
            _ => Err(type_error(format!(
                "'{}' object is not an async iterator",
                aiter.type_name_owned()
            ))),
        }
    }

    /// Pull the next value from `iter`. Returns `Ok(None)` on
    /// exhaustion, `Ok(Some(v))` on yield, or propagates errors.
    fn iter_next(
        &mut self,
        iter: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Option<Object>, RuntimeError> {
        match iter {
            Object::Iter(it) => Ok(it.borrow_mut().next_value()),
            Object::Generator(g) => match self.generator_send(g, Object::None) {
                Ok(v) => Ok(Some(v)),
                Err(RuntimeError::PyException(exc)) if exc.type_name() == "StopIteration" => {
                    Ok(None)
                }
                Err(e) => Err(e),
            },
            Object::Instance(_) => {
                if let Some(method) = instance_method(iter, "__next__") {
                    match self.call(&method, &[], &[], globals) {
                        Ok(v) => Ok(Some(v)),
                        Err(RuntimeError::PyException(exc))
                            if exc.type_name() == "StopIteration" =>
                        {
                            Ok(None)
                        }
                        Err(e) => Err(e),
                    }
                } else {
                    Err(type_error(format!(
                        "'{}' object is not an iterator",
                        iter.type_name_owned()
                    )))
                }
            }
            _ => Err(type_error(format!(
                "'{}' object is not an iterator",
                iter.type_name_owned()
            ))),
        }
    }

    /// Format a value through the f-string mini-language. The exact
    /// rules are CPython's: `!s` calls `str`, `!r` calls `repr`,
    /// `!a` calls `ascii`; the optional `format_spec` then drives
    /// width / precision / type formatting.
    fn format_value(
        &mut self,
        value: &Object,
        conversion: u32,
        spec: Option<&Object>,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<String, RuntimeError> {
        let s = match conversion {
            0 => self.stringify(value, globals)?,
            1 => self.stringify(value, globals)?, // !s
            2 => self.repr_of(value, globals)?,   // !r
            3 => ascii_repr(value),
            _ => {
                return Err(RuntimeError::Internal(format!(
                    "unknown f-string conversion {conversion}"
                )))
            }
        };
        match spec {
            None => Ok(s),
            Some(Object::Str(spec_str)) => {
                if spec_str.is_empty() {
                    return Ok(s);
                }
                apply_format_spec(value, spec_str, &s)
            }
            Some(_) => Err(type_error("format spec must be a string")),
        }
    }

    /// `gen.send(value)` / `coro.send(value)` / `agen.asend(value)`
    /// dispatch entry. The receiver is one of the three async-shaped
    /// object kinds; this routes to [`Self::generator_send`].
    fn gen_method_send(
        &mut self,
        receiver: &Object,
        value: Object,
    ) -> Result<Object, RuntimeError> {
        let (g, is_async_gen) = match receiver {
            Object::Generator(g) | Object::Coroutine(g) => (g.clone(), false),
            Object::AsyncGenerator(g) => (g.clone(), true),
            other => {
                return Err(type_error(format!(
                    "send() requires a generator/coroutine, got '{}'",
                    other.type_name()
                )))
            }
        };
        match self.generator_send(&g, value) {
            Err(RuntimeError::PyException(exc))
                if is_async_gen && exc.type_name() == "StopIteration" =>
            {
                Err(stop_async_iteration())
            }
            other => other,
        }
    }

    /// `gen.throw(exc[, val[, tb]])` — inject an exception at the
    /// suspended yield-point. Minimal implementation: we don't try
    /// to faithfully resume the frame; we raise the exception out of
    /// the caller's `.throw()` call site.
    fn gen_method_throw(
        &mut self,
        receiver: &Object,
        args: &[Object],
    ) -> Result<Object, RuntimeError> {
        let (g, is_async_gen) = match receiver {
            Object::Generator(g) | Object::Coroutine(g) => (g.clone(), false),
            Object::AsyncGenerator(g) => (g.clone(), true),
            _ => return Err(type_error("throw() requires a generator/coroutine")),
        };
        let exc_obj = args
            .first()
            .cloned()
            .ok_or_else(|| type_error("throw() requires an exception argument"))?;
        let instance = match &exc_obj {
            Object::Type(t) => crate::builtin_types::make_exception_with_class(t.clone(), ""),
            inst @ Object::Instance(_) => inst.clone(),
            other => {
                return Err(type_error(format!(
                    "throw() argument must be an exception, got '{}'",
                    other.type_name()
                )))
            }
        };
        match self.generator_throw(&g, PyException::new(instance)) {
            Err(RuntimeError::PyException(exc))
                if is_async_gen && exc.type_name() == "StopIteration" =>
            {
                Err(stop_async_iteration())
            }
            other => other,
        }
    }

    /// Inject `exc` into the suspended generator at its current
    /// resume point. The frame's exception table gets first crack;
    /// if no handler matches the exception bubbles out of `throw()`.
    ///
    /// PEP 380: if the frame is paused inside a ``yield from``
    /// delegation, the inner sub-iterator gets the exception
    /// first. If the inner swallows it (yields a new value), that
    /// value is returned from our `.throw()`. If the inner raises
    /// `StopIteration`, the outer frame resumes with the returned
    /// value. Any other exception falls through to the outer's
    /// exception table.
    fn generator_throw(
        &mut self,
        gen: &Rc<PyGenerator>,
        exc: PyException,
    ) -> Result<Object, RuntimeError> {
        let prev_state = std::mem::replace(&mut *gen.state.borrow_mut(), GeneratorState::Running);
        let mut frame = match prev_state {
            GeneratorState::Created(boxed) | GeneratorState::Suspended(boxed) => *boxed
                .downcast::<Frame>()
                .map_err(|_| RuntimeError::Internal("generator frame downcast".to_owned()))?,
            GeneratorState::Finished => {
                *gen.state.borrow_mut() = GeneratorState::Finished;
                return Err(RuntimeError::PyException(exc));
            }
            GeneratorState::Running => {
                return Err(value_error("generator already executing"));
            }
        };

        // PEP 380 delegation. We detect "frame paused in
        // yield-from" via the bytecode pattern: the most recently
        // executed instruction was YIELD_VALUE, the one before that
        // was SEND, and the stack top is an iterator-like.
        if let Some(sub_iter) = detect_yield_from_subiter(&frame) {
            match self.throw_into_subiter(&sub_iter, exc.clone()) {
                Ok(v) => {
                    // Inner yielded: re-suspend the outer at the
                    // same point and surface the new value.
                    *gen.state.borrow_mut() = GeneratorState::Suspended(Box::new(frame));
                    return Ok(v);
                }
                Err(RuntimeError::PyException(inner_exc))
                    if inner_exc.type_name() == "StopIteration" =>
                {
                    // Inner finished cleanly. Replace the iter on
                    // the stack with the StopIteration's value and
                    // advance past the SEND/YIELD/JUMP-BACK loop.
                    let ret_val = exception_value(&inner_exc.instance);
                    if !frame.stack.is_empty() {
                        let len = frame.stack.len();
                        frame.stack[len - 1] = ret_val;
                    }
                    advance_past_yield_from(&mut frame);
                    return match self.run_until_yield_or_return(&mut frame, None) {
                        Ok(FrameOutcome::Yielded(v)) => {
                            *gen.state.borrow_mut() = GeneratorState::Suspended(Box::new(frame));
                            Ok(v)
                        }
                        Ok(FrameOutcome::Returned(v)) => {
                            *gen.state.borrow_mut() = GeneratorState::Finished;
                            Err(stop_iteration_with(v))
                        }
                        Ok(FrameOutcome::StartGenerator) => {
                            *gen.state.borrow_mut() = GeneratorState::Finished;
                            Err(RuntimeError::Internal(
                                "RETURN_GENERATOR inside generator_throw".to_owned(),
                            ))
                        }
                        Err(err) => {
                            *gen.state.borrow_mut() = GeneratorState::Finished;
                            Err(err)
                        }
                    };
                }
                Err(RuntimeError::PyException(inner_exc)) => {
                    // Inner re-raised; drop the sub-iter and hand
                    // the new exception to the outer's table.
                    if !frame.stack.is_empty() {
                        frame.stack.pop();
                    }
                    return self.resume_outer_with_exc(gen, frame, inner_exc);
                }
                Err(err) => {
                    *gen.state.borrow_mut() = GeneratorState::Finished;
                    return Err(err);
                }
            }
        }

        // Let the suspended frame handle the exception via its own
        // exception table; if no handler claims it the error bubbles
        // out and we mark the generator finished.
        match self.handle_exception(&mut frame, exc) {
            Ok(Some(())) => match self.run_until_yield_or_return(&mut frame, None) {
                Ok(FrameOutcome::Yielded(v)) => {
                    *gen.state.borrow_mut() = GeneratorState::Suspended(Box::new(frame));
                    Ok(v)
                }
                Ok(FrameOutcome::Returned(v)) => {
                    *gen.state.borrow_mut() = GeneratorState::Finished;
                    Err(stop_iteration_with(v))
                }
                Ok(FrameOutcome::StartGenerator) => {
                    *gen.state.borrow_mut() = GeneratorState::Finished;
                    Err(RuntimeError::Internal(
                        "RETURN_GENERATOR inside generator_throw".to_owned(),
                    ))
                }
                Err(err) => {
                    *gen.state.borrow_mut() = GeneratorState::Finished;
                    Err(err)
                }
            },
            Ok(None) => unreachable!(),
            Err(err) => {
                *gen.state.borrow_mut() = GeneratorState::Finished;
                Err(err)
            }
        }
    }

    /// `gen.close()` — request the generator to terminate. CPython
    /// injects a `GeneratorExit` at the resume point so any
    /// `finally` blocks run; we mirror that by routing through
    /// `generator_throw` and absorbing the resulting StopIteration.
    fn gen_method_close(&mut self, receiver: &Object) -> Result<Object, RuntimeError> {
        let g = match receiver {
            Object::Generator(g) | Object::Coroutine(g) | Object::AsyncGenerator(g) => g.clone(),
            _ => return Err(type_error("close() requires a generator/coroutine")),
        };
        if g.is_finished() {
            return Ok(Object::None);
        }
        // Build a `GeneratorExit` exception and inject it.
        let bt = crate::builtin_types::builtin_types();
        let exc_inst =
            crate::builtin_types::make_exception_with_class(bt.generator_exit.clone(), "");
        match self.generator_throw(&g, PyException::new(exc_inst)) {
            Ok(_yielded) => {
                // PEP 342: generator ignored GeneratorExit (yielded
                // a new value instead of allowing the exit to
                // propagate). CPython raises RuntimeError here.
                *g.state.borrow_mut() = GeneratorState::Finished;
                Err(crate::error::runtime_error(
                    "generator ignored GeneratorExit",
                ))
            }
            Err(RuntimeError::PyException(exc))
                if exc.type_name() == "GeneratorExit"
                    || exc.type_name() == "StopIteration"
                    || exc.type_name() == "StopAsyncIteration" =>
            {
                *g.state.borrow_mut() = GeneratorState::Finished;
                Ok(Object::None)
            }
            Err(err) => {
                *g.state.borrow_mut() = GeneratorState::Finished;
                Err(err)
            }
        }
    }

    /// Drive ``sub_iter.throw(exc)`` — the inner sub-iterator's
    /// own throw machinery. Used by yield-from delegation. Returns
    /// the inner's yielded value, or propagates whatever exception
    /// the inner re-raises.
    fn throw_into_subiter(
        &mut self,
        sub_iter: &Object,
        exc: PyException,
    ) -> Result<Object, RuntimeError> {
        match sub_iter {
            Object::Generator(g) | Object::Coroutine(g) | Object::AsyncGenerator(g) => {
                self.generator_throw(g, exc)
            }
            _ => {
                // Non-generator iterators don't have `.throw()`;
                // CPython just re-raises the exception out of the
                // delegation.
                Err(RuntimeError::PyException(exc))
            }
        }
    }

    /// Continue the outer generator after the sub-iterator raised
    /// an exception other than StopIteration. Hands the exception
    /// to the outer's exception table.
    fn resume_outer_with_exc(
        &mut self,
        gen: &Rc<PyGenerator>,
        mut frame: Frame,
        exc: PyException,
    ) -> Result<Object, RuntimeError> {
        match self.handle_exception(&mut frame, exc) {
            Ok(Some(())) => match self.run_until_yield_or_return(&mut frame, None) {
                Ok(FrameOutcome::Yielded(v)) => {
                    *gen.state.borrow_mut() = GeneratorState::Suspended(Box::new(frame));
                    Ok(v)
                }
                Ok(FrameOutcome::Returned(v)) => {
                    *gen.state.borrow_mut() = GeneratorState::Finished;
                    Err(stop_iteration_with(v))
                }
                Ok(FrameOutcome::StartGenerator) => {
                    *gen.state.borrow_mut() = GeneratorState::Finished;
                    Err(RuntimeError::Internal(
                        "RETURN_GENERATOR inside resume_outer_with_exc".to_owned(),
                    ))
                }
                Err(err) => {
                    *gen.state.borrow_mut() = GeneratorState::Finished;
                    Err(err)
                }
            },
            Ok(None) => unreachable!(),
            Err(err) => {
                *gen.state.borrow_mut() = GeneratorState::Finished;
                Err(err)
            }
        }
    }

    /// Run a generator's frame to its next yield or terminal state.
    /// `sent` is the value pushed onto the frame's stack as the
    /// result of the prior `YIELD_VALUE`; for `__next__()` callers
    /// it's `None`.
    fn generator_send(
        &mut self,
        gen: &Rc<PyGenerator>,
        sent: Object,
    ) -> Result<Object, RuntimeError> {
        // Take ownership of the frame so we can mutate it.
        let prev_state = std::mem::replace(&mut *gen.state.borrow_mut(), GeneratorState::Running);
        let (mut frame, first_resume) = match prev_state {
            GeneratorState::Created(boxed) => (
                *boxed
                    .downcast::<Frame>()
                    .map_err(|_| RuntimeError::Internal("generator frame downcast".to_owned()))?,
                true,
            ),
            GeneratorState::Suspended(boxed) => (
                *boxed
                    .downcast::<Frame>()
                    .map_err(|_| RuntimeError::Internal("generator frame downcast".to_owned()))?,
                false,
            ),
            GeneratorState::Finished => {
                *gen.state.borrow_mut() = GeneratorState::Finished;
                return Err(stop_iteration());
            }
            GeneratorState::Running => {
                return Err(value_error("generator already executing"));
            }
        };
        // On the first call, `sent` must be None (or omitted).
        if first_resume && !matches!(sent, Object::None) {
            *gen.state.borrow_mut() = GeneratorState::Suspended(Box::new(frame));
            return Err(type_error(
                "can't send non-None value to a just-started generator",
            ));
        }
        let sent_for_frame = if first_resume { None } else { Some(sent) };
        match self.run_until_yield_or_return(&mut frame, sent_for_frame) {
            Ok(FrameOutcome::Yielded(v)) => {
                *gen.state.borrow_mut() = GeneratorState::Suspended(Box::new(frame));
                Ok(v)
            }
            Ok(FrameOutcome::Returned(v)) => {
                // Generators always surface the return value through
                // `StopIteration.value`. Even a `return None` (or an
                // implicit return) needs to leave the attribute set
                // so the `SEND`/`END_SEND` machinery in the await
                // dance unwraps to `None` rather than to the empty
                // string we get from `from_builtin("StopIteration",
                // "")`.
                *gen.state.borrow_mut() = GeneratorState::Finished;
                Err(stop_iteration_with(v))
            }
            Ok(FrameOutcome::StartGenerator) => {
                *gen.state.borrow_mut() = GeneratorState::Finished;
                Err(RuntimeError::Internal(
                    "RETURN_GENERATOR in already-running generator".to_owned(),
                ))
            }
            Err(err) => {
                *gen.state.borrow_mut() = GeneratorState::Finished;
                Err(err)
            }
        }
    }

    /// Implement `MATCH_CLASS`: check `isinstance(subject, cls)` and
    /// pull out positional + keyword sub-values into a single tuple.
    /// Returns `None` if the type test or any attribute access fails.
    fn match_class(
        &mut self,
        subject: &Object,
        cls: &Object,
        nargs: usize,
        kw_names: &[String],
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        // Type check.
        let ty = match cls {
            Object::Type(t) => t.clone(),
            _ => return Err(type_error("called match pattern must be a type")),
        };
        let is_inst = match subject {
            Object::Instance(inst) => inst.class.is_subclass_of(&ty),
            _ => {
                // Built-in mapping: roughly match by type_name.
                let bt = builtin_types();
                let expected = ty.name.as_str();
                let actual = subject.type_name();
                expected == actual
                    || (expected == "object")
                    || self.builtin_is_subtype(subject, &ty, &bt)
            }
        };
        if !is_inst {
            return Ok(Object::None);
        }
        // Positional matching uses `__match_args__` on the class.
        // For the 8 "self-match" built-in types, `Cls(p)` matches by
        // identity: the single positional captures the whole subject.
        const SELF_MATCH: &[&str] = &[
            "bool",
            "bytearray",
            "bytes",
            "dict",
            "float",
            "frozenset",
            "int",
            "list",
            "set",
            "str",
            "tuple",
        ];
        let mut values: Vec<Object> = Vec::with_capacity(nargs + kw_names.len());
        if nargs > 0 {
            let is_self_match = SELF_MATCH.contains(&ty.name.as_str()) && nargs == 1;
            if is_self_match {
                values.push(subject.clone());
            } else {
                let match_args = self
                    .load_attr(cls, "__match_args__")
                    .unwrap_or(Object::None);
                let names: Vec<String> = match match_args {
                    Object::Tuple(items) => items.iter().map(|x| x.to_str()).collect(),
                    _ => Vec::new(),
                };
                if names.len() < nargs {
                    return Ok(Object::None);
                }
                for name in names.iter().take(nargs) {
                    match self.load_attr(subject, name) {
                        Ok(v) => values.push(v),
                        Err(_) => return Ok(Object::None),
                    }
                }
            }
        }
        for name in kw_names {
            match self.load_attr(subject, name) {
                Ok(v) => values.push(v),
                Err(_) => return Ok(Object::None),
            }
        }
        let _ = globals;
        Ok(Object::new_tuple(values))
    }

    /// Heuristic for `match Cls(...)` when `Cls` is a built-in
    /// wrapper around a primitive type (e.g. `int`, `str`, `list`).
    fn builtin_is_subtype(
        &self,
        subject: &Object,
        ty: &Rc<TypeObject>,
        bt: &crate::builtin_types::BuiltinTypes,
    ) -> bool {
        let name = ty.name.as_str();
        match (name, subject) {
            ("int", Object::Int(_)) => true,
            ("int", Object::Bool(_)) => true,
            ("bool", Object::Bool(_)) => true,
            ("float", Object::Float(_)) => true,
            ("str", Object::Str(_)) => true,
            ("tuple", Object::Tuple(_)) => true,
            ("list", Object::List(_)) => true,
            ("dict", Object::Dict(_)) => true,
            ("object", _) => true,
            _ => {
                let _ = bt;
                false
            }
        }
    }

    fn dispatch_binary_op(
        &mut self,
        a: &Object,
        b: &Object,
        op: BinOpKind,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let (dunder, rdunder) = binop_dunders(op);
        // `a.__op__(b)` first, then `b.__rop__(a)` if it returns
        // NotImplemented. Our slice treats "no method" as
        // NotImplemented and the missing-symmetric falls through to
        // [`binary_op`] for built-in types.
        if let Some(method) = instance_method(a, dunder) {
            return self.call(&method, std::slice::from_ref(b), &[], globals);
        }
        if let Some(method) = instance_method(b, rdunder) {
            return self.call(&method, std::slice::from_ref(a), &[], globals);
        }
        binary_op(a, b, op)
    }

    fn dispatch_compare_op(
        &mut self,
        a: &Object,
        b: &Object,
        op: CompareKind,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<bool, RuntimeError> {
        let (dunder, swapped) = cmp_dunder(op);
        if let Some(method) = instance_method(a, dunder) {
            let r = self.call(&method, std::slice::from_ref(b), &[], globals)?;
            return Ok(r.is_truthy());
        }
        if let Some(method) = instance_method(b, swapped) {
            let r = self.call(&method, std::slice::from_ref(a), &[], globals)?;
            return Ok(r.is_truthy());
        }
        // Container equality must defer to per-element `__eq__` so
        // that wrapper objects with custom equality (e.g. mock.ANY)
        // compare as expected when embedded in a tuple/list.
        if matches!(op, CompareKind::Eq | CompareKind::NotEq) {
            if let Some(rv) = self.deep_equal_collection(a, b, globals)? {
                let truth = match op {
                    CompareKind::Eq => rv,
                    _ => !rv,
                };
                return Ok(truth);
            }
        }
        compare_op(a, b, op)
    }

    // ---------- RFC 0021 specialized fast paths ----------

    /// Run the `BINARY_OP` cache machinery. Returns `Ok(true)` if a
    /// fast path consumed both operands and pushed the result,
    /// `Ok(false)` if the caller should run the generic handler
    /// (the operands are still on the stack), or an error from
    /// inside a fast path.
    ///
    /// On `Empty` cache state, this peeks the operands and either
    /// installs a specialization + runs the fast path or installs
    /// `Cooldown` and yields to the generic path. On `Cooldown(n)`
    /// it decrements and yields. Specialization installation
    /// happens here (not after the generic path) because we have
    /// the operands at hand; reusing them avoids a second pop +
    /// type-inspect later.
    fn specialized_binary_op(
        &mut self,
        frame: &mut Frame,
        cache_pc: u32,
        kind: BinOpKind,
    ) -> Result<bool, RuntimeError> {
        use weavepy_compiler::InlineCache as IC;
        let cache = frame.code.caches.get(cache_pc);
        let op_idx = OpCode::BinaryOp as u8;
        match cache {
            IC::Empty => {
                // Peek operands; decide specialization.
                let (a_peek, b_peek) = match (frame.peek_back(1), frame.peek_back(0)) {
                    (Some(a), Some(b)) => (a.clone(), b.clone()),
                    _ => return Ok(false),
                };
                specialize::record_specialize_attempt(op_idx);
                let decision = specialize::attempt_specialize_binary_op(&a_peek, &b_peek, kind);
                frame.code.caches.set(cache_pc, decision);
                if matches!(decision, IC::Cooldown(_)) {
                    specialize::record_specialize_skip(op_idx);
                    return Ok(false);
                }
                specialize::record_specialize_success(op_idx);
                // Fall through to the specialized arm below by
                // re-reading the cache.
                self.specialized_binary_op(frame, cache_pc, kind)
            }
            IC::BinOpAddInt | IC::BinOpSubInt | IC::BinOpMulInt => {
                let (a, b) = match (frame.peek_back(1), frame.peek_back(0)) {
                    (Some(Object::Int(x)), Some(Object::Int(y))) => (*x, *y),
                    _ => return self.deopt_binary_op(frame, cache_pc),
                };
                let (r, overflowed) = match (cache, kind) {
                    (IC::BinOpAddInt, BinOpKind::Add) => {
                        (a.wrapping_add(b), a.checked_add(b).is_none())
                    }
                    (IC::BinOpSubInt, BinOpKind::Sub) => {
                        (a.wrapping_sub(b), a.checked_sub(b).is_none())
                    }
                    (IC::BinOpMulInt, BinOpKind::Mult) => {
                        (a.wrapping_mul(b), a.checked_mul(b).is_none())
                    }
                    _ => return self.deopt_binary_op(frame, cache_pc),
                };
                if overflowed {
                    return self.deopt_binary_op(frame, cache_pc);
                }
                let len = frame.stack.len();
                frame.stack.truncate(len - 2);
                frame.push(Object::Int(r));
                specialize::record_hit(op_idx);
                Ok(true)
            }
            IC::BinOpAddFloat | IC::BinOpSubFloat | IC::BinOpMulFloat => {
                let (a, b) = match (frame.peek_back(1), frame.peek_back(0)) {
                    (Some(Object::Float(x)), Some(Object::Float(y))) => (*x, *y),
                    _ => return self.deopt_binary_op(frame, cache_pc),
                };
                let r = match (cache, kind) {
                    (IC::BinOpAddFloat, BinOpKind::Add) => a + b,
                    (IC::BinOpSubFloat, BinOpKind::Sub) => a - b,
                    (IC::BinOpMulFloat, BinOpKind::Mult) => a * b,
                    _ => return self.deopt_binary_op(frame, cache_pc),
                };
                let len = frame.stack.len();
                frame.stack.truncate(len - 2);
                frame.push(Object::Float(r));
                specialize::record_hit(op_idx);
                Ok(true)
            }
            IC::BinOpAddStr if matches!(kind, BinOpKind::Add) => {
                let r = match (frame.peek_back(1), frame.peek_back(0)) {
                    (Some(Object::Str(x)), Some(Object::Str(y))) => {
                        let mut out = String::with_capacity(x.len() + y.len());
                        out.push_str(x);
                        out.push_str(y);
                        Object::from_str(out)
                    }
                    _ => return self.deopt_binary_op(frame, cache_pc),
                };
                let len = frame.stack.len();
                frame.stack.truncate(len - 2);
                frame.push(r);
                specialize::record_hit(op_idx);
                Ok(true)
            }
            IC::Cooldown(n) => {
                let next = if n > 0 {
                    IC::Cooldown(n - 1)
                } else {
                    IC::Empty
                };
                frame.code.caches.set(cache_pc, next);
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    /// Deopt a `BINARY_OP` cache: install `Cooldown` and yield
    /// control back to the generic handler. The operands are
    /// already on the stack, so `Ok(false)` just lets the caller
    /// pop them as usual.
    #[inline]
    fn deopt_binary_op(&self, frame: &Frame, cache_pc: u32) -> Result<bool, RuntimeError> {
        specialize::record_miss(OpCode::BinaryOp as u8);
        frame
            .code
            .caches
            .set(cache_pc, weavepy_compiler::InlineCache::Cooldown(COOLDOWN));
        Ok(false)
    }

    /// Run the `COMPARE_OP` cache machinery. Same shape as
    /// [`Self::specialized_binary_op`].
    fn specialized_compare_op(
        &mut self,
        frame: &mut Frame,
        cache_pc: u32,
        kind: CompareKind,
    ) -> Result<bool, RuntimeError> {
        use weavepy_compiler::InlineCache as IC;
        let cache = frame.code.caches.get(cache_pc);
        let op_idx = OpCode::CompareOp as u8;
        match cache {
            IC::Empty => {
                let (a_peek, b_peek) = match (frame.peek_back(1), frame.peek_back(0)) {
                    (Some(a), Some(b)) => (a.clone(), b.clone()),
                    _ => return Ok(false),
                };
                specialize::record_specialize_attempt(op_idx);
                let decision = specialize::attempt_specialize_compare_op(&a_peek, &b_peek, kind);
                frame.code.caches.set(cache_pc, decision);
                if matches!(decision, IC::Cooldown(_)) {
                    specialize::record_specialize_skip(op_idx);
                    return Ok(false);
                }
                specialize::record_specialize_success(op_idx);
                self.specialized_compare_op(frame, cache_pc, kind)
            }
            IC::CompareOpInt => {
                let (a, b) = match (frame.peek_back(1), frame.peek_back(0)) {
                    (Some(Object::Int(x)), Some(Object::Int(y))) => (*x, *y),
                    _ => return self.deopt_compare_op(frame, cache_pc),
                };
                let r = compare_int(a, b, kind);
                let len = frame.stack.len();
                frame.stack.truncate(len - 2);
                frame.push(Object::Bool(r));
                specialize::record_hit(op_idx);
                Ok(true)
            }
            IC::CompareOpFloat => {
                let (a, b) = match (frame.peek_back(1), frame.peek_back(0)) {
                    (Some(Object::Float(x)), Some(Object::Float(y))) => (*x, *y),
                    _ => return self.deopt_compare_op(frame, cache_pc),
                };
                let r = compare_float(a, b, kind);
                let len = frame.stack.len();
                frame.stack.truncate(len - 2);
                frame.push(Object::Bool(r));
                specialize::record_hit(op_idx);
                Ok(true)
            }
            IC::CompareOpStr => {
                let (a_str, b_str) = match (frame.peek_back(1), frame.peek_back(0)) {
                    (Some(Object::Str(x)), Some(Object::Str(y))) => (x.clone(), y.clone()),
                    _ => return self.deopt_compare_op(frame, cache_pc),
                };
                let r = compare_str(a_str.as_ref(), b_str.as_ref(), kind);
                let len = frame.stack.len();
                frame.stack.truncate(len - 2);
                frame.push(Object::Bool(r));
                specialize::record_hit(op_idx);
                Ok(true)
            }
            IC::Cooldown(n) => {
                let next = if n > 0 {
                    IC::Cooldown(n - 1)
                } else {
                    IC::Empty
                };
                frame.code.caches.set(cache_pc, next);
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    /// Deopt a `COMPARE_OP` cache.
    #[inline]
    fn deopt_compare_op(&self, frame: &Frame, cache_pc: u32) -> Result<bool, RuntimeError> {
        specialize::record_miss(OpCode::CompareOp as u8);
        frame
            .code
            .caches
            .set(cache_pc, weavepy_compiler::InlineCache::Cooldown(COOLDOWN));
        Ok(false)
    }

    /// Specialized `LOAD_GLOBAL`. On a warm cache, looks up the
    /// value by integer slot in the appropriate dict (skipping the
    /// hash-keyed lookup). On `Empty` cache, performs the regular
    /// lookup and installs a specialization. On `Cooldown`,
    /// decrements and uses the slow path.
    ///
    /// The specialized paths still verify the dict's `Rc::as_ptr`
    /// fingerprint against the cache so user code that swaps out
    /// `globals` (rare but legal in `exec`) deopts cleanly.
    fn specialized_load_global(
        &mut self,
        frame: &Frame,
        cache_pc: u32,
        name_idx: u32,
    ) -> Result<Object, RuntimeError> {
        use weavepy_compiler::InlineCache as IC;
        let cache = frame.code.caches.get(cache_pc);
        let op_idx = OpCode::LoadGlobal as u8;
        match cache {
            IC::LoadGlobalModule {
                globals_id,
                key_idx,
            } => {
                if specialize::rc_id(&frame.globals) != globals_id {
                    return self.deopt_load_global_slow(frame, cache_pc, name_idx);
                }
                let g = frame.globals.borrow();
                if let Some((_, v)) = g.get_index(key_idx as usize) {
                    specialize::record_hit(op_idx);
                    return Ok(v.clone());
                }
                drop(g);
                self.deopt_load_global_slow(frame, cache_pc, name_idx)
            }
            IC::LoadGlobalBuiltin {
                builtins_id,
                key_idx,
            } => {
                if specialize::rc_id(&self.builtins) != builtins_id {
                    return self.deopt_load_global_slow(frame, cache_pc, name_idx);
                }
                // Guard that the name *isn't* shadowed in globals
                // since we last specialized — otherwise we'd
                // bypass user code that subsequently bound the name
                // at module scope.
                let name = self.name_at(&frame.code, name_idx)?;
                if frame
                    .globals
                    .borrow()
                    .contains_key(&DictKey(Object::from_str(&name)))
                {
                    return self.deopt_load_global_slow(frame, cache_pc, name_idx);
                }
                let b = self.builtins.borrow();
                if let Some((_, v)) = b.get_index(key_idx as usize) {
                    specialize::record_hit(op_idx);
                    return Ok(v.clone());
                }
                drop(b);
                self.deopt_load_global_slow(frame, cache_pc, name_idx)
            }
            IC::Empty => {
                let name = self.name_at(&frame.code, name_idx)?;
                specialize::record_specialize_attempt(op_idx);
                let decision = specialize::attempt_specialize_load_global(
                    &frame.globals,
                    &self.builtins,
                    &name,
                );
                frame.code.caches.set(cache_pc, decision);
                if matches!(decision, IC::Cooldown(_)) {
                    specialize::record_specialize_skip(op_idx);
                } else {
                    specialize::record_specialize_success(op_idx);
                }
                self.lookup_global_or_builtin(&frame.globals, &name)
            }
            IC::Cooldown(n) => {
                let next = if n > 0 {
                    IC::Cooldown(n - 1)
                } else {
                    IC::Empty
                };
                frame.code.caches.set(cache_pc, next);
                let name = self.name_at(&frame.code, name_idx)?;
                self.lookup_global_or_builtin(&frame.globals, &name)
            }
            _ => {
                let name = self.name_at(&frame.code, name_idx)?;
                self.lookup_global_or_builtin(&frame.globals, &name)
            }
        }
    }

    /// Deopt a `LOAD_GLOBAL` cache and run the generic lookup.
    #[inline]
    fn deopt_load_global_slow(
        &self,
        frame: &Frame,
        cache_pc: u32,
        name_idx: u32,
    ) -> Result<Object, RuntimeError> {
        specialize::record_miss(OpCode::LoadGlobal as u8);
        frame
            .code
            .caches
            .set(cache_pc, weavepy_compiler::InlineCache::Cooldown(COOLDOWN));
        let name = self.name_at(&frame.code, name_idx)?;
        self.lookup_global_or_builtin(&frame.globals, &name)
    }

    /// Specialized `LOAD_ATTR`. The receiver lives at TOS; on a
    /// warm cache we lookup by integer slot in the appropriate
    /// dict (instance / module / type), guarded by the cached
    /// type/module fingerprint. On miss we deopt and run the
    /// generic [`Self::load_attr`].
    fn specialized_load_attr(
        &mut self,
        frame: &mut Frame,
        cache_pc: u32,
        name_idx: u32,
    ) -> Result<Object, RuntimeError> {
        use weavepy_compiler::InlineCache as IC;
        let cache = frame.code.caches.get(cache_pc);
        let op_idx = OpCode::LoadAttr as u8;
        match cache {
            IC::LoadAttrInstance { type_id, key_idx } => {
                let receiver = frame.top()?.clone();
                if let Object::Instance(inst) = &receiver {
                    if specialize::rc_id(&inst.class) == type_id {
                        let dict = inst.dict.borrow();
                        if let Some((_, v)) = dict.get_index(key_idx as usize) {
                            let v = v.clone();
                            drop(dict);
                            frame.pop()?;
                            specialize::record_hit(op_idx);
                            return Ok(v);
                        }
                    }
                }
                self.deopt_load_attr_slow(frame, cache_pc, name_idx)
            }
            IC::LoadAttrModule { module_id, key_idx } => {
                let receiver = frame.top()?.clone();
                if let Object::Module(m) = &receiver {
                    if specialize::rc_id(&m.dict) == module_id {
                        let dict = m.dict.borrow();
                        if let Some((_, v)) = dict.get_index(key_idx as usize) {
                            let v = v.clone();
                            drop(dict);
                            frame.pop()?;
                            specialize::record_hit(op_idx);
                            return Ok(v);
                        }
                    }
                }
                self.deopt_load_attr_slow(frame, cache_pc, name_idx)
            }
            IC::LoadAttrType { type_id, key_idx } => {
                let receiver = frame.top()?.clone();
                if let Object::Instance(inst) = &receiver {
                    if specialize::rc_id(&inst.class) == type_id {
                        let dict = inst.class.dict.borrow();
                        if let Some((_, v)) = dict.get_index(key_idx as usize) {
                            let v = v.clone();
                            drop(dict);
                            frame.pop()?;
                            specialize::record_hit(op_idx);
                            // For function descriptors found on the
                            // type we'd normally bind to the
                            // instance — bail to the slow path
                            // when the value is callable, so the
                            // generic descriptor protocol runs.
                            // (Bound-method specialization is RFC
                            // 0022 territory.)
                            if matches!(
                                v,
                                Object::Function(_)
                                    | Object::Builtin(_)
                                    | Object::Property(_)
                                    | Object::ClassMethod(_)
                                    | Object::StaticMethod(_)
                                    | Object::SlotDescriptor(_)
                            ) {
                                // Push receiver back and deopt.
                                frame.push(receiver);
                                return self.deopt_load_attr_slow(frame, cache_pc, name_idx);
                            }
                            return Ok(v);
                        }
                    }
                }
                self.deopt_load_attr_slow(frame, cache_pc, name_idx)
            }
            IC::Empty => {
                let receiver = frame.top()?.clone();
                let name = self.name_at(&frame.code, name_idx)?;
                specialize::record_specialize_attempt(op_idx);
                let decision = specialize::attempt_specialize_load_attr(&receiver, &name);
                frame.code.caches.set(cache_pc, decision);
                if matches!(decision, IC::Cooldown(_)) {
                    specialize::record_specialize_skip(op_idx);
                } else {
                    specialize::record_specialize_success(op_idx);
                }
                let obj = frame.pop()?;
                self.load_attr(&obj, &name)
            }
            IC::Cooldown(n) => {
                let next = if n > 0 {
                    IC::Cooldown(n - 1)
                } else {
                    IC::Empty
                };
                frame.code.caches.set(cache_pc, next);
                let obj = frame.pop()?;
                let name = self.name_at(&frame.code, name_idx)?;
                self.load_attr(&obj, &name)
            }
            _ => {
                let obj = frame.pop()?;
                let name = self.name_at(&frame.code, name_idx)?;
                self.load_attr(&obj, &name)
            }
        }
    }

    /// Deopt a `LOAD_ATTR` cache and run the generic handler.
    #[inline]
    fn deopt_load_attr_slow(
        &mut self,
        frame: &mut Frame,
        cache_pc: u32,
        name_idx: u32,
    ) -> Result<Object, RuntimeError> {
        specialize::record_miss(OpCode::LoadAttr as u8);
        frame
            .code
            .caches
            .set(cache_pc, weavepy_compiler::InlineCache::Cooldown(COOLDOWN));
        let obj = frame.pop()?;
        let name = self.name_at(&frame.code, name_idx)?;
        self.load_attr(&obj, &name)
    }

    /// Specialized `STORE_ATTR`. Stack discipline matches the
    /// existing arm: TOS is the receiver, TOS-1 is the value.
    /// On a warm cache, writes the value into the indexed dict
    /// slot; on miss, deopts to the generic [`Self::store_attr`].
    fn specialized_store_attr(
        &mut self,
        frame: &mut Frame,
        cache_pc: u32,
        name_idx: u32,
    ) -> Result<(), RuntimeError> {
        use weavepy_compiler::InlineCache as IC;
        let cache = frame.code.caches.get(cache_pc);
        let op_idx = OpCode::StoreAttr as u8;
        match cache {
            IC::StoreAttrInstance { type_id, key_idx } => {
                let receiver = frame.top()?.clone();
                if let Object::Instance(inst) = &receiver {
                    if specialize::rc_id(&inst.class) == type_id {
                        let dict_len = inst.dict.borrow().len();
                        if dict_len > key_idx as usize {
                            frame.pop()?;
                            let val = frame.pop()?;
                            // The slot still exists; reach in by
                            // index and overwrite. We rebuild the
                            // mutable borrow here because the
                            // earlier read-only check has been
                            // dropped.
                            if let Some((_, slot)) =
                                inst.dict.borrow_mut().get_index_mut(key_idx as usize)
                            {
                                *slot = val;
                                specialize::record_hit(op_idx);
                                return Ok(());
                            }
                        }
                    }
                }
                self.deopt_store_attr_slow(frame, cache_pc, name_idx)
            }
            IC::Empty => {
                let receiver = frame.top()?.clone();
                let name = self.name_at(&frame.code, name_idx)?;
                specialize::record_specialize_attempt(op_idx);
                let decision = specialize::attempt_specialize_store_attr(&receiver, &name);
                frame.code.caches.set(cache_pc, decision);
                if matches!(decision, IC::Cooldown(_)) {
                    specialize::record_specialize_skip(op_idx);
                } else {
                    specialize::record_specialize_success(op_idx);
                }
                let obj = frame.pop()?;
                let val = frame.pop()?;
                self.store_attr(&obj, &name, val)
            }
            IC::Cooldown(n) => {
                let next = if n > 0 {
                    IC::Cooldown(n - 1)
                } else {
                    IC::Empty
                };
                frame.code.caches.set(cache_pc, next);
                let obj = frame.pop()?;
                let val = frame.pop()?;
                let name = self.name_at(&frame.code, name_idx)?;
                self.store_attr(&obj, &name, val)
            }
            _ => {
                let obj = frame.pop()?;
                let val = frame.pop()?;
                let name = self.name_at(&frame.code, name_idx)?;
                self.store_attr(&obj, &name, val)
            }
        }
    }

    /// Deopt a `STORE_ATTR` cache.
    #[inline]
    fn deopt_store_attr_slow(
        &mut self,
        frame: &mut Frame,
        cache_pc: u32,
        name_idx: u32,
    ) -> Result<(), RuntimeError> {
        specialize::record_miss(OpCode::StoreAttr as u8);
        frame
            .code
            .caches
            .set(cache_pc, weavepy_compiler::InlineCache::Cooldown(COOLDOWN));
        let obj = frame.pop()?;
        let val = frame.pop()?;
        let name = self.name_at(&frame.code, name_idx)?;
        self.store_attr(&obj, &name, val)
    }

    /// Specialized `FOR_ITER`. Returns `Ok(true)` when the fast
    /// path handled the dispatch (a value was pushed or the loop
    /// exited), and `Ok(false)` when the caller should run the
    /// generic `FOR_ITER` arm.
    ///
    /// The cache stores no fingerprint — the iterator's concrete
    /// `PyIterator` variant is the fingerprint. If the variant
    /// changes (the same `Iter` started life as a list iter and
    /// somehow became a tuple iter), the guard bails into the
    /// generic path.
    fn specialized_for_iter(
        &mut self,
        frame: &mut Frame,
        cache_pc: u32,
        jump_arg: u32,
    ) -> Result<bool, RuntimeError> {
        use weavepy_compiler::InlineCache as IC;
        let cache = frame.code.caches.get(cache_pc);
        let op_idx = OpCode::ForIter as u8;
        let it_handle = match frame.stack.last() {
            Some(Object::Iter(it)) => it.clone(),
            _ => return Ok(false),
        };
        match cache {
            IC::ForIterList => {
                let mut it = it_handle.borrow_mut();
                if let crate::object::PyIterator::List { items, index } = &mut *it {
                    let next = items.borrow().get(*index).cloned();
                    if let Some(v) = next {
                        *index += 1;
                        drop(it);
                        frame.push(v);
                    } else {
                        drop(it);
                        frame.pop()?;
                        frame.pc += jump_arg;
                    }
                    specialize::record_hit(op_idx);
                    return Ok(true);
                }
                drop(it);
                self.deopt_for_iter(frame, cache_pc);
                Ok(false)
            }
            IC::ForIterTuple => {
                let mut it = it_handle.borrow_mut();
                if let crate::object::PyIterator::Tuple { items, index } = &mut *it {
                    let next = items.get(*index).cloned();
                    if let Some(v) = next {
                        *index += 1;
                        drop(it);
                        frame.push(v);
                    } else {
                        drop(it);
                        frame.pop()?;
                        frame.pc += jump_arg;
                    }
                    specialize::record_hit(op_idx);
                    return Ok(true);
                }
                drop(it);
                self.deopt_for_iter(frame, cache_pc);
                Ok(false)
            }
            IC::ForIterRange => {
                let mut it = it_handle.borrow_mut();
                if let crate::object::PyIterator::Range {
                    current,
                    stop,
                    step,
                } = &mut *it
                {
                    let exhausted = if *step > 0 {
                        *current >= *stop
                    } else if *step < 0 {
                        *current <= *stop
                    } else {
                        true
                    };
                    if exhausted {
                        drop(it);
                        frame.pop()?;
                        frame.pc += jump_arg;
                    } else {
                        let v = *current;
                        *current += *step;
                        drop(it);
                        frame.push(Object::Int(v));
                    }
                    specialize::record_hit(op_idx);
                    return Ok(true);
                }
                drop(it);
                self.deopt_for_iter(frame, cache_pc);
                Ok(false)
            }
            IC::Empty => {
                let receiver = frame.stack.last().cloned().unwrap_or(Object::None);
                specialize::record_specialize_attempt(op_idx);
                let decision = specialize::attempt_specialize_for_iter(&receiver);
                frame.code.caches.set(cache_pc, decision);
                if matches!(decision, IC::Cooldown(_)) {
                    specialize::record_specialize_skip(op_idx);
                } else {
                    specialize::record_specialize_success(op_idx);
                }
                Ok(false)
            }
            IC::Cooldown(n) => {
                let next = if n > 0 {
                    IC::Cooldown(n - 1)
                } else {
                    IC::Empty
                };
                frame.code.caches.set(cache_pc, next);
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    /// Deopt a `FOR_ITER` cache.
    #[inline]
    fn deopt_for_iter(&self, frame: &Frame, cache_pc: u32) {
        specialize::record_miss(OpCode::ForIter as u8);
        frame
            .code
            .caches
            .set(cache_pc, weavepy_compiler::InlineCache::Cooldown(COOLDOWN));
    }

    /// Specialized `UNPACK_SEQUENCE`. Tuple / list / two-tuple
    /// fast paths skip the iterator construction the generic arm
    /// runs for arbitrary iterables. Returns `Ok(true)` when the
    /// fast path consumed the sequence and pushed N elements;
    /// `Ok(false)` lets the caller run the generic arm.
    fn specialized_unpack_sequence(
        &mut self,
        frame: &mut Frame,
        cache_pc: u32,
        n: usize,
    ) -> Result<bool, RuntimeError> {
        use weavepy_compiler::InlineCache as IC;
        let cache = frame.code.caches.get(cache_pc);
        let op_idx = OpCode::UnpackSequence as u8;
        match cache {
            IC::UnpackSequenceTwoTuple if n == 2 => {
                let v = frame.top()?.clone();
                if let Object::Tuple(items) = &v {
                    if items.len() == 2 {
                        frame.pop()?;
                        // Push reversed so a, b = (1, 2) -> a==1, b==2.
                        frame.push(items[1].clone());
                        frame.push(items[0].clone());
                        specialize::record_hit(op_idx);
                        return Ok(true);
                    }
                }
                self.deopt_unpack_sequence(frame, cache_pc);
                Ok(false)
            }
            IC::UnpackSequenceTuple => {
                let v = frame.top()?.clone();
                if let Object::Tuple(items) = &v {
                    if items.len() == n {
                        frame.pop()?;
                        for x in items.iter().rev() {
                            frame.push(x.clone());
                        }
                        specialize::record_hit(op_idx);
                        return Ok(true);
                    }
                }
                self.deopt_unpack_sequence(frame, cache_pc);
                Ok(false)
            }
            IC::UnpackSequenceList => {
                let v = frame.top()?.clone();
                if let Object::List(items) = &v {
                    let items_borrow = items.borrow();
                    if items_borrow.len() == n {
                        let snapshot: Vec<Object> = items_borrow.iter().cloned().collect();
                        drop(items_borrow);
                        frame.pop()?;
                        for x in snapshot.into_iter().rev() {
                            frame.push(x);
                        }
                        specialize::record_hit(op_idx);
                        return Ok(true);
                    }
                }
                self.deopt_unpack_sequence(frame, cache_pc);
                Ok(false)
            }
            IC::Empty => {
                let receiver = frame.top()?.clone();
                specialize::record_specialize_attempt(op_idx);
                let decision = specialize::attempt_specialize_unpack_sequence(&receiver, n);
                frame.code.caches.set(cache_pc, decision);
                if matches!(decision, IC::Cooldown(_)) {
                    specialize::record_specialize_skip(op_idx);
                } else {
                    specialize::record_specialize_success(op_idx);
                }
                Ok(false)
            }
            IC::Cooldown(n_) => {
                let next = if n_ > 0 {
                    IC::Cooldown(n_ - 1)
                } else {
                    IC::Empty
                };
                frame.code.caches.set(cache_pc, next);
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    /// Deopt an `UNPACK_SEQUENCE` cache.
    #[inline]
    fn deopt_unpack_sequence(&self, frame: &Frame, cache_pc: u32) {
        specialize::record_miss(OpCode::UnpackSequence as u8);
        frame
            .code
            .caches
            .set(cache_pc, weavepy_compiler::InlineCache::Cooldown(COOLDOWN));
    }

    /// Try to compare two container values element-wise via the full
    /// `__eq__` protocol. Returns `None` if either argument is not a
    /// container we recognise — the caller falls back to the
    /// structural `compare_op`.
    fn deep_equal_collection(
        &mut self,
        a: &Object,
        b: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<Option<bool>, RuntimeError> {
        match (a, b) {
            (Object::Tuple(xs), Object::Tuple(ys)) => {
                if xs.len() != ys.len() {
                    return Ok(Some(false));
                }
                for (x, y) in xs.iter().zip(ys.iter()) {
                    if !self.dispatch_compare_op(x, y, CompareKind::Eq, globals)? {
                        return Ok(Some(false));
                    }
                }
                Ok(Some(true))
            }
            (Object::List(xs), Object::List(ys)) => {
                let xs = xs.borrow().clone();
                let ys = ys.borrow().clone();
                if xs.len() != ys.len() {
                    return Ok(Some(false));
                }
                for (x, y) in xs.iter().zip(ys.iter()) {
                    if !self.dispatch_compare_op(x, y, CompareKind::Eq, globals)? {
                        return Ok(Some(false));
                    }
                }
                Ok(Some(true))
            }
            _ => Ok(None),
        }
    }

    fn store_attr(&mut self, obj: &Object, name: &str, value: Object) -> Result<(), RuntimeError> {
        match obj {
            Object::Instance(inst) => self.store_attr_instance(inst, obj, name, value),
            Object::Type(ty) => {
                if ty.flags.is_builtin {
                    return Err(type_error(format!(
                        "cannot set '{name}' attribute of immutable type '{}'",
                        ty.name
                    )));
                }
                ty.dict
                    .borrow_mut()
                    .insert(DictKey(Object::from_str(name)), value);
                Ok(())
            }
            Object::Module(m) => {
                m.dict
                    .borrow_mut()
                    .insert(DictKey(Object::from_str(name)), value);
                Ok(())
            }
            Object::Function(f) => {
                f.attrs
                    .borrow_mut()
                    .insert(DictKey(Object::from_str(name)), value);
                Ok(())
            }
            Object::SimpleNamespace(d) => {
                d.borrow_mut()
                    .insert(DictKey(Object::from_str(name)), value);
                Ok(())
            }
            Object::MappingProxy(_) => Err(type_error(format!(
                "'mappingproxy' object does not support item assignment ('{}')",
                name
            ))),
            Object::Frame(fr) => match name {
                "f_trace" => {
                    *fr.trace.borrow_mut() = value;
                    Ok(())
                }
                "f_trace_lines" | "f_trace_opcodes" => Ok(()),
                "f_lineno" => match value {
                    Object::Int(i) if i >= 0 => {
                        fr.override_lineno.set(Some(i as u32));
                        Ok(())
                    }
                    _ => Err(type_error("f_lineno must be a non-negative int")),
                },
                _ => Err(attribute_error(format!(
                    "'frame' object attribute '{}' is read-only",
                    name
                ))),
            },
            Object::Traceback(tb) => match name {
                "tb_next" => {
                    match value {
                        Object::None => {
                            *tb.next.borrow_mut() = None;
                        }
                        Object::Traceback(next) => {
                            *tb.next.borrow_mut() = Some(next);
                        }
                        other => {
                            return Err(type_error(format!(
                                "expected traceback, got {}",
                                other.type_name()
                            )))
                        }
                    }
                    Ok(())
                }
                _ => Err(attribute_error(format!(
                    "'traceback' object attribute '{}' is read-only",
                    name
                ))),
            },
            _ => Err(type_error(format!(
                "'{}' object has no attribute '{}'",
                obj.type_name(),
                name
            ))),
        }
    }

    /// Attribute set on a user instance. Implements the data-descriptor
    /// path: if the class has a descriptor with `__set__`, dispatch
    /// through it; otherwise enforce slot membership (when the class
    /// declares `__slots__` without `__dict__`); otherwise write to
    /// the instance dict.
    fn store_attr_instance(
        &mut self,
        inst: &Rc<PyInstance>,
        obj: &Object,
        name: &str,
        value: Object,
    ) -> Result<(), RuntimeError> {
        // User-defined __setattr__ on the class overrides everything.
        // We only honour Python-level overrides; the builtin default
        // (`object.__setattr__`) falls through to direct dict writes
        // below to keep the fast path inlineable.
        if let Some(setattr) = inst.class.lookup("__setattr__") {
            if matches!(
                setattr,
                Object::Function(_) | Object::BoundMethod(_) | Object::Instance(_)
            ) {
                self.call(
                    &setattr,
                    &[obj.clone(), Object::from_str(name), value],
                    &[],
                    &self.builtins.clone(),
                )?;
                return Ok(());
            }
        }
        if let Some(attr) = inst.class.lookup(name) {
            match &attr {
                Object::Property(prop) => {
                    if matches!(prop.fset, Object::None) {
                        return Err(attribute_error(format!(
                            "property '{}' of '{}' object has no setter",
                            name, inst.class.name
                        )));
                    }
                    let setter = prop.fset.clone();
                    self.call(&setter, &[obj.clone(), value], &[], &self.builtins.clone())?;
                    return Ok(());
                }
                Object::SlotDescriptor(_) => {
                    inst.dict
                        .borrow_mut()
                        .insert(DictKey(Object::from_str(name)), value);
                    return Ok(());
                }
                Object::Instance(descriptor_inst) => {
                    if let Some(setter) = descriptor_inst.class.lookup("__set__") {
                        let bound = Object::BoundMethod(Rc::new(BoundMethod {
                            receiver: attr.clone(),
                            function: setter,
                        }));
                        self.call(&bound, &[obj.clone(), value], &[], &self.builtins.clone())?;
                        return Ok(());
                    }
                }
                _ => {}
            }
        }
        if inst.class.forbids_dict {
            let slots = inst.class.slot_names.borrow();
            if !slots.iter().any(|s| s == name) {
                return Err(attribute_error(format!(
                    "'{}' object has no attribute '{}'",
                    inst.class.name, name
                )));
            }
        }
        inst.dict
            .borrow_mut()
            .insert(DictKey(Object::from_str(name)), value);
        Ok(())
    }

    fn delete_attr(&mut self, obj: &Object, name: &str) -> Result<(), RuntimeError> {
        match obj {
            Object::Instance(inst) => {
                if let Some(delattr) = inst.class.lookup("__delattr__") {
                    if matches!(
                        delattr,
                        Object::Function(_) | Object::BoundMethod(_) | Object::Instance(_)
                    ) {
                        self.call(
                            &delattr,
                            &[obj.clone(), Object::from_str(name)],
                            &[],
                            &self.builtins.clone(),
                        )?;
                        return Ok(());
                    }
                }
                if let Some(attr) = inst.class.lookup(name) {
                    match &attr {
                        Object::Property(prop) => {
                            if matches!(prop.fdel, Object::None) {
                                return Err(attribute_error(format!(
                                    "property '{}' of '{}' object has no deleter",
                                    name, inst.class.name
                                )));
                            }
                            let deleter = prop.fdel.clone();
                            self.call(
                                &deleter,
                                std::slice::from_ref(obj),
                                &[],
                                &self.builtins.clone(),
                            )?;
                            return Ok(());
                        }
                        Object::Instance(descriptor_inst) => {
                            if let Some(deleter) = descriptor_inst.class.lookup("__delete__") {
                                let bound = Object::BoundMethod(Rc::new(BoundMethod {
                                    receiver: attr.clone(),
                                    function: deleter,
                                }));
                                self.call(
                                    &bound,
                                    std::slice::from_ref(obj),
                                    &[],
                                    &self.builtins.clone(),
                                )?;
                                return Ok(());
                            }
                        }
                        _ => {}
                    }
                }
                if inst
                    .dict
                    .borrow_mut()
                    .shift_remove(&DictKey(Object::from_str(name)))
                    .is_none()
                {
                    return Err(attribute_error(format!(
                        "'{}' object has no attribute '{}'",
                        inst.class.name, name
                    )));
                }
                Ok(())
            }
            _ => Err(type_error(format!(
                "'{}' object has no attribute '{}'",
                obj.type_name(),
                name
            ))),
        }
    }

    fn binary_subscr(&self, container: &Object, index: &Object) -> Result<Object, RuntimeError> {
        // `Type[...]` dispatches to `__class_getitem__` when defined.
        // We can't reach into Vm::call from a `&self` method, so we
        // bail out for now and let the dispatch site handle classes
        // via the dedicated path. Built-in subscript paths below
        // are unchanged.
        if let Object::Type(_) = container {
            // Caller paths that need class subscripting go through
            // `binary_subscr_with_class_dispatch` below.
        }
        self.binary_subscr_basic(container, index)
    }

    fn binary_subscr_basic(
        &self,
        container: &Object,
        index: &Object,
    ) -> Result<Object, RuntimeError> {
        match (container, index) {
            (Object::List(items), Object::Int(i)) => {
                let items = items.borrow();
                let idx = normalize_index(*i, items.len())?;
                Ok(items[idx].clone())
            }
            (Object::Tuple(items), Object::Int(i)) => {
                let idx = normalize_index(*i, items.len())?;
                Ok(items[idx].clone())
            }
            (Object::Str(s), Object::Int(i)) => {
                let chars: Vec<char> = s.chars().collect();
                let idx = normalize_index(*i, chars.len())?;
                Ok(Object::from_str(chars[idx].to_string()))
            }
            (Object::Dict(d), key) => {
                let d = d.borrow();
                d.get(&DictKey(key.clone()))
                    .cloned()
                    .ok_or_else(|| key_error(key.repr()))
            }
            (Object::List(items), Object::Slice(s)) => {
                let items = items.borrow();
                let sliced = slice_seq(&items, s)?;
                Ok(Object::new_list(sliced))
            }
            (Object::Tuple(items), Object::Slice(s)) => {
                let v: Vec<Object> = items.iter().cloned().collect();
                let sliced = slice_seq(&v, s)?;
                Ok(Object::new_tuple(sliced))
            }
            (Object::Str(s), Object::Slice(slc)) => {
                let chars: Vec<char> = s.chars().collect();
                let obj_chars: Vec<Object> = chars
                    .iter()
                    .map(|c| Object::from_str(c.to_string()))
                    .collect();
                let sliced = slice_seq(&obj_chars, slc)?;
                let s: String = sliced.iter().map(|o| o.to_str()).collect();
                Ok(Object::from_str(s))
            }
            (Object::Bytes(buf), Object::Int(i)) => {
                let idx = normalize_index(*i, buf.len())?;
                Ok(Object::Int(i64::from(buf[idx])))
            }
            (Object::ByteArray(buf), Object::Int(i)) => {
                let buf = buf.borrow();
                let idx = normalize_index(*i, buf.len())?;
                Ok(Object::Int(i64::from(buf[idx])))
            }
            (Object::Bytes(buf), Object::Slice(slc)) => {
                let as_objs: Vec<Object> = buf.iter().map(|b| Object::Int(i64::from(*b))).collect();
                let sliced = slice_seq(&as_objs, slc)?;
                let mut out = Vec::with_capacity(sliced.len());
                for o in sliced {
                    match o {
                        Object::Int(i) => out.push(i as u8),
                        _ => return Err(type_error("bytes slice produced non-int")),
                    }
                }
                Ok(Object::Bytes(Rc::from(out.as_slice())))
            }
            (Object::ByteArray(buf), Object::Slice(slc)) => {
                let buf = buf.borrow();
                let as_objs: Vec<Object> = buf.iter().map(|b| Object::Int(i64::from(*b))).collect();
                let sliced = slice_seq(&as_objs, slc)?;
                let mut out = Vec::with_capacity(sliced.len());
                for o in sliced {
                    match o {
                        Object::Int(i) => out.push(i as u8),
                        _ => return Err(type_error("bytearray slice produced non-int")),
                    }
                }
                Ok(Object::ByteArray(Rc::new(RefCell::new(out))))
            }
            (Object::MemoryView(mv), Object::Int(i)) => {
                let bytes = mv.to_bytes();
                let idx = normalize_index(*i, bytes.len())?;
                Ok(Object::Int(i64::from(bytes[idx])))
            }
            (Object::MemoryView(mv), Object::Slice(slc)) => {
                let bytes = mv.to_bytes();
                let as_objs: Vec<Object> =
                    bytes.iter().map(|b| Object::Int(i64::from(*b))).collect();
                let sliced = slice_seq(&as_objs, slc)?;
                let mut out = Vec::with_capacity(sliced.len());
                for o in sliced {
                    match o {
                        Object::Int(i) => out.push(i as u8),
                        _ => return Err(type_error("memoryview slice produced non-int")),
                    }
                }
                Ok(Object::Bytes(Rc::from(out.as_slice())))
            }
            (Object::MappingProxy(d), key) => {
                let d = d.borrow();
                d.get(&DictKey(key.clone()))
                    .cloned()
                    .ok_or_else(|| key_error(key.repr()))
            }
            (Object::SimpleNamespace(d), key) => {
                let d = d.borrow();
                d.get(&DictKey(key.clone()))
                    .cloned()
                    .ok_or_else(|| key_error(key.repr()))
            }
            (_, _) => Err(type_error(format!(
                "'{}' object is not subscriptable with '{}'",
                container.type_name(),
                index.type_name()
            ))),
        }
    }

    fn store_subscr(
        &self,
        container: &Object,
        index: &Object,
        value: Object,
    ) -> Result<(), RuntimeError> {
        match (container, index) {
            (Object::List(items), Object::Int(i)) => {
                let mut items = items.borrow_mut();
                let idx = normalize_index(*i, items.len())?;
                items[idx] = value;
                Ok(())
            }
            (Object::List(items), Object::Slice(s)) => {
                // CPython: `xs[start:stop:step] = iterable`. We
                // collect the RHS, then splice in place. Supporting
                // strided slice assignment requires that `len(rhs)`
                // matches the slice width.
                let replacement = match value {
                    Object::List(l) => l.borrow().clone(),
                    Object::Tuple(t) => t.iter().cloned().collect::<Vec<_>>(),
                    Object::Str(ref txt) => txt
                        .chars()
                        .map(|c| Object::from_str(c.to_string()))
                        .collect(),
                    other => {
                        let mut buf = Vec::new();
                        let mut it = other.make_iter()?;
                        while let Some(v) = it.next_value() {
                            buf.push(v);
                        }
                        buf
                    }
                };
                let mut data = items.borrow_mut();
                apply_slice_assignment(&mut data, s, replacement)?;
                Ok(())
            }
            (Object::Dict(d), key) => {
                d.borrow_mut().insert(DictKey(key.clone()), value);
                Ok(())
            }
            (Object::ByteArray(b), Object::Int(i)) => {
                let mut b = b.borrow_mut();
                let idx = normalize_index(*i, b.len())?;
                let byte = match value {
                    Object::Int(v) if (0..=255).contains(&v) => v as u8,
                    _ => return Err(value_error("byte must be in 0..256")),
                };
                b[idx] = byte;
                Ok(())
            }
            _ => Err(type_error(format!(
                "'{}' object does not support item assignment",
                container.type_name()
            ))),
        }
    }

    fn delete_subscr(&self, container: &Object, index: &Object) -> Result<(), RuntimeError> {
        match (container, index) {
            (Object::List(items), Object::Int(i)) => {
                let mut items = items.borrow_mut();
                let idx = normalize_index(*i, items.len())?;
                items.remove(idx);
                Ok(())
            }
            (Object::Dict(d), key) => {
                if d.borrow_mut().shift_remove(&DictKey(key.clone())).is_none() {
                    return Err(key_error(key.repr()));
                }
                Ok(())
            }
            _ => Err(type_error(format!(
                "'{}' object does not support item deletion",
                container.type_name()
            ))),
        }
    }

    // ---------- calling ----------

    fn call(
        &mut self,
        callable: &Object,
        args: &[Object],
        kwargs: &[(String, Object)],
        outer_globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let _ = outer_globals;
        match callable {
            Object::Builtin(b) => {
                if b.name == builtins::BUILD_CLASS_NAME {
                    return self.build_class(args, kwargs);
                }
                // `type.__new__(mcs, name, bases, ns)` — invoked from
                // user metaclasses via `super().__new__(...)`. Route
                // to the actual type-construction path so the class
                // ends up wired with the right metaclass.
                if b.name == "__new__" && args.len() == 4 {
                    if let Object::Type(mcs) = &args[0] {
                        if mcs.is_subclass_of(&builtin_types().type_) {
                            return self.dynamic_type_call_with_meta(
                                mcs.clone(),
                                &args[1..],
                                kwargs,
                            );
                        }
                    }
                }
                if b.name == "print" {
                    return self.do_print(args, kwargs, outer_globals);
                }
                if b.name == "str" && args.len() == 1 {
                    return self.do_str_call(&args[0], outer_globals);
                }
                if b.name == "repr" && args.len() == 1 {
                    return self.do_repr_call(&args[0], outer_globals);
                }
                if b.name == "len" && args.len() == 1 {
                    return self.do_len_call(&args[0], outer_globals);
                }
                if b.name == "int" && args.len() <= 2 {
                    return self.do_int_call(args, outer_globals);
                }
                if b.name == "float" && args.len() <= 1 {
                    return self.do_float_call(args, outer_globals);
                }
                if b.name == "next" && (args.len() == 1 || args.len() == 2) {
                    return self.do_next_call(args, outer_globals);
                }
                if b.name == "iter" && args.len() == 1 {
                    return self.do_iter_call(&args[0], outer_globals);
                }
                if b.name == "iter" && args.len() == 2 {
                    // ``iter(callable, sentinel)`` — return a small
                    // VM-aware iterator that re-invokes ``callable``
                    // each step. Modelled as a Python-side
                    // generator so the existing FOR_ITER /
                    // for-loop machinery just works.
                    return self.do_iter_callable_sentinel(args, outer_globals);
                }
                if (b.name == "list" || b.name == "tuple") && args.len() == 1 {
                    return self.do_list_or_tuple_call(b.name, &args[0], outer_globals);
                }
                if b.name == "dict" && args.len() == 1 && kwargs.is_empty() {
                    if let Some(d) = self.try_dict_from_mapping(&args[0], outer_globals)? {
                        return Ok(d);
                    }
                }
                if b.name == "sum" {
                    return self.do_sum_call(args, outer_globals);
                }
                if b.name == "max" || b.name == "min" {
                    return self.do_min_max_call(b.name, args, kwargs, outer_globals);
                }
                if b.name == "any" || b.name == "all" {
                    return self.do_any_all_call(b.name, args, outer_globals);
                }
                if b.name == "isinstance" && args.len() == 2 {
                    return self.do_isinstance_call(&args[0], &args[1], outer_globals);
                }
                if b.name == "issubclass" && args.len() == 2 {
                    return self.do_issubclass_call(&args[0], &args[1], outer_globals);
                }
                if b.name == "hash" && args.len() == 1 {
                    return self.do_hash_call(&args[0], outer_globals);
                }
                if b.name == "globals" && args.is_empty() && kwargs.is_empty() {
                    // CPython returns the calling function's module
                    // globals. With our frame-by-argument model, the
                    // active frame's globals are whatever the caller
                    // is currently executing inside — and that's
                    // exactly `outer_globals`.
                    return Ok(Object::Dict(outer_globals.clone()));
                }
                if b.name == "locals" && args.is_empty() && kwargs.is_empty() {
                    // CPython returns a dict of the locals visible
                    // in the calling frame. At module / class /
                    // exec scope this *is* the module dict; in
                    // function scope it's a fresh dict snapshot of
                    // the locals.
                    if let Some(top) = self.frame_stack.borrow().last() {
                        return Ok(top.locals());
                    }
                    return Ok(Object::Dict(outer_globals.clone()));
                }
                if b.name == "breakpoint" {
                    return self.do_breakpoint_call(args, kwargs, outer_globals);
                }
                let _ = kwargs;
                if b.name == "__vm:input" {
                    return self.do_input_call(args, outer_globals);
                }
                if b.name == "__vm:__import__" {
                    // ``__import__(name, globals=None, locals=None,
                    //              fromlist=(), level=0)`` — mirror
                    // CPython's signature. We honour the first, fourth
                    // and fifth arguments; ``globals`` is used for
                    // package resolution but we pass through the
                    // calling frame's globals which is more
                    // useful for relative imports.
                    let name = match args.first() {
                        Some(Object::Str(s)) => s.to_string(),
                        _ => return Err(type_error("__import__() argument 1 must be str")),
                    };
                    let fromlist = args.get(3).cloned().unwrap_or(Object::None);
                    let level = match args.get(4) {
                        Some(Object::Int(i)) => *i as u32,
                        Some(Object::None) | None => 0,
                        _ => return Err(type_error("__import__() argument 5 (level) must be int")),
                    };
                    return self.do_import(&name, &fromlist, level, outer_globals);
                }
                if b.name == "__vm:compile" {
                    return self.do_compile_call(args, outer_globals);
                }
                if b.name == "__vm:exec" {
                    return self.do_exec_call(args, outer_globals);
                }
                if b.name == "__vm:eval" {
                    return self.do_eval_call(args, outer_globals);
                }
                // RFC 0024: `gc.collect()` is a Rust BuiltinFn that
                // queues `__del__` finalizers but can't run them
                // (no interpreter handle). Intercept here so we can
                // drain the queue synchronously, matching CPython
                // semantics where `gc.collect()` returns *after*
                // every finaliser has fired.
                if b.name == ".gc.collect" {
                    let result = (b.call)(args)?;
                    self.run_pending_finalizers();
                    return Ok(result);
                }
                // Pre-materialize generator/instance iterables for
                // builtin methods that need to iterate them. The
                // underlying static builtins call `Object::make_iter`
                // directly, which can't drive a Python generator.
                if matches!(b.name, "join" | "extend") && args.len() == 2 {
                    if matches!(&args[1], Object::Generator(_) | Object::Instance(_)) {
                        let collected = self.collect_iterable(&args[1], outer_globals)?;
                        let new_args = vec![args[0].clone(), Object::new_list(collected)];
                        return (b.call)(&new_args);
                    }
                }
                if b.name == "sorted" && !args.is_empty() {
                    return self.do_sorted_call(args, kwargs, outer_globals);
                }
                if b.name == "sort" && !args.is_empty() {
                    return self.do_list_sort_call(args, kwargs, outer_globals);
                }
                if (b.name == "min" || b.name == "max") && !args.is_empty() {
                    return self.do_min_max_call(b.name, args, kwargs, outer_globals);
                }
                // ``re.sub(pat, repl, text, count=0, flags=0)``
                // accepts a callable ``repl``; routing it through the
                // VM lets the callback invoke arbitrary user code.
                if b.name == "sub" && args.len() >= 3 {
                    let callable_repl = matches!(
                        args.get(1),
                        Some(Object::Function(_))
                            | Some(Object::Builtin(_))
                            | Some(Object::BoundMethod(_))
                    );
                    if callable_repl {
                        return self.do_re_sub_callable(args, outer_globals);
                    }
                }
                // `format`'s dispatching: when args[0] is a string we
                // assume this is `"...".format(...)` (str_format
                // builtin) and pass kwargs through. Otherwise fall
                // back to the global builtin `format(value, spec)`.
                if b.name == "format" {
                    if matches!(args.first(), Some(Object::Str(_))) && !args.is_empty() {
                        let template = match &args[0] {
                            Object::Str(s) => s.to_string(),
                            _ => unreachable!(),
                        };
                        let rest = &args[1..];
                        return self
                            .do_str_format(&template, rest, kwargs, outer_globals)
                            .map(Object::from_str);
                    }
                    if args.len() == 1 || args.len() == 2 {
                        let spec = match args.get(1) {
                            Some(Object::Str(s)) => s.to_string(),
                            None => String::new(),
                            Some(_) => return Err(type_error("format() spec must be a string")),
                        };
                        return Ok(Object::from_str(format_via_spec(&args[0], &spec)?));
                    }
                }
                if let Some(call_kw) = b.call_kw.as_ref() {
                    return call_kw(args, kwargs);
                }
                if !kwargs.is_empty() {
                    return Err(type_error(format!(
                        "builtin '{}' does not accept keyword arguments",
                        b.name
                    )));
                }
                (b.call)(args)
            }
            Object::Function(f) => self.call_python(f, args, kwargs),
            Object::BoundMethod(bm) => {
                // Generator / coroutine / async-generator methods are
                // wired through internal builtin names so the
                // dispatcher can see them here and run them with
                // interpreter access. (Plain `Builtin.call` is a
                // `fn(&[Object])` and can't.)
                if let Object::Builtin(b) = &bm.function {
                    match b.name {
                        ".gen_send" => {
                            let value = args.first().cloned().unwrap_or(Object::None);
                            return self.gen_method_send(&bm.receiver, value);
                        }
                        ".gen_throw" => {
                            return self.gen_method_throw(&bm.receiver, args);
                        }
                        ".gen_close" => {
                            return self.gen_method_close(&bm.receiver);
                        }
                        ".gen_next" => {
                            return self.gen_method_send(&bm.receiver, Object::None);
                        }
                        ".gen_iter" => {
                            return Ok(bm.receiver.clone());
                        }
                        // --- async generator methods ---------------------
                        // `__aiter__` returns the agen itself.
                        ".agen_aiter" => return Ok(bm.receiver.clone()),
                        // `__anext__` returns the agen wrapped as a
                        // coroutine-shaped awaitable: when driven via
                        // SEND, it forwards to the underlying generator
                        // and translates StopIteration into
                        // StopAsyncIteration so async-for can terminate.
                        ".agen_anext" => match &bm.receiver {
                            Object::AsyncGenerator(_) => return Ok(bm.receiver.clone()),
                            other => {
                                return Err(type_error(format!(
                                    "__anext__ requires an async_generator, got '{}'",
                                    other.type_name()
                                )))
                            }
                        },
                        ".agen_send" => {
                            let value = args.first().cloned().unwrap_or(Object::None);
                            return self.gen_method_send(&bm.receiver, value);
                        }
                        ".agen_throw" => {
                            return self.gen_method_throw(&bm.receiver, args);
                        }
                        ".agen_close" => {
                            return self.gen_method_close(&bm.receiver);
                        }
                        _ => {}
                    }
                }
                let mut combined: Vec<Object> = Vec::with_capacity(args.len() + 1);
                combined.push(bm.receiver.clone());
                combined.extend_from_slice(args);
                self.call(&bm.function, &combined, kwargs, outer_globals)
            }
            Object::Type(ty) => {
                // CPython routes `str(x)` / `repr(x)` through dunders;
                // intercept the built-in classes here so that the
                // user's `__str__` / `__repr__` wins over the default
                // type constructor.
                if ty.flags.is_builtin && args.len() == 1 && kwargs.is_empty() {
                    if ty.name == "str" {
                        return self.do_str_call(&args[0], outer_globals);
                    }
                    if ty.name == "repr" {
                        return self.do_repr_call(&args[0], outer_globals);
                    }
                }
                // `type(name, bases, ns)` builds a new class dynamically.
                if Rc::ptr_eq(ty, &builtin_types().type_) && args.len() == 3 {
                    return self.dynamic_type_call_with_meta(ty.clone(), args, kwargs);
                }
                // `Meta(name, bases, ns)` for a user metaclass —
                // route through the metaclass-aware class builder.
                let bt = builtin_types();
                if ty.is_subclass_of(&bt.type_) && !Rc::ptr_eq(ty, &bt.type_) && args.len() == 3 {
                    return self.dynamic_type_call_with_meta(ty.clone(), args, kwargs);
                }
                // If the class's *metaclass* overrides `__call__`,
                // dispatch through it so EnumMeta etc. can hook
                // calls like `Color(3)`.
                let meta = ty.metaclass_or_type();
                if !Rc::ptr_eq(&meta, &bt.type_) {
                    if let Some(call_method) = meta.lookup("__call__") {
                        let bound = Object::BoundMethod(Rc::new(BoundMethod {
                            receiver: Object::Type(ty.clone()),
                            function: call_method,
                        }));
                        return self.call(&bound, args, kwargs, outer_globals);
                    }
                }
                self.instantiate(ty.clone(), args, kwargs)
            }
            Object::Instance(inst) => {
                // Honour __call__ if defined.
                if let Some(m) = inst.class.lookup("__call__") {
                    let bound = Object::BoundMethod(Rc::new(BoundMethod {
                        receiver: Object::Instance(inst.clone()),
                        function: m,
                    }));
                    self.call(&bound, args, kwargs, outer_globals)
                } else {
                    Err(type_error(format!(
                        "'{}' object is not callable",
                        inst.class.name
                    )))
                }
            }
            _ => Err(type_error(format!(
                "'{}' object is not callable",
                callable.type_name()
            ))),
        }
    }

    /// Run a `class` statement.
    ///
    /// `args[0]` is the class body function, `args[1]` is the class
    /// name, and the rest are explicit bases. Keyword arguments carry
    /// `metaclass=` (used to pick a custom metaclass) and any other
    /// class-creation keywords forwarded to `__init_subclass__`.
    fn build_class(
        &mut self,
        args: &[Object],
        kwargs: &[(String, Object)],
    ) -> Result<Object, RuntimeError> {
        if args.len() < 2 {
            return Err(type_error("__build_class__ takes at least 2 args"));
        }
        let body_fn = match &args[0] {
            Object::Function(f) => f.clone(),
            _ => return Err(type_error("__build_class__ arg 1 must be a function")),
        };
        let name = match &args[1] {
            Object::Str(s) => s.to_string(),
            _ => return Err(type_error("__build_class__ arg 2 must be a str")),
        };
        let mut bases: Vec<Rc<TypeObject>> = Vec::new();
        for b in &args[2..] {
            match b {
                Object::Type(t) => bases.push(t.clone()),
                other => {
                    return Err(type_error(format!(
                        "base of class '{}' must be a class, got '{}'",
                        name,
                        other.type_name()
                    )));
                }
            }
        }
        if bases.is_empty() {
            bases.push(builtin_types().object_.clone());
        }

        // Pull `metaclass=` out of kwargs; the rest are passed to
        // `__init_subclass__` (matching CPython's PEP 487 rules).
        let mut metaclass_arg: Option<Rc<TypeObject>> = None;
        let mut subclass_kwargs: Vec<(String, Object)> = Vec::new();
        for (k, v) in kwargs {
            if k == "metaclass" {
                if let Object::Type(t) = v {
                    metaclass_arg = Some(t.clone());
                } else {
                    return Err(type_error("metaclass= must be a type"));
                }
            } else {
                subclass_kwargs.push((k.clone(), v.clone()));
            }
        }

        // Determine the effective metaclass: explicit `metaclass=`
        // beats anything inherited; otherwise pick the most-derived
        // metaclass of any base.
        let metaclass = resolve_metaclass(metaclass_arg, &bases)?;

        let class_ns = Rc::new(RefCell::new(DictData::new()));
        {
            let mut ns = class_ns.borrow_mut();
            ns.insert(
                DictKey(Object::from_static("__name__")),
                Object::from_str(&name),
            );
            ns.insert(
                DictKey(Object::from_static("__qualname__")),
                Object::from_str(&name),
            );
            // Stamp `__module__` so `pickle` (and any user code that
            // introspects classes) can find the qualified name. We
            // copy whatever `globals['__name__']` is at definition
            // time, which is `__main__` for top-level classes and the
            // module name for everything else.
            if let Some(module_name) = body_fn
                .globals
                .borrow()
                .get(&DictKey(Object::from_static("__name__")))
                .cloned()
            {
                ns.insert(DictKey(Object::from_static("__module__")), module_name);
            }
        }
        // Build a frame for the class body. Locals are unused; names
        // store and load through `class_ns`. The body's `__class__`
        // cell is captured so methods can reference it.
        let code = body_fn.code.clone();
        let mut frame = self.make_frame(
            code,
            Vec::new(),
            body_fn.closure.clone(),
            body_fn.globals.clone(),
            false,
        );
        frame.class_namespace = Some(class_ns.clone());
        let _ = self.run_frame(&mut frame)?;

        // Dispatch through the metaclass: this also runs any
        // user-defined `__new__` / `__init__` chain (EnumMeta uses
        // __new__ for member processing, ABCMeta uses __init__).
        let bt = builtin_types();
        let is_plain_type = Rc::ptr_eq(&metaclass, &bt.type_);
        let ty = if is_plain_type {
            let dict = class_ns.borrow().clone();
            let ty = TypeObject::new_user(&name, bases.clone(), dict)?;
            ty.set_metaclass(metaclass.clone());
            self.finalize_class_namespace(&ty)?;
            self.invoke_set_name_hooks(&ty)?;
            self.invoke_init_subclass(&ty, &subclass_kwargs)?;
            ty
        } else {
            // Custom metaclass: route through `metaclass(name, bases, ns)`.
            // The metaclass's `__new__` (if any) chains into
            // `type.__new__`, which we intercept via
            // `dynamic_type_call_with_meta` to actually build the type.
            let bases_tuple =
                Object::new_tuple(bases.iter().map(|b| Object::Type(b.clone())).collect());
            let ns_dict = Object::Dict(class_ns.clone());
            let call_args = vec![Object::from_str(&name), bases_tuple, ns_dict];
            // Run the metaclass's __new__ first if it defines one;
            // otherwise fall through to the default class construction.
            let new_method = metaclass.lookup("__new__");
            // Detect whether the resolved __new__ is the sentinel
            // `type.__new__` we install at startup (which would
            // recurse) — in that case skip the call and build the
            // class directly via `dynamic_type_call_with_meta`.
            let is_type_new_sentinel = matches!(
                new_method.as_ref(),
                Some(Object::StaticMethod(inner)) if matches!(
                    inner.as_ref(),
                    Object::Builtin(b) if b.name == "__new__"
                )
            );
            let class_obj = match &new_method {
                Some(_) if !is_type_new_sentinel => {
                    let callable = match new_method.as_ref().unwrap() {
                        Object::StaticMethod(inner) => (**inner).clone(),
                        Object::ClassMethod(inner) => (**inner).clone(),
                        other => other.clone(),
                    };
                    let mut new_args = Vec::with_capacity(call_args.len() + 1);
                    new_args.push(Object::Type(metaclass.clone()));
                    new_args.extend(call_args.iter().cloned());
                    self.call(
                        &callable,
                        &new_args,
                        &subclass_kwargs,
                        &Rc::new(RefCell::new(DictData::new())),
                    )?
                }
                _ => self.dynamic_type_call_with_meta(
                    metaclass.clone(),
                    &call_args,
                    &subclass_kwargs,
                )?,
            };
            let ty = match class_obj {
                Object::Type(t) => t,
                other => {
                    return Err(type_error(format!(
                        "metaclass.__new__ must return a type, got '{}'",
                        other.type_name()
                    )))
                }
            };
            // Run `__init__` if a user `__new__` was used (the
            // dynamic_type_call_with_meta path already invokes
            // __init__ when it falls through to the default).
            if let Some(new_fn) = new_method.as_ref() {
                if !is_type_new_sentinel {
                    let _ = new_fn;
                    if let Some(init) = metaclass.lookup("__init__") {
                        let bound = Object::BoundMethod(Rc::new(BoundMethod {
                            receiver: Object::Type(ty.clone()),
                            function: init,
                        }));
                        let _ = self.call(
                            &bound,
                            &call_args,
                            &subclass_kwargs,
                            &Rc::new(RefCell::new(DictData::new())),
                        )?;
                    }
                }
            }
            ty
        };

        // If the body produced a `__class__` cell (because a method
        // references super or __class__), point it at the new type.
        for (i, cell_name) in body_fn.code.cellvars.iter().enumerate() {
            if cell_name == "__class__" {
                if let Some(cell) = frame.cells.get(i) {
                    *cell.borrow_mut() = Object::Type(ty.clone());
                }
            }
        }
        Ok(Object::Type(ty))
    }

    /// `metaclass(name, bases, ns)` — the three-arg form that
    /// builds a new class. Used by `type(name, bases, ns)`, by
    /// custom metaclasses, and by the build_class path when the
    /// metaclass's `__new__` chains back into `type.__new__`.
    fn dynamic_type_call_with_meta(
        &mut self,
        metaclass: Rc<TypeObject>,
        args: &[Object],
        kwargs: &[(String, Object)],
    ) -> Result<Object, RuntimeError> {
        let name = match &args[0] {
            Object::Str(s) => s.to_string(),
            _ => return Err(type_error("type() arg 1 must be str")),
        };
        let bases: Vec<Rc<TypeObject>> = match &args[1] {
            Object::Tuple(items) => items
                .iter()
                .map(|b| match b {
                    Object::Type(t) => Ok(t.clone()),
                    other => Err(type_error(format!(
                        "type() arg 2 entry must be a class, got '{}'",
                        other.type_name()
                    ))),
                })
                .collect::<Result<_, _>>()?,
            _ => return Err(type_error("type() arg 2 must be tuple of bases")),
        };
        let ns_dict_obj = args[2].clone();
        let ns = match &args[2] {
            Object::Dict(d) => d.borrow().clone(),
            _ => return Err(type_error("type() arg 3 must be a dict")),
        };
        let mut effective_bases = bases.clone();
        if effective_bases.is_empty() {
            effective_bases.push(builtin_types().object_.clone());
        }
        let bt = builtin_types();
        let is_plain_type = Rc::ptr_eq(&metaclass, &bt.type_);

        // Separate `metaclass=` and per-class-creation kwargs (the
        // latter flow to `__init_subclass__` and to the metaclass's
        // `__init__`).
        let subclass_kwargs: Vec<(String, Object)> = kwargs
            .iter()
            .filter(|(k, _)| k != "metaclass")
            .cloned()
            .collect();

        let ty = TypeObject::new_user(&name, effective_bases.clone(), ns)?;
        ty.set_metaclass(metaclass.clone());
        self.finalize_class_namespace(&ty)?;

        // If we're under a user metaclass, run its `__init__` so it
        // can mutate the class (member registration in EnumMeta,
        // abstract-method tracking in ABCMeta).
        if !is_plain_type {
            if let Some(init) = metaclass.lookup("__init__") {
                let bound = Object::BoundMethod(Rc::new(BoundMethod {
                    receiver: Object::Type(ty.clone()),
                    function: init,
                }));
                let bases_tuple = Object::new_tuple(
                    effective_bases
                        .iter()
                        .map(|b| Object::Type(b.clone()))
                        .collect(),
                );
                let _ = self.call(
                    &bound,
                    &[Object::from_str(&name), bases_tuple, ns_dict_obj],
                    &subclass_kwargs,
                    &Rc::new(RefCell::new(DictData::new())),
                )?;
            }
        }

        self.invoke_set_name_hooks(&ty)?;
        self.invoke_init_subclass(&ty, &subclass_kwargs)?;
        Ok(Object::Type(ty))
    }

    /// Post-process a freshly built class dict: lift `__slots__`
    /// into descriptors, and propagate the `forbids_dict` flag from
    /// the MRO.
    fn finalize_class_namespace(&mut self, ty: &Rc<TypeObject>) -> Result<(), RuntimeError> {
        // Pull __slots__ out if present.
        let slots_obj = ty
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("__slots__")))
            .cloned();
        if let Some(slots) = slots_obj {
            let names = match &slots {
                Object::Str(s) => vec![s.to_string()],
                Object::Tuple(_) | Object::List(_) => {
                    let mut out = Vec::new();
                    let mut it = slots.make_iter()?;
                    while let Some(v) = it.next_value() {
                        if let Object::Str(s) = v {
                            out.push(s.to_string());
                        } else {
                            return Err(type_error("__slots__ items must be str"));
                        }
                    }
                    out
                }
                _ => return Err(type_error("__slots__ must be a tuple/list/str")),
            };
            let allows_dict_in_slots = names.iter().any(|s| s == "__dict__");
            *ty.slot_names.borrow_mut() = names.clone();
            // Install slot descriptors for each name (skipping
            // `__dict__` and `__weakref__` which are marker names).
            for slot_name in &names {
                if slot_name == "__dict__" || slot_name == "__weakref__" {
                    continue;
                }
                let desc = Object::SlotDescriptor(Rc::new(crate::object::SlotDescriptor {
                    name: slot_name.clone(),
                    class_name: ty.name.clone(),
                }));
                ty.dict
                    .borrow_mut()
                    .insert(DictKey(Object::from_str(slot_name)), desc);
            }
            // If the slot list omits __dict__ AND no base allows
            // arbitrary attrs (i.e. every base also forbids dict),
            // mark this class as forbidding instance __dict__.
            let bases_all_forbid = ty.bases.iter().all(|b| {
                if Rc::ptr_eq(b, &builtin_types().object_) {
                    return true;
                }
                b.forbids_dict
            });
            if !allows_dict_in_slots && bases_all_forbid {
                // SAFETY: we own the only `Rc<TypeObject>` reference
                // before installing the class anywhere; mutating
                // `forbids_dict` is fine because no other code path
                // observes it yet.
                let raw = Rc::as_ptr(ty).cast_mut();
                // SAFETY: see comment above; no aliasing reads in flight.
                unsafe { (*raw).forbids_dict = true };
            }
        }
        Ok(())
    }

    /// Invoke `__set_name__(cls, name)` for every descriptor in the
    /// freshly-built class namespace that defines it. PEP 487.
    fn invoke_set_name_hooks(&mut self, ty: &Rc<TypeObject>) -> Result<(), RuntimeError> {
        let entries: Vec<(String, Object)> = ty
            .dict
            .borrow()
            .iter()
            .filter_map(|(k, v)| match &k.0 {
                Object::Str(s) => Some((s.to_string(), v.clone())),
                _ => None,
            })
            .collect();
        for (attr_name, value) in entries {
            if let Object::Instance(inst) = &value {
                if let Some(hook) = inst.class.lookup("__set_name__") {
                    let bound = Object::BoundMethod(Rc::new(BoundMethod {
                        receiver: value.clone(),
                        function: hook,
                    }));
                    let _ = self.call(
                        &bound,
                        &[Object::Type(ty.clone()), Object::from_str(&attr_name)],
                        &[],
                        &self.builtins.clone(),
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Run `__init_subclass__` for the first base in MRO order that
    /// defines it (excluding the new class itself). PEP 487.
    fn invoke_init_subclass(
        &mut self,
        ty: &Rc<TypeObject>,
        subclass_kwargs: &[(String, Object)],
    ) -> Result<(), RuntimeError> {
        // Snapshot the MRO and bind the hook into a local first so we
        // can drop every borrow before re-entering the VM. The user
        // hook is free to mutate any class in the chain.
        let mro_bases: Vec<Rc<TypeObject>> = ty.mro.borrow().iter().skip(1).cloned().collect();
        let mut hook: Option<Object> = None;
        for base in &mro_bases {
            if let Some(h) = base
                .dict
                .borrow()
                .get(&DictKey(Object::from_static("__init_subclass__")))
                .cloned()
            {
                hook = Some(h);
                break;
            }
        }
        let Some(hook) = hook else {
            return Ok(());
        };
        // CPython treats __init_subclass__ as an implicit classmethod
        // regardless of how it was defined.
        let callable = match hook {
            Object::ClassMethod(inner) => (*inner).clone(),
            other => other,
        };
        let bound = Object::BoundMethod(Rc::new(BoundMethod {
            receiver: Object::Type(ty.clone()),
            function: callable,
        }));
        self.call(
            &bound,
            &[],
            subclass_kwargs,
            &Rc::new(RefCell::new(DictData::new())),
        )?;
        Ok(())
    }

    /// Allocate an instance of `cls`, then run the `__new__` /
    /// `__init__` two-phase initialisation. The descriptor protocol
    /// gives us classmethod binding for `__new__` for free.
    fn instantiate(
        &mut self,
        cls: Rc<TypeObject>,
        args: &[Object],
        kwargs: &[(String, Object)],
    ) -> Result<Object, RuntimeError> {
        // Built-in conversion types route to the underlying builtin
        // function so `int("3")`, `range(5)`, `list(xs)` keep working.
        if cls.flags.is_builtin {
            // Descriptor wrapper classes (property/staticmethod/
            // classmethod) — route to dedicated constructors.
            match cls.name.as_str() {
                "property" => {
                    if !kwargs.is_empty() {
                        // CPython accepts `fget=`, `fset=`, etc.,
                        // but we keep the keyword form simple here.
                        return Err(type_error(
                            "property() takes positional arguments only here",
                        ));
                    }
                    return builtins::construct_property(args);
                }
                "staticmethod" => {
                    return builtins::construct_staticmethod(args);
                }
                "classmethod" => {
                    return builtins::construct_classmethod(args);
                }
                _ => {}
            }
            // Special-case `list(it)` / `tuple(it)` so generators flow
            // through the VM-aware collector.
            if (cls.name == "list" || cls.name == "tuple") && args.len() == 1 {
                let global_dummy = Rc::new(RefCell::new(DictData::new()));
                return self.do_list_or_tuple_call(cls.name.as_str(), &args[0], &global_dummy);
            }
            // CPython's `dict(obj)` accepts a mapping (anything with
            // `keys()`); recognise that path before falling through to
            // the simple "iter of pairs" builtin.
            if cls.name == "dict" && args.len() == 1 && kwargs.is_empty() {
                let global_dummy = Rc::new(RefCell::new(DictData::new()));
                if let Some(d) = self.try_dict_from_mapping(&args[0], &global_dummy)? {
                    return Ok(d);
                }
            }
            // `int(x)` / `float(x)` honour the user's `__int__` /
            // `__float__` when `x` is a non-primitive — matches CPython.
            if cls.name == "int" && args.len() <= 2 && kwargs.is_empty() {
                let global_dummy = Rc::new(RefCell::new(DictData::new()));
                return self.do_int_call(args, &global_dummy);
            }
            if cls.name == "float" && args.len() <= 1 && kwargs.is_empty() {
                let global_dummy = Rc::new(RefCell::new(DictData::new()));
                return self.do_float_call(args, &global_dummy);
            }
            if let Some(builtin) = self.builtin_constructor_for(&cls) {
                if !kwargs.is_empty() {
                    return Err(type_error(format!(
                        "{}() does not accept keyword arguments",
                        cls.name
                    )));
                }
                return (builtin.call)(args);
            }
            if cls.flags.is_exception {
                let instance = self.build_exception_instance(cls.clone(), args);
                // If a class anywhere between `cls` and `BaseException`
                // (exclusive) defines its own `__init__`, run it so
                // subclasses such as `BaseExceptionGroup` get to stitch
                // `exceptions` onto the instance. We stop at
                // `BaseException` because its default `__init__` only
                // populates `args` — which the fast path already did.
                if let Some(init) = lookup_exception_init(&cls) {
                    let bound = Object::BoundMethod(Rc::new(BoundMethod {
                        receiver: instance.clone(),
                        function: init,
                    }));
                    let result = self.call(
                        &bound,
                        args,
                        kwargs,
                        &Rc::new(RefCell::new(DictData::new())),
                    )?;
                    if !matches!(result, Object::None) {
                        return Err(type_error(format!(
                            "__init__() should return None, not '{}'",
                            result.type_name()
                        )));
                    }
                }
                return Ok(instance);
            }
        }

        // `__new__` chain: walk the MRO; the first base that defines a
        // user `__new__` (other than the implicit `object.__new__`)
        // owns instance allocation. If none is found, fall back to the
        // default allocator.
        let new_fn = cls.lookup("__new__");
        let is_object_new = matches!(
            &new_fn,
            Some(Object::StaticMethod(inner))
                if matches!(
                    inner.as_ref(),
                    Object::Builtin(b) if b.name == "__new__"
                )
        );
        let instance = match new_fn {
            Some(_) if !is_object_new => {
                // User-defined `__new__` — pass cls + args + kwargs.
                let callable = match new_fn.unwrap() {
                    Object::StaticMethod(inner) => (*inner).clone(),
                    Object::ClassMethod(inner) => (*inner).clone(),
                    other => other,
                };
                let mut new_args: Vec<Object> = Vec::with_capacity(args.len() + 1);
                new_args.push(Object::Type(cls.clone()));
                new_args.extend_from_slice(args);
                self.call(
                    &callable,
                    &new_args,
                    kwargs,
                    &Rc::new(RefCell::new(DictData::new())),
                )?
            }
            _ => {
                let inst = Object::Instance(Rc::new(PyInstance::new(cls.clone())));
                // RFC 0024: auto-track every fresh user instance with
                // the cycle collector. CPython does the same for any
                // type whose `tp_traverse` is non-NULL — for us that's
                // every Python-defined class (they all carry a dict).
                gc_trace::track(inst.clone());
                inst
            }
        };

        // Only run `__init__` when `type(instance) is cls`, matching
        // CPython. If `__new__` returned something else, leave it
        // alone (this is how `int.__new__` etc. work for immutable
        // subclasses).
        let init_eligible = match &instance {
            Object::Instance(inst) => Rc::ptr_eq(&inst.class, &cls),
            // Built-in `__new__` returns may not be Instance; in that
            // case don't run __init__ — the caller meant to bypass.
            _ => false,
        };
        if init_eligible {
            if let Some(init) = cls.lookup("__init__") {
                // CPython rule: if `__new__` is overridden and
                // `__init__` is the default `object.__init__`, skip
                // the `__init__` call entirely so user code can pass
                // arbitrary args to `__new__` without tripping on
                // `object.__init__()`'s strict arity.
                let init_owner_is_object = init_is_from_object(&cls);
                if init_owner_is_object && !is_object_new {
                    return Ok(instance);
                }
                let bound = Object::BoundMethod(Rc::new(BoundMethod {
                    receiver: instance.clone(),
                    function: init,
                }));
                let result = self.call(
                    &bound,
                    args,
                    kwargs,
                    &Rc::new(RefCell::new(DictData::new())),
                )?;
                if !matches!(result, Object::None) {
                    return Err(type_error(format!(
                        "__init__() should return None, not '{}'",
                        result.type_name()
                    )));
                }
            } else if !args.is_empty() || !kwargs.is_empty() {
                // Inherit `object()` semantics: object()-without-args
                // is fine; any args trigger `TypeError`.
                let inherits_only_object_init = cls
                    .mro
                    .borrow()
                    .iter()
                    .skip(1)
                    .all(|t| Rc::ptr_eq(t, &builtin_types().object_));
                if inherits_only_object_init {
                    return Err(type_error(format!("{}() takes no arguments", cls.name)));
                }
            }
        }
        Ok(instance)
    }

    /// Construct a built-in exception instance carrying `args` as the
    /// canonical Python `.args` tuple. Used by both `raise` and
    /// explicit `ExceptionClass(...)` calls.
    fn build_exception_instance(&self, cls: Rc<TypeObject>, args: &[Object]) -> Object {
        let inst = PyInstance::new(cls);
        let args_tuple = Object::new_tuple(args.to_vec());
        let mut dict = inst.dict.borrow_mut();
        dict.insert(DictKey(Object::from_static("args")), args_tuple);
        if let Some(first) = args.first() {
            dict.insert(DictKey(Object::from_static("message")), first.clone());
        }
        drop(dict);
        Object::Instance(Rc::new(inst))
    }

    /// Look up the existing built-in callable that mirrors `cls`'s
    /// constructor — `int`, `range`, `list`, etc.
    fn builtin_constructor_for(&self, cls: &TypeObject) -> Option<Rc<BuiltinFn>> {
        let key = DictKey(Object::from_str(&cls.name));
        match self.builtins.borrow().get(&key).cloned() {
            Some(Object::Builtin(b)) => Some(b),
            _ => None,
        }
    }

    fn call_python(
        &mut self,
        f: &Rc<PyFunction>,
        args: &[Object],
        kwargs: &[(String, Object)],
    ) -> Result<Object, RuntimeError> {
        let code = f.code.clone();
        let total_args = code.arg_count as usize;
        let has_varargs = code.has_varargs;
        let has_varkeywords = code.has_varkeywords;
        // Bind positional args; remainder go to *args if present, else error.
        let mut positional: Vec<Object> = vec![Object::None; code.varnames.len()];
        let mut filled = vec![false; code.varnames.len()];
        let provided = args.len();
        let direct = provided.min(total_args);
        for (i, v) in args.iter().take(direct).enumerate() {
            positional[i] = v.clone();
            filled[i] = true;
        }
        if has_varargs {
            let star_idx = total_args;
            let rest: Vec<Object> = args.iter().skip(direct).cloned().collect();
            positional[star_idx] = Object::new_tuple(rest);
            filled[star_idx] = true;
        } else if provided > total_args {
            return Err(type_error(format!(
                "{}() takes {} positional arguments but {} were given",
                f.name, total_args, provided
            )));
        }
        // Keyword args: match by name. Unmatched ones go into the
        // `**kwargs` dict if the function declares one; otherwise we
        // raise the usual TypeError. Defaults are applied AFTER
        // kwargs so an explicit `arg=` always wins over the default.
        // Keyword-binding range = positional params + kwonly params.
        // *args/**kwargs sit just outside this range and can't be
        // addressed by keyword. Locals beyond it MUST NOT pull the
        // kwarg out of the **kwargs catchall.
        let kwonly_count = code.kwonly_count as usize;
        let kwonly_start = total_args + usize::from(has_varargs);
        let kwonly_end = kwonly_start + kwonly_count;
        let kwargs_slot = if has_varkeywords {
            Some(kwonly_end)
        } else {
            None
        };
        let mut extra_kwargs = crate::object::DictData::new();
        for (name, value) in kwargs {
            let mut slot = None;
            if let Some(p) = code
                .varnames
                .iter()
                .take(total_args)
                .position(|n| n == name)
            {
                slot = Some(p);
            } else if let Some(p) = code
                .varnames
                .get(kwonly_start..kwonly_end)
                .and_then(|range| range.iter().position(|n| n == name))
            {
                slot = Some(kwonly_start + p);
            }
            match slot {
                Some(slot) => {
                    if filled[slot] {
                        return Err(type_error(format!(
                            "{}() got multiple values for argument '{}'",
                            f.name, name
                        )));
                    }
                    positional[slot] = value.clone();
                    filled[slot] = true;
                }
                None => {
                    if kwargs_slot.is_some() {
                        extra_kwargs.insert(
                            crate::object::DictKey(Object::from_str(name.clone())),
                            value.clone(),
                        );
                    } else {
                        return Err(type_error(format!(
                            "{}() got an unexpected keyword argument '{}'",
                            f.name, name
                        )));
                    }
                }
            }
        }
        if let Some(slot) = kwargs_slot {
            positional[slot] = Object::Dict(Rc::new(RefCell::new(extra_kwargs)));
            filled[slot] = true;
        }
        // Defaults plug remaining holes among positional args. CPython
        // attaches positional defaults right-aligned to the param
        // list (so `def f(a, b=1, c=2)` has `defaults = (1, 2)`).
        if filled.iter().take(total_args).any(|x| !x) {
            let needed = total_args;
            let first_default = needed.saturating_sub(f.defaults.len());
            for (i, d) in f.defaults.iter().enumerate() {
                let slot = first_default + i;
                if slot < needed && !filled[slot] {
                    positional[slot] = d.clone();
                    filled[slot] = true;
                }
            }
        }
        // Then plug kwonly defaults by name.
        for (name, default) in &f.kw_defaults {
            if let Some(p) = code
                .varnames
                .get(kwonly_start..kwonly_end)
                .and_then(|range| range.iter().position(|n| n == name))
            {
                let slot = kwonly_start + p;
                if !filled[slot] {
                    positional[slot] = default.clone();
                    filled[slot] = true;
                }
            }
        }
        for (i, was_filled) in filled.iter().take(total_args).enumerate() {
            if !was_filled {
                return Err(type_error(format!(
                    "{}() missing required argument: '{}'",
                    f.name, code.varnames[i]
                )));
            }
        }
        for (i, was_filled) in filled
            .iter()
            .enumerate()
            .skip(kwonly_start)
            .take(kwonly_end - kwonly_start)
        {
            if !was_filled {
                return Err(type_error(format!(
                    "{}() missing required keyword-only argument: '{}'",
                    f.name, code.varnames[i]
                )));
            }
        }
        let mut frame = self.make_frame(
            code.clone(),
            positional,
            f.closure.clone(),
            f.globals.clone(),
            false,
        );
        if code.is_generator || code.is_coroutine || code.is_async_generator {
            // Run the bootstrap so the frame is past
            // `RETURN_GENERATOR; POP_TOP; RESUME`. We then wrap the
            // frame in a PyGenerator and hand it back to the caller.
            match self.run_until_yield_or_return(&mut frame, None)? {
                FrameOutcome::StartGenerator => {
                    let gen = Rc::new(PyGenerator::new(f.name.clone(), Box::new(frame)));
                    if code.is_coroutine {
                        Ok(Object::Coroutine(gen))
                    } else if code.is_async_generator {
                        Ok(Object::AsyncGenerator(gen))
                    } else {
                        Ok(Object::Generator(gen))
                    }
                }
                FrameOutcome::Returned(_) | FrameOutcome::Yielded(_) => {
                    Err(RuntimeError::Internal(
                        "generator bootstrap did not stop at RETURN_GENERATOR".to_owned(),
                    ))
                }
            }
        } else {
            self.run_frame(&mut frame)
        }
    }

    // ---------- imports (RFC 0012) ----------

    /// `IMPORT_NAME` runtime side. Resolves relative imports against
    /// the current frame's `__package__`/`__name__`, walks dotted
    /// names, and returns either the top-level package (when
    /// `fromlist` is empty/None) or the leaf module (otherwise).
    /// Compile a Python source string into a `code` object. Mirrors
    /// CPython's signature `compile(source, filename, mode)`; the
    /// `flags`/`dont_inherit`/`optimize` arguments are accepted but
    /// ignored — they don't change WeavePy's bytecode.
    /// `breakpoint(*args, **kwargs)` — RFC 0023. Honours
    /// `sys.breakpointhook`; if unset (the default), falls back to
    /// printing a hint about how WeavePy debugging works without
    /// actually entering pdb. Real pdb integration requires
    /// `bdb`/`pdb` modules which are shipped as frozen Python.
    fn do_breakpoint_call(
        &mut self,
        args: &[Object],
        kwargs: &[(String, Object)],
        outer_globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let sys_key = DictKey(Object::from_static("sys"));
        let sys_module = self.cache.modules.borrow().get(&sys_key).cloned();
        if let Some(Object::Module(sys_mod)) = sys_module {
            let hook = sys_mod
                .dict
                .borrow()
                .get(&DictKey(Object::from_static("breakpointhook")))
                .cloned();
            if let Some(hook) = hook {
                if !matches!(hook, Object::None) {
                    return self.call(&hook, args, kwargs, outer_globals);
                }
            }
        }
        let import_result = self.do_import("pdb", &Object::None, 0, outer_globals);
        if let Ok(Object::Module(pdb)) = import_result {
            if let Some(set_trace) = pdb
                .dict
                .borrow()
                .get(&DictKey(Object::from_static("set_trace")))
                .cloned()
            {
                return self.call(&set_trace, args, kwargs, outer_globals);
            }
        }
        eprintln!("breakpoint() called but no debugger is attached.");
        Ok(Object::None)
    }

    /// `input([prompt])` — read a line from stdin. Honours
    /// `sys.stdin`/`sys.stdout` when wired; falls back to the host
    /// `std::io::stdin()`.
    fn do_input_call(
        &mut self,
        args: &[Object],
        _outer_globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        use std::io::Write;
        if let Some(prompt) = args.first() {
            let s = prompt.to_str();
            print!("{}", s);
            let _ = std::io::stdout().flush();
        }
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) => Err(crate::error::RuntimeError::PyException(
                crate::error::PyException::new(crate::builtin_types::make_exception_with_class(
                    crate::builtin_types::builtin_types().eof_error.clone(),
                    "EOF when reading a line",
                )),
            )),
            Ok(_) => {
                if line.ends_with('\n') {
                    line.pop();
                    if line.ends_with('\r') {
                        line.pop();
                    }
                }
                Ok(Object::from_str(line))
            }
            Err(e) => Err(crate::error::os_error(e.to_string())),
        }
    }

    fn do_compile_call(
        &mut self,
        args: &[Object],
        _outer_globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let source = match args.first() {
            Some(Object::Str(s)) => s.to_string(),
            Some(Object::Bytes(b)) => String::from_utf8_lossy(b).into_owned(),
            _ => {
                return Err(type_error(
                    "compile() argument 1 must be a string or bytes-like",
                ))
            }
        };
        let filename = match args.get(1) {
            Some(Object::Str(s)) => s.to_string(),
            _ => "<string>".to_owned(),
        };
        let mode = match args.get(2) {
            Some(Object::Str(s)) => s.to_string(),
            _ => "exec".to_owned(),
        };
        match mode.as_str() {
            "exec" => {
                let module = weavepy_parser::parse_module(&source)
                    .map_err(|e| crate::error::value_error(format!("compile error: {e}")))?;
                let code =
                    weavepy_compiler::compile_module_with_source(&module, &source, &filename)
                        .map_err(|e| crate::error::value_error(format!("compile error: {e}")))?;
                Ok(Object::Code(Rc::new(code)))
            }
            "eval" => {
                let module = weavepy_parser::parse_module(&source)
                    .map_err(|e| crate::error::value_error(format!("compile error: {e}")))?;
                let code =
                    weavepy_compiler::compile_module_with_source(&module, &source, &filename)
                        .map_err(|e| crate::error::value_error(format!("compile error: {e}")))?;
                Ok(Object::Code(Rc::new(code)))
            }
            other => Err(crate::error::value_error(format!(
                "compile() mode must be 'exec' or 'eval', not '{other}'"
            ))),
        }
    }

    /// `exec(source, globals=None, locals=None)`. Accepts either a
    /// `Code` object (the typical CPython use case) or a Python source
    /// string we compile on the fly. The body runs with `globals`
    /// taking the place of the calling frame's globals, exactly the
    /// way frozen modules execute.
    fn do_exec_call(
        &mut self,
        args: &[Object],
        outer_globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let source = args
            .first()
            .cloned()
            .ok_or_else(|| type_error("exec() missing required argument 'source'"))?;
        let globals_dict = match args.get(1) {
            Some(Object::Dict(d)) => d.clone(),
            Some(Object::None) | None => outer_globals.clone(),
            _ => return Err(type_error("exec() globals must be a dict")),
        };
        let code_rc = match source {
            Object::Code(c) => c,
            Object::Str(src) => {
                let module = weavepy_parser::parse_module(&src)
                    .map_err(|e| crate::error::value_error(format!("exec error: {e}")))?;
                let compiled =
                    weavepy_compiler::compile_module_with_source(&module, &src, "<string>")
                        .map_err(|e| crate::error::value_error(format!("exec error: {e}")))?;
                Rc::new(compiled)
            }
            other => {
                return Err(type_error(format!(
                    "exec() expected str or code, got {}",
                    other.type_name()
                )))
            }
        };
        // Ensure the globals dict carries a `__builtins__` entry so
        // user code can reach `print`, `range`, etc. Mirrors how
        // module globals are seeded by the import path.
        {
            let mut g = globals_dict.borrow_mut();
            if !g.contains_key(&DictKey(Object::from_static("__builtins__"))) {
                g.insert(
                    DictKey(Object::from_static("__builtins__")),
                    Object::Dict(self.builtins.clone()),
                );
            }
        }
        let mut frame = self.make_frame(code_rc, Vec::new(), Vec::new(), globals_dict, true);
        self.run_frame(&mut frame)?;
        Ok(Object::None)
    }

    /// `eval(expr, globals=None, locals=None)`. Accepts a `Code`
    /// object or a source string of a single expression. Returns the
    /// expression's value rather than `None`.
    fn do_eval_call(
        &mut self,
        args: &[Object],
        outer_globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let source = args
            .first()
            .cloned()
            .ok_or_else(|| type_error("eval() missing required argument 'source'"))?;
        let globals_dict = match args.get(1) {
            Some(Object::Dict(d)) => d.clone(),
            Some(Object::None) | None => outer_globals.clone(),
            _ => return Err(type_error("eval() globals must be a dict")),
        };
        let src = match source {
            Object::Code(c) => {
                let mut frame =
                    self.make_frame(c, Vec::new(), Vec::new(), globals_dict.clone(), true);
                self.run_frame(&mut frame)?;
                return Ok(Object::None);
            }
            Object::Str(s) => s.to_string(),
            other => {
                return Err(type_error(format!(
                    "eval() expected str or code, got {}",
                    other.type_name()
                )))
            }
        };
        // Wrap as a single-expression module: parse the source, then
        // synthesize a `_result_ = <expr>` statement so the value is
        // captured in the globals dict.
        let wrapped = format!("__weavepy_eval_result = ({})\n", src);
        let module = weavepy_parser::parse_module(&wrapped)
            .map_err(|e| crate::error::value_error(format!("eval error: {e}")))?;
        let code = weavepy_compiler::compile_module_with_source(&module, &wrapped, "<eval>")
            .map_err(|e| crate::error::value_error(format!("eval error: {e}")))?;
        {
            let mut g = globals_dict.borrow_mut();
            if !g.contains_key(&DictKey(Object::from_static("__builtins__"))) {
                g.insert(
                    DictKey(Object::from_static("__builtins__")),
                    Object::Dict(self.builtins.clone()),
                );
            }
        }
        let mut frame = self.make_frame(
            Rc::new(code),
            Vec::new(),
            Vec::new(),
            globals_dict.clone(),
            true,
        );
        self.run_frame(&mut frame)?;
        let result = globals_dict
            .borrow()
            .get(&DictKey(Object::from_static("__weavepy_eval_result")))
            .cloned()
            .unwrap_or(Object::None);
        globals_dict
            .borrow_mut()
            .shift_remove(&DictKey(Object::from_static("__weavepy_eval_result")));
        Ok(result)
    }

    fn do_import(
        &mut self,
        name: &str,
        fromlist: &Object,
        level: u32,
        current_globals: &Rc<RefCell<DictData>>,
    ) -> Result<Object, RuntimeError> {
        let package = current_package(current_globals);
        let absolute = resolve_relative(package.as_deref(), name, level).map_err(import_error)?;
        let leaf = self.import_path(&absolute)?;

        // CPython: with no fromlist, return the top-level package.
        // Otherwise return the leaf module — and pre-load any items
        // listed in `fromlist` that look like submodules (so that
        // `from pkg import sub` triggers loading of `pkg.sub`).
        let want_leaf = !matches!(fromlist, Object::None);
        if !want_leaf {
            let top_name = absolute.split('.').next().unwrap_or(&absolute);
            return self
                .cache
                .get(top_name)
                .ok_or_else(|| module_not_found_error(format!("import of '{top_name}' failed")));
        }
        if let Object::Tuple(items) = fromlist {
            for item in items.iter() {
                if let Object::Str(s) = item {
                    if s.as_ref() == "*" {
                        continue;
                    }
                    let sub_name = format!("{absolute}.{s}");
                    let _ = self.import_path(&sub_name);
                }
            }
        }
        Ok(leaf)
    }

    /// Walk a dotted name (`a.b.c`), loading each part lazily and
    /// linking submodules into their parents' dicts. Returns the
    /// leaf module.
    pub fn import_path(&mut self, full: &str) -> Result<Object, RuntimeError> {
        let parts: Vec<&str> = full.split('.').collect();
        let mut so_far = String::new();
        let mut current: Option<Object> = None;
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                so_far.push('.');
            }
            so_far.push_str(part);
            let module = self.load_one(&so_far)?;
            if let Some(Object::Module(parent_mod)) = current.as_ref() {
                parent_mod
                    .dict
                    .borrow_mut()
                    .insert(DictKey(Object::from_str(*part)), module.clone());
            }
            current = Some(module);
        }
        current.ok_or_else(|| import_error("empty module name"))
    }

    /// Load a single fully-qualified module name. Honours the cache
    /// first, then the built-in registry, then frozen Python sources,
    /// then the filesystem.
    fn load_one(&mut self, full: &str) -> Result<Object, RuntimeError> {
        if let Some(cached) = self.cache.get(full) {
            return Ok(cached);
        }
        if let Some(factory) = self.cache.builtin_factory(full) {
            let module = factory(&self.cache);
            let obj = Object::Module(module);
            self.cache.insert(full, obj.clone());
            return Ok(obj);
        }
        if let Some(frozen) = self.cache.frozen_source(full) {
            // RFC 0021 — frozen modules pay a parse + compile cost
            // on every fresh `Interpreter::new()` (tests, the REPL,
            // and the bench harness all spin up many). A
            // process-global cache keyed on the static name lets
            // the *second* and subsequent interpreters skip both
            // stages and go straight from `&'static str` source to
            // a fully-compiled `CodeObject`.
            if let Some(code) = frozen_code_cache::get(full) {
                return self.run_frozen_compiled(full, code, frozen.is_package, "<frozen>");
            }
            return self.load_from_source(full, frozen.source, frozen.is_package, "<frozen>");
        }
        // RFC 0022 — try the C-extension loader before the source
        // loader. We invoke it through a hook to keep the
        // dependency one-way (`weavepy-capi` depends on `weavepy-vm`,
        // not the reverse): the binary registers a callback before
        // any user code runs.
        if let Some(loader) = ext_loader::current_extension_loader() {
            if let Some(loaded) = loader(self, full)? {
                self.cache.insert(full, loaded.clone());
                return Ok(loaded);
            }
        }
        if let Some((path, is_package)) = self.cache.find_source(full) {
            return self.load_from_file(full, &path, is_package);
        }
        // PEP 420 — namespace packages. If we found one or more
        // directories named `full` on `sys.path` without an
        // `__init__.py`, construct a namespace package: a module
        // whose `__path__` lists the contributing directories and
        // whose `__name__` is the dotted name.
        let ns_dirs = self.cache.find_namespace_package(full);
        if !ns_dirs.is_empty() {
            let pkg_for_globals = full.rsplit_once('.').map(|(p, _)| p.to_owned());
            let globals = self.build_module_globals(full, None, pkg_for_globals.as_deref());
            {
                let mut g = globals.borrow_mut();
                g.insert(
                    crate::object::DictKey(Object::from_static("__path__")),
                    Object::new_list(
                        ns_dirs
                            .iter()
                            .map(|d| Object::from_str(d.to_string_lossy().into_owned()))
                            .collect(),
                    ),
                );
                g.insert(
                    crate::object::DictKey(Object::from_static("__file__")),
                    Object::None,
                );
                g.insert(
                    crate::object::DictKey(Object::from_static("__spec__")),
                    Object::None,
                );
            }
            let module_obj = Object::Module(Rc::new(PyModule {
                name: full.to_owned(),
                filename: None,
                dict: globals,
            }));
            self.cache.insert(full, module_obj.clone());
            return Ok(module_obj);
        }
        Err(module_not_found_error(format!("No module named '{full}'")))
    }

    /// Compile and execute Python source provided as a string. Used
    /// for frozen stdlib modules; shares the post-parse path with
    /// `load_from_file`.
    fn load_from_source(
        &mut self,
        full: &str,
        source: &str,
        is_package: bool,
        filename: &str,
    ) -> Result<Object, RuntimeError> {
        let module = weavepy_parser::parse_module(source)
            .map_err(|e| import_error(format!("parse error in '{full}': {e}")))?;
        let code = weavepy_compiler::compile_module_with_source(&module, source, filename)
            .map_err(|e| import_error(format!("compile error in '{full}': {e}")))?;
        // RFC 0021 — populate the process-global frozen cache so the
        // *next* interpreter in this process skips parse + compile.
        // We cache only the compiled code, never the running module
        // — module *state* is interpreter-local (different
        // `sys.modules`, different `__name__`).
        if filename == "<frozen>" {
            frozen_code_cache::insert(full, &code);
        }
        self.run_frozen_compiled(full, code, is_package, filename)
    }

    /// Shared tail for "compile a module in this VM and run it" —
    /// used both by the source path and by the cache-hit path that
    /// skips the parse + compile stages.
    fn run_frozen_compiled(
        &mut self,
        full: &str,
        code: weavepy_compiler::CodeObject,
        is_package: bool,
        filename: &str,
    ) -> Result<Object, RuntimeError> {
        let package = if is_package {
            full.to_owned()
        } else {
            full.rsplit_once('.')
                .map_or(String::new(), |(p, _)| p.to_owned())
        };
        let pkg_for_globals = if package.is_empty() {
            None
        } else {
            Some(package.as_str())
        };
        let globals = self.build_module_globals(full, Some(filename), pkg_for_globals);
        let module_obj = Object::Module(Rc::new(PyModule {
            name: full.to_owned(),
            filename: Some(filename.to_owned()),
            dict: globals.clone(),
        }));
        self.cache.insert(full, module_obj.clone());
        let code_rc = Rc::new(code);
        let mut frame = self.make_frame(code_rc, Vec::new(), Vec::new(), globals, true);
        if let Err(e) = self.run_frame(&mut frame) {
            self.cache.remove(full);
            return Err(e);
        }
        Ok(module_obj)
    }

    /// Read, parse, compile, and execute the module's source.
    /// The module is inserted into `sys.modules` *before* its body
    /// runs so that circular imports observe a partially-initialised
    /// module instead of looping.
    ///
    /// PEP 3147 / 0020: a `__pycache__/<name>.<cache_tag>.pyc`
    /// sibling is consulted before parsing. On a healthy hit we
    /// unmarshal directly to a `CodeObject` and skip the front-end
    /// entirely; on a miss we compile and write a fresh cache
    /// (subject to `-B` / `PYTHONDONTWRITEBYTECODE`).
    fn load_from_file(
        &mut self,
        full: &str,
        path: &Path,
        is_package: bool,
    ) -> Result<Object, RuntimeError> {
        let filename = path.to_string_lossy().into_owned();
        let (code, source_for_diag) = if let Some(cached) = crate::pycache::try_load(path) {
            (cached, String::new())
        } else {
            let source = std::fs::read_to_string(path)
                .map_err(|e| import_error(format!("failed to read '{}': {e}", path.display())))?;
            let module = weavepy_parser::parse_module(&source)
                .map_err(|e| import_error(format!("parse error in '{}': {e}", path.display())))?;
            let code = weavepy_compiler::compile_module_with_source(&module, &source, &filename)
                .map_err(|e| import_error(format!("compile error in '{}': {e}", path.display())))?;
            if !self.bytecode_writes_disabled() {
                crate::pycache::try_write(path, &code);
            }
            (code, source)
        };
        let _ = source_for_diag;

        // Build the module value first and seed sys.modules so that
        // circular imports see a stub.
        let package = if is_package {
            full.to_owned()
        } else {
            full.rsplit_once('.')
                .map_or(String::new(), |(p, _)| p.to_owned())
        };
        let pkg_for_globals = if package.is_empty() {
            None
        } else {
            Some(package.as_str())
        };
        let globals = self.build_module_globals(full, Some(&filename), pkg_for_globals);
        if is_package {
            globals.borrow_mut().insert(
                DictKey(Object::from_static("__path__")),
                package_search_path(path),
            );
        }
        let module_obj = Object::Module(Rc::new(PyModule {
            name: full.to_owned(),
            filename: Some(filename.clone()),
            dict: globals.clone(),
        }));
        self.cache.insert(full, module_obj.clone());

        // Run the body. On failure, drop the partial module so a
        // subsequent retry can try again from scratch.
        let code_rc = Rc::new(code);
        let mut frame = self.make_frame(code_rc, Vec::new(), Vec::new(), globals, true);
        if let Err(e) = self.run_frame(&mut frame) {
            self.cache.remove(full);
            return Err(e);
        }
        Ok(module_obj)
    }

    /// `IMPORT_FROM` runtime side. Looks up `name` on the module on
    /// top of the stack, returning the attribute or
    /// `ImportError("cannot import name 'name' from 'module'")`.
    fn import_from(&mut self, module: &Object, name: &str) -> Result<Object, RuntimeError> {
        match module {
            Object::Module(m) => {
                if let Some(v) = m.dict.borrow().get(&DictKey(Object::from_str(name))) {
                    return Ok(v.clone());
                }
                // Submodule that we deferred loading: try loading it
                // on demand. Matches CPython's `_handle_fromlist`.
                let candidate = format!("{}.{}", m.name, name);
                if self.cache.get(&candidate).is_some()
                    || self.cache.builtin_factory(&candidate).is_some()
                    || self.cache.frozen_source(&candidate).is_some()
                    || self.cache.find_source(&candidate).is_some()
                {
                    let sub = self.load_one(&candidate)?;
                    m.dict
                        .borrow_mut()
                        .insert(DictKey(Object::from_str(name)), sub.clone());
                    return Ok(sub);
                }
                Err(import_error(format!(
                    "cannot import name '{name}' from '{}'",
                    m.name
                )))
            }
            other => Err(type_error(format!(
                "IMPORT_FROM on non-module: '{}'",
                other.type_name()
            ))),
        }
    }

    /// `IMPORT_STAR` runtime side. Copies every public name from the
    /// module into the current globals. Honours `__all__` if defined.
    fn import_star(
        &mut self,
        module: &Object,
        globals: &Rc<RefCell<DictData>>,
    ) -> Result<(), RuntimeError> {
        let m = match module {
            Object::Module(m) => m,
            other => {
                return Err(type_error(format!(
                    "IMPORT_STAR on non-module: '{}'",
                    other.type_name()
                )))
            }
        };
        let dict = m.dict.borrow();
        if let Some(Object::List(all_list)) = dict.get(&DictKey(Object::from_static("__all__"))) {
            let names: Vec<String> = all_list
                .borrow()
                .iter()
                .filter_map(|o| match o {
                    Object::Str(s) => Some(s.to_string()),
                    _ => None,
                })
                .collect();
            let mut g = globals.borrow_mut();
            for n in names {
                if let Some(v) = dict.get(&DictKey(Object::from_str(&n))) {
                    g.insert(DictKey(Object::from_str(n)), v.clone());
                }
            }
            return Ok(());
        }
        let mut g = globals.borrow_mut();
        for (k, v) in dict.iter() {
            let name = match &k.0 {
                Object::Str(s) => s.to_string(),
                _ => continue,
            };
            if name.starts_with('_') {
                continue;
            }
            g.insert(DictKey(Object::from_str(name)), v.clone());
        }
        Ok(())
    }
}

/// Read the current module's `__package__` (or fall back to
/// `__name__`'s parent) so relative imports can resolve.
fn current_package(globals: &Rc<RefCell<DictData>>) -> Option<String> {
    let dict = globals.borrow();
    if let Some(Object::Str(p)) = dict.get(&DictKey(Object::from_static("__package__"))) {
        let s = p.to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }
    if let Some(Object::Str(n)) = dict.get(&DictKey(Object::from_static("__name__"))) {
        let s = n.to_string();
        if let Some((parent, _)) = s.rsplit_once('.') {
            return Some(parent.to_owned());
        }
    }
    None
}

fn apply_slice_assignment(
    data: &mut Vec<Object>,
    s: &PySlice,
    replacement: Vec<Object>,
) -> Result<(), RuntimeError> {
    let len = data.len() as i64;
    let step = match &s.step {
        Object::None => 1i64,
        Object::Int(i) => *i,
        _ => return Err(type_error("slice indices must be integers or None")),
    };
    if step == 0 {
        return Err(value_error("slice step cannot be zero"));
    }
    let extract = |o: &Object, default: i64| -> Result<i64, RuntimeError> {
        match o {
            Object::None => Ok(default),
            Object::Int(i) => Ok(*i),
            _ => Err(type_error("slice indices must be integers or None")),
        }
    };
    let start_raw = extract(&s.start, if step > 0 { 0 } else { len - 1 })?;
    let stop_raw = extract(&s.stop, if step > 0 { len } else { -1 })?;
    let norm = |x: i64| -> i64 {
        if x < 0 {
            ((x + len).max(0)).min(len)
        } else {
            x.min(len)
        }
    };
    if step == 1 {
        let start = norm(start_raw).max(0) as usize;
        let stop = norm(stop_raw).max(start as i64) as usize;
        data.splice(start..stop, replacement);
        return Ok(());
    }
    // Strided assignment: build the list of indices first, then
    // verify the lengths match before applying.
    let mut indices: Vec<usize> = Vec::new();
    let mut i = if step > 0 {
        norm(start_raw)
    } else {
        if start_raw < 0 {
            (start_raw + len).max(-1)
        } else {
            start_raw.min(len - 1)
        }
    };
    let stop = if step > 0 {
        norm(stop_raw)
    } else if stop_raw < 0 {
        (stop_raw + len).max(-1)
    } else {
        stop_raw.min(len)
    };
    while (step > 0 && i < stop) || (step < 0 && i > stop) {
        if i >= 0 && (i as usize) < data.len() {
            indices.push(i as usize);
        }
        i += step;
    }
    if indices.len() != replacement.len() {
        return Err(value_error(format!(
            "attempt to assign sequence of size {} to extended slice of size {}",
            replacement.len(),
            indices.len()
        )));
    }
    for (slot, value) in indices.into_iter().zip(replacement) {
        data[slot] = value;
    }
    Ok(())
}

fn slice_seq(seq: &[Object], s: &PySlice) -> Result<Vec<Object>, RuntimeError> {
    let len = seq.len() as i64;
    let step = match &s.step {
        Object::None => 1i64,
        Object::Int(i) => *i,
        _ => {
            return Err(type_error(
                "slice indices must be integers or None or have an __index__ method",
            ))
        }
    };
    if step == 0 {
        return Err(value_error("slice step cannot be zero"));
    }
    let extract = |o: &Object, default: i64| -> Result<i64, RuntimeError> {
        match o {
            Object::None => Ok(default),
            Object::Int(i) => Ok(*i),
            _ => Err(type_error(
                "slice indices must be integers or None or have an __index__ method",
            )),
        }
    };
    let start = extract(&s.start, if step > 0 { 0 } else { len - 1 })?;
    let stop = extract(&s.stop, if step > 0 { len } else { -1 })?;
    let norm = |x: i64| -> i64 {
        if x < 0 {
            let n = x + len;
            if n < 0 && step > 0 {
                0
            } else {
                n
            }
        } else if x > len {
            len
        } else {
            x
        }
    };
    let mut i = norm(start);
    let stop_norm = norm(stop);
    let mut out = Vec::new();
    if step > 0 {
        while i < stop_norm {
            if (0..len).contains(&i) {
                out.push(seq[i as usize].clone());
            }
            i += step;
        }
    } else {
        while i > stop_norm {
            if (0..len).contains(&i) {
                out.push(seq[i as usize].clone());
            }
            i += step;
        }
    }
    Ok(out)
}

fn path_contains(path: &[Object], needle: &str) -> bool {
    path.iter()
        .any(|o| matches!(o, Object::Str(s) if s.as_ref() == needle))
}

fn normalize_index(i: i64, len: usize) -> Result<usize, RuntimeError> {
    let n = len as i64;
    let idx = if i < 0 { i + n } else { i };
    if idx < 0 || idx >= n {
        return Err(index_error("index out of range"));
    }
    Ok(idx as usize)
}

/// Outcome of executing a single instruction.
enum StepOutcome {
    Continue,
    Return(Object),
    /// `YIELD_VALUE` suspended the frame. The value is the yielded
    /// object; the frame's `pc` already points past `YIELD_VALUE`.
    Yield(Object),
    /// `RETURN_GENERATOR` ran at the top of a generator body. The
    /// caller should wrap this frame in a [`PyGenerator`] and return
    /// the wrapped object instead of continuing execution.
    StartGenerator,
}

/// Outcome of running a frame to its next suspension point.
enum FrameOutcome {
    Returned(Object),
    Yielded(Object),
    StartGenerator,
}

/// If `obj` is an instance and its class defines `name`, return the
/// bound method. Used by dunder dispatch to avoid the full
/// `load_attr` codepath (and the AttributeError if missing).
/// Resolve the effective metaclass for a new class given an explicit
/// `metaclass=` keyword (if any) and the list of explicit bases.
///
/// Matches CPython's `type.__new__` rule: the chosen metaclass must
/// be a (possibly equal) subclass of every base's metaclass, with the
/// explicit `metaclass=` keyword acting as the seed when present.
fn resolve_metaclass(
    explicit: Option<Rc<TypeObject>>,
    bases: &[Rc<TypeObject>],
) -> Result<Rc<TypeObject>, RuntimeError> {
    let bt = builtin_types();
    let mut winner: Rc<TypeObject> = explicit.unwrap_or_else(|| bt.type_.clone());
    for b in bases {
        let m = b.metaclass_or_type();
        if winner.is_subclass_of(&m) {
            continue;
        }
        if m.is_subclass_of(&winner) {
            winner = m;
            continue;
        }
        return Err(type_error(
            "metaclass conflict: the metaclass of a derived class must be a (non-strict) \
             subclass of the metaclasses of all its bases",
        ));
    }
    Ok(winner)
}

fn instance_method(obj: &Object, name: &str) -> Option<Object> {
    let inst = match obj {
        Object::Instance(i) => i.clone(),
        _ => return None,
    };
    let m = inst.class.lookup(name)?;
    Some(Object::BoundMethod(Rc::new(BoundMethod {
        receiver: Object::Instance(inst),
        function: m,
    })))
}

/// Return a fresh empty globals dict — used by the awaitable
/// dispatch paths that don't have a frame's globals handy. The
/// dispatched method itself carries its own `__globals__`.
fn fallback_globals() -> Rc<RefCell<DictData>> {
    Rc::new(RefCell::new(DictData::new()))
}

/// `True` when `o` is a `StopAsyncIteration` instance (or one of
/// its subclasses).
fn is_stop_async_iteration_obj(o: &Object) -> bool {
    if let Object::Instance(inst) = o {
        let target = builtin_types().stop_async_iteration.clone();
        return inst.class.is_subclass_of(&target);
    }
    false
}

/// Walk `cls`'s MRO until we hit `BaseException` (exclusive). If any
/// class in that prefix carries its own `__init__`, return it.
/// Otherwise the caller can stick with the cheap `args`-only setup.
fn lookup_exception_init(cls: &Rc<TypeObject>) -> Option<Object> {
    let mro = cls.mro.borrow();
    for ty in mro.iter() {
        if ty.name == "BaseException" || ty.name == "object" {
            return None;
        }
        let dict = ty.dict.borrow();
        if let Some(init) = dict.get(&DictKey(Object::from_static("__init__"))) {
            return Some(init.clone());
        }
    }
    None
}

/// `True` if `cls`'s `__init__` is the one defined on `object` — i.e.
/// no class between `cls` and `object` overrides `__init__`. Used to
/// decide whether to skip the default no-op `__init__` after a user-
/// defined `__new__` already consumed the constructor args.
fn init_is_from_object(cls: &Rc<TypeObject>) -> bool {
    let mro = cls.mro.borrow();
    for ty in mro.iter() {
        let dict = ty.dict.borrow();
        if dict.contains_key(&DictKey(Object::from_static("__init__"))) {
            return ty.name == "object";
        }
    }
    false
}

/// Build the `Object::BoundMethod` returned by
/// `<gen>.send` / `.throw` / `.close` / `.__next__` / `.__iter__`.
/// The actual dispatch is handled by [`Interpreter::call`] via the
/// special name prefix `.gen_*`.
fn make_gen_method(name: &str, receiver: &Object) -> Object {
    fn unreachable_call(_args: &[Object]) -> Result<Object, RuntimeError> {
        Err(RuntimeError::Internal(
            "generator method must be dispatched via Interpreter::call".to_owned(),
        ))
    }
    let internal_name: &'static str = match name {
        "send" => ".gen_send",
        "throw" => ".gen_throw",
        "close" => ".gen_close",
        "__next__" => ".gen_next",
        "__iter__" | "__await__" => ".gen_iter",
        "__aiter__" => ".agen_aiter",
        "__anext__" => ".agen_anext",
        "asend" => ".agen_send",
        "athrow" => ".agen_throw",
        "aclose" => ".agen_close",
        _ => ".gen_unknown",
    };
    let builtin = Object::Builtin(Rc::new(BuiltinFn {
        name: internal_name,
        call: Box::new(unreachable_call),
        call_kw: None,
    }));
    Object::BoundMethod(Rc::new(BoundMethod {
        receiver: receiver.clone(),
        function: builtin,
    }))
}

/// Look up the `value` attribute on a `StopIteration` instance. Falls
/// back to `None` if absent.
fn exception_value(instance: &Object) -> Object {
    if let Object::Instance(inst) = instance {
        if let Some(v) = inst
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("value")))
        {
            return v.clone();
        }
        if let Some(Object::Tuple(items)) = inst
            .dict
            .borrow()
            .get(&DictKey(Object::from_static("args")))
        {
            if let Some(first) = items.first() {
                return first.clone();
            }
        }
    }
    Object::None
}

fn union_sets(a: &crate::object::SetData, b: &crate::object::SetData) -> Object {
    let mut out = a.clone();
    for k in b.iter() {
        out.insert(k.clone());
    }
    Object::Set(Rc::new(RefCell::new(out)))
}

fn intersect_sets(a: &crate::object::SetData, b: &crate::object::SetData) -> Object {
    let mut out = crate::object::SetData::new();
    for k in a.iter() {
        if b.contains(k) {
            out.insert(k.clone());
        }
    }
    Object::Set(Rc::new(RefCell::new(out)))
}

fn difference_sets(a: &crate::object::SetData, b: &crate::object::SetData) -> Object {
    let mut out = crate::object::SetData::new();
    for k in a.iter() {
        if !b.contains(k) {
            out.insert(k.clone());
        }
    }
    Object::Set(Rc::new(RefCell::new(out)))
}

fn symmetric_diff_sets(a: &crate::object::SetData, b: &crate::object::SetData) -> Object {
    let mut out = crate::object::SetData::new();
    for k in a.iter() {
        if !b.contains(k) {
            out.insert(k.clone());
        }
    }
    for k in b.iter() {
        if !a.contains(k) {
            out.insert(k.clone());
        }
    }
    Object::Set(Rc::new(RefCell::new(out)))
}

/// Public entry point shared with the `format` builtin: drive the
/// format-spec mini-language without going through `FORMAT_VALUE`.
pub(crate) fn format_via_spec(value: &Object, spec: &str) -> Result<String, RuntimeError> {
    let plain = value.to_str();
    if spec.is_empty() {
        return Ok(plain);
    }
    apply_format_spec(value, spec, &plain)
}

/// Public wrapper for `ascii()`.
pub(crate) fn ascii_value(value: &Object) -> String {
    ascii_repr(value)
}

/// Implement `str.format(*args, **kwargs)` at runtime. The grammar
/// matches CPython's `string.Formatter.vformat`: `{}`, `{0}`,
/// `{name}`, `{0.attr}`, `{name[key]}`, with optional `!r`/`!s`/`!a`
/// conversion and `:spec` format spec (which itself may be an
/// f-string-like template using `{0}`/`{name}` references).
pub(crate) fn str_format_impl(
    template: &str,
    positional: &[Object],
    keyword: &[(String, Object)],
) -> Result<String, RuntimeError> {
    let mut out = String::new();
    let bytes = template.as_bytes();
    let mut i = 0;
    let mut auto_idx = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'{' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                out.push('{');
                i += 2;
                continue;
            }
            let (field, end) = scan_format_field(bytes, i + 1)?;
            i = end;
            let rendered = render_format_field(&field, positional, keyword, &mut auto_idx, None)?;
            out.push_str(&rendered);
        } else if b == b'}' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'}' {
                out.push('}');
                i += 2;
                continue;
            }
            return Err(value_error("Single '}' encountered in format string"));
        } else {
            let ch_len = utf8_seq_len(b);
            let end = (i + ch_len).min(bytes.len());
            out.push_str(&template[i..end]);
            i = end;
        }
    }
    Ok(out)
}

pub(crate) fn str_format_map_impl(
    template: &str,
    mapping: &Rc<RefCell<DictData>>,
) -> Result<String, RuntimeError> {
    let mut out = String::new();
    let bytes = template.as_bytes();
    let mut i = 0;
    let mut auto_idx = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'{' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                out.push('{');
                i += 2;
                continue;
            }
            let (field, end) = scan_format_field(bytes, i + 1)?;
            i = end;
            let rendered = render_format_field(&field, &[], &[], &mut auto_idx, Some(mapping))?;
            out.push_str(&rendered);
        } else if b == b'}' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'}' {
                out.push('}');
                i += 2;
                continue;
            }
            return Err(value_error("Single '}' encountered in format string"));
        } else {
            let ch_len = utf8_seq_len(b);
            let end = (i + ch_len).min(bytes.len());
            out.push_str(&template[i..end]);
            i = end;
        }
    }
    Ok(out)
}

fn utf8_seq_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b & 0xE0 == 0xC0 {
        2
    } else if b & 0xF0 == 0xE0 {
        3
    } else if b & 0xF8 == 0xF0 {
        4
    } else {
        1
    }
}

/// Scan from just past the opening `{` to the matching `}` at the
/// same nesting depth. Returns the body and the index after the
/// closing brace.
fn scan_format_field(bytes: &[u8], start: usize) -> Result<(String, usize), RuntimeError> {
    let mut depth = 0i32;
    let mut i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                if depth == 0 {
                    let field = std::str::from_utf8(&bytes[start..i])
                        .map_err(|_| value_error("invalid utf-8 in format field"))?
                        .to_owned();
                    return Ok((field, i + 1));
                }
                depth -= 1;
            }
            _ => {}
        }
        i += 1;
    }
    Err(value_error("Single '{' encountered in format string"))
}

/// Render a single `{field}` interpolation.
fn render_format_field(
    field: &str,
    positional: &[Object],
    keyword: &[(String, Object)],
    auto_idx: &mut usize,
    mapping: Option<&Rc<RefCell<DictData>>>,
) -> Result<String, RuntimeError> {
    // Split off the conversion (`!s`/`!r`/`!a`) and spec (`:...`).
    let (name_part, conv, spec_part) = split_format_field(field);
    // Resolve the leading "field name" reference, applying any
    // attribute / subscript trailers.
    let value = resolve_field_name(name_part, positional, keyword, auto_idx, mapping)?;
    let converted = match conv {
        Some('s') => Object::from_str(value.to_str()),
        Some('r') => Object::from_str(value.repr()),
        Some('a') => Object::from_str(ascii_repr(&value)),
        Some(other) => return Err(value_error(format!("Unknown conversion: {other}"))),
        None => value,
    };
    let spec_str = match spec_part {
        Some(s) if s.contains('{') => {
            // Nested format spec — recursively interpolate.
            str_format_impl(s, positional, keyword)?
        }
        Some(s) => s.to_owned(),
        None => String::new(),
    };
    format_via_spec(&converted, &spec_str)
}

fn split_format_field(field: &str) -> (&str, Option<char>, Option<&str>) {
    let mut name_end = field.len();
    let mut conv: Option<char> = None;
    let mut spec_start: Option<usize> = None;
    let bytes = field.as_bytes();
    let mut depth = 0i32;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'[' => depth += 1,
            b']' => depth -= 1,
            b'!' if depth == 0 && conv.is_none() && spec_start.is_none() => {
                name_end = i;
                if let Some(&next) = bytes.get(i + 1) {
                    conv = Some(next as char);
                }
            }
            b':' if depth == 0 && spec_start.is_none() => {
                if name_end == field.len() {
                    name_end = i;
                }
                spec_start = Some(i + 1);
                break;
            }
            _ => {}
        }
    }
    let name = &field[..name_end];
    let spec = spec_start.map(|s| &field[s..]);
    (name, conv, spec)
}

fn resolve_field_name(
    name: &str,
    positional: &[Object],
    keyword: &[(String, Object)],
    auto_idx: &mut usize,
    mapping: Option<&Rc<RefCell<DictData>>>,
) -> Result<Object, RuntimeError> {
    // Split into base + trailers (`.attr`/`[idx]`).
    let (base, trailers) = split_name_trailers(name);
    let mut value = if base.is_empty() {
        let idx = *auto_idx;
        *auto_idx += 1;
        positional
            .get(idx)
            .cloned()
            .ok_or_else(|| index_error("Replacement index out of range"))?
    } else if let Ok(idx) = base.parse::<usize>() {
        positional
            .get(idx)
            .cloned()
            .ok_or_else(|| index_error(format!("Replacement index {idx} out of range")))?
    } else if let Some(map) = mapping {
        let key = DictKey(Object::from_str(base));
        map.borrow()
            .get(&key)
            .cloned()
            .ok_or_else(|| key_error(format!("'{base}'")))?
    } else {
        keyword
            .iter()
            .find_map(|(k, v)| (k == base).then(|| v.clone()))
            .ok_or_else(|| key_error(format!("'{base}'")))?
    };
    for trailer in trailers {
        value = apply_trailer(value, trailer)?;
    }
    Ok(value)
}

fn split_name_trailers(name: &str) -> (&str, Vec<&str>) {
    let mut trailers = Vec::new();
    let bytes = name.as_bytes();
    let mut base_end = bytes.len();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'.' || bytes[i] == b'[' {
            base_end = i;
            break;
        }
        i += 1;
    }
    let base = &name[..base_end];
    let mut start = base_end;
    while start < bytes.len() {
        if bytes[start] == b'.' {
            let mut j = start + 1;
            while j < bytes.len() && bytes[j] != b'.' && bytes[j] != b'[' {
                j += 1;
            }
            trailers.push(&name[start..j]);
            start = j;
        } else if bytes[start] == b'[' {
            let mut j = start + 1;
            while j < bytes.len() && bytes[j] != b']' {
                j += 1;
            }
            if j < bytes.len() {
                j += 1;
            }
            trailers.push(&name[start..j]);
            start = j;
        } else {
            break;
        }
    }
    (base, trailers)
}

fn apply_trailer(value: Object, trailer: &str) -> Result<Object, RuntimeError> {
    if trailer.starts_with('.') {
        let attr = &trailer[1..];
        match &value {
            Object::Module(m) => m
                .dict
                .borrow()
                .get(&DictKey(Object::from_str(attr)))
                .cloned()
                .ok_or_else(|| attribute_error(format!("module has no attribute '{attr}'"))),
            Object::Instance(inst) => inst
                .dict
                .borrow()
                .get(&DictKey(Object::from_str(attr)))
                .cloned()
                .or_else(|| inst.class.lookup(attr))
                .ok_or_else(|| attribute_error(format!("has no attribute '{attr}'"))),
            _ => Err(attribute_error(format!(
                "'{}' has no attribute '{}'",
                value.type_name(),
                attr
            ))),
        }
    } else if trailer.starts_with('[') && trailer.ends_with(']') {
        let inner = &trailer[1..trailer.len() - 1];
        let key: Object = if let Ok(i) = inner.parse::<i64>() {
            Object::Int(i)
        } else {
            Object::from_str(inner)
        };
        match &value {
            Object::List(l) => {
                let idx = match key {
                    Object::Int(i) => {
                        let len = l.borrow().len() as i64;

                        if i < 0 {
                            i + len
                        } else {
                            i
                        }
                    }
                    _ => return Err(type_error("list indices must be integers")),
                };
                l.borrow()
                    .get(idx as usize)
                    .cloned()
                    .ok_or_else(|| index_error("list index out of range"))
            }
            Object::Tuple(t) => {
                let idx = match key {
                    Object::Int(i) => {
                        let len = t.len() as i64;

                        if i < 0 {
                            i + len
                        } else {
                            i
                        }
                    }
                    _ => return Err(type_error("tuple indices must be integers")),
                };
                t.get(idx as usize)
                    .cloned()
                    .ok_or_else(|| index_error("tuple index out of range"))
            }
            Object::Dict(d) => d
                .borrow()
                .get(&DictKey(key))
                .cloned()
                .ok_or_else(|| key_error("key not found")),
            _ => Err(type_error(format!(
                "'{}' is not subscriptable",
                value.type_name()
            ))),
        }
    } else {
        Err(value_error(format!("Unknown trailer: {trailer}")))
    }
}

/// Apply Python's `%` formatting (`'%s %d' % (x, y)`, or `'%(k)s' %
/// {'k': v}`).
/// Map ``bytes %`` arguments through a latin-1 decoding so that the
/// shared ``percent_format`` engine can substitute them as if they
/// were strings. The output is re-encoded back to bytes by the
/// caller so opaque byte values round-trip unchanged.
fn bytes_percent_args(value: &Object) -> Object {
    fn map_one(v: &Object) -> Object {
        match v {
            Object::Bytes(b) => {
                let s: String = b.iter().map(|byte| *byte as char).collect();
                Object::from_str(s)
            }
            Object::ByteArray(cell) => {
                let b = cell.borrow().clone();
                let s: String = b.iter().map(|byte| *byte as char).collect();
                Object::from_str(s)
            }
            _ => v.clone(),
        }
    }
    match value {
        Object::Tuple(items) => {
            let mapped: Vec<Object> = items.iter().map(map_one).collect();
            Object::Tuple(Rc::from(mapped))
        }
        Object::Dict(d) => {
            let src = d.borrow();
            let mut out: crate::object::DictData = indexmap::IndexMap::new();
            for (k, v) in src.iter() {
                out.insert(k.clone(), map_one(v));
            }
            Object::Dict(Rc::new(RefCell::new(out)))
        }
        other => map_one(other),
    }
}

pub(crate) fn percent_format(template: &str, value: &Object) -> Result<String, RuntimeError> {
    let mut out = String::new();
    let bytes = template.as_bytes();
    let mut i = 0;
    let mut idx = 0usize;
    let positional: Vec<Object> = match value {
        Object::Tuple(items) => items.to_vec(),
        Object::Dict(_) => Vec::new(),
        other => vec![other.clone()],
    };
    while i < bytes.len() {
        if bytes[i] == b'%' {
            i += 1;
            if i >= bytes.len() {
                return Err(value_error("incomplete format"));
            }
            // Optional mapping key: %(name)s
            let mut mapping_key: Option<String> = None;
            if bytes[i] == b'(' {
                let mut depth = 1;
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && depth > 0 {
                    match bytes[j] {
                        b'(' => depth += 1,
                        b')' => depth -= 1,
                        _ => {}
                    }
                    if depth > 0 {
                        j += 1;
                    }
                }
                mapping_key = Some(
                    std::str::from_utf8(&bytes[start..j])
                        .map_err(|_| value_error("invalid utf-8"))?
                        .to_owned(),
                );
                i = j + 1;
            }
            // Flags / width / precision / type — parse loosely.
            let mut flags = String::new();
            while i < bytes.len() && b"#0- +".contains(&bytes[i]) {
                flags.push(bytes[i] as char);
                i += 1;
            }
            let mut width = String::new();
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                width.push(bytes[i] as char);
                i += 1;
            }
            let mut precision: Option<String> = None;
            if i < bytes.len() && bytes[i] == b'.' {
                i += 1;
                let mut p = String::new();
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    p.push(bytes[i] as char);
                    i += 1;
                }
                precision = Some(p);
            }
            if i >= bytes.len() {
                return Err(value_error("incomplete format"));
            }
            let kind = bytes[i] as char;
            i += 1;
            if kind == '%' {
                out.push('%');
                continue;
            }
            let item = if let Some(k) = mapping_key {
                match value {
                    Object::Dict(d) => d
                        .borrow()
                        .get(&DictKey(Object::from_str(&k)))
                        .cloned()
                        .ok_or_else(|| key_error(format!("'{k}'")))?,
                    _ => return Err(type_error("format requires a mapping")),
                }
            } else {
                let v = positional
                    .get(idx)
                    .cloned()
                    .ok_or_else(|| type_error("not enough arguments for format string"))?;
                idx += 1;
                v
            };
            let mut spec = String::new();
            if !flags.is_empty() {
                // Build `[fill][align]`. Zero-pad needs the fill
                // char *and* the align char together, e.g. "0=" for
                // sign-aware zero padding. Left-align via '-' uses
                // explicit '<'.
                if flags.contains('-') {
                    spec.push('<');
                } else if flags.contains('0') {
                    // ``%05d`` → ``0=05d`` (fill='0', align='=',
                    // ``0`` flag, width=5, type=d). The ``0`` flag
                    // is harmless after the align prefix.
                    spec.push('0');
                    spec.push('=');
                }
                if flags.contains('+') {
                    spec.push('+');
                } else if flags.contains(' ') {
                    spec.push(' ');
                }
                if flags.contains('#') {
                    spec.push('#');
                }
                if flags.contains('0') && !flags.contains('-') {
                    spec.push('0');
                }
            }
            if !width.is_empty() {
                spec.push_str(&width);
            }
            if let Some(p) = precision {
                spec.push('.');
                spec.push_str(&p);
            }
            spec.push(kind);
            let rendered = match kind {
                's' => format_via_spec(&Object::from_str(item.to_str()), &spec)?,
                'r' => format_via_spec(&Object::from_str(item.repr()), &spec.replace('r', "s"))?,
                'a' => format_via_spec(
                    &Object::from_str(ascii_repr(&item)),
                    &spec.replace('a', "s"),
                )?,
                'd' | 'i' | 'u' => format_via_spec(&item, &spec.replace(['i', 'u'], "d"))?,
                'b' | 'o' | 'x' | 'X' => format_via_spec(&item, &spec)?,
                'f' | 'F' | 'e' | 'E' | 'g' | 'G' => format_via_spec(&item, &spec)?,
                'c' => match &item {
                    Object::Int(c) => {
                        char::from_u32(*c as u32).map_or(String::new(), |c| c.to_string())
                    }
                    Object::Str(s) => s.to_string(),
                    _ => return Err(type_error("%c requires int or single character")),
                },
                _ => {
                    return Err(value_error(format!(
                        "unsupported format character '{kind}'"
                    )))
                }
            };
            out.push_str(&rendered);
        } else {
            let ch_len = utf8_seq_len(bytes[i]);
            let end = (i + ch_len).min(bytes.len());
            out.push_str(&template[i..end]);
            i = end;
        }
    }
    Ok(out)
}

/// `ascii()` builtin: like `repr()` but escapes non-ASCII codepoints.
fn ascii_repr(value: &Object) -> String {
    let r = value.repr();
    let mut out = String::with_capacity(r.len());
    for c in r.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let n = c as u32;
            if n <= 0xFFFF {
                out.push_str(&format!("\\u{n:04x}"));
            } else {
                out.push_str(&format!("\\U{n:08x}"));
            }
        }
    }
    out
}

/// Apply a CPython-style format spec to a value. We implement the
/// subset needed by f-strings: fill/align, sign, width, precision,
/// type. Anything we don't yet handle falls back to the plain string.
fn apply_format_spec(value: &Object, spec: &str, plain: &str) -> Result<String, RuntimeError> {
    let parsed = parse_format_spec(spec)?;
    // Type-driven formatting first; if no type code, just pad.
    let formatted = match parsed.type_char {
        Some('d') => match value {
            Object::Int(i) => format_int(*i, &parsed),
            Object::Bool(b) => format_int(i64::from(*b), &parsed),
            _ => return Err(value_error("Unknown format code 'd' for non-integer")),
        },
        Some('b') => match value {
            Object::Int(i) => format_int_base(*i, 2, &parsed),
            Object::Bool(b) => format_int_base(i64::from(*b), 2, &parsed),
            _ => return Err(value_error("Unknown format code 'b' for non-integer")),
        },
        Some('o') => match value {
            Object::Int(i) => format_int_base(*i, 8, &parsed),
            Object::Bool(b) => format_int_base(i64::from(*b), 8, &parsed),
            _ => return Err(value_error("Unknown format code 'o' for non-integer")),
        },
        Some('x') => match value {
            Object::Int(i) => format_int_hex(*i, false, &parsed),
            Object::Bool(b) => format_int_hex(i64::from(*b), false, &parsed),
            _ => return Err(value_error("Unknown format code 'x' for non-integer")),
        },
        Some('X') => match value {
            Object::Int(i) => format_int_hex(*i, true, &parsed),
            Object::Bool(b) => format_int_hex(i64::from(*b), true, &parsed),
            _ => return Err(value_error("Unknown format code 'X' for non-integer")),
        },
        Some('f') | Some('F') => {
            let f = obj_as_float(value)?;
            let prec = parsed.precision.unwrap_or(6);
            format_float_fixed(f, prec, &parsed)
        }
        Some('e') => {
            let f = obj_as_float(value)?;
            let prec = parsed.precision.unwrap_or(6);
            format_float_scientific(f, prec, false, &parsed)
        }
        Some('E') => {
            let f = obj_as_float(value)?;
            let prec = parsed.precision.unwrap_or(6);
            format_float_scientific(f, prec, true, &parsed)
        }
        Some('g') | Some('G') => {
            let f = obj_as_float(value)?;
            let prec = parsed.precision.unwrap_or(6).max(1);
            format_float_general(f, prec, parsed.type_char == Some('G'), &parsed)
        }
        Some('%') => {
            let f = obj_as_float(value)?;
            let prec = parsed.precision.unwrap_or(6);
            let body = format_float_fixed(f * 100.0, prec, &parsed);
            format!("{body}%")
        }
        Some('s') | None => {
            let mut s = plain.to_owned();
            if let Some(p) = parsed.precision {
                if matches!(parsed.type_char, Some('s') | None) {
                    s.truncate(p);
                }
            }
            // CPython 3.13: when no presentation type is given,
            // numeric values default to right-alignment (and a
            // leading-zero pad if `0` is set), strings default to
            // left-alignment. Match that.
            let numeric_default = parsed.type_char.is_none()
                && matches!(
                    value,
                    Object::Int(_)
                        | Object::Long(_)
                        | Object::Bool(_)
                        | Object::Float(_)
                        | Object::Complex(_)
                );
            apply_alignment(&s, &parsed, numeric_default)
        }
        Some('c') => match value {
            Object::Int(i) => {
                let c = u32::try_from(*i)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or_else(|| value_error("integer is not a valid unicode codepoint"))?;
                apply_alignment(&c.to_string(), &parsed, false)
            }
            _ => return Err(value_error("%c requires int or char")),
        },
        Some(other) => {
            return Err(value_error(format!(
                "Unknown format code '{other}' for object of type '{}'",
                value.type_name()
            )));
        }
    };
    Ok(formatted)
}

#[derive(Debug, Default)]
struct ParsedSpec {
    fill: Option<char>,
    align: Option<char>,
    sign: Option<char>,
    alt: bool,
    zero: bool,
    width: Option<usize>,
    grouping: Option<char>,
    precision: Option<usize>,
    type_char: Option<char>,
}

fn parse_format_spec(spec: &str) -> Result<ParsedSpec, RuntimeError> {
    let mut p = ParsedSpec::default();
    let chars: Vec<char> = spec.chars().collect();
    let mut i = 0;
    // [[fill]align]
    if chars.len() >= 2 && matches!(chars[1], '<' | '>' | '^' | '=') {
        p.fill = Some(chars[0]);
        p.align = Some(chars[1]);
        i = 2;
    } else if !chars.is_empty() && matches!(chars[0], '<' | '>' | '^' | '=') {
        p.align = Some(chars[0]);
        i = 1;
    }
    // [sign]
    if let Some(&c) = chars.get(i) {
        if matches!(c, '+' | '-' | ' ') {
            p.sign = Some(c);
            i += 1;
        }
    }
    // [#]
    if let Some(&'#') = chars.get(i) {
        p.alt = true;
        i += 1;
    }
    // [0]
    if let Some(&'0') = chars.get(i) {
        p.zero = true;
        if p.align.is_none() {
            p.align = Some('=');
            p.fill = Some('0');
        }
        i += 1;
    }
    // [width]
    let mut width = 0usize;
    let mut had_width = false;
    while let Some(&c) = chars.get(i) {
        if c.is_ascii_digit() {
            width = width * 10 + (c as usize - '0' as usize);
            i += 1;
            had_width = true;
        } else {
            break;
        }
    }
    if had_width {
        p.width = Some(width);
    }
    // [grouping]
    if let Some(&c) = chars.get(i) {
        if matches!(c, ',' | '_') {
            p.grouping = Some(c);
            i += 1;
        }
    }
    // [.precision]
    if let Some(&'.') = chars.get(i) {
        i += 1;
        let mut prec = 0usize;
        let mut had_prec = false;
        while let Some(&c) = chars.get(i) {
            if c.is_ascii_digit() {
                prec = prec * 10 + (c as usize - '0' as usize);
                i += 1;
                had_prec = true;
            } else {
                break;
            }
        }
        if had_prec {
            p.precision = Some(prec);
        }
    }
    // [type]
    if let Some(&c) = chars.get(i) {
        if !c.is_whitespace() {
            p.type_char = Some(c);
            i += 1;
        }
    }
    if i < chars.len() {
        return Err(value_error(format!("invalid format specifier: {spec:?}")));
    }
    Ok(p)
}

fn obj_as_float(v: &Object) -> Result<f64, RuntimeError> {
    match v {
        Object::Float(f) => Ok(*f),
        Object::Int(i) => Ok(*i as f64),
        Object::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        _ => Err(type_error(format!(
            "unsupported format string passed to {}",
            v.type_name()
        ))),
    }
}

fn format_int(i: i64, p: &ParsedSpec) -> String {
    let mag = i.unsigned_abs();
    let mut body = if let Some(grp) = p.grouping {
        group_decimal(mag, grp)
    } else {
        mag.to_string()
    };
    body = with_sign(i < 0, &body, p);
    apply_alignment(&body, p, true)
}

fn format_int_base(i: i64, base: u32, p: &ParsedSpec) -> String {
    let mag = i.unsigned_abs();
    let mut body = match base {
        2 => format!("{mag:b}"),
        8 => format!("{mag:o}"),
        10 => mag.to_string(),
        _ => mag.to_string(),
    };
    if p.alt {
        let prefix = match base {
            2 => "0b",
            8 => "0o",
            _ => "",
        };
        body = format!("{prefix}{body}");
    }
    body = with_sign(i < 0, &body, p);
    apply_alignment(&body, p, true)
}

fn format_int_hex(i: i64, upper: bool, p: &ParsedSpec) -> String {
    let mag = i.unsigned_abs();
    let body_core = if upper {
        format!("{mag:X}")
    } else {
        format!("{mag:x}")
    };
    let mut body = if p.alt {
        format!("{}{body_core}", if upper { "0X" } else { "0x" })
    } else {
        body_core
    };
    body = with_sign(i < 0, &body, p);
    apply_alignment(&body, p, true)
}

fn format_float_fixed(f: f64, prec: usize, p: &ParsedSpec) -> String {
    if f.is_nan() {
        return apply_alignment("nan", p, false);
    }
    if f.is_infinite() {
        let s = if f < 0.0 { "-inf" } else { "inf" };
        return apply_alignment(s, p, false);
    }
    let neg = f.is_sign_negative();
    let mag = f.abs();
    let body = format!("{mag:.*}", prec);
    let body = with_sign(neg, &body, p);
    apply_alignment(&body, p, true)
}

fn format_float_scientific(f: f64, prec: usize, upper: bool, p: &ParsedSpec) -> String {
    if f.is_nan() {
        return apply_alignment(if upper { "NAN" } else { "nan" }, p, false);
    }
    if f.is_infinite() {
        let s = if upper {
            if f < 0.0 {
                "-INF"
            } else {
                "INF"
            }
        } else if f < 0.0 {
            "-inf"
        } else {
            "inf"
        };
        return apply_alignment(s, p, false);
    }
    let neg = f.is_sign_negative();
    let mag = f.abs();
    let raw = format!("{mag:.*e}", prec);
    // Rust gives e.g. "1.230000e2"; CPython wants "1.230000e+02".
    let body = normalize_exponent(&raw, upper);
    let body = with_sign(neg, &body, p);
    apply_alignment(&body, p, true)
}

fn normalize_exponent(raw: &str, upper: bool) -> String {
    if let Some(idx) = raw.find('e') {
        let (mant, exp) = raw.split_at(idx);
        let exp = &exp[1..]; // drop 'e'
        let (sign, digits) = if let Some(stripped) = exp.strip_prefix('-') {
            ('-', stripped)
        } else if let Some(stripped) = exp.strip_prefix('+') {
            ('+', stripped)
        } else {
            ('+', exp)
        };
        let digits = if digits.len() < 2 {
            format!("0{digits}")
        } else {
            digits.to_owned()
        };
        let e = if upper { 'E' } else { 'e' };
        format!("{mant}{e}{sign}{digits}")
    } else {
        raw.to_owned()
    }
}

fn format_float_general(f: f64, prec: usize, upper: bool, p: &ParsedSpec) -> String {
    if f == 0.0 || f.is_nan() || f.is_infinite() {
        return format_float_fixed(f, prec.saturating_sub(1), p);
    }
    let exp = f.abs().log10().floor() as i32;
    if exp < -4 || exp >= prec as i32 {
        format_float_scientific(f, prec.saturating_sub(1), upper, p)
    } else {
        let digits_after = (prec as i32 - 1 - exp).max(0) as usize;
        format_float_fixed(f, digits_after, p)
    }
}

fn with_sign(neg: bool, body: &str, p: &ParsedSpec) -> String {
    if neg {
        format!("-{body}")
    } else {
        match p.sign {
            Some('+') => format!("+{body}"),
            Some(' ') => format!(" {body}"),
            _ => body.to_owned(),
        }
    }
}

fn apply_alignment(body: &str, p: &ParsedSpec, default_right: bool) -> String {
    let width = p.width.unwrap_or(0);
    if body.chars().count() >= width {
        return body.to_owned();
    }
    let fill = p.fill.unwrap_or(' ');
    let pad = width - body.chars().count();
    let align = p.align.unwrap_or(if default_right { '>' } else { '<' });
    match align {
        '<' => {
            let mut s = body.to_owned();
            for _ in 0..pad {
                s.push(fill);
            }
            s
        }
        '>' => {
            let mut s = String::with_capacity(body.len() + pad);
            for _ in 0..pad {
                s.push(fill);
            }
            s.push_str(body);
            s
        }
        '^' => {
            let left = pad / 2;
            let right = pad - left;
            let mut s = String::with_capacity(body.len() + pad);
            for _ in 0..left {
                s.push(fill);
            }
            s.push_str(body);
            for _ in 0..right {
                s.push(fill);
            }
            s
        }
        '=' => {
            // Pad between sign and digits.
            let mut chars = body.chars();
            let lead = chars
                .next()
                .filter(|c| matches!(*c, '+' | '-' | ' '))
                .map_or(String::new(), |c| c.to_string());
            let rest: String = if lead.is_empty() {
                body.to_owned()
            } else {
                chars.collect()
            };
            let mut s = String::with_capacity(body.len() + pad);
            s.push_str(&lead);
            for _ in 0..pad {
                s.push(fill);
            }
            s.push_str(&rest);
            s
        }
        _ => body.to_owned(),
    }
}

fn group_decimal(mag: u64, sep: char) -> String {
    let s = mag.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    let mut first = bytes.len() % 3;
    if first == 0 {
        first = 3;
    }
    out.push_str(std::str::from_utf8(&bytes[..first]).unwrap());
    let mut i = first;
    while i < bytes.len() {
        out.push(sep);
        out.push_str(std::str::from_utf8(&bytes[i..i + 3]).unwrap());
        i += 3;
    }
    out
}

fn binop_dunders(op: BinOpKind) -> (&'static str, &'static str) {
    use BinOpKind as B;
    match op {
        B::Add => ("__add__", "__radd__"),
        B::Sub => ("__sub__", "__rsub__"),
        B::Mult => ("__mul__", "__rmul__"),
        B::MatMult => ("__matmul__", "__rmatmul__"),
        B::Div => ("__truediv__", "__rtruediv__"),
        B::FloorDiv => ("__floordiv__", "__rfloordiv__"),
        B::Mod => ("__mod__", "__rmod__"),
        B::Pow => ("__pow__", "__rpow__"),
        B::LShift => ("__lshift__", "__rlshift__"),
        B::RShift => ("__rshift__", "__rrshift__"),
        B::BitOr => ("__or__", "__ror__"),
        B::BitXor => ("__xor__", "__rxor__"),
        B::BitAnd => ("__and__", "__rand__"),
    }
}

fn cmp_dunder(op: CompareKind) -> (&'static str, &'static str) {
    use CompareKind as C;
    match op {
        C::Eq => ("__eq__", "__eq__"),
        C::NotEq => ("__ne__", "__ne__"),
        C::Lt => ("__lt__", "__gt__"),
        C::LtE => ("__le__", "__ge__"),
        C::Gt => ("__gt__", "__lt__"),
        C::GtE => ("__ge__", "__le__"),
    }
}

fn find_handler(table: &[ExcHandler], pc: u32) -> Option<&ExcHandler> {
    // Innermost-first: nested `compile_try` calls push the inner
    // entry before the outer one, so a forward scan finds the
    // tightest range first.
    table.iter().find(|h| pc >= h.start && pc < h.end)
}

fn is_super_callable(obj: &Object) -> bool {
    matches!(obj, Object::Builtin(b) if b.name == "super")
}

/// Numeric protocol data attributes exposed by the ``numbers`` ABC
/// hierarchy. Matches CPython's ``int.real``, ``int.imag``,
/// ``int.numerator``, ``int.denominator``, ``float.real`` /
/// ``float.imag``, and ``complex.real`` / ``complex.imag``.
fn numeric_data_attr(obj: &Object, name: &str) -> Option<Object> {
    match (obj, name) {
        // int / bool
        (Object::Int(_) | Object::Long(_) | Object::Bool(_), "real")
        | (Object::Int(_) | Object::Long(_) | Object::Bool(_), "numerator") => Some(obj.clone()),
        (Object::Int(_) | Object::Long(_) | Object::Bool(_), "imag") => Some(Object::Int(0)),
        (Object::Int(_) | Object::Long(_) | Object::Bool(_), "denominator") => Some(Object::Int(1)),
        // float
        (Object::Float(_), "real") => Some(obj.clone()),
        (Object::Float(_), "imag") => Some(Object::Float(0.0)),
        // complex
        (Object::Complex(c), "real") => Some(Object::Float(c.real)),
        (Object::Complex(c), "imag") => Some(Object::Float(c.imag)),
        _ => None,
    }
}

/// Detect whether `frame` is paused inside a ``yield from``
/// delegation. The canonical bytecode shape is
/// ``SEND -> YIELD_VALUE -> JUMP_BACKWARD``; if the most recently
/// executed instruction was ``YIELD_VALUE`` and the one preceding
/// it was ``SEND``, the top of the stack is the inner sub-iterator
/// the outer is delegating to.
fn detect_yield_from_subiter(frame: &Frame) -> Option<Object> {
    if frame.pc == 0 {
        return None;
    }
    let prev_pc = frame.pc as usize - 1;
    let yielded = frame.code.instructions.get(prev_pc)?;
    if yielded.op != OpCode::YieldValue {
        return None;
    }
    if prev_pc == 0 {
        return None;
    }
    let sender = frame.code.instructions.get(prev_pc - 1)?;
    if sender.op != OpCode::Send {
        return None;
    }
    let top = frame.stack.last()?;
    match top {
        Object::Generator(_)
        | Object::Coroutine(_)
        | Object::AsyncGenerator(_)
        | Object::Iter(_) => Some(top.clone()),
        _ => None,
    }
}

/// Advance `frame.pc` past the SEND/YIELD/JUMP-BACKWARD loop the
/// frame is currently parked in. Used after the sub-iterator
/// finishes (``StopIteration``) so the outer continues at the
/// END_SEND target rather than re-entering ``SEND`` with the
/// stale iter.
fn advance_past_yield_from(frame: &mut Frame) {
    // The SEND instruction at `prev_pc - 1` carries the
    // jump-arg that points past END_SEND. Replicate that jump.
    if frame.pc == 0 {
        return;
    }
    let prev_pc = frame.pc as usize - 1;
    if prev_pc == 0 {
        return;
    }
    let send_pc = prev_pc - 1;
    let send_ins = match frame.code.instructions.get(send_pc) {
        Some(i) => i,
        None => return,
    };
    if send_ins.op != OpCode::Send {
        return;
    }
    // SEND's jump arg is relative-forward by `arg` from `send_pc + 1`.
    let target = (send_pc as u32) + 1 + send_ins.arg;
    frame.pc = target;
}

fn find_cell(frame: &Frame, name: &str) -> Option<Rc<RefCell<Object>>> {
    let cells = &frame.cells;
    let cellvars = &frame.code.cellvars;
    let freevars = &frame.code.freevars;
    cellvars
        .iter()
        .position(|n| n == name)
        .or_else(|| {
            freevars
                .iter()
                .position(|n| n == name)
                .map(|i| i + cellvars.len())
        })
        .and_then(|idx| cells.get(idx).cloned())
}

fn is_param(code: &CodeObject, name: &str) -> bool {
    let total = (code.arg_count + code.kwonly_count) as usize
        + usize::from(code.has_varargs)
        + usize::from(code.has_varkeywords);
    code.varnames.iter().take(total).any(|n| n == name)
}

// ---------- arithmetic ----------

pub fn constant_to_object_public(c: Constant) -> Object {
    constant_to_object(c)
}

fn constant_to_object(c: Constant) -> Object {
    match c {
        Constant::None => Object::None,
        Constant::Bool(b) => Object::Bool(b),
        Constant::Int(i) => Object::Int(i),
        Constant::BigInt(b) => Object::int_from_bigint(b),
        Constant::Float(f) => Object::Float(f),
        Constant::Complex(real, imag) => Object::new_complex(real, imag),
        Constant::Str(s) => Object::from_str(s),
        Constant::Bytes(b) => Object::new_bytes(b),
        Constant::Tuple(xs) => Object::new_tuple(xs.into_iter().map(constant_to_object).collect()),
        Constant::Code(c) => Object::Code(Rc::from(*c)),
        Constant::Ellipsis => Object::None,
    }
}

fn binary_op(a: &Object, b: &Object, op: BinOpKind) -> Result<Object, RuntimeError> {
    use BinOpKind as B;
    use Object as O;
    // Promote bool → int where appropriate.
    let (a, b) = (promote_bool(a), promote_bool(b));

    // Numeric tower: any (int-like, int-like) arithmetic routes
    // through the bignum-aware path with i64 fast-track and overflow
    // promotion to BigInt.
    if a.is_int_like() && b.is_int_like() {
        return bignum_op(&a, &b, op);
    }
    // Complex absorbs (complex, anything-numeric).
    if matches!(a, O::Complex(_)) || matches!(b, O::Complex(_)) {
        if let (Some(ac), Some(bc)) = (a.as_complex(), b.as_complex()) {
            return complex_arith(ac, bc, op);
        }
    }

    match (&a, &b, op) {
        (O::Float(x), O::Float(y), B::Add) => Ok(O::Float(x + y)),
        (O::Float(x), O::Float(y), B::Sub) => Ok(O::Float(x - y)),
        (O::Float(x), O::Float(y), B::Mult) => Ok(O::Float(x * y)),
        (O::Float(x), O::Float(y), B::Div) => {
            if *y == 0.0 {
                Err(zero_division_error("float division by zero"))
            } else {
                Ok(O::Float(x / y))
            }
        }
        (O::Float(x), O::Float(y), B::Mod) => Ok(O::Float(x.rem_euclid(*y))),
        (O::Float(x), O::Float(y), B::Pow) => Ok(O::Float(x.powf(*y))),
        (O::Float(x), O::Float(y), B::FloorDiv) => Ok(O::Float((x / y).floor())),

        (O::Int(x), O::Float(y), op) => binary_op(&O::Float(*x as f64), &O::Float(*y), op),
        (O::Float(x), O::Int(y), op) => binary_op(&O::Float(*x), &O::Float(*y as f64), op),
        (O::Long(x), O::Float(y), op) => {
            let xf = x
                .to_f64()
                .ok_or_else(|| value_error("int too large to convert to float"))?;
            binary_op(&O::Float(xf), &O::Float(*y), op)
        }
        (O::Float(x), O::Long(y), op) => {
            let yf = y
                .to_f64()
                .ok_or_else(|| value_error("int too large to convert to float"))?;
            binary_op(&O::Float(*x), &O::Float(yf), op)
        }

        (O::Str(x), O::Str(y), B::Add) => {
            let mut out = String::with_capacity(x.len() + y.len());
            out.push_str(x);
            out.push_str(y);
            Ok(Object::from_str(out))
        }
        (O::Str(x), O::Int(n), B::Mult) | (O::Int(n), O::Str(x), B::Mult) => {
            let times = if *n < 0 { 0 } else { *n as usize };
            let mut out = String::with_capacity(x.len() * times);
            for _ in 0..times {
                out.push_str(x);
            }
            Ok(Object::from_str(out))
        }
        (O::Str(template), v, B::Mod) => Ok(Object::from_str(percent_format(template, v)?)),
        (O::Bytes(template), v, B::Mod) => {
            // PEP 461: ``bytes % args`` reuses the same templating
            // engine as ``str % args``. We decode the template as
            // latin-1 (raw byte → 1:1 codepoint mapping), substitute,
            // and re-encode the same way so opaque bytes round-trip.
            let s: String = template.iter().map(|b| *b as char).collect();
            let mapped = bytes_percent_args(v);
            let rendered = percent_format(&s, &mapped)?;
            let out: Vec<u8> = rendered.chars().map(|c| c as u8).collect();
            Ok(Object::new_bytes(out))
        }
        (O::ByteArray(cell), v, B::Mod) => {
            let template = cell.borrow().clone();
            let s: String = template.iter().map(|b| *b as char).collect();
            let mapped = bytes_percent_args(v);
            let rendered = percent_format(&s, &mapped)?;
            let out: Vec<u8> = rendered.chars().map(|c| c as u8).collect();
            Ok(Object::new_bytes(out))
        }
        (O::Bytes(x), O::Bytes(y), B::Add) => {
            let mut out = Vec::with_capacity(x.len() + y.len());
            out.extend_from_slice(x);
            out.extend_from_slice(y);
            Ok(Object::new_bytes(out))
        }
        (O::Bytes(x), O::Int(n), B::Mult) | (O::Int(n), O::Bytes(x), B::Mult) => {
            let times = if *n < 0 { 0 } else { *n as usize };
            let mut out = Vec::with_capacity(x.len() * times);
            for _ in 0..times {
                out.extend_from_slice(x);
            }
            Ok(Object::new_bytes(out))
        }
        (O::Set(a), O::Set(b), B::BitOr) => Ok(union_sets(&a.borrow(), &b.borrow())),
        (O::Set(a), O::Set(b), B::BitAnd) => Ok(intersect_sets(&a.borrow(), &b.borrow())),
        (O::Set(a), O::Set(b), B::Sub) => Ok(difference_sets(&a.borrow(), &b.borrow())),
        (O::Set(a), O::Set(b), B::BitXor) => Ok(symmetric_diff_sets(&a.borrow(), &b.borrow())),
        (O::FrozenSet(a), O::FrozenSet(b), B::BitOr) => Ok(union_sets(a, b)),
        (O::FrozenSet(a), O::FrozenSet(b), B::BitAnd) => Ok(intersect_sets(a, b)),
        (O::FrozenSet(a), O::FrozenSet(b), B::Sub) => Ok(difference_sets(a, b)),
        (O::FrozenSet(a), O::FrozenSet(b), B::BitXor) => Ok(symmetric_diff_sets(a, b)),

        (O::List(x), O::List(y), B::Add) => {
            let mut out = x.borrow().clone();
            out.extend(y.borrow().iter().cloned());
            Ok(Object::new_list(out))
        }
        (O::List(x), O::Int(n), B::Mult) | (O::Int(n), O::List(x), B::Mult) => {
            let times = if *n < 0 { 0 } else { *n as usize };
            let body = x.borrow().clone();
            let mut out = Vec::with_capacity(body.len() * times);
            for _ in 0..times {
                out.extend(body.iter().cloned());
            }
            Ok(Object::new_list(out))
        }
        (O::Tuple(x), O::Tuple(y), B::Add) => {
            let mut out: Vec<Object> = x.iter().cloned().collect();
            out.extend(y.iter().cloned());
            Ok(Object::new_tuple(out))
        }

        // PEP 604 — type union via `|`. Matches `Type | Type`,
        // `Type | None`, `Type | UnionType`, and the symmetric forms.
        // Builds a `SimpleNamespace`-backed PEP 604 union that
        // `isinstance` / `issubclass` recognise via
        // [`is_pep604_union`].
        _ if op == B::BitOr && is_union_eligible(&a) && is_union_eligible(&b) => {
            Ok(make_pep604_union(&a, &b))
        }

        _ => Err(type_error(format!(
            "unsupported operand type(s) for {}: '{}' and '{}'",
            op.as_str(),
            a.type_name(),
            b.type_name()
        ))),
    }
}

/// Return `true` if `obj` can participate in a PEP 604 `X | Y` union
/// construction — a real type, the runtime singleton `None`
/// (interpreted as `type(None)`), or an existing PEP 604 union we
/// can flatten.
fn is_union_eligible(obj: &Object) -> bool {
    matches!(obj, Object::Type(_) | Object::None) || is_pep604_union(obj).is_some()
}

/// Detect whether `obj` is a PEP 604 union. Returns the flattened
/// list of `__args__` if so, else `None`.
///
/// We represent PEP 604 unions as a `SimpleNamespace` carrying a
/// `__is_pep604_union__` sentinel and an `__args__` tuple. This
/// piggy-backs on the existing generic-alias machinery in
/// `builtins::class_matches_classinfo` without needing a fresh
/// `Object` variant; the sentinel disambiguates "regular
/// namespace" from "real union".
pub fn is_pep604_union(obj: &Object) -> Option<Vec<Object>> {
    let Object::SimpleNamespace(d) = obj else {
        return None;
    };
    let dict = d.borrow();
    dict.get(&DictKey(Object::from_static("__is_pep604_union__")))
        .filter(|v| matches!(v, Object::Bool(true)))?;
    let args = dict.get(&DictKey(Object::from_static("__args__")))?;
    let Object::Tuple(items) = args else {
        return None;
    };
    Some(items.iter().cloned().collect())
}

/// Build a PEP 604 union `a | b`. `None` is normalised to
/// `type(None)`; nested unions are flattened; duplicate types
/// (by identity) are de-duplicated, preserving first-seen
/// order. Matches CPython's behaviour in
/// `Objects/unionobject.c::_Py_make_union`.
pub fn make_pep604_union(a: &Object, b: &Object) -> Object {
    let mut args: Vec<Object> = Vec::new();
    let mut push = |x: &Object| {
        if let Some(existing) = is_pep604_union(x) {
            for e in existing {
                args.push(normalize_union_arg(e));
            }
        } else {
            args.push(normalize_union_arg(x.clone()));
        }
    };
    push(a);
    push(b);

    // Dedup by identity (Rc::ptr_eq for types; address for None).
    let mut seen_types: Vec<*const ()> = Vec::new();
    let mut seen_none = false;
    args.retain(|x| match x {
        Object::Type(t) => {
            let p = Rc::as_ptr(t).cast::<()>();
            if seen_types.contains(&p) {
                return false;
            }
            seen_types.push(p);
            true
        }
        Object::None => {
            if seen_none {
                false
            } else {
                seen_none = true;
                true
            }
        }
        _ => true,
    });

    let mut dict = DictData::new();
    dict.insert(
        DictKey(Object::from_static("__is_pep604_union__")),
        Object::Bool(true),
    );
    dict.insert(
        DictKey(Object::from_static("__args__")),
        Object::new_tuple(args.clone()),
    );
    dict.insert(
        DictKey(Object::from_static("__parameters__")),
        Object::new_tuple(Vec::new()),
    );
    // Surface a `__class__` string so `repr` / type introspection
    // sees something reasonable; we don't have a real
    // `types.UnionType` runtime type yet but the str is cheap.
    dict.insert(
        DictKey(Object::from_static("__class__")),
        Object::from_static("types.UnionType"),
    );
    Object::SimpleNamespace(Rc::new(RefCell::new(dict)))
}

/// Normalise a single argument for inclusion in a PEP 604 union:
/// keep types as types; keep `None` as `None` (downstream
/// `isinstance` recognises both).
fn normalize_union_arg(x: Object) -> Object {
    x
}

fn unary_op(v: &Object, op: UnaryKind) -> Result<Object, RuntimeError> {
    use Object as O;
    match (op, v) {
        (UnaryKind::Pos, O::Int(i)) => Ok(O::Int(*i)),
        (UnaryKind::Neg, O::Int(i)) => match i.checked_neg() {
            Some(r) => Ok(O::Int(r)),
            None => Ok(Object::int_from_bigint(-num_bigint::BigInt::from(*i))),
        },
        (UnaryKind::Pos, O::Long(b)) => Ok(O::Long(b.clone())),
        (UnaryKind::Neg, O::Long(b)) => Ok(Object::int_from_bigint(-(**b).clone())),
        (UnaryKind::Pos, O::Float(f)) => Ok(O::Float(*f)),
        (UnaryKind::Neg, O::Float(f)) => Ok(O::Float(-f)),
        (UnaryKind::Pos, O::Bool(b)) => Ok(O::Int(i64::from(*b))),
        (UnaryKind::Neg, O::Bool(b)) => Ok(O::Int(-i64::from(*b))),
        (UnaryKind::Pos, O::Complex(c)) => Ok(O::Complex(c.clone())),
        (UnaryKind::Neg, O::Complex(c)) => Ok(Object::new_complex(-c.real, -c.imag)),
        (UnaryKind::Invert, O::Int(i)) => Ok(O::Int(!i)),
        (UnaryKind::Invert, O::Long(b)) => Ok(Object::int_from_bigint(!(**b).clone())),
        (UnaryKind::Invert, O::Bool(b)) => Ok(O::Int(!i64::from(*b))),
        (UnaryKind::Not, x) => Ok(O::Bool(!x.is_truthy())),
        _ => Err(type_error(format!(
            "bad operand type for unary {}: '{}'",
            op.as_str(),
            v.type_name()
        ))),
    }
}

/// Bignum-aware integer arithmetic for `int`-flavoured operands.
/// Both inputs are guaranteed `int`/`long`/`bool` by the caller; the
/// fast path stays in `i64` until an overflow forces promotion.
fn bignum_op(a: &Object, b: &Object, op: BinOpKind) -> Result<Object, RuntimeError> {
    use BinOpKind as B;

    // Fast path: both sides fit in i64 *and* no operation overflows.
    if let (Some(x), Some(y)) = (a.as_i64(), b.as_i64()) {
        if let Some(out) = i64_op(x, y, op)? {
            return Ok(out);
        }
    }

    // Slow path: promote both sides to BigInt.
    let x = a.as_bigint().expect("int-like");
    let y = b.as_bigint().expect("int-like");
    match op {
        B::Add => Ok(Object::int_from_bigint(&x + &y)),
        B::Sub => Ok(Object::int_from_bigint(&x - &y)),
        B::Mult => Ok(Object::int_from_bigint(&x * &y)),
        B::Div => {
            if y.is_zero() {
                return Err(zero_division_error("division by zero"));
            }
            let xf = x
                .to_f64()
                .ok_or_else(|| value_error("int too large for float"))?;
            let yf = y
                .to_f64()
                .ok_or_else(|| value_error("int too large for float"))?;
            Ok(Object::Float(xf / yf))
        }
        B::FloorDiv => {
            if y.is_zero() {
                return Err(zero_division_error("integer division or modulo by zero"));
            }
            // num-bigint's div truncates toward zero; adjust for
            // floor semantics like CPython.
            use num_integer::Integer;
            let (q, _) = x.div_mod_floor(&y);
            Ok(Object::int_from_bigint(q))
        }
        B::Mod => {
            if y.is_zero() {
                return Err(zero_division_error("integer division or modulo by zero"));
            }
            use num_integer::Integer;
            let (_, r) = x.div_mod_floor(&y);
            Ok(Object::int_from_bigint(r))
        }
        B::Pow => {
            if y.is_negative() {
                let xf = x
                    .to_f64()
                    .ok_or_else(|| value_error("int too large for float"))?;
                let yf = y
                    .to_f64()
                    .ok_or_else(|| value_error("int too large for float"))?;
                return Ok(Object::Float(xf.powf(yf)));
            }
            let exp = y
                .to_u32()
                .ok_or_else(|| value_error("exponent too large for int.__pow__"))?;
            Ok(Object::int_from_bigint(x.pow(exp)))
        }
        B::LShift => {
            if y.is_negative() {
                return Err(value_error("negative shift count"));
            }
            let n = y
                .to_usize()
                .ok_or_else(|| value_error("shift count too large"))?;
            Ok(Object::int_from_bigint(x << n))
        }
        B::RShift => {
            if y.is_negative() {
                return Err(value_error("negative shift count"));
            }
            let n = y
                .to_usize()
                .ok_or_else(|| value_error("shift count too large"))?;
            Ok(Object::int_from_bigint(x >> n))
        }
        B::BitOr => Ok(Object::int_from_bigint(&x | &y)),
        B::BitXor => Ok(Object::int_from_bigint(&x ^ &y)),
        B::BitAnd => Ok(Object::int_from_bigint(&x & &y)),
        B::MatMult => Err(type_error(
            "unsupported operand type(s) for @: 'int' and 'int'".to_owned(),
        )),
    }
}

/// Try to perform an `i64` arithmetic op; return `None` on overflow,
/// `Some(out)` on success.
fn i64_op(x: i64, y: i64, op: BinOpKind) -> Result<Option<Object>, RuntimeError> {
    use BinOpKind as B;
    Ok(match op {
        B::Add => x.checked_add(y).map(Object::Int),
        B::Sub => x.checked_sub(y).map(Object::Int),
        B::Mult => x.checked_mul(y).map(Object::Int),
        B::Div => {
            if y == 0 {
                return Err(zero_division_error("division by zero"));
            }
            Some(Object::Float(x as f64 / y as f64))
        }
        B::FloorDiv => {
            if y == 0 {
                return Err(zero_division_error("integer division or modulo by zero"));
            }
            // Avoid overflow on i64::MIN / -1.
            if x == i64::MIN && y == -1 {
                None
            } else {
                let q = x / y;
                let r = x % y;
                let adjusted = if r != 0 && ((r < 0) != (y < 0)) {
                    q - 1
                } else {
                    q
                };
                Some(Object::Int(adjusted))
            }
        }
        B::Mod => {
            if y == 0 {
                return Err(zero_division_error("integer division or modulo by zero"));
            }
            let r = x % y;
            let adjusted = if r != 0 && ((r < 0) != (y < 0)) {
                r + y
            } else {
                r
            };
            Some(Object::Int(adjusted))
        }
        B::Pow => {
            if y < 0 {
                return Ok(Some(Object::Float((x as f64).powf(y as f64))));
            }
            // Fall through to bignum if the exponent is wide or if
            // the result is suspected to overflow.
            if let Ok(exp) = u32::try_from(y) {
                if exp <= 8 {
                    if let Some(r) = x.checked_pow(exp) {
                        return Ok(Some(Object::Int(r)));
                    }
                }
                // Larger exponents always go through bignum.
                None
            } else {
                None
            }
        }
        B::LShift => {
            if y < 0 {
                return Err(value_error("negative shift count"));
            }
            // Shifts beyond the i64 width fall through to bignum to
            // avoid silent wrapping.
            if y < 63 {
                let candidate = i128::from(x).wrapping_shl(y as u32);
                if let Ok(small) = i64::try_from(candidate) {
                    if i128::from(small) >> y == i128::from(x) {
                        Some(Object::Int(small))
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        }
        B::RShift => {
            if y < 0 {
                return Err(value_error("negative shift count"));
            }
            if y >= 64 {
                Some(Object::Int(if x < 0 { -1 } else { 0 }))
            } else {
                Some(Object::Int(x >> y))
            }
        }
        B::BitOr => Some(Object::Int(x | y)),
        B::BitXor => Some(Object::Int(x ^ y)),
        B::BitAnd => Some(Object::Int(x & y)),
        B::MatMult => {
            return Err(type_error(
                "unsupported operand type(s) for @: 'int' and 'int'".to_owned(),
            ))
        }
    })
}

/// Complex arithmetic. Mirrors CPython `complex.__add__` & friends.
/// PEP 585 — generic alias factory used as the fallback for
/// `BuiltinType[params]` when the class doesn't define
/// `__class_getitem__`. The result is a `SimpleNamespace`-shaped
/// object with `__origin__` and `__args__` attributes; `isinstance`
/// unwraps it via `__origin__` before walking the MRO.
fn make_generic_alias(origin: Object, params: Object) -> Object {
    let mut d = DictData::new();
    let args_tuple = match &params {
        Object::Tuple(_) => params.clone(),
        other => Object::new_tuple(vec![other.clone()]),
    };
    d.insert(DictKey(Object::from_static("__origin__")), origin);
    d.insert(DictKey(Object::from_static("__args__")), args_tuple);
    Object::SimpleNamespace(Rc::new(RefCell::new(d)))
}

fn complex_arith(
    (ar, ai): (f64, f64),
    (br, bi): (f64, f64),
    op: BinOpKind,
) -> Result<Object, RuntimeError> {
    use BinOpKind as B;
    match op {
        B::Add => Ok(Object::new_complex(ar + br, ai + bi)),
        B::Sub => Ok(Object::new_complex(ar - br, ai - bi)),
        B::Mult => Ok(Object::new_complex(ar * br - ai * bi, ar * bi + ai * br)),
        B::Div => {
            let denom = br * br + bi * bi;
            if denom == 0.0 {
                return Err(zero_division_error("complex division by zero"));
            }
            Ok(Object::new_complex(
                (ar * br + ai * bi) / denom,
                (ai * br - ar * bi) / denom,
            ))
        }
        B::Pow => {
            // Approximate via polar: r^n * cis(n*θ). Pure real
            // exponent is the common case; fall back to numerics.
            let base_mag = (ar * ar + ai * ai).sqrt();
            let base_arg = ai.atan2(ar);
            let exp_re = br;
            let exp_im = bi;
            let log_mag = base_mag.ln();
            // (a + bi)^(c + di) = exp((c + di) * (log_mag + i*arg))
            let new_log = exp_re * log_mag - exp_im * base_arg;
            let new_arg = exp_re * base_arg + exp_im * log_mag;
            let new_mag = new_log.exp();
            Ok(Object::new_complex(
                new_mag * new_arg.cos(),
                new_mag * new_arg.sin(),
            ))
        }
        _ => Err(type_error(format!(
            "unsupported operand type(s) for {}: 'complex' and 'complex'",
            op.as_str()
        ))),
    }
}

fn compare_op(a: &Object, b: &Object, op: CompareKind) -> Result<bool, RuntimeError> {
    // CPython lifts ``<``, ``<=``, ``>``, ``>=`` to subset/superset
    // tests on the set family. They are *not* total orderings, so we
    // intercept this before falling through to ``Object::cmp``.
    if let (Some(lhs), Some(rhs)) = (set_view(a), set_view(b)) {
        return Ok(compare_sets(&lhs, &rhs, op));
    }
    match op {
        CompareKind::Eq => Ok(a.eq_value(b)),
        CompareKind::NotEq => Ok(!a.eq_value(b)),
        CompareKind::Lt => Ok(a.cmp(b)?.is_lt()),
        CompareKind::LtE => Ok(a.cmp(b)?.is_le()),
        CompareKind::Gt => Ok(a.cmp(b)?.is_gt()),
        CompareKind::GtE => Ok(a.cmp(b)?.is_ge()),
    }
}

/// Snapshot a ``set``/``frozenset`` payload for subset comparison.
/// Returns ``None`` for any non-set so the caller can fall through
/// to the generic comparison path.
fn set_view(o: &Object) -> Option<crate::object::SetData> {
    match o {
        Object::Set(s) => Some(s.borrow().clone()),
        Object::FrozenSet(s) => Some((**s).clone()),
        _ => None,
    }
}

fn compare_sets(a: &crate::object::SetData, b: &crate::object::SetData, op: CompareKind) -> bool {
    match op {
        CompareKind::Eq => a == b,
        CompareKind::NotEq => a != b,
        CompareKind::LtE => a.iter().all(|k| b.contains(k)),
        CompareKind::Lt => a.len() < b.len() && a.iter().all(|k| b.contains(k)),
        CompareKind::GtE => b.iter().all(|k| a.contains(k)),
        CompareKind::Gt => a.len() > b.len() && b.iter().all(|k| a.contains(k)),
    }
}

fn promote_bool(o: &Object) -> Object {
    match o {
        Object::Bool(b) => Object::Int(i64::from(*b)),
        other => other.clone(),
    }
}

// ---------- RFC 0021 specialized comparison helpers ----------
//
// Each takes already-narrowed operands and a comparison kind and
// returns the boolean result. The dispatcher's specialized
// `COMPARE_OP_*` arms call these directly without paying for the
// dunder-method search or the deep-equality walk that
// `dispatch_compare_op` performs.

#[inline]
fn compare_int(a: i64, b: i64, op: CompareKind) -> bool {
    match op {
        CompareKind::Lt => a < b,
        CompareKind::LtE => a <= b,
        CompareKind::Eq => a == b,
        CompareKind::NotEq => a != b,
        CompareKind::Gt => a > b,
        CompareKind::GtE => a >= b,
    }
}

// Python's `==` on floats is bit-exact (and `==` ≠ `math.isclose`),
// so the float_cmp lint here would mask correctness, not catch a
// real bug.
#[allow(clippy::float_cmp)]
#[inline]
fn compare_float(a: f64, b: f64, op: CompareKind) -> bool {
    match op {
        CompareKind::Lt => a < b,
        CompareKind::LtE => a <= b,
        CompareKind::Eq => a == b,
        CompareKind::NotEq => a != b,
        CompareKind::Gt => a > b,
        CompareKind::GtE => a >= b,
    }
}

#[inline]
fn compare_str(a: &str, b: &str, op: CompareKind) -> bool {
    match op {
        CompareKind::Lt => a < b,
        CompareKind::LtE => a <= b,
        CompareKind::Eq => a == b,
        CompareKind::NotEq => a != b,
        CompareKind::Gt => a > b,
        CompareKind::GtE => a >= b,
    }
}

// ---------- public re-exports ----------

pub use object::Object as Value;

#[cfg(test)]
mod tests {
    use super::*;
    use weavepy_compiler::compile_module;
    use weavepy_parser::parse_module;

    fn run(src: &str) -> String {
        let module = parse_module(src).expect("parse");
        let code = compile_module(&module).expect("compile");
        let mut interp = Interpreter::new();
        let buf: Rc<RefCell<Vec<u8>>> = Rc::new(RefCell::new(Vec::new()));
        let writer: Stdout = buf.clone() as Rc<RefCell<dyn Write + Send + Sync>>;
        interp.set_stdout(writer);
        interp.run_module(&code).expect("run");
        let bytes = buf.borrow().clone();
        String::from_utf8(bytes).expect("utf-8")
    }

    #[test]
    fn runs_print_int() {
        assert_eq!(run("print(42)\n"), "42\n");
    }

    #[test]
    fn arithmetic_precedence() {
        assert_eq!(run("print(1 + 2 * 3)\n"), "7\n");
    }

    #[test]
    fn variable_assignment() {
        assert_eq!(run("x = 10\ny = 20\nprint(x + y)\n"), "30\n");
    }

    #[test]
    fn if_else() {
        assert_eq!(
            run("x = 5\nif x > 0:\n    print('positive')\nelse:\n    print('np')\n"),
            "positive\n"
        );
    }

    #[test]
    fn while_loop() {
        assert_eq!(
            run("i = 0\nwhile i < 3:\n    print(i)\n    i = i + 1\n"),
            "0\n1\n2\n"
        );
    }

    #[test]
    fn for_loop_range() {
        assert_eq!(run("for i in range(3):\n    print(i)\n"), "0\n1\n2\n");
    }

    #[test]
    fn function_call() {
        let src = "def add(a, b):\n    return a + b\nprint(add(2, 3))\n";
        assert_eq!(run(src), "5\n");
    }

    #[test]
    fn closure() {
        let src = "def make_adder(x):\n    def add(y):\n        return x + y\n    return add\nadd5 = make_adder(5)\nprint(add5(3))\n";
        assert_eq!(run(src), "8\n");
    }

    #[test]
    fn list_comprehension() {
        let src = "xs = [x * x for x in range(4)]\nprint(xs)\n";
        assert_eq!(run(src), "[0, 1, 4, 9]\n");
    }

    #[test]
    fn fibonacci() {
        let src = "def fib(n):\n    if n < 2:\n        return n\n    return fib(n - 1) + fib(n - 2)\nprint(fib(10))\n";
        assert_eq!(run(src), "55\n");
    }

    #[test]
    fn simple_class() {
        let src = "class Greeter:\n    def __init__(self, name):\n        self.name = name\n    def hello(self):\n        return 'hello, ' + self.name\ng = Greeter('Owen')\nprint(g.hello())\n";
        assert_eq!(run(src), "hello, Owen\n");
    }

    #[test]
    fn class_attribute_default() {
        let src = "class C:\n    x = 1\nc = C()\nprint(c.x)\n";
        assert_eq!(run(src), "1\n");
    }

    #[test]
    fn single_inheritance() {
        let src = "class Animal:\n    def speak(self):\n        return 'generic'\nclass Dog(Animal):\n    def speak(self):\n        return 'woof'\nprint(Dog().speak())\nprint(Animal().speak())\n";
        assert_eq!(run(src), "woof\ngeneric\n");
    }

    #[test]
    fn isinstance_with_class() {
        let src = "class A: pass\nclass B(A): pass\nb = B()\nprint(isinstance(b, A))\nprint(isinstance(b, B))\nprint(isinstance(1, int))\n";
        assert_eq!(run(src), "True\nTrue\nTrue\n");
    }

    #[test]
    fn try_except_catches() {
        let src = "try:\n    raise ValueError('boom')\nexcept ValueError as e:\n    print('caught', e.args[0])\n";
        assert_eq!(run(src), "caught boom\n");
    }

    #[test]
    fn try_finally_runs() {
        let src = "x = 0\ntry:\n    x = 1\nfinally:\n    x = x + 10\nprint(x)\n";
        assert_eq!(run(src), "11\n");
    }

    #[test]
    fn raise_propagates_from_function() {
        let src = "def boom():\n    raise ValueError('nope')\ntry:\n    boom()\nexcept ValueError:\n    print('handled')\n";
        assert_eq!(run(src), "handled\n");
    }

    #[test]
    fn with_statement_calls_exit() {
        let src = "class CM:\n    def __enter__(self):\n        print('enter')\n        return self\n    def __exit__(self, t, v, tb):\n        print('exit')\nwith CM() as c:\n    print('body')\n";
        assert_eq!(run(src), "enter\nbody\nexit\n");
    }

    #[test]
    fn except_does_not_match_other() {
        let src = "try:\n    raise KeyError('k')\nexcept ValueError:\n    print('value')\nexcept KeyError:\n    print('key')\n";
        assert_eq!(run(src), "key\n");
    }

    #[test]
    fn dunder_add_dispatch() {
        let src = "class Vec:\n    def __init__(self, x):\n        self.x = x\n    def __add__(self, other):\n        return Vec(self.x + other.x)\nv = Vec(2) + Vec(3)\nprint(v.x)\n";
        assert_eq!(run(src), "5\n");
    }

    #[test]
    fn dunder_repr_used_by_print() {
        let src = "class P:\n    def __repr__(self):\n        return 'P!'\nprint(P())\n";
        assert_eq!(run(src), "P!\n");
    }

    #[test]
    fn dunder_str_overrides_repr() {
        let src = concat!(
            "class P:\n",
            "    def __repr__(self):\n",
            "        return 'P-repr'\n",
            "    def __str__(self):\n",
            "        return 'P-str'\n",
            "print(P())\n",
            "print(repr(P()))\n"
        );
        assert_eq!(run(src), "P-str\nP-repr\n");
    }

    #[test]
    fn dunder_len_dispatch() {
        let src = concat!(
            "class C:\n",
            "    def __len__(self):\n",
            "        return 7\n",
            "print(len(C()))\n"
        );
        assert_eq!(run(src), "7\n");
    }

    #[test]
    fn super_two_arg_form() {
        let src = concat!(
            "class A:\n",
            "    def hello(self):\n",
            "        return 'A'\n",
            "class B(A):\n",
            "    def hello(self):\n",
            "        return 'B-' + super(B, self).hello()\n",
            "print(B().hello())\n"
        );
        assert_eq!(run(src), "B-A\n");
    }

    #[test]
    fn nested_try_except() {
        let src = concat!(
            "try:\n",
            "    try:\n",
            "        raise ValueError('inner')\n",
            "    except ValueError:\n",
            "        print('caught inner')\n",
            "        raise RuntimeError('outer')\n",
            "except RuntimeError as r:\n",
            "    print('caught outer', r.args[0])\n"
        );
        assert_eq!(run(src), "caught inner\ncaught outer outer\n");
    }

    #[test]
    fn raise_from_chains_cause() {
        let src = concat!(
            "try:\n",
            "    try:\n",
            "        raise ValueError('inner')\n",
            "    except ValueError as e:\n",
            "        raise RuntimeError('outer') from e\n",
            "except RuntimeError as r:\n",
            "    print(type(r).__name__)\n",
            "    print(r.args[0])\n"
        );
        assert_eq!(run(src), "RuntimeError\nouter\n");
    }

    #[test]
    fn imports_math_module() {
        let src = concat!(
            "import math\n",
            "print(math.floor(3.7))\n",
            "print(int(math.sqrt(9)))\n",
        );
        assert_eq!(run(src), "3\n3\n");
    }

    #[test]
    fn from_import_binds_names() {
        let src = concat!(
            "from math import floor, gcd\n",
            "print(floor(2.9))\n",
            "print(gcd(8, 12))\n",
        );
        assert_eq!(run(src), "2\n4\n");
    }

    #[test]
    fn import_as_renames() {
        let src = concat!(
            "import math as m\n",
            "from math import pi as P\n",
            "print(int(m.pow(2, 5)))\n",
            "print(round(P, 4))\n",
        );
        assert_eq!(run(src), "32\n3.1416\n");
    }

    #[test]
    fn missing_module_raises_module_not_found_error() {
        let src = concat!(
            "try:\n",
            "    import does_not_exist\n",
            "except ModuleNotFoundError as e:\n",
            "    print('caught', type(e).__name__)\n",
        );
        assert_eq!(run(src), "caught ModuleNotFoundError\n");
    }

    #[test]
    fn dotted_import_walks_attributes() {
        let src = concat!(
            "import os.path\n",
            "print(os.path.basename('/a/b/c.txt'))\n",
        );
        assert_eq!(run(src), "c.txt\n");
    }

    #[test]
    fn class_iter_protocol() {
        let src = concat!(
            "class Count:\n",
            "    def __init__(self, n):\n",
            "        self.n = n\n",
            "        self.i = 0\n",
            "    def __iter__(self):\n",
            "        return self\n",
            "    def __next__(self):\n",
            "        if self.i >= self.n:\n",
            "            raise StopIteration\n",
            "        v = self.i\n",
            "        self.i = v + 1\n",
            "        return v\n",
            "for x in Count(3):\n",
            "    print(x)\n"
        );
        assert_eq!(run(src), "0\n1\n2\n");
    }

    // ---------- f-strings (RFC 0005) ----------

    #[test]
    fn fstring_plain_interpolation() {
        let src = "name = 'Owen'\nprint(f'hello, {name}!')\n";
        assert_eq!(run(src), "hello, Owen!\n");
    }

    #[test]
    fn fstring_expression() {
        let src = "x = 2\ny = 3\nprint(f'{x} + {y} = {x + y}')\n";
        assert_eq!(run(src), "2 + 3 = 5\n");
    }

    #[test]
    fn fstring_format_spec_fixed() {
        let src = "import math\nprint(f'{math.pi:.3f}')\n";
        assert_eq!(run(src), "3.142\n");
    }

    #[test]
    fn fstring_format_spec_width_align() {
        let src = "print(f'[{42:>5}]')\nprint(f'[{42:<5}]')\nprint(f'[{42:^5}]')\n";
        assert_eq!(run(src), "[   42]\n[42   ]\n[ 42  ]\n");
    }

    #[test]
    fn fstring_repr_conversion() {
        let src = "s = 'hi'\nprint(f'{s!r}')\n";
        assert_eq!(run(src), "'hi'\n");
    }

    #[test]
    fn fstring_hex_and_binary() {
        let src = "print(f'{255:#x} {7:b}')\n";
        assert_eq!(run(src), "0xff 111\n");
    }

    // ---------- generators (RFC 0006) ----------

    #[test]
    fn generator_basic_yield() {
        let src = concat!(
            "def gen():\n",
            "    yield 1\n",
            "    yield 2\n",
            "    yield 3\n",
            "for x in gen():\n",
            "    print(x)\n",
        );
        assert_eq!(run(src), "1\n2\n3\n");
    }

    #[test]
    fn generator_next_then_loop() {
        let src = concat!(
            "def gen():\n",
            "    yield 'a'\n",
            "    yield 'b'\n",
            "g = gen()\n",
            "print(next(g))\n",
            "print(next(g))\n",
        );
        assert_eq!(run(src), "a\nb\n");
    }

    #[test]
    fn generator_yield_from() {
        let src = concat!(
            "def inner():\n",
            "    yield 1\n",
            "    yield 2\n",
            "def outer():\n",
            "    yield from inner()\n",
            "    yield 3\n",
            "for x in outer():\n",
            "    print(x)\n",
        );
        assert_eq!(run(src), "1\n2\n3\n");
    }

    #[test]
    fn generator_expression_is_lazy() {
        let src = concat!("g = (x * x for x in range(4))\n", "print(list(g))\n",);
        assert_eq!(run(src), "[0, 1, 4, 9]\n");
    }

    #[test]
    fn generator_returns_value_in_stopiteration() {
        let src = concat!(
            "def gen():\n",
            "    yield 1\n",
            "    return 'done'\n",
            "g = gen()\n",
            "print(next(g))\n",
            "try:\n",
            "    next(g)\n",
            "except StopIteration as e:\n",
            "    print(e.value)\n",
        );
        assert_eq!(run(src), "1\ndone\n");
    }

    // ---------- pattern matching (RFC 0009) ----------

    #[test]
    fn match_literal_and_wildcard() {
        let src = concat!(
            "def describe(x):\n",
            "    match x:\n",
            "        case 0:\n",
            "            return 'zero'\n",
            "        case 1:\n",
            "            return 'one'\n",
            "        case _:\n",
            "            return 'many'\n",
            "print(describe(0))\n",
            "print(describe(1))\n",
            "print(describe(7))\n",
        );
        assert_eq!(run(src), "zero\none\nmany\n");
    }

    #[test]
    fn match_capture_with_guard() {
        let src = concat!(
            "def sign(x):\n",
            "    match x:\n",
            "        case n if n > 0:\n",
            "            return 'pos'\n",
            "        case n if n < 0:\n",
            "            return 'neg'\n",
            "        case _:\n",
            "            return 'zero'\n",
            "print(sign(5))\n",
            "print(sign(-3))\n",
            "print(sign(0))\n",
        );
        assert_eq!(run(src), "pos\nneg\nzero\n");
    }

    #[test]
    fn match_sequence_pattern() {
        let src = concat!(
            "def head(xs):\n",
            "    match xs:\n",
            "        case []:\n",
            "            return 'empty'\n",
            "        case [a]:\n",
            "            return ('one', a)\n",
            "        case [a, *rest]:\n",
            "            return ('many', a, rest)\n",
            "print(head([]))\n",
            "print(head([1]))\n",
            "print(head([1, 2, 3]))\n",
        );
        let out = run(src);
        assert!(out.contains("empty"));
        assert!(out.contains("one"));
        assert!(out.contains("many"));
    }

    #[test]
    fn match_or_pattern() {
        let src = concat!(
            "def label(x):\n",
            "    match x:\n",
            "        case 0 | 1 | 2:\n",
            "            return 'small'\n",
            "        case _:\n",
            "            return 'large'\n",
            "print(label(0))\n",
            "print(label(2))\n",
            "print(label(99))\n",
        );
        assert_eq!(run(src), "small\nsmall\nlarge\n");
    }
}
