# Tasks and channels

Programs can run many *tasks* at once, like goroutines, which talk to each
other through *channels*: on the VM's scheduler in hosted programs, and as
threads on every core in native ones.

```jihoo
enum Msg { Item(i64), Done }

fn numbers(out: chan Msg, n: i64) {
    let i = 0
    while i < n {
        send(out, Msg.Item(i))
        i = i + 1
    }
    send(out, Msg.Done)
}

fn main() {
    let c = chan(Msg)            // unbuffered; `chan(Msg, 8)` buffers 8 values
    go numbers(c, 5)
    let total = 0
    let running = true
    while running {
        match recv(c) {
            Item(x) => total = total + x
            Done => running = false
        }
    }
    print(total)                 // 10
}
```

## Tasks

`go f(x)` runs a call in a new task. The function and its arguments are
evaluated first, in the task that says `go`; any function value works, closures
included:

```jihoo
go worker(jobs, results)
go fn() { send(results, work(id)) }()
```

The result of the call is dropped; send it over a channel to get it back.

## Channels

- `chan(T)` makes an unbuffered channel of `T` values, and `chan(T, n)` one that
  buffers up to `n`. The type is `chan T`.
- `send(c, v)` waits while the channel is full; an unbuffered one is full until
  a receiver comes, so sender and receiver meet.
- `recv(c)` waits until there is a value, and returns it.
- There is no `close`. To say "no more values", send a value that says so, as
  `Msg.Done` above; `match` then handles both cases.

Tasks share nothing but channels and [cells](cells.md): arguments are copies,
closures capture by value, and [refs](references.md) are immutable. So there
are no data races to worry about.

## `select`

`select` waits on several channels at once (`examples/select.jh`):

```jihoo
select {
    let n = recv(numbers) => total = total + n
    recv(quit) => running = false
    send(log, line) => {}
    _ => print("nothing ready")     // optional: do not wait
}
```

- The cases are `recv(c)`, `let x = recv(c)` (binding the value for the arm),
  `send(c, v)`, and an optional last `_`.
- The channels and the values to send are evaluated first, in order.
- Then the first case that can go ahead without waiting does, and its arm runs.
  If several can, the first in source order wins (Go picks at random; jihoo
  stays reproducible).
- With `_` and no case ready, the `_` arm runs. Otherwise the task waits on all
  the channels, and the first case another task makes possible goes ahead; its
  waits on the other channels are cancelled.

## Scheduling

Tasks run on one OS thread. The VM's scheduler is round-robin and
deterministic: a task runs until it waits on a channel, finishes, or has run
1000 instructions and reaches a *safepoint* (a call, or a loop going round);
then the next ready task gets its turn.

- The same program prints the same output on every run, which keeps tests
  reliable.
- Code without calls and loops is never interrupted, which is what makes
  updates of [cells](cells.md#cells-and-tasks) such as `*n = *n + 1` safe.

## Ending

- The program ends when `main` returns, even if other tasks are still running
  or waiting (as in Go).
- If every task waits on a channel, the run stops with
  `deadlock: every task is waiting on a channel`.
- An error in any task stops the whole run.

## In native programs

Natively `go f(x)` starts a [thread](threads.md), and `chan`, `send`,
`recv` and `select` work as above, on the `channel` library, which the
compiler adds to a native program that uses them. The differences:

- Tasks run at the same time, on every core, so their order is not
  deterministic: a program whose output must not depend on it waits for what
  it prints, through channels, as a correct Go program does.
- Tasks share no [cells](cells.md#in-each-profile): a cell in the arguments
  of `go`, or in the values of a channel, is an error, and starting a
  closure that captured one stops the program. Share state by sending
  values instead.
- A deadlock is not detected: the waiting threads wait forever.
- A `select` has at most 16 cases.

A value sent moves into the channel and then to the receiver, so sending a
list copies nothing; values still in a channel when its last copy goes are
freed with it. Freestanding programs have no tasks or channels, and native
programs also have [coroutines](../reference/standard-library.md#coro).

In JIR, tasks and channels are the `chan`, `send`, `recv`, `select` and
`spawn` instructions.
