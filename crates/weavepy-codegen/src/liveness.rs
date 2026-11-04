//! The order blocks are emitted in, which values live across blocks, and
//! where.

use crate::ir::{Block, Function, Inst, Op, Value, ValueDef};

/// No block.
pub(crate) const NO_BLOCK: u32 = u32::MAX;

/// What code generation needs to know about a function before it starts.
pub(crate) struct Analysis {
    /// The reachable blocks in emission order: the hot ones in layout
    /// order, then the cold ones.
    pub order: Vec<Block>,
    /// Each instruction's block.
    pub inst_block: Vec<u32>,
    /// The block whose only edge enters each block, when a single-edge block
    /// takes its register state from it (see [`Analysis::inherits`]).
    pub single_pred: Vec<u32>,
    /// Each value's uses, over the whole function.
    pub uses: Vec<u32>,
    /// Each value's index among the values that live across blocks
    /// (`u32::MAX`: it lives in one block, or is rematerialized).
    pub gidx: Vec<u32>,
    pub globals: Vec<Value>,
    /// The values live into each block, a bitset over `globals`.
    pub live_in: Vec<u64>,
    pub words: usize,
    /// Values rematerialized where they're used, rather than kept.
    pub remat: Vec<bool>,
}

impl Analysis {
    pub(crate) fn live_in(&self, b: Block) -> &[u64] {
        let at = b.index() * self.words;
        &self.live_in[at..at + self.words]
    }

    /// Whether `b` starts from its only predecessor's register state.
    pub(crate) fn inherits(&self, b: Block) -> bool {
        self.single_pred[b.index()] != NO_BLOCK
    }
}

/// The blocks a terminator branches to.
pub(crate) fn successors(func: &Function, inst: Inst, mut f: impl FnMut(Block)) {
    match &func.dfg.insts[inst.index()].op {
        Op::Jump(e) => f(e.block),
        Op::Brif(t, e) => {
            f(t.block);
            f(e.block);
        }
        Op::BrTable(jt) => {
            let t = &func.dfg.tables[jt.index()];
            f(t.default.block);
            for c in &t.table {
                f(c.block);
            }
        }
        _ => {}
    }
}

/// Every value a branch passes to its targets.
fn branch_args(func: &Function, inst: Inst, mut f: impl FnMut(Value)) {
    match &func.dfg.insts[inst.index()].op {
        Op::Jump(e) => func.dfg.slice(e.args).iter().copied().for_each(&mut f),
        Op::Brif(t, e) => {
            func.dfg.slice(t.args).iter().copied().for_each(&mut f);
            func.dfg.slice(e.args).iter().copied().for_each(&mut f);
        }
        Op::BrTable(jt) => {
            let t = &func.dfg.tables[jt.index()];
            t.default.args.iter().copied().for_each(&mut f);
            for c in &t.table {
                c.args.iter().copied().for_each(&mut f);
            }
        }
        _ => {}
    }
}

/// Every value `inst` reads: its operands, then its branch arguments.
pub(crate) fn each_use(func: &Function, inst: Inst, mut f: impl FnMut(Value)) {
    func.dfg.args(inst).iter().copied().for_each(&mut f);
    branch_args(func, inst, f);
}

/// Whether `v` is a value worth recomputing at each use rather than
/// keeping: a constant, or a constant offset from a pinned value (see
/// [`Analysis`]; decided after pinning).
fn is_const(func: &Function, v: Value) -> bool {
    match func.dfg.values[v.index()].def {
        ValueDef::Result(inst, 0) => matches!(
            func.dfg.insts[inst.index()].op,
            Op::Iconst(_) | Op::Fconst(_) | Op::StackAddr(..)
        ),
        _ => false,
    }
}

pub(crate) fn analyze(func: &Function) -> Analysis {
    let dfg = &func.dfg;
    let nblocks = dfg.blocks.len();
    let entry = func.layout[0];

    // Reachability, from the entry.
    let mut reach = vec![false; nblocks];
    let mut work = vec![entry];
    reach[entry.index()] = true;
    while let Some(b) = work.pop() {
        if let Some(&last) = dfg.blocks[b.index()].insts.last() {
            successors(func, last, |s| {
                if !reach[s.index()] {
                    reach[s.index()] = true;
                    work.push(s);
                }
            });
        }
    }
    // Cold: what the builder marked, and exits: blocks that return, entered
    // only by one side of one conditional branch (a guard's deopt, an error
    // path), whose code would otherwise sit between a loop's hot blocks.
    let mut edges = vec![0u32; nblocks];
    let mut by_brif = vec![0u32; nblocks];
    for &b in &func.layout {
        if let Some(&last) = dfg.blocks[b.index()].insts.last() {
            let brif = matches!(dfg.insts[last.index()].op, Op::Brif(..));
            successors(func, last, |s| {
                edges[s.index()] += 1;
                by_brif[s.index()] += u32::from(brif);
            });
        }
    }
    let cold = |b: Block| {
        let bd = &dfg.blocks[b.index()];
        b != entry
            && (bd.cold
                || (edges[b.index()] == 1
                    && by_brif[b.index()] == 1
                    && bd.insts.len() > 1
                    && bd
                        .insts
                        .last()
                        .is_some_and(|l| matches!(dfg.insts[l.index()].op, Op::Return))))
    };
    let mut order: Vec<Block> = func
        .layout
        .iter()
        .copied()
        .filter(|&b| reach[b.index()] && !cold(b))
        .collect();
    order.extend(
        func.layout
            .iter()
            .copied()
            .filter(|&b| reach[b.index()] && cold(b)),
    );
    let mut pos = vec![NO_BLOCK; nblocks];
    for (i, b) in order.iter().enumerate() {
        pos[b.index()] = i as u32;
    }

    let mut inst_block = vec![NO_BLOCK; dfg.insts.len()];
    let mut preds = vec![0u32; nblocks];
    let mut last_pred = vec![NO_BLOCK; nblocks];
    let mut table_target = vec![false; nblocks];
    let mut uses = vec![0u32; dfg.values.len()];
    for &b in &order {
        for &inst in &dfg.blocks[b.index()].insts {
            inst_block[inst.index()] = b.0;
            each_use(func, inst, |v| uses[v.index()] += 1);
        }
        if let Some(&last) = dfg.blocks[b.index()].insts.last() {
            let table = matches!(dfg.insts[last.index()].op, Op::BrTable(_));
            successors(func, last, |s| {
                preds[s.index()] += 1;
                last_pred[s.index()] = b.0;
                table_target[s.index()] |= table;
            });
        }
    }
    // The function's entry is an edge of its own.
    preds[entry.index()] += 1;
    let single_pred: Vec<u32> = (0..nblocks)
        .map(|i| {
            let p = last_pred[i];
            let ok = preds[i] == 1
                && p != NO_BLOCK
                && !table_target[i]
                && dfg.blocks[i].params.is_empty()
                && pos[p as usize] < pos[i];
            if ok {
                p
            } else {
                NO_BLOCK
            }
        })
        .collect();

    let remat: Vec<bool> = (0..dfg.values.len())
        .map(|i| is_const(func, Value(i as u32)))
        .collect();

    // The values used outside the block that defines them.
    let def_block = |v: Value| match dfg.values[v.index()].def {
        ValueDef::Result(inst, _) => inst_block[inst.index()],
        ValueDef::Param(b, _) => b.0,
        ValueDef::Var => NO_BLOCK,
    };
    let mut gidx = vec![u32::MAX; dfg.values.len()];
    let mut globals = Vec::new();
    for &b in &order {
        for &inst in &dfg.blocks[b.index()].insts {
            each_use(func, inst, |v| {
                if !remat[v.index()] && def_block(v) != b.0 && gidx[v.index()] == u32::MAX {
                    gidx[v.index()] = globals.len() as u32;
                    globals.push(v);
                }
            });
        }
    }
    let words = globals.len().div_ceil(64).max(1);
    let mut live_in = vec![0u64; nblocks * words];
    // Each block's upward-exposed uses and its definitions, of the globals.
    let mut ue: Vec<Vec<u32>> = vec![Vec::new(); nblocks];
    let mut defs: Vec<Vec<u32>> = vec![Vec::new(); nblocks];
    for &v in &globals {
        let g = gidx[v.index()];
        let db = def_block(v);
        if db != NO_BLOCK {
            defs[db as usize].push(g);
        }
    }
    for &b in &order {
        let list = &mut ue[b.index()];
        for &inst in &dfg.blocks[b.index()].insts {
            each_use(func, inst, |v| {
                let g = gidx[v.index()];
                if g != u32::MAX && def_block(v) != b.0 {
                    list.push(g);
                }
            });
        }
        list.sort_unstable();
        list.dedup();
    }
    let mut out = vec![0u64; words];
    let mut changed = true;
    while changed {
        changed = false;
        for &b in order.iter().rev() {
            out.iter_mut().for_each(|w| *w = 0);
            if let Some(&last) = dfg.blocks[b.index()].insts.last() {
                successors(func, last, |s| {
                    let at = s.index() * words;
                    for (o, w) in out.iter_mut().zip(&live_in[at..at + words]) {
                        *o |= *w;
                    }
                });
            }
            for &g in &defs[b.index()] {
                out[g as usize / 64] &= !(1 << (g % 64));
            }
            for &g in &ue[b.index()] {
                out[g as usize / 64] |= 1 << (g % 64);
            }
            let at = b.index() * words;
            if live_in[at..at + words] != out[..] {
                live_in[at..at + words].copy_from_slice(&out);
                changed = true;
            }
        }
    }

    Analysis {
        order,
        inst_block,
        single_pred,
        uses,
        gidx,
        globals,
        live_in,
        words,
        remat,
    }
}
