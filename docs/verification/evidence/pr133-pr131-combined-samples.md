# PRs #133 and #131 combined delivery samples

- Main base: `0d7da418dae2f304ae901174425d32902a20cbd3`.
- Fixture repair: `4dbbd992b912589e13e3841858de09079f1d60f6`.
- PathVfs test delivery: `2b4cf66d5284ec14f56fd89944fd6b524303c082`.
- Clean combined tree: `3be28823bc631d8612ec36e6b3fc2c3774229538`.

The combined tree preserves the fixture repair's `health.rs`, the PathVfs delivery's `tests.rs`, and main's two benchmark classifier files byte-identically.
The full tree was extracted into a fresh native source archive, with an isolated `CARGO_TARGET_DIR` and private project-local `TMPDIR`.
All four source files were compared with the combined tree blobs before execution.
The run used one bounded 600-second heavy-lock acquisition, an 8 GiB artifact cap, a 20 GiB free-space floor, and failure/no-progress exits.

| Exact command | Actual result | Exit |
| --- | --- | ---: |
| `cargo test --locked -p cowfs-meta --test health repeated_rollbacks_keep_counting_and_keep_moving_the_floors -- --exact --nocapture` | 1 passed, 0 failed, 6 filtered; 14.75 seconds | 0 |
| `cargo test --locked -p cowfs-vfs-path --lib tests::readdir_sees_a_name_created_outside_the_vfs_while_a_listing_is_paged -- --exact --nocapture` | 1 passed, 0 failed, 32 filtered; 0.00 seconds reported | 0 |

Raw logs remain under `bench/out/pr133-pr131-combined/3be28823bc631d8612ec36e6b3fc2c3774229538/`.

| Log | Bytes | SHA256 |
| --- | ---: | --- |
| `recovery-sample.log` | 2230 | `330bd987b91c716f35ccfe98b9880af863a8db7129087f01ba2ab21bd5a8fb65` |
| `path-sample.log` | 1107 | `8e8325368fcfaa81665488c0915815e756046342b5be1708c3c1487ee0d84954` |

These are two actual combined-tree samples, not full-suite or filesystem acceptance.
No shared daemon, store, mount, or lease was used or changed.
The original #131 CI failure remains unreproduced.
Independent #133 review and the later integrated #131 exact-head CI gate are still required before their respective merges.
