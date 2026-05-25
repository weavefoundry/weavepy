//! Thread-safe synchronisation primitives + workspace-wide
//! interior-mutability layer — RFCs 0024 and 0025.
//!
//! After RFC 0025 the entire VM heap is `Arc`-rooted: every
//! `Object` variant, every `TypeObject`, every `CodeObject` reaches
//! across threads through the aliases defined here. Existing call
//! sites compile unchanged after swapping
//! `use std::rc::Rc;` / `use std::cell::{Cell, RefCell};` for
//! `use crate::sync::{Rc, Cell, RefCell};`.
//!
//! Types exported by this module (the RFC 0025 surface):
//!
//! - [`Rc`] — type alias for [`std::sync::Arc`]. `Arc::ptr_eq` /
//!   `Arc::clone` / `Arc::as_ptr` / `Arc::strong_count` /
//!   `Arc::try_unwrap` / `Arc::get_mut` / `Arc::downgrade` all
//!   work unchanged.
//! - [`Weak`] — type alias for [`std::sync::Weak`].
//! - [`GilCell`] — interior-mutability primitive whose surface
//!   matches [`std::cell::RefCell`] (and the [`std::cell::Cell`]
//!   methods for `T: Copy`), but is `Send + Sync` when `T: Send`.
//!   Backed by a `parking_lot::ReentrantMutex` so the codebase's
//!   pervasive nested-borrow patterns keep working.
//! - [`RefCell`] / [`Cell`] — type aliases for [`GilCell`] so
//!   existing `RefCell<T>` / `Cell<T>` signatures don't need any
//!   per-call-site edits.
//!
//! The RFC 0024 surface (real lock / event / barrier primitives
//! that back `threading.Lock` etc.) lives below the new aliases.

use std::cell::UnsafeCell;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex, ReentrantMutex, ReentrantMutexGuard};

// ---------------------------------------------------------------------------
// RFC 0025 — drop-in replacements for `std::rc::Rc`, `std::rc::Weak`,
// `std::cell::RefCell`, `std::cell::Cell`.
// ---------------------------------------------------------------------------

/// Drop-in replacement for [`std::rc::Rc`]. Backed by
/// [`std::sync::Arc`], so it carries the same atomic refcount
/// behaviour. Every method on `Arc` (`ptr_eq`, `clone`, `as_ptr`,
/// `strong_count`, `weak_count`, `downgrade`, `try_unwrap`,
/// `get_mut`, `into_inner`) is identical to the `Rc` API the
/// workspace already calls.
pub type Rc<T> = std::sync::Arc<T>;

/// Drop-in replacement for [`std::rc::Weak`]. Backed by
/// [`std::sync::Weak`].
pub type Weak<T> = std::sync::Weak<T>;

/// An interior-mutability cell that's `Send + Sync` (when the
/// payload is `Send`) and supports both the `RefCell` and `Cell`
/// surfaces. Backed by a [`parking_lot::ReentrantMutex`] so nested
/// borrows on the same thread (extremely common in the
/// dispatcher's descriptor / metaclass / dunder paths) don't
/// deadlock. A `RefCell`-style borrow counter on top of the mutex
/// preserves the "no concurrent shared-and-mutable borrow"
/// invariant.
///
/// Cross-thread synchronisation is the mutex's job — a
/// `borrow()`/`borrow_mut()` on thread A blocks any `borrow()` or
/// `borrow_mut()` on thread B until A drops its guard. Within a
/// single thread the mutex is reentrant, so nested `borrow()`
/// calls succeed; `borrow_mut()` while any borrow is live still
/// panics (mirroring `std::cell::RefCell`).
pub struct GilCell<T: ?Sized> {
    /// CPython-shaped borrow counter:
    ///
    /// - `0` — unborrowed.
    /// - `>0` — that many shared borrows are live.
    /// - `< 0` — a mutable borrow is live (always exactly `-1`).
    ///
    /// Atomic because `GilCell` is `Sync`; the mutex below makes
    /// cross-thread access exclusive, but within a single OS thread
    /// the reentrant mutex lets us re-enter — the counter prevents
    /// undefined behaviour on nested `borrow_mut()`.
    borrow: AtomicIsize,
    inner: ReentrantMutex<UnsafeCell<T>>,
}

// SAFETY: `GilCell<T>` is `Send` whenever `T: Send` — moving a cell
// across threads is fine because we move the payload too.
// `GilCell<T>` is `Sync` whenever `T: Send` because the
// `ReentrantMutex` guarantees that only one thread accesses the
// `UnsafeCell` at a time and the borrow counter prevents the same
// thread from creating aliasing `&mut T`. Together those rule out
// the data race / aliasing UB that a bare `UnsafeCell<T>` would
// otherwise produce.
unsafe impl<T: ?Sized + Send> Send for GilCell<T> {}
unsafe impl<T: ?Sized + Send> Sync for GilCell<T> {}

/// `RefCell<T>` is the workspace's name for [`GilCell`] when the
/// caller previously imported `std::cell::RefCell`.
pub type RefCell<T> = GilCell<T>;

/// `Cell<T>` is the workspace's name for [`GilCell`] when the
/// caller previously imported `std::cell::Cell`. Use the
/// `GilCell::get` / `GilCell::set` methods (available for
/// `T: Copy`) for the classic `Cell` ergonomics.
pub type Cell<T> = GilCell<T>;

/// Error returned by [`GilCell::try_borrow`] when the cell is
/// already mutably borrowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BorrowError;

impl fmt::Display for BorrowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("already mutably borrowed")
    }
}

impl std::error::Error for BorrowError {}

/// Error returned by [`GilCell::try_borrow_mut`] when the cell is
/// already borrowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BorrowMutError;

impl fmt::Display for BorrowMutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("already borrowed")
    }
}

impl std::error::Error for BorrowMutError {}

impl<T> GilCell<T> {
    /// Build a cell holding `value`. `const`-callable so the
    /// codebase's existing `thread_local!{ static FOO: RefCell<…> =
    /// const { RefCell::new(None) }; }` patterns keep working.
    #[must_use]
    pub const fn new(value: T) -> Self {
        Self {
            borrow: AtomicIsize::new(0),
            inner: ReentrantMutex::new(UnsafeCell::new(value)),
        }
    }

    /// Move out the cell's payload, consuming the cell.
    pub fn into_inner(self) -> T {
        // Drop the borrow counter; the mutex's `into_inner` returns
        // the `UnsafeCell<T>`, whose `into_inner` extracts the
        // payload.
        self.inner.into_inner().into_inner()
    }
}

impl<T: ?Sized> GilCell<T> {
    /// Borrow the cell immutably. Multiple immutable borrows can
    /// coexist on the same thread. Panics if a mutable borrow is
    /// already live (matches `std::cell::RefCell::borrow`).
    #[track_caller]
    pub fn borrow(&self) -> Ref<'_, T> {
        self.try_borrow()
            .expect("GilCell::borrow: cell is mutably borrowed")
    }

    /// Borrow the cell mutably. Panics if any borrow is already
    /// live (matches `std::cell::RefCell::borrow_mut`).
    #[track_caller]
    pub fn borrow_mut(&self) -> RefMut<'_, T> {
        self.try_borrow_mut()
            .expect("GilCell::borrow_mut: cell is already borrowed")
    }

    /// Non-panicking variant of [`borrow`](Self::borrow). Returns
    /// [`BorrowError`] if a mutable borrow is live.
    pub fn try_borrow(&self) -> Result<Ref<'_, T>, BorrowError> {
        let guard = self.inner.lock();
        // Bump the borrow counter. If we observed a negative value
        // (a mutable borrow is live on this thread — the reentrant
        // mutex let us in), unwind and refuse.
        let prev = self.borrow.fetch_add(1, Ordering::Acquire);
        if prev < 0 {
            self.borrow.fetch_sub(1, Ordering::Release);
            return Err(BorrowError);
        }
        // SAFETY: we hold the reentrant mutex (so no other thread
        // can race) and the borrow counter is `>= 1` (so no `&mut T`
        // to the inner exists). `UnsafeCell::raw_get` is itself a
        // safe transmute; the surrounding `unsafe` covers the raw
        // dereference into a shared reference.
        let value: &T = unsafe { &*UnsafeCell::raw_get(self.inner.data_ptr()) };
        Ok(Ref {
            counter: &self.borrow,
            _guard: guard,
            value,
        })
    }

    /// Non-panicking variant of [`borrow_mut`](Self::borrow_mut).
    /// Returns [`BorrowMutError`] if any borrow is live.
    pub fn try_borrow_mut(&self) -> Result<RefMut<'_, T>, BorrowMutError> {
        let guard = self.inner.lock();
        // Only succeed if the counter is exactly zero — i.e. no
        // shared borrow and no nested mutable borrow.
        if self
            .borrow
            .compare_exchange(0, -1, Ordering::Acquire, Ordering::Acquire)
            .is_err()
        {
            return Err(BorrowMutError);
        }
        // SAFETY: reentrant mutex held; borrow counter is `-1` so
        // no other `&T` or `&mut T` to the inner exists. Safe to
        // hand out an exclusive `&mut T`.
        let value: &mut T = unsafe { &mut *UnsafeCell::raw_get(self.inner.data_ptr()) };
        Ok(RefMut {
            counter: &self.borrow,
            _guard: guard,
            value,
        })
    }

    /// Returns a raw pointer to the inner data. Doesn't claim any
    /// borrow; the caller is responsible for ensuring the pointer
    /// isn't dereferenced concurrently with another borrow.
    pub fn as_ptr(&self) -> *mut T {
        UnsafeCell::raw_get(self.inner.data_ptr())
    }
}

impl<T> GilCell<T> {
    /// Replace the inner value with `new`, returning the old one.
    pub fn replace(&self, new: T) -> T {
        std::mem::replace(&mut *self.borrow_mut(), new)
    }

    /// Apply `f` to the current value, then store its return into
    /// the cell. Returns the previous value.
    pub fn replace_with<F>(&self, f: F) -> T
    where
        F: FnOnce(&mut T) -> T,
    {
        let mut guard = self.borrow_mut();
        let new = f(&mut guard);
        std::mem::replace(&mut *guard, new)
    }

    /// Swap the contents of two cells. The cells may be the same
    /// (no-op).
    pub fn swap(&self, other: &Self) {
        if std::ptr::eq(self, other) {
            return;
        }
        let mut a = self.borrow_mut();
        let mut b = other.borrow_mut();
        std::mem::swap(&mut *a, &mut *b);
    }

    /// Move the value out, leaving `Default::default()` in its
    /// place.
    pub fn take(&self) -> T
    where
        T: Default,
    {
        std::mem::take(&mut *self.borrow_mut())
    }
}

impl<T: Copy> GilCell<T> {
    /// Get the inner value (copying it). Equivalent to
    /// `*self.borrow()`.
    pub fn get(&self) -> T {
        *self.borrow()
    }

    /// Replace the inner value with `value`.
    pub fn set(&self, value: T) {
        *self.borrow_mut() = value;
    }
}

impl<T: Default> Default for GilCell<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> From<T> for GilCell<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T: Clone> Clone for GilCell<T> {
    fn clone(&self) -> Self {
        Self::new(self.borrow().clone())
    }
}

impl<T: fmt::Debug> fmt::Debug for GilCell<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_borrow() {
            Ok(borrow) => f.debug_tuple("GilCell").field(&*borrow).finish(),
            Err(_) => f.debug_tuple("GilCell").field(&"<borrowed>").finish(),
        }
    }
}

impl<T: PartialEq> PartialEq for GilCell<T> {
    fn eq(&self, other: &Self) -> bool {
        if std::ptr::eq(self, other) {
            return true;
        }
        *self.borrow() == *other.borrow()
    }
}

impl<T: Eq> Eq for GilCell<T> {}

impl<T: PartialOrd> PartialOrd for GilCell<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.borrow().partial_cmp(&*other.borrow())
    }
}

impl<T: Ord> Ord for GilCell<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.borrow().cmp(&*other.borrow())
    }
}

impl<T: Hash> Hash for GilCell<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.borrow().hash(state);
    }
}

/// RAII guard returned by [`GilCell::borrow`]. Releases the borrow
/// counter on drop.
pub struct Ref<'a, T: ?Sized + 'a> {
    counter: &'a AtomicIsize,
    /// Held for the lifetime of the borrow; ensures cross-thread
    /// exclusion. Drop order: `value` is logically a view into the
    /// guarded data; we hold `_guard` so it lives at least as long
    /// as `value`.
    _guard: ReentrantMutexGuard<'a, UnsafeCell<T>>,
    value: &'a T,
}

impl<T: ?Sized> Deref for Ref<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.value
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for Ref<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.value, f)
    }
}

impl<T: ?Sized + fmt::Display> fmt::Display for Ref<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.value, f)
    }
}

impl<T: ?Sized> Drop for Ref<'_, T> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Release);
    }
}

/// RAII guard returned by [`GilCell::borrow_mut`]. Resets the
/// borrow counter on drop.
pub struct RefMut<'a, T: ?Sized + 'a> {
    counter: &'a AtomicIsize,
    _guard: ReentrantMutexGuard<'a, UnsafeCell<T>>,
    value: &'a mut T,
}

impl<T: ?Sized> Deref for RefMut<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.value
    }
}

impl<T: ?Sized> DerefMut for RefMut<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.value
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for RefMut<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.value, f)
    }
}

impl<T: ?Sized> Drop for RefMut<'_, T> {
    fn drop(&mut self) {
        // From -1 back to 0 — there's only ever one outstanding
        // mutable borrow at a time.
        self.counter.store(0, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// RFC 0024 — real cross-thread synchronisation primitives.
// ---------------------------------------------------------------------------

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

    // ----- GilCell tests (RFC 0025) -----

    #[test]
    fn gilcell_borrow_returns_inner() {
        let c = GilCell::new(42);
        assert_eq!(*c.borrow(), 42);
    }

    #[test]
    fn gilcell_borrow_mut_mutates() {
        let c = GilCell::new(1);
        *c.borrow_mut() = 7;
        assert_eq!(*c.borrow(), 7);
    }

    #[test]
    fn gilcell_multi_shared_borrows() {
        let c = GilCell::new(String::from("hello"));
        let a = c.borrow();
        let b = c.borrow();
        assert_eq!(&*a, &*b);
    }

    #[test]
    fn gilcell_mut_borrow_blocks_shared() {
        let c = GilCell::new(0_i32);
        let _m = c.borrow_mut();
        assert!(c.try_borrow().is_err());
    }

    #[test]
    fn gilcell_shared_borrow_blocks_mut() {
        let c = GilCell::new(0_i32);
        let _r = c.borrow();
        assert!(c.try_borrow_mut().is_err());
    }

    #[test]
    fn gilcell_release_re_enables() {
        let c = GilCell::new(0_i32);
        {
            let mut m = c.borrow_mut();
            *m = 9;
        }
        assert_eq!(c.get(), 9);
    }

    #[test]
    fn gilcell_replace_returns_old() {
        let c = GilCell::new(1);
        let old = c.replace(2);
        assert_eq!(old, 1);
        assert_eq!(c.get(), 2);
    }

    #[test]
    fn gilcell_take_resets_to_default() {
        let c: GilCell<Vec<i32>> = GilCell::new(vec![1, 2, 3]);
        let v = c.take();
        assert_eq!(v, vec![1, 2, 3]);
        assert!(c.borrow().is_empty());
    }

    #[test]
    fn gilcell_swap_exchanges() {
        let a = GilCell::new(1);
        let b = GilCell::new(2);
        a.swap(&b);
        assert_eq!(a.get(), 2);
        assert_eq!(b.get(), 1);
    }

    #[test]
    fn gilcell_is_send_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<GilCell<i32>>();
        assert_sync::<GilCell<i32>>();
        assert_send::<Rc<GilCell<String>>>();
        assert_sync::<Rc<GilCell<String>>>();
    }

    #[test]
    fn gilcell_cross_thread_mutation() {
        // The classic "list captured by worker mutates parent's
        // view" assertion that RFC 0025 promises.
        let shared = Rc::new(GilCell::new(Vec::<i32>::new()));
        let s2 = Rc::clone(&shared);
        let handle = thread::spawn(move || {
            for i in 0..5 {
                s2.borrow_mut().push(i);
            }
        });
        handle.join().unwrap();
        assert_eq!(*shared.borrow(), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn gilcell_clone_deep_copies() {
        let a = GilCell::new(vec![1, 2, 3]);
        let b = a.clone();
        b.borrow_mut().push(4);
        assert_eq!(*a.borrow(), vec![1, 2, 3]);
        assert_eq!(*b.borrow(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn gilcell_reentrant_shared_borrow_on_same_thread() {
        let c = GilCell::new(7_i32);
        let a = c.borrow();
        let b = c.borrow();
        assert_eq!(*a, 7);
        assert_eq!(*b, 7);
    }

    #[test]
    #[should_panic(expected = "GilCell::borrow_mut")]
    fn gilcell_nested_borrow_mut_panics() {
        let c = GilCell::new(0_i32);
        let _a = c.borrow_mut();
        let _b = c.borrow_mut();
    }

    // ----- RealLock / RealRLock / RealEvent / RealSemaphore /
    // RealBarrier tests (RFC 0024) -----

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
