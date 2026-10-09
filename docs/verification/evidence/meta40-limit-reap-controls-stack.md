# #40 stacked snapshot-limit, inode-limit and reap-durability controls

Refs #40.
This stacks the accepted-in-isolation slices #149, #150 and #151 onto main c7f915b84fa9877aed1ce8917b466f1d0b1adaae so one CI run tests exactly the tree that will be merged.
Head e720bfb contains only `crates/cowfs-meta/src/db.rs` merge resolution and the three heads' existing changes.

## Prior per-slice evidence

- #153 merged at c7f915b84fa9877aed1ce8917b466f1d0b1adaae after run 37559308173 passed both platforms with both named content-edge tests.
  Tested merge 1f616bfa83e2a1502f7cf2f4be22834942ebe09c and actual merge share tree 1a21e7042b08aae8750528ccace71ea621752e04.
  This delivers the original #40 M4 and M6 requirements, including reproduced failure then fix.
- #149 head 190b5b6, run 37559200801: snapshot-limit positive and exact-panic control passed on macOS job 112592387689 and Ubuntu job 112592387703.
- #150 head 5fbbd39, run 37559203481: inode-limit positive and exact-panic control passed on Ubuntu job 112592396363 and macOS job 112592396377.
- #151 head c51eaa1, run 37559204904: reap periodic-sync positive and exact-panic control passed on Ubuntu job 112592400990 and macOS job 112592401147.
- All jobs also passed the three format tests, the Core health regression, the fixed-cost large reservation test and both real-Store crash tests.

## Conflict resolution

Each branch added its fixtures at the same point in `db.rs`.
Both resolutions kept every fixture and test function from both sides.
A scripted check confirmed every snapshot, inode and reap fixture function is byte-identical to its accepted source.
All four test-only skip flags are declared once each.
rustfmt and whitespace checks pass.

## Pending

Require the nine named tests above on both platforms at head e720bfb and tested/merged tree equality before merge.
Then close #149, #150 and #151 as superseded by this stack.
Large contiguous inode reservations are untouched.
This is in-process mutation control evidence, not external source-mutant rebuilds, physical power loss, or whole #40 acceptance.
Tracker stays 68 items, 43 accepted.
