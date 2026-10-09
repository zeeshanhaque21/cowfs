# PR #142 physical directory elision repair

Refs #42 and the retained #94 regression suite.

Run 37549687291 on abc63bb554b6455086bf26c975f1326b6a115d8b failed on both platforms after the corrected durability suite passed.
All seven durability integration cases passed, including counter-only create, closed-session refusal, Store sync failure/retry, and reopen identity/bytes.
Both private legacy marker ordering/refusal cases also passed on Ubuntu and macOS.
This is named prior-head runtime evidence, not whole-branch acceptance.

## Reproduced failure

Both platforms failed two directory cases at elide_dentry.rs line 191: the second rmdir elided zero queued mkdirs rather than one.
The physical create path marks the child namespace sequence so later metadata reads cannot address a not-yet-persisted inode.
The unconditional rmdir barrier consequently committed a newly created empty directory, preventing the retained mkdir/rmdir cancellation.
The same run failed the reopen fixture's obsolete expectation that a new session changes the file ID; both observed IDs were 2199023321088.

## Narrow repair

Commit 31596e6 preserves the child namespace sequence and changes only the rmdir barrier predicate.
The barrier is needed when the cached child count is unknown and the namespace sequence is uncommitted.
When the count is known, the final require_empty check under the namespace lock can judge it directly without a metadata read.
The state read is released before any barrier commit; the final name and child are still re-resolved under the namespace lock.
Known nonempty directories remain refused by the existing NotEmpty assertion.
Both directory elision assertions, cache-drop checks, snapshot isolation checks, reopen checks, and fsck assertions remain unchanged.
The reopen fixture now requires equal committed physical IDs and retains exact byte readback and fsck.
Standalone rustfmt and git diff --check passed before commit.

## Integration and remaining gates

Head ccfcc2d integrates main f31c81d3, including the merged NFS namespace repairs, adapter coverage, and allocator-extreme tests.
The integration is a genuine source merge, not a workflow dispatch, rerun, or artificial trigger commit.
The new head's directory and joint-tree runtime results remain pending.
No local Cargo ran while the artifact cap remains binding.
Large reservations and store/session-bound one-use tickets are unchanged.
Full identity/conformance/Meta retry, crash, session-limit load, and git-status cost obligations still remain open.
No store, mount, runner, shared artifact, or lease was changed or discarded.

## Log-access boundary

While the run was active, gh-axi refused its logs and rejected GitHub's required --allow-escape-sequences flag for completed job logs.
A read-only native gh api fetch retrieved job 112561940688 and filtered it in context-mode; no runner operation occurred.
Once the runs completed, normal gh-axi run view logs independently confirmed both platforms' results.

## Lock-audit follow-up at ee87670

Integrated run 37550296547 compiled and stopped at every_lock_site_is_in_the_audit_table before reaching elide_dentry.
Completed Ubuntu job 112563885367 reports 26 critic2b tests passed, one failed, and one ignored.
The single failure names ns.rs::barrier_if_needed, whose new state read was absent from the documentation table.
Commit ee87670 adds only the missing lock-audit row, mirrored in the primary checkout without overwriting its other pending edits.
The row records st.rd and its release before the barrier commit.
Directory-elision runtime acceptance and whole-branch checks remain pending on this new head.
No source assertions or lock-audit detection were weakened.

## Live and deleted identity fixture at 4f9d207

Run 37550656271 on ee87670 passed the lock-audit and all retained directory-elision cases on Ubuntu and macOS.
Both platform checks then failed virtual_inode_numbers_are_never_reused_across_a_restart at names_ino.rs line 29.
The fixture expected a still-live file to be stale solely because the session reopened; actual getattr returned its original physical ID and attributes.
This expectation belongs to the retired virtual-ID contract, not the approved physical-ID contract.
Commit 4f9d207 retargets the fixture to preserve live IDs and refuse reuse of an explicitly deleted ID.
It verifies the live file's original ID, attributes, and exact AAAA bytes after reopen.
The second allocated ID is explicitly unlinked and synchronized before reopen; its attributes and reads must stay stale, and its name must be absent.
All 200 new creates must differ from both earlier IDs, retain BBBB readback, and leave the original live file's AAAA bytes intact.
The deleted handle is checked again after those creates synchronize, so absence before allocation alone cannot satisfy the no-reuse contract.
The separate process-abort reservation fixture remains unchanged.
Standalone rustfmt passes; this head's runtime remains pending.

## Reservation fixture correction and integrated head

Run 37551320489 on 4f9d207 reaches cowfs-meta unit tests after the corrected Core identity tests.
The completed Ubuntu job fails a_ticket_from_another_store_is_refused and a_failed_durable_commit_does_not_strand_the_reserved_number.
The foreign-store fixture mistakenly spends A's ticket in A's snapshot, so acceptance is correct.
Commit 9c4ceb6 instead submits it to B with a confirmed same-number B ticket, requires refusal and absence, then successfully creates with both stores' respective tickets.
This retains store/session authority as distinct from numeric identity.
The failed-commit fixture clears its injection before graceful drop, which intentionally persists pending work.
It now explicitly refuses close with the exact injected pre-commit error before drop and reopen, retaining the pending-inode and retry-Exists assertions and reopened absence.
This is a refused-close pre-persist fixture, not an abrupt-process or power-loss claim.
The separate post-persist duplicate-refusal and actual process-abort reservation fixtures remain unchanged.
No reservation production code, range-size limit, or authority check changed.
Standalone rustfmt passes.
Integrated head c8398a8 includes main a759e6bc and #146's now-verified follower coverage; exact-head runtime remains pending.

## Integrated Ubuntu acceptance evidence

Run 37552521968's completed Ubuntu job 112571103245 passes on head c8398a82139f04d5433ede9211dea583e7d7f965.
The completed log confirms seven Core durability cases, eight directory-elision cases, four names/identity cases, four reserved-inode-identity cases, 132 Core conformance cases, and 27 executed critic2b cases pass.
It also confirms both private legacy virtual-marker ordering/refusal tests pass.
Meta's 36 unit tests pass, including a_ticket_from_another_store_is_refused, a_failed_durable_commit_does_not_strand_the_reserved_number, and a_durable_commit_that_persisted_then_failed_does_not_duplicate.
The fixed-cost large reservation test passes without adding a small-request cap.
The four recovery40 and eleven inode_reservation integration cases pass.
Counts are separated by crate and suite, since different crates reuse durability.rs as a filename.
Ubuntu and FUSE checks pass; macOS remains pending, so the branch stays draft and whole #42 remains open.

## Verified merge

Run 37552521968 completed successfully on all three required checks for c8398a82139f04d5433ede9211dea583e7d7f965.
The completed macOS log independently confirms the same suite counts and named reservation/marker cases recorded for Ubuntu above.
All jobs checked out CI merge a9598e9b8c50a4770fc853b01a0d4416a9d5873b, tree f6aa31ea1d82f0688f3e1d60dcd0e0cc8fa76f19.
PR #142 merged at 23cae2e86b7d5d03b477e5d1f9472fa0614d51d6 with exactly that tree, using a SHA-guarded merge request.
This supersedes the pending branch-runtime statements, without rewriting historical evidence.
The reserved-inode consumer and its identity/retry fixtures are delivered; separate crash, session-load, and git-status-cost obligations still keep whole #42 open.
