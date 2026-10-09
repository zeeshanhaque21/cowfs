# #43 existing security and Translate evidence

This audits the original #43 requirements without adding behavior or a new tracker item.
It is a coordinator coverage/source audit, not a completed independent round-3 critic review or whole-issue acceptance.
The inspected READY7 source is at integrated head 5be872d.
There is no difference in `crates/cowfs-nfs` between that head and merged main 7f7b50595a464fd2de7257d5ad385fc31673556f.

## Executed sample and tree binding

Run 37555619010 tested CI merge 75b32464335e00d13b3ed192ef94264aae7fd5b5.
Its tree 3719beb178dffdea190835adf0a5c91eb568699b equals actual merge 7f7b50595a464fd2de7257d5ad385fc31673556f.
Direct macOS job 112581048020 and Ubuntu job 112581048044 logs each contain exactly one successful result for each of the thirteen tests below.
These are existing raw-protocol/in-process fixtures, not native mounted conformance, a git checkout, or a physical dead-server experiment.
No new runtime, daemon, mount, store, local Cargo, runner, cleanup, or lease operation was performed for this audit.

| Original requirement or relevant control | Existing test confirmed on both platforms | Scope |
|---|---|---|
| Random export restriction | `only_the_servers_own_export_path_answers_mnt` | Wrong paths refused; real export distinguished |
| Server-specific export | `two_servers_never_answer_each_others_mnt` | Independent servers do not answer one another's export |
| One-shot root handle | `only_the_first_mnt_gets_the_root_handle` | Claiming connection may repeat; another is refused; explicit rearm permits remount |
| Handle MAC | `forged_and_guessed_handles_are_refused` | Altered fields/MACs refused; valid stolen handle is deliberately shown to work |
| Server generation | `handles_of_a_previous_server_generation_are_stale` | Previous server handles rejected |
| Connection-cap availability | `served_but_silent_connections_do_not_lock_the_client_out` | Four-slot fixture serves a late connection and then another legitimate client |
| Reconnect replay | `a_retransmission_on_a_new_connection_is_replayed_too` | Same remove reply restored after reconnect; same xid with different call is not conflated |
| Independent connection xid space | `identical_xid_and_call_on_a_second_connection_is_executed` | A second connection really recreates a removed file rather than receiving a stale success |
| Mutating replay coverage | `every_non_idempotent_procedure_replays` | Existing protocol replay assertions executed |
| 200+ xattr encoding | `many_attributes_stay_visible_through_the_mount_protocol` | Counts 0, 1, 150, 200, 255 round-trip through decoder and underlying xattr count |
| Orphan sidecar as real file | `a_sidecar_without_a_main_file_is_a_real_file` | Archive-like name is stored/listed/read with exact bytes, then removed |
| Reserved sidecar whole-file refusal | `a_sidecar_that_is_not_a_sidecar_is_refused_not_dropped` | Implausible bytes fail and create neither xattrs nor a real reserved-name file |
| Sidecar generation | `a_sidecar_handle_of_a_reused_inode_is_stale` | Backend deliberately reuses inode; stale write cannot plant an attribute on the new file |

## Source-reviewed seams and limits

`crates/cowfs-nfs/src/handle.rs` uses keyed BLAKE3 over the handle body and compares the MAC with a bytewise XOR accumulator.
`HandleCodec::decode` checks exact length, server generation, MAC, kind and inode generation.
This is inspected source plus executed forgery/generation fixtures, not a formal cryptographic audit or timing measurement.

`crates/cowfs-nfs/src/sidecar.rs`, `Adapter::side_write`, obtains the per-inode lock before reading, modifying and storing a sidecar.
It bounds offset conversion, checked end addition and MAX_SIDECAR, and rejects an implausible prefix on an offset-zero write.
This establishes the inspected write-path locking seam, not every read/write/rename serial history or the complete round-3 locking review.

`crates/cowfs-nfs/src/peer.rs`, `same_user`, compares owners when both are available, but returns true when owners are unknown or the lsof command fails.
That is an explicit best-effort boundary in the current source, not evidence of verified second-UID isolation.
No second-UID attack was run, and this audit does not report an exploited vulnerability.
The one-shot export restriction and MAC do not turn that missing test into an isolation guarantee.
The valid-stolen-handle result and export-path visibility in the process table remain documented residual risks from the original issue.
The forgery test loops over guesses 1 through 1999; its diagnostic text saying 2000 must not be mistaken for an exact sample count.

## Required work still open

- Complete the requested round-3 Translate and security critic review, including all affected read/write/rename seams rather than only the inspected functions.
- Establish second-UID behavior or retain an explicit release limitation; do not claim isolation from these same-user fixtures.
- Verify actual daemon cleanup wiring and an owned private dead-server mount end to end without changing shared runtime.
- Assert Store-mode checkout sidecar behavior using the real workload; orphan-Translate coverage is not a substitute.
- Run mounted Posix/full conformance and the corresponding raw-protocol cases where silly rename prevents judgment.
- Measure real-Core macOS warm edit-and-rebuild against the existing added-time budget under one second.

The do-nothing baseline is the already-merged implementation and its existing thirteen passing tests.
No speedup, new test coverage, default-mode change, whole #43 closure, or increase above 43 accepted tracker items is claimed.

## Delivery self-check

Accuracy 4/5: direct logs bind thirteen named results to a verified merge tree, but second-UID and native-mount results are absent.
Completeness 2/5: this closes an evidence-inventory gap only; the six remaining original obligations above prevent whole #43 acceptance.
Clarity 4/5: protocol fixture scope and source-only limits are explicit, though historical and integrated heads require careful distinction.
Actionability 3/5: exact test names and job IDs are supplied, but independent review and owned mounted-workload execution remain required.
Conciseness 4/5: one requirement map replaces separate per-test reports; the audit cannot stand in for implementation progress.
Overall 3.4/5 for this bounded evidence slice, not for completing cowfs.
Highest-impact next steps are the complete round-3 seam review, second-UID verification, and actual mounted workload gates.
The user would reasonably reject whole-issue completion based on this audit, so the tracker remains open.
