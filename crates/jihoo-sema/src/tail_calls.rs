//! Tail calls of a function to itself: `return f(a, b)` inside `f`.
//!
//! Such a call becomes a jump: the arguments are assigned to the parameters
//! and control goes back to the start of the body, so the recursion runs in
//! constant stack. This is a rule of the language, not an optimization: it
//! holds on the VM and natively, at every `-O` level, where the backend would
//! otherwise leave deep recursion to LLVM (which may or may not turn it into a
//! loop) or the VM's call stack (which has a limit).
//!
//! It happens in sema, so JIR only sees a loop: natively the ownership pass
//! frees the locals of one round before the jump and moves the arguments into
//! the parameters, as it does for any loop that assigns its variables, and a
//! call that would have had to free something after it returned is a tail
//! call all the same.

use jihoo_ir::{BlockId, Inst, Reg, Terminator};

use crate::{BlockBuf, FnCx};

impl FnCx<'_> {
    /// If the instruction that made `r`, the value to return, is the last one
    /// so far and calls the function being lowered, replaces it and the return
    /// with assignments to the parameters and a jump back to the start.
    pub(crate) fn tail_call(&mut self, r: Reg) -> bool {
        let Some(name) = &self.name else { return false };
        let cur = self.cur.0 as usize;
        let args = match self.blocks[cur].insts.last() {
            Some(Inst::Call { dst, func, args }) if *dst == r && func == name => args.clone(),
            _ => return false,
        };
        self.blocks[cur].insts.pop();
        let head = self.body_start();

        // The new values first, then the parameters: an argument may be a
        // parameter (`f(b, a)` in `f(a, b)`), which must be read before it is
        // assigned.
        let n = self.sig.params.len();
        let mut values = Vec::with_capacity(n);
        for (i, &a) in args.iter().enumerate() {
            let is_other_param = (a.0 as usize) < n && a.0 as usize != i;
            values.push(if is_other_param { self.emit_to(self.ty(a).clone(), |dst| Inst::Copy { dst, src: a }) } else { a });
        }
        for (i, v) in values.into_iter().enumerate() {
            let p = Reg(i as u32);
            if v != p {
                self.emit(Inst::Copy { dst: p, src: v });
            }
        }
        self.terminate(Terminator::Jump(head));
        true
    }

    /// The block that the body starts in, after the constants hoisted to the
    /// front of the entry block. The entry block is split the first time, so
    /// that the constants are made once and the entry has no predecessors.
    fn body_start(&mut self) -> BlockId {
        if let Some(h) = self.head {
            return h;
        }
        let head = self.new_block();
        let entry = &mut self.blocks[0];
        let body: Vec<Inst> = entry.insts.drain(self.hoisted..).collect();
        let term = entry.term.replace(Terminator::Jump(head));
        self.blocks[head.0 as usize] = BlockBuf { insts: body, term };
        if self.cur == BlockId(0) {
            self.cur = head;
        }
        self.head = Some(head);
        head
    }
}
