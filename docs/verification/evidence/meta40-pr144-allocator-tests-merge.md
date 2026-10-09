# PR144 allocator test delivery

PR144 merged as `01fa855fc3521c519e8a93fe0b867dc56ebfdc5d` with accepted head `a1ebd3d034c2284c189c340a8c419d59c5269154`.
Independent source approval is recorded in `docs/reviews/pr144-original-snapshot-final-wbuddy-review.md`.
Independent executed-test approval is recorded in `docs/verification/evidence/meta40-pr144-completed-runtime-acceptance.md`.
Run `37535494085` completed successfully, with all four `allocation_option_extremes` tests passing on both Ubuntu and macOS, plus formatting and clippy.
All three job checkout logs identify tested merge `18afe7054697d9233e4002f4d804980dcdc4c0d1`.
The actual merge and tested merge share tree `ab4aa81620c22a04eadfad3590bf8c7590bd395c` and ordered parents `1580e69b9d987f63c07b2430f8c0b4547ecd8622` and the accepted head.
The merge used a head-SHA-guarded request and did not delete the branch or release its lease.
Issue40 was read back as open after the merge.
Only the allocator-option test slice is accepted, not whole-issue or whole-filesystem completion.
Follower mutation proof, the other four named controls, real-store crash proof and Core health integration remain open.
The primary checkout remains at its preserved local revision, not the remote merge revision.
`detect_changes` ran for the indexed primary project; its local graph must not be treated as remote-main source.
