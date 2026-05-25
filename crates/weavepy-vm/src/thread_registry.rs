//! Cross-thread registry of live OS threads — RFC 0024.
//!
//! Each call to `_thread.start_new_thread` registers an entry
//! in the global registry indexed by the OS thread id. The
//! entry holds:
//!
//! - The `JoinHandle<()>` so `Thread.join()` can wait for the
//!   target to finish.
//! - A `RealEvent` that the target signals on exit so any
//!   waiter (the join, the daemon-shutdown sweep, the
//!   `_set_sentinel` lock) can observe completion.
//! - The thread's "name" (set by `threading.Thread(name=...)`).
//! - Whether the thread is daemon (controls interpreter
//!   shutdown behaviour).
//! - A short identification string for debug logging.
//!
//! The registry is `Send + Sync`; it lives in a `OnceLock<…>`
//! so the first thread spawn lazily initialises it.
//!
//! Shutdown semantics follow CPython:
//!
//! - Non-daemon threads block interpreter shutdown until
//!   they've finished. The main thread joins each one in turn.
//! - Daemon threads are abandoned (their stacks leaked) when
//!   the interpreter exits. Best practice is to set
//!   `Thread.daemon = True` on long-running background workers
//!   that don't matter at shutdown.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use parking_lot::RwLock;

use crate::sync::RealEvent;

/// Per-OS-thread bookkeeping owned by the global registry.
#[allow(missing_debug_implementations)]
pub struct ThreadEntry {
    /// OS-level thread id (via `pthread_self` on POSIX,
    /// `GetCurrentThreadId` on Windows). Stable for the
    /// thread's lifetime.
    pub native_id: u64,
    /// User-supplied thread name from `threading.Thread(name=...)`.
    pub name: String,
    /// `True` if `Thread.daemon = True` was set before
    /// `start()`.
    pub daemon: AtomicBool,
    /// Set when the worker exits (either normally or via an
    /// uncaught exception).
    pub finished: Arc<RealEvent>,
    /// `JoinHandle<()>`. `None` after the entry has been
    /// joined; left intact otherwise so a second `join()`
    /// is a no-op.
    pub handle: parking_lot::Mutex<Option<JoinHandle<()>>>,
    /// Whether `Thread.start()` has actually called the
    /// target. `False` while the thread is queued but not
    /// yet running.
    pub started: AtomicBool,
    /// RFC 0025: the per-thread sentinel lock that the worker
    /// pre-acquires on entry and releases on exit. `Thread.join`
    /// blocks on `lock.acquire(timeout=…)`, which drops the GIL
    /// while waiting so other threads can run. `None` for the
    /// main thread.
    pub join_lock: parking_lot::Mutex<Option<Arc<crate::sync::RealLock>>>,
}

impl ThreadEntry {
    pub fn new(native_id: u64, name: String, daemon: bool, handle: JoinHandle<()>) -> Self {
        Self {
            native_id,
            name,
            daemon: AtomicBool::new(daemon),
            finished: Arc::new(RealEvent::new()),
            handle: parking_lot::Mutex::new(Some(handle)),
            started: AtomicBool::new(false),
            join_lock: parking_lot::Mutex::new(None),
        }
    }

    /// Attach the sentinel lock that workers spin against on
    /// `Thread.join(timeout=…)`. Released by the worker body on
    /// exit (RFC 0025).
    pub fn attach_join_lock(&self, lock: Arc<crate::sync::RealLock>) {
        *self.join_lock.lock() = Some(lock);
    }

    pub fn is_daemon(&self) -> bool {
        self.daemon.load(Ordering::Acquire)
    }

    pub fn set_daemon(&self, value: bool) {
        self.daemon.store(value, Ordering::Release);
    }

    pub fn is_alive(&self) -> bool {
        !self.finished.is_set()
    }

    pub fn mark_finished(&self) {
        self.finished.set();
    }

    pub fn is_started(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }

    pub fn mark_started(&self) {
        self.started.store(true, Ordering::Release);
    }

    /// Block until the thread exits, optionally with a timeout.
    pub fn join(&self, timeout: Option<Duration>) -> bool {
        if let Some(t) = timeout {
            self.finished.wait_timeout(t)
        } else {
            self.finished.wait()
        }
    }

    /// Take ownership of the JoinHandle, if we still hold it.
    pub fn take_handle(&self) -> Option<JoinHandle<()>> {
        self.handle.lock().take()
    }
}

#[allow(missing_debug_implementations)]
pub struct ThreadRegistry {
    entries: RwLock<BTreeMap<u64, Arc<ThreadEntry>>>,
    /// Synthetic id used for Python-level `threading.get_ident`
    /// when we want monotonically-increasing values rather than
    /// the underlying pthread handle.
    next_synthetic: AtomicU64,
    /// Native id of the main interpreter thread, captured at
    /// `Interpreter::default()`. Distinct from worker threads.
    pub main_native_id: AtomicU64,
}

impl Default for ThreadRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadRegistry {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(BTreeMap::new()),
            next_synthetic: AtomicU64::new(1),
            main_native_id: AtomicU64::new(0),
        }
    }

    pub fn register(&self, entry: Arc<ThreadEntry>) {
        let mut g = self.entries.write();
        g.insert(entry.native_id, entry);
    }

    pub fn unregister(&self, native_id: u64) {
        let mut g = self.entries.write();
        g.remove(&native_id);
    }

    pub fn get(&self, native_id: u64) -> Option<Arc<ThreadEntry>> {
        let g = self.entries.read();
        g.get(&native_id).cloned()
    }

    /// Return all currently-registered (live) threads.
    pub fn alive(&self) -> Vec<Arc<ThreadEntry>> {
        let g = self.entries.read();
        g.values().filter(|e| e.is_alive()).cloned().collect()
    }

    pub fn all(&self) -> Vec<Arc<ThreadEntry>> {
        let g = self.entries.read();
        g.values().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.read().is_empty()
    }

    /// Allocate a fresh synthetic id. Useful when the
    /// platform's native id is not stable enough.
    pub fn next_id(&self) -> u64 {
        self.next_synthetic.fetch_add(1, Ordering::AcqRel)
    }

    /// Joins all non-daemon threads. Called at interpreter
    /// shutdown so user-visible work runs to completion before
    /// the process exits.
    pub fn join_non_daemon(&self) {
        let entries: Vec<_> = self
            .entries
            .read()
            .values()
            .filter(|e| !e.is_daemon())
            .cloned()
            .collect();
        for entry in entries {
            // Wait for the finished event first (cooperative);
            // then take and join the handle to surface any panic.
            entry.finished.wait();
            if let Some(h) = entry.take_handle() {
                let _ = h.join();
            }
        }
    }
}

/// The process-wide thread registry. Initialised on first use.
pub fn registry() -> &'static ThreadRegistry {
    static REGISTRY: OnceLock<ThreadRegistry> = OnceLock::new();
    REGISTRY.get_or_init(ThreadRegistry::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn registry_register_unregister() {
        let r = ThreadRegistry::new();
        let h = thread::spawn(|| ());
        let entry = Arc::new(ThreadEntry::new(123, "t".into(), false, h));
        r.register(entry.clone());
        assert!(r.get(123).is_some());
        r.unregister(123);
        assert!(r.get(123).is_none());
    }

    #[test]
    fn entry_join_completes() {
        let h = thread::spawn(|| ());
        let entry = Arc::new(ThreadEntry::new(1, "t".into(), false, h));
        let e2 = entry.clone();
        thread::spawn(move || {
            e2.mark_finished();
        });
        entry.join(Some(Duration::from_secs(1)));
        assert!(!entry.is_alive());
    }
}
