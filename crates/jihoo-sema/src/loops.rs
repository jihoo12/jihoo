//! `for` loops, lowered to the same blocks as a `while` loop: a counter, a
//! test against a limit, and the body.
//!
//! `for x in xs` walks the value `xs` has when the loop starts: the loop reads
//! a copy of it, which the VM shares until one side changes, and which the
//! ownership pass keeps as a view of `xs` unless the body changes `xs`
//! (`crates/jihoo-ir/src/ownership.rs`). So the loop copies nothing unless
//! changing `xs` makes a copy necessary. A range `lo..hi` counts from `lo` up to
//! but not including `hi`, both evaluated once.
//!
//! The counter steps before the body runs, so `continue` goes straight back to
//! the test, and the counter never steps past the limit, even when that is
//! the largest value of its type.

use jihoo_ir::{BinOp, BlockId, Inst, IntTy, Reg, Terminator, Type};
use jihoo_syntax::ast::{Block, Expr};
use jihoo_syntax::{Error, Pos};

use crate::{number_literal, FnCx, Loop};

impl FnCx<'_> {
    pub(crate) fn for_stmt(
        &mut self,
        pos: Pos,
        index: Option<&str>,
        name: &str,
        iter: &Expr,
        end: Option<&Expr>,
        body: &Block,
    ) -> Result<(), Error> {
        match end {
            Some(end) => {
                if index.is_some() {
                    return Err(Error::new(pos, format!("a range gives one value: `for {name} in lo..hi`")));
                }
                self.range_loop(name, iter, end, body)
            }
            None => self.each_loop(index, name, iter, body),
        }
    }

    /// `for i in lo..hi`.
    fn range_loop(&mut self, name: &str, lo: &Expr, hi: &Expr, body: &Block) -> Result<(), Error> {
        // `0..n`: the literal takes the type of `n`. Literals have no side
        // effects, so evaluating `n` first is not observable.
        let (l, h) = if number_literal(lo).is_some() && number_literal(hi).is_none() {
            let h = self.expr(hi, None)?;
            let t = self.ty(h).clone();
            (self.expr(lo, Some(&t))?, h)
        } else {
            let l = self.expr(lo, None)?;
            let t = self.ty(l).clone();
            (l, self.expr(hi, Some(&t))?)
        };
        let t = self.ty(l).clone();
        if t.as_int().is_none() {
            return Err(Error::new(lo.pos, format!("a range needs integers, found {t}")));
        }
        self.expect(hi.pos, h, &t, "the end of the range")?;
        // Copies, so that the body changing a variable used as a bound
        // changes neither the counter nor the limit.
        let counter = self.emit_to(t.clone(), |dst| Inst::Copy { dst, src: l });
        let limit = self.emit_to(t, |dst| Inst::Copy { dst, src: h });
        let head = self.loop_head(counter, limit);
        self.loop_body(head, vec![(name.to_string(), head.value)], body)
    }

    /// `for x in xs` and `for i, x in xs`, over a list, an array or a str.
    fn each_loop(&mut self, index: Option<&str>, name: &str, iter: &Expr, body: &Block) -> Result<(), Error> {
        let v = self.expr(iter, None)?;
        let t = self.ty(v).clone();
        let elem = match &t {
            Type::List(elem) | Type::Array(elem, _) => (**elem).clone(),
            Type::Str => Type::Int(IntTy::U8),
            _ => {
                let msg = format!("cannot loop over {t}; `for` walks a list, an array, a str or a range `lo..hi`");
                return Err(Error::new(iter.pos, msg));
            }
        };
        let seq = self.emit_to(t.clone(), |dst| Inst::Copy { dst, src: v });
        let limit = match t {
            Type::Array(_, n) => {
                let n = i64::try_from(n).map_err(|_| Error::new(iter.pos, "array is too long"))?;
                self.konst(Type::I64, n)
            }
            _ => self.emit_to(Type::I64, |dst| Inst::Len { dst, src: seq }),
        };
        let zero = self.konst(Type::I64, 0);
        let counter = self.emit_to(Type::I64, |dst| Inst::Copy { dst, src: zero });
        let head = self.loop_head(counter, limit);
        let i = head.value;
        // A register read into afresh each round: the body may assign to it
        // without a copy, as nothing else holds it.
        let x = match t {
            Type::Str => self.emit_to(elem, |dst| Inst::StrByte { dst, src: seq, index: i }),
            _ => self.emit_to(elem, |dst| Inst::Elem { dst, src: seq, index: i }),
        };
        let mut names = vec![(name.to_string(), x)];
        if let Some(index) = index {
            names.push((index.to_string(), i));
        }
        self.loop_body(head, names, body)
    }

    /// Starts a loop while `counter < limit`. Each round copies the counter
    /// into `value`, which the body may change, and then steps it in place.
    fn loop_head(&mut self, counter: Reg, limit: Reg) -> LoopHead {
        let t = self.ty(counter).clone();
        let one = self.konst(t.clone(), 1);
        let test = self.new_block();
        let body = self.new_block();
        let end = self.new_block();
        self.terminate(Terminator::Jump(test));

        self.switch_to(test);
        let c = self.emit_to(Type::Bool, |dst| Inst::Binary { dst, op: BinOp::Lt, lhs: counter, rhs: limit });
        self.terminate(Terminator::Branch { cond: c, then: body, els: end });

        self.switch_to(body);
        let value = self.emit_to(t, |dst| Inst::Copy { dst, src: counter });
        self.emit(Inst::Binary { dst: counter, op: BinOp::Add, lhs: counter, rhs: one });
        LoopHead { value, test, end }
    }

    /// Runs `body` with `names` in scope, then goes back to the test.
    fn loop_body(&mut self, head: LoopHead, names: Vec<(String, Reg)>, body: &Block) -> Result<(), Error> {
        self.scopes.push(names.into_iter().collect());
        self.loops.push(Loop { next: head.test, end: head.end });
        let r = self.block(body);
        self.loops.pop();
        self.scopes.pop();
        r?;
        self.terminate(Terminator::Jump(head.test));
        self.switch_to(head.end);
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct LoopHead {
    /// The counter's value in this round.
    value: Reg,
    /// The block that tests the counter, where `continue` goes.
    test: BlockId,
    /// The block after the loop.
    end: BlockId,
}
