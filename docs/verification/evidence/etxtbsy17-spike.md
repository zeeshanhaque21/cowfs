# Issue 17 ETXTBSY: boundary diagnosis

Read-only diagnosis of the ETXTBSY failure that blocks merging PR #92.
No production code was changed, no commit was made, and PR #92's head was not moved.

- Branch under diagnosis: `feat/linux-namespaces-17`, head `78f19f86fc6530b23720bcb7a1e760c096b65657`
- Canonical source SHA of the failure: `2c559ebef57c55d8e835e2cc6498668d4a47effa`, an ancestor of that head
- Verdict: **REPRODUCED in a standalone probe of the identical spawn seam. The original test-binary
  failure was NOT reproduced**, because reproducing that needs at least a dozen repeated runs of the
  real binary, which this budget excluded.
- Root cause confidence: **high on the trigger, medium on the kernel mechanism.** The trigger is
  measured. The mechanism is inferred from the measurements and is labelled as inferred throughout.

## 1. The original failure, from the immutable log

The saved CI log is preserved unedited at
`bench/out/etxtbsy17-spike/logs/origin-ci-37244286403.log`,
sha256 `fa8af4b99b815f09b9ce50a413e2c4e5d51aafdb2eec02b211220ab71487d79b`, 2078 lines.

| identity | value | how it was established |
|---|---|---|
| run | 37244286403, run_number 404, event `pull_request`, workflow `ci`, check_suite 100879196925 | `gh-axi api repos/zeeshanhaque21/cowfs/actions/runs/37244286403` |
| head SHA | `2c559ebef57c55d8e835e2cc6498668d4a47effa` | same call, `head_sha` |
| base at the time | `46b0f269d5bef4a2c204c25f5b3015da601d3beb` | same call, `pull_requests[0].base.sha` |
| job and step | `check (ubuntu-latest)`, `Run cargo test --workspace` | log header, every line is prefixed with the step |
| suite result | `test result: FAILED. 12 passed; 1 failed` | log |
| failing test | `a_working_namespace_lets_a_failing_build_stay_a_failure` | `test ... FAILED` line |
| panic site | `crates/cowfs-treehouse/tests/canonical.rs:365:5` at that SHA | log |
| ETXTBSY occurrences in the whole log | 1 | `grep -c 'Text file busy'` |

The verbatim assertion, which is the whole failure:

```
a payload exit of 77 must stay an Io failure, got Io("cannot run the namespace helper
/tmp/cowfs-canon-payload77-19079/bin/ns-payload77: Text file busy (os error 26)")
```

### Backend: neither Core nor Path

This is worth stating plainly because it removes most of the search space.
`run_build` has the signature `run_build(dir: &Path, command: &str, canonical: Option<&Canonical>)`
at `crates/cowfs-treehouse/src/mode_b.rs:638`.
It takes a directory and a canonical configuration.
It constructs no `Backend`, opens no store, starts no daemon, mounts nothing, and never calls
`snapshots()`.
The failing test's `slot` is a plain directory created by `Tmp::dir`.

So no cowfs filesystem, no FUSE, no NFS loopback, no redb, no Merkle tree, no snapshot is anywhere in
this failure.
The seam is process creation.

### Filesystem, export and mount parameters: none of them apply

`fixture_root()` at that SHA is `std::env::temp_dir()`, and `Tmp::new` builds
`fixture_root()/cowfs-canon-{tag}-{pid}`.
On the runner that resolved to `/tmp/cowfs-canon-payload77-19079/`.
The stub therefore lived in the runner's own `/tmp`, not on the cowfs mount, not on NFS, not in a
cowfs store.

The runner's `/tmp` filesystem type is **not recorded anywhere in the log** and is therefore unknown.
No mount option, export, or namespace parameter appears in the failure.

### The exact public operation

1. `canonical.rs:357` calls `write_stub(&p, &body)`, producing the helper path
   `/tmp/cowfs-canon-payload77-19079/bin/ns-payload77`.
2. `canonical.rs:364` calls `run_build(&slot, "exit 0", Some(&c))`.
3. Inside, `mode_b.rs:651-656` runs the **probe** spawn:
   `Command::new(&c.helper).args(c.args(dir, "exit 0")).output()`, and maps any error to
   `Error::Io("cannot run the namespace helper {helper}: {e}")`.

So the failing call is the namespace probe, the first of the two spawns `run_build` makes, and it
failed before any child of this test had run.

### Syscall boundary

`io::Error` with `raw_os_error() == 26`, `ETXTBSY`, produced by `Command::output()`.

The errno is reported by the child to the parent through the error pipe that Rust's process spawn
sets up, and the failing operation in the child is `execve` of the helper path.
Three things in the public evidence support `execve` and not process creation:

- `c.validate()` runs before the spawn, so the path was already accepted as a helper.
- The same `Command` on the same path succeeds in the overwhelming majority of runs, so the
  parent-side spawn path is not what fails.
- A missing helper yields `ENOENT`, 2, not 26. The path existed and was executable.

**There is no child PID to attribute.** The child failed at `execve` and never became a process, so
there is no pid, no start time, no `/proc/<pid>/exe` and no fd table for it.
The `19086` in the log is a test **thread** id.
No fd-owner evidence survives the failure post-mortem, which is precisely why a reproduction was
needed to observe it.

### `write_stub` closes the handle, from source

At that SHA, `canonical.rs:96-107` is:

```rust
{
    let mut f = std::fs::File::create(path).expect("create the stub");
    f.write_all(body.as_bytes()).expect("write the stub");
    f.flush().expect("flush the stub");
}
std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod the stub");
```

The `File` is dropped at the end of that block, at least two statements before `run_build` is called.
There is no open writer at spawn time, which rules out the "a writer still holds the file" reading on
its own.
`flush()` on a `File` is a no-op, so the sequence is one open, one `write`, one close, one `chmod`, and
then `execve`.

## 2. What was reproduced, and on what

The original failing operation is one `Command` spawn of a stub script that the calling thread has
just written.
`bench/out/etxtbsy17-spike/spike/spike.rs` does exactly that and nothing else: std only, no crate
dependencies, no cowfs build, no daemon, no mount.

Host: `moonscape`, `Linux 6.12.109+rpt-rpi-2712 aarch64`, 4 cores, `rustc 1.95.0`, `/tmp` on tmpfs.
This is a different architecture and a different filesystem from the failing CI runner, so it is a
reproduction of the *seam*, not of the *incident*.

Every arm is one shot, started by a `std::sync::Barrier` across its threads, and bounded.
Each arm either execs a file it has just written or execs a file nobody wrote.
On every ETXTBSY the spike records the errno, the path, whether this thread wrote that path, the
file size, and a `/proc` scan of every process holding an fd on the failing path with that fd's flags.

### Positive control, run before every arm

`CONTROL arm=phase0 exec-after-write=ok` in all four runs.
The real production helper was also exercised once, with the production argv shape and two distinct
directories, and ran the command inside a verified namespace:

```
cowfs-ns-run-probe.sh: mount namespace ready (unprivileged),
  /home/moonscape/etxtbsy17b/src at /home/moonscape/etxtbsy17b/canon
HELPER_RAN
/home/moonscape/etxtbsy17b/canon
helper_exit=0
```

The first attempt at that control exited 2 and was my error: I passed the same directory for `--src`
and `--canonical`, which the helper refuses at `cowfs-ns-run.sh:131-136`.
It is recorded rather than hidden.

### Results

| arm | what the exec target is | threads | execs | ok | ETXTBSY | other | run |
|---|---|---|---|---|---|---|---|
| `inplace` | `write_stub` shape: create, write, flush, drop, chmod | 8 | 400 | 396 | **4** | 0 | 1 |
| `inplace-pre` | same, plus a no-op `pre_exec` so std uses fork+exec | 8 | 400 | 398 | **2** | 0 | 1 |
| `pub` | tmp + rename + chmod, then exec | 8 | 400 | 397 | **3** | 0 | 1 |
| `serial` | `inplace`, one thread | 1 | 50 | 50 | 0 | 0 | 1 |
| `serial` | `inplace`, one thread | 1 | 400 | 400 | 0 | 0 | 2 |
| `bystander` | 4 writers write their own files, 4 execers exec a never-written file | 8 | 1600 | 1600 | 0 | 0 | 2 |
| `nowrite` | 8 threads exec files nobody writes | 8 | 3200 | 3200 | 0 | 0 | 2 |
| `inplace` | as run 1 | 8 | 3200 | 3145 | **55** | 0 | 3 |
| `wopen` | write-open, **zero bytes written**, close, chmod | 8 | 3200 | 0 | **4** | 3196 | 3 |
| `inplace` | as run 1 | 8 | 3200 | 3162 | **38** | 0 | 4 |
| `childwrite` | same bytes, written by a waited-for `/bin/sh` child | 8 | 3200 | 3200 | 0 | 0 | 4 |

`inplace` pooled across the three runs that measured it: **97 of 6800 execs, 1.43%**.

The `wopen` arm's 3196 "other" results are `ENOEXEC`, because a zero-byte file is not a valid
executable.
That arm is therefore not degenerate: it had 3200 real spawn attempts, 4 of which returned `ETXTBSY`
and the rest returned the expected "not an executable" error.

One earlier `bystander` arm is excluded from the table on purpose.
Its first run returned 196 `ENOENT` because the spike recycled each thread's directory at the end of
every round, deleting the never-written exec target.
That is a bug in my spike, not a result, and it is fixed in the version hashed below.

## 3. What is measured, and what is inferred

Measured, on this host, in these four runs:

- **Concurrency is necessary.** One thread: 0 of 450 execs. Eight threads writing and exec'ing their
  own files: 97 of 6800.
- **The target must have been write-opened.** Execing a file nobody wrote: 0 of 3200, with eight
  threads. Execing files while other threads write *different* files: 0 of 1600.
- **Writing zero bytes is enough.** `wopen` never calls `write`, only `File::create` and `close`, and
  still produced 4 ETXTBSY.
  So whatever is being refused is not dirty pages or writeback.
- **Atomic publication does not help.** Publishing by tmp + rename, so the inode being exec'd was
  never open for write by anyone at exec time, still failed 3 of 400.
- **posix_spawn is not the variable.** Forcing fork+exec with a no-op `pre_exec` failed 2 of 400, the
  same order as the posix_spawn path's 4 of 400.
- **No holder is normally present.** Of 68 inspected failures, exactly one showed any process holding
  the failing path.
  That one was a live `O_WRONLY|O_LARGEFILE|O_CLOEXEC` fd, pid 1382537 fd 3, which is exactly the flag
  set Rust's `File::create` uses.
  The holder was the writer itself, and it is usually gone by the time the failure is inspected.

Inferred, not measured:

- The consistent explanation is that the inode's write count is still non-zero when `execve` runs,
  because the final `fput` after `close()` is deferred, and the exec path's write-access check
  returns `ETXTBSY` immediately instead of waiting for that release.
  Concurrency widens the window because the deferred release competes with other threads' work.
- I did not read the kernel source on the box, did not install `strace`, and did not use `sudo`, so
  this mechanism is **inferred from the measurements above and not directly observed**.
- **No kernel bug is claimed.** `ETXTBSY` here is the documented refusal to exec a file with an open
  writer; the defect, if there is one, is in the test fixture's timing, not in the kernel.

## 4. Where the defect actually is

In the test fixture, not in the production code.

`mode_b::run_build` behaves correctly.
If the helper cannot be exec'd, that is a real failure and it should surface as an error.
The defect is that `canonical.rs`'s `write_stub` creates an executable in a directory shared by all
tests running in the same process and then spawns it microseconds later.

This is not cowfs-specific.
Any program that writes an executable and immediately execs it, from several threads, on one
filesystem, has the same exposure.
cowfs itself does not write the helper, so the production path has low exposure: the operator
creates the helper at least one command earlier.

## 5. Narrow fix plan, not implemented

Change `write_stub` in `crates/cowfs-treehouse/tests/canonical.rs` so the write-open is released by
process exit rather than by a deferred final `fput` in the test process: write the file from a
short-lived `/bin/sh -c` child and wait for it.

That is the `childwrite` arm, measured in run 4 against `inplace` in the same shot on the same host:
**0 of 3200 versus 38 of 3200**, with `childwrite` reporting 3200 successful execs, so the arm is
valid rather than degenerate.

Rejected alternatives, each with the measurement that rejects it:

| alternative | rejected by |
|---|---|
| publish the stub atomically by rename | `pub` still failed 3 of 400 |
| avoid posix_spawn | `inplace-pre` still failed 2 of 400 |
| serialise the tests or the runtime | not needed and not permitted; there is no cowfs race to serialise |
| retry on ETXTBSY, sleep, or treat ETXTBSY as unmeasurable | not permitted, and each would hide a real error |

Limits of the fix claim, stated plainly:

- Measured at the stub level, not at the test-binary level.
- One host, one run, tmpfs, kernel 6.12.109, aarch64.
- 0 of 3200 is not proof of 0.
  Before this is trusted, `--test canonical` should be run repeatedly on a Linux runner, which is
  exactly the repetition this budget excluded.

## 6. Reproducibility

Spike source, as run:

| run | sha256 of `spike.rs` | log | log sha256 |
|---|---|---|---|
| 1 | `a2cb24474d5fc53d87d89bc0d4632b4bcc21b9e5a098d2e9272774efcdbcb30f` | `run-2026-10-05-pi-tmpfs.log` | `6860b8ce9cc3d019eb2dec4bfb32a2d247b65558cc2912e431f90bc9b3e5ba79` |
| 2 | `8f76ebbd387de44a7b3ca168f7d450a169f3e9c5e429438e113482b2e1aed4f0` | `run-2026-10-05-pi-tmpfs-run2.log` | `c9ffbddaeb0cd1689e5ab9abcb475bc6198116994ff84fad7cc0fea42ca434f5` |
| 3 | `4bf80f866096697db6e98bcb32c92f83d8082c4a1cbc35ad0294dfe206ea4fff` | `run-2026-10-05-pi-tmpfs-run3.log` | `c6ccc896c2b9ae7339d8114e345331d910048cd3e9e0efc0ea98c17af184ad37` |
| 4 | `3346938ac7d93fa3604e170c7647a7c7e38beb659c45aa455c05e6c4bc3d7b12` | `run-2026-10-05-pi-tmpfs-run4.log` | `9674df17ca6b4ae32483e04f1d7e4dc433211694082b41f4993fc26cd1995d86` |

Run 4's log is summary-level for the `inplace` arm, because the remote command filtered its
per-failure lines out.
Its count comes from the `RESULT` line.
Run 3's log is full-detail for every failure.

Also preserved:

- `real-helper-control.log`, sha256 `76b0bd2ac8372263f93c9b7ef4f2a4bc7b46bf485027f540ecdc0c58291a3226`
- `origin-ci-37244286403.log`, the immutable original
- earlier probe artefacts from this work, with hashes in `prior-probe-artifacts.sha256`:
  `probe.rs` `3cef4ddbb0b9a2637356d1bb268c2d797eacac0b870e69925b2ed2ff8f499a6b`,
  `namespaces17-etxtbsy-loop.sh` `718e44d9755c0aa2f770987beab34dd9832a9ac1cd14aca58e09100a4c35377c`,
  `namespaces17-etxtbsy-repro.sh` `9959532d7ae158e2b5214862a8f4e0c7f2f5de12dad79bf8c4fe8109f18d9ce9`
- `scripts/cowfs-ns-run.sh`, sha256 `0b1fe1ee33375ebc10973bffb0e0e690063d83baa0b303aaddc568d4375d48ec`

Earlier in this work, on this same host, an ext4-backed directory was measured failing as well, at a
higher rate than tmpfs.
That measurement is recalled from the earlier session, is not among the logs above, and was not
re-measured here.

All artefacts live under `bench/out/etxtbsy17-spike/`, which `.gitignore:10` keeps out of the branch.
The only file written outside that directory is this report.

## 7. Status

ETXTBSY **remains a merge block for issue 17 and PR #92.**
This report characterises the failure and measures a candidate fix.
It does not repair anything, it does not claim the original incident is reproduced, and it is not
independent review.
The block lifts only after an actual repair lands and has been independently reviewed.