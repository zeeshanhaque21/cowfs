# Adapter race delivery, PR #143

PR #143 merged as `1580e69b9d987f63c07b2430f8c0b4547ecd8622` after independent wbuddy review and completed CI.
The accepted head is `7afe72264e1564b471e94aeaace8a370384514e0`.
GitHub readback confirms the PR is merged and closed.

## Evidence

- Independent review: `docs/reviews/pr143-rpc-outcome-channel-final-wbuddy-review.md`.
- Review SHA256: `d4dee6dff0962e5863c1aab5696be0bb0c07006b79611e7d8ee9369d2be2cff7`.
- Completed CI: run `37526812136`, Ubuntu, macOS, and Linux FUSE jobs successful.
- Ubuntu and macOS each executed all five `namespace_race` tests successfully, plus formatting and clippy checks.
- The fixture asserts successful create, lookup, and mkdir before checking the legal outcome.
- Valid encoded AppleDouble channel bytes round-trip; implausible bytes receive `NFS3ERR_NOTSUPP` without destroying the valid channel.

## Exact merged artifact

CI tested merge `6ab4b2e172d1fc5753727395e128c962d8cccde9`.
The actual merge and tested merge have identical tree `c4ace0bf61f18e62e21fc331e5178806f2eabf09`.
Both have ordered parents `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e` and `7afe72264e1564b471e94aeaace8a370384514e0`.
This comparison used GitHub commit objects, not a title or aggregate-green inference.

## Not whole-issue acceptance

Only the adapter-reachable sidecar namespace race is accepted here.
Issue #43 remains open for its other recorded critic, security, dead-server, Store-mode, conformance, and warm-build obligations.
Root-handle versus snapshot-view and separate-adapter runtime coverage is still missing.
No filesystem performance, crash durability, or overall implementation gate is claimed.

## Operational corrections

The worker retained logs under `/tmp` and created and removed a disposable worktree despite explicit restrictions.
Those violations are not excused by passing CI.
Resource preflight and older deletion provenance remain unverified.
Historical receipts and reviews remain immutable.
No cleanup, lease release, shared-resource restart, or local heavy execution was performed for this merge.
The primary checkout remains at its preserved local head; `detect_changes` inspected that checkout, not remote main.
