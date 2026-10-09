//! VM-managed heap with a simple stop-the-world mark & sweep collector.
//!
//! Only strings live here for now. The worklist-based mark phase is written so that
//! objects with children (arrays, structs, closures) can be added later.

use crate::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GcRef(u32);

#[derive(Debug)]
enum Obj {
    Str(Box<str>),
}

impl Obj {
    fn size(&self) -> usize {
        match self {
            Obj::Str(s) => s.len() + std::mem::size_of::<Obj>(),
        }
    }

    fn children(&self, _out: &mut Vec<GcRef>) {
        match self {
            Obj::Str(_) => {}
        }
    }
}

#[derive(Debug)]
struct Slot {
    obj: Option<Obj>,
    marked: bool,
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
        }
    }
}

impl Heap {
    /// True when the caller should run [`Heap::collect`] before allocating more.
    pub fn should_collect(&self) -> bool {
        self.live_bytes >= self.next_gc
    }

    pub fn alloc_str(&mut self, s: &str) -> GcRef {
        self.alloc(Obj::Str(s.into()))
    }

    fn alloc(&mut self, obj: Obj) -> GcRef {
        self.live_bytes += obj.size();
        let slot = Slot { obj: Some(obj), marked: false };
        match self.free.pop() {
            Some(i) => {
                self.slots[i as usize] = slot;
                GcRef(i)
            }
            None => {
                self.slots.push(slot);
                GcRef(self.slots.len() as u32 - 1)
            }
        }
    }

    pub fn str(&self, r: GcRef) -> &str {
        match &self.slots[r.0 as usize].obj {
            Some(Obj::Str(s)) => s,
            None => panic!("use of freed object {r:?}"),
        }
    }

    pub fn collect<'a>(&mut self, roots: impl IntoIterator<Item = &'a Value>) {
        // Mark.
        let mut work: Vec<GcRef> = roots
            .into_iter()
            .filter_map(|v| match v {
                Value::Str(r) => Some(*r),
                Value::Unit | Value::Int(_) | Value::Bool(_) => None,
            })
            .collect();
        while let Some(r) = work.pop() {
            let slot = &mut self.slots[r.0 as usize];
            if slot.marked {
                continue;
            }
            slot.marked = true;
            if let Some(obj) = &slot.obj {
                obj.children(&mut work);
            }
        }

        // Sweep.
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.marked {
                slot.marked = false;
            } else if let Some(obj) = slot.obj.take() {
                self.live_bytes -= obj.size();
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
