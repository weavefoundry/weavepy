//! Variables to SSA values, once every block is known (Braun et al.'s
//! construction, run over sealed blocks): a read before its block defines
//! the variable takes its predecessor's value, or, where edges merge, a
//! new block parameter that each edge passes its value to.

use std::collections::HashMap;

use crate::ir::{
    Block, Edge, Function, Inst, InstData, JumpTable, Op, Type, Value, ValueDef, Variable,
};

/// An edge into a block: a jump's or a `brif`'s (its `then` or `else`),
/// or a jump table's entry (`u32::MAX`: its default).
#[derive(Copy, Clone, Debug)]
enum EdgeRef {
    Inst(Inst, bool),
    Table(JumpTable, u32),
}

struct Ssa<'a> {
    f: &'a mut Function,
    tys: &'a [Type],
    defs: &'a HashMap<(u32, u32), Value>,
    preds: Vec<Vec<(Block, EdgeRef)>>,
    entry: HashMap<(u32, u32), Value>,
    /// The block parameters made for merges, by block.
    phis: Vec<(Block, Value)>,
}

pub(crate) fn resolve(
    f: &mut Function,
    tys: &[Type],
    defs: &HashMap<(u32, u32), Value>,
    reads: &[(Value, Block, Variable)],
) {
    let nb = f.dfg.blocks.len();
    let mut preds: Vec<Vec<(Block, EdgeRef)>> = vec![Vec::new(); nb];
    for &b in &f.layout {
        let Some(&last) = f.dfg.blocks[b.index()].insts.last() else {
            continue;
        };
        match &f.dfg.insts[last.index()].op {
            Op::Jump(e) => preds[e.block.index()].push((b, EdgeRef::Inst(last, true))),
            Op::Brif(t, e) => {
                preds[t.block.index()].push((b, EdgeRef::Inst(last, true)));
                preds[e.block.index()].push((b, EdgeRef::Inst(last, false)));
            }
            Op::BrTable(jt) => {
                let t = &f.dfg.tables[jt.index()];
                preds[t.default.block.index()].push((b, EdgeRef::Table(*jt, u32::MAX)));
                for (i, c) in t.table.iter().enumerate() {
                    preds[c.block.index()].push((b, EdgeRef::Table(*jt, i as u32)));
                }
            }
            _ => {}
        }
    }
    let mut s = Ssa {
        f,
        tys,
        defs,
        preds,
        entry: HashMap::new(),
        phis: Vec::new(),
    };
    let mut alias: HashMap<Value, Value> = HashMap::new();
    for &(p, b, var) in reads {
        let v = s.read_entry(b, var);
        alias.insert(p, v);
    }
    s.remove_trivial_phis(&mut alias);
    let resolve = |mut v: Value| {
        for _ in 0..64 {
            match alias.get(&v) {
                Some(&n) if n != v => v = n,
                _ => break,
            }
        }
        v
    };
    let f = s.f;
    for x in &mut f.dfg.pool {
        *x = resolve(*x);
    }
    for t in &mut f.dfg.tables {
        for x in t.default.args.iter_mut() {
            *x = resolve(*x);
        }
        for c in &mut t.table {
            for x in c.args.iter_mut() {
                *x = resolve(*x);
            }
        }
    }
}

impl Ssa<'_> {
    /// The variable's value at the end of `b`.
    fn read_end(&mut self, b: Block, var: Variable) -> Value {
        match self.defs.get(&(b.0, var.0)) {
            Some(&v) => v,
            None => self.read_entry(b, var),
        }
    }

    /// The variable's value at the start of `b`.
    fn read_entry(&mut self, b: Block, var: Variable) -> Value {
        if let Some(&v) = self.entry.get(&(b.0, var.0)) {
            return v;
        }
        let preds = self.preds[b.index()].clone();
        let ty = self.tys[var.index()];
        if preds.is_empty() {
            // Read before any definition: zero, as Cranelift has it.
            let v = self.zero_at_entry(ty);
            self.entry.insert((b.0, var.0), v);
            return v;
        }
        if preds.len() == 1 && preds[0].0 != b {
            let v = self.read_end(preds[0].0, var);
            self.entry.insert((b.0, var.0), v);
            return v;
        }
        let n = self.f.dfg.blocks[b.index()].params.len() as u32;
        let phi = self.f.dfg.make_value(ty, ValueDef::Param(b, n));
        self.f.dfg.blocks[b.index()].params.push(phi);
        self.entry.insert((b.0, var.0), phi);
        self.phis.push((b, phi));
        for (pred, edge) in preds {
            let v = self.read_end(pred, var);
            self.append_arg(edge, v);
        }
        phi
    }

    /// Drop the merge parameters every edge passes the same value (or the
    /// parameter itself) to, the value taking their place, until none is
    /// left: a variable a loop never assigns has a parameter at the loop's
    /// head otherwise.
    fn remove_trivial_phis(&mut self, alias: &mut HashMap<Value, Value>) {
        let resolve = |alias: &HashMap<Value, Value>, mut v: Value| {
            for _ in 0..64 {
                match alias.get(&v) {
                    Some(&n) if n != v => v = n,
                    _ => break,
                }
            }
            v
        };
        let mut live: Vec<(Block, Value)> = std::mem::take(&mut self.phis);
        loop {
            let mut changed = false;
            let mut i = 0;
            while i < live.len() {
                let (b, phi) = live[i];
                let Some(at) = self.f.dfg.blocks[b.index()]
                    .params
                    .iter()
                    .position(|&p| p == phi)
                else {
                    live.swap_remove(i);
                    continue;
                };
                let mut same: Option<Value> = None;
                let mut trivial = true;
                for &(_, edge) in &self.preds[b.index()] {
                    let arg = resolve(alias, self.edge_arg(edge, at));
                    if arg == phi || Some(arg) == same {
                        continue;
                    }
                    if same.is_some() {
                        trivial = false;
                        break;
                    }
                    same = Some(arg);
                }
                if !trivial {
                    i += 1;
                    continue;
                }
                let to = match same {
                    Some(v) => v,
                    None => self.zero_at_entry(self.f.dfg.value_type(phi)),
                };
                alias.insert(phi, to);
                self.f.dfg.blocks[b.index()].params.remove(at);
                for (_, edge) in self.preds[b.index()].clone() {
                    self.remove_arg(edge, at);
                }
                live.swap_remove(i);
                changed = true;
            }
            if !changed {
                break;
            }
        }
    }

    fn edge_arg(&self, edge: EdgeRef, at: usize) -> Value {
        let dfg = &self.f.dfg;
        match edge {
            EdgeRef::Inst(inst, first) => match &dfg.insts[inst.index()].op {
                Op::Jump(e) => dfg.slice(e.args)[at],
                Op::Brif(t, _) if first => dfg.slice(t.args)[at],
                Op::Brif(_, e) => dfg.slice(e.args)[at],
                _ => unreachable!("an edge of a branch"),
            },
            EdgeRef::Table(jt, i) => {
                let t = &dfg.tables[jt.index()];
                if i == u32::MAX {
                    t.default.args[at]
                } else {
                    t.table[i as usize].args[at]
                }
            }
        }
    }

    fn remove_arg(&mut self, edge: EdgeRef, at: usize) {
        let dfg = &mut self.f.dfg;
        match edge {
            EdgeRef::Inst(inst, first) => {
                let shrink = |dfg: &mut crate::ir::DataFlowGraph, e: Edge| -> Edge {
                    let mut args = dfg.slice(e.args).to_vec();
                    args.remove(at);
                    Edge {
                        block: e.block,
                        args: dfg.intern(&args),
                    }
                };
                let op = dfg.insts[inst.index()].op.clone();
                let op = match op {
                    Op::Jump(e) => Op::Jump(shrink(dfg, e)),
                    Op::Brif(t, e) if first => Op::Brif(shrink(dfg, t), e),
                    Op::Brif(t, e) => Op::Brif(t, shrink(dfg, e)),
                    other => other,
                };
                dfg.insts[inst.index()].op = op;
            }
            EdgeRef::Table(jt, i) => {
                let t = &mut dfg.tables[jt.index()];
                if i == u32::MAX {
                    t.default.args.remove(at);
                } else {
                    t.table[i as usize].args.remove(at);
                }
            }
        }
    }

    fn append_arg(&mut self, edge: EdgeRef, v: Value) {
        let dfg = &mut self.f.dfg;
        match edge {
            EdgeRef::Inst(inst, first) => {
                let grow = |dfg: &mut crate::ir::DataFlowGraph, e: Edge| -> Edge {
                    let mut args = dfg.slice(e.args).to_vec();
                    args.push(v);
                    Edge {
                        block: e.block,
                        args: dfg.intern(&args),
                    }
                };
                let op = dfg.insts[inst.index()].op.clone();
                let op = match op {
                    Op::Jump(e) => Op::Jump(grow(dfg, e)),
                    Op::Brif(t, e) if first => Op::Brif(grow(dfg, t), e),
                    Op::Brif(t, e) => Op::Brif(t, grow(dfg, e)),
                    other => other,
                };
                dfg.insts[inst.index()].op = op;
            }
            EdgeRef::Table(jt, i) => {
                let t = &mut dfg.tables[jt.index()];
                if i == u32::MAX {
                    t.default.args.push(v);
                } else {
                    t.table[i as usize].args.push(v);
                }
            }
        }
    }

    fn zero_at_entry(&mut self, ty: Type) -> Value {
        let entry = self.f.layout[0];
        let dfg = &mut self.f.dfg;
        let inst = Inst(dfg.insts.len() as u32);
        let v = dfg.make_value(ty, ValueDef::Result(inst, 0));
        let results = dfg.intern(&[v]);
        let op = if ty.is_float() {
            Op::Fconst(0)
        } else {
            Op::Iconst(0)
        };
        dfg.insts.push(InstData {
            op,
            args: (0, 0),
            results,
            ty,
        });
        dfg.blocks[entry.index()].insts.insert(0, inst);
        v
    }
}
