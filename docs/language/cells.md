# Cells

`cell T` is the one kind of shared, mutable state outside pointers. Copies of a
cell are the same cell, so a change made through one is seen through all.

```jihoo
struct Account {
    owner: str
    balance: i64
}

fn deposit(a: cell Account, amount: i64) {
    a.balance = a.balance + amount       // seen by everything holding the cell
}

fn main() {
    let acct = cell(Account { owner: "ada", balance: 0 })
    deposit(acct, 50)
    deposit(acct, 25)
    print(acct.balance)                  // 75

    let n = cell(0)
    let next = fn() -> i64 {             // a closure with state
        *n = *n + 1
        return *n
    }
    next()
    print(next())                        // 2
}
```

## Making and using cells

- `cell(v)` makes a cell holding a copy of `v`. Its type is `cell T`.
- Copying the cell — assigning it, passing it, capturing it in a closure,
  sending it over a channel — shares it, and everything holding it sees changes.
- `*c` is what it holds, and `c.x`, `c[i]`, `c.a[i]` are parts of that: all can
  be read and assigned.
- Reading gives a copy: `let before = *c` keeps the value of that moment, even
  after the cell changes.

## Cost

Each read or write is one instruction (`cellget`/`cellset` with a path), and a
nested write updates what the cell holds in place
([VM and GC](../internals/vm.md#value-semantics-and-in-place-updates)), so a cell
holding a large array has O(1) element writes.

## Cells and tasks

Natively, the default, [tasks](tasks-and-channels.md) are threads and share no
cells (see [In each profile](#in-each-profile)): each task keeps its own, and
tasks pass values over channels.

In a hosted program (`#![hosted]`) tasks may share cells. A task is only interrupted at a
call or where a loop goes round, so a statement without calls, such as
`*n = *n + 1` or `c.count = c.count + 1`, never interleaves with another task.

An update that calls a function in between, `*c = f(*c)`, can be interleaved.
Guard it with a channel that has room for one value, used as a lock:

```jihoo
let lock = chan(bool, 1)
// in each task:
send(lock, true)        // take the lock (waits while another task holds it)
*c = f(*c)
recv(lock)              // release it
```

## Comparing

Cells cannot be compared: two cells holding equal values are still two cells.
Compare what they hold, `*a == *b`.

## In each profile

On the VM the GC owns what a cell holds. Natively a cell points to memory its
copies share, with a count of them, freed when the last copy goes. Reading
what a cell holds makes a copy (another holder could change it while it is
read), so read the part you need: `c.items[i]` copies one element, `*c` copies
everything.

A cell never reaches another thread, where two threads could change it at
once: `go f(c)` with a cell in the arguments is an error, and starting a
closure that captured a cell with `go` stops the program. Threads share state
through [channels and locks](threads.md) instead.

A cell that holds itself, through other cells, is never freed natively: the
count of its copies never reaches zero. Freestanding programs have no cells;
they share state through [pointers](pointers.md).
