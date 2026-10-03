# cowfs-nfs hardening test regressions (#54, #57, #66)

## Root cause

The NFS server runs inside the test process.
`hardening.rs` ran 21 tests as threads of that one process.
Four of them measure a process-wide resource: the descriptor table or the resident set.
Each one sized itself against the whole machine and ignored its neighbours.

- The flood and half-sent-frame tests each opened `min(want, (fd_limit - 120) / 2)` sockets at the same time, so together they needed about twice the table.
- `fd_limit()` spawned `sh -c 'ulimit -n'` in the middle of the flood, when the table was nearly full, and unwrapped the spawn.
- `half_sent_frames_keep_memory_bounded` and its siblings read the RSS of the whole process while other test threads allocated.

## Evidence (before, at main ab99868)

All logs are under `bench/out/nfs-regressions/` (gitignored).

| run | result |
| --- | --- |
| `hardening`, `ulimit -n` 1048576, 3 runs | 3 of 3 pass, so the default limit does not show it |
| `hardening`, `ulimit -n` 256 (the macOS terminal default), 4 runs | 4 of 4 fail: the flood panics at `fd_limit()` (`hardening.rs:50`) and half-sent panics at `connect_raw` (`hardening.rs:25`) |
| `hardening`, `ulimit -n` 512, 600, 700, 800, 900 | each fails; 1000 and 1024 pass |
| half-sent alone, 5 runs | RSS grew 17 MiB each |
| half-sent with `the_reply_cache`, `oversized`, `absurd` as neighbours, 5 runs | RSS grew 27, 27, 29, 27, 31 MiB |
| whole `hardening` binary | RSS grew 23 to 25 MiB, and the reply-cache test measured 28 of its 40 MiB bound |

The same test, server and sockets cost 17 MiB alone and up to 31 MiB with neighbours.
That is the ambient load in #66.
It comes from other threads in the same process, not from other binaries, which cargo runs one at a time.

EBADF (`Os { code: 9 }`) from #57 was not reproduced.
Every reproduction here was `EMFILE` (24), `ECONNREFUSED` (61) or `ECONNRESET` (54), from the same `fd_limit()` line and the same exhaustion.
The reporter's `ulimit -n` for #57 is not recorded, so the EBADF variant is unconfirmed.

## Fix

The four tests moved to `tests/resource_bounds.rs`, a binary of its own, where each takes a process-wide lock.
Nothing else runs in that process while one of them measures.

- `fd_limit()` reads the soft limit once, before any flood, and never panics.
  An unreadable limit is reported and replaced by 256.
  `unlimited` is read as a large number.
- The flood and half-sent tests size themselves from the descriptors still free: `limit - open - 32`, divided by two (client and server side).
- The flood fails with a message if the limit leaves room for no more than `max_connections` sockets, and fails if fewer than that many connected.
  The cap is therefore always exercised, never skipped.
- Half-sent frames take the peak RSS over a 500 ms window and bound it per connection, at a quarter of the declared 1 MiB frame, instead of 60 MiB for the process.
- `rss_bytes()` panics if `ps` cannot be read.
  It used to return 0, which passes every "stayed bounded" assertion vacuously.
- A control test allocates and touches 64 MiB and requires the probe to see at least 48 MiB.

## Evidence (after)

| run | result |
| --- | --- |
| `resource_bounds`, default limit, 10 runs | 10 of 10 pass; half-sent grew 15 to 18 MiB (75 to 90 KiB per connection) |
| `resource_bounds`, `ulimit -n` 256, 5 runs, plus `hardening` | 5 of 5 each |
| `resource_bounds` and `hardening` at 192, 256, 512, 900, 4096 | all pass |
| `resource_bounds` beside a concurrent `hardening` process, 3 runs | 3 of 3 pass, same RSS |
| `cargo test -j4 -p cowfs-nfs`, 3 runs | 3 of 3 pass |
| `cargo test -p cowfs-nfs` at `ulimit -n` 256 | passes |
| reply-cache test alone | grew 6 MiB, against 28 MiB with neighbours |

## Known floor

At `ulimit -n` 128 the flood test fails on purpose with `fd limit 128 leaves room for 41 flood connections, not more than the cap 64; raise it`.
`hardening` itself also fails there, because 17 tests run at once.
Both pass from 192 up.
