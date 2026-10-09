# PR #99 current-main integration

The scoped delivery is shared snapshot-name validation, request 5 of issue #42.
Requests 1-4 and whole-issue acceptance remain open.

- Reviewed pin: `3951d50922127450f781883c65864caf83189d07`.
- Independent source review: `docs/reviews/snapshot-name42-final.md`, SHA256 `3f0a1526b044e6374a44267e9e2b4d9d55a4105a167266e2d2b783235ba3acc9`.
- The final delta from reviewed `a98ece8e6a737fa91c6825dea5ac3fb173939d93` changes only the doc-comment byte count from 382 to 378.
- Integration base: `4c95d5dcb183acd8b8acc978e2452654a4cf1b4d`.
- Tested merge tree: `c5ae115408686b9c6f2b33856a06926f0f22a99d`.
- Merge commit: `36b1709bdf9850e9dfb60be9099716e997a0e5a3`.

The sole overlapping branch file is `crates/cowfs-ctl/src/validate.rs`.
The combined tree was extracted into its own native archive and built with a fresh, archive-specific `CARGO_TARGET_DIR`.
The extracted validation, naming library, and drift-test bytes were compared with the merge-tree blobs before execution.
The four-test real-store naming-drift sample passed before the remaining checks ran.

| Command | Executed tests | Exit |
| --- | ---: | ---: |
| `cargo test --locked -p cowfs-daemon --test snapname_drift` | 4 passed, 0 failed | 0 |
| `cargo test --locked -p cowfs-ctl --lib` | 11 passed, 0 failed | 0 |
| `cargo test --locked -p cowfs-snapname` | 6 passed, 0 failed; 0 doc tests | 0 |

Logs remain under `bench/out/pr99-current-main-integration/c5ae115408686b9c6f2b33856a06926f0f22a99d/`.
This run used one bounded 600-second heavy-lock acquisition, an 8 GiB artifact cap, and a 20 GiB free-space floor.
No shared daemon, mount, store, or lease was changed.
The merged commit tree equals the tested tree.
All three required CI checks passed at the exact reviewed PR pin.
PR closing references and commit-history closing directives are empty; issue #42 remains open.
The primary checkout fast-forward preserved unrelated local changes and adopted the incoming canonical document only after byte equality was checked.

This is scoped integration proof, not full filesystem, performance, or durability acceptance.
