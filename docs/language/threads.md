# Threads and atomics

Native programs can run several threads at once, on as many cores as the
machine has. The threads share memory, and the atomic builtins and the `sync`
library keep them from getting in each other's way (`examples/threads.jh`).

```jihoo
#![native]
import libc
import sync

struct Counter {
    lock: sync.Mutex
    total: i64
    hits: i64
    running: sync.Group
}

fn work(c: *Counter) {
    let i = 0
    while i < 1000 {
        sync.lock(&c.lock)          // one thread at a time in here
        c.total = c.total + i
        sync.unlock(&c.lock)
        atomic_add(&c.hits, 1)      // no lock needed
        i = i + 1
    }
    sync.group_done(&c.running)
}

fn main() -> i64 {
    let c = Counter { lock: sync.mutex_new(), total: 0, hits: 0, running: sync.group_new() }
    sync.group_add(&c.running, 2)
    go work(&c)                     // two threads, on two cores
    go work(&c)
    sync.group_wait(&c.running)     // until both are done
    libc.printf("%ld %ld\n", c.total, c.hits)    // 999000 2000
    return 0
}
```

## Threads

`go f(x, y)` starts a thread that calls `f(x, y)`, as it starts a
[task](tasks-and-channels.md) in hosted programs. The function and its
arguments are evaluated first, in the thread that says `go`, and copied, so
the new thread has values of its own; a pointer argument still points to the
same memory. Any function value works, and the result is dropped.

- The thread is the C library's (`pthread_create`), and runs on any core.
- Nothing waits for it: `main` returning ends the program, threads and all,
  as in Go. Wait for threads with a `sync.Group`, or have them send their
  results over a [channel](#channels).
- If the system has no thread or memory to spare, the program stops, as it
  does for a failed bounds check.
- Freestanding programs have no threads, so no `go`.

The [`thread`](../reference/standard-library.md#thread) library starts
threads too, ones that can be joined to get their result, and that report
failing to start instead of stopping the program:

- `thread.spawn(f, arg)` starts a thread that calls `f(arg)`, where `f` is a
  `fn(*u8) -> i64` and `arg` is any pointer, cast to `*u8`. It gives
  `Option(Thread)`: `None` if the system has no thread to spare.
- `thread.join(&t)` waits for `t` to finish, and gives what `f` returned. Join
  every thread once; joining the same `Thread` twice stops the program, but
  a copy of one does not know that the other was joined.
- `thread.cpus()` is the number of cores; `thread.yield()` and
  `thread.sleep_ms(ms)` give the others a turn.

Whatever a pointer given to a thread points to must outlive the thread's use
of it: memory from `malloc`, or a local of a function that waits for the
thread (with `join` or a `sync.Group`) before it returns.

## Data races

Two threads that use the same memory at the same time, at least one of them
writing, are a *data race*, and what they read is then undefined, as in C.
jihoo does not prevent data races in compiled programs, just as it does not
check pointers. Avoid them with one of:

- the atomic builtins, for single integers and pointers;
- a `sync.Mutex`, for anything larger: hold it while using the shared data;
- giving each thread memory of its own, as `examples/threads.jh` gives each
  worker its part of the job, and combining the results after `join`.

Hosted programs need none of this: their [tasks](tasks-and-channels.md) share
only channels and cells, and run one at a time.

## Atomics

An atomic operation reads or changes memory in one step that no other thread
can see half done. They work on memory of integer types, and loads, stores,
swaps and compare-and-swaps also on pointers.

| builtin | result | description |
|---------|--------|-------------|
| `atomic_load(p)` | `T` | reads `*p` |
| `atomic_store(p, v)` | `unit` | writes `v` to `*p` |
| `atomic_swap(p, v)` | `T` | writes `v` to `*p`; gives the old value |
| `atomic_add(p, v)`, `atomic_sub(p, v)` | `T` | adds or subtracts `v`, wrapping like `+` and `-`; gives the old value |
| `atomic_and(p, v)`, `atomic_or(p, v)`, `atomic_xor(p, v)` | `T` | the bitwise operations; give the old value |
| `atomic_cas(p, old, new)` | `bool` | if `*p` is `old`, writes `new`; says whether it did |
| `fence()` | `unit` | a full memory barrier |

`p` is a `*T`, and the values are of type `T`, so `atomic_add(&n, 1)` with
`n: u32` adds a `u32`.

All of them are *sequentially consistent*: every thread sees all atomic
operations happen in one order, the order each thread wrote its own in. That
is the easiest memory order to reason about, and the one C and Rust use when
none is named. They also order the plain reads and writes around them, so data
written before an `atomic_store` is seen by a thread that reads the stored
value with `atomic_load`.

```jihoo
// A spin lock out of a byte.
fn spin_lock(flag: *u8) {
    while atomic_swap(flag, 1) != 0 {
        thread.yield()
    }
}

fn spin_unlock(flag: *u8) {
    atomic_store(flag, 0)
}
```

The atomics work in freestanding programs too; there are no threads there,
but a program can share memory with the kernel, a signal handler, or other
processes. Hosted programs have no pointers, so no atomics. As with `sqrt`,
a module may define a function with an atomic's name, which then hides the
builtin.

## Locks

The [`sync`](../reference/standard-library.md#sync) library has the usual
ways to make threads wait for each other, built on the atomics and the Linux
kernel's futexes. It needs no libc, so freestanding programs can import it
too.

- `sync.Mutex`: one thread at a time between `lock` and `unlock`.
- `sync.Cond`: wait, holding a Mutex, until another thread signals that
  something changed.
- `sync.Group`: wait until a number of threads have finished, like Go's
  `WaitGroup`.

```jihoo
// Wait until `ready`, which another thread sets under `m` and then signals.
sync.lock(&m)
while !ready {
    sync.wait(&c, &m)
}
sync.unlock(&m)
```

A Mutex, Cond or Group is shared by pointer and must stay where it is while
threads use it: a copy is an unrelated second one.

## Channels

Threads can also share nothing and pass values instead, as hosted tasks do:
the [`channel`](../reference/standard-library.md#channel) library has
channels between threads, with `close` to say that no more values will come
(`examples/pipeline.jh`, a worker pool).

```jihoo
// A worker: takes jobs until the channel is closed and empty.
let more = true
while more {
    match channel.recv(i64, jobs) {
        Some(n) => channel.send(i64, results, work(n))
        None => more = false
    }
}
```

The type of the values comes first in each call, as with `alloc`'s `Vec`.

To wait on several channels at once, as hosted programs do with `select`,
use a `channel.Select`: add receive and send cases to it, then
`channel.wait(&sel)` gives the number of the case that went ahead
([Select](../reference/standard-library.md#select)).
