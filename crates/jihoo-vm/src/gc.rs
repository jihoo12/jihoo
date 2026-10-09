//! VM-managed heap with a simple stop-the-world mark & sweep collector.
//!
//! Strings and aggregates (structs, arrays, enums, refs, closures) live here.
//! Aggregate objects are immutable: the VM implements value semantics by
//! allocating a new object on every field or element update. Channels are the
//! one mutable kind of object: tasks share them to communicate.
//!
//! As an optimization, an aggregate that only one register refers to may be
//! updated in place: the result of an update replaces that register's value,
//! and nothing else can see the difference. The VM marks an object *shared*
//! as soon as a second reference to it may exist (see `Vm::share`), and only
//! updates unshared objects in place.
//!
//! Setting `JIHOO_GC_STRESS=1` collects before every allocation, so a value that
//! is live but not reachable from the roots is freed at the first chance instead
//! of once in a blue moon. Run the test suite this way after touching rooting.

use std::collections::VecDeque;

use jihoo_ir::Reg;

use crate::Value;

/// A handle to a heap object: a slot index plus the slot's generation when the
/// object was allocated. A freed slot moves to the next generation, so a stale
/// handle is caught on use even after the slot is reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GcRef {
    index: u32,
    gen: u32,
}

#[derive(Debug)]
enum Obj {
    Str(Box<str>),
    /// Fields of a struct or elements of an array.
    Agg(Box<[Value]>),
    Chan(Box<Channel>),
}

/// A channel between tasks. Tasks are numbered by the VM.
#[derive(Debug, Default)]
pub struct Channel {
    /// How many values `buf` may hold; 0 means a sender waits for a receiver.
    pub cap: usize,
    pub buf: VecDeque<Value>,
    /// Tasks waiting to send, with their values.
    pub senders: VecDeque<(Waiter, Value)>,
    /// Tasks waiting to receive.
    pub receivers: VecDeque<Waiter>,
}

/// A task waiting on a channel. A task in a `select` waits on several channels
/// at once, with one waiter in each; they share its `token`, so the others can
/// be removed when one goes ahead.
#[derive(Debug, Clone, Copy)]
pub struct Waiter {
    pub task: usize,
    pub token: u64,
    /// For a receiver: the register the value goes to.
    pub dst: Option<Reg>,
    /// For a `select`: the register that gets the index of the case that went
    /// ahead, and that index.
    pub choice: Option<(Reg, i64)>,
}

impl Obj {
    fn size(&self) -> usize {
        std::mem::size_of::<Obj>()
            + match self {
                Obj::Str(s) => s.len(),
                Obj::Agg(fields) => std::mem::size_of_val(&**fields),
                // The buffer grows later; this is only an estimate.
                Obj::Chan(c) => std::mem::size_of::<Channel>() + c.cap * std::mem::size_of::<Value>(),
            }
    }

    fn children(&self, out: &mut Vec<GcRef>) {
        match self {
            Obj::Str(_) => {}
            Obj::Agg(fields) => out.extend(fields.iter().filter_map(Value::gc_ref)),
            Obj::Chan(c) => {
                out.extend(c.buf.iter().filter_map(Value::gc_ref));
                out.extend(c.senders.iter().filter_map(|(_, v)| v.gc_ref()));
            }
        }
    }
}

#[derive(Debug)]
struct Slot {
    obj: Option<Obj>,
    gen: u32,
    marked: bool,
    /// More than one reference to the object may exist.
    shared: bool,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct HeapStats {
    pub live_objects: usize,
    pub live_bytes: usize,
    pub collections: usize,
}

#[derive(Debug)]
pub struct Heap {
    slots: Vec<Slot>,
    free: Vec<u32>,
    live_bytes: usize,
    next_gc: usize,
    collections: usize,
    /// Collect before every allocation (`JIHOO_GC_STRESS`).
    stress: bool,
}

const INITIAL_THRESHOLD: usize = 1 << 20;

impl Default for Heap {
    fn default() -> Self {
        Heap {
            slots: Vec::new(),
            free: Vec::new(),
            live_bytes: 0,
            next_gc: INITIAL_THRESHOLD,
            collections: 0,
            stress: std::env::var_os("JIHOO_GC_STRESS").is_some_and(|v| !v.is_empty() && v != "0"),
        }
    }
}

impl Heap {
    /// True when the caller should run [`Heap::collect`] before allocating more.
    pub fn should_collect(&self) -> bool {
        self.stress || self.live_bytes >= self.next_gc
    }

    /// Collects before every allocation from now on, whatever `JIHOO_GC_STRESS` says.
    pub fn set_stress(&mut self, on: bool) {
        self.stress = on;
    }

    pub fn alloc_str(&mut self, s: &str) -> GcRef {
        self.alloc(Obj::Str(s.into()))
    }

    pub fn alloc_agg(&mut self, fields: Vec<Value>) -> GcRef {
        self.alloc(Obj::Agg(fields.into()))
    }

    pub fn alloc_chan(&mut self, cap: usize) -> GcRef {
        self.alloc(Obj::Chan(Box::new(Channel { cap, ..Channel::default() })))
    }

    fn alloc(&mut self, obj: Obj) -> GcRef {
        self.live_bytes += obj.size();
        match self.free.pop() {
            Some(index) => {
                let slot = &mut self.slots[index as usize];
                slot.obj = Some(obj);
                slot.shared = false;
                GcRef { index, gen: slot.gen }
            }
            None => {
                self.slots.push(Slot { obj: Some(obj), gen: 0, marked: false, shared: false });
                GcRef { index: self.slots.len() as u32 - 1, gen: 0 }
            }
        }
    }

    /// The live object `r` refers to. Panics on a freed object: that is a rooting
    /// bug in the VM, never an error in the jihoo program.
    fn obj(&self, r: GcRef) -> &Obj {
        let slot = &self.slots[r.index as usize];
        match &slot.obj {
            Some(obj) if slot.gen == r.gen => obj,
            _ => panic!("use of freed object {r:?}"),
        }
    }

    pub fn str(&self, r: GcRef) -> &str {
        match self.obj(r) {
            Obj::Str(s) => s,
            _ => panic!("{r:?} is not a string"),
        }
    }

    /// Notes that another reference to `r` may now exist.
    pub fn mark_shared(&mut self, r: GcRef) {
        self.slots[r.index as usize].shared = true;
    }

    pub fn is_shared(&self, r: GcRef) -> bool {
        self.slots[r.index as usize].shared
    }

    /// The parts of aggregate `r`, to update in place. Only for an unshared
    /// object, whose one reference is about to be replaced by itself.
    pub fn items_mut(&mut self, r: GcRef) -> &mut [Value] {
        let slot = &mut self.slots[r.index as usize];
        match &mut slot.obj {
            Some(Obj::Agg(f)) if slot.gen == r.gen => f,
            _ => panic!("{r:?} is not a live aggregate"),
        }
    }

    pub fn chan_mut(&mut self, r: GcRef) -> &mut Channel {
        let slot = &mut self.slots[r.index as usize];
        match &mut slot.obj {
            Some(Obj::Chan(c)) if slot.gen == r.gen => c,
            Some(_) if slot.gen == r.gen => panic!("{r:?} is not a channel"),
            _ => panic!("use of freed object {r:?}"),
        }
    }

    pub fn items(&self, r: GcRef) -> &[Value] {
        match self.obj(r) {
            Obj::Agg(f) => f,
            _ => panic!("{r:?} is not an aggregate"),
        }
    }

    pub fn collect<'a>(&mut self, roots: impl IntoIterator<Item = &'a Value>) {
        // Mark.
        let mut work: Vec<GcRef> = roots.into_iter().filter_map(Value::gc_ref).collect();
        while let Some(r) = work.pop() {
            let slot = &mut self.slots[r.index as usize];
            assert!(slot.obj.is_some() && slot.gen == r.gen, "reachable value refers to freed object {r:?}");
            if slot.marked {
                continue;
            }
            slot.marked = true;
            slot.obj.as_ref().unwrap().children(&mut work);
        }

        // Sweep.
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.marked {
                slot.marked = false;
            } else if let Some(obj) = slot.obj.take() {
                self.live_bytes -= obj.size();
                slot.gen = slot.gen.wrapping_add(1);
                self.free.push(i as u32);
            }
        }

        self.collections += 1;
        self.next_gc = (self.live_bytes * 2).max(INITIAL_THRESHOLD);
    }

    pub fn stats(&self) -> HeapStats {
        HeapStats {
            live_objects: self.slots.iter().filter(|s| s.obj.is_some()).count(),
            live_bytes: self.live_bytes,
            collections: self.collections,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "use of freed object")]
    fn stale_ref_is_caught_after_slot_reuse() {
        let mut heap = Heap::default();
        let old = heap.alloc_str("old");
        heap.collect([]);
        let new = heap.alloc_str("new");
        assert_eq!(old.index, new.index, "the slot should be reused");
        heap.str(old);
    }
}
