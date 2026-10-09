# Issue #121 spike: terminal-frame delivery at shutdown

Diagnostic spike for #121, run against PR #79's published head. Source read only: no production
edit, no test edit, no commit, no push. `crates/cowfs-ctl/src/server.rs` is byte-identical to what
CI ran.

## What this document is

A bounded, falsifier-driven spike following the five-step recipe in
`docs/reviews/control-shutdown77-fixture-final.md`. It answers two questions and refuses to answer a
third one it cannot support.

**Verdict: the #121 event is UNREPRODUCED.** A related, real failure of the same assertion was
reproduced, but with a different elapsed signature, and its cause is identified. The latent
teardown hole the review pointed at is confirmed by reading and is not reachable in any of the five
topologies probed.

#121 therefore remains a MERGE BLOCK. This spike does not clear it.

## Headline numbers

| | value |
|---|---|
| head under diagnosis | `4b210deea33f177daf2fa91950d4a3a28dcbb923`, unchanged |
| production source sha256 | `5eb3f6bcd092ec2a9fd69a3898b06060f5bd4fc8296bb4b0608afc3aa4a71664` |
| test blob sha256 | `d479840b78e6ea51eb0675f6ed1e75fb4923a4e3cae70d1bad12fe87aa173d17` |
| spike source | `bench/out/control121-spike/src/main.rs` |
| spike binary sha256, final build | `095a6c15dd3a6b9f3ae2602564cbf9854fa3c75f0b2ad9d9268ae09ec5ee9de9` |
| runs | 15, five labelled cases x 3 reps, no other repetition |
| cases where the terminal frame was lost | 1 of 15, case B rep 0 |
| raw bytes captured | 15 files, one per run, fsync'd before cleanup |

The spike is a separate cargo package with a path dependency on the lease's real
`crates/cowfs-ctl`. It uses only the public surface: `Server::start`, `ServerOptions`,
`ControlHandler`, and `ServerFrame::decode` on a raw `UnixStream`. No production hook, no scheduler
patch, no new assertion. Each run hashes the dependency's `server.rs` itself and records the hash in
its own trace, so no result can be attributed to the wrong source.

## Disclosure: three builds of one spike source, and only the last is hashable now

The 15 runs came from three builds of `src/main.rs`, not one binary. The final build, whose hash is
recorded above, produced the three case E runs. The single case A positive control and the twelve A to
D runs came from the two earlier builds. The differences between those builds were the per-run
artifact directory layout, and then the addition of case E and the `resume_delay_ms` parameter; the
measurement code, the handler, the read loop and the record set are the same in all three. The earlier
binaries were overwritten and their hashes were not retained, so the A to D runs cannot be tied to a
binary hash now. That is recorded rather than papered over: the source binding that matters is the
dependency's `server.rs`, hashed by the probe into every one of the 15 traces, and that is identical
across all 15.

## The original log is gone

The reported failure exists only as prose in `docs/verification/control-shutdown77-grace-repair.md`
and in a plan file. There is no raw run, no `PROGRESS77` line with context, no job log, and no record
of which machine or suite topology produced it. Nothing below reconstructs it, and nothing below
should be read as an explanation of it.

## Deduction from the test source, not from the lost log

`an_unblocked_client_still_receives_a_terminal_frame_at_shutdown` records
`terminal_present=false elapsed_ms=332`. That number constrains the mechanism more than it first
appears, and the constraint comes from the test's own loop:

```rust
let until = Instant::now() + Duration::from_secs(2);
while Instant::now() < until {
    match (&s).read(&mut buf) {
        Ok(0) => break,
        Ok(n) => { /* parse complete lines, set terminal on response|error */ }
        Err(_) => {}
    }
}
```

Three exits exist. The 2 s bound would report `elapsed_ms` near 2000. The panic inside the loop has
the message `terminal frame carries the request id`, not the message that actually fired. That leaves
`Ok(0)`, a bare EOF. So `terminal_present=false elapsed_ms=332` means **the peer received EOF at
332 ms and the terminal frame was neither complete nor present.**

This is a deduction from the committed test source plus the recorded number. It is not a
measurement, and the lost log would still be needed to confirm it.

## The reproduced failure, and why it is not #121

Case `B_slowclient_deadline300` rep 0. Verbatim from `trace.jsonl`:

```json
{"event":"case_start","label":"B_slowclient_deadline300","server_src_sha256":"5eb3f6bc…","shutdown_deadline_ms":300,"drain_deadline_ms":1500,"park_ms":400,"client_bytes_per_tick":65536,"client_tick_ms":100,"read_window_ms":2500,"steps_at_t0":2}
{"event":"shutdown_called","t0_ms":0}
{"event":"case_end","terminal":false,"terminal_kind":"none","terminal_len":0,"terminal_stream_offset":0,"terminal_fnv1a64":0,"bytes_read":1040384,"frames":0,"loop_ended":"eof","eof_at_ms":1814,"first_timeout_ms":null,"tail_len":1040384,"tail_head":"{\"event\":{\"done\":0,\"message\":\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx","wait_ms":2322,"wait_budget_ms":1800,"peer_eof_at_wait_return":true,"raw_fnv1a64":12875785374708713318}
{"event":"trace_closed"}
```

What the numbers say:

- `frames=0`. Not one complete frame was ever parsed.
- `bytes_read=1040384`, which is 1 MiB minus 4230 bytes: one 1 MiB progress frame, cut short.
- `tail_len=1040384` and the tail head is the opening of a progress frame. The client was holding a
  **truncated progress frame** when EOF landed, so the buffer proves truncation rather than absence.
- `eof_at_ms=1814`. `shutdown_deadline + drain_deadline` is 1800 ms, so the close came from the
  accept loop's bounded cutoff at `grace_end`, about 14 ms of poll slack past it.

So the assertion failed for real, with byte-level evidence, and the close was the documented bounded
cutoff rather than a mis-ordered teardown. The cause is arithmetic: this case throttles the client to
64 KiB per 100 ms, which is 0.64 MiB/s, so in the 1800 ms window it can drain about 1.15 MiB. The
queued backlog plus a 93-byte terminal frame does not fit in 1.15 MiB. The client lost the race
against its own drain rate.

**The signature does not match #121.** EOF at 1814 ms against a 1800 ms cutoff is the opposite end of
the window from the reported 332 ms. This is a different event until proven otherwise, and it is not
offered as the cause of #121.

## The falsifier pair that closes the competing hypothesis

The competing explanations were a client-side drain limit and a teardown-ordering defect. One variable
separates them: `shutdown_deadline`, with backlog and client rate held identical.

| case | deadline | drain | park | client | result |
|---|---|---|---|---|---|
| A published shape, positive control | 300 ms | 1500 ms | 0 | fast | terminal 3/3 |
| B hypothesis | 300 ms | 1500 ms | 400 ms | 64 KiB/100 ms | **terminal 2/3** |
| C falsifier for B | 5000 ms | 1500 ms | 400 ms | 64 KiB/100 ms | terminal 3/3 |
| D do-nothing baseline | 5000 ms | 1500 ms | 0 | fast | terminal 3/3 |

C is not a clean discriminator by itself, and saying so matters: C gives the same client 5000 ms
more deadline, so it also gives it more time to drain. C passing therefore does not isolate the
deadline. What the four together establish is narrower and is what is claimed: with the client rate
and backlog held fixed, the same source delivers whenever the window is long enough for that client
to drain, and loses the frame only when the window is too short for it. That is the drain
explanation. A teardown-ordering defect would lose the frame at a time unrelated to how much the
client managed to drain, and no case here shows that.

A is the positive control the recipe requires and it ran first, alone, before any timed variant: it
decoded a real terminal frame, 93 bytes at stream offset 1048576, which proves the probe, the socket
and the bound source were all genuinely operating.

## The terminal frame's byte identity

Every delivered case produced a byte-identical terminal frame. That is useful to anyone comparing a
`Partial` result against a `whole` one, including the reviewer's open disagreement at
`window450/resume340`, which this spike did not attempt to resolve:

| field | value, identical in all 12 delivered runs |
|---|---|
| length | 93 bytes including the newline |
| stream offset | 1048576, immediately after exactly one 1 MiB progress frame |
| fnv1a64 of the body | `18005200570233432349` |
| decoded as | `ServerFrame::Error`, `shutting_down` |

## The latent hole, confirmed by reading and not reproduced

The review's candidate 1 and 2 both point at real gaps. Quoting the source.

`Conn::finish` maintains a counter so teardown cannot observe completion before the terminal write
lands:

```rust
// server.rs:669-688
let mut inflight = lock(&self.inflight);
if inflight.remove(&id).is_none() { return; }
// Keep teardown from observing completion before the terminal write finishes, but do
// not hold the map lock across the write…
self.finishing.fetch_add(1, Ordering::SeqCst);
…
let ok = self.write_frame(frame);
self.finishing.fetch_sub(1, Ordering::SeqCst);
```

`inflight_empty` counts it (`server.rs:696`), and the accept loop's own worker sequences its writes
safely:

```rust
// server.rs:307-311
owned.abandon_inflight();
owned.kill();
```

`abandon_inflight` completes before `kill` runs, in the same thread, so that ordering cannot truncate
its own write. **But `kill` itself never consults `finishing`:**

```rust
// server.rs:761-765
fn kill(&self) {
    self.dead.store(true, Ordering::SeqCst);
    self.cancel_all();
    let _ = self.stream.shutdown(Shutdown::Write);
}
```

So when the request worker has already removed the id and is inside `write_frame`, the accept loop's
worker finds `inflight` empty, its `abandon_inflight` is a no-op, and its unconditional `kill`
half-closes the write side underneath a write in flight. The `finishing` counter that exists for
exactly this purpose is not consulted on that path. This is a real gap, established by reading, and
it is **not reproduced** here.

Why it was not reachable in five topologies: the terminal write and the blocked progress write are
gated by the same client drain. The flood handler holds the write lock until its 1 MiB write
completes, and its write completes when the client has drained the backlog. The terminal write starts
at that moment and needs 93 more bytes. So the interval during which a `kill` could land inside the
terminal write is bounded by how long 93 bytes take to hand to a socket the client is actively
draining, which is not reachable by moving the resume point. Case E exists to force exactly that
attempt and is reported below.

## Case E: the forced attempt at the 300 ms truncation

Case E parks to build backlog, then resumes reading at 280 ms, twenty milliseconds before
`shutdown_deadline` at 300 ms, and drains fast. If the flood handler returns inside the deadline, the
request worker owns the terminal write, the accept loop's worker finds `inflight` empty, and its
unconditional `kill` at 300 ms should truncate the frame. That is the 332 ms signature.

Result: **terminal delivered 3 of 3**, EOF at 670, 669 and 562 ms.

The mechanism that produced those EOF times is visible in the ordering: at 300 ms the flood was still
blocked, because the client had only just resumed and had not drained the backlog, so `inflight` was
non-empty, the accept loop's worker won the dedup itself, and its `kill` ran only after its own
`abandon_inflight` write completed. That is the safe ordering. Reaching the unsafe ordering needs the
handler to have returned before 300 ms, which needs the backlog drained before 300 ms, which leaves
no backlog for the terminal write to be slow behind. The window is self-closing.

**The `kill`-ignores-`finishing` hole is therefore reachable only under a condition this spike could
not construct from the public surface, and I am not claiming it is unreachable in general.** It needs
a terminal write that is slow for a reason other than a queued progress backlog, for example a socket
whose receive queue is full at the moment the terminal frame is written.

## A second measurement, not diagnosed

In case B, `wait()` took 2322 ms against a `shutdown_deadline + drain_deadline` budget of 1800 ms,
in all three reps. The other four cases were inside budget: A 404, 405 and 613 of 1800; C 5629 to
5634 of 6500; D 5227 to 5228 of 6500; E 1207 to 1214 of 1800.

This is measured, once, on the parked-then-slow-client shape. It is **not diagnosed and not claimed as
a budget defect**. `grace_end` is computed at the poll where `abandoned()` first becomes true, the
accept loop polls every 10 ms, and the budget then covers a worker join and `drain_and_close`; this
spike did not isolate which of those consumed the extra 522 ms, and the committed mixed-budget test
measures 811 ms against a 1050 ms budget on a shape where the client is not parked. The falsifier a
follow-up needs is the same case with the client reading fast: if the overrun disappears, it belongs
to the parked-client path; if it stays, it belongs to the join or the drain.

## Narrow fix plan, reported before any patch, as required

**For the reproduced flake, in `an_unblocked_client_still_receives_a_terminal_frame_at_shutdown` only.**
The test asserts a terminal frame while demanding that the client drain a 1 MiB progress frame
inside the grace, and its margin is therefore a function of host load, which is the whole sensitivity.
The narrow fix is to remove the load dependence from the fixture rather than to relax the assertion:
stream a bounded number of 1 MiB events, exactly the shape `FiniteFlood` already uses for the
late-window regression, so the client has a known, fixed amount to drain and the terminal assertion
measures delivery rather than drain throughput. The terminal assertion itself, the `assert_parked`
precondition, the `shutdown_deadline` and `drain_deadline` values, and every frame, EOF, budget and
ownership gate stay exactly as they are. Nothing in this plan touches production.

**For the `kill`-ignores-`finishing` hole, as a separate issue, not folded into #79.** The narrow
source change is for the accept loop's per-connection `kill` to respect the in-flight terminal write,
either by making `kill` a no-op while `finishing` is nonzero, or by letting the worker wait for
`finishing` to reach zero within the same `grace_end` it already has. Either is a few lines and both
need a test that forces a slow terminal write from something other than a queued backlog, which this
spike did not build. Filing it with the source locations and this spike's negative result is the
honest next step; patching it on the strength of a reading alone would be exactly the unproved-theory
patch the brief forbids.

## What is still missing

- **The original failing run.** Not recoverable. Every characterisation of the 332 ms event comes
  from the committed test source plus a prose summary, not from an artifact.
- **A construction that reaches the `kill`-versus-`finishing` race.** Five topologies did not reach
  it. The necessary ingredient is identified: a terminal write that is slow for a reason other than
  queued progress frames.
- **The 522 ms `wait()` overrun in case B.** Measured, not explained.
- **The reviewer's `window450/resume340` disagreement.** Not attempted. The byte identity of the
  terminal frame above gives a future comparison a fixed reference to check against.
- **No Linux run.** All of this is single-machine macOS on a loaded host.
- **No real daemon and no live Core.** The spike drives the public control server over its own Unix
  socket, with no filesystem daemon involved, so nothing here says anything about daemon shutdown
  ordering.

## Reproducing

```sh
cd bench/out/control121-spike
CARGO_TARGET_DIR="$PWD/target" cargo build
./target/debug/c121spike "$PWD/run" ../../../crates/cowfs-ctl/src/server.rs 3
```

The probe takes the artifact directory, the dependency source path to hash for binding, a rep count
and an optional single case label. Traces land in `run/<case>-rep<N>/trace.jsonl`, one JSON object
per observation, each appended, flushed and fsync'd as it happens, and captured bytes in
`run/<case>-rep<N>/captured.bin`, written and fsync'd before any cleanup including on the failure
path. The sockets live in this process's own short private directory under the system temp dir,
because the lease path is far past `SUN_LEN`; that directory is removed at exit. Per-item fsync is
what makes the traces usable after a failure rather than only after a clean return.

## Provenance

Lease 15, `/Users/zeeshanhaque/.cowfs/mnt/base/.treehouse/cowfs-7c1bf8/15/cowfs`. Spike sources and
all artifacts confined to `bench/out/control121-spike/**`. No production or test file was edited, no
commit, no push, no merge, no rebase, no reset, no stash, no lease return, no CI dispatch, rerun or
poll, no install, no sudo, no sysctl, no reboot. Reviewer reports read only and unmodified. The
shared daemon on this Mac, its store, socket and mount, all 32 leases and every other worker's owned
paths were untouched; no signal was sent to any process this spike did not start, and no mount was
walked. Shared PID 15263 was verified alive and unchanged at the start and left alone.