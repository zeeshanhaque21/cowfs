# PR #113 current-main integration

The delivery is the reviewed server-requirements test harness, not full issue #19 acceptance.

- Reviewed pin: `b434afe98f95db3ad5c134e80747ccad0e1a5732`.
- Independent portability review SHA256: `ee228a281a9cf2261574526c13c03cc8c24dc4c54c2a6d7cd8a74dff0593ee14`.
- Integration base: `42a2efacfbabcd122488db88c2058fb89145cef7`.
- Tested merge tree: `392b96999b4a092f504b130c8b2850ff688aad0e`.
- Merged commit: `00065ce75dcd554e1fb4bb084d1c70b2e2a21a87`.

The combined tree was extracted into a fresh native archive with its own `CARGO_TARGET_DIR`.
The requirements and protocol test bytes were compared against the merge-tree blobs before execution.
`TMPDIR` pointed inside the owned archive's project-local fixtures.
One bounded 600-second heavy-lock acquisition, an 8 GiB artifact cap, and a 20 GiB free-space floor governed the run.

| Command | Result | Exit |
| --- | --- | ---: |
| `cargo test --locked -p cowfs-nfs --test requirements19 hardlinked_names_in_one_directory_are_listed_once_each -- --exact --nocapture` | 1 passed, 9 filtered; 0.06 seconds | 0 |
| `cargo test --locked -p cowfs-nfs --test requirements19 -- --test-threads=1 --nocapture` | 7 passed, 0 failed, 3 ignored; 245.03 seconds | 0 |
| `cargo test --locked -p cowfs-nfs --test protocol` | 23 passed, 0 failed; 0.14 seconds | 0 |
| `cargo clippy --locked -p cowfs-nfs --test requirements19 -- -D warnings` | Scoped lint passed | 0 |

The complete real-filesystem hardlink RPC sample passed before the larger default battery.
The three ignored tests include the two mounted checks and a measurement; none was executed or counted as acceptance.
Logs remain under `bench/out/pr113-current-main-integration/392b96999b4a092f504b130c8b2850ff688aad0e/`.
No shared daemon, store, mount, or lease was used or changed.
The 245-second requirements duration is a wall-clock observation, not a performance result.

All three required CI checks passed at the exact reviewed PR pin.
The merged commit tree equals the tested tree.
The primary checkout adopted its two canonical documentation mirrors only after byte equality and local-overlap checks.

The branch history contains an ambiguous `closed` followed by `#19` across a paragraph break in commit `b4b3f37905d7f571a38c55d8ae37a83c26df9a23`.
No claim is made that GitHub would necessarily parse that as a closing directive.
A SHA-pinned squash merge with an explicit neutral `Refs #19` message avoided introducing that history into main without rewriting the reviewed branch.
Post-merge GraphQL confirms issue #19 is OPEN and closing references are empty.

Linux symlink-mode refusal, actual mounted acceptance, shared-host performance, and whole-issue completion remain unproved or open as previously disclosed.
