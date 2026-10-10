# Cells

`cell T` is the one kind of shared, mutable state in hosted code. Copies of a
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

[Tasks](tasks-and-channels.md) may share cells. A task is only interrupted at a
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

## Hosted only

The GC owns what a cell holds, so cells exist only in hosted programs. Native
and freestanding code shares state through [pointers](pointers.md).
