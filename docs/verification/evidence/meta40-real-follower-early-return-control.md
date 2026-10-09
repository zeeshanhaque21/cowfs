# PR #146 real follower early-return control

Refs #40.

The PR covers durable acknowledgements, a real private follower branch, and crash-image reopen checks through follower_durability.rs and db.rs unit tests.
It does not close the other mutation, Store crash, or Core health integration obligations of #40.

## Verified positive head

Run 37546518387 completed success for bdcbfcd6e08eaa906c0c75aa32de38b7f217b97e.
I read the completed logs directly, not only the aggregate green status.
Both Ubuntu and macOS report db::tests::follower_wait_does_not_ack_before_the_leader_publishes_durable_seq as ok.
Both report single_durable_ack_follows_the_hook and every_durable_ack_is_durable_on_return_including_followers as ok, with follower_durability.rs reporting 2 passed.
All three required checks succeeded.
That proves the unmutated witness ran, not that a mutant was killed.

## Direct test-only change at 3c29cfb

A thread-local cfg-test switch executes return Ok(()) at the start of the real wait_durable if-led branch, before the witness and condition-variable wait.
This is the canonical early-follower defect's branch behavior in the actual Meta method, not a replica of its loop or a scheduling simulation.
The switch defaults off and is enabled only on the negative control's follower thread.
The positive case and negative control call the same fixture and verifier, with false and true respectively.
The negative control requires the exact branch-order assertion to panic; a setup error, timeout, leader error, or unrelated panic does not satisfy its expected message.
The first event is captured while the leader is held, and both threads are released and joined before that event is judged.
Durable sequence and reopen-survivor assertions remain in the shared verifier.

The hook holds only its first invocation, so close no longer incurs an unrelated 30-second wait.
Channel disconnects and release timeouts now return an error rather than silently permitting the held commit.
Stripping cfg-test blocks leaves wait_durable byte-identical to the verified bdcbfcd implementation.
Standalone rustfmt check passes.

## Evidence boundary

The new compiled in-process mutation control has not yet executed; normal push-triggered exact-head CI is pending.
The historical external source-mutant rebuild remains unexecuted and is not claimed by this change.
No local Cargo execution occurred while READY5 remains above the artifact cap.
No public fault API, production behavior, dependency, workflow, runner, store, mount, cleanup, or lease was changed.
The PR remains draft and historical receipts remain unchanged.

## Self-check

Accuracy 4/5: claims are pinned to named logs, but the new negative control is runtime-unverified.
Completeness 3/5: the follower control is implemented, but other #40 acceptance obligations remain open.
Clarity 4/5: compiled in-process mutation is explicitly distinguished from an external source-mutant rebuild.
Actionability 4/5: both cases share a runnable CI verifier, but exact-head execution is pending.
Conciseness 4/5: the report separates delivered evidence from pending work without a new acceptance framework.
Overall 3.8/5.
Next priority is named exact-head positive and negative-control execution, followed by the remaining recorded #40 gates.
The user would reasonably still consider the overall task unfinished.
