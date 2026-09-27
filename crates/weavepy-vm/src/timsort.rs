//! CPython's list sort: an adaptive, stable, natural merge sort (timsort with
//! the powersort merge policy), ported from `Objects/listobject.c` in 3.14.
//!
//! The comparison sequence follows CPython's, so a comparator that is
//! inconsistent (NaNs, a random `__lt__`) or that raises produces the same
//! observable behavior: no panic, and on error the slice is left as a
//! permutation of its input. Every comparison is a strict "less than".

use std::ptr;

/// Once a merge is galloping, it stays there until both runs win fewer
/// than this many consecutive times.
const MIN_GALLOP: usize = 7;

/// The largest minimum run length; a power of 2.
const MAX_MINRUN: usize = 64;

/// A run pending a merge: `len` elements starting at index `base`.
#[derive(Clone, Copy)]
struct Run {
    base: usize,
    len: usize,
    /// Depth in the conceptual binary merge tree (powersort).
    power: u32,
}

struct MergeState<T, F> {
    base: *mut T,
    len: usize,
    lt: F,
    min_gallop: usize,
    /// Scratch storage for merges. Its length is always 0: elements are
    /// moved in and out bitwise, and never dropped from here.
    tmp: Vec<T>,
    pending: Vec<Run>,
}

/// Sort `v` in place, stably, by `lt`, the strict ordering. On error, `v`
/// holds a permutation of its input.
pub(crate) fn sort<T, E, F>(v: &mut [T], lt: F) -> Result<(), E>
where
    F: FnMut(&T, &T) -> Result<bool, E>,
{
    let n = v.len();
    if n < 2 {
        return Ok(());
    }
    let mut ms = MergeState {
        base: v.as_mut_ptr(),
        len: n,
        lt,
        min_gallop: MIN_GALLOP,
        tmp: Vec::new(),
        pending: Vec::new(),
    };
    let minrun = compute_minrun(n);
    let mut lo = 0;
    let mut nremaining = n;
    // SAFETY: every index handed to the helpers below lies in `v`, which is
    // borrowed mutably for the whole sort.
    unsafe {
        while nremaining > 0 {
            let mut run = ms.count_run(lo, nremaining)?;
            if run < minrun {
                let force = nremaining.min(minrun);
                ms.binarysort(lo, force, run)?;
                run = force;
            }
            ms.found_new_run(run)?;
            ms.pending.push(Run {
                base: lo,
                len: run,
                power: 0,
            });
            lo += run;
            nremaining -= run;
        }
        ms.merge_force_collapse()
    }
}

/// A good minimum run length: `n` itself below `MAX_MINRUN`, else a value in
/// `MAX_MINRUN / 2 ..= MAX_MINRUN` such that `n / minrun` is close to, but
/// strictly less than, a power of 2.
fn compute_minrun(mut n: usize) -> usize {
    let mut r = 0;
    while n >= MAX_MINRUN {
        r |= n & 1;
        n >>= 1;
    }
    n + r
}

/// The powersort "power" of the run at `s1` (length `n1`) followed by one of
/// length `n2`, in a list of length `n`.
fn powerloop(s1: usize, n1: usize, n2: usize, n: usize) -> u32 {
    let mut result = 0;
    // Twice the two runs' midpoints, so that both are integers.
    let mut a = 2 * s1 + n1;
    let mut b = a + n1 + n2;
    loop {
        result += 1;
        if a >= n {
            a -= n;
            b -= n;
        } else if b >= n {
            break;
        }
        a <<= 1;
        b <<= 1;
    }
    result
}

/// Unmerged elements of a merge, parked in scratch storage: `len` of them
/// from `src` belong at `dest`. Dropping the hole moves them there, which
/// also restores a complete permutation if a comparison fails or panics.
struct Hole<T> {
    src: *const T,
    dest: *mut T,
    len: usize,
}

impl<T> Drop for Hole<T> {
    fn drop(&mut self) {
        // SAFETY: the merge maintains that `dest` is exactly the vacated
        // range the parked elements fill, and scratch never overlaps the
        // list.
        unsafe { ptr::copy_nonoverlapping(self.src, self.dest, self.len) };
    }
}

impl<T, E, F> MergeState<T, F>
where
    F: FnMut(&T, &T) -> Result<bool, E>,
{
    #[inline]
    unsafe fn at(&self, i: usize) -> *mut T {
        // SAFETY: callers pass indices within the list.
        unsafe { self.base.add(i) }
    }

    #[inline]
    fn lt(&mut self, a: *const T, b: *const T) -> Result<bool, E> {
        // SAFETY: both point at initialized elements, in the list or
        // parked in scratch, and no element moves during a comparison.
        unsafe { (self.lt)(&*a, &*b) }
    }

    /// Scratch storage for `need` elements.
    fn scratch(&mut self, need: usize) -> *mut T {
        if self.tmp.capacity() < need {
            self.tmp = Vec::with_capacity(need);
        }
        self.tmp.as_mut_ptr()
    }

    unsafe fn reverse(&mut self, lo: usize, n: usize) {
        // SAFETY: `lo..lo + n` lies in the list.
        unsafe { std::slice::from_raw_parts_mut(self.at(lo), n).reverse() };
    }

    /// Stable binary insertion sort of `lo..lo + n`, whose first `ok`
    /// elements are already sorted.
    unsafe fn binarysort(&mut self, lo: usize, n: usize, ok: usize) -> Result<(), E> {
        let a = unsafe { self.at(lo) };
        let mut ok = ok.max(1);
        while ok < n {
            // Find where a[ok] belongs: a[..l] <= pivot < a[r..ok].
            let (mut l, mut r) = (0, ok);
            let pivot = unsafe { a.add(ok) };
            while l < r {
                let m = (l + r) >> 1;
                if self.lt(pivot, unsafe { a.add(m) })? {
                    r = m;
                } else {
                    l = m + 1;
                }
            }
            // SAFETY: rotate a[l..=ok] right by one; nothing compares
            // while the pivot is out.
            unsafe {
                let p = ptr::read(pivot);
                ptr::copy(a.add(l), a.add(l + 1), ok - l);
                ptr::write(a.add(l), p);
            }
            ok += 1;
        }
        Ok(())
    }

    /// The length of the run starting at `lo`, no longer than `nremaining`,
    /// made ascending in place.
    unsafe fn count_run(&mut self, lo: usize, nremaining: usize) -> Result<usize, E> {
        let a = unsafe { self.at(lo) };
        let next_smaller =
            |ms: &mut Self, n: usize| ms.lt(unsafe { a.add(n) }, unsafe { a.add(n - 1) });
        let next_larger =
            |ms: &mut Self, n: usize| ms.lt(unsafe { a.add(n - 1) }, unsafe { a.add(n) });
        // Try an ascending run first.
        let mut n = 1;
        while n < nremaining {
            if next_smaller(self, n)? {
                break;
            }
            n += 1;
        }
        if n == nremaining {
            return Ok(n);
        }
        // a[n] is strictly less. With a longer ascending prefix, it either
        // rose somewhere (done), or is all equal and can start a
        // descending run, reversed in place.
        if n > 1 {
            if self.lt(a, unsafe { a.add(n - 1) })? {
                return Ok(n);
            }
            unsafe { self.reverse(lo, n) };
        }
        n += 1;
        // Finish the descending run, reversing all-equal subruns on the fly
        // so the final whole-run reversal restores their order.
        let mut neq = 0;
        while n < nremaining {
            if next_smaller(self, n)? {
                if neq > 0 {
                    neq += 1;
                    unsafe { self.reverse(lo + n - neq, neq) };
                    neq = 0;
                }
            } else if next_larger(self, n)? {
                break;
            } else {
                neq += 1;
            }
            n += 1;
        }
        if neq > 0 {
            neq += 1;
            unsafe { self.reverse(lo + n - neq, neq) };
        }
        unsafe { self.reverse(lo, n) };
        // The reversed run may extend with a naturally increasing suffix.
        while n < nremaining {
            if next_smaller(self, n)? {
                break;
            }
            n += 1;
        }
        Ok(n)
    }

    /// The index `k` in `0..=n` where `key` belongs in the sorted `a[..n]`,
    /// left of any equal elements: `a[k - 1] < key <= a[k]`. The search
    /// starts at `hint`.
    unsafe fn gallop_left(
        &mut self,
        key: *const T,
        a: *const T,
        n: usize,
        hint: usize,
    ) -> Result<usize, E> {
        let at = |i: usize| unsafe { a.add(i) };
        let (mut lastofs, mut ofs);
        if self.lt(at(hint), key)? {
            // Gallop right until a[hint + lastofs] < key <= a[hint + ofs].
            let maxofs = n - hint;
            lastofs = 0;
            ofs = 1;
            while ofs < maxofs {
                if self.lt(at(hint + ofs), key)? {
                    lastofs = ofs;
                    ofs = (ofs << 1) + 1;
                } else {
                    break;
                }
            }
            ofs = ofs.min(maxofs);
            lastofs += hint + 1;
            ofs += hint;
        } else {
            // Gallop left until a[hint - ofs] < key <= a[hint - lastofs].
            let maxofs = hint + 1;
            lastofs = 0;
            ofs = 1;
            while ofs < maxofs {
                if self.lt(at(hint - ofs), key)? {
                    break;
                }
                lastofs = ofs;
                ofs = (ofs << 1) + 1;
            }
            ofs = ofs.min(maxofs);
            let k = lastofs;
            // `hint - ofs` may be -1; the binary search starts one past it.
            lastofs = hint + 1 - ofs;
            ofs = hint - k;
        }
        // Binary search with a[lastofs - 1] < key <= a[ofs].
        while lastofs < ofs {
            let m = lastofs + ((ofs - lastofs) >> 1);
            if self.lt(at(m), key)? {
                lastofs = m + 1;
            } else {
                ofs = m;
            }
        }
        Ok(ofs)
    }

    /// Like [`Self::gallop_left`], but right of any equal elements:
    /// `a[k - 1] <= key < a[k]`.
    unsafe fn gallop_right(
        &mut self,
        key: *const T,
        a: *const T,
        n: usize,
        hint: usize,
    ) -> Result<usize, E> {
        let at = |i: usize| unsafe { a.add(i) };
        let (mut lastofs, mut ofs);
        if self.lt(key, at(hint))? {
            // Gallop left until a[hint - ofs] <= key < a[hint - lastofs].
            let maxofs = hint + 1;
            lastofs = 0;
            ofs = 1;
            while ofs < maxofs {
                if self.lt(key, at(hint - ofs))? {
                    lastofs = ofs;
                    ofs = (ofs << 1) + 1;
                } else {
                    break;
                }
            }
            ofs = ofs.min(maxofs);
            let k = lastofs;
            lastofs = hint + 1 - ofs;
            ofs = hint - k;
        } else {
            // Gallop right until a[hint + lastofs] <= key < a[hint + ofs].
            let maxofs = n - hint;
            lastofs = 0;
            ofs = 1;
            while ofs < maxofs {
                if self.lt(key, at(hint + ofs))? {
                    break;
                }
                lastofs = ofs;
                ofs = (ofs << 1) + 1;
            }
            ofs = ofs.min(maxofs);
            lastofs += hint + 1;
            ofs += hint;
        }
        // Binary search with a[lastofs - 1] <= key < a[ofs].
        while lastofs < ofs {
            let m = lastofs + ((ofs - lastofs) >> 1);
            if self.lt(key, at(m))? {
                ofs = m;
            } else {
                lastofs = m + 1;
            }
        }
        Ok(ofs)
    }

    /// Merge the adjacent runs `sa..sa + na` and `sa + na..sa + na + nb`,
    /// with `na <= nb`, through scratch space for the first. The last
    /// element of the first run belongs at the end of the merge.
    // A move's bookkeeping is dead when the merge returns right after it.
    #[allow(unused_assignments)]
    unsafe fn merge_lo(&mut self, sa: usize, na: usize, nb: usize) -> Result<(), E> {
        let tmp = self.scratch(na);
        let mut dest = unsafe { self.at(sa) };
        let mut b = unsafe { self.at(sa + na) };
        unsafe { ptr::copy_nonoverlapping(dest, tmp, na) };
        // The unmerged part of the first run, parked in scratch.
        let mut hole = Hole {
            src: tmp,
            dest,
            len: na,
        };
        let mut nb = nb;
        // SAFETY (throughout): the merged prefix, the parked first-run
        // elements, and the rest of the second run always partition the
        // two runs' span, so `hole.dest` is where the parked ones belong.
        macro_rules! take_b {
            ($k:expr) => {{
                let k = $k;
                unsafe { ptr::copy(b, dest, k) };
                dest = unsafe { dest.add(k) };
                b = unsafe { b.add(k) };
                nb -= k;
                hole.dest = dest;
            }};
        }
        macro_rules! take_a {
            ($k:expr) => {{
                let k = $k;
                unsafe { ptr::copy_nonoverlapping(hole.src, dest, k) };
                dest = unsafe { dest.add(k) };
                hole.src = unsafe { hole.src.add(k) };
                hole.len -= k;
                hole.dest = dest;
            }};
        }
        take_b!(1);
        if nb == 0 {
            return Ok(());
        }
        if hole.len == 1 {
            // The last element of the first run goes after the second.
            take_b!(nb);
            return Ok(());
        }
        let mut min_gallop = self.min_gallop;
        loop {
            let mut acount = 0;
            let mut bcount = 0;
            // One element at a time, until one run keeps winning.
            loop {
                if self.lt(b, hole.src)? {
                    take_b!(1);
                    bcount += 1;
                    acount = 0;
                    if nb == 0 {
                        return Ok(());
                    }
                    if bcount >= min_gallop {
                        break;
                    }
                } else {
                    take_a!(1);
                    acount += 1;
                    bcount = 0;
                    if hole.len == 1 {
                        take_b!(nb);
                        return Ok(());
                    }
                    if acount >= min_gallop {
                        break;
                    }
                }
            }
            // Gallop until neither run is winning consistently.
            min_gallop += 1;
            loop {
                min_gallop -= usize::from(min_gallop > 1);
                self.min_gallop = min_gallop;
                let k = unsafe { self.gallop_right(b, hole.src, hole.len, 0)? };
                acount = k;
                if k > 0 {
                    take_a!(k);
                    if hole.len == 1 {
                        take_b!(nb);
                        return Ok(());
                    }
                    // Impossible for a consistent comparison, but possible.
                    if hole.len == 0 {
                        return Ok(());
                    }
                }
                take_b!(1);
                if nb == 0 {
                    return Ok(());
                }
                let k = unsafe { self.gallop_left(hole.src, b, nb, 0)? };
                bcount = k;
                if k > 0 {
                    take_b!(k);
                    if nb == 0 {
                        return Ok(());
                    }
                }
                take_a!(1);
                if hole.len == 1 {
                    take_b!(nb);
                    return Ok(());
                }
                if acount < MIN_GALLOP && bcount < MIN_GALLOP {
                    break;
                }
            }
            // Penalize leaving galloping mode.
            min_gallop += 1;
            self.min_gallop = min_gallop;
        }
    }

    /// Merge the adjacent runs `sa..sa + na` and `sa + na..sa + na + nb`,
    /// with `na >= nb`, from the top, through scratch space for the second.
    /// The first element of the second run belongs at the front.
    #[allow(unused_assignments)]
    unsafe fn merge_hi(&mut self, sa: usize, na: usize, nb: usize) -> Result<(), E> {
        let tmp = self.scratch(nb);
        let a_base = unsafe { self.at(sa) };
        unsafe { ptr::copy_nonoverlapping(self.at(sa + na), tmp, nb) };
        // The unmerged part of the second run, parked in scratch; it always
        // belongs just above the unmerged part of the first.
        let mut hole = Hole {
            src: tmp,
            dest: unsafe { a_base.add(na) },
            len: nb,
        };
        let mut na = na;
        // `dest` is the highest unfilled slot: a_base[na + hole.len - 1].
        // SAFETY (throughout): as in `merge_lo`, mirrored.
        macro_rules! take_a {
            ($k:expr) => {{
                let k = $k;
                // Move a_base[na - k..na] up to end at the top slot.
                unsafe { ptr::copy(a_base.add(na - k), a_base.add(na - k + hole.len), k) };
                na -= k;
                hole.dest = unsafe { a_base.add(na) };
            }};
        }
        macro_rules! take_b {
            ($k:expr) => {{
                let k = $k;
                let len = hole.len;
                unsafe { ptr::copy_nonoverlapping(tmp.add(len - k), a_base.add(na + len - k), k) };
                hole.len -= k;
            }};
        }
        take_a!(1);
        if na == 0 {
            return Ok(());
        }
        if hole.len == 1 {
            // The first element of the second run goes before the first.
            take_a!(na);
            return Ok(());
        }
        let mut min_gallop = self.min_gallop;
        loop {
            let mut acount = 0;
            let mut bcount = 0;
            loop {
                let (a_top, b_top) = unsafe { (a_base.add(na - 1), tmp.add(hole.len - 1)) };
                if self.lt(b_top, a_top)? {
                    take_a!(1);
                    acount += 1;
                    bcount = 0;
                    if na == 0 {
                        return Ok(());
                    }
                    if acount >= min_gallop {
                        break;
                    }
                } else {
                    take_b!(1);
                    bcount += 1;
                    acount = 0;
                    if hole.len == 1 {
                        take_a!(na);
                        return Ok(());
                    }
                    if bcount >= min_gallop {
                        break;
                    }
                }
            }
            min_gallop += 1;
            loop {
                min_gallop -= usize::from(min_gallop > 1);
                self.min_gallop = min_gallop;
                let b_top = unsafe { tmp.add(hole.len - 1) };
                let k = na - unsafe { self.gallop_right(b_top, a_base, na, na - 1)? };
                acount = k;
                if k > 0 {
                    take_a!(k);
                    if na == 0 {
                        return Ok(());
                    }
                }
                take_b!(1);
                if hole.len == 1 {
                    take_a!(na);
                    return Ok(());
                }
                let a_top = unsafe { a_base.add(na - 1) };
                let k = hole.len - unsafe { self.gallop_left(a_top, tmp, hole.len, hole.len - 1)? };
                bcount = k;
                if k > 0 {
                    take_b!(k);
                    if hole.len == 1 {
                        take_a!(na);
                        return Ok(());
                    }
                    // Impossible for a consistent comparison, but possible.
                    if hole.len == 0 {
                        return Ok(());
                    }
                }
                take_a!(1);
                if na == 0 {
                    return Ok(());
                }
                if acount < MIN_GALLOP && bcount < MIN_GALLOP {
                    break;
                }
            }
            min_gallop += 1;
            self.min_gallop = min_gallop;
        }
    }

    /// Merge the pending runs at stack indices `i` and `i + 1`.
    unsafe fn merge_at(&mut self, i: usize) -> Result<(), E> {
        let Run {
            base: sa, len: na, ..
        } = self.pending[i];
        let Run {
            base: sb, len: nb, ..
        } = self.pending[i + 1];
        self.pending[i].len = na + nb;
        self.pending.remove(i + 1);
        // Elements of the first run before where the second starts are
        // already in place.
        let k = unsafe { self.gallop_right(self.at(sb), self.at(sa), na, 0)? };
        let (sa, na) = (sa + k, na - k);
        if na == 0 {
            return Ok(());
        }
        // Elements of the second run after where the first ends are too.
        let nb = unsafe { self.gallop_left(self.at(sa + na - 1), self.at(sb), nb, nb - 1)? };
        if nb == 0 {
            return Ok(());
        }
        if na <= nb {
            unsafe { self.merge_lo(sa, na, nb) }
        } else {
            unsafe { self.merge_hi(sa, na, nb) }
        }
    }

    /// A run of length `n2` follows the pending ones: merge runs deeper in
    /// the powersort tree than the top one.
    unsafe fn found_new_run(&mut self, n2: usize) -> Result<(), E> {
        let Some(top) = self.pending.last() else {
            return Ok(());
        };
        let power = powerloop(top.base, top.len, n2, self.len);
        while self.pending.len() > 1 && self.pending[self.pending.len() - 2].power > power {
            unsafe { self.merge_at(self.pending.len() - 2)? };
        }
        let last = self.pending.len() - 1;
        self.pending[last].power = power;
        Ok(())
    }

    /// Merge every pending run into one.
    unsafe fn merge_force_collapse(&mut self) -> Result<(), E> {
        while self.pending.len() > 1 {
            let mut n = self.pending.len() - 2;
            if n > 0 && self.pending[n - 1].len < self.pending[n + 1].len {
                n -= 1;
            }
            unsafe { self.merge_at(n)? };
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(mut v: Vec<(i32, usize)>) {
        let mut expected = v.clone();
        expected.sort_by_key(|p| p.0);
        sort(&mut v, |a, b| Ok::<bool, ()>(a.0 < b.0)).unwrap();
        assert_eq!(v, expected);
    }

    #[test]
    fn sorts_stably() {
        let mut seed = 12345u64;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for n in [0, 1, 2, 3, 10, 63, 64, 65, 100, 257, 1000, 5000] {
            for modulus in [2, 10, 1000, u64::MAX] {
                let v: Vec<(i32, usize)> = (0..n).map(|i| ((rand() % modulus) as i32, i)).collect();
                check(v.clone());
                let mut sorted = v.clone();
                sorted.sort_unstable();
                check(sorted.clone());
                sorted.reverse();
                check(sorted);
            }
        }
    }

    #[test]
    fn inconsistent_order_keeps_every_element() {
        let mut seed = 99u64;
        let v: Vec<String> = (0..3000).map(|i| i.to_string()).collect();
        let mut w = v.clone();
        sort(&mut w, |_, _| {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            Ok::<bool, ()>(seed >> 63 == 1)
        })
        .unwrap();
        w.sort();
        let mut v = v;
        v.sort();
        assert_eq!(v, w);
    }

    #[test]
    fn error_keeps_every_element() {
        let v: Vec<String> = (0..2000).rev().map(|i| i.to_string()).collect();
        for limit in [0, 1, 10, 500, 5000] {
            let mut w = v.clone();
            let mut count = 0;
            let r = sort(&mut w, |a, b| {
                count += 1;
                if count > limit {
                    Err(())
                } else {
                    Ok(a.len() < b.len() || (a.len() == b.len() && a < b))
                }
            });
            assert!(r.is_err() || limit >= 5000);
            let mut sorted = w.clone();
            sorted.sort();
            let mut expected = v.clone();
            expected.sort();
            assert_eq!(sorted, expected);
        }
    }
}
