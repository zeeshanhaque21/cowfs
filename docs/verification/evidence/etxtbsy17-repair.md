# Issue 17 ETXTBSY: the test-fixture repair

The narrow, test-only repair of the ETXTBSY failure that blocked PR #92.
Diagnosis and the measurements this rests on are in
`docs/verification/evidence/etxtbsy17-spike.md`
(sha256 `72920d10dab70862212c1bd473ed322f0aa41aa4a359ac404e82f3b40bebf285`).

- Branch: `feat/linux-namespaces-17`, parent `78f19f86fc6530b23720bcb7a1e760c096b65657`
- Changed file: `crates/cowfs-treehouse/tests/canonical.rs`, and nothing else. 221 insertions, 20 deletions.
- No production code was touched. `mode_b.rs`, the daemon, the backends and the metadata are unchanged,
  and they are correct as they are: `run_build` is right to report a helper it cannot exec.
- The failing operation was never a cowfs failure. `run_build(dir, command, Option<&Canonical>)` builds
  no backend, opens no store and mounts nothing, and the stub lived in the runner's own `/tmp`.

## What changed

`write_stub` used to create the executable in this process and hand the path straight to `Command`:

```rust
let mut f = std::fs::File::create(path).expect("create the stub");
f.write_all(body.as_bytes()).expect("write the stub");
f.flush().expect("flush the stub");
std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod the stub");
```

It now writes from a short-lived child and does not return until that child is gone:

```rust
let mut child = Command::new("/bin/sh")
    .arg("-c")
    .arg("cat > \"$1\"")
    .arg("stub-writer")
    .arg(path.as_os_str())
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .spawn()
    .expect("spawn the stub writer");
let mut pipe = child.stdin.take().expect("the stub writer takes a pipe");
let written = pipe.write_all(body.as_bytes());
drop(pipe); // the write end closes here, which is what lets `cat` reach end of input
let out = child.wait_with_output().expect("wait for the stub writer");
```

Process exit releases the write-open on the target, so by the time the caller's `execve` lands the
inode has no writer left. Nothing is retried, serialised, slept, ignored or accepted as an error, and
no dependency was added.

The properties the brief required, and where each one lives:

| requirement | how it is met |
|---|---|
| nothing interpolated into shell source | the command is the fixed string `cat > "$1"`; the path is `$1`, an argument |
| body over stdin | the bytes go through the pipe, never argv |
| pipe closure handled | `drop(pipe)` before the wait, which is what lets `cat` see end of input |
| child failure and status handled | non-zero status asserts, and it is checked before the write result so the shell's own message wins over a bare broken pipe |
| wait before exec | `wait_with_output` returns only after the child is reaped |
| no writable descriptor left in the parent | the parent takes the pipe, so `Child` holds no stdin; the parent never opens the target for write at all |
| exact bytes preserved | the file is read back and compared to the input |
| executable permissions preserved | `set_permissions(0o755)` as before, then the mode is read back and asserted |
| public command behaviour preserved | `Canonical`, `args` and `run_build` are untouched; the stub bytes are byte-identical to before |

The wait is bounded in the only sense that matters here: the child's stdin is already closed, so `cat`
reads to end of input and `chmod` does not block. There is no polling loop and no timeout machinery,
because there is nothing to poll for.

The parent's `chmod` is a metadata call and opens nothing, so it is not a write-open. That was
deliberate: keeping it in the parent is a smaller diff and keeps the existing `#[cfg(unix)]` shape.

## Controls that discriminate

Four new tests plus one ignored control. All four plain tests run on both platforms.

| test | what would make it fail |
|---|---|
| `a_stub_is_byte_exact_and_executable_after_the_writer_exits` | a byte changed in transit, or a mode other than 0755 |
| `a_stub_path_is_never_shell_source` | the path reaching the command as source: the directory is named `bin ' quote; * $(exit 3)`, so an interpolated `$(exit 3)` would make the redirect fail and `write_stub` would panic before the assertions |
| `arbitrary_stub_bytes_survive_the_pipe` | any mangling of bytes: a NUL round trips, and metacharacters plus non-ASCII round trip and still execute |
| `a_stub_the_writer_cannot_create_is_refused` | a false pass: the target is under a directory that does not exist, so the child must fail, and the test requires the panic to name the writer and requires nothing to be left at the path |
| `write_stub_is_safe_under_concurrent_exec` (`#[ignore]`) | the defect itself: 8 threads x 400 rounds, each writing its own stub through `write_stub` and exec'ing it, asserting zero refusals and that every exec ran |

The NUL body is checked for round trip only and not exec'd, because a shell is not required to read a
NUL as an ordinary byte. Claiming otherwise would be a guess.

## The paired measurement, at the real seam

Two source trees that differ in exactly one function. `diff -rq` over the two trees reports one file,
and inside it only `write_stub`.

| | tree-new | tree-old |
|---|---|---|
| `crates/cowfs-treehouse/tests/canonical.rs` sha256 | `86b1b70f6c1f99b40455fc541c69171d105282459e795f651a33f7bc427ad634` | `4f063b2fbf5fdef8770315c51537e4d5edb36390e05bb8d74b5d9650dd2ed80a` |
| built | cold, `Finished in 57.61s` | cold, `Finished in 1m 00s` |
| test binary sha256 | `c8c6e32ab6f76171befc56759e1e5d2669d055929cc6696d0ff9aee59f212917` | `8c2df003f7224265a0f6140de875564b44bc342a9a850d55dbd6247ff34b7b16` |
| occurrences of the marker string `stub-writer` | 1 | 0 |

The marker count is the binding proof: the old binary does not contain the new writer, and the two
binaries are not the same file.

Same test, same host, same session, `moonscape`, Linux 6.12.109+rpt-rpi-2712 aarch64, 4 cores:

| writer | result | refusals |
|---|---|---|
| old, in-process `File::create` | **FAILED**, `assertion left == right failed: 47 of 3200 execs were refused after write_stub` | **47 / 3200**, 1.47% |
| new, waited-for child | **ok**, `1 passed; 0 failed` | **0 / 3200** |

47 in 3200 matches the 97 in 6800 pooled from the standalone probe, so the real seam reproduces the
probe's rate rather than something new.

## A method error, disclosed

The first attempt at the old arm was **invalid** and its numbers are not used anywhere.

It seeded tree-old's target directory with `cp -a` of tree-new's, to avoid a second cold build.
Cargo then satisfied the fingerprint against the copied absolute source path, which still pointed at
tree-new, never looked at tree-old's source, and did not rebuild: `Finished in 0.26s` with no
`Compiling cowfs-treehouse`. Both test binaries hashed `c8c6e32a...`, and the "old" arm had run the
new code.

This was caught by hashing both binaries and diffing them, which is why that step is in the script
rather than assumed. It is also exactly the stale-artifact hazard the brief named. The correction was
to delete the seeded directory and build tree-old cold; the cold build did recompile
`cowfs-treehouse`, and the two binaries then differed. Both the invalid run and the correction are kept
in the logs.

## The representative sample, before any batch

Run first, from the cold tree-new binary, before the control and before the full suite:

- The originally failing test, by exact name:
  `a_working_namespace_lets_a_failing_build_stay_a_failure` -> `1 passed; 0 failed; 17 filtered out`.
  That is the real path: `write_stub`, then `run_build`'s probe spawn, then the payload spawn, with the
  exit-77 collision still detected as an `Io` failure and not as an unavailable namespace.
- The four writer controls, each `1 passed; 0 failed; 17 filtered out`.
- The real `scripts/cowfs-ns-run.sh`, with the production argv shape and two distinct directories,
  running a real command in a verified namespace:

```
parent ns: mnt:[4026531841]
cowfs-ns-run.sh: mount namespace ready (unprivileged),
  /home/moonscape/etxtbsy17-repair/ns/src at /home/moonscape/etxtbsy17-repair/ns/canon
REAL_COMMAND_RAN
/home/moonscape/etxtbsy17-repair/ns/canon
inner ns: mnt:[4026534967]
made-here
helper_exit=0
```

The two namespace ids differ, so a namespace was really created, and the command really ran at the
canonical path. The `stub-writer` child had already exited before any of this, which is the ordering
the repair depends on.

Every phase asserts positively rather than by absence of output: the helper printed
`REAL_COMMAND_RAN` and its own namespace id, the tests report exact pass counts, and the control
reports a refusal count.

## Gates

| gate | host | result |
|---|---|---|
| `cargo fmt --all --check` | macOS, rustc 1.99.0 | exit 0 |
| `cargo clippy -p cowfs-treehouse --tests -- -D warnings` | macOS, clippy 0.1.99 | exit 0 |
| `cargo test -p cowfs-treehouse --test canonical` | macOS native | **14 passed, 0 failed, 0 ignored** |
| `cargo test -p cowfs-treehouse --test canonical` | Linux, the cold tree-new binary | **17 passed, 0 failed, 1 ignored**, 18 tests |
| `write_stub_is_safe_under_concurrent_exec --ignored` | Linux | 1 passed, 0 refused of 3200, twice |

The ignored control does not exist on macOS, because it is gated to Linux where the failure occurs.
The macOS run is the native baseline and it covers all four plain writer controls.

The 20 deletions and 221 insertions are one file. `git diff --name-only` returns
`crates/cowfs-treehouse/tests/canonical.rs` and nothing else.

## Residual risk, stated

- **The original CI event is still unreproduced.** It was never reproduced: that needs a dozen repeated
  runs of the real 13-test binary, which this budget excluded. What is reproduced here is the ETXTBSY
  refusal at the same seam, under the same concurrency, from the same writer.
- **0 of 6400 is not a universal zero.** The new writer was run twice at 3200 execs with no refusal.
  The old writer failed at 1.47% in one run of 3200, in the same session as one of those clean runs.
  That is a strong discrimination, not a guarantee.
- **The mechanism is still inferred.** The explanation is that the write-open is still in flight when
  the exec lands. It was inferred from measurements, not observed: no `strace`, no `sudo`, no kernel
  source read on the host. No kernel bug is claimed, and none is needed for the repair.
- **The runner's `/tmp` filesystem type is still unknown.** It is not recorded in the CI log. All Linux
  numbers here are tmpfs on a Pi.
- **The `spawn` shape of the repair is a fixture change, not a product guarantee.** Any caller that
  writes an executable and execs it immediately, from several threads, has the same exposure. cowfs
  does not write the helper, so the product's exposure stays low.
- **Independent review has not happened.** This is my own delivery. A fresh reviewer who did not write
  it must read the diff before merge.

## Status of the open items

- **ETXTBSY**: repaired in the fixture and measured old-fail new-pass at the real seam. Still a merge
  block until a fresh independent reviewer has read it. Not self-cleared.
- **#124** (`swap` replaces a tree without touching a base's record): open, unimplemented, untouched here.
- **#98** (warm base provenance): incomplete, as recorded in `base-provenance98.md`.
- **#17 acceptance**: open. Linux acceptance is still owed by `scripts/namespaces17-treehouse-linux.sh`.
- **`crates/cowfs-meta/src/tx.rs`** clippy discrepancy: other-owned, unresolved, not waived here.

## Artefacts

Under `bench/out/etxtbsy17-repair/`, which `.gitignore:10` keeps out of the branch:

| artefact | sha256 |
|---|---|
| `logs/mac-2026-10-05.log` | `965af9cfc96c812c860f40c04b59f79c60200cfbabef63621d5d45ee46b6d20a` |
| `logs/linux-2026-10-05.log`, includes the invalid old arm | `4b353fb23e38494a20e197e56b23cbcb7ed747810545acd9787480648811b73b` |
| `logs/linux-paired-2026-10-05.log`, the corrected pair | `e4125ab944d391eb055fa41f50416e4e1235efca51666025668254d7bdf3b091` |
| `linux-acceptance.sh` | `7a41fc49d4aea0e99d1efaa40b2569be3972557d2f853ab0507726c06d00fdb3` |
| `linux-paired-corrected.sh` | `239fac05018c08691801d90659fe6383a15f39197ef6872d1d24db10b4e315cd` |
| `trees.tgz`, both source trees | `ea750b0d2736be5f79a5b90adcf9b100e508572579292c50f634f6a785f986e5` |

Preserved unchanged from the diagnosis: the original CI log at
`bench/out/etxtbsy17-spike/logs/origin-ci-37244286403.log`,
sha256 `fa8af4b99b815f09b9ce50a413e2c4e5d51aafdb2eec02b211220ab71487d79b`, and the spike source and
its four run logs.

On the Pi, `/home/moonscape/etxtbsy17-repair` keeps both source trees, both run logs and both scripts,
22 MiB total. The two 356 MiB cargo target directories were removed after their binaries were hashed.
No daemon, mount, lease, shared process or borrowed pool was started, signalled, unmounted or cleaned.