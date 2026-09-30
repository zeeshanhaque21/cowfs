# v1 treehouse integration

Issues: #15 (mode a), #16 (mode b), #20 (open-fd holders).
Contract for the protocol this document drives: `docs/v1-control-api.md`.
Design: `docs/design.md`, "Treehouse integration (day 1)".
Implementation: `crates/cowfs-treehouse/`.

## What treehouse 3.1.0 actually offers

Everything below was read from the source at tag `v3.1.0`, not from its documentation, and the
command surface was checked against the installed `treehouse` binary (v3.1.0) in a sandbox pool.

### Pool layout on disk

`internal/config/config.go`:

- `ResolvePoolRoot` (`config.go:252`): an empty root is `$HOME/.treehouse`. Any other root is
  environment-expanded, resolved against the repository root when relative, and then has
  `.treehouse` **appended**. So `--root /a/b` means the pool root is `/a/b/.treehouse`, not
  `/a/b`.
- `ResolvePoolDir` (`config.go:230`): the per-repository pool directory is
  `{pool_root}/{basename(repo_root)}-{short6}` where `short6` is the first 3 bytes of
  `sha256(git remote get-url origin)`, hex, falling back to `sha256(repo_root)` when the
  repository has no `origin`.
- A slot lives at `{pool_dir}/{slot}/{repo}` and `pool.IsPoolDir` recognises a pool by the
  presence of `treehouse-state.json`.

Verified in a sandbox: `--root <sandbox>/pool` produced
`<sandbox>/pool/.treehouse/{repo}-{short6}/1/{repo}`.

### Config keys (`treehouse.toml`, and `~/.config/treehouse/config.toml`)

`max_trees`, `root`, `base_branch`, `unique_leaf`, `worktree_path`, `apfs_sharing`, `vcs`,
`[hooks] post_create`, `[hooks] pre_destroy`.

Precedence for every key is flag, then environment variable, then repo config, then user config.
The environment variables are `TREEHOUSE_ROOT`, `TREEHOUSE_UNIQUE_LEAF`,
`TREEHOUSE_WORKTREE_PATH`, `TREEHOUSE_APFS_SHARING`, `TREEHOUSE_VCS`, `TREEHOUSE_LEASE_HOLDER`.

- `unique_leaf = true` names a new slot directory `{repo}-{slot}` instead of `{repo}`.
- `worktree_path` templates the whole path from `{pool}`, `{slot}`, `{repo}`, `{repo_parent}`.
  It must contain `{slot}` and one of `{pool}` or `{repo}`, must be absolute, must not be inside
  the repository, must not contain the pool directory, and if it is inside the pool it must be
  `{pool}/{slot}/<name>`. **A templated path that already exists is never adopted**
  (`internal/pool/pool.go:641-651`): the candidate is skipped and the next slot name is tried.
- `apfs_sharing = "fresh"` shares tracked files of at least 64 KiB with the main checkout using
  `clonefile`, on macOS APFS only. `internal/fileclone` is build-tagged `_darwin.go`, and
  `sharing_other.go` returns the reason "requires macOS APFS". It is **not** a pluggable
  interface, so it cannot be pointed at another block store.

### Hooks: exactly two, and they are late

`internal/hooks/hooks.go` and `internal/config/hooks.go`:

- The only hook keys are `post_create` and `pre_destroy` (`config/hooks.go:18`).
- They are read **only** from the user config `~/.config/treehouse/config.toml`. A `[hooks]`
  table in a repo-level `treehouse.toml` is discarded and warned about once on stderr
  (`config/config.go:167-171`), so running treehouse in an untrusted clone cannot execute
  checked-in shell.
- Each command is a list of strings run sequentially through the OS shell (`/bin/sh -c` on
  unix), with the **worktree directory** as the working directory, stdout and stderr streamed
  to the caller's. A non-zero exit is logged to stderr and does not stop the remaining commands
  or fail the acquisition (`hooks/hooks.go:22-38`).
- `post_create` runs in `internal/pool/pool.go:812`, after acquisition has fully succeeded. With
  `treehouse get --lease` its stdout is routed to stderr so stdout stays the leased path.
- `pre_destroy` runs in `internal/pool/destroy.go:451` and `internal/pool/prune.go:492`.

**There is no `pre_create`, no `pre_return`, no `post_return`, no `post_lease`, and no
provisioner interface.** That is the whole extension surface.

### Order of operations inside an acquisition

From `internal/pool/pool.go` (`freeTemplatedSlot` or the built-in layout, then):

1. choose a free slot name, `os.MkdirAll` its parent (`pool.go:655`).
2. `vcs.PruneWorktrees(repoRoot)`, best effort (`pool.go:671`).
3. resolve the base branch and its commit.
4. `vcs.AddWorktree(repoRoot, wtPath, branch)` (`pool.go:684`). This is `git worktree add`: it
   creates the checkout and writes the `.git` file plus `{main}/.git/worktrees/{slot}/`
   bookkeeping. A failure records the slot as `quarantined: ...` and returns.
5. `seedWorktree` copies the ignored and untracked files named by the committed
   `.worktreeinclude` into the slot (`pool.go:686`).
6. append the entry with the provisional lease `quarantined: acquisition state incomplete` and
   `persistState` (`pool.go:711-719`).
7. optional branch creation.
8. **only on the fresh-allocation branch**, `apfsSharing`: `shareWorktreeFiles(repoRoot, wtPath)`
   (`pool.go:783`). Reused and recycled slots are explicitly excluded, with the comment "a reused
   slot may still have external writers".
9. `markAcquired`, `persistState`, set `runPostCreate`.
10. `hooks.Run(postCreate, acquired.Path, ...)`.

So a hook always runs **after** the worktree exists and its content is fully materialised, and
**before** the caller is handed the path. Every acquisition is recorded in state before the hook
runs, so an interrupted hook leaves a quarantined or leased slot that a later `get` will not hand
out.

### Process detection and termination

- `internal/process/detect.go` `FindProcessesInWorktree` lists every pid with `gopsutil` and
  keeps a process only when `p.Cwd()` resolves inside the slot. No `lsof`, no open file
  descriptors, no locks, no process groups. Confirmed for tag v3.1.0 and reproduced in
  `docs/spikes/5-treehouse-process-detection.md`.
- `internal/process/terminate_unix.go`: SIGTERM, poll `kill(pid, 0)`, SIGKILL the survivors, then
  poll again so a killed process is reaped before git runs.
- `cmd/get.go:331` passes a grace period of exactly `2*time.Second`.
- `filterProtectedProcesses` (`internal/process/terminate.go:76`) removes the caller and its
  entire ancestry from the target set, and fails rather than silently protecting everything.
- `cmd/get.go:251` `returnWorktreeToPool` runs `killLingeringProcesses` as the release's
  `beforeReset` step **under the pool state lock**, so a writer cannot slip in between the
  emptiness check and the destructive reset. Survivors after termination make the return fail
  and leave the slot in place.
- `treehouse destroy` is a **dry run by default** and takes `--yes` to execute, with
  `--include-unlanded`, `--include-in-use` and `--include-leased` for the risk classes. There is
  no `--force` any more.
- `treehouse prune` is a dry run by default and takes `--yes`. `--all`/`--global` sweeps every
  pool under the **user-level** root.

### Machine-readable output

- `treehouse get --lease --json` prints one JSON object on stdout:
  `{path, lease_id, lease_holder, leased_at, base_branch}`. Verified.
- `treehouse status --json` prints the array of this repository's pool entries. Verified `[]` for
  an empty pool.
- `treehouse return <path|name>` takes `--force`, `--if-lease-id`, `--if-lease-holder`, `--all`.
  A path argument is read as a path first and only then as a name, and the pool is found from
  the path itself, so a return works from outside the repository.

## How mode (a) maps onto it

Mode (a) is "unmodified treehouse on the mount" and needs nothing from treehouse.

- Put the pool root on the mount: `treehouse --root {mount}/th` gives pool root `{mount}/th/.treehouse`.
- Put each repository's main checkout on the mount as a snapshot, for example
  `{mount}/{pool_id}-main`, and run treehouse from there.
- A slot is then `{mount}/th/.treehouse/{pool_id}/{slot}/{repo}`, an ordinary git worktree on the
  mount, and block dedup happens underneath. Nothing else changes.
- Do not set `worktree_path`: it adds a constraint (absolute, not inside the repository, never
  adopt an existing path) for no benefit, and the built-in layout is what `return` resolves a pool
  from without a repository.
- Leave `apfs_sharing` off. On a cowfs mount the blocks are already shared, and the sharing step
  is macOS-APFS-only.

`cowfs-treehouse setup` writes the recommended config and reports what it could not verify.
`cowfs-treehouse doctor` checks the things mode (a) actually depends on:

| check | how |
|---|---|
| the mount is a cowfs mount, not a plain directory | `mount_info` reports `adapter` and `mounted`; the path is the mount path |
| snapshot directories are visible under the mount | each `snapshot_list` name exists as a directory at `{mount_path}/{name}` |
| flock works on the mount | create a file under the mount, take `LOCK_EX`, read it back from a second descriptor |
| hardlinks work on the mount | link a file under the mount and compare inode numbers and content |
| no `.nfs*` silly-rename dirt in any slot | directory scan of every pool slot |
| no open-fd or flock holder in any slot | `ps` for the snapshot behind each slot |
| the pool root and the main checkout resolve inside the mount | canonicalise and prefix-check |

The `.nfs*` and open-fd checks are the issue #20 answer for mode (a). On the NFS loopback,
`treehouse return` unlinks a file a process still holds, the macOS NFS client silly-renames it to
`.nfs.<id>` inside the slot, `return` still exits 0, and the slot then reads `dirty` forever
because the next `get` skips it.

## How mode (b) maps onto it

### The one thing that does not fit

cowfs exposes a snapshot as a directory at `{mount_path}/{name}`.
A treehouse slot is a directory at `{pool_dir}/{slot}/{repo}`, which is three components below the
pool root and therefore never a snapshot name.

Neither of treehouse's two path mechanisms can bridge that:

- `--root` puts the pool root anywhere, but the slot path shape below it is fixed.
- `worktree_path` can point outside the pool, but **treehouse never adopts a path that already
  exists** (`pool.go:641-651`), and a snapshot directory always exists. It is skipped as an
  occupied candidate.

So the slot directory has to be made snapshot-backed by cowfs, and treehouse has to keep using it
as an ordinary directory. That is possible, and it does not need a fork.

### The flow that works with stock 3.1.0

The slot is snapshot-backed, and `post_create` is the one hook that runs at exactly the right
moment: after the worktree exists, before anybody is handed the path.

Per acquisition:

1. The operator runs `cowfs-treehouse get` instead of `treehouse get --lease`.
2. cowfs materialises an empty snapshot as the slot's backing store, so `git worktree add` in
   step 4 of treehouse's own acquisition writes into a snapshot instead of onto the mount's
   native side. This is a cowfs-side operation and needs a control-API request, see the gap list.
3. treehouse runs its normal acquisition, including `git worktree add` and the `.git` file and
   `{main}/.git/worktrees/{slot}/` bookkeeping. That bookkeeping is per slot and git owns it, so
   nothing has to imitate git.
4. `post_create` runs `cowfs-treehouse provision --slot-path {slot}`:
   - derive the pool id, find the base snapshot with `snapshot_list` on `base.repo`,
   - `snapshot_create {name: {pool_id}-{slot}, from: {pool_id}-base}`, or
     `snapshot_reset` with the same arguments when the slot snapshot already exists,
   - rewrite the slot's `.git` file so it points at `{main}/.git/worktrees/{slot}` again, because
     the reset replaced the tree with the base's.
5. The caller gets a path whose source is the warm base and whose `target/` already holds a
   dependency build, and `cargo` sees it as Fresh (spike 6: a warm `target/` copied to a new path
   built 34 of 34 and 41 of 41 units Fresh with 0 crates compiled).

The whole re-provisioning is one `snapshot_reset` plus one small file write. No data is copied,
which is what makes it O(1) and what makes the second and later slots nearly free.

Two ordering rules the implementation depends on:

- Where the slot's bookkeeping lives is resolved **before** the swap, while the slot's own `.git`
  link is still valid. After the reset the link belongs to the base, so git can no longer say where
  this slot's bookkeeping is, and a repair that asked then would be guessing. The directory git
  names it after is the slot path's own leaf (`{repo}`, or `{repo}-{slot}` under `unique_leaf`),
  never the slot number.
- A slot whose `.git` is a directory is left completely alone. That is a real repository, not a
  worktree link, and overwriting it would destroy a checkout.

### Return is the interesting half

`treehouse return` resets the worktree with `git reset --hard` and `git clean -xdf`. Against a
slot whose `target/` holds gigabytes that is the expensive operation, and it destroys the very
content the next slot was going to reuse. So the wrapper inverts the order:

1. `ps {pool_id}-{slot}` for the report.
2. With `--force`, terminate holders using treehouse's own policy (below).
3. Wait for `.nfs*` dirt in the slot to clear, bounded, with a clear message on timeout.
4. `snapshot_reset {name: {pool_id}-{slot}, from: {pool_id}-empty, expect_no_holders: true}`. The
   slot is now empty, so treehouse's own reset and clean are instant and cannot produce
   `.nfs` dirt. It is reset to the empty snapshot and not to the base, because a base left in
   place is exactly the warm tree treehouse is about to walk.
5. `treehouse return {slot} --force --root {pool} --if-lease-id {id}`, which releases the lease and
   parks the slot. The lease identity comes from `treehouse status` run from the main repository,
   because run from inside a slot treehouse reports that slot as "you're here" with an empty
   `lease_id`.
6. Optionally `snapshot_rm {pool_id}-{slot}` to release the snapshot, or keep it for reuse.

Mode (a) is the same flow with the snapshot steps left out, so it needs no cowfs daemon at all and
only the holder checks and the wait apply. The mode is explicit, `--mode a` or `--mode b`, because
guessing it from whether a root was given would make a mode (a) return demand a daemon it does not
need.

Pinning the release with `--if-lease-id` means a slot that was re-leased between our `ps` and our
return is left alone instead of being reset under its new owner. The `expect_no_holders` check in
step 4 is the authoritative one, evaluated by the daemon under the same lock as the reset; `ps` is
only ever used to name the holder in the message. A holder that appears between `ps` and
`snapshot_reset` produces `busy` with nothing changed, which is the race the task asks about.

### Termination policy, mirrored

`cowfs-treehouse return` mirrors treehouse so a holder is dealt with the same way
(`internal/process/terminate_unix.go`, `cmd/get.go:331`):

- only processes the caller can signal: never pid 1, never the caller, never any of the caller's
  ancestors, and never without `--force`,
- SIGTERM, poll `kill(pid, 0)` every 100 ms for 2 s, SIGKILL the survivors, then poll again for
  2 s so a killed process is reaped before git runs,
- re-scan afterwards and fail if a foreign live writer remains, leaving the slot in place.

The difference is the selection. `ps` on the cowfs control API reports `cwd`, `fd` and `lock`
holds, so a process that chdir'd out but still holds an open file or a flock is caught. That is
issue #20, and it is why the wrapper calls `ps` before doing anything destructive.

### Warm base

- The base is a snapshot of a repository at a git ref, with the dependency build already done, so
  it is created by `base_refresh {repo, git_ref, name}` and found with `snapshot_list` on
  `base.repo`.
- `cowfs-treehouse base refresh --repo R --ref REF [--build CMD]` optionally runs `CMD` in a real
  treehouse slot first, so the artifacts the daemon snapshots are ones cargo actually produced,
  then calls `base_refresh`, then `snapshot_promote` (idempotent), then returns the slot.
- Staleness is `base.commit` against `git rev-parse REF` in the repository. `cowfs-treehouse base
  status` prints fresh or stale with both commits and the age of the base.
- Promoting an arbitrary slot to the base is explicit: `cowfs-treehouse base promote --slot
  {path}`. Nothing is promoted implicitly, per `docs/design.md`.
- Discarding a slot after a commit is explicit too: `cowfs-treehouse discard --slot {path}`,
  which is return plus `snapshot_rm`.

### Path rules from spike 6, and the choice made

Spike 6 measured: a warm `target/` copied to a new path stays Fresh for cargo; a
slot-specific `--remap-path-prefix` makes the copy 100% dirty because `RUSTFLAGS` is part of the
fingerprint; and a clone that then remaps to its own prefix recompiles every unit.

So mode (b) uses **clone without remap**, and no canonical path is required. Consequences, all of
which the implementation enforces or checks:

- The base refresh flow never injects `RUSTFLAGS`, `--remap-path-prefix` or
  `CARGO_INCREMENTAL`. `base refresh` refuses `--rustflags` outright rather than accepting it and
  producing a base that dirties every slot.
- The warm base is built at whatever path the build ran at. The `.git` file is the one thing that
  cannot be shared, so it is rewritten per slot. This is the single per-slot write in the flow.
- `doctor` warns when the repository's `.cargo/config.toml` sets a `[build] rustflags` or
  `[target.*] rustflags` that mentions a slot path, because that is the spike 6 dirt case.

Canonical paths and per-agent mount namespaces remain issue #17 and are not needed for mode (b).
They matter for mode (a), where unmodified treehouse builds independently at different paths and
pays 12.8% to 28% per debug slot.

### Slot and snapshot naming

- `pool_id` is `{basename(repo_root)}-{short6}`, byte-identical to treehouse's `ResolvePoolDir`:
  the basename plus the first 3 bytes of `sha256`, hex, of `git remote get-url origin`, falling
  back to `sha256(repo_root)` when there is no `origin`. The hash is what makes two repositories
  with the same directory name distinct on a mount shared by every repository.
- base snapshot: `{pool_id}-base`
- main checkout snapshot: `{pool_id}-main`
- empty snapshot: `{pool_id}-empty`, what a returned slot is reset to
- slot snapshot: `{pool_id}-{slot}`, where `{slot}` is treehouse's slot name.
- Every derived name goes through `cowfs_ctl::validate_snapshot_name`, so a repository whose name
  would produce an illegal snapshot name is refused with a clear message rather than sent to the
  daemon. `short6` is 6 hex characters, so `-base`, `-main` and `-empty` cannot collide with a slot
  suffix.
- A slot name is additionally checked to be a single path component. The snapshot validator alone
  would accept `pool-..`, which is still wrong.
- `-base` is derived, never configurable. A configurable base name is how two repositories end up
  sharing one warm base.
- The pool id is read from the pool **directory**, which needs no git at all, falling back to the
  repository only for a slot outside a pool. A run that crashed between the reset and the `.git`
  repair has a broken link, and identity derived from git would be unavailable exactly then.
- Paths that act as an identity are canonicalised at the control-API boundary. treehouse reports
  `/private/var/...` where a caller on macOS holds `/var/...`, and without that a `base.repo`
  lookup misses.
- A slot path is passed on to treehouse exactly as the caller spelled it, never canonicalised:
  treehouse records the path it was given and matches later calls against that string, so rewriting
  it makes the slot read as unleased.

## Gap list

Ordered by what blocks what.

| # | gap | blocks | owner |
|---|---|---|---|
| 1 | No control-API operation that materialises a snapshot at an arbitrary path. `mount_info` is read-only and there is no `mount`, `export` or `submount` method. | mode (b) entirely | cowfs control API (request below) |
| 2 | `apfs_sharing` is not a pluggable interface, only a macOS-APFS `clonefile` implementation, and it runs only on fresh allocations. | an alternative mode (b) design that reuses it | treehouse upstream |
| 3 | No `pre_create` hook, so a provisioner cannot claim the slot path before treehouse materialises a worktree into it. Costs one throwaway `git worktree add` per acquisition. | nothing; a performance and elegance gap | treehouse upstream |
| 4 | No `pre_return`/`post_return` hook, so the O(1) swap cannot happen inside treehouse's locked release. The wrapper has to do it before `return`, which leaves a window where the slot is handed out empty. | nothing; a correctness-tightness gap | treehouse upstream |
| 5 | `FindProcessesInWorktree` is cwd-only, so open-fd and flock holders are invisible to `treehouse return`. | issue #20 on native disk | treehouse upstream |
| 6 | No `cowfs treehouse` subcommand in `cowfs-cli`, so the companion is a separate binary. | packaging, not function | cowfs-cli (request below) |
| 7 | The warm base must be shaped so only the `.git` indirection is per slot. A base carrying a full `.git` directory forces a directory delete per slot. | mode (b) O(1) claim | cowfs-core (`base_refresh` implementation) |
| 8 | `base_refresh` must record the real commit, not a synthetic one, for staleness detection to mean anything. The stub records `stub-{git_ref}`. | staleness detection against a real backend | cowfs-core |
| 9 | AppleDouble `._*` files: not a snapshot problem, a mount problem. The mount must hide them or translate them, or `git status` in a slot reports them. | mode (a) and (b) on macOS | cowfs adapters |
| 10 | `treehouse` with `vcs = "jj"` uses a different remote-URL lookup, so `pool_id` derivation in the companion is the git backend only. | jj pools | accepted limitation, documented |

The upstream proposal for gaps 3, 4 and 5 is `docs/upstream-treehouse-proposal.md`.
Gap 5 is the same change spike 5 proposed. Gaps 3 and 4 are one small hook-key addition.
Nothing in gaps 1 to 10 needs a fork.

## Requests for other crates

### `cowfs-cli` (gap 6)

`cowfs-treehouse` ships as its own binary because the CLI has no seam for it. The exact request:

- add a `Treehouse` arm to `Command` in `crates/cowfs-cli/src/cli.rs` that forwards all
  remaining arguments to `cowfs_treehouse::run(args, Env)`,
- export `Env { socket: Option<PathBuf>, json: bool }` from `cowfs-treehouse` so the CLI can pass
  the `--socket` and `--json` values it already parsed rather than have them parsed twice,
- add `cowfs-treehouse` to the workspace and to the `--version`/`completions` plumbing.

Without that, the binary is installed and documented separately and the `cowfs` and
`cowfs-treehouse` version strings can drift.

### `cowfs-ctl` (gap 1)

The control protocol has no way to make a snapshot appear at a chosen path. The smallest request
that unblocks mode (b), consistent with the existing evolution rules in
`docs/v1-control-api.md`:

- a new method `mount_snapshot {name, path, expect_no_holders?}`, returning `mount_info`, where
  `path` is an absolute path to an empty or absent directory,
- and `unmount_snapshot {path}`.

Both are new methods, which the evolution rules already allow as compatible within major version 1,
and a client learns they exist from `hello.methods`, so a client that does not know them degrades
cleanly. `params` for a destructive method must be strict, as `snapshot_rm` is.

An alternative that needs no protocol change is for the mount to accept a
`--slot-export {snapshot}={path}` option at `serve` time, driven by the companion, and for the
companion to shell out to that. It is less clean, because the mount configuration then depends on
which slots exist.

## What needs the real core or a real mount

Not testable today, listed so it is not mistaken for done.

1. Snapshot materialisation at a chosen path (gap 1).
2. `snapshot_reset` actually swapping the visible content of a live mountpoint, including the
   `.git` file rewrite and `target/` becoming Fresh.
3. `flock` and hardlinks on the real adapter. The doctor checks run against native directories
   today and report what they can.
4. `.nfs*` silly-rename behaviour on the real NFS loopback. The sandbox tests reproduce the
   condition with a held file descriptor on a native directory, which is enough to prove the
   wrapper's detect, wait and refuse logic but not the kernel behaviour.
5. Real commit hashes in `base.commit`, so staleness is a real comparison.
6. A warm base with a `.git` file rather than a `.git` directory (gap 7).
7. AppleDouble hiding or translation (gap 9).
8. Dedup actually happening across slots, which is the point of the whole thing and needs the
   store and the GC.
9. `ps` reporting real `fd` and `lock` holds from a real adapter. The stub reports injected
   holders, so the tests prove the companion's logic and not the adapter's scan.

## User guide

### Prerequisites

A cowfs daemon serving a real store, and a repository whose main checkout is on the mount.

```sh
cowfs serve --store ~/.cowfs/store --mount ~/.cowfs/mnt
```

### Mode (a), end to end

```sh
# 1. create the main-checkout snapshot and import the repository into it
cowfs import ~/src/myrepo --name "$(cowfs-treehouse pool-id ~/src/myrepo)-main"

# 2. prepare the mount for treehouse, pool root on the mount
cowfs-treehouse setup --mount ~/.cowfs/mnt --pool-root ~/.cowfs/mnt/th \
    --repo ~/.cowfs/mnt/"$(cowfs-treehouse pool-id ~/src/myrepo)-main"

# 3. check it
cowfs-treehouse doctor --mount ~/.cowfs/mnt --repo ~/.cowfs/mnt/...-main

# 4. work, from the main checkout on the mount
cd ~/.cowfs/mnt/...-main
treehouse get --lease --json --root ~/.cowfs/mnt/th
cd <path from the JSON>

# 5. return, through the wrapper, so open-fd holders are handled
cowfs-treehouse return --slot <path> --root ~/.cowfs/mnt/th
```

The wrapper is optional in mode (a). Plain `treehouse return` works, and leaves issue #20 open:
a holder with only an open fd or a flock is missed and the slot ends up `dirty`.

### Mode (b), end to end

```sh
POOL=~/.cowfs/mnt/th

# 1. build a warm base: the repo's build runs in a real slot, then the base is refreshed
cd ~/.cowfs/mnt/myrepo-1a2b3c-main
cowfs-treehouse base refresh --repo "$PWD" --ref main --root $POOL \
    --build 'cargo build --locked'

# 2. check freshness. Exit 1 means stale, which is a real answer and not a crash
cowfs-treehouse base status --repo "$PWD" --ref main

# 3. install the hook in the treehouse user config
cowfs-treehouse hooks install --home "$HOME"

# 4. acquire a slot. Needs the real backend, see the gap list
cowfs-treehouse get --repo "$PWD" --root $POOL --json

# 5. return it
cowfs-treehouse return --mode b --slot <path> --root $POOL --force

# 6. discard it once the work is committed
cowfs-treehouse discard --slot <path> --root $POOL --force
```

`hooks install` writes `post_create` into `$HOME/.config/treehouse/config.toml` and never touches
repo-level `treehouse.toml`, because treehouse discards hooks there anyway. It is idempotent, and
it refuses rather than overwriting a `post_create` that is not its own, which it marks with a
sentinel comment. Its default command is `cowfs-treehouse provision --slot $PWD`, because
`post_create` runs with the worktree as its working directory and stock treehouse 3.1.0 sets no slot
variable. The proposed upstream `pre_create` hook would supply `TREEHOUSE_SLOT_PATH`, which is the
better form once it exists.

`base refresh` refuses `--rustflags` outright rather than accepting it and quietly producing a base
that dirties every slot it is cloned into.

### Exit codes

The same codes as `docs/v1-control-api.md`, plus one:

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | the daemon returned an error, treehouse failed, or another failure |
| 2 | usage error |
| 3 | the cowfs daemon is not running |
| 4 | the cowfs daemon did not answer in time |
| 5 | the slot is held: `busy` from the daemon, or holders survived termination |
| 130 | interrupted |

Code 5 is separate from 1 because a held slot is a normal, recoverable condition that a script
should retry rather than treat as a crash.
