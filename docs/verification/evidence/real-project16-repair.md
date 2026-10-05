# Evidence: real-project acceptance harness, mode (b) treehouse slots

Lane: `verify/treehouse-real-project-16`, lease `cowfs-7c1bf8/4/cowfs`.
Report: `docs/verification/ready-real-project.md`, which is the document to read.

Warm-base acceptance for mode (b) is **NOT met (NONACCEPTED)**, and nothing in this file or in the
harness claims otherwise. Tracking issues 15 and 16 stay open. This file records what was executed,
what was refused, and what is unverified.

## Commit history of this lane's branch, and what each commit changed

| Commit | Content |
| --- | --- |
| `1ca8242` | the harness as first written |
| `1b1f2e1` | safety and honesty repair of the harness |
| `4a70c53` | the report rewrite: this head's numbers, the chain, the measured limits |
| the N1 to N7 repair | the harness repairs this file documents, plus this file |

The carry from `1b1f2e1` to `4a70c53` is **docs-only and structural**: the test file is
byte-identical at those two commits, so a runtime measured at `4a70c53` is measuring `1b1f2e1` code.
That is stated here because a reader must not treat a `4a70c53` runtime as a different program.

## Blocker chain, in the order it has to be broken

1. **`can_ingest` gate, the blocker.** `handler.rs` `base_refresh` refuses on the core because
   `CoreBackend::ingests_directories()` is `false` by design. Nothing is published.
   Real receipt: exit 1, `unsupported: this backend stores snapshots as trees, not as directories`,
   `{"snapshots":[]}`, no git worktree left behind. A provenance fix alone cannot unblock this.
2. **Worktree path.** `import.rs` parsed the checkout from `git worktree add` stdout, which is
   `HEAD is now at <sha> <subject>` on git 2.56.0 and empty with `-q`. Neither is a path, and both
   forms leak a worktree at `<repo>/<sha>`. Repair is `b59bc3c`, unmerged, not an ancestor of this
   head. `e243cb1` is a documentation retraction and is not the repair.
3. **Companion side.** `CowfsMaterialiser` never calls `mount_snapshot`, though the daemon implements
   it behind `--export-root`. Exactly one transition is unsupported.
4. **Provenance.** Reachable only once 1 publishes. At this head `base_commit` is empty and `fresh`
   is false.

## Commands and real exits

```
cargo fmt --all --check                                  exit 0, 0 diff lines
cargo clippy -p cowfs-treehouse --tests                  0 warnings
cargo test -p cowfs-treehouse --test real_project_acceptance --no-run
                                                         0 errors, 0 warnings
```

Executed per test, each once on the repaired tree:

```
every_implemented_mode_b_postcondition_holds_over_the_real_core          exit 0    14.7s
a_real_project_builds_and_tests_inside_an_exported_slot_snapshot         exit 0  151.9s
native_control_builds_and_tests_the_sample_project                      exit 0   94.1s
the_core_daemon_refuses_base_refresh_and_publishes_nothing              exit 0    4.8s
the_companion_never_calls_the_mount_snapshot_the_daemon_provides        exit 0    6.0s
a_published_warm_base_must_be_discoverable_with_its_provenance          exit 0    4.8s
git_never_prints_the_worktree_path_this_codebase_parses                 exit 0    1.5s
the_cache_hook_is_installed_and_read_back_from_the_real_config          exit 0    0.1s
six safe controls, no mount and no daemon                                all exit 0
warm_base_acceptance_over_a_real_core                                    ignored, never executed
```

Default tests in the file 15: 13 executed and asserted for real, 0 capability skips, 1 ignored.
The receipt file is append-only across several runs, so its 42 rows are not a count of tests; 37
carry `outcome: "measured"`, 5 carry `outcome: "cleanup"`, and 0 lack an outcome.

The two expensive builds ran while the repair was still uncommitted, so their receipts name the
previous commit. The difference is one test file and one document, with no production source change.
That is stated, not assumed.

## Old fail, new pass

| Defect | Old behaviour | New behaviour | Control |
| --- | --- | --- | --- |
| N1 mount grammar | Linux `type` read as the filesystem type, `ubuntu-latest` red | both grammars decoded exactly, 16 synthetic cases, escapes decoded, 4 malformed shapes refused | `the_mount_grammar_of_both_platforms_is_decoded_exactly` |
| N2 unbounded drain join | **20,012ms** against a 3s bound | **3,005ms**, classified incomplete drain, child's real exit status kept | `the_bounded_runner_finishes_inside_its_bound_for_every_child_shape` |
| N3 start time discarded | parsed then dropped, pid identity by argv alone | start time registered and compared; same argv with a different start refused | `a_recycled_pid_with_the_same_argv_is_refused` |
| N4 vacuous store check | `ends_with("")` is true, so an unanswered status passed | `Result`, empty refused, exact canonical equality, no prefix or basename arm | exercised by the two gated tests that read the answering store |
| N5 guard could not fire | guard compared a JSON boolean, writer emitted strings | typed writer, both shapes refused, five non-claims not read as claims | seeded boolean claim exit 101; seeded string claim exit **101**, previously 0 |
| N6 rows not uniform | 5 teardown rows had no `outcome` | every row carries one; a seeded undescribed row is refused | seeded no-outcome row exit 101 |
| N7 stranded daemon | undisclosed | disclosed below with its preserved receipt | this section |

## Child shapes, measured, against a 3s bound

| Shape | Result |
| --- | --- |
| chatty, 4 MiB on stdout | exit 0, 4,194,304 bytes, 46ms |
| empty stdout, exits at once | exit 0 |
| hangs | killed at its own pid, 3001ms |
| exits at once, helper holds the pipe | 3005ms, incomplete drain, child status preserved, helper cleaned and the cleanup asserted |

The control's helper is cleaned by the pid it recorded about itself, verified to be exactly
`sleep 20`; if that verification or the cleanup fails, the test fails. A control cannot leave an
orphan and still report success. One earlier revision of this control used a subshell, so `$!` was
the subshell and the sleeper was orphaned; the orphan exited on its own and was verified gone.

## The stranded daemon, disclosed

A real `cowfs-daemon` (pid 2875) on a real mount was left running with its store locked. The receipt
is preserved byte for byte at `bench/out/ready-real-project/acceptance-pre-fix.jsonl`,
sha256 `0309db70e65a46f3ab4da572f663732cd948726272ddcb1299dd77aa86666a5b`, 15,243 bytes, 29 rows:

```
daemon_pid                        2875
daemon_exited                     false
signalled_after_identity_check    false
unmounted                         ""
mounts_left_listed                unknown: deadline already spent before spawning mount
quarantined                       mount table unknown: deadline already spent before spawning mount
runtime_root                      /var/folders/.../T/.tmp53obIz
```

- The harness behaved correctly: it had spent its teardown deadline, could not read the mount table,
  and so unmounted nothing, signalled nothing and deleted nothing. Fail-closed, by accident, on a
  real daemon.
- **Why the deadline was spent is UNKNOWN.** The unbounded pipe join is a candidate, being the one
  unbounded path in `teardown`, but other calls had caps too and the receipt does not say which one
  consumed the time. **Hypothesis, not finding.**
- **Cleanup was done by this lane, by hand, from a shell, not through the harness. There is therefore
  no harness receipt for it, and none is invented here.** Steps: read the native mount table and find
  the mount listed; confirm by `ps` that the pid's argv is this lane's binary with this lane's store
  and socket; unmount that exact path; re-read the pid's identity and terminate it; remove the
  private runtime root only after the table was confirmed to hold nothing under it. Each step is
  **UNVERIFIED by any receipt this lane produced.**
- **That the pid is gone now proves nothing about the above.**
- The file is a hand-assembled mixture: of its six teardown rows, five carry the pre-repair schema
  and the pids of the `1ca8242` run, and only the sixth is from the repaired code. Preserved rather
  than rewritten.

## Teardown and host safety

All five private daemons in the recorded runs exited on their own after a control-plane shutdown.
None was signalled. Every teardown recorded an empty `mounts_left_listed` and an empty quarantine.
Nothing of this lane's was left mounted.

The shared daemon on this host was never signalled, unmounted or traversed, and its argv was
re-verified afterwards. No process group, no `pkill`, no `abort`, no recursive delete over an unknown
mount table, no real-path dead NFS mount.

## Identity of the binaries: artifact only

```
sha256 cowfs-daemon    b6b9970944b487249d3041f40ed78d6c8755116e3df19b7d7d953cf14b4eeb51
sha256 cowfs           59b88405ab1e3ad00ed242938684ae01f56fe2254bfa18e34a6fe7c445bb9f3c
sha256 cowfs-treehouse 65a42acb158c37e46b4a454057cd0d56bec7d90d8c7c5ef51dc4f1f0b15fab6e
```

These are the same three digests two independent reviews recorded. That is consistent with artifact
identity and is **not** evidence about any build: they were built once in this lease and reused, and
nothing binds them to a recorded source build. **Build provenance is UNVERIFIED.** They say which
bytes answered; they say nothing about new production source, and this branch changes none.

## Independent corroboration, carried with its own limits

An independent reviewer validated the small real Core chain, and a second reviewer measured the
reset convergence on it. Both are **source-bound historical evidence for an earlier head**, not for
this one, and neither is an acceptance.

The second reviewer also measured that a post-reset readback through a still-mounted export can lag
about two seconds while the store is already correct. That is recorded in the report as a limit of
that observation, not as a coherence guarantee. This harness unmounts and re-exports before reading
back after a reset, so it avoids the race and does not characterise it.

## Not touched

No production source. The companion, `import.rs`, the Core and meta seams, the holder lane and the
provenance seam belong to other owners. `main` has moved since this branch forked and its merge
changed production NFS code; a run on merged `main` is a different measurement.