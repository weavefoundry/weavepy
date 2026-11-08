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
use crate::ir::{condcodes::FloatCC, Alu, Block, Function, Inst, Op, Type, Value, ValueDef};
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
    /// A pinned register plus a constant.
    Lea(Reg, i64),
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
    // By value.
    reg_of: Vec<Reg>,
    home_ok: Vec<bool>,
    next: Vec<u32>,
    slot_of: Vec<u32>,
    tracked: Vec<bool>,
    pin: Vec<Reg>,
    /// Values that are a pinned value plus a constant (recomputed where
    /// used, and folded into addresses): the pinned value and the constant.
    lea: Vec<Option<(Reg, i64)>>,
    /// The explicit stack slots' offsets above the stack pointer, and the
    /// bytes they take.
    slot_offs: Vec<i32>,
    explicit: u32,
    /// The values in registers or slots, in no order.
    live: Vec<Value>,
    /// Each tracked value's index in `live`.
    live_pos: Vec<u32>,
    /// The block being generated.
    cur: Block,
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
    /// Each merge block's entry state, once fixed (by the first edge into
    /// it, or by the block itself when no edge came first): where each
    /// value live into it is, and whether its slot holds it. Every other
    /// edge into the block moves values to match.
    canon: Vec<Option<Vec<(Value, Reg, bool)>>>,
    done: Vec<bool>,
    stubs: Vec<(Label, Vec<Mv>, Label)>,
    epi: Label,
    // The block being generated.
    arg_next: Vec<u32>,
    def_next: Vec<u32>,
    nu: Vec<u32>,
    nu_stamp: Vec<u32>,
    stamp: u32,
    /// Instructions an earlier one generated with it (see
    /// [`Gen::add_in_place`]): their uses are taken, nothing emitted.
    skip: u8,
    calls_after: Vec<u32>,
    pending: Option<Pending>,
    /// Scratch buffers, kept to save allocations.
    tmp: Vec<Value>,
    /// Emptied snapshots' vectors, for the next ones.
    snap_pool: Vec<Vec<(Value, Reg, bool)>>,
    /// [`Self::moves`]' scratch: the slots written, and the register moves
    /// still to make.
    mv_written: Vec<i32>,
    mv_pending: Vec<(Reg, Reg)>,
    tmp2: Vec<Value>,
    srcs: Vec<Src>,
    /// Each signature's argument locations and stack slots, once computed.
    sig_locs: Vec<Option<(Vec<Reg>, u32)>>,
}

/// Whether to print each compile's phase times (`WPCG_TIME`, a tuning
/// aid).
fn timing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WPCG_TIME").is_some())
}

pub(crate) fn compile<A: Asm>(f: &mut Function) -> Result<crate::Code, Error> {
    if f.layout.is_empty() {
        return Err(Error::Unsupported("an empty function"));
    }
    crate::simplify::simplify(f);
    let f = &*f;
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
        reg_of: vec![NOREG; nv],
        home_ok: vec![false; nv],
        next: vec![NONE; nv],
        slot_of: vec![NONE; nv],
        tracked: vec![false; nv],
        pin: vec![NOREG; nv],
        lea: vec![None; nv],
        slot_offs: Vec::new(),
        explicit: 0,
        live: Vec::new(),
        live_pos: vec![0; nv],
        cur: Block(0),
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
        canon: vec![None; nb],
        done: vec![false; nb],
        stubs: Vec::new(),
        epi,
        arg_next: Vec::new(),
        def_next: Vec::new(),
        nu: vec![NONE; nv],
        nu_stamp: vec![u32::MAX; nv],
        stamp: 0,
        skip: 0,
        calls_after: Vec::new(),
        pending: None,
        tmp: Vec::new(),
        snap_pool: Vec::new(),
        mv_written: Vec::new(),
        mv_pending: Vec::new(),
        tmp2: Vec::new(),
        srcs: Vec::new(),
        sig_locs: vec![None; f.dfg.sigs.len()],
        an,
    };
    g.out = g.outgoing_area()?;
    g.layout_slots();
    g.choose_pins();
    g.find_leas();
    g.entry()?;
    for i in 0..g.an.order.len() {
        let b = g.an.order[i];
        g.block(b, i)?;
    }
    let t_gen = t0.elapsed();
    if timing() {
        eprintln!(
            "wpcg: {} insts {} blocks {} globals: analyze {:?} gen {:?}",
            f.dfg.insts.len(),
            f.dfg.blocks.len(),
            g.an.globals.len(),
            t_an,
            t_gen.saturating_sub(t_an)
        );
    }
    g.a.bind(g.epi);
    let frame = Frame {
        saved: g.used & g.callee_saved,
        slots: g.explicit + g.nslots * 8,
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
                let sig = match self.f.dfg.insts[inst.index()].op {
                    Op::CallIndirect(sig) => Some(sig),
                    Op::Call(fr) => Some(self.f.dfg.funcs[fr.index()].sig),
                    _ => None,
                };
                if let Some(sig) = sig {
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

    /// Place the explicit stack slots above the outgoing area.
    fn layout_slots(&mut self) {
        let mut at = self.out;
        for s in &self.f.dfg.stack_slots {
            let align = 1u32 << s.align_shift.min(4);
            at = at.next_multiple_of(align.max(8));
            self.slot_offs.push(at as i32);
            at += s.size;
        }
        self.explicit = at.next_multiple_of(8) - self.out;
    }

    /// Find the values that are a pinned value plus a constant.
    fn find_leas(&mut self) {
        let f = self.f;
        for &b in &self.an.order {
            for &inst in &f.dfg.blocks[b.index()].insts {
                let d = &f.dfg.insts[inst.index()];
                if let Op::StackAddr(slot, off) = d.op {
                    let v = f.dfg.slice(d.results)[0];
                    if self.pin[v.index()] == NOREG {
                        let at = i64::from(self.slot_offs[slot.index()]) + i64::from(off);
                        self.lea[v.index()] = Some((A::SP, at));
                        self.an.remat[v.index()] = true;
                    }
                    continue;
                }
                let Op::AluImm(Alu::Add, c) = d.op else {
                    continue;
                };
                let v = f.dfg.slice(d.results)[0];
                if d.ty.bits() != 64 || self.pin[v.index()] != NOREG {
                    continue;
                }
                let x = f.dfg.slice(d.args)[0];
                let base = if self.pin[x.index()] != NOREG {
                    Some((self.pin[x.index()], 0))
                } else {
                    self.lea[x.index()]
                };
                if let Some((p, o)) = base {
                    let off = o.wrapping_add(c);
                    if i32::try_from(off).is_ok() {
                        self.lea[v.index()] = Some((p, off));
                        self.an.remat[v.index()] = true;
                    }
                }
            }
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
        let dsts = self.param_regs(entry, 0);
        let params = self.f.dfg.blocks[entry.index()].params.clone();
        let mut moves = Vec::with_capacity(dsts.len());
        for ((&d, &s), &p) in dsts.iter().zip(&locs).zip(&params) {
            let dst = if d == NOREG {
                Dst::Sp(self.slot(p))
            } else {
                Dst::R(d)
            };
            moves.push((dst, Src::R(s)));
        }
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
            self.live_pos[i] = self.live.len() as u32;
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
        let p = self.live_pos[i] as usize;
        debug_assert_eq!(self.live[p], v);
        self.live.swap_remove(p);
        if let Some(&moved) = self.live.get(p) {
            self.live_pos[moved.index()] = p as u32;
        }
        // A value of one block gives its slot back.
        if self.recyclable(v) && self.slot_of[i] != NONE {
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

    fn snapshot(&mut self) -> Snap {
        let mut vals = self.snap_pool.pop().unwrap_or_default();
        vals.extend(
            self.live
                .iter()
                .map(|&v| (v, self.reg_of[v.index()], self.home_ok[v.index()])),
        );
        Snap { vals }
    }

    fn slot(&mut self, v: Value) -> i32 {
        let i = v.index();
        if self.slot_of[i] == NONE {
            self.slot_of[i] = if self.recyclable(v) {
                self.free_slots.pop()
            } else {
                None
            }
            .unwrap_or_else(|| {
                self.nslots += 1;
                self.nslots - 1
            });
        }
        (self.out + self.explicit + 8 * self.slot_of[i]) as i32
    }

    /// Whether `v`'s slot goes back for reuse when it dies: a value of one
    /// block that an instruction defines (a parameter's slot is where
    /// edges into its block write it).
    fn recyclable(&self, v: Value) -> bool {
        self.an.gidx[v.index()] == u32::MAX
            && matches!(self.f.dfg.values[v.index()].def, ValueDef::Result(..))
    }

    fn is_remat(&self, v: Value) -> bool {
        self.an.remat[v.index()]
    }

    fn const_of(&self, v: Value) -> Option<Src> {
        if let Some((base, off)) = self.lea[v.index()] {
            return Some(Src::Lea(base, off));
        }
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
        let avail =
            |r: Reg| self.holds[r as usize] == NONE && (self.pinned | self.locked) & bit(r) == 0;
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
        let r = self.alloc_any(float, prefer_saved)?;
        // (Temporaries too: a callee-saved register written is saved.)
        self.used |= bit(r);
        Ok(r)
    }

    fn alloc_any(&mut self, float: bool, prefer_saved: bool) -> Result<Reg, Error> {
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
                Src::Lea(base, off) => self.lea_into(r, base, off),
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

    fn lea_into(&mut self, r: Reg, base: Reg, off: i64) {
        if off == 0 && base != A::SP {
            self.a.mov(r, base);
        } else {
            self.a
                .alu(Alu::Add, crate::ir::types::I64, r, base, Opnd::I(off));
        }
    }

    /// The register and offset that address `[v + off]`: a pinned register
    /// with the constant folded in, for a pinned value plus a constant.
    fn address(&mut self, v: Value, off: i32) -> Result<(Reg, i32), Error> {
        if !self.in_reg(v) {
            if let Some((base, o)) = self.lea[v.index()] {
                if let Ok(o) = i32::try_from(o + i64::from(off)) {
                    return Ok((base, o));
                }
            }
        }
        Ok((self.use_reg(v)?, off))
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
        let mut dead = std::mem::take(&mut self.tmp);
        dead.clear();
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
        for &v in &dead {
            if self.tracked[v.index()] && self.next[v.index()] == NONE {
                self.untrack(v);
            }
        }
        self.tmp = dead;
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
                    && (A::INT_REGS.contains(&p) || A::FLOAT_REGS.contains(&p))
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

    /// The registers `b`'s parameters arrive in (chosen, the first time,
    /// from those not in `avoid`).
    fn param_regs(&mut self, b: Block, avoid: u64) -> Vec<Reg> {
        if !self.params[b.index()].is_empty() || self.f.dfg.blocks[b.index()].params.is_empty() {
            return self.params[b.index()].clone();
        }
        let mut taken = self.pinned | avoid;
        // The entry's parameters stay where the caller put them.
        let entry_locs = if b == self.an.order[0] {
            let tys: Vec<Type> = self
                .f
                .signature
                .params
                .iter()
                .map(|p| p.value_type)
                .collect();
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
                        self.used |= bit(r);
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
                    .or_else(|| pool.iter().copied().find(|&r| taken & bit(r) == 0));
                // (Past the registers, a parameter arrives in its slot.)
                let Some(r) = r else { return NOREG };
                taken |= bit(r);
                // (Edges write it even where the block never reads it: a
                // callee-saved one must be saved.)
                self.used |= bit(r);
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
        let mut occ = std::mem::take(&mut self.tmp2);
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
            if matches!(
                f.dfg.insts[inst.index()].op,
                Op::CallIndirect(_) | Op::Call(_)
            ) {
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
        self.tmp2 = occ;
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
        self.cur = b;
        self.next_uses(b);
        // The state at the start.
        if let Some(snap) = self.snaps[b.index()].take() {
            self.reset();
            let mut vals = snap.vals;
            for &(v, r, home) in &vals {
                let n = self.first_use(v);
                if n != NONE || self.pin[v.index()] != NOREG {
                    self.track(v, r, home, n);
                }
            }
            vals.clear();
            self.snap_pool.push(vals);
        } else {
            debug_assert!(!self.an.inherits(b), "an inheriting block without a state");
            self.reset();
            // The state the first edge fixed, or else every value in its
            // slot (and the edges still to come store them).
            let canon = match self.canon[b.index()].take() {
                Some(c) => c,
                None => self
                    .live_into(b)
                    .into_iter()
                    .map(|v| (v, NOREG, true))
                    .collect(),
            };
            for &(v, r, home) in &canon {
                let n = self.first_use(v);
                self.track(v, r, home, n);
            }
            self.canon[b.index()] = Some(canon);
            let regs = self.param_regs(b, 0);
            let f = self.f;
            let params = &f.dfg.blocks[b.index()].params;
            for (&p, &r) in params.iter().zip(&regs) {
                let n = self.first_use(p);
                if n != NONE || self.pin[p.index()] != NOREG {
                    // (A parameter with no register is in its slot.)
                    self.track(p, r, r == NOREG, n);
                }
            }
            for v in self.pinned_into(b) {
                let n = self.first_use(v);
                self.track(v, self.pin[v.index()], false, n);
            }
        }
        self.done[b.index()] = true;
        self.a.bind(self.labels[b.index()]);
        let f = self.f;
        let insts = &f.dfg.blocks[b.index()].insts;
        let mut at = 0;
        let mut rat = 0;
        for (k, &inst) in insts.iter().enumerate() {
            self.locked = 0;
            let nres = f.dfg.results(inst).len();
            if self.skip > 0 {
                self.skip -= 1;
                self.consume(inst, &mut at);
            } else {
                self.inst(inst, k, pos, &mut at, rat)?;
            }
            rat += nres;
        }
        self.locked = 0;
        Ok(())
    }

    // ---- instructions ----

    fn res(&self, inst: Inst, j: usize) -> Value {
        self.f.dfg.results(inst)[j]
    }

    /// Whether `v`'s only use is as the condition of the `brif` after the
    /// current block's instruction `k`, past any constants (which emit
    /// nothing).
    fn fuses(&self, v: Value, k: usize) -> bool {
        if self.an.uses[v.index()] != 1 {
            return false;
        }
        let f = self.f;
        let insts = &f.dfg.blocks[self.cur.index()].insts;
        for &i in insts.get(k + 1..).unwrap_or(&[]) {
            let d = &f.dfg.insts[i.index()];
            match d.op {
                Op::Iconst(_) | Op::Fconst(_) => {}
                Op::Brif(..) => return f.dfg.slice(d.args)[0] == v,
                _ => return false,
            }
        }
        false
    }

    /// The immediate of an in-place add the load at `k` starts: `load`,
    /// `iadd_imm` of it, then `store` of the sum back where it came from,
    /// each value used once (a reference count's increment).
    fn add_in_place(&self, k: usize) -> Option<i64> {
        let f = self.f;
        let insts = &f.dfg.blocks[self.cur.index()].insts;
        let (&ld, &add, &st) = (insts.get(k)?, insts.get(k + 1)?, insts.get(k + 2)?);
        let (dl, da, ds) = (
            &f.dfg.insts[ld.index()],
            &f.dfg.insts[add.index()],
            &f.dfg.insts[st.index()],
        );
        let Op::Load { bytes: 8, off, .. } = dl.op else {
            return None;
        };
        let Op::AluImm(Alu::Add, c) = da.op else {
            return None;
        };
        let Op::Store { bytes: 8, off: so } = ds.op else {
            return None;
        };
        let v = f.dfg.results(ld)[0];
        let sum = f.dfg.results(add)[0];
        let (la, aa, sa) = (f.dfg.slice(dl.args), f.dfg.slice(da.args), f.dfg.slice(ds.args));
        (so == off
            && i32::try_from(c).is_ok()
            && aa[0] == v
            && sa[0] == sum
            && sa[1] == la[0]
            && self.an.uses[v.index()] == 1
            && self.an.uses[sum.index()] == 1)
            .then_some(c)
    }

    #[allow(clippy::too_many_lines)]
    fn inst(
        &mut self,
        inst: Inst,
        k: usize,
        pos: usize,
        at: &mut usize,
        rat: usize,
    ) -> Result<(), Error> {
        let f = self.f;
        let d = &f.dfg.insts[inst.index()];
        let args = f.dfg.slice(d.args);
        let dn = |g: &Self, j: usize| g.def_next[rat + j];
        match &d.op {
            Op::Iconst(_) | Op::Fconst(_) | Op::StackAddr(..) => {
                // Materialized where used.
                self.consume(inst, at);
            }
            Op::Load { bytes, ext, off } => {
                // `[a] += c`, in place.
                if A::ADD_MEM {
                    if let Some(c) = self.add_in_place(k) {
                        let (base, off) = self.address(args[0], *off)?;
                        self.a.add_mem(base, off, c);
                        self.consume(inst, at);
                        self.skip = 2;
                        return Ok(());
                    }
                }
                // A load from a constant address in reach (one of this
                // program's globals): addressed from the code itself.
                let abs = self
                    .imm_of(args[0])
                    .filter(|_| !self.in_reg(args[0]))
                    .map(|c| c.wrapping_add(i64::from(*off)) as u64)
                    .filter(|&a| self.a.reaches(a));
                if let Some(addr) = abs {
                    self.consume(inst, at);
                    let v = self.res(inst, 0);
                    let r = self.def(v, k, dn(self, 0), None)?;
                    self.a.load_abs(*bytes, *ext, d.ty, r, addr);
                    self.unused(v, dn(self, 0));
                    return Ok(());
                }
                let (base, off) = self.address(args[0], *off)?;
                self.consume(inst, at);
                let v = self.res(inst, 0);
                let r = self.def(v, k, dn(self, 0), Some(base))?;
                self.a.load(*bytes, *ext, d.ty, r, base, off);
                self.unused(v, dn(self, 0));
            }
            Op::Store { bytes, off } => {
                let (base, off) = self.address(args[1], *off)?;
                let off = &off;
                let stored = matches!(
                    self.imm_of(args[0]),
                    Some(i) if !self.in_reg(args[0]) && self.a.store_imm(*bytes, i, base, *off)
                );
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
                self.compare(inst, k, at, rat, ty, c, a, b)?;
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
                self.compare(inst, k, at, rat, ty, Cond::from_int(*cc), a, b)?;
            }
            Op::Fcmp(cc) => {
                let ty = self.ty(args[0]);
                let a = self.use_reg(args[0])?;
                let b = self.use_reg(args[1])?;
                let v = self.res(inst, 0);
                if self.fuses(v, k) {
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
                self.a.extend(matches!(d.op, Op::Sextend), from, d.ty, r, a);
                self.unused(v, dn(self, 0));
            }
            Op::Overflow(op) => {
                let ty = d.ty;
                let a = self.use_reg(args[0])?;
                let b = self.use_reg(args[1])?;
                self.consume(inst, at);
                let (v, flag) = (self.res(inst, 0), self.res(inst, 1));
                let r = self.def(v, k, dn(self, 0), Some(a))?;
                if self.fuses(flag, k) {
                    let c = self.a.overflow(*op, ty, r, a, b);
                    self.pending = Some(Pending::Int(c, flag));
                } else {
                    let fr = self.def(flag, k, dn(self, 1), None)?;
                    let c = self.a.overflow(*op, ty, r, a, b);
                    self.a.setcc(c, fr);
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
            Op::CallIndirect(sig) => self.call(inst, k, at, rat, *sig, None)?,
            Op::Call(fr) => {
                let data = f.dfg.funcs[fr.index()];
                self.call(inst, k, at, rat, data.sig, Some(data.target))?;
            }
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
                self.branch(
                    pos,
                    c,
                    (t.block, f.dfg.slice(t.args)),
                    (e.block, f.dfg.slice(e.args)),
                )?;
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
                        *self
                            .abi
                            .float_rets
                            .get(nf - 1)
                            .ok_or(Error::Unsupported("returns"))?
                    } else {
                        ni += 1;
                        *self
                            .abi
                            .int_rets
                            .get(ni - 1)
                            .ok_or(Error::Unsupported("returns"))?
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
        ty: Type,
        c: Cond,
        a: Reg,
        b: Opnd,
    ) -> Result<(), Error> {
        let v = self.res(inst, 0);
        if self.fuses(v, k) {
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
        sig_ref: crate::ir::SigRef,
        direct: Option<crate::ir::FuncTarget>,
    ) -> Result<(), Error> {
        let f = self.f;
        let sig = &f.dfg.sigs[sig_ref.index()];
        let abi = A::abi(sig.call_conv).ok_or(Error::Unsupported("a calling convention"))?;
        let args = f.dfg.args(inst);
        let si = sig_ref.index();
        if self.sig_locs[si].is_none() {
            let tys: Vec<Type> = sig.params.iter().map(|p| p.value_type).collect();
            self.sig_locs[si] = Some(arg_locs(abi, &tys));
        }
        // The operands' locations, before anything moves.
        let mut srcs = std::mem::take(&mut self.srcs);
        srcs.clear();
        // (The callee first: an address, or nothing for the function
        // itself.)
        match direct {
            Some(crate::ir::FuncTarget::Addr(a)) => srcs.push(Src::I(a as i64)),
            Some(crate::ir::FuncTarget::SelfFn) => srcs.push(Src::I(0)),
            None => {}
        }
        for &v in args {
            srcs.push(self.src(v)?);
        }
        // A callee at a constant address in reach is called directly, its
        // address not loaded.
        let near = match (direct, srcs[0]) {
            (Some(crate::ir::FuncTarget::SelfFn), _) => None,
            (_, Src::I(a)) => self.a.reaches(a as u64).then_some(a as u64),
            _ => None,
        };
        // Take the uses: what dies here needn't survive the call.
        let mut dead = std::mem::take(&mut self.tmp);
        dead.clear();
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
        let mut survivors = std::mem::take(&mut self.tmp2);
        survivors.clear();
        survivors.extend(self.live.iter().copied().filter(|&v| {
            let r = self.reg_of[v.index()];
            r != NOREG
                && abi.clobbers & bit(r) != 0
                && self.pin[v.index()] == NOREG
                && self.next[v.index()] != NONE
        }));
        let mut stored: Vec<Value> = Vec::new();
        for &v in &survivors {
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
        self.tmp2 = survivors;
        // The arguments, and the callee.
        let mut moves: Vec<Mv> = Vec::with_capacity(srcs.len());
        if direct != Some(crate::ir::FuncTarget::SelfFn) && near.is_none() {
            moves.push((Dst::R(A::CALL_REG), srcs[0]));
        }
        let locs = &self.sig_locs[si].as_ref().expect("computed").0;
        for (loc, src) in locs.iter().zip(&srcs[1..]) {
            let dst = match *loc {
                l if l < 64 => Dst::R(l),
                l => Dst::Sp((abi.shadow + 8 * u32::from(l - 64)) as i32),
            };
            moves.push((dst, *src));
        }
        self.srcs = srcs;
        self.moves(moves);
        // The clobbered registers' values are gone (saved, or dead).
        for v in stored {
            self.drop_reg(v);
        }
        for &v in &dead {
            if self.tracked[v.index()] && self.next[v.index()] == NONE {
                self.untrack(v);
            }
        }
        self.tmp = dead;
        let mut clobbered = abi.clobbers;
        while clobbered != 0 {
            let r = clobbered.trailing_zeros() as usize;
            clobbered &= clobbered - 1;
            if self.holds[r] != NONE {
                let v = Value(self.holds[r]);
                if self.pin[v.index()] == NOREG {
                    self.untrack(v);
                }
            }
        }
        if direct == Some(crate::ir::FuncTarget::SelfFn) {
            self.a.call_self();
        } else if let Some(target) = near {
            self.a.call_abs(target);
        } else {
            self.a.call(A::CALL_REG);
        }
        let results = f.dfg.results(inst);
        let (mut ni, mut nf) = (0, 0);
        for (j, &v) in results.iter().enumerate() {
            let r = if self.ty(v).is_float() {
                nf += 1;
                *abi.float_rets
                    .get(nf - 1)
                    .ok_or(Error::Unsupported("returns"))?
            } else {
                ni += 1;
                *abi.int_rets
                    .get(ni - 1)
                    .ok_or(Error::Unsupported("returns"))?
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
        if self.canon[t.index()].is_none() && !self.done[t.index()] {
            // The first edge in: the block starts in this edge's state.
            let live = self.live_into(t);
            let mut avoid = 0;
            let mut canon = Vec::with_capacity(live.len());
            for v in live {
                if !self.tracked[v.index()] {
                    return Err(Error::Internal("a live value with no location"));
                }
                let r = self.reg_of[v.index()];
                if r != NOREG {
                    avoid |= bit(r);
                }
                canon.push((v, r, self.home_ok[v.index()]));
            }
            // Room for the parameters: values beyond it start in their slots.
            for float in [false, true] {
                let pool = if float { A::FLOAT_REGS } else { A::INT_REGS };
                let need = self.f.dfg.blocks[t.index()]
                    .params
                    .iter()
                    .filter(|&&p| self.ty(p).is_float() == float && self.pin[p.index()] == NOREG)
                    .count();
                let mut free = pool
                    .iter()
                    .filter(|&&r| (avoid | self.pinned) & bit(r) == 0)
                    .count();
                for e in canon.iter_mut() {
                    if free >= need {
                        break;
                    }
                    let (v, r, home) = *e;
                    if r == NOREG || is_float(r) != float {
                        continue;
                    }
                    if !home {
                        let off = self.slot(v);
                        moves.push((Dst::Sp(off), Src::R(r)));
                    }
                    *e = (v, NOREG, true);
                    avoid &= !bit(r);
                    free += 1;
                }
            }
            self.canon[t.index()] = Some(canon);
            let regs = self.param_regs(t, avoid);
            self.param_moves(t, &regs, args, &mut moves)?;
            return Ok(moves);
        }
        let canon = self.canon[t.index()].take().unwrap_or_default();
        for &(v, r, home) in &canon {
            let i = v.index();
            if home && !self.home_ok[i] {
                let cur = self.reg_of[i];
                if !self.tracked[i] || cur == NOREG {
                    return Err(Error::Internal("a live value with no location"));
                }
                let off = self.slot(v);
                moves.push((Dst::Sp(off), Src::R(cur)));
            }
            if r != NOREG {
                let src = self.src(v)?;
                if src != Src::R(r) {
                    moves.push((Dst::R(r), src));
                }
            }
        }
        self.canon[t.index()] = Some(canon);
        let regs = self.param_regs(t, 0);
        self.param_moves(t, &regs, args, &mut moves)?;
        Ok(moves)
    }

    /// The moves of an edge's arguments to `t`'s parameters: their
    /// registers, or their slots past the registers.
    fn param_moves(
        &mut self,
        t: Block,
        regs: &[Reg],
        args: &[Value],
        moves: &mut Vec<Mv>,
    ) -> Result<(), Error> {
        let f = self.f;
        let params = &f.dfg.blocks[t.index()].params;
        for ((&v, &r), &p) in args.iter().zip(regs).zip(params) {
            let src = self.src(v)?;
            if r == NOREG {
                let off = self.slot(p);
                if src != Src::Slot(off, self.ty(p).is_float()) {
                    moves.push((Dst::Sp(off), src));
                }
            } else if src != Src::R(r) {
                moves.push((Dst::R(r), src));
            }
        }
        Ok(())
    }

    /// The values live into `t` that a state keeps: neither pinned nor
    /// recomputed where used.
    fn live_into(&self, t: Block) -> Vec<Value> {
        let mut out = Vec::new();
        for (w, &word) in self.an.live_in(t).iter().enumerate() {
            let mut bits = word;
            while bits != 0 {
                let i = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let v = self.an.globals[w * 64 + i];
                if !self.is_remat(v) && self.pin[v.index()] == NOREG {
                    out.push(v);
                }
            }
        }
        out
    }

    /// The pinned values live into `t`.
    fn pinned_into(&self, t: Block) -> Vec<Value> {
        let mut out = Vec::new();
        for (w, &word) in self.an.live_in(t).iter().enumerate() {
            let mut bits = word;
            while bits != 0 {
                let i = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let v = self.an.globals[w * 64 + i];
                if self.pin[v.index()] != NOREG {
                    out.push(v);
                }
            }
        }
        out
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
        let invertible = true;
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
        // A slot this assignment writes and also reads (parameters in their
        // slots, swapped by a loop's back edge): read through a copy, made
        // before anything is written.
        let mut written = std::mem::take(&mut self.mv_written);
        written.clear();
        written.extend(moves.iter().filter_map(|&(d, _)| match d {
            Dst::Sp(off) => Some(off),
            Dst::R(_) => None,
        }));
        if !written.is_empty() {
            for m in &mut moves {
                if let Src::Slot(off, float) = m.1 {
                    if written.contains(&off) {
                        self.nslots += 1;
                        let t = (self.out + self.explicit + 8 * (self.nslots - 1)) as i32;
                        self.a.ld_sp(A::FSCRATCH, off);
                        self.a.st_sp(A::FSCRATCH, t);
                        m.1 = Src::Slot(t, float);
                    }
                }
            }
        }
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
                Src::Lea(base, o) => {
                    self.lea_into(A::SCRATCH, base, o);
                    self.a.st_sp(A::SCRATCH, off);
                }
            }
        }
        // Register to register, in an order that reads each source before
        // it's overwritten, cycles broken by swaps.
        self.mv_written = written;
        let mut pending = std::mem::take(&mut self.mv_pending);
        pending.clear();
        pending.extend(moves.iter().filter_map(|&(d, s)| match (d, s) {
            (Dst::R(d), Src::R(s)) => Some((d, s)),
            _ => None,
        }));
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
        self.mv_pending = pending;
        // Then what comes from memory or immediates.
        for &(d, s) in &moves {
            let Dst::R(r) = d else { continue };
            match s {
                Src::R(_) => {}
                Src::Slot(off, _) => self.a.ld_sp(r, off),
                Src::I(i) => self.a.imm(r, i),
                Src::F(bits, ty) => self.a.fimm(r, bits, ty),
                Src::Lea(base, o) => self.lea_into(r, base, o),
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
