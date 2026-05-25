//! Thread-safe synchronisation primitives — RFC 0024.
//!
//! Today most of the runtime lives behind `Rc<…>` / `Rc<RefCell<…>>`,
//! which is fine because the dispatch loop only ever runs on one
//! OS thread. After RFC 0024, real OS threads exist; the `Object`
//! enum stays Rc-rooted (each interpreter owns its own subtree)
//! but cross-thread coordination — `threading.Lock`, `Queue`,
//! `Event`, `Condition`, etc. — needs `Send + Sync` types.
//!
//! This module owns those primitives. They are constructed in one
//! interpreter, attached to an `Object::Lock(Rc<RealLock>)`-shaped
//! variant, and shipped to other threads via the cross-thread
//! channel infrastructure that pickles closures on
//! `_thread.start_new_thread`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex, ReentrantMutex};

/// A real cross-thread mutex with CPython `_thread.LockType`
/// semantics. `acquire(blocking=True, timeout=-1)` blocks until
/// the lock is available; `acquire(blocking=False)` returns
/// immediately. `release()` unlocks; calling release on an
/// unlocked lock raises `RuntimeError` upstream.
///
/// The lock is *not* reentrant — see [`RealRLock`] for that. The
/// owner thread id is tracked so cross-thread releases can be
/// detected and surfaced as `RuntimeError("release unlocked
/// lock")` per CPython.
#[derive(Debug)]
pub struct RealLock {
    state: Mutex<LockState>,
    cv: Condvar,
}

#[derive(Debug, Default)]
struct LockState {
    held: bool,
    owner: Option<u64>,
}

impl Default for RealLock {
    fn default() -> Self {
        Self::new()
    }
}

impl RealLock {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(LockState::default()),
            cv: Condvar::new(),
        }
    }

    /// Try once without blocking. Returns `true` if acquired.
    pub fn try_acquire(&self, owner: u64) -> bool {
        let mut state = self.state.lock();
        if state.held {
            false
        } else {
            state.held = true;
            state.owner = Some(owner);
            true
        }
    }

    /// Acquire, blocking forever if necessary.
    pub fn acquire(&self, owner: u64) {
        let mut state = self.state.lock();
        while state.held {
            self.cv.wait(&mut state);
        }
        state.held = true;
        state.owner = Some(owner);
    }

    /// Acquire, blocking until the deadline. Returns `true` if
    /// acquired in time.
    pub fn acquire_timeout(&self, owner: u64, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock();
        while state.held {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let remaining = deadline - now;
            let result = self.cv.wait_for(&mut state, remaining);
            if result.timed_out() && state.held {
                return false;
            }
        }
        state.held = true;
        state.owner = Some(owner);
        true
    }

    /// Release the lock. Returns `Ok(())` on success, `Err` if
    /// the lock isn't held.
    pub fn release(&self) -> Result<(), &'static str> {
        let mut state = self.state.lock();
        if !state.held {
            return Err("release unlocked lock");
        }
        state.held = false;
        state.owner = None;
        drop(state);
        self.cv.notify_one();
        Ok(())
    }

    pub fn is_locked(&self) -> bool {
        self.state.lock().held
    }
}

/// A real cross-thread reentrant mutex with CPython
/// `_thread.RLock` semantics. Multiple `acquire()`s from the
/// same OS thread succeed without blocking; release counts
/// down to zero before the lock actually becomes free.
#[derive(Debug)]
pub struct RealRLock {
    state: Mutex<RLockState>,
    cv: Condvar,
}

#[derive(Debug, Default)]
struct RLockState {
    owner: Option<u64>,
    depth: usize,
}

impl Default for RealRLock {
    fn default() -> Self {
        Self::new()
    }
}

impl RealRLock {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(RLockState::default()),
            cv: Condvar::new(),
        }
    }

    pub fn try_acquire(&self, owner: u64) -> bool {
        let mut state = self.state.lock();
        match state.owner {
            Some(o) if o == owner => {
                state.depth += 1;
                true
            }
            None => {
                state.owner = Some(owner);
                state.depth = 1;
                true
            }
            _ => false,
        }
    }

    pub fn acquire(&self, owner: u64) {
        let mut state = self.state.lock();
        loop {
            match state.owner {
                Some(o) if o == owner => {
                    state.depth += 1;
                    return;
                }
                None => {
                    state.owner = Some(owner);
                    state.depth = 1;
                    return;
                }
                _ => self.cv.wait(&mut state),
            }
        }
    }

    pub fn acquire_timeout(&self, owner: u64, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock();
        loop {
            match state.owner {
                Some(o) if o == owner => {
                    state.depth += 1;
                    return true;
                }
                None => {
                    state.owner = Some(owner);
                    state.depth = 1;
                    return true;
                }
                _ => {
                    let now = Instant::now();
                    if now >= deadline {
                        return false;
                    }
                    let result = self.cv.wait_for(&mut state, deadline - now);
                    if result.timed_out() && state.owner.is_some() && state.owner != Some(owner) {
                        return false;
                    }
                }
            }
        }
    }

    /// Release. Returns the resulting depth (0 means fully released).
    pub fn release(&self, owner: u64) -> Result<usize, &'static str> {
        let mut state = self.state.lock();
        match state.owner {
            Some(o) if o == owner => {
                state.depth -= 1;
                if state.depth == 0 {
                    state.owner = None;
                    drop(state);
                    self.cv.notify_one();
                    Ok(0)
                } else {
                    Ok(state.depth)
                }
            }
            Some(_) => Err("cannot release foreign lock"),
            None => Err("cannot release un-acquired lock"),
        }
    }

    pub fn is_owned_by(&self, owner: u64) -> bool {
        self.state.lock().owner == Some(owner)
    }

    pub fn depth(&self) -> usize {
        self.state.lock().depth
    }
}

/// A real cross-thread `threading.Event`. `set()` flips a
/// boolean; `clear()` flips it back; `wait(timeout=None)` blocks
/// until set or until the timeout expires. Backed by `Condvar`
/// so wakeups are immediate.
#[derive(Debug)]
pub struct RealEvent {
    state: Mutex<bool>,
    cv: Condvar,
}

impl Default for RealEvent {
    fn default() -> Self {
        Self::new()
    }
}

impl RealEvent {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(false),
            cv: Condvar::new(),
        }
    }

    pub fn is_set(&self) -> bool {
        *self.state.lock()
    }

    pub fn set(&self) {
        let mut state = self.state.lock();
        *state = true;
        drop(state);
        self.cv.notify_all();
    }

    pub fn clear(&self) {
        *self.state.lock() = false;
    }

    /// Block until set. Always returns `true`.
    pub fn wait(&self) -> bool {
        let mut state = self.state.lock();
        while !*state {
            self.cv.wait(&mut state);
        }
        true
    }

    /// Block until set or until `timeout` elapses. Returns
    /// the flag's value at the time of return.
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock();
        while !*state {
            let now = Instant::now();
            if now >= deadline {
                return *state;
            }
            self.cv.wait_for(&mut state, deadline - now);
        }
        *state
    }
}

/// Real `threading.Semaphore`. A counter that `acquire`
/// decrements (blocking when zero) and `release` increments.
#[derive(Debug)]
pub struct RealSemaphore {
    state: Mutex<usize>,
    cv: Condvar,
}

impl RealSemaphore {
    pub fn new(initial: usize) -> Self {
        Self {
            state: Mutex::new(initial),
            cv: Condvar::new(),
        }
    }

    pub fn try_acquire(&self) -> bool {
        let mut count = self.state.lock();
        if *count == 0 {
            false
        } else {
            *count -= 1;
            true
        }
    }

    pub fn acquire(&self) {
        let mut count = self.state.lock();
        while *count == 0 {
            self.cv.wait(&mut count);
        }
        *count -= 1;
    }

    pub fn acquire_timeout(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut count = self.state.lock();
        while *count == 0 {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            self.cv.wait_for(&mut count, deadline - now);
        }
        *count -= 1;
        true
    }

    pub fn release(&self, n: usize) {
        let mut count = self.state.lock();
        *count = count.saturating_add(n);
        drop(count);
        self.cv.notify_all();
    }

    pub fn current(&self) -> usize {
        *self.state.lock()
    }
}

/// Real `threading.Condition`. A `Mutex` + `Condvar` pair.
/// `wait()` releases the mutex, blocks until notified, and
/// re-acquires; `notify`/`notify_all` wake one or all waiters.
#[derive(Debug)]
pub struct RealCondition {
    /// The protected boolean is just a tick counter — Python
    /// callers track their own predicate state, we just need
    /// somewhere for `wait()` to release-and-reacquire.
    state: Mutex<u64>,
    cv: Condvar,
}

impl Default for RealCondition {
    fn default() -> Self {
        Self::new()
    }
}

impl RealCondition {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(0),
            cv: Condvar::new(),
        }
    }

    pub fn wait(&self) {
        let mut tick = self.state.lock();
        let start = *tick;
        while *tick == start {
            self.cv.wait(&mut tick);
        }
    }

    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut tick = self.state.lock();
        let start = *tick;
        while *tick == start {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let result = self.cv.wait_for(&mut tick, deadline - now);
            if result.timed_out() {
                return *tick != start;
            }
        }
        true
    }

    pub fn notify(&self, n: usize) {
        let mut tick = self.state.lock();
        *tick = tick.wrapping_add(1);
        drop(tick);
        for _ in 0..n {
            self.cv.notify_one();
        }
    }

    pub fn notify_all(&self) {
        let mut tick = self.state.lock();
        *tick = tick.wrapping_add(1);
        drop(tick);
        self.cv.notify_all();
    }
}

/// Real `threading.Barrier`. `wait()` blocks until `parties`
/// threads have called it, then releases all of them.
#[derive(Debug)]
pub struct RealBarrier {
    parties: usize,
    state: Mutex<BarrierState>,
    cv: Condvar,
}

#[derive(Debug)]
struct BarrierState {
    waiting: usize,
    generation: u64,
    broken: bool,
}

impl RealBarrier {
    pub fn new(parties: usize) -> Self {
        Self {
            parties: parties.max(1),
            state: Mutex::new(BarrierState {
                waiting: 0,
                generation: 0,
                broken: false,
            }),
            cv: Condvar::new(),
        }
    }

    /// Wait at the barrier. Returns `Some(index)` (0..parties)
    /// if released normally; `None` if the barrier was broken.
    pub fn wait(&self, timeout: Option<Duration>) -> Option<usize> {
        let mut state = self.state.lock();
        if state.broken {
            return None;
        }
        let my_gen = state.generation;
        state.waiting += 1;
        let my_index = state.waiting - 1;
        if state.waiting == self.parties {
            state.generation = state.generation.wrapping_add(1);
            state.waiting = 0;
            drop(state);
            self.cv.notify_all();
            return Some(my_index);
        }
        if let Some(t) = timeout {
            let deadline = Instant::now() + t;
            while state.generation == my_gen && !state.broken {
                let now = Instant::now();
                if now >= deadline {
                    state.broken = true;
                    drop(state);
                    self.cv.notify_all();
                    return None;
                }
                self.cv.wait_for(&mut state, deadline - now);
            }
        } else {
            while state.generation == my_gen && !state.broken {
                self.cv.wait(&mut state);
            }
        }
        if state.broken {
            None
        } else {
            Some(my_index)
        }
    }

    pub fn reset(&self) {
        let mut state = self.state.lock();
        state.waiting = 0;
        state.generation = state.generation.wrapping_add(1);
        state.broken = false;
        drop(state);
        self.cv.notify_all();
    }

    pub fn abort(&self) {
        let mut state = self.state.lock();
        state.broken = true;
        drop(state);
        self.cv.notify_all();
    }

    pub fn n_waiting(&self) -> usize {
        self.state.lock().waiting
    }

    pub fn parties(&self) -> usize {
        self.parties
    }

    pub fn broken(&self) -> bool {
        self.state.lock().broken
    }
}

/// A reentrant guard for the GIL. A re-entrant mutex is necessary
/// because the eval loop may call back into Rust functions that
/// themselves want to assert "the GIL is held"; using a non-
/// reentrant mutex would deadlock the second nested acquisition.
#[derive(Debug)]
pub struct GilLock(ReentrantMutex<()>);

impl Default for GilLock {
    fn default() -> Self {
        Self::new()
    }
}

impl GilLock {
    pub fn new() -> Self {
        Self(ReentrantMutex::new(()))
    }

    pub fn lock(&self) -> parking_lot::ReentrantMutexGuard<'_, ()> {
        self.0.lock()
    }

    pub fn try_lock(&self) -> Option<parking_lot::ReentrantMutexGuard<'_, ()>> {
        self.0.try_lock()
    }
}

/// A typed Arc alias for the threading primitives. Each `Object`
/// variant that wraps one of these uses this alias so the
/// "this is a thread-shareable handle" contract is visible at
/// the type level.
pub type Shared<T> = Arc<T>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn lock_basic_acquire_release() {
        let l = RealLock::new();
        assert!(!l.is_locked());
        assert!(l.try_acquire(1));
        assert!(l.is_locked());
        assert!(!l.try_acquire(2));
        l.release().unwrap();
        assert!(!l.is_locked());
    }

    #[test]
    fn rlock_reentrant() {
        let l = RealRLock::new();
        assert!(l.try_acquire(1));
        assert!(l.try_acquire(1));
        assert_eq!(l.depth(), 2);
        assert_eq!(l.release(1).unwrap(), 1);
        assert_eq!(l.release(1).unwrap(), 0);
    }

    #[test]
    fn event_signal_across_threads() {
        let e = Arc::new(RealEvent::new());
        let e2 = Arc::clone(&e);
        let handle = thread::spawn(move || e2.wait());
        thread::sleep(Duration::from_millis(20));
        e.set();
        assert!(handle.join().unwrap());
    }

    #[test]
    fn semaphore_blocks_at_zero() {
        let s = RealSemaphore::new(2);
        assert!(s.try_acquire());
        assert!(s.try_acquire());
        assert!(!s.try_acquire());
        s.release(1);
        assert!(s.try_acquire());
    }

    #[test]
    fn barrier_releases_all_parties() {
        let b = Arc::new(RealBarrier::new(3));
        let mut handles = Vec::new();
        for _ in 0..3 {
            let b2 = Arc::clone(&b);
            handles.push(thread::spawn(move || b2.wait(None)));
        }
        for h in handles {
            assert!(h.join().unwrap().is_some());
        }
    }
}
