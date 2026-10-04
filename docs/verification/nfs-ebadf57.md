# Issue 57: the connection flood, the descriptor table, and what the server does about it

Branch `fix/nfs-ebadf-57`, base `ceb96c6`.
Machine: Apple M3 Max Mac, macOS 26.6.2 (25G83), `kern.maxfilesperproc` 245760, shell soft `ulimit -n` 1048576, hard unlimited.
Two symbols recur: `EMFILE` is errno 24, `ECONNREFUSED` is errno 61.

## What the issue reported

On `main` at `f2be21b`, and on `v1/10-gc`:

```
thread 'a_connection_flood_is_capped_and_the_server_recovers' panicked at crates/cowfs-nfs/tests/hardening.rs:50:10:
called `Result::unwrap()` on an `Err` value: Os { code: 9, kind: Uncategorized, message: "Bad file descriptor" }
```

Line 50 was the `fd_limit()` helper, which spawns `/bin/sh -c 'ulimit -n'`.
The issue asked two questions: should the helper read the limit once before the flood rather than during it, and is the flood opening more descriptors than the test intends.

## Two separate things, and only one of them is the server's fault

The reported panic is a harness defect, and PR #74 (already merged) fixed it.
Behind it there was a real server defect that the harness defect was hiding, and that is what this branch fixes.

| | reported panic | the real defect |
|---|---|---|
| what | `EBADF` from the test's own `/bin/sh` spawn | `EMFILE` from the server's `accept` |
| cause | the test spent the descriptor budget its siblings were already using | the flood drove the table to exhaustion and `accept`'s `EMFILE` was treated as fatal |
| effect | one test panics | the listening socket is dropped and the server never accepts again |
| fixed by | PR #74, already on `main` | this branch |

## Part 1: reproducing the reported panic

The pre-#74 `hardening.rs` was restored verbatim as its own test binary and run with the default
test parallelism, because that is the shape the issue saw ("3 of 3 runs, with no other load" means
no load outside the test binary, not an empty process).

`crates/cowfs-nfs/tests/hardening.rs` at `76f5a86^`, 13 soft limits, one run each, 12 s per run:

| soft limit | result |
|---|---|
| 128 | FAILED, 2 of 21 |
| 160 | FAILED, 3 of 21 |
| 192 | FAILED, 1 of 21 |
| 224 | FAILED, 3 of 21 |
| 256 | FAILED, 2 of 21 |
| 320 | FAILED, 2 of 21 |
| 384 | FAILED, 2 of 21 |
| 512 | FAILED, 2 of 21 |
| 768 | FAILED, 1 of 21 |
| 1024 | ok, 21 of 21 |
| 2048 | ok, 21 of 21 |
| 4096 | ok, 21 of 21 |
| 16384 | ok, 21 of 21 |

The named test panicked at exactly the reported call site, 5 of 6 runs at limit 256:

```
thread 'a_connection_flood_is_capped_and_the_server_recovers' panicked at crates/cowfs-nfs/tests/hardening_pre74.rs:50:10:
called `Result::unwrap()` on an `Err` value: Os { code: 24, kind: TooManyOpenFiles, message: "Too many open files" }
```

**The errno here is 24 (EMFILE), not 9 (EBADF).** The call site, the test and the mechanism match
the issue exactly; the number does not. See "EBADF" below for what was and was not established about that.

### Answering the issue's two questions

**Should the helper read the limit once before the flood rather than during it?**
Yes, and that is not the whole fix. Reading once removes the spawn from the exhausted window, but
the flood itself still spent the whole table.

**Is the flood opening more descriptors than the test intends?**
It sized itself `n = min(10_000, (limit - 120) / 2)`, which assumes it owns the descriptor table.
It does not. Instrumenting the concurrent run, descriptors already in use before the flood opened
its first socket:

| run | in use before the flood | limit | client sockets opened | after the flood |
|---|---|---|---|---|
| 1 | 121 | 256 | 49 | `/dev/fd` could not be opened at all |
| 2 | 122 | 256 | 60 | 255 |
| 3 | 126 | 256 | 68 | 198 |

121 to 126 of 256 descriptors were consumed by sibling tests before the flood began.
Each of the ~21 tests in that binary runs its own `Server`, and a `Server` is a 4-worker tokio
runtime plus a listener, which measured 9 descriptors (`base` 4 to `after_server` 13).
The flood then needs its own `n` client descriptors plus up to the cap on the server side, and
there was no room left for the spawn.

### Control: the same spawn at the same descriptor count with no cowfs at all

A standalone probe held `limit - 120 / 2` UDP sockets and then ran the identical
`Command::new("/bin/sh").args(["-c", "ulimit -n"]).output()`, sampling every fill level:

| soft limit | spawn succeeded | spawn failed |
|---|---|---|
| 256 | 166 | 4 |
| 128 | 80 | 4 |

In both cases the failures were the last two fill levels, and all of them were **errno 24**.
The spawn needs descriptors, and running out of them fails the spawn. No cowfs code involved.

### FD ownership, traced

Server side, measured in isolation (one flood, no sibling tests, `resource_bounds`-style probe):

```
base=4  after_server=13  during=145  client_socks=68  server_side_during_flood=64  after_drop=13
```

- Server side peaks at exactly `Limits::default().max_connections` (64). The cap holds.
- `after_drop` returns to `after_server`, so the server released every connection. No leak.
- A new client is served after the flood, and the original client keeps working.

**No bad close, no double close, no leak.** Neither `cowfs-nfs` nor `nfsserve` contains `unsafe`,
`libc`, `from_raw_fd`, `AsRawFd` or any raw descriptor handling; every descriptor is owned by
Rust's `TcpStream` or tokio and dropped exactly once, and the workspace lint is
`unsafe_code = "deny"`. Worker cancellation and descriptor ownership are not reachable as a cause.

## Part 2: the real defect, which the harness defect was hiding

The pre-#74 and current tests only ever check recovery when descriptors are plentiful.
The claim in the issue's own title, that the server recovers from a connection flood, was never
tested against an exhausted descriptor table.

Driving the table to exhaustion for real, in a private process at `ulimit -n 512`:

```
flood stopped at 436 client sockets of limit 512, 511 of 512 descriptors in use
established client during exhaustion: OK
open_fds after release: 12
reattach after the flood: ECONNREFUSED (61)
```

Twelve of 512 descriptors in use, and the port refuses connections.
The listening socket was gone. Instrumenting the accept loop confirmed why:

```
NFS57PROBE accept loop exiting: errno=Some(24) kind=TooManyOpenFiles Too many open files (os error 24)
```

5 of 5 instrumented runs printed it.

### Mechanism

1. `crates/nfsserve/src/tcp.rs` `handle_forever` calls `self.listener.accept().await`.
2. With the descriptor table full the kernel cannot allocate a descriptor for the accepted socket, so `accept` returns `EMFILE`.
3. `is_transient_accept_error` matched only `ConnectionAborted`, `ConnectionReset` and `Interrupted`.
   `EMFILE` fell through to `Err(e) => return Err(e)`.
4. Returning drops the `TcpListener`. The port stops accepting.
5. `crates/cowfs-nfs/src/mount.rs` `Server::start` discarded that result with `let _ =`, so nothing was logged and nothing recovered.

`crates/nfsserve/PATCHES.md` already claimed "`accept` errors do not end the server loop".
The claim was true for three errnos and false for the one a flood produces.

The client that had already connected kept working, which is what makes this worth fixing rather
than noting: a remount, a reconnect, or the kernel's own NFS client opening a new connection all
need `accept`, and after the flood none of them succeeded.

## The fix

Two changes, both minimal, neither relaxing a limit or a timeout.

**`crates/nfsserve/src/tcp.rs`** — `EMFILE` is a recoverable accept error:

```rust
const EMFILE: i32 = 24;

fn is_transient_accept_error(e: &io::Error) -> bool {
    if e.raw_os_error() == Some(EMFILE) {
        return true;
    }
    ...
}
```

`io::ErrorKind::TooManyOpenFiles` is still unstable on stable Rust, so the number is spelled out.
It is 24 on Linux and macOS, the only targets.

**`crates/cowfs-nfs/src/mount.rs`** — a fatal accept-loop exit is now reported instead of dropped.
Recoverable errors are looped over inside the listener, so anything arriving at that `await` is
fatal and about to stop the port accepting. Silently swallowing it is what made this class of
failure invisible.

**Deliberately not added:** `ENFILE` (23, system-wide) has no stable `ErrorKind` and does not occur
on either target; `ENOMEM` was not observed. Both would be additions beyond the evidence.

**Known ceiling:** a sustained flood with a non-empty accept queue retries `accept` at syscall
speed. Measured cost was not observable (the child runs 2.05 s wall, almost all of it a 2 s sleep),
so no pause was added. If CPU burn under a sustained flood ever shows up, add a short sleep on
`EMFILE`. Marked in the source as a `ponytail:` comment.

## The regression

`crates/cowfs-nfs/tests/resource_bounds.rs`, `an_exhausting_flood_does_not_end_the_accept_loop`.

It re-runs this test binary in a **private child** under `ulimit -n 512` and asserts the child got
that limit, so a failed `ulimit` cannot pass vacuously. `ulimit` inside that one shell moves
nothing outside it: no global change, no process-wide `setrlimit`, no sibling test affected.
The parent holds `exclusive()` so its own spawn cannot land in another test's flood.

**Fails before the fix, 5 of 5:**

```
cowfs-nfs: the NFS accept loop stopped, the port no longer accepts: Too many open files (os error 24)
called `Result::unwrap()` on an `Err` value: Os { code: 61, kind: ConnectionRefused, message: "Connection refused" }
test result: FAILED. 0 passed; 1 failed
```

**Passes after the fix, 10 of 10** (5 through the parent, 5 driving the child directly):

```
flood opened 436 sockets and stopped on errno Some(24), 0 of 512 descriptors in use
after the flood subsides: 13 descriptors in use
test result: ok. 1 passed
```

Deterministic across 10 runs: 436 or 437 sockets, always `EMFILE`, 13 descriptors after release,
a new client served every time.

Note that `open_fds()` cannot be measured at the moment of exhaustion, because opening `/dev/fd`
is itself a descriptor. That is why the test reports `0` there. It uses a best-effort count for
that one reading and leaves the panicking `open_fds()` that `affordable()` depends on alone.

## What was not established about EBADF

**UNCONFIRMED: that the errno was EBADF (9).**

Every reproduction of the reported call site on this machine produced errno 24 (EMFILE), never 9.
170 spawn-at-exhaustion probes of the identical `Command::output()` call: all failures were 24.
The exhausted-flood mechanism behind the reported panic is confirmed and the call site matches, so
the cause is the descriptor budget either way, but the specific number 9 was not reproduced here
and no mechanism for producing 9 at that call site was found. Both are failures of the same
`spawn` under descriptor exhaustion; whether macOS reports one or the other depends on where inside
the spawn the allocation fails.

**CONFIRMED independently of the errno:** the server defect in Part 2, which is the part worth
fixing, and which the reported panic was concealing.

## Environment notes

- No shared daemon, store, mount, socket or lease was touched. The work is a `cargo test` binary on
  ephemeral ports and a private child process.
- No signal was sent to any process. The only `kill` in the spike scripts is `perl -e 'alarm'`,
  which signals exactly the exec'd pid and never a process group (macOS has no `timeout`).
- No global `ulimit` was modified. Every limit change is inside a one-off `/bin/sh -c` subshell.
- Artifact paths: `bench/out/nfs57/` (gitignored), holding the scan table, the per-limit logs, the
  instrumented concurrent-run logs, and the two standalone probes
  (`spawn_errno_probe.rs`, `accept_errno_probe.rs`).

## Suite state at the tip

`cargo test -p cowfs-nfs -p nfsserve`: all green, every binary, no ignored test newly failing.
`cargo fmt --check` clean, `cargo clippy --all-targets` clean for both crates.