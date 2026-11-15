//! Cheap rewrites before code generation: identities become their
//! operands, constants move to the immediate side of commutative
//! operations, jumps through empty blocks go straight to their targets,
//! and instructions nothing uses go away.

use crate::ir::{Alu, Block, Function, Op, Value, ValueDef};

fn const_int(f: &Function, v: Value) -> Option<i64> {
    match f.dfg.values[v.index()].def {
        ValueDef::Result(inst, 0) => match f.dfg.insts[inst.index()].op {
            Op::Iconst(i) => Some(i),
            _ => None,
        },
        _ => None,
    }
}

/// Whether `x op imm` is `x` (all ones is `-1` in the canonical form).
fn identity(op: Alu, imm: i64) -> bool {
    match op {
        Alu::Add | Alu::Sub | Alu::Or | Alu::Xor | Alu::Shl | Alu::Ushr | Alu::Sshr => imm == 0,
        Alu::Mul => imm == 1,
        Alu::And => imm == -1,
    }
}

fn pure(op: &Op) -> bool {
    !matches!(
        op,
        Op::Store { .. }
            | Op::CallIndirect(_)
            | Op::Call(_)
            | Op::Jump(_)
            | Op::Brif(..)
            | Op::BrTable(_)
            | Op::Return
            // (Division by zero traps on some targets.)
            | Op::Div(_)
    )
}

pub(crate) fn simplify(f: &mut Function) {
    let nv = f.dfg.values.len();
    let mut alias: Vec<Value> = (0..nv as u32).map(Value).collect();
    let resolve = |alias: &[Value], mut v: Value| {
        while alias[v.index()] != v {
            v = alias[v.index()];
        }
        v
    };
    crate::fold::fold(f, &mut alias);
    let nv = f.dfg.values.len();
    // Identities, and constants to the right.
    for bi in 0..f.layout.len() {
        let b = f.layout[bi];
        for ii in 0..f.dfg.blocks[b.index()].insts.len() {
            let inst = f.dfg.blocks[b.index()].insts[ii];
            let d = &f.dfg.insts[inst.index()];
            let (a0, a1) = (d.args.0 as usize, d.args.1 as usize);
            match d.op {
                Op::AluImm(op, imm) if identity(op, imm) => {
                    let res = f.dfg.pool[d.results.0 as usize];
                    let x = resolve(&alias, f.dfg.pool[a0]);
                    alias[res.index()] = x;
                }
                Op::Alu(op) if a1 - a0 == 2 => {
                    let (x, y) = (f.dfg.pool[a0], f.dfg.pool[a0 + 1]);
                    let commutes =
                        matches!(op, Alu::Add | Alu::Mul | Alu::And | Alu::Or | Alu::Xor);
                    if commutes && const_int(f, x).is_some() && const_int(f, y).is_none() {
                        f.dfg.pool.swap(a0, a0 + 1);
                    }
                    let (x, y) = (f.dfg.pool[a0], f.dfg.pool[a0 + 1]);
                    if const_int(f, y).is_some_and(|i| identity(op, i)) {
                        let res = f.dfg.pool[d.results.0 as usize];
                        alias[res.index()] = resolve(&alias, x);
                    }
                }
                _ => {}
            }
        }
    }
    // Jumps through empty blocks (by the blocks' shapes alone).
    let nb = f.dfg.blocks.len();
    let entry = f.layout[0];
    let mut fwd: Vec<Block> = (0..nb as u32).map(Block).collect();
    for &b in &f.layout {
        let bd = &f.dfg.blocks[b.index()];
        if b == entry || !bd.params.is_empty() || bd.insts.len() != 1 {
            continue;
        }
        if let Op::Jump(e) = &f.dfg.insts[bd.insts[0].index()].op {
            if e.args.0 == e.args.1 && e.block != b {
                fwd[b.index()] = e.block;
            }
        }
    }
    let target = |fwd: &[Block], mut b: Block| {
        // (Bounded: a cycle of empty blocks keeps its first.)
        for _ in 0..8 {
            let n = fwd[b.index()];
            if n == b {
                break;
            }
            b = n;
        }
        b
    };
    // One pass: every operand and branch argument through the aliases,
    // branches past empty blocks, the uses counted, and the instructions
    // that might be unused noted.
    let mut uses = vec![0u32; nv];
    let mut work: Vec<crate::ir::Inst> = Vec::new();
    for_each_inst(f, |f, inst| {
        let d = &f.dfg.insts[inst.index()];
        let none = (0, 0);
        let ranges = match &d.op {
            Op::Jump(e) => [d.args, e.args, none],
            Op::Brif(t, e) => [d.args, t.args, e.args],
            _ => [d.args, none, none],
        };
        let candidate = pure(&d.op) && d.results.1 > d.results.0;
        for (s, e) in ranges {
            for slot in &mut f.dfg.pool[s as usize..e as usize] {
                let v = resolve(&alias, *slot);
                *slot = v;
                uses[v.index()] += 1;
            }
        }
        match &mut f.dfg.insts[inst.index()].op {
            Op::Jump(e) => e.block = target(&fwd, e.block),
            Op::Brif(t, e) => {
                t.block = target(&fwd, t.block);
                e.block = target(&fwd, e.block);
            }
            Op::BrTable(jt) => {
                let jt = *jt;
                let t = &mut f.dfg.tables[jt.index()];
                t.default.block = target(&fwd, t.default.block);
                for v in t.default.args.iter_mut() {
                    *v = resolve(&alias, *v);
                    uses[v.index()] += 1;
                }
                for c in &mut t.table {
                    c.block = target(&fwd, c.block);
                    for v in c.args.iter_mut() {
                        *v = resolve(&alias, *v);
                        uses[v.index()] += 1;
                    }
                }
            }
            _ => {}
        }
        if candidate {
            work.push(inst);
        }
    });
    // Instructions nothing uses.
    let mut dead = vec![false; f.dfg.insts.len()];
    work.retain(|&i| {
        let d = &f.dfg.insts[i.index()];
        f.dfg.slice(d.results).iter().all(|r| uses[r.index()] == 0)
    });
    let work_found = !work.is_empty();
    while let Some(inst) = work.pop() {
        if dead[inst.index()] {
            continue;
        }
        dead[inst.index()] = true;
        let (s, e) = f.dfg.insts[inst.index()].args;
        for i in s as usize..e as usize {
            let v = f.dfg.pool[i];
            uses[v.index()] -= 1;
            if uses[v.index()] == 0 {
                if let ValueDef::Result(def, _) = f.dfg.values[v.index()].def {
                    let d = &f.dfg.insts[def.index()];
                    if !dead[def.index()]
                        && pure(&d.op)
                        && f.dfg.slice(d.results).iter().all(|r| uses[r.index()] == 0)
                    {
                        work.push(def);
                    }
                }
            }
        }
    }
    if work_found {
        for bi in 0..f.layout.len() {
            let b = f.layout[bi];
            f.dfg.blocks[b.index()].insts.retain(|i| !dead[i.index()]);
        }
    }
}

/// Run `g` over every instruction of `f`'s layout, in order, with `f`
/// itself (the instruction lists stay as they are).
fn for_each_inst(f: &mut Function, mut g: impl FnMut(&mut Function, crate::ir::Inst)) {
    for bi in 0..f.layout.len() {
        let b = f.layout[bi];
        let mut ii = 0;
        while let Some(&inst) = f.dfg.blocks[b.index()].insts.get(ii) {
            g(f, inst);
            ii += 1;
        }
    }
}
