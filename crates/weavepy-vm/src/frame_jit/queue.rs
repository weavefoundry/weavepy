//! Code generation for frame compiles, on a background thread.
//!
//! A frame compile has two halves. Lowering a code object to Cranelift IR
//! reads the interpreter's state (the code's caches, its sites' settled
//! values) and allocates the caches the code will address, so it runs on
//! the interpreter's thread. Code generation (register allocation and
//! emission, about two thirds of a compile) reads only the IR, so it runs
//! here, on one worker thread, while the interpreter keeps running the code
//! in the core loop. The interpreter takes the finished machine code at its
//! next safe point (see `frame_jit::drain`), defines it in the engine's
//! module and publishes the code's native form as a synchronous compile
//! would.
//!
//! One worker takes the jobs in order, one at a time, and frees each
//! compile's working memory before the next: the process's peak memory
//! is the largest compile's, as before. The queue is bounded (see
//! [`full`]): a code object that gets hot while it's full waits for room.
//!
//! `WEAVEPY_JIT_SYNC=1` generates the code on the interpreter's thread, at
//! the point the code got hot, as tests that expect a compile at a
//! deterministic point need.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::ir::Function;
use cranelift_codegen::isa::{OwnedTargetIsa, TargetIsa};
use cranelift_module::{FuncId, ModuleReloc};

/// One function of a compile: the code's, or one of its chunks'.
pub(super) struct Unit {
    /// Its id in the engine's module (declared when it was lowered).
    pub id: FuncId,
    pub func: Function,
    /// For `WEAVEPY_FRAME_JIT_STATS` and `WEAVEPY_FRAME_JIT_VERIFY`: the
    /// code's name (and the chunk's range), whether it was lowered lean,
    /// and how long the lowering took (with stats on).
    pub label: String,
    pub lean: bool,
    pub lowered: Option<Duration>,
}

/// A unit's machine code, ready to define in the module.
pub(super) struct Machine {
    pub id: FuncId,
    pub align: u64,
    pub bytes: Vec<u8>,
    pub relocs: Vec<ModuleReloc>,
}

/// What code generation made of a compile's units.
pub(super) enum Outcome {
    Ready(Vec<Machine>),
    /// Cranelift refused a unit (the code stays interpreted).
    Failed,
    /// The code generator panicked: frame compiles stop for the process.
    Panicked,
}

/// Generate the machine code of `units`, on whichever thread: each unit's
/// working memory is freed before the next.
pub(super) fn generate(isa: &dyn TargetIsa, units: Vec<Unit>) -> Outcome {
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut out = Vec::with_capacity(units.len());
        for unit in units {
            match generate_one(isa, unit) {
                Some(m) => out.push(m),
                None => return Outcome::Failed,
            }
        }
        Outcome::Ready(out)
    }));
    run.unwrap_or(Outcome::Panicked)
}

fn generate_one(isa: &dyn TargetIsa, unit: Unit) -> Option<Machine> {
    let Unit {
        id,
        func,
        label,
        lean,
        lowered,
    } = unit;
    let insts = func.dfg.num_insts();
    let blocks = func.dfg.num_blocks();
    let t1 = lowered.map(|_| std::time::Instant::now());
    let mut ctx = cranelift_codegen::Context::for_function(func);
    let align = match ctx.compile(isa, &mut ControlPlane::default()) {
        Ok(compiled) => u64::from(compiled.buffer.alignment),
        Err(e) => {
            if super::verify() {
                eprintln!("weavepy: frame {label:?} failed to compile: {e:?}");
            }
            return None;
        }
    };
    let compiled = ctx.compiled_code()?;
    let bytes = compiled.code_buffer().to_vec();
    let relocs = compiled
        .buffer
        .relocs()
        .iter()
        .map(|r| ModuleReloc::from_mach_reloc(r, &ctx.func, id))
        .collect();
    if let (Some(lowered), Some(t1)) = (lowered, t1) {
        eprintln!(
            "frame jit: {label} lowered in {lowered:?} ({insts} IR instructions, {blocks} blocks{}, {} bytes), compiled in {:?}",
            if lean { ", lean" } else { "" },
            bytes.len(),
            t1.elapsed()
        );
    }
    Some(Machine {
        id,
        align,
        bytes,
        relocs,
    })
}

/// A compile waiting for, or in, code generation.
pub(super) struct Job {
    pub ticket: u64,
    pub units: Vec<Unit>,
    /// The jobs' IR instructions, for the queue's bound.
    pub size: usize,
}

struct Queue {
    jobs: VecDeque<Job>,
    /// Finished jobs (their tickets, sizes and outcomes), until the
    /// interpreter takes them.
    done: Vec<(u64, usize, Outcome)>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// The worker is to exit once the queue is empty (see [`quiesce`]).
    stop: bool,
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue {
    jobs: VecDeque::new(),
    done: Vec::new(),
    worker: None,
    stop: false,
});
static WAKE: Condvar = Condvar::new();

/// Whether finished jobs wait for the interpreter (see [`take_done`]).
static READY: AtomicBool = AtomicBool::new(false);

/// Jobs submitted and not yet taken back, and their IR instructions.
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static IN_FLIGHT_IR: AtomicUsize = AtomicUsize::new(0);

/// At most this many jobs, or this many IR instructions of them, are in
/// flight at once (beyond one job): a burst of code getting hot together
/// waits its turn instead of holding its lowered IR in memory.
const MAX_JOBS: usize = 8;
const MAX_IR: usize = 24_000;

fn lock() -> MutexGuard<'static, Queue> {
    QUEUE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether finished jobs wait for the interpreter: one load, for the
/// warm-up counters to test.
#[inline(always)]
pub(super) fn ready() -> bool {
    READY.load(Ordering::Relaxed)
}

/// Whether the queue has no room for another job.
pub(super) fn full() -> bool {
    let jobs = IN_FLIGHT.load(Ordering::Relaxed);
    jobs >= MAX_JOBS || (jobs > 0 && IN_FLIGHT_IR.load(Ordering::Relaxed) >= MAX_IR)
}

/// Queue `job` for the worker (started if it isn't running), or hand it
/// back when no worker thread can start.
pub(super) fn submit(isa: &OwnedTargetIsa, job: Job) -> Result<(), Job> {
    let mut q = lock();
    if q.worker.is_none() {
        let isa = isa.clone();
        match std::thread::Builder::new()
            .name("weavepy-jit-compile".to_owned())
            .stack_size(8 << 20)
            .spawn(move || work(isa))
        {
            Ok(handle) => q.worker = Some(handle),
            Err(_) => return Err(job),
        }
    }
    IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
    IN_FLIGHT_IR.fetch_add(job.size, Ordering::Relaxed);
    q.jobs.push_back(job);
    drop(q);
    WAKE.notify_one();
    Ok(())
}

/// The worker: generate each job's code in turn, until told to stop with
/// nothing left to do.
fn work(isa: OwnedTargetIsa) {
    loop {
        let job = {
            let mut q = lock();
            loop {
                if let Some(job) = q.jobs.pop_front() {
                    break job;
                }
                if q.stop {
                    return;
                }
                q = WAKE.wait(q).unwrap_or_else(PoisonError::into_inner);
            }
        };
        let (ticket, size) = (job.ticket, job.size);
        let outcome = generate(&*isa, job.units);
        // (Its size counts against the bound until the interpreter takes
        // it.)
        lock().done.push((ticket, size, outcome));
        READY.store(true, Ordering::Release);
        // Nothing else allocates on this thread, so its pages are free now:
        // they would stay resident on top of the interpreter's heap,
        // unused until the next compile, if it comes.
        crate::malloc_stats::release_free_memory();
    }
}

/// The finished jobs' tickets and outcomes, which leave the queue's
/// bound.
pub(super) fn take_done() -> Vec<(u64, Outcome)> {
    if !READY.swap(false, Ordering::Acquire) {
        return Vec::new();
    }
    let done = std::mem::take(&mut lock().done);
    IN_FLIGHT.fetch_sub(done.len(), Ordering::Relaxed);
    let ir: usize = done.iter().map(|d| d.1).sum();
    IN_FLIGHT_IR.fetch_sub(ir, Ordering::Relaxed);
    done.into_iter().map(|(t, _, o)| (t, o)).collect()
}

/// Let the worker finish the queued jobs and exit, before `fork()`: the
/// child then inherits no thread partway through a compile (holding a lock
/// the child would wait on forever), the finished jobs wait for the
/// interpreter in parent and child alike, and `os.fork` doesn't count the
/// worker as a thread of a multi-threaded process. The next job starts a
/// new worker.
pub(crate) fn quiesce() {
    let handle = {
        let mut q = lock();
        let Some(handle) = q.worker.take() else {
            return;
        };
        q.stop = true;
        handle
    };
    let before = crate::stdlib::os_process::count_os_threads();
    WAKE.notify_all();
    let _ = handle.join();
    lock().stop = false;
    // The kernel retires a joined thread a moment after the join returns:
    // wait for that (briefly), or `os.fork` would still count it.
    if let Some(before) = before {
        let deadline = std::time::Instant::now() + Duration::from_millis(50);
        while crate::stdlib::os_process::count_os_threads().is_some_and(|n| n >= before)
            && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
    }
}
