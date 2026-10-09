# PR #142 counter-only reservation contract correction

Refs #42, request 4.

Head abc63bb554b6455086bf26c975f1326b6a115d8b corrects the two physical durability fixtures.
Run 37548902575 on 691ead3 reproduced both failures on Ubuntu and macOS.
The first failed because create did not increment the Store sync-hook counter.
The second failed because create succeeded while that hook was armed to fail.
Both failures contradicted my fixtures, not the documented reservation contract.

## Boundary diagnosis

The exact branch implements Core::make through take_reserved, Meta::reserve_tickets, and Meta::reserve_inodes.
The reservation path durably commits its counter intent and floor through redb, without invoking before_sync.
The public reserve_inodes documentation explicitly states that no hook runs because there are no chunk references.
The cached reservation floor can cover requests without any new commit.
Consequently, observing the Store hook cannot establish or falsify reservation-counter durability.
This corrects the earlier durability-retarget receipt's mistaken hook assumption, which remains preserved as history.

## Corrected runnable comparisons

The positive fixture now requires unchanged hook count at create, physical identity below the reserved floor, and increased hook count after writing content and synchronizing.
It retains reopen identity, exact bytes, floor persistence, and no-reuse checks.
The failed-hook fixture now explicitly requires counter-only create to succeed, then requires content synchronization to fail while the Store hook is armed.
It verifies the same handle and exact bytes before retry, retries synchronization, and verifies the same identity and bytes after reopen.
A separate closed-session fixture requires the first create to fail without publishing a namespace entry when Meta refuses reservations.
No new fault seam or production change was added.

## Evidence limits

Standalone rustfmt passes.
Run 37549687291 is queued for the exact head; the new assertions have not executed yet.
Local Cargo remains prohibited by the artifact cap.
The previous run's named critic2b results are 27 passed and one ignored on both platforms; this is not new-head proof.
These fixtures do not claim direct redb fsync-failure injection or pre-sync crash durability proof.
Existing reservation/crash/recovery tests must separately establish those recorded acceptance gates.
Large requests, store/session ticket authority, one-time consumption, and retry ownership remain unchanged.
No runner operation, workflow rerun, artificial trigger commit, cleanup, mount change, or lease release occurred.
The PR stays draft and #42 stays open.

## Self-check

Accuracy 3/5: the hook contract is now pinned to exact source and failed runtime, but my previous fixture confused two durability layers.
Completeness 2/5: the fixed 68-item objective remains at 43 accepted items, and this head still lacks runtime proof.
Clarity 4/5: the receipt separates Store hook failure, closed-session refusal, and redb counter durability, but earlier receipts need to be read as superseded history.
Actionability 3/5: runnable comparisons are pushed and exact-head CI is queued; acceptance cannot proceed until named results exist.
Conciseness 4/5: the change uses existing hooks and APIs without a new production seam, although the historical correction trail is long.
Overall 3.2/5.
Priority is to verify this head's named tests, then retain independent crash and counter-failure gates rather than substitute Store hook observations.
The user would reasonably still regard the overall objective as unfinished.
