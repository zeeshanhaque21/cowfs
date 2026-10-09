# Large reservation metadata merge

PR [140](https://github.com/zeeshanhaque21/cowfs/pull/140) was merged at `2026-10-06T19:46:24Z`.
The reviewed head is `1c12ef84302d954898ef01d7b01f5e268ee45a2e`.
The verified merge commit is `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e`.
Its parents are previously tested main `89353e17e5085000711dc428e834f9cc41840a1f` and the reviewed head.

## Acceptance evidence

Independent wbuddy review is `docs/reviews/pr140-final-runtime-acceptance-wbuddy-review.md`, SHA-256 `0c76f95b5d8e5edd0ec8908453f614161f0042dca27dbc049d876d33085aed66`.
Run [37518576562](https://github.com/zeeshanhaque21/cowfs/actions/runs/37518576562) tested the pull-request merge into that main revision.
Ubuntu job `112457763474` and macOS job `112457763281` passed the real redb repair, ID-ceiling, timing-harness, and eleven reservation integration tests.
The Linux FUSE job passed but is not reservation-test evidence.

| CI platform | One-ID median | One-million-ID median | Repetitions |
|---|---:|---:|---:|
| Ubuntu | 1.707343 ms | 1.512210 ms | 3 |
| macOS | 6.568666 ms | 5.956875 ms | 3 |

These are measured fresh-store samples under CI load, not universal performance guarantees.
They do not measure the filesystem's separate 1.5x build or Git-status gate.

## Mutation verification and preservation

GitHub returned errors during ready-state requests; readback established that the PR nevertheless became ready.
The first merge request returned a response-decoding error; readback established that it had not merged.
A subsequent merge request used an explicit JSON body and required the exact reviewed head SHA.
The coordinator verified the merged PR, both merge parents, and issue #42 remaining open afterward.
No branch deletion, lease release, history rewrite, primary-checkout reset, local build, runner change, or shared-resource restart was performed.
`detect_changes` was run on the preserved local checkout; this does not mean that its older source tree was updated to remote main.

## Remaining work

This delivers the metadata reservation API only.
PR 142's Core consumer still needs commit-error retry safety, independent review, and actual end-to-end identity proof.
Issue #42 and the overall 68-item goal remain incomplete.
