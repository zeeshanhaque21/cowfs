# Proposal to treehouse: two lifecycle hooks and fd-aware lingering-process detection

To: kunchenguid/treehouse
From: the cowfs project (github.com/zeeshanhaque21/cowfs)
Against: tag `v3.1.0`
Status: proposal, not a fork. Nothing here is merged and nothing is on the critical path for
cowfs v1; see "Why this is not urgent" at the end.

## Summary

Three small changes, each independently useful and each backwards compatible.

1. A `pre_create` hook, so a provisioner can claim a slot's path **before** treehouse materialises
   a worktree into it. Today the only early extension point is `post_create`, which runs after the
   worktree and its content already exist.
2. A `pre_return` hook, so the work that a return is about to destroy can be dealt with first.
   Today there is no hook at all around the release path.
3. `FindProcessesInWorktree` should treat an open file descriptor or a `flock` inside the worktree
   as a lingering process, not only a working directory inside it.

Change 3 is a bug report as much as a feature request.
`docs/spikes/5-treehouse-process-detection.md` in cowfs has the measurements; the short version is
that a process which chdir'd out of a slot but still holds an open file or a flock in it is
invisible to `treehouse return`, on native disk and on any mount.

## 1. `pre_create`

### Motivation

Treehouse materialises a slot like this (`internal/pool/pool.go`, `AcquireWithOptions`):

```
choose slot name -> MkdirAll parent -> PruneWorktrees -> AddWorktree -> seedWorktree
  -> record entry with a provisional lease -> persistState -> [apfsSharing] -> markAcquired
  -> persistState -> hooks.Run(postCreate)
```

An external tool that wants to own the storage under a slot has no seam before step
`AddWorktree`. It gets `post_create`, at the very end, when the worktree is already fully
materialised on whatever filesystem the pool root happens to be on. For a block store that means
the expensive part, copying a warm build tree into the slot, has to happen in `post_create` instead
of being an O(1) clone.

`apfs_sharing` is the existing answer to the same problem, and it is instructive: it runs at the
right point, on fresh allocations only, and re-materialises the slot's tracked files as APFS clones
of the main checkout's. Two limitations make it unusable as a general seam: `internal/fileclone` is
build-tagged `_darwin.go` and `sharing_other.go` returns the reason "requires macOS APFS", and
there is no way to point it at another mechanism.

A `pre_create` hook turns that special case into a general one.

### Interface

Add `pre_create` to the existing `Hooks` struct, alongside `post_create` and `pre_destroy`:

```go
type Hooks struct {
    PreCreate  []string `toml:"pre_create,omitempty"`
    PostCreate []string `toml:"post_create,omitempty"`
    PreDestroy []string `toml:"pre_destroy,omitempty"`
}
```

```toml
# ~/.config/treehouse/config.toml
[hooks]
pre_create = ["/usr/local/bin/my-provisioner"]
post_create = ["./scripts/setup-venv.sh"]
pre_destroy = ["./scripts/teardown.sh"]
```

Semantics, matching the existing hooks as closely as possible:

- **Where it runs.** In the slot's **parent** directory, the directory treehouse just created with
  `os.MkdirAll(filepath.Dir(wtPath))`. `post_create` runs in the worktree itself, which does not
  exist yet at `pre_create` time.
- **When it runs.** After the slot name and path have been chosen, after `PruneWorktrees`, and
  **before** `AddWorktree` and `seedWorktree`. The provisional lease is already recorded, so an
  interrupted `pre_create` still leaves a slot that a later `get` will not hand out.
- **Environment.** The existing `TREEHOUSE_*` overrides, plus, for this hook only:

  | variable | value |
  |---|---|
  | `TREEHOUSE_SLOT_NAME` | the slot name, as `treehouse status` prints it |
  | `TREEHOUSE_SLOT_PATH` | the absolute path `AddWorktree` is about to use |
  | `TREEHOUSE_POOL_DIR` | the per-repository pool directory |
  | `TREEHOUSE_REPO_ROOT` | the main repository root |
  | `TREEHOUSE_BASE_BRANCH` | the base branch this acquisition is cutting from |

- **Exit status and claiming.** If the command exits non-zero, treehouse logs it, as it does for
  every other hook, and proceeds with `AddWorktree` exactly as today. To claim the path, the
  command exits 0 **and** leaves a file named `.treehouse-provisioned` in
  `TREEHOUSE_SLOT_PATH`. When both are true, treehouse **skips `AddWorktree` and
  `seedWorktree`** and continues to handoff: `apfs_sharing` is skipped, `post_create` still runs.
- **Anything else.** A missing marker file, an empty slot path, or a non-zero exit all mean
  "not provisioned", and treehouse behaves as if the hook were not configured.
- **The marker is left in place.** It is what `post_create`, a later `return`, or a human reading
  the slot can key on to know the slot was provisioned externally. Treehouse does not delete it
  and does not treat it as untracked clutter: like `.git`, it is a dotfile in the worktree root.

A marker file rather than a magic exit code, because a marker survives for inspection. A crash
between provisioning and the end of the acquisition is then visible in the slot instead of being
invisible in a log.

### Why it is backwards compatible

- A new key in a struct that already has `omitempty`. A config that does not set it decodes to an
  empty slice, and `hooks.Run` on an empty slice is already a documented no-op with no output
  (`internal/hooks/hooks_test.go`, `TestRun_EmptyListIsNoop`).
- `pre_create` is read from the **user** config only, like the other two hooks, so running
  treehouse in an untrusted clone can neither execute a checked-in command nor make treehouse skip
  materialisation. The existing repo-level discard in `config.Load` needs no change: it clears the
  whole `Hooks` value.
- No change to `treehouse-state.json`. The schema version stays 4 and no field is added. A slot
  provisioned externally is an ordinary entry, which is what a later `get`, `prune` or `destroy`
  should see.
- `apfs_sharing` keeps its current meaning and its current position. It becomes one of two ways to
  materialise a fresh slot, and the two do not combine.

### What it is not

It is not a general plugin interface. Treehouse still does the git work: a provisioner provides
storage and content, and `AddWorktree` still runs unless the provisioner claims the path. A
provisioner that claims the path is responsible for the `.git` file and for
`{main}/.git/worktrees/{slot}/`, which is a git invariant, not a treehouse one.

## 2. `pre_return`

### The seam already exists

This is smaller than it looks. `internal/pool/pool.go:1047` `ReleaseConditional` already takes a
`beforeReset func() error` and `cmd/get.go:251` already passes `killLingeringProcesses` into it,
inside `ReleaseConditional`'s `beforeReset` position, under the same pool state lock and
immediately before the destructive reset. The lock is what makes it valuable: a writer cannot slip
in between the emptiness check and the reset.

So `pre_return` is mostly a matter of wiring that existing parameter to a hook list, the way
`AcquireWithOptions` already wires `postCreate`. No new transaction, no new lock, no new ordering to
reason about.

### Motivation

`treehouse return` destroys the worktree's content: `git reset --hard` and `git clean -xdf`. For
a slot that holds a large build tree this is the expensive part of the operation, and it removes
the content a later acquisition would have reused. A tool that can replace that content in O(1)
has to do it **before** the destructive reset, and today there is no hook at that point.

`pre_destroy` is close but not equivalent: it runs only for `destroy` and `prune`, never for
`return`, and it runs after treehouse has already decided what is removable.

Without the hook, the cowfs companion has to swap the slot's snapshot to an empty one *before* it
calls `treehouse return`, which works but leaves a window: between the reset and the release, a
concurrent `get` can observe a slot that is empty and not yet returned. The window is small and the
release is pinned with `--if-lease-id`, so it is not a correctness hole today. It is the reason the
hook is still worth asking for.

### Interface

```go
type Hooks struct {
    PreCreate  []string `toml:"pre_create,omitempty"`
    PostCreate []string `toml:"post_create,omitempty"`
    PreReturn  []string `toml:"pre_return,omitempty"`
    PreDestroy []string `toml:"pre_destroy,omitempty"`
}
```

- **Where it runs.** In the worktree directory.
- **When it runs.** Inside `returnWorktreeToPool` (`cmd/get.go:251`), as a step before
  `killLingeringProcesses`, still under the pool state lock and still inside the release's
  `beforeReset` position. That placement matters: it is the window in which treehouse has decided
  to release this slot and has not yet destroyed anything, and it is already fenced by the same
  lock the emptiness check uses.
- **Environment.** As for `pre_create`, plus `TREEHOUSE_LEASE_ID` and `TREEHOUSE_LEASE_HOLDER`
  when the slot is leased, so a hook can key on the lease identity the release is pinned to.
- **Exit status.** Logged and non-fatal, exactly like every other hook, and for the same reason as
  every other hook: a hook must not be able to turn "return this slot" into "do nothing", because
  that failure would be invisible in the pool's state. A return the operator asked for either
  happens or fails.

Deliberately **not** claiming semantics, unlike `pre_create`. A return either happens or it does
not, and a hook that could stop one would make `treehouse return` unreliable in a way that is hard
to debug.

### Why it is backwards compatible

Same argument as `pre_create`: a new optional key, an empty slice by default, no state change, no
change to the release transaction. A hook that hangs delays a return, which is the one risk worth
documenting, and the reason the documentation should say hooks are expected to be short.

## 3. fd and lock aware lingering-process detection

### The problem

`internal/process/detect.go` `FindProcessesInWorktree` keeps a process only when `p.Cwd()` resolves
inside the worktree. A process that has chdir'd out but still holds an open file, or holds a
`flock` on a file in the worktree, is not detected.

On native disk the consequence is a slot that is returned while a live writer still has it open.
On a network mount it is worse: the client's unlink of the held file becomes a silly-rename to
`.nfs.<id>` inside the slot, `return` still exits 0 and prints that it returned the worktree, and
`treehouse status` then reports the slot as `dirty` for good, because the next `get` skips dirty
slots and takes another one. Plain `umount` can also start failing with "Resource busy".

### Measured, in cowfs spike 5

treehouse v3.1.0, macOS 26, one run per case, so timings are indicative:

| case | native | NFS mount |
|---|---|---|
| cwd at slot root | pass | pass |
| cwd in a nested subdirectory | pass | pass |
| chdir'd out, open fd on a file in the slot | not detected | not detected, slot left dirty |
| chdir'd out, `flock` on a file in the slot | not detected | not detected, slot left dirty |
| setsid child with cwd in the slot | pass | pass |
| setsid child with cwd outside the slot | child survives | child survives |
| ignores SIGTERM | pass, escalates to SIGKILL after 2.5 s | pass, 2.34 s |
| clean return, then a second `get` | pass, same slot | pass, same slot |

Cost of the scan, because a slower return is a real cost: `lsof +D` over a 10,000-file tree was
0.41 s to 0.43 s on native disk and 0.67 s to 0.86 s on the mount. A direct
`proc_pidinfo(PROC_PIDLISTFDS)` walk was not timed, but the analogous
`PROC_PIDVNODEPATHINFO` walk that treehouse already does measured at a median of 6.5 to 7.6
microseconds per pid over 200 lookups, so a per-pid fd walk is likely well under a millisecond for
a typical process table.

### Proposed change

Extend `FindProcessesInWorktree` to also match:

- any open file descriptor whose resolved path is inside the worktree, and
- any `flock` (or `fcntl` byte-range lock) held on a file inside the worktree,

in addition to the existing cwd match. Report the match kind alongside the process, so a message
can say which hold was found.

Two details worth preserving from the existing code:

- **Protected processes still apply.** `filterProtectedProcesses` drops the caller and its whole
  ancestry, and that must extend to the new hold kinds, so a return never signals itself or its
  parents.
- **Do not match the worktree's own git internals by accident.** A `git` process treehouse itself
  started, or a shell the operator left in a subdirectory, already matches on cwd. A process that
  has merely opened a file for reading and exited is not a writer; a cheap narrowing is to keep the
  existing behaviour and add the new kinds, then rely on `filterProtectedProcesses` and the
  post-termination re-scan, which is the safety net that already exists.

If a per-pid fd walk is judged too slow on some platform, `lsof +D` is the fallback and the numbers
above say it is affordable: under a second for a 10,000-file tree.

### One detail worth copying: an unanswered scan is not an empty one

cowfs has since implemented this detector for its own slots, and the failure mode worth avoiding
upstream is the quiet one. `lsof` can be absent, can fail, and can hang on a wedged mount, and a
scan that reports "nothing found" in any of those cases hands the caller a false all-clear that
stops exactly where it should have refused. The implementation separates the two answers:

- `lsof` exiting 0 or 1 is an answer. Exit 1 is its "matched no file", which is the common clean
  case and must not be treated as a failure.
- A missing binary, any other exit status, a scan that outran its deadline, or an unreadable
  `/proc` or `/proc/locks` is `unavailable`, and the caller blocks rather than proceeding.
- The child's pipes are drained off the thread that waits for it. Reading a pipe to end before
  checking the deadline makes the deadline unreachable, so a wedged mount hangs the caller instead
  of being reported.

The same applies to `/proc/locks` on Linux: an unreadable lock table is not an empty lock table, and
a lock alone is enough to block an unmount.

## Why this is not urgent, and what happens if it is declined

cowfs does not need any of the three to ship. It needs a `cowfs mount_snapshot {name, path}`
control operation, which is entirely on the cowfs side. What the hooks would buy:

- `pre_create` saves one throwaway `git worktree add` per acquisition. Today cowfs' `post_create`
  approach creates the slot worktree into an empty snapshot and then resets the snapshot to the
  warm base, which is one O(1) reset plus one small file write. With `pre_create` the throwaway step
  disappears.
- `pre_return` would move the O(1) reset inside treehouse's locked release, closing a window in
  which the slot has been emptied and not yet repopulated. Today cowfs' `return` wrapper does the
  reset first and then calls `treehouse return`, which is safe because the reset is atomic under the
  daemon's lock and the release is pinned with `--if-lease-id`.
- fd detection is what cowfs wants for **its** slots, and gets from its own `ps` control method,
  which reports cwd, fd and lock holds. The upstream change is for native-disk users and for the
  general case.

So the proposal is offered because the three changes are small, generally useful, and remove real
footguns, and because the design explicitly says to propose upstream before forking. If it is
declined or ignored, cowfs ships the `post_create` hook and the `return` wrapper, and no fork is
needed.
