# Git integrity repair worker interruption

## Failure capture

Session `ses_ef6d89446ffecL6CweaiKQeGX0` returned two completion notifications without visible final text while repairing PR #104.
The second terminal message has `finish: stop`, no recorded error, and zero visible text characters.
Its token metadata records 255360 cached-input tokens, 89 input tokens, and 242 output tokens.
The session lifecycle labels the turn `succeeded`, but this does not prove task completion.
The preceding calls successfully read and edited the script.

The worktree is `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/3/cowfs`.
HEAD remains `6df8b9fcfd6f9d54a6af0c8e424f3851fc6ecc71`, and `scripts/verify-git-index-integrity.py` has uncommitted changes.
The canonical repair evidence document was absent at the first recovery check.
GitHub closing references are empty and issue #21 remains open, so that metadata repair succeeded.

## Diagnosis and contained recovery

High context pressure is a hypothesis supported by token accounting, not a proven provider or harness fault.
No quota error, transport error, or context-limit error was recorded in the inspected terminal messages.
A third identical resume was rejected in favor of a fresh-context Space Bunny worker with a bounded handoff.
The previous worker is idle, and ownership of its existing lease and partial edits transfers to the replacement worker.
No edits, fixtures, logs, or leases are discarded, and no daemon or mount is touched.

## Success criteria

Recovery requires a nonempty completion report, a pushed source identity, canonical repair documentation, true test and lint exits, and an explicit account of any remaining mounted-test resource blocker.
Independent review remains required before merge.
The bounded investigation does not resolve the original unreproduced integrity issue.

## Preventive rule

A successful session finish is not a successful task receipt.
After repeated empty finishes, inspect saved state and context pressure, preserve partial work, and narrow a fresh-context handoff instead of repeating the same resume.
