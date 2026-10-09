# Existing real-Store crash-image coverage for #40

## Pin and executed sample

Audited main a759e6bc714388045146b4eb8e37fb82e64ab00b.
crates/cowfs-core/tests/crash.rs has 686 lines and SHA256 42fecfb388f6c78196e4c9d463d0d4a29e2b78b9d62d3dc032160e4846f72962.
The READY3 source inspected through the graph matches this main file byte for byte.
Run 37550267162 executed crash_images_reopen_consistent_and_keep_fsynced_data successfully on Ubuntu and macOS.
The run's tested merge tree exactly matches main a759e6bc, as established in meta40-follower-negative-control-runtime-and-integration.md.
No new local build, workload, or mutation execution was performed for this audit.

## Prompt-to-artifact mapping

- Real cowfs-store: World::new uses Core::open_with_meta, which opens the real Store and installs its production sync hook.
- Metadata crash model: Meta uses a logging redb StorageBackend; it is not a real power-loss event.
- Real Store artifacts: World::point snapshots actual pack files, SYNCED, and index.cix into isolated fixtures.
- Store crash image: crash_store truncates the last real pack beyond its synchronized bound and optionally omits the index.
- Metadata crash image: run applies All, SyncedOnly, and two SyncedPlusSome policies to the logged metadata writes.
- Actual recovery: verify opens each combined image with Core::open and refuses a corruption report.
- Namespace and data integrity: verify checks metadata invariants, clean fsck, durable snapshot presence, and exact previously fsynced bytes for untouched files.
- Continued operation: verify creates and fsyncs a new 300000-byte file, closes, reopens, reads the exact pattern, and requires clean fsck again.
- Negative control: the_crash_test_notices_a_missing_store_sync_before_metadata_commits also passed as a should-panic test on both platforms.

## Limits and acceptance

The harness is integrated with the real Store, correcting the stale statement that all available crash coverage uses only a model Store.
Its default sample is one seeded workload of 60 operations, producing four metadata policies per captured point.
The completed success lines prove execution, but do not expose the printed image total or an explicit environment override; no measured image count is asserted here.
Truncation obeys the fixture's recorded synchronization marker, so this is modeled crash-image coverage, not proof of OS or hardware fsync semantics.
The verifier intentionally permits losses in files touched after their durable checkpoint.
The negative control has a broad should-panic annotation, not an exact expected error, so its aggregate pass alone cannot exclude an unrelated setup panic.
This accepts the existing bounded real-Store crash-image delivery and named positive runtime, not full crash robustness or all mutation obligations.
The other #40 controls, pending Core health runtime, and the complete 68-item objective remain open.
No historical review or receipt was rewritten.
