//! From a [`Function`] to machine code, in one pass over its blocks.
//!
//! Register allocation is local, with two kinds of block. A block entered
//! by one edge from a block emitted before it *inherits* that block's
//! register state at the branch, so values flow down chains and trees of
//! such blocks in registers. Any other block is a *merge*: it starts with
//! its parameters in registers chosen for it, the function's pinned values
//! in theirs, and every other value it needs in its spill slot, which each
//! edge into it fills. Within a block, registers go to values as they're
//! defined (a callee-saved one when the value outlives a call before its
//! next use), the value used furthest ahead is evicted when none is free,
//! and a call saves the values in registers it clobbers that are used
//! after it.
//!
//! A handful of values used in most blocks (the frame JIT's state and
//! frame pointers) are *pinned*: kept in callee-saved registers for the
//! whole function.

use crate::asm::{bit, is_float, Abi, Asm, Cond, Frame, Label, Opnd, Reg, NOREG};
use crate::ir::{
    condcodes::FloatCC, Alu, Block, CallConv, Function, Inst, Op, Type, Value, ValueDef,
};
use crate::liveness::{analyze, each_use, successors, Analysis, NO_BLOCK};
use crate::Error;

const NONE: u32 = u32::MAX;
/// Used after the block.
const LIVEOUT: u32 = u32::MAX - 1;

/// Where a move reads from.
#[derive(Copy, Clone, Debug, PartialEq)]
enum Src {
    R(Reg),
    /// A spill slot, by byte offset, holding an integer or a float.
    Slot(i32, bool),
    I(i64),
    F(u64, Type),
}

/// Where a move writes to.
#[derive(Copy, Clone, Debug, PartialEq)]
enum Dst {
    R(Reg),
    Sp(i32),
}

/// A move that edge or call code makes.
type Mv = (Dst, Src);

/// A block's register state at the end of the edge into it.
#[derive(Clone, Debug)]
struct Snap {
    vals: Vec<(Value, Reg, bool)>,
}

/// A flag result left in the flags for the branch after it.
#[derive(Copy, Clone, Debug)]
enum Pending {
    Int(Cond, Value),
    Float(FloatCC, Value),
}

struct Gen<'f, A: Asm> {
    f: &'f Function,
    an: Analysis,
    a: A,
    abi: &'static Abi,
    conv: CallConv,
    // By value.
    reg_of: Vec<Reg>,
    home_ok: Vec<bool>,
    next: Vec<u32>,
    slot_of: Vec<u32>,
    tracked: Vec<bool>,
    pin: Vec<Reg>,
    /// The values in registers or slots, in no order.
    live: Vec<Value>,
    holds: [u32; 64],
    pinned: u64,
    locked: u64,
    callee_saved: u64,
    used: u64,
    nslots: u32,
    free_slots: Vec<u32>,
    out: u32,
    // By block.
    labels: Vec<Label>,
    params: Vec<Vec<Reg>>,
    snaps: Vec<Option<Snap>>,
    stubs: Vec<(Label, Vec<Mv>, Label)>,
    epi: Label,
    // The block being generated.
    arg_next: Vec<u32>,
    def_next: Vec<u32>,
    nu: Vec<u32>,
    nu_stamp: Vec<u32>,
    stamp: u32,
    calls_after: Vec<u32>,
    pending: Option<Pending>,
}

/// Whether to print each compile's phase times (`WPCG_TIME`, a tuning
/// aid).
fn timing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WPCG_TIME").is_some())
}

pub(crate) fn compile<A: Asm>(f: &Function) -> Result<Vec<u8>, Error> {
    if f.layout.is_empty() {
        return Err(Error::Unsupported("an empty function"));
    }
    let conv = f.signature.call_conv;
    let abi = A::abi(conv).ok_or(Error::Unsupported("the function's calling convention"))?;
    let t0 = std::time::Instant::now();
    let an = analyze(f);
    let t_an = t0.elapsed();
    let nv = f.dfg.values.len();
    let nb = f.dfg.blocks.len();
    let mut a = A::new();
    let labels = (0..nb).map(|_| a.new_label()).collect();
    let epi = a.new_label();
    let mut g = Gen {
        f,
        a,
        abi,
        conv,
        reg_of: vec![NOREG; nv],
        home_ok: vec![false; nv],
        next: vec![NONE; nv],
        slot_of: vec![NONE; nv],
        tracked: vec![false; nv],
        pin: vec![NOREG; nv],
        live: Vec::new(),
        holds: [NONE; 64],
        pinned: 0,
        locked: 0,
        callee_saved: A::callee_saved(conv),
        used: 0,
        nslots: 0,
        free_slots: Vec::new(),
        out: 0,
        labels,
        params: vec![Vec::new(); nb],
        snaps: vec![None; nb],
        stubs: Vec::new(),
        epi,
        arg_next: Vec::new(),
        def_next: Vec::new(),
        nu: vec![NONE; nv],
        nu_stamp: vec![u32::MAX; nv],
        stamp: 0,
        calls_after: Vec::new(),
        pending: None,
        an,
    };
    g.out = g.outgoing_area()?;
    g.choose_pins();
    g.entry()?;
    for i in 0..g.an.order.len() {
        let b = g.an.order[i];
        g.block(b, i)?;
    }
    let t_gen = t0.elapsed();
    if timing() {
        eprintln!("wpcg: {} insts {} blocks {} globals: analyze {:?} gen {:?}", f.dfg.insts.len(), f.dfg.blocks.len(), g.an.globals.len(), t_an, t_gen - t_an);
    }
    g.a.bind(g.epi);
    let frame = Frame {
        saved: g.used & g.callee_saved,
        slots: g.nslots * 8,
        out: g.out,
    };
    g.a.epilogue(&frame);
    for (l, moves, target) in std::mem::take(&mut g.stubs) {
        g.a.bind(l);
        g.moves(moves);
        g.a.jmp(target);
    }
    g.a.finish(&frame)
}

impl<A: Asm> Gen<'_, A> {
    // ---- setup ----

    /// The bytes below the spill slots that calls' stack arguments take.
    fn outgoing_area(&self) -> Result<u32, Error> {
        let mut most = 0;
        for &b in &self.an.order {
            for &inst in &self.f.dfg.blocks[b.index()].insts {
                if let Op::CallIndirect(sig) = self.f.dfg.insts[inst.index()].op {
                    let sig = &self.f.dfg.sigs[sig.index()];
                    let abi = A::abi(sig.call_conv)
                        .ok_or(Error::Unsupported("a callee's calling convention"))?;
                    let tys: Vec<Type> = sig.params.iter().map(|p| p.value_type).collect();
                    let (_, stack) = arg_locs(abi, &tys);
                    most = most.max(abi.shadow + 8 * stack);
                }
            }
        }
        Ok(most)
    }

    /// Pin the integer values live into the most blocks.
    fn choose_pins(&mut self) {
        let an = &self.an;
        let nblocks = an.order.len() as u32;
        let mut count = vec![0u32; an.globals.len()];
        for &b in &an.order {
            for (w, &word) in an.live_in(b).iter().enumerate() {
                let mut bits = word;
                while bits != 0 {
                    let i = bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    count[w * 64 + i] += 1;
                }
            }
        }
        let mut cands: Vec<(u32, Value)> = an
            .globals
            .iter()
            .enumerate()
            .filter(|&(g, &v)| {
                self.f.dfg.value_type(v).is_int() && count[g] * 4 >= nblocks && count[g] >= 2
            })
            .map(|(g, &v)| (count[g], v))
            .collect();
        cands.sort_by(|x, y| y.0.cmp(&x.0).then(x.1.cmp(&y.1)));
        let mut regs = A::INT_REGS
            .iter()
            .copied()
            .filter(|&r| self.callee_saved & bit(r) != 0)
            .rev();
        for &(_, v) in cands.iter().take(A::MAX_PINNED) {
            let Some(r) = regs.next() else { break };
            self.pin[v.index()] = r;
            self.pinned |= bit(r);
            self.used |= bit(r);
        }
    }

    /// The function's entry: its arguments moved to the entry block's
    /// parameters.
    fn entry(&mut self) -> Result<(), Error> {
        let entry = self.an.order[0];
        let tys: Vec<Type> = self
            .f
            .signature
            .params
            .iter()
            .map(|p| p.value_type)
            .collect();
        let (locs, stack) = arg_locs(self.abi, &tys);
        if stack > 0 {
            return Err(Error::Unsupported("stack parameters"));
        }
        let dsts = self.param_regs(entry);
        let moves = dsts
            .iter()
            .zip(&locs)
            .map(|(&d, &s)| (Dst::R(d), Src::R(s)))
            .collect();
        self.moves(moves);
        Ok(())
    }

    // ---- state ----

    fn ty(&self, v: Value) -> Type {
        self.f.dfg.value_type(v)
    }

    fn track(&mut self, v: Value, reg: Reg, home: bool, next: u32) {
        let i = v.index();
        if !self.tracked[i] {
            self.tracked[i] = true;
            self.live.push(v);
        }
        self.reg_of[i] = reg;
        self.home_ok[i] = home;
        self.next[i] = next;
        if reg != NOREG {
            self.holds[reg as usize] = v.0;
            self.used |= bit(reg);
        }
    }

    fn untrack(&mut self, v: Value) {
        let i = v.index();
        if !self.tracked[i] {
            return;
        }
        self.drop_reg(v);
        self.tracked[i] = false;
        if let Some(p) = self.live.iter().position(|&x| x == v) {
            self.live.swap_remove(p);
        }
        // A value of one block gives its slot back.
        if self.an.gidx[i] == u32::MAX && self.slot_of[i] != NONE {
            self.free_slots.push(self.slot_of[i]);
            self.slot_of[i] = NONE;
        }
    }

    fn drop_reg(&mut self, v: Value) {
        let r = self.reg_of[v.index()];
        if r != NOREG && self.pin[v.index()] == NOREG {
            self.holds[r as usize] = NONE;
            self.reg_of[v.index()] = NOREG;
        }
    }

    fn reset(&mut self) {
        for v in std::mem::take(&mut self.live) {
            let i = v.index();
            self.tracked[i] = false;
            self.reg_of[i] = NOREG;
            self.home_ok[i] = false;
        }
        self.holds = [NONE; 64];
    }

    fn snapshot(&self) -> Snap {
        Snap {
            vals: self
                .live
                .iter()
                .map(|&v| (v, self.reg_of[v.index()], self.home_ok[v.index()]))
                .collect(),
        }
    }

    fn slot(&mut self, v: Value) -> i32 {
        let i = v.index();
        if self.slot_of[i] == NONE {
            self.slot_of[i] = if self.an.gidx[i] == u32::MAX {
                self.free_slots.pop()
            } else {
                None
            }
            .unwrap_or_else(|| {
                self.nslots += 1;
                self.nslots - 1
            });
        }
        (self.out + 8 * self.slot_of[i]) as i32
    }

    fn is_remat(&self, v: Value) -> bool {
        self.an.remat[v.index()]
    }

    fn const_of(&self, v: Value) -> Option<Src> {
        let ValueDef::Result(inst, 0) = self.f.dfg.values[v.index()].def else {
            return None;
        };
        let d = &self.f.dfg.insts[inst.index()];
        match d.op {
            Op::Iconst(i) => Some(Src::I(i)),
            Op::Fconst(b) => Some(Src::F(b, d.ty)),
            _ => None,
        }
    }

    fn imm_of(&self, v: Value) -> Option<i64> {
        match self.const_of(v) {
            Some(Src::I(i)) => Some(i),
            _ => None,
        }
    }

    /// Where `v` is, for a move.
    fn src(&mut self, v: Value) -> Result<Src, Error> {
        let i = v.index();
        if self.tracked[i] && self.reg_of[i] != NOREG {
            return Ok(Src::R(self.reg_of[i]));
        }
        if let Some(c) = self.const_of(v) {
            return Ok(c);
        }
        if self.tracked[i] && self.home_ok[i] {
            let float = self.ty(v).is_float();
            return Ok(Src::Slot(self.slot(v), float));
        }
        Err(Error::Internal("a value with no location"))
    }

    // ---- registers ----

    fn free_reg(&self, float: bool, prefer_saved: bool) -> Option<Reg> {
        let regs = if float { A::FLOAT_REGS } else { A::INT_REGS };
        let avail = |r: Reg| {
            self.holds[r as usize] == NONE && (self.pinned | self.locked) & bit(r) == 0
        };
        if prefer_saved {
            if let Some(&r) = regs
                .iter()
                .find(|&&r| self.callee_saved & bit(r) != 0 && avail(r))
            {
                return Some(r);
            }
        }
        regs.iter().copied().find(|&r| avail(r))
    }

    /// A register for a new value: a free one, or one emptied by evicting
    /// the value used furthest ahead.
    fn alloc(&mut self, float: bool, prefer_saved: bool) -> Result<Reg, Error> {
        if let Some(r) = self.free_reg(float, prefer_saved) {
            return Ok(r);
        }
        let regs = if float { A::FLOAT_REGS } else { A::INT_REGS };
        let mut best: Option<(u32, Reg)> = None;
        for &r in regs {
            let held = self.holds[r as usize];
            if held == NONE || (self.pinned | self.locked) & bit(r) != 0 {
                continue;
            }
            let n = self.next[held as usize];
            if best.is_none_or(|(bn, _)| n > bn) {
                best = Some((n, r));
            }
        }
        let (_, r) = best.ok_or(Error::Internal("out of registers"))?;
        self.evict(r);
        Ok(r)
    }

    /// Empty `r`, saving what it holds.
    fn evict(&mut self, r: Reg) {
        let held = self.holds[r as usize];
        if held == NONE {
            return;
        }
        let v = Value(held);
        self.spill(v);
        self.drop_reg(v);
    }

    /// Make sure `v` is in its slot (or can be recomputed).
    fn spill(&mut self, v: Value) {
        let i = v.index();
        if self.home_ok[i] || self.is_remat(v) || self.pin[i] != NOREG {
            return;
        }
        let r = self.reg_of[i];
        let off = self.slot(v);
        self.a.st_sp(r, off);
        self.home_ok[i] = true;
    }

    /// `v` in a register (locked for the current instruction).
    fn use_reg(&mut self, v: Value) -> Result<Reg, Error> {
        let i = v.index();
        let r = if self.tracked[i] && self.reg_of[i] != NOREG {
            self.reg_of[i]
        } else {
            let float = self.ty(v).is_float();
            let src = self.src(v)?;
            let r = self.alloc(float, false)?;
            match src {
                Src::I(imm) => self.a.imm(r, imm),
                Src::F(bits, ty) => self.a.fimm(r, bits, ty),
                Src::Slot(off, _) => self.a.ld_sp(r, off),
                Src::R(_) => unreachable!(),
            }
            let home = self.tracked[i] && self.home_ok[i];
            let next = if self.tracked[i] { self.next[i] } else { NONE };
            self.track(v, r, home, next);
            r
        };
        self.locked |= bit(r);
        Ok(r)
    }

    /// `v` as an operand that may be an immediate `ok` accepts.
    fn opnd(&mut self, v: Value, ok: impl Fn(i64) -> bool) -> Result<Opnd, Error> {
        if let Some(i) = self.imm_of(v) {
            if ok(i) && !(self.tracked[v.index()] && self.reg_of[v.index()] != NOREG) {
                return Ok(Opnd::I(i));
            }
        }
        Ok(Opnd::R(self.use_reg(v)?))
    }

    /// Take inst `k`'s uses (its operands' next uses), freeing the
    /// registers of values that die here: `at` indexes the block's uses.
    fn consume(&mut self, inst: Inst, at: &mut usize) {
        let mut dead: Vec<Value> = Vec::new();
        let f = self.f;
        each_use(f, inst, |v| {
            let n = self.arg_next[*at];
            *at += 1;
            if self.tracked[v.index()] {
                self.next[v.index()] = n;
                if n == NONE && self.pin[v.index()] == NOREG {
                    dead.push(v);
                }
            }
        });
        for v in dead {
            if self.tracked[v.index()] && self.next[v.index()] == NONE {
                self.untrack(v);
            }
        }
    }

    /// A register for the result `v`, defined at `k` with its next use
    /// `next`: its pinned one, `prefer` (a dying operand's), or a new one.
    fn def(&mut self, v: Value, k: usize, next: u32, prefer: Option<Reg>) -> Result<Reg, Error> {
        let pinned = self.pin[v.index()];
        if pinned != NOREG {
            self.track(v, pinned, false, next);
            return Ok(pinned);
        }
        let float = self.ty(v).is_float();
        let across = next != NONE && self.calls_after[k] != NONE && next > self.calls_after[k];
        let r = match prefer {
            Some(p)
                if self.holds[p as usize] == NONE
                    && self.pinned & bit(p) == 0
                    && is_float(p) == float
                    && (!across || self.callee_saved & bit(p) != 0) =>
            {
                p
            }
            _ => self.alloc(float, across)?,
        };
        self.track(v, r, false, next);
        Ok(r)
    }

    // ---- blocks ----

    /// The registers `b`'s parameters arrive in.
    fn param_regs(&mut self, b: Block) -> Vec<Reg> {
        if !self.params[b.index()].is_empty() || self.f.dfg.blocks[b.index()].params.is_empty() {
            return self.params[b.index()].clone();
        }
        let mut taken = self.pinned;
        // The entry's parameters stay where the caller put them.
        let entry_locs = if b == self.an.order[0] {
            let tys: Vec<Type> = self.f.signature.params.iter().map(|p| p.value_type).collect();
            arg_locs(self.abi, &tys).0
        } else {
            Vec::new()
        };
        let pool_has = |r: Reg| A::INT_REGS.contains(&r) || A::FLOAT_REGS.contains(&r);
        let regs: Vec<Reg> = self.f.dfg.blocks[b.index()]
            .params
            .iter()
            .enumerate()
            .map(|(i, &p)| {
                if self.pin[p.index()] != NOREG {
                    return self.pin[p.index()];
                }
                if let Some(&r) = entry_locs.get(i) {
                    if r < 64 && pool_has(r) && taken & bit(r) == 0 {
                        taken |= bit(r);
                        return r;
                    }
                }
                let pool = if self.ty(p).is_float() {
                    A::FLOAT_REGS
                } else {
                    A::INT_REGS
                };
                let r = pool
                    .iter()
                    .copied()
                    .find(|&r| taken & bit(r) == 0 && self.callee_saved & bit(r) == 0)
                    .or_else(|| pool.iter().copied().find(|&r| taken & bit(r) == 0))
                    .expect("a register for a block parameter");
                taken |= bit(r);
                r
            })
            .collect();
        self.params[b.index()] = regs.clone();
        regs
    }

    /// The uses of `b`'s instructions, each with the position of the next
    /// use of its value after it (see `arg_next`), and its results'.
    fn next_uses(&mut self, b: Block) {
        let f = self.f;
        let insts = &f.dfg.blocks[b.index()].insts;
        self.stamp = self.stamp.wrapping_add(1);
        let stamp = self.stamp;
        // Live out: the successors' live-ins.
        if let Some(&last) = insts.last() {
            let an = &self.an;
            let (nu, nu_stamp) = (&mut self.nu, &mut self.nu_stamp);
            successors(f, last, |s| {
                for (w, &word) in an.live_in(s).iter().enumerate() {
                    let mut bits = word;
                    while bits != 0 {
                        let i = bits.trailing_zeros() as usize;
                        bits &= bits - 1;
                        let v = an.globals[w * 64 + i];
                        nu[v.index()] = LIVEOUT;
                        nu_stamp[v.index()] = stamp;
                    }
                }
            });
        }
        let mut nuses = 0;
        let mut nres = 0;
        for &inst in insts {
            each_use(f, inst, |_| nuses += 1);
            nres += f.dfg.results(inst).len();
        }
        self.arg_next.clear();
        self.arg_next.resize(nuses, NONE);
        self.def_next.clear();
        self.def_next.resize(nres, NONE);
        self.calls_after.clear();
        self.calls_after.resize(insts.len(), NONE);
        let mut at = nuses;
        let mut rat = nres;
        let mut call = NONE;
        let mut occ: Vec<Value> = Vec::new();
        for (k, &inst) in insts.iter().enumerate().rev() {
            let res = f.dfg.results(inst);
            rat -= res.len();
            for (j, &r) in res.iter().enumerate() {
                self.def_next[rat + j] = if self.nu_stamp[r.index()] == stamp {
                    self.nu[r.index()]
                } else {
                    NONE
                };
            }
            // The first call after `k`.
            self.calls_after[k] = call;
            if matches!(f.dfg.insts[inst.index()].op, Op::CallIndirect(_)) {
                call = k as u32;
            }
            occ.clear();
            each_use(f, inst, |v| occ.push(v));
            at -= occ.len();
            for (j, &v) in occ.iter().enumerate() {
                self.arg_next[at + j] = if self.nu_stamp[v.index()] == stamp {
                    self.nu[v.index()]
                } else {
                    NONE
                };
            }
            for &v in &occ {
                self.nu[v.index()] = k as u32;
                self.nu_stamp[v.index()] = stamp;
            }
        }
    }

    /// The first use of `v` in the block `next_uses` ran over.
    fn first_use(&self, v: Value) -> u32 {
        if self.nu_stamp[v.index()] == self.stamp {
            self.nu[v.index()]
        } else {
            NONE
        }
    }

    fn block(&mut self, b: Block, pos: usize) -> Result<(), Error> {
        self.next_uses(b);
        // The state at the start.
        if let Some(snap) = self.snaps[b.index()].take() {
            self.reset();
            for (v, r, home) in snap.vals {
                let n = self.first_use(v);
                if n != NONE || self.pin[v.index()] != NOREG {
                    self.track(v, r, home, n);
                }
            }
        } else {
            debug_assert!(!self.an.inherits(b), "an inheriting block without a state");
            self.reset();
            let regs = self.param_regs(b);
            let params = self.f.dfg.blocks[b.index()].params.clone();
            for (&p, &r) in params.iter().zip(&regs) {
                let n = self.first_use(p);
                if n != NONE || self.pin[p.index()] != NOREG {
                    self.track(p, r, false, n);
                }
            }
            let words = self.an.live_in(b).to_vec();
            for (w, &word) in words.iter().enumerate() {
                let mut bits = word;
                while bits != 0 {
                    let i = bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    let v = self.an.globals[w * 64 + i];
                    if self.is_remat(v) {
                        continue;
                    }
                    let n = self.first_use(v);
                    let pr = self.pin[v.index()];
                    if pr != NOREG {
                        self.track(v, pr, false, n);
                    } else {
                        self.track(v, NOREG, true, n);
                    }
                }
            }
        }
        self.a.bind(self.labels[b.index()]);
        let f = self.f;
        let insts = &f.dfg.blocks[b.index()].insts;
        let mut at = 0;
        let mut rat = 0;
        for (k, &inst) in insts.iter().enumerate() {
            self.locked = 0;
            let nres = f.dfg.results(inst).len();
            let next_inst = insts.get(k + 1).copied();
            self.inst(inst, k, pos, &mut at, rat, next_inst)?;
            rat += nres;
        }
        self.locked = 0;
        Ok(())
    }

    // ---- instructions ----

    fn res(&self, inst: Inst, j: usize) -> Value {
        self.f.dfg.results(inst)[j]
    }

    /// Whether `v`'s only use is as the condition of `next`, a `brif`.
    fn fuses(&self, v: Value, next: Option<Inst>) -> bool {
        let Some(next) = next else { return false };
        let d = &self.f.dfg.insts[next.index()];
        matches!(d.op, Op::Brif(..))
            && self.f.dfg.slice(d.args)[0] == v
            && self.an.uses[v.index()] == 1
    }

    #[allow(clippy::too_many_lines)]
    fn inst(
        &mut self,
        inst: Inst,
        k: usize,
        pos: usize,
        at: &mut usize,
        rat: usize,
        next: Option<Inst>,
    ) -> Result<(), Error> {
        let f = self.f;
        let d = &f.dfg.insts[inst.index()];
        let args = f.dfg.slice(d.args);
        let dn = |g: &Self, j: usize| g.def_next[rat + j];
        match &d.op {
            Op::Iconst(_) | Op::Fconst(_) => {
                // Materialized where used.
                self.consume(inst, at);
            }
            Op::Load { bytes, ext, off } => {
                let base = self.use_reg(args[0])?;
                self.consume(inst, at);
                let v = self.res(inst, 0);
                let r = self.def(v, k, dn(self, 0), Some(base))?;
                self.a.load(*bytes, *ext, d.ty, r, base, *off);
                self.unused(v, dn(self, 0));
            }
            Op::Store { bytes, off } => {
                let base = self.use_reg(args[1])?;
                let stored = match self.imm_of(args[0]) {
                    Some(i) if !self.in_reg(args[0]) && self.a.store_imm(*bytes, i, base, *off) => {
                        true
                    }
                    _ => false,
                };
                if !stored {
                    let x = self.use_reg(args[0])?;
                    self.a.store(*bytes, x, base, *off);
                }
                self.consume(inst, at);
            }
            Op::Icmp(cc) => {
                let ty = self.ty(args[0]);
                let mut c = Cond::from_int(*cc);
                let (x, y) = if self.imm_of(args[0]).is_some() && self.imm_of(args[1]).is_none() {
                    c = c.swapped();
                    (args[1], args[0])
                } else {
                    (args[0], args[1])
                };
                let a = self.use_reg(x)?;
                let b = self.opnd(y, A::cmp_imm_ok)?;
                self.compare(inst, k, at, rat, next, ty, c, a, b)?;
            }
            Op::IcmpImm(cc, imm) => {
                let ty = self.ty(args[0]);
                let a = self.use_reg(args[0])?;
                let b = if A::cmp_imm_ok(*imm) {
                    Opnd::I(*imm)
                } else {
                    let r = self.alloc(false, false)?;
                    self.a.imm(r, *imm);
                    self.locked |= bit(r);
                    Opnd::R(r)
                };
                self.compare(inst, k, at, rat, next, ty, Cond::from_int(*cc), a, b)?;
            }
            Op::Fcmp(cc) => {
                let ty = self.ty(args[0]);
                let a = self.use_reg(args[0])?;
                let b = self.use_reg(args[1])?;
                let v = self.res(inst, 0);
                if self.fuses(v, next) {
                    self.consume(inst, at);
                    self.a.fcmp(*cc, ty, a, b);
                    self.pending = Some(Pending::Float(*cc, v));
                } else {
                    self.consume(inst, at);
                    let r = self.def(v, k, dn(self, 0), None)?;
                    self.a.fcmp(*cc, ty, a, b);
                    self.a.fsetcc(*cc, r);
                    self.unused(v, dn(self, 0));
                }
            }
            Op::Alu(op) => {
                let ty = d.ty;
                if matches!(op, Alu::Shl | Alu::Ushr | Alu::Sshr) && A::SHIFT_REG.is_some() {
                    if let Some(i) = self.imm_of(args[1]) {
                        let a = self.use_reg(args[0])?;
                        self.consume(inst, at);
                        let v = self.res(inst, 0);
                        let r = self.def(v, k, dn(self, 0), Some(a))?;
                        self.a.alu(*op, ty, r, a, Opnd::I(i));
                        self.unused(v, dn(self, 0));
                        return Ok(());
                    }
                    let cx = A::SHIFT_REG.unwrap();
                    self.locked |= bit(cx);
                    self.clear_reg(cx)?;
                    let a = self.use_reg(args[0])?;
                    let b = self.use_reg(args[1])?;
                    self.a.mov(cx, b);
                    self.locked |= bit(cx);
                    self.consume(inst, at);
                    let v = self.res(inst, 0);
                    let r = self.def(v, k, dn(self, 0), Some(a))?;
                    self.a.alu(*op, ty, r, a, Opnd::R(cx));
                    self.unused(v, dn(self, 0));
                    return Ok(());
                }
                let a = self.use_reg(args[0])?;
                let op2 = *op;
                let b = self.opnd(args[1], |i| A::alu_imm_ok(op2, i))?;
                self.consume(inst, at);
                let v = self.res(inst, 0);
                let r = self.def(v, k, dn(self, 0), Some(a))?;
                self.a.alu(*op, ty, r, a, b);
                self.unused(v, dn(self, 0));
            }
            Op::AluImm(op, imm) => {
                let ty = d.ty;
                let a = self.use_reg(args[0])?;
                let b = if A::alu_imm_ok(*op, *imm) {
                    Opnd::I(*imm)
                } else {
                    let r = self.alloc(false, false)?;
                    self.a.imm(r, *imm);
                    self.locked |= bit(r);
                    Opnd::R(r)
                };
                self.consume(inst, at);
                let v = self.res(inst, 0);
                let r = self.def(v, k, dn(self, 0), Some(a))?;
                self.a.alu(*op, ty, r, a, b);
                self.unused(v, dn(self, 0));
            }
            Op::Div(op) => {
                let ty = d.ty;
                if let Some((ax, dx)) = A::DIV_REGS {
                    self.locked |= bit(ax) | bit(dx);
                    self.clear_reg(ax)?;
                    self.clear_reg(dx)?;
                }
                let a = self.use_reg(args[0])?;
                let b = self.use_reg(args[1])?;
                self.consume(inst, at);
                let v = self.res(inst, 0);
                let r = self.def(v, k, dn(self, 0), None)?;
                self.a.div(*op, ty, r, a, b);
                self.unused(v, dn(self, 0));
            }
            Op::Smulhi => {
                let ty = d.ty;
                if let Some((ax, dx)) = A::DIV_REGS {
                    self.locked |= bit(ax) | bit(dx);
                    self.clear_reg(ax)?;
                    self.clear_reg(dx)?;
                }
                let a = self.use_reg(args[0])?;
                let b = self.use_reg(args[1])?;
                self.consume(inst, at);
                let v = self.res(inst, 0);
                let r = self.def(v, k, dn(self, 0), None)?;
                self.a.smulhi(ty, r, a, b);
                self.unused(v, dn(self, 0));
            }
            Op::Bnot | Op::Ineg | Op::Fneg | Op::Fabs | Op::Sqrt => {
                let ty = d.ty;
                let a = self.use_reg(args[0])?;
                self.consume(inst, at);
                let v = self.res(inst, 0);
                let r = self.def(v, k, dn(self, 0), Some(a))?;
                match d.op {
                    Op::Bnot => self.a.not(ty, r, a),
                    Op::Ineg => self.a.neg(ty, r, a),
                    Op::Fneg => self.a.fneg(ty, r, a),
                    Op::Fabs => self.a.fabs(ty, r, a),
                    _ => self.a.fsqrt(ty, r, a),
                }
                self.unused(v, dn(self, 0));
            }
            Op::Select => {
                let ty = d.ty;
                let cty = self.ty(args[0]);
                let c = self.use_reg(args[0])?;
                let x = self.use_reg(args[1])?;
                let y = self.use_reg(args[2])?;
                self.consume(inst, at);
                let v = self.res(inst, 0);
                let r = self.def(v, k, dn(self, 0), None)?;
                self.a.test(cty, c);
                self.a.csel(Cond::Ne, ty, r, x, y);
                self.unused(v, dn(self, 0));
            }
            Op::Ireduce => {
                let a = self.use_reg(args[0])?;
                self.consume(inst, at);
                let v = self.res(inst, 0);
                let r = self.def(v, k, dn(self, 0), Some(a))?;
                if r != a {
                    self.a.mov(r, a);
                }
                self.unused(v, dn(self, 0));
            }
            Op::Uextend | Op::Sextend => {
                let from = self.ty(args[0]);
                let a = self.use_reg(args[0])?;
                self.consume(inst, at);
                let v = self.res(inst, 0);
                let r = self.def(v, k, dn(self, 0), Some(a))?;
                self.a
                    .extend(matches!(d.op, Op::Sextend), from, d.ty, r, a);
                self.unused(v, dn(self, 0));
            }
            Op::Overflow(op) => {
                let ty = d.ty;
                let a = self.use_reg(args[0])?;
                let b = self.use_reg(args[1])?;
                self.consume(inst, at);
                let (v, flag) = (self.res(inst, 0), self.res(inst, 1));
                let r = self.def(v, k, dn(self, 0), Some(a))?;
                if self.fuses(flag, next) {
                    self.a.overflow(*op, ty, r, a, b);
                    self.pending = Some(Pending::Int(Cond::O, flag));
                } else {
                    let fr = self.def(flag, k, dn(self, 1), None)?;
                    self.a.overflow(*op, ty, r, a, b);
                    self.a.setcc(Cond::O, fr);
                    self.unused(flag, dn(self, 1));
                }
                self.unused(v, dn(self, 0));
            }
            Op::Bitcast => {
                let a = self.use_reg(args[0])?;
                self.consume(inst, at);
                let v = self.res(inst, 0);
                let r = self.def(v, k, dn(self, 0), None)?;
                self.a.bitcast(d.ty, r, a);
                self.unused(v, dn(self, 0));
            }
            Op::Fop(op) => {
                let ty = d.ty;
                let a = self.use_reg(args[0])?;
                let b = self.use_reg(args[1])?;
                self.consume(inst, at);
                let v = self.res(inst, 0);
                let r = self.def(v, k, dn(self, 0), Some(a))?;
                self.a.fop(*op, ty, r, a, b);
                self.unused(v, dn(self, 0));
            }
            Op::FcvtFromSint => {
                let from = self.ty(args[0]);
                let a = self.use_reg(args[0])?;
                self.consume(inst, at);
                let v = self.res(inst, 0);
                let r = self.def(v, k, dn(self, 0), None)?;
                self.a.fcvt_from_sint(from, d.ty, r, a);
                self.unused(v, dn(self, 0));
            }
            Op::CallIndirect(sig) => self.call(inst, k, at, rat, *sig)?,
            Op::Jump(e) => {
                // (The edge reads its arguments before their uses end.)
                let moves = self.edge(e.block, f.dfg.slice(e.args))?;
                self.consume(inst, at);
                self.goto(pos, e.block, moves);
            }
            Op::Brif(t, e) => {
                let cond = args[0];
                let c = match self.pending.take() {
                    Some(Pending::Int(c, v)) if v == cond => Some(Ok(c)),
                    Some(Pending::Float(cc, v)) if v == cond => Some(Err(cc)),
                    _ => None,
                };
                let c = match c {
                    Some(c) => c,
                    None => {
                        let ty = self.ty(cond);
                        let r = self.use_reg(cond)?;
                        self.a.test(ty, r);
                        Ok(Cond::Ne)
                    }
                };
                self.branch(pos, c, (t.block, f.dfg.slice(t.args)), (e.block, f.dfg.slice(e.args)))?;
                self.consume(inst, at);
            }
            Op::BrTable(jt) => {
                let ty = self.ty(args[0]);
                let idx = self.use_reg(args[0])?;
                let table = &f.dfg.tables[jt.index()];
                let default = self.target(table.default.block, &table.default.args)?;
                let mut labels = Vec::with_capacity(table.table.len());
                for c in &table.table {
                    labels.push(self.target(c.block, &c.args)?);
                }
                self.consume(inst, at);
                self.a.br_table(idx, ty, default, &labels);
            }
            Op::Return => {
                let mut moves = Vec::new();
                let (mut ni, mut nf) = (0, 0);
                for &v in args {
                    let dst = if self.ty(v).is_float() {
                        nf += 1;
                        *self.abi.float_rets.get(nf - 1).ok_or(Error::Unsupported("returns"))?
                    } else {
                        ni += 1;
                        *self.abi.int_rets.get(ni - 1).ok_or(Error::Unsupported("returns"))?
                    };
                    moves.push((Dst::R(dst), self.src(v)?));
                }
                self.consume(inst, at);
                self.moves(moves);
                // (The epilogue follows the last block.)
                if pos + 1 != self.an.order.len() {
                    self.a.jmp(self.epi);
                }
            }
        }
        Ok(())
    }

    fn in_reg(&self, v: Value) -> bool {
        self.tracked[v.index()] && self.reg_of[v.index()] != NOREG
    }

    /// Drop a result no one uses.
    fn unused(&mut self, v: Value, next: u32) {
        if next == NONE && self.pin[v.index()] == NOREG {
            self.untrack(v);
        }
    }

    /// Empty `r` for an instruction's fixed use, moving what it holds to
    /// another register if one's free.
    fn clear_reg(&mut self, r: Reg) -> Result<(), Error> {
        let held = self.holds[r as usize];
        if held == NONE {
            return Ok(());
        }
        let v = Value(held);
        let was = self.locked;
        self.locked |= bit(r);
        let to = self.free_reg(is_float(r), false);
        self.locked = was;
        match to {
            Some(to) => {
                self.a.mov(to, r);
                self.holds[r as usize] = NONE;
                self.holds[to as usize] = held;
                self.reg_of[v.index()] = to;
                self.used |= bit(to);
            }
            None => self.evict(r),
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn compare(
        &mut self,
        inst: Inst,
        k: usize,
        at: &mut usize,
        rat: usize,
        next: Option<Inst>,
        ty: Type,
        c: Cond,
        a: Reg,
        b: Opnd,
    ) -> Result<(), Error> {
        let v = self.res(inst, 0);
        if self.fuses(v, next) {
            self.consume(inst, at);
            self.a.cmp(ty, a, b);
            self.pending = Some(Pending::Int(c, v));
            return Ok(());
        }
        self.consume(inst, at);
        let dn = self.def_next[rat];
        let r = self.def(v, k, dn, None)?;
        self.a.cmp(ty, a, b);
        self.a.setcc(c, r);
        self.unused(v, dn);
        Ok(())
    }

    // ---- calls ----

    fn call(
        &mut self,
        inst: Inst,
        k: usize,
        at: &mut usize,
        rat: usize,
        sig: crate::ir::SigRef,
    ) -> Result<(), Error> {
        let f = self.f;
        let sig = &f.dfg.sigs[sig.index()];
        let abi = A::abi(sig.call_conv).ok_or(Error::Unsupported("a calling convention"))?;
        let args = f.dfg.args(inst);
        let tys: Vec<Type> = sig.params.iter().map(|p| p.value_type).collect();
        let (locs, _) = arg_locs(abi, &tys);
        // The operands' locations, before anything moves.
        let mut srcs = Vec::with_capacity(args.len());
        for &v in args {
            srcs.push(self.src(v)?);
        }
        // Take the uses: what dies here needn't survive the call.
        let mut dead = Vec::new();
        each_use(f, inst, |v| {
            let n = self.arg_next[*at];
            *at += 1;
            if self.tracked[v.index()] {
                self.next[v.index()] = n;
                if n == NONE {
                    dead.push(v);
                }
            }
        });
        // Save what lives on in the registers the call clobbers: in a free
        // callee-saved register, or in its slot.
        let survivors: Vec<Value> = self
            .live
            .iter()
            .copied()
            .filter(|&v| {
                let r = self.reg_of[v.index()];
                r != NOREG
                    && abi.clobbers & bit(r) != 0
                    && self.pin[v.index()] == NOREG
                    && self.next[v.index()] != NONE
            })
            .collect();
        let mut stored = Vec::new();
        for v in survivors {
            let r = self.reg_of[v.index()];
            let float = is_float(r);
            let to = (if float { A::FLOAT_REGS } else { A::INT_REGS })
                .iter()
                .copied()
                .find(|&s| {
                    self.callee_saved & bit(s) != 0
                        && abi.clobbers & bit(s) == 0
                        && self.holds[s as usize] == NONE
                        && self.pinned & bit(s) == 0
                });
            match to {
                Some(s) => {
                    self.a.mov(s, r);
                    self.holds[r as usize] = NONE;
                    self.holds[s as usize] = v.0;
                    self.reg_of[v.index()] = s;
                    self.used |= bit(s);
                    for src in &mut srcs {
                        if *src == Src::R(r) {
                            *src = Src::R(s);
                        }
                    }
                }
                None => {
                    self.spill(v);
                    stored.push(v);
                }
            }
        }
        // The arguments, and the callee.
        let mut moves: Vec<Mv> = vec![(Dst::R(A::CALL_REG), srcs[0])];
        for (loc, src) in locs.iter().zip(&srcs[1..]) {
            let dst = match *loc {
                l if l < 64 => Dst::R(l),
                l => Dst::Sp((abi.shadow + 8 * u32::from(l - 64)) as i32),
            };
            moves.push((dst, *src));
        }
        self.moves(moves);
        // The clobbered registers' values are gone (saved, or dead).
        for v in stored {
            self.drop_reg(v);
        }
        for v in dead {
            if self.tracked[v.index()] && self.next[v.index()] == NONE {
                self.untrack(v);
            }
        }
        for r in 0..64u8 {
            if abi.clobbers & bit(r) != 0 && self.holds[r as usize] != NONE {
                let v = Value(self.holds[r as usize]);
                if self.pin[v.index()] == NOREG {
                    self.untrack(v);
                }
            }
        }
        self.a.call(A::CALL_REG);
        let results = f.dfg.results(inst);
        let (mut ni, mut nf) = (0, 0);
        for (j, &v) in results.iter().enumerate() {
            let r = if self.ty(v).is_float() {
                nf += 1;
                *abi.float_rets.get(nf - 1).ok_or(Error::Unsupported("returns"))?
            } else {
                ni += 1;
                *abi.int_rets.get(ni - 1).ok_or(Error::Unsupported("returns"))?
            };
            let n = self.def_next[rat + j];
            let pinned = self.pin[v.index()];
            if pinned != NOREG {
                self.a.mov(pinned, r);
                self.track(v, pinned, false, n);
            } else if n != NONE {
                self.track(v, r, false, n);
            }
        }
        // A result that outlives the next call goes to a callee-saved
        // register now, while one's free.
        for &v in results {
            if !self.in_reg(v) || self.pin[v.index()] != NOREG {
                continue;
            }
            let n = self.next[v.index()];
            let call = self.calls_after[k];
            if call != NONE && n > call && n != NONE {
                let r = self.reg_of[v.index()];
                if let Some(s) = self.free_reg(is_float(r), true) {
                    if self.callee_saved & bit(s) != 0 {
                        self.a.mov(s, r);
                        self.holds[r as usize] = NONE;
                        self.holds[s as usize] = v.0;
                        self.reg_of[v.index()] = s;
                        self.used |= bit(s);
                    }
                }
            }
        }
        Ok(())
    }

    // ---- edges ----

    /// The code an edge to `t` with `args` needs: none into a block that
    /// inherits this state (saved for it), and otherwise the live values
    /// written to their slots and the arguments moved to `t`'s parameter
    /// registers.
    fn edge(&mut self, t: Block, args: &[Value]) -> Result<Vec<Mv>, Error> {
        if self.an.single_pred[t.index()] != NO_BLOCK {
            self.snaps[t.index()] = Some(self.snapshot());
            return Ok(Vec::new());
        }
        let mut moves = Vec::new();
        let words = self.an.live_in(t).to_vec();
        for (w, &word) in words.iter().enumerate() {
            let mut bits = word;
            while bits != 0 {
                let i = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let v = self.an.globals[w * 64 + i];
                if self.is_remat(v) || self.pin[v.index()] != NOREG || self.home_ok[v.index()] {
                    continue;
                }
                let r = self.reg_of[v.index()];
                if !self.tracked[v.index()] || r == NOREG {
                    return Err(Error::Internal("a live value with no location"));
                }
                let off = self.slot(v);
                moves.push((Dst::Sp(off), Src::R(r)));
            }
        }
        let regs = self.param_regs(t);
        for (&v, &r) in args.iter().zip(&regs) {
            let src = self.src(v)?;
            if src != Src::R(r) {
                moves.push((Dst::R(r), src));
            }
        }
        Ok(moves)
    }

    /// The label a branch to `t` jumps to: `t`'s own, or a stub's that does
    /// the edge's moves first.
    fn target(&mut self, t: Block, args: &[Value]) -> Result<Label, Error> {
        if self.an.single_pred[t.index()] != NO_BLOCK {
            return Err(Error::Internal("a table branch into an inheriting block"));
        }
        let moves = self.edge(t, args)?;
        if moves.is_empty() {
            return Ok(self.labels[t.index()]);
        }
        let l = self.a.new_label();
        self.stubs.push((l, moves, self.labels[t.index()]));
        Ok(l)
    }

    fn is_next(&self, pos: usize, b: Block) -> bool {
        self.an.order.get(pos + 1) == Some(&b)
    }

    fn goto(&mut self, pos: usize, t: Block, moves: Vec<Mv>) {
        self.moves(moves);
        if !self.is_next(pos, t) {
            self.a.jmp(self.labels[t.index()]);
        }
    }

    fn branch(
        &mut self,
        pos: usize,
        c: Result<Cond, FloatCC>,
        (tb, ta): (Block, &[Value]),
        (eb, ea): (Block, &[Value]),
    ) -> Result<(), Error> {
        // Both edges see the state at the branch.
        let tm = self.edge(tb, ta)?;
        let em = self.edge(eb, ea)?;
        let jump = |g: &mut Self, c: Result<Cond, FloatCC>, l: Label| match c {
            Ok(c) => g.a.jcc(c, l),
            Err(cc) => g.a.fjcc(cc, l),
        };
        let invertible = match c {
            Ok(_) => true,
            Err(cc) => matches!(
                cc,
                FloatCC::Equal | FloatCC::NotEqual | FloatCC::Ordered | FloatCC::Unordered
            ),
        };
        let inverse = |c: Result<Cond, FloatCC>| match c {
            Ok(c) => Ok(c.inverse()),
            Err(cc) => Err(cc.inverse()),
        };
        if invertible && self.is_next(pos, tb) && tm.is_empty() {
            // Fall into the then block.
            let l = self.stub(eb, em);
            jump(self, inverse(c), l);
            return Ok(());
        }
        let l = self.stub(tb, tm);
        jump(self, c, l);
        self.goto(pos, eb, em);
        Ok(())
    }

    /// The label for an edge to `t` with `moves`.
    fn stub(&mut self, t: Block, moves: Vec<Mv>) -> Label {
        if moves.is_empty() {
            return self.labels[t.index()];
        }
        let l = self.a.new_label();
        self.stubs.push((l, moves, self.labels[t.index()]));
        l
    }

    // ---- moves ----

    /// Make `moves` as one parallel assignment.
    fn moves(&mut self, mut moves: Vec<Mv>) {
        moves.retain(|&(d, s)| !matches!((d, s), (Dst::R(r), Src::R(q)) if r == q));
        // Into memory first, while every source is in place.
        for &(d, s) in &moves {
            let Dst::Sp(off) = d else { continue };
            match s {
                Src::R(r) => self.a.st_sp(r, off),
                Src::Slot(from, float) => {
                    let t = if float { A::FSCRATCH } else { A::SCRATCH };
                    self.a.ld_sp(t, from);
                    self.a.st_sp(t, off);
                }
                Src::I(i) => {
                    self.a.imm(A::SCRATCH, i);
                    self.a.st_sp(A::SCRATCH, off);
                }
                Src::F(bits, ty) => {
                    self.a.fimm(A::FSCRATCH, bits, ty);
                    self.a.st_sp(A::FSCRATCH, off);
                }
            }
        }
        // Register to register, in an order that reads each source before
        // it's overwritten, cycles broken by swaps.
        let mut pending: Vec<(Reg, Reg)> = moves
            .iter()
            .filter_map(|&(d, s)| match (d, s) {
                (Dst::R(d), Src::R(s)) => Some((d, s)),
                _ => None,
            })
            .collect();
        while !pending.is_empty() {
            let ready = (0..pending.len()).find(|&i| {
                let d = pending[i].0;
                !pending.iter().any(|&(_, s)| s == d)
            });
            match ready {
                Some(i) => {
                    let (d, s) = pending.swap_remove(i);
                    self.a.mov(d, s);
                }
                None => {
                    let (d, s) = pending.swap_remove(0);
                    self.a.swap(d, s);
                    for m in &mut pending {
                        if m.1 == d {
                            m.1 = s;
                        } else if m.1 == s {
                            m.1 = d;
                        }
                    }
                    pending.retain(|&(d, s)| d != s);
                }
            }
        }
        // Then what comes from memory or immediates.
        for &(d, s) in &moves {
            let Dst::R(r) = d else { continue };
            match s {
                Src::R(_) => {}
                Src::Slot(off, _) => self.a.ld_sp(r, off),
                Src::I(i) => self.a.imm(r, i),
                Src::F(bits, ty) => self.a.fimm(r, bits, ty),
            }
        }
    }
}

/// Where each of a call's arguments goes: a register, or `64 + n` for the
/// `n`th stack slot; and how many stack slots they take.
fn arg_locs(abi: &Abi, tys: &[Type]) -> (Vec<Reg>, u32) {
    let (mut ni, mut nf, mut stack) = (0usize, 0usize, 0u32);
    let mut locs = Vec::with_capacity(tys.len());
    for (i, ty) in tys.iter().enumerate() {
        let reg = if abi.positional {
            if ty.is_float() {
                abi.float_args.get(i)
            } else {
                abi.int_args.get(i)
            }
        } else if ty.is_float() {
            nf += 1;
            abi.float_args.get(nf - 1)
        } else {
            ni += 1;
            abi.int_args.get(ni - 1)
        };
        match reg {
            Some(&r) => locs.push(r),
            None => {
                locs.push(64 + stack as u8);
                stack += 1;
            }
        }
    }
    (locs, stack)
}
